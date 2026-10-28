//! HTTP request handlers for the Bobbin REST API.
//!
//! Split into per-resource modules; shared types and the router live here.
#![allow(private_interfaces)]

mod admin;
mod analysis;
mod archive;
mod commands;
mod context;
mod feedback;
mod grep;
mod review;
mod search;
mod search_exec;
mod similar;
mod tags;
mod webhook;

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use serde::Serialize;

use crate::access::RepoFilter;
use crate::config::Config;
use crate::storage::{MetadataStore, VectorStore};
use crate::tags::TagsConfig;
use crate::types::MatchType;

use super::AppState;

// Re-export items referenced by http/mod.rs
pub(super) use self::webhook::resolve_sources;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the axum router with all routes
pub(super) fn router(state: Arc<AppState>) -> axum::Router {
    use axum::routing::{get, post};
    use tower_http::cors::CorsLayer;
    use tower_http::trace::TraceLayer;

    let app = axum::Router::new()
        .route("/", get(ui_page))
        .route("/search", get(search::search))
        .route("/grep", get(grep::grep))
        .route("/context", get(context::context))
        .route("/chunk/{id}", get(search::get_chunk))
        .route("/read", get(context::read_chunk))
        .route("/related", get(analysis::related))
        .route("/refs", get(analysis::find_refs))
        .route("/symbols", get(analysis::list_symbols))
        .route("/hotspots", get(analysis::hotspots))
        .route("/impact", get(analysis::impact))
        .route("/deps", get(analysis::deps))
        .route("/history", get(analysis::history))
        .route("/review", get(review::review))
        .route("/similar", get(similar::similar))
        .route("/prime", get(admin::prime))
        .route("/beads", get(archive::search_beads))
        .route("/archive/search", get(archive::archive_search))
        .route("/archive/entry/{id}", get(archive::archive_entry))
        .route("/archive/recent", get(archive::archive_recent))
        .route("/status", get(admin::status))
        .route("/version", get(admin::version))
        .route("/healthz", get(admin::healthz))
        .route("/repos", get(admin::list_repos))
        .route("/groups", get(admin::list_groups))
        .route("/tags", get(tags::tags))
        .route("/bundles", get(tags::bundles_list))
        .route("/bundles/{name}", get(tags::bundles_show))
        .route("/ontology", get(tags::ontology_list))
        .route("/ontology/{tag}", get(tags::ontology_show))
        .route("/suggest", get(admin::suggest))
        .route("/repos/{name}/files", get(admin::list_repo_files))
        .route("/commands", get(commands::list_commands))
        .route("/metrics", get(admin::metrics))
        .route("/webhook/push", post(webhook::webhook_push))
        .route("/injections", post(feedback::injection_store))
        .route("/injections/{id}", get(feedback::injection_detail))
        .route(
            "/injections/{id}/items",
            post(feedback::injection_items_store),
        )
        .route("/feedback", post(feedback::feedback_submit))
        .route("/feedback", get(feedback::feedback_list))
        .route("/feedback/stats", get(feedback::feedback_stats))
        .route(
            "/feedback/lineage",
            post(feedback::lineage_store).get(feedback::lineage_list),
        )
        .route(
            "/cmd",
            get(commands::list_http_commands).post(commands::register_http_command),
        )
        .route(
            "/cmd/{name}",
            get(commands::invoke_http_command).delete(commands::delete_http_command),
        )
        .with_state(state.clone())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    // Store state-resolved router for /cmd internal dispatch
    let _ = state.inner_router.set(app.clone());

    app
}

/// Serve the embedded web UI
async fn ui_page() -> Html<&'static str> {
    Html(include_str!("../ui.html"))
}

// ---------------------------------------------------------------------------
// Shared error helpers
// ---------------------------------------------------------------------------

/// Error response body
#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

/// Map anyhow errors to HTTP 500 responses
fn internal_error(err: anyhow::Error) -> (StatusCode, Json<ErrorBody>) {
    tracing::error!("Internal error: {:#}", err);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            error: err.to_string(),
        }),
    )
}

