//! Search and chunk retrieval handlers.
#![allow(private_interfaces)]

use super::search_exec::{execute_or_search, execute_single_search};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::search::{HybridSearch, SemanticSearch};
use crate::tags::{build_tag_exclude_filter, build_tag_include_filter};
use crate::types::{MatchType, SearchResult};

use super::{
    bad_request, find_matching_bundles, internal_error, open_vector_store, parse_chunk_type,
    truncate, AppState, BundleMatchOutput, ErrorBody,
};

// ---------------------------------------------------------------------------
// /search
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct SearchParams {
    /// Search query
    q: String,
    /// Search mode: hybrid (default), semantic, keyword
    mode: Option<String>,
    /// Filter by chunk type
    r#type: Option<String>,
    /// Max results (default 10)
    limit: Option<usize>,
    /// Filter by repository name
    repo: Option<String>,
    /// Filter by named repo group (composable with role filtering)
    group: Option<String>,
    /// Role for access filtering
    role: Option<String>,
    /// Include only chunks with these tags (comma-separated)
    tag: Option<String>,
    /// Exclude chunks with these tags (comma-separated)
    exclude_tag: Option<String>,
    /// Scope search to a named context bundle
    bundle: Option<String>,
    /// Include optional Quipu entity annotations (default true).
    spotlight: Option<bool>,
}

#[derive(Serialize)]
pub(super) struct SearchResponse {
    query: String,
    mode: String,
    count: usize,
    /// How `query` was actually interpreted. Without this a caller cannot
    /// tell a typo'd filter (`rpeo:aegis`, parsed as a free-text term) from a
    /// working one — both return results, and only one of them means what the
    /// caller wrote.
    parsed: ParsedQueryOutput,
    results: Vec<SearchResultItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bundles: Vec<BundleMatchOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    spotlight_annotations: Vec<SpotlightHit>,
}

/// The interpreted query, echoed back to the caller.
#[derive(Serialize, Default)]
pub(super) struct ParsedQueryOutput {
    /// The text actually sent to the index, with filters stripped.
    text_query: String,
    terms: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    phrases: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    filters: Vec<ParsedFilterOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    required_terms: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    negated_terms: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    regex_patterns: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    or_branches: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct ParsedFilterOutput {
    field: String,
    values: Vec<String>,
    negated: bool,
}

impl From<&crate::search::query::ParsedQuery> for ParsedQueryOutput {
    fn from(p: &crate::search::query::ParsedQuery) -> Self {
        Self {
            text_query: p.text_query.clone(),
            terms: p.terms.clone(),
            phrases: p.phrases.clone(),
            filters: p
                .filters
                .iter()
                .map(|f| ParsedFilterOutput {
                    field: f.field.canonical_name().to_string(),
                    values: f.values.clone(),
                    negated: f.negated,
                })
                .collect(),
            required_terms: p.required_terms.clone(),
            negated_terms: p.negated_terms.clone(),
            regex_patterns: p.regex_patterns.clone(),
            or_branches: p.or_branches.clone(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SpotlightHit {
    surface: String,
    iri: String,
    #[serde(alias = "type")]
    entity_type: String,
    confidence: f32,
}

#[derive(Serialize)]
pub(super) struct SearchResultItem {
    file_path: String,
    name: Option<String>,
    chunk_type: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    start_line: u32,
    end_line: u32,
    score: f32,
    match_type: Option<String>,
    language: String,
    content_preview: String,
}

pub(super) async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, (StatusCode, Json<ErrorBody>)> {
    let start = std::time::Instant::now();
    let limit = params.limit.unwrap_or(10);
    let mode = params.mode.as_deref().unwrap_or("hybrid");

    // Parse advanced query syntax: extract inline filters, phrases, and free text
    let parsed = crate::search::query::parse(&params.q);

    let type_filter = params
        .r#type
        .as_deref()
        .map(parse_chunk_type)
        .transpose()
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorBody {
                    error: e.to_string(),
                }),
            )
        })?;

    let vector_store = open_vector_store(&state).await.map_err(internal_error)?;

    let stats = vector_store
        .get_stats(None)
        .await
        .map_err(|e| internal_error(e.into()))?;
    if stats.total_chunks == 0 {
        return Ok(Json(SearchResponse {
            query: params.q,
            mode: mode.to_string(),
            count: 0,
            parsed: (&parsed).into(),
            results: vec![],
            bundles: vec![],
            spotlight_annotations: vec![],
        }));
    }

    let search_limit = if type_filter.is_some() {
        limit * 3
    } else {
        limit
    };

