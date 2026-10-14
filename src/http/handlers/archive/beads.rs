//! `/beads` search handlers.
//!
//! Split out of `archive/mod.rs` (bobbin-aoz): the beads endpoints are a
//! distinct surface from the archive ones and were 270 of its lines.

//! Archive and beads search handlers.
#![allow(private_interfaces)]

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::search::HybridSearch;
use crate::types::SearchResult;

use super::super::{internal_error, open_vector_store, AppState, ErrorBody};

use super::helpers::*;

// ---------------------------------------------------------------------------
// /beads
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct SearchBeadsParams {
    /// Natural language search query
    q: String,
    /// Filter by priority (1-4)
    priority: Option<i32>,
    /// Filter by status
    status: Option<String>,
    /// Filter by assignee
    assignee: Option<String>,
    /// Filter by rig name
    rig: Option<String>,
    /// Filter by issue type (bug, task, feature, etc.)
    issue_type: Option<String>,
    /// Filter by label
    label: Option<String>,
    /// Max results (default 10)
    limit: Option<usize>,
    /// Reread configured-store metadata (default true); not necessarily live.
    enrich: Option<bool>,
    /// Compact mode - omit snippet (default true)
    compact: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct SearchBeadsResponse {
    query: String,
    count: usize,
    warnings: Vec<String>,
    results: Vec<BeadResultItem>,
}

#[derive(Serialize)]
pub(crate) struct BeadResultItem {
    bead_id: String,
    metadata_provenance: crate::index::beads::provenance::BeadMetadataProvenance,
    title: String,
    priority: String,
    status: String,
    issue_type: String,
    assignee: String,
    owner: String,
    rig: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    relevance_score: f32,
    match_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet: Option<String>,
}

pub(crate) async fn search_beads(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchBeadsParams>,
) -> Result<Json<SearchBeadsResponse>, (StatusCode, Json<ErrorBody>)> {
    let limit = params.limit.unwrap_or(10);
    let should_enrich = params.enrich.unwrap_or(true);
    let compact = params.compact.unwrap_or(true);

    let vector_store = open_vector_store(&state).await.map_err(internal_error)?;

    let embedder = state.get_embedder().await.map_err(internal_error)?.clone();

    let mut search = HybridSearch::new(embedder, vector_store, state.config.search.semantic_weight);

    // Push the Issue filter INTO the LanceDB query instead of over-fetching the
    // whole corpus and keeping Issue chunks in Rust. Issue chunks are ~0.1% of the
    // index, so for a common-term query the top limit*5 candidates were all
    // code/markdown and the post-filter yielded ZERO — /beads?q=dolt returned
    // nothing while /search?q=dolt&mode=keyword returned the same beads.
    // search_filtered pushes `chunk_type = 'issue'` into both the semantic and
    // keyword halves (the exact pattern the archive /search path uses above), so
    // retrieval returns Issue chunks directly and never starves. The predicate
    // value is lowercase 'issue' to match chunk_type_to_str (storage/lance.rs).
    let search_results = search
        .search_filtered(&params.q, limit, None, Some("chunk_type = 'issue'"))
        .await
        .map_err(|e| internal_error(e.into()))?;

    let mut filtered: Vec<SearchResult> = search_results.into_iter().collect();

    // Apply rig filter
    if let Some(ref rig) = params.rig {
        let prefix = format!("beads:{}:", rig);
        filtered.retain(|r| r.chunk.file_path.starts_with(&prefix));
    }

    // Reread configured-source metadata; exports remain snapshots.
    let live_metadata = if should_enrich && state.config.beads.enabled {
        let bead_ids: Vec<(String, String)> = filtered
            .iter()
            .filter_map(|r| {
                let parts: Vec<&str> = r.chunk.file_path.splitn(3, ':').collect();
                if parts.len() == 3 {
                    Some((parts[1].to_string(), parts[2].to_string()))
                } else {
                    None
                }
            })
            .collect();

        crate::index::beads::fetch_bead_metadata(&state.config.beads, &bead_ids)
            .await
            .map_err(internal_error)?
    } else {
        std::collections::HashMap::new()
    };

    // Apply all filters
    let has_filters = params.status.is_some()
        || params.priority.is_some()
        || params.assignee.is_some()
        || params.issue_type.is_some()
        || params.label.is_some();
    if has_filters {
        filtered.retain(|r| {
            let bead_id = r.chunk.file_path.split(':').nth(2).unwrap_or("");
            if let Some(meta) = live_metadata.get(bead_id) {
                if let Some(ref status) = params.status {
                    if meta.status != *status {
                        return false;
                    }
                }
                if let Some(priority) = params.priority {
                    if meta.priority != priority {
                        return false;
                    }
                }
                if let Some(ref assignee) = params.assignee {
                    let meta_assignee = meta.assignee.as_deref().unwrap_or("unassigned");
                    if !meta_assignee.contains(assignee.as_str()) {
                        return false;
                    }
                }
                if let Some(ref issue_type) = params.issue_type {
                    if meta.issue_type != *issue_type {
                        return false;
                    }
                }
                if let Some(ref label) = params.label {
                    if !meta.labels.iter().any(|l| l.contains(label.as_str())) {
                        return false;
                    }
                }
                true
            } else {
                let content = &r.chunk.content;
                if let Some(ref status) = params.status {
                    if !content.contains(&format!("Status: {}", status)) {
                        return false;
                    }
                }
                if let Some(priority) = params.priority {
                    if !content.contains(&format!("Priority: P{}", priority)) {
                        return false;
                    }
                }
                if let Some(ref assignee) = params.assignee {
                    if !content.contains(&format!("Assignee: {}", assignee)) {
                        return false;
                    }
                }
                true
            }
        });
    }

    // Boost relevance scores: title match and status weighting
    let query_lower = params.q.to_lowercase();
    let query_terms: Vec<&str> = query_lower.split_whitespace().collect();
    for result in &mut filtered {
        let mut boost: f32 = 1.0;

        // Title match boost
        if let Some(ref name) = result.chunk.name {
            let title_lower = name.to_lowercase();
            let matching_terms = query_terms
                .iter()
                .filter(|t| title_lower.contains(**t))
                .count();
            if matching_terms > 0 {
                boost += 0.3 * (matching_terms as f32 / query_terms.len().max(1) as f32);
            }
        }

        // Status boost: open/in_progress are more actionable
        let bead_id = result.chunk.file_path.split(':').nth(2).unwrap_or("");
        if let Some(meta) = live_metadata.get(bead_id) {
            match meta.status.as_str() {
                "in_progress" | "hooked" => boost += 0.15,
                "open" | "blocked" => boost += 0.1,
                "closed" => boost -= 0.1,
                _ => {}
            }
        }

        result.score = (result.score * boost).min(1.0);
    }

    // Re-sort by boosted score
    filtered.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    filtered.truncate(limit);

    let results: Vec<BeadResultItem> = filtered
        .iter()
        .map(|r| {
            let parts: Vec<&str> = r.chunk.file_path.splitn(3, ':').collect();
            let rig = if parts.len() >= 2 { parts[1] } else { "" };
            let bead_id = if parts.len() == 3 {
                parts[2]
            } else {
                &r.chunk.file_path
            };

            let match_type = r
                .match_type
                .as_ref()
                .map(|mt| format!("{:?}", mt).to_lowercase())
                .unwrap_or_else(|| "hybrid".to_string());

            if let Some(meta) = live_metadata.get(bead_id) {
                let snippet = if compact {
                    None
                } else {
                    Some(clean_bead_snippet(&r.chunk.content, 200))
                };

                BeadResultItem {
                    bead_id: bead_id.to_string(),
                    metadata_provenance:
                        crate::index::beads::provenance::BeadMetadataProvenance::for_result(
                            &state.config.beads,
                            rig,
                            true,
                        ),
                    title: meta.title.clone(),
                    priority: format!("P{}", meta.priority),
                    status: meta.status.clone(),
                    issue_type: meta.issue_type.clone(),
                    assignee: meta
                        .assignee
                        .clone()
                        .unwrap_or_else(|| "unassigned".to_string()),
                    owner: meta.owner.clone(),
                    rig: rig.to_string(),
                    labels: meta.labels.clone(),
                    created_at: meta.created_at.clone(),
                    relevance_score: r.score,
                    match_type,
                    snippet,
                }
            } else {
                let content = &r.chunk.content;
                let snippet = if compact {
                    None
                } else {
                    Some(clean_bead_snippet(content, 200))
                };

                BeadResultItem {
                    bead_id: bead_id.to_string(),
                    metadata_provenance:
                        crate::index::beads::provenance::BeadMetadataProvenance::for_result(
                            &state.config.beads,
                            rig,
                            false,
                        ),
                    title: r.chunk.name.clone().unwrap_or_default(),
                    priority: extract_bead_field(content, "Priority: "),
                    status: extract_bead_field(content, "Status: "),
                    issue_type: "task".to_string(),
                    assignee: extract_bead_field(content, "Assignee: "),
                    owner: String::new(),
                    rig: rig.to_string(),
                    labels: Vec::new(),
                    created_at: None,
                    relevance_score: r.score,
                    match_type,
                    snippet,
                }
            }
        })
        .collect();

    Ok(Json(SearchBeadsResponse {
        query: params.q,
        count: results.len(),
        warnings: crate::index::beads::provenance::search_warnings(),
        results,
    }))
}
