//! Feedback, injection, and lineage handlers.
#![allow(private_interfaces)]

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::config::Config;
use crate::storage::FeedbackStore;

use super::{bad_request, internal_error, not_found, AppState, ErrorBody};

pub(super) fn open_feedback_store(state: &AppState) -> anyhow::Result<FeedbackStore> {
    let path = Config::feedback_db_path(&state.repo_root);
    FeedbackStore::open(&path)
}

// ---------------------------------------------------------------------------
// /injections
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct InjectionInput {
    injection_id: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    query: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    total_chunks: usize,
    #[serde(default)]
    budget_lines: usize,
    #[serde(default)]
    formatted_output: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    faction_id: Option<String>,
    #[serde(default)]
    grounding_id: Option<String>,
}

/// POST /injections — store an injection record for feedback reference
pub(super) async fn injection_store(
    State(state): State<Arc<AppState>>,
    Json(input): Json<InjectionInput>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    if input.injection_id.is_empty() {
        return Err(bad_request("injection_id is required".to_string()));
    }
    let store = open_feedback_store(&state).map_err(internal_error)?;
    store
        .store_scoped_injection_with_output(
            &input.injection_id,
            input.session_id.as_deref(),
            input.agent.as_deref(),
            &input.query,
            &input.files,
            input.total_chunks,
            input.budget_lines,
            input.formatted_output.as_deref(),
            input.scope.as_deref(),
            input.faction_id.as_deref(),
            input.grounding_id.as_deref(),
        )
        .map_err(internal_error)?;
    Ok(Json(serde_json::json!({
        "status": "ok",
        "injection_id": input.injection_id
    })))
}

/// GET /injections/:id — get injection detail with associated feedback
#[derive(Deserialize)]
pub(super) struct InjectionItemsInput {
    #[serde(default)]
    items: Vec<crate::memory_recall::CitedItem>,
}

/// Log the cited memory items (`m1`..`m5` -> entity IRI) of an injection, so
/// item-level feedback can resolve to an entity (aegis-gshu.2).
pub(super) async fn injection_items_store(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(input): Json<InjectionItemsInput>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    let store = open_feedback_store(&state).map_err(internal_error)?;
    store
        .store_injection_items(&id, &input.items)
        .map_err(|e| {
            if e.to_string().contains("invalid item") || e.to_string().contains("required") {
                bad_request(e.to_string())
            } else {
                internal_error(e)
            }
        })?;
    Ok(Json(serde_json::json!({
        "status": "ok",
        "injection_id": id,
        "items": input.items.len(),
    })))
}

pub(super) async fn injection_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    let store = open_feedback_store(&state).map_err(internal_error)?;
    match store.get_injection(&id).map_err(internal_error)? {
        Some(detail) => Ok(Json(serde_json::to_value(detail).unwrap())),
        None => Err(not_found(format!("injection {} not found", id))),
    }
}

// ---------------------------------------------------------------------------
// /feedback
// ---------------------------------------------------------------------------

/// POST /feedback — submit feedback on an injection
pub(super) async fn feedback_submit(
    State(state): State<Arc<AppState>>,
    Json(input): Json<crate::storage::feedback::FeedbackInput>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    let store = open_feedback_store(&state).map_err(internal_error)?;
    store.store_feedback(&input).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("Invalid rating")
            || msg.contains("Invalid item")
            || msg.contains("required")
        {
            bad_request(e.to_string())
        } else {
            internal_error(e)
        }
    })?;
    let target = match input.item.as_deref() {
        Some(item) => format!("{} {}", input.injection_id, item),
        None => input.injection_id.clone(),
    };
    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Feedback recorded: {} for {}", input.rating, target)
    })))
}

#[derive(Deserialize)]
pub(super) struct FeedbackListParams {
    injection_id: Option<String>,
    rating: Option<String>,
    agent: Option<String>,
    limit: Option<usize>,
}

/// GET /feedback — list feedback records with optional filters
pub(super) async fn feedback_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FeedbackListParams>,
) -> Result<Json<Vec<crate::storage::feedback::FeedbackRecord>>, (StatusCode, Json<ErrorBody>)> {
    let store = open_feedback_store(&state).map_err(internal_error)?;
    let query = crate::storage::feedback::FeedbackQuery {
        injection_id: params.injection_id,
        rating: params.rating,
        agent: params.agent,
        limit: params.limit,
    };
    let records = store.list_feedback(&query).map_err(internal_error)?;
    Ok(Json(records))
}

// ---------------------------------------------------------------------------
// /feedback/stats
// ---------------------------------------------------------------------------

/// Query parameters for feedback stats endpoint.
#[derive(Debug, Deserialize)]
pub(super) struct FeedbackStatsParams {
    /// Group results by "bundle" or "bead". If omitted, returns aggregated totals.
    pub group_by: Option<String>,
}

/// GET /feedback/stats — aggregated feedback statistics
pub(super) async fn feedback_stats(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FeedbackStatsParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    let store = open_feedback_store(&state).map_err(internal_error)?;
    match params.group_by.as_deref() {
        Some("bundle") => {
            let entries = store.stats_by_bundle().map_err(internal_error)?;
            Ok(Json(serde_json::to_value(entries).unwrap()))
        }
        Some("bead") => {
            let entries = store.stats_by_bead().map_err(internal_error)?;
            Ok(Json(serde_json::to_value(entries).unwrap()))
        }
        _ => {
            let stats = store.stats().map_err(internal_error)?;
            Ok(Json(serde_json::to_value(stats).unwrap()))
        }
    }
}

// ---------------------------------------------------------------------------
// /feedback/lineage
// ---------------------------------------------------------------------------

/// POST /feedback/lineage — record a lineage action that resolves feedback
pub(super) async fn lineage_store(
    State(state): State<Arc<AppState>>,
    Json(input): Json<crate::storage::feedback::LineageInput>,
) -> Result<
    (StatusCode, Json<crate::storage::feedback::LineageRecord>),
    (StatusCode, Json<ErrorBody>),
> {
    let store = open_feedback_store(&state).map_err(internal_error)?;
    let id = store.store_lineage(&input).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: e.to_string(),
            }),
        )
    })?;
    let records = store
        .list_lineage(&crate::storage::feedback::LineageQuery {
            feedback_id: None,
            bead: None,
            commit_hash: None,
            limit: Some(1),
        })
        .map_err(internal_error)?;
    let record = records.into_iter().find(|r| r.id == id).ok_or_else(|| {
        internal_error(anyhow::anyhow!("Failed to retrieve created lineage record"))
    })?;
    Ok((StatusCode::CREATED, Json(record)))
}

#[derive(Deserialize)]
pub(super) struct LineageListParams {
    feedback_id: Option<i64>,
    bead: Option<String>,
    #[serde(alias = "commit")]
    commit_hash: Option<String>,
    limit: Option<usize>,
}

/// GET /feedback/lineage — list lineage records with optional filters
pub(super) async fn lineage_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LineageListParams>,
) -> Result<Json<Vec<crate::storage::feedback::LineageRecord>>, (StatusCode, Json<ErrorBody>)> {
    let store = open_feedback_store(&state).map_err(internal_error)?;
    let query = crate::storage::feedback::LineageQuery {
        feedback_id: params.feedback_id,
        bead: params.bead,
        commit_hash: params.commit_hash,
        limit: params.limit,
    };
    let records = store.list_lineage(&query).map_err(internal_error)?;
    Ok(Json(records))
}
