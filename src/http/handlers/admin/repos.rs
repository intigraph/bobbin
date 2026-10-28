//! Repository, group and primer admin endpoints.
//!
//! Split out of the former `admin.rs` (bobbin-aoz).

//! Admin handlers: status, healthz, metrics, prime, suggest, repos, groups, files.
#![allow(private_interfaces)]

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::super::{
    bad_request, detect_language, internal_error, open_vector_store, AppState, ErrorBody,
};

// ---------------------------------------------------------------------------
// /repos
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct ReposParams {
    /// Filter by named repo group
    group: Option<String>,
    /// Role for access filtering
    role: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ReposListResponse {
    count: usize,
    repos: Vec<RepoSummary>,
}

#[derive(Serialize)]
pub(crate) struct RepoSummary {
    name: String,
    file_count: u64,
    chunk_count: u64,
    languages: Vec<crate::types::LanguageStats>,
}

pub(crate) async fn list_repos(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ReposParams>,
) -> Result<Json<ReposListResponse>, (StatusCode, Json<ErrorBody>)> {
    let store = open_vector_store(&state).await.map_err(internal_error)?;

    let repos = store
        .get_all_repos()
        .await
        .map_err(|e| internal_error(e.into()))?;

    let mut summaries = Vec::new();
    for repo_name in &repos {
        let stats = store
            .get_stats(Some(repo_name))
            .await
            .map_err(|e| internal_error(e.into()))?;
        summaries.push(RepoSummary {
            name: repo_name.clone(),
            file_count: stats.total_files,
            chunk_count: stats.total_chunks,
            languages: stats.languages,
        });
    }

    // Apply role-based access filtering
    let access = super::super::resolve_filter(&state, params.role.as_deref());
    summaries.retain(|r| access.is_allowed(&r.name));

    // Apply group filtering
    if let Some(ref group_name) = params.group {
        if let Some(group_repos) = state.config.resolve_group(group_name) {
            summaries.retain(|r| group_repos.iter().any(|g| g == &r.name));
        } else {
            return Err(bad_request(format!("Unknown group '{}'", group_name)));
        }
    }

    // Sort by chunk count descending
    summaries.sort_by(|a, b| b.chunk_count.cmp(&a.chunk_count));

    Ok(Json(ReposListResponse {
        count: summaries.len(),
        repos: summaries,
    }))
}

// ---------------------------------------------------------------------------
// /groups
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct GroupsResponse {
    count: usize,
    groups: Vec<GroupItem>,
}

#[derive(Serialize)]
pub(crate) struct GroupItem {
    name: String,
    repos: Vec<String>,
}

pub(crate) async fn list_groups(State(state): State<Arc<AppState>>) -> Json<GroupsResponse> {
    let groups: Vec<GroupItem> = state
        .config
        .groups
        .iter()
        .map(|g| GroupItem {
            name: g.name.clone(),
            repos: g.repos.clone(),
        })
        .collect();

    Json(GroupsResponse {
        count: groups.len(),
        groups,
    })
}

// ---------------------------------------------------------------------------
// /repos/{name}/files
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct RepoFilesParams {
    /// Role for access filtering
    role: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct RepoFilesResponse {
    repo: String,
    count: usize,
    files: Vec<RepoFileItem>,
}

#[derive(Serialize)]
pub(crate) struct RepoFileItem {
    path: String,
    language: String,
}

pub(crate) async fn list_repo_files(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<RepoFilesParams>,
) -> Result<Json<RepoFilesResponse>, (StatusCode, Json<ErrorBody>)> {
    // Check role-based access for this repo
    let access = super::super::resolve_filter(&state, params.role.as_deref());
    if !access.is_allowed(&name) {
        return Err(bad_request(format!("Repo not accessible: {}", name)));
    }

    let store = open_vector_store(&state).await.map_err(internal_error)?;

    let files = store
        .get_all_file_paths(Some(&name))
        .await
        .map_err(|e| internal_error(e.into()))?;

    let items: Vec<RepoFileItem> = files
        .into_iter()
        .map(|p| {
            let lang = detect_language(&p);
            RepoFileItem {
                path: p,
                language: lang,
            }
        })
        .collect();

    Ok(Json(RepoFilesResponse {
        repo: name,
        count: items.len(),
        files: items,
    }))
}

// ---------------------------------------------------------------------------
// /prime
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct PrimeParams {
    /// Specific section to show
    section: Option<String>,
    /// Show brief overview only
    brief: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct PrimeResponse {
    primer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    section: Option<String>,
    initialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats: Option<PrimeStats>,
}

#[derive(Serialize)]
pub(crate) struct PrimeStats {
    total_files: u64,
    total_chunks: u64,
    total_embeddings: u64,
    languages: Vec<PrimeLanguageStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_indexed: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PrimeLanguageStats {
    language: String,
    file_count: u64,
    chunk_count: u64,
}

pub(crate) async fn prime(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PrimeParams>,
) -> Result<Json<PrimeResponse>, (StatusCode, Json<ErrorBody>)> {
    let primer_text =
        crate::primer::legacy_text(params.section.as_deref(), params.brief.unwrap_or(false));

    let stats = match open_vector_store(&state).await {
        Ok(store) => match store.get_stats(None).await {
            Ok(s) => Some(PrimeStats {
                total_files: s.total_files,
                total_chunks: s.total_chunks,
                total_embeddings: s.total_embeddings,
                languages: s
                    .languages
                    .iter()
                    .map(|l| PrimeLanguageStats {
                        language: l.language.clone(),
                        file_count: l.file_count,
                        chunk_count: l.chunk_count,
                    })
                    .collect(),
                last_indexed: s
                    .last_indexed
                    .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0).map(|t| t.to_rfc3339())),
            }),
            Err(_) => None,
        },
        Err(_) => None,
    };

    Ok(Json(PrimeResponse {
        primer: primer_text,
        section: params.section,
        initialized: true,
        stats,
    }))
}

// ---------------------------------------------------------------------------
// /suggest
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct SuggestParams {
    /// Filter field to suggest values for: repo, lang, type, group, tag
    field: String,
    /// Optional prefix to filter suggestions
    #[serde(default)]
    q: Option<String>,
    /// Role for access filtering
    #[serde(default)]
    role: Option<String>,
    /// Max results (default 20)
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Serialize)]
pub(crate) struct SuggestResponse {
    field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    count: usize,
    values: Vec<String>,
}

pub(crate) async fn suggest(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SuggestParams>,
) -> Result<Json<SuggestResponse>, (StatusCode, Json<ErrorBody>)> {
    let limit = params.limit.unwrap_or(20);
    let prefix = params.q.as_deref().unwrap_or("").to_lowercase();

    let mut values: Vec<String> = match params.field.as_str() {
        "repo" => {
            let store = open_vector_store(&state).await.map_err(internal_error)?;
            let repos = store
                .get_all_repos()
                .await
                .map_err(|e| internal_error(e.into()))?;
            let access = super::super::resolve_filter(&state, params.role.as_deref());
            repos.into_iter().filter(|r| access.is_allowed(r)).collect()
        }
        "lang" | "language" => {
            let store = open_vector_store(&state).await.map_err(internal_error)?;
            store
                .get_all_languages()
                .await
                .map_err(|e| internal_error(e.into()))?
        }
        "type" => vec![
            "function",
            "method",
            "class",
            "struct",
            "enum",
            "interface",
            "module",
            "impl",
            "trait",
            "doc",
            "section",
            "table",
            "code_block",
            "commit",
            "issue",
            "other",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        "group" => state.config.groups.iter().map(|g| g.name.clone()).collect(),
        "tag" => {
            let store = open_vector_store(&state).await.map_err(internal_error)?;
            let counts = store
                .get_tag_counts()
                .await
                .map_err(|e| internal_error(e.into()))?;
            counts.into_iter().map(|(tag, _)| tag).collect()
        }
        _ => {
            return Err(bad_request(format!(
                "Unknown field '{}'. Use: repo, lang, type, group, tag",
                params.field
            )));
        }
    };

    // Apply prefix filter
    if !prefix.is_empty() {
        values.retain(|v| v.to_lowercase().starts_with(&prefix));
    }
    values.truncate(limit);

    Ok(Json(SuggestResponse {
        field: params.field,
        query: params.q,
        count: values.len(),
        values,
    }))
}