fn bad_request(msg: String) -> (StatusCode, Json<ErrorBody>) {
    (StatusCode::BAD_REQUEST, Json(ErrorBody { error: msg }))
}

fn not_found(msg: String) -> (StatusCode, Json<ErrorBody>) {
    (StatusCode::NOT_FOUND, Json(ErrorBody { error: msg }))
}

// ---------------------------------------------------------------------------
// Shared store/filter helpers
// ---------------------------------------------------------------------------

/// Build a RepoFilter from app state and an optional role query param.
fn resolve_filter(state: &AppState, role: Option<&str>) -> RepoFilter {
    let resolved = RepoFilter::resolve_role(role);
    RepoFilter::from_config(&state.config.access, &resolved)
}

/// Resolve a group name to a SQL filter clause, or None if no group specified.
/// Returns Err with a user-facing message if the group name is unknown.
fn resolve_group_filter(state: &AppState, group: Option<&str>) -> Result<Option<String>, String> {
    match group {
        None => Ok(None),
        Some(name) => state.config.group_filter(name).map(Some).ok_or_else(|| {
            let available: Vec<&str> = state
                .config
                .groups
                .iter()
                .map(|g| g.name.as_str())
                .collect();
            if available.is_empty() {
                format!("Unknown group '{}'. No groups configured.", name)
            } else {
                format!(
                    "Unknown group '{}'. Available: {}",
                    name,
                    available.join(", ")
                )
            }
        }),
    }
}

async fn open_vector_store(state: &AppState) -> anyhow::Result<VectorStore> {
    let lance_path = Config::lance_path(&state.repo_root);
    VectorStore::open(&lance_path).await
}

fn open_metadata_store(state: &AppState) -> anyhow::Result<MetadataStore> {
    let db_path = Config::db_path(&state.repo_root);
    MetadataStore::open(&db_path)
}

// ---------------------------------------------------------------------------
// Shared utility functions
// ---------------------------------------------------------------------------

fn detect_language(file: &str) -> String {
    let ext = file.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "cpp" | "cc" | "cxx" | "hpp" | "h" => "cpp",
        "c" => "c",
        "md" => "markdown",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        _ => "unknown",
    }
    .to_string()
}

fn parse_chunk_type(s: &str) -> anyhow::Result<crate::types::ChunkType> {
    use crate::types::ChunkType;
    match s.to_lowercase().as_str() {
        "function" | "func" | "fn" => Ok(ChunkType::Function),
        "method" => Ok(ChunkType::Method),
        "class" => Ok(ChunkType::Class),
        "struct" => Ok(ChunkType::Struct),
        "enum" => Ok(ChunkType::Enum),
        "interface" => Ok(ChunkType::Interface),
        "module" | "mod" => Ok(ChunkType::Module),
        "impl" => Ok(ChunkType::Impl),
        "trait" => Ok(ChunkType::Trait),
        "doc" | "documentation" => Ok(ChunkType::Doc),
        "section" => Ok(ChunkType::Section),
        "table" => Ok(ChunkType::Table),
        "code_block" | "codeblock" => Ok(ChunkType::CodeBlock),
        "commit" => Ok(ChunkType::Commit),
        "issue" | "bead" => Ok(ChunkType::Issue),
        "other" => Ok(ChunkType::Other),
        _ => anyhow::bail!("Unknown chunk type: {}", s),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{}...", t.trim_end())
    }
}

// ---------------------------------------------------------------------------
// Shared response types (used by search + context + review)
// ---------------------------------------------------------------------------

/// A bundle that matched the query keywords.
#[derive(Serialize)]
struct BundleMatchOutput {
    name: String,
    slug: String,
    description: String,
    #[serde(rename = "match")]
    match_reason: String,
    file_count: usize,
    drill: String,
}