    // Repo filter: prefer inline filter, fall back to query param
    let inline_repo: Option<String> = parsed
        .filters
        .iter()
        .find(|f| {
            f.field == crate::search::query::FilterField::Repo && !f.negated && f.values.len() == 1
        })
        .map(|f| f.values[0].clone());
    let repo_filter_str = inline_repo.as_deref().or(params.repo.as_deref());

    // Group filter: prefer inline filter, fall back to query param
    let inline_groups = crate::search::query::extract_group_filters(&parsed.filters);
    let group_param = if !inline_groups.is_empty() {
        Some(inline_groups[0].as_str())
    } else {
        params.group.as_deref()
    };
    let group_sql = super::resolve_group_filter(&state, group_param).map_err(|e| bad_request(e))?;

    // Build combined filter from parsed inline filters + group + tag include/exclude
    let mut extra_filters: Vec<String> = Vec::new();

    // Add SQL from inline filters (repo:, lang:, type:, file:, path:, tag:)
    let inline_sql = crate::search::query::filters_to_sql(&parsed.filters);
    extra_filters.extend(inline_sql);

    if let Some(ref g) = group_sql {
        extra_filters.push(g.clone());
    }
    if let Some(ref tags) = params.tag {
        let tag_list: Vec<String> = tags
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if !tag_list.is_empty() {
            // Expand requested tags through the ontology hierarchy so a query for
            // a parent concept (e.g. `security`) also matches descendant tags
            // (`auth`, `session`). No-op when no ontology is configured (GH#14).
            let expanded = state.tags_config.expand_tags_via_ontology(&tag_list);
            extra_filters.push(build_tag_include_filter(&expanded));
        }
    }
    if let Some(ref tags) = params.exclude_tag {
        let tag_list: Vec<String> = tags
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if !tag_list.is_empty() {
            extra_filters.push(build_tag_exclude_filter(&tag_list));
        }
    }
    // Apply bundle filter (scope search to bundle member files). Use the
    // ontology-aware variant so a bundle's tags expand through the hierarchy
    // (GH#14); falls back to plain tag membership when no ontology is defined.
    if let Some(ref bundle_name) = params.bundle {
        if let Some(bundle_filter) = state
            .tags_config
            .build_bundle_file_filter_with_ontology(bundle_name)
        {
            extra_filters.push(bundle_filter);
        }
    }
    // Apply tag effect exclusions (e.g. auto:init exclude=true from tags.toml)
    if let Some(effect_filter) =
        crate::tags::build_effect_exclude_filter(&state.tags_config, params.role.as_deref())
    {
        extra_filters.push(effect_filter);
    }
    let combined_filter = if extra_filters.is_empty() {
        None
    } else {
        Some(extra_filters.join(" AND "))
    };

    // Use parsed text_query (filters stripped) for actual search
    let search_query = if parsed.text_query.is_empty() {
        // If only filters and no text, use a broad match
        params.q.clone()
    } else {
        parsed.text_query.clone()
    };

    // Execute search — with OR branch merging if applicable
    let results = if parsed.has_or && parsed.or_branches.len() > 1 {
        // OR query: run each branch separately and merge results by best score
        execute_or_search(
            &state,
            &parsed.or_branches,
            mode,
            search_limit,
            repo_filter_str,
            combined_filter.as_deref(),
        )
        .await?
    } else {
        execute_single_search(
            &state,
            &search_query,
            mode,
            search_limit,
            repo_filter_str,
            combined_filter.as_deref(),
        )
        .await?
    };

    // Apply role-based access filtering
    let access = super::resolve_filter(&state, params.role.as_deref());
    let mut results = access.filter_vec_by_path(results, |r| &r.chunk.file_path);

    // Apply the content-level parts of the query the store cannot express as
    // SQL: +required, -negated / NOT, and /regex/. Shared with the CLI so both
    // surfaces answer the same query the same way.
    crate::search::query::retain_matching(&mut results, &parsed, |r| r.chunk.content.as_str());

    let filtered: Vec<_> = if let Some(ref chunk_type) = type_filter {
        results
            .into_iter()
            .filter(|r| &r.chunk.chunk_type == chunk_type)
            .take(limit)
            .collect()
    } else {
        results.into_iter().take(limit).collect()
    };

    let elapsed = start.elapsed();
    log_search(
        &params.q,
        params.repo.as_deref(),
        params.role.as_deref(),
        mode,
        filtered.len(),
        elapsed.as_millis() as u64,
    );

