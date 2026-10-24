//! Context assembly and read-chunk handlers.
#![allow(private_interfaces)]

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::access::RepoFilter;
use crate::search::context::{BridgeMode, ContentMode, ContextAssembler, ContextConfig};
use crate::tags::{build_tag_exclude_filter, build_tag_include_filter};

use super::{
    bad_request, find_matching_bundles, internal_error, open_metadata_store, open_vector_store,
    to_context_file, to_context_summary, AppState, BundleMatchOutput, ContextBudgetInfo,
    ContextFileOutput, ContextSummaryOutput, ErrorBody,
};

// ---------------------------------------------------------------------------
// /context
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ContextParams {
    /// Task description query
    q: String,
    /// Max lines budget (default 500)
    budget: Option<usize>,
    /// Coupling expansion depth (default 1)
    depth: Option<u32>,
    /// Max coupled files per seed (default 3)
    max_coupled: Option<usize>,
    /// Max initial search results (default 20)
    limit: Option<usize>,
    /// Min coupling threshold (default 0.1)
    coupling_threshold: Option<f32>,
    /// Filter by repository
    repo: Option<String>,
    /// Filter by named repo group
    group: Option<String>,
    /// Role for access filtering
    role: Option<String>,
    /// Include only chunks with these tags (comma-separated)
    tag: Option<String>,
    /// Exclude chunks with these tags (comma-separated)
    exclude_tag: Option<String>,
    /// Repo affinity: boost results from this repo (agent's current repo)
    repo_affinity: Option<String>,
    /// Override semantic_weight (0.0=keyword only, 1.0=semantic only)
    semantic_weight: Option<f32>,
    /// Override doc_demotion (0.0=full demotion, 1.0=no demotion)
    doc_demotion: Option<f32>,
    /// Override recency_weight (0.0=no recency, 1.0=full recency)
    recency_weight: Option<f32>,
    /// Scope context to a named context bundle
    bundle: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ContextResponse {
    query: String,
    budget: ContextBudgetInfo,
    files: Vec<ContextFileOutput>,
    summary: ContextSummaryOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bundles: Vec<BundleMatchOutput>,
}

pub(super) async fn context(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextParams>,
) -> Result<Json<ContextResponse>, (StatusCode, Json<ErrorBody>)> {
    let start = std::time::Instant::now();
    let vector_store = open_vector_store(&state).await.map_err(internal_error)?;

    let stats = vector_store
        .get_stats(None)
        .await
        .map_err(|e| internal_error(e.into()))?;
    if stats.total_chunks == 0 {
        return Ok(Json(ContextResponse {
            query: params.q,
            budget: ContextBudgetInfo {
                max_lines: params.budget.unwrap_or(500),
                used_lines: 0,
            },
            files: vec![],
            summary: ContextSummaryOutput {
                total_files: 0,
                total_chunks: 0,
                direct_hits: 0,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.0,
                intent: None,
                gate_boost: None,
            },
            bundles: vec![],
        }));
    }

    let metadata_store = open_metadata_store(&state).map_err(internal_error)?;

    let embedder = state.get_embedder().await.map_err(internal_error)?.clone();

    // Build tag filter for context pipeline
    let mut tag_filters: Vec<String> = Vec::new();
    if let Some(ref tags) = params.tag {
        let tag_list: Vec<String> = tags
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if !tag_list.is_empty() {
            tag_filters.push(build_tag_include_filter(&tag_list));
        }
    }
    if let Some(ref tags) = params.exclude_tag {
        let tag_list: Vec<String> = tags
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if !tag_list.is_empty() {
            tag_filters.push(build_tag_exclude_filter(&tag_list));
        }
    }
    // Apply bundle filter (scope context to bundle member files)
    if let Some(ref bundle_name) = params.bundle {
        if let Some(bundle_filter) = state.tags_config.build_bundle_file_filter(bundle_name) {
            tag_filters.push(bundle_filter);
        }
    }
    let extra_filter = if tag_filters.is_empty() {
        None
    } else {
        Some(tag_filters.join(" AND "))
    };

    // Cross-agent feedback: compute file-level boost scores from prior ratings
    let feedback_scores = super::feedback::open_feedback_store(&state)
        .ok()
        .and_then(|fb| fb.file_feedback_scores(&params.q, 0.15).ok())
        .filter(|m| !m.is_empty());

    let context_config = ContextConfig {
        budget_lines: params.budget.unwrap_or(500),
        budget_unit: state.config.context.budget_unit,
        depth: params.depth.unwrap_or(1),
        max_coupled: params.max_coupled.unwrap_or(3),
        coupling_threshold: params
            .coupling_threshold
            .unwrap_or(state.config.context.coupling_threshold),
        semantic_weight: params
            .semantic_weight
            .unwrap_or(state.config.search.semantic_weight),
        content_mode: ContentMode::Full,
        search_limit: params.limit.unwrap_or(20),
        doc_demotion: params
            .doc_demotion
            .unwrap_or(state.config.search.doc_demotion),
        recency_half_life_days: state.config.search.recency_half_life_days,
        recency_weight: params
            .recency_weight
            .unwrap_or(state.config.search.recency_weight),
        rrf_k: state.config.search.rrf_k,
        bridge_mode: BridgeMode::default(),
        bridge_boost_factor: state.config.context.bridge_boost_factor,
        extra_filter,
        tags_config: Some(state.tags_config.clone()),
        role: params.role.clone(),
        file_type_rules: state.config.file_types.clone(),
        repo_affinity: params.repo_affinity.clone(),
        repo_affinity_boost: state.config.hooks.repo_affinity_boost,
        max_bridged_files: state.config.context.max_bridged_files,
        max_bridged_chunks_per_file: state.config.context.max_bridged_chunks_per_file,
        knowledge_budget_pct: state.config.context.knowledge_budget_pct,
        neighbor_budget_pct: state.config.context.neighbor_budget_pct,
        knowledge_max_hops: state.config.context.knowledge_max_hops,
        feedback_boost_max: state.config.feedback.boost_max,
        feedback_boost_weight: state.config.feedback.boost_weight,
        repo_path_prefix: state.config.server.repo_path_prefix.clone(),
        feedback_scores,
        // PPR ranking signal: weight from config, and repo_root so the seed IRIs are
        // built repo-relative (the seed-IRI path-keying fix) — else every seed misses.
        ppr_weight: state.config.search.ppr_weight,
        repo_root: Some(state.repo_root.clone()),
        ..ContextConfig::default()
    };

    let mut assembler =
        ContextAssembler::new(embedder, vector_store, metadata_store, context_config);

    // Wire the Quipu store so PPR runs on the served /context path too (knowledge build only).
    // Mirrors cli/context.rs; without this the HTTP endpoint left quipu_store None and PPR was dark
    // in the serve path even in a --features knowledge binary.
    #[cfg(feature = "knowledge")]
    if state.config.search.ppr_weight > 0.0 {
        let quipu_config = quipu::QuipuConfig::load(&state.repo_root);
        let qpath = if quipu_config.store_path.is_relative() {
            state.repo_root.join(&quipu_config.store_path)
        } else {
            quipu_config.store_path.clone()
        };
        match quipu::Store::open(qpath.to_string_lossy().as_ref()) {
            Ok(store) => assembler = assembler.with_quipu_store(store),
            Err(e) => tracing::warn!("PPR enabled but Quipu store unavailable: {}", e),
        }
    }

    let mut bundle = assembler
        .assemble(&params.q, params.repo.as_deref())
        .await
        .map_err(internal_error)?;

    // Apply role-based access filtering
    let access = super::resolve_filter(&state, params.role.as_deref());
    bundle.files.retain(|f| access.is_path_allowed(&f.path));

    // Apply group filtering (narrow to repos in the named group)
    if let Some(ref group_name) = params.group {
        let group_repos = state.config.resolve_group(group_name).ok_or_else(|| {
            let available: Vec<&str> = state
                .config
                .groups
                .iter()
                .map(|g| g.name.as_str())
                .collect();
            if available.is_empty() {
                bad_request(format!(
                    "Unknown group '{}'. No groups configured.",
                    group_name
                ))
            } else {
                bad_request(format!(
                    "Unknown group '{}'. Available: {}",
                    group_name,
                    available.join(", ")
                ))
            }
        })?;
        bundle.files.retain(|f| {
            let repo = RepoFilter::repo_from_path(&f.path);
            group_repos.iter().any(|g| g == repo)
        });
    }

    let file_count = bundle.files.len();
    let elapsed = start.elapsed();
    log_context(
        &bundle.query,
        params.repo.as_deref(),
        params.role.as_deref(),
        file_count,
        (bundle.budget.used_lines, bundle.budget.max_lines),
        elapsed.as_millis() as u64,
    );

    // Classify query intent and include in response for client-side gating
    let intent = crate::search::intent::classify_intent(&params.q);
    let adj = crate::search::intent::intent_adjustments(intent);
    let mut summary = to_context_summary(&bundle.summary);
    summary.intent = Some(format!("{:?}", intent));
    summary.gate_boost = Some(adj.gate_boost);

    // Check for bundle keyword matches
    let matched_bundles = find_matching_bundles(&state.tags_config, &params.q);

    Ok(Json(ContextResponse {
        query: bundle.query,
        budget: ContextBudgetInfo {
            max_lines: bundle.budget.max_lines,
            used_lines: bundle.budget.used_lines,
        },
        files: bundle.files.iter().map(to_context_file).collect(),
        summary,
        bundles: matched_bundles,
    }))
}

// ---------------------------------------------------------------------------
// /read
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ReadChunkParams {
    /// Repository name from the search result.
    repo: Option<String>,
    /// File path (relative to repo root)
    file: String,
    /// Start line
    start_line: u32,
    /// End line
    end_line: u32,
    /// Context lines before/after (default 0)
    context: Option<u32>,
}

#[derive(Serialize)]
pub(super) struct ReadChunkResponse {
    file: String,
    start_line: u32,
    end_line: u32,
    actual_start_line: u32,
    actual_end_line: u32,
    content: String,
    language: String,
}

pub(super) async fn read_chunk(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ReadChunkParams>,
) -> Result<Json<ReadChunkResponse>, (StatusCode, Json<ErrorBody>)> {
    let ctx = params.context.unwrap_or(0);
    let (content, actual_start, actual_end) = read_file_lines(
        &state.repo_root,
        &params.file,
        params.repo.as_deref(),
        state.config.server.repo_path_prefix.as_deref(),
        params.start_line,
        params.end_line,
        ctx,
    )
    .map_err(internal_error)?;

    Ok(Json(ReadChunkResponse {
        file: params.file.clone(),
        start_line: params.start_line,
        end_line: params.end_line,
        actual_start_line: actual_start,
        actual_end_line: actual_end,
        content,
        language: super::detect_language(&params.file),
    }))
}

fn read_file_lines(
    repo_root: &std::path::Path,
    file: &str,
    repo: Option<&str>,
    repo_prefix: Option<&str>,
    start: u32,
    end: u32,
    context: u32,
) -> anyhow::Result<(String, u32, u32)> {
    let file_path = crate::read_path::resolve_read_path(repo_root, file, repo, repo_prefix)?;

    let content = std::fs::read_to_string(&file_path)?;
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as u32;

    let actual_start = start.saturating_sub(context).max(1);
    let actual_end = (end + context).min(total_lines);

    let start_idx = (actual_start - 1) as usize;
    let end_idx = actual_end as usize;

    let selected = if end_idx <= lines.len() {
        lines[start_idx..end_idx].join("\n")
    } else {
        lines[start_idx..].join("\n")
    };

    Ok((selected, actual_start, actual_end))
}

/// The per-request context log. Its query is the user's PROMPT on the
/// inject-context hook path, so INFO carries only a fingerprint and the
/// length; the text is DEBUG (aegis-uy7boa).
pub(super) fn log_context(
    query: &str,
    repo: Option<&str>,
    role: Option<&str>,
    files: usize,
    (budget_used, budget_max): (usize, usize),
    duration_ms: u64,
) {
    let query_sha = super::query_sha(query);
    tracing::debug!(query = %query, query_sha = %query_sha, "context query");
    tracing::info!(
        query_len = query.chars().count(),
        query_sha = %query_sha,
        repo = repo.unwrap_or("-"),
        role = role.unwrap_or("-"),
        files = files,
        budget_used = budget_used,
        budget_max = budget_max,
        duration_ms = duration_ms,
        "context"
    );
}