/// Find bundles whose keywords match the query.
fn find_matching_bundles(tags_config: &TagsConfig, query: &str) -> Vec<BundleMatchOutput> {
    tags_config
        .match_bundle_keywords(query)
        .into_iter()
        .map(|(bundle, match_reason)| BundleMatchOutput {
            name: bundle.name.clone(),
            slug: bundle.slug(),
            description: bundle.description.clone(),
            match_reason,
            file_count: bundle.member_files().len(),
            drill: format!("bobbin bundle show {}", bundle.name),
        })
        .collect()
}

#[derive(Serialize)]
struct ContextBudgetInfo {
    max_lines: usize,
    used_lines: usize,
}

#[derive(Serialize)]
struct ContextFileOutput {
    path: String,
    language: String,
    relevance: String,
    score: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    coupled_to: Vec<String>,
    chunks: Vec<ContextChunkOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
}

#[derive(Serialize)]
struct ContextChunkOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    chunk_type: String,
    start_line: u32,
    end_line: u32,
    score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Serialize)]
struct ContextSummaryOutput {
    total_files: usize,
    total_chunks: usize,
    direct_hits: usize,
    coupled_additions: usize,
    bridged_additions: usize,
    source_files: usize,
    doc_files: usize,
    /// Raw cosine similarity of the top semantic result (before RRF).
    /// Used by clients for gate_threshold checks.
    top_semantic_score: f32,
    /// Detected query intent (e.g. "BugFix", "Architecture", "Operational").
    /// Clients can use this with gate_boost to apply intent-aware gating.
    #[serde(skip_serializing_if = "Option::is_none")]
    intent: Option<String>,
    /// Recommended additive gate boost for detected intent.
    /// Add to base gate_threshold for intent-aware gating.
    #[serde(skip_serializing_if = "Option::is_none")]
    gate_boost: Option<f32>,
}

fn to_context_file(f: &crate::search::context::ContextFile) -> ContextFileOutput {
    use crate::search::context::FileRelevance;

    ContextFileOutput {
        path: f.path.clone(),
        language: f.language.clone(),
        relevance: match f.relevance {
            FileRelevance::Direct => "direct".to_string(),
            FileRelevance::Coupled => "coupled".to_string(),
            FileRelevance::Bridged => "bridged".to_string(),
            FileRelevance::Pinned => "pinned".to_string(),
            FileRelevance::Knowledge => "knowledge".to_string(),
            FileRelevance::Structural => "structural".to_string(),
        },
        score: f.score,
        coupled_to: f.coupled_to.clone(),
        chunks: f
            .chunks
            .iter()
            .map(|c| ContextChunkOutput {
                name: c.name.clone(),
                chunk_type: c.chunk_type.to_string(),
                start_line: c.start_line,
                end_line: c.end_line,
                score: c.score,
                match_type: c.match_type.map(|mt| match mt {
                    MatchType::Semantic => "semantic".to_string(),
                    MatchType::Keyword => "keyword".to_string(),
                    MatchType::Hybrid => "hybrid".to_string(),
                }),
                content: c.content.clone(),
            })
            .collect(),
        repo: f.repo.clone(),
    }
}

fn to_context_summary(s: &crate::search::context::ContextSummary) -> ContextSummaryOutput {
    ContextSummaryOutput {
        total_files: s.total_files,
        total_chunks: s.total_chunks,
        direct_hits: s.direct_hits,
        coupled_additions: s.coupled_additions,
        bridged_additions: s.bridged_additions,
        source_files: s.source_files,
        doc_files: s.doc_files,
        top_semantic_score: s.top_semantic_score,
        intent: None,
        gate_boost: None,
    }
}

/// A short, non-reversible fingerprint of user-supplied text (a search query,
/// a hook prompt) for INFO logs: the first 12 hex of its sha256. INFO lines
/// carry this and the length, never the text, so a journal scan for words
/// such as `oom-kill` cannot match a search FOR them, and prompts do not land
/// in the shared journal. The text itself is logged at DEBUG with the same
/// fingerprint, so the two lines correlate (aegis-uy7boa).
pub(crate) fn query_sha(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod query_log_tests;