    // Check for bundle keyword matches
    let matched_bundles = find_matching_bundles(&state.tags_config, &params.q);

    // Call Quipu spotlight API if configured (non-blocking on failure)
    let spotlight_annotations =
        optional_spotlight(state.config.quipu_endpoint.as_deref(), &params).await;

    Ok(Json(SearchResponse {
        query: params.q,
        mode: mode.to_string(),
        count: filtered.len(),
        parsed: (&parsed).into(),
        results: filtered.iter().map(to_search_item).collect(),
        bundles: matched_bundles,
        spotlight_annotations,
    }))
}

// ---------------------------------------------------------------------------
// /chunk/{id}
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct ChunkResponse {
    id: String,
    file_path: String,
    chunk_type: String,
    name: Option<String>,
    start_line: u32,
    end_line: u32,
    language: String,
    content: String,
}

pub(super) async fn get_chunk(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ChunkResponse>, (StatusCode, Json<ErrorBody>)> {
    let store = open_vector_store(&state).await.map_err(internal_error)?;

    let chunk = store
        .get_chunk_by_id(&id)
        .await
        .map_err(|e| internal_error(e.into()))?;

    match chunk {
        Some(c) => Ok(Json(ChunkResponse {
            id: c.id,
            file_path: c.file_path,
            chunk_type: c.chunk_type.to_string(),
            name: c.name,
            start_line: c.start_line,
            end_line: c.end_line,
            language: c.language,
            content: c.content,
        })),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: format!("Chunk not found: {}", id),
            }),
        )),
    }
}

// ---------------------------------------------------------------------------
// Search helpers
// ---------------------------------------------------------------------------

async fn optional_spotlight(endpoint: Option<&str>, params: &SearchParams) -> Vec<SpotlightHit> {
    match endpoint.filter(|_| params.spotlight.unwrap_or(true)) {
        Some(endpoint) => fetch_spotlight(endpoint, &params.q).await,
        None => vec![],
    }
}

/// Call Quipu's spotlight API to get entity annotations for a query.
/// Returns empty vec on any error (graceful degradation).
async fn fetch_spotlight(endpoint: &str, query: &str) -> Vec<SpotlightHit> {
    #[derive(Deserialize)]
    struct SpotlightResponse {
        #[serde(default)]
        annotations: Vec<SpotlightHit>,
    }

    let url = format!("{}/spotlight", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    match client
        .post(&url)
        .header("X-Quipu-Client", "bobbin-search")
        .json(&serde_json::json!({ "text": query, "confidence": 0.3 }))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp
            .json::<SpotlightResponse>()
            .await
            .map(|r| r.annotations)
            .unwrap_or_default(),
        Ok(resp) => {
            tracing::debug!(status = %resp.status(), "spotlight API non-success");
            vec![]
        }
        Err(e) => {
            tracing::debug!(error = %e, "spotlight API unreachable");
            vec![]
        }
    }
}

#[cfg(test)]
#[path = "search_spotlight_tests.rs"]
mod spotlight_tests;

fn to_search_item(r: &crate::types::SearchResult) -> SearchResultItem {
    SearchResultItem {
        file_path: r.chunk.file_path.clone(),
        name: r.chunk.name.clone(),
        chunk_type: r.chunk.chunk_type.to_string(),
        source: crate::types::source_kind(&r.chunk.chunk_type).to_string(),
        repo: r.repo.clone(),
        start_line: r.chunk.start_line,
        end_line: r.chunk.end_line,
        score: r.score,
        match_type: r.match_type.map(|mt| match mt {
            MatchType::Semantic => "semantic".to_string(),
            MatchType::Keyword => "keyword".to_string(),
            MatchType::Hybrid => "hybrid".to_string(),
        }),
        language: r.chunk.language.clone(),
        content_preview: truncate(&r.chunk.content, 300),
    }
}

/// The per-request search log. INFO carries the operational fields and a
/// fingerprint of the query, never its text; the text is DEBUG (aegis-uy7boa).
pub(super) fn log_search(
    q: &str,
    repo: Option<&str>,
    role: Option<&str>,
    mode: &str,
    results: usize,
    duration_ms: u64,
) {
    let query_sha = super::query_sha(q);
    tracing::debug!(query = %q, query_sha = %query_sha, "search query");
    tracing::info!(
        query_len = q.chars().count(),
        query_sha = %query_sha,
        repo = repo.unwrap_or("-"),
        role = role.unwrap_or("-"),
        mode = mode,
        results = results,
        duration_ms = duration_ms,
        "search"
    );
}
