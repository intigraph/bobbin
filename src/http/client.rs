//! HTTP client for thin-client mode.
//!
//! When `--server <url>` is passed to the CLI, commands proxy through
//! a remote Bobbin HTTP server instead of opening local stores.

use anyhow::{Context, Result};
use serde::Deserialize;

/// HTTP client that proxies CLI commands to a remote Bobbin server.
pub struct Client {
    base_url: String,
    http: reqwest::Client,
}

/// Response from the /search endpoint
#[derive(Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub mode: String,
    pub count: usize,
    pub results: Vec<SearchResultItem>,
}

/// A single search result item from the server
#[derive(Deserialize)]
pub struct SearchResultItem {
    pub file_path: String,
    pub name: Option<String>,
    pub chunk_type: String,
    pub source: Option<String>,
    pub repo: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    pub match_type: Option<String>,
    pub language: String,
    pub content_preview: String,
}

/// Response from the /status endpoint
#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    pub status: String,
    pub index: IndexStats,
}

/// Index stats from the server
#[derive(Debug, Deserialize)]
pub struct IndexStats {
    pub total_files: u64,
    pub total_chunks: u64,
    pub total_embeddings: u64,
    pub languages: Vec<LanguageStats>,
    pub last_indexed: Option<i64>,
    pub index_size_bytes: u64,
}

/// Per-language statistics
#[derive(Debug, Deserialize)]
pub struct LanguageStats {
    pub language: String,
    pub file_count: u64,
    pub chunk_count: u64,
}

/// Response from the /chunk/{id} endpoint
#[derive(Deserialize)]
pub struct ChunkResponse {
    pub id: String,
    pub file_path: String,
    pub chunk_type: String,
    pub name: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub language: String,
    pub content: String,
}

/// Response from the /grep endpoint
#[derive(Deserialize)]
pub struct GrepResponse {
    pub pattern: String,
    pub count: usize,
    pub results: Vec<GrepResultItem>,
}

/// A single grep result item
#[derive(Deserialize)]
pub struct GrepResultItem {
    pub file_path: String,
    pub name: Option<String>,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    pub language: String,
    pub content_preview: String,
    pub matching_lines: Vec<GrepMatchingLine>,
}

/// A matching line from grep
#[derive(Deserialize)]
pub struct GrepMatchingLine {
    pub line_number: u32,
    pub content: String,
}

/// Response from the /context endpoint
#[derive(Deserialize)]
pub struct ContextResponse {
    pub query: String,
    pub budget: ContextBudgetInfo,
    pub files: Vec<ContextFileOutput>,
    pub summary: ContextSummaryOutput,
}

#[derive(Deserialize)]
pub struct ContextBudgetInfo {
    pub max_lines: usize,
    pub used_lines: usize,
}

#[derive(Deserialize)]
pub struct ContextFileOutput {
    pub path: String,
    pub language: String,
    pub relevance: String,
    pub score: f32,
    #[serde(default)]
    pub coupled_to: Vec<String>,
    pub chunks: Vec<ContextChunkOutput>,
    /// Repository name this file belongs to (from index metadata)
    #[serde(default)]
    pub repo: Option<String>,
}

#[derive(Deserialize)]
pub struct ContextChunkOutput {
    pub name: Option<String>,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    pub match_type: Option<String>,
    pub content: Option<String>,
}

#[derive(Deserialize)]
pub struct ContextSummaryOutput {
    pub total_files: usize,
    pub total_chunks: usize,
    pub direct_hits: usize,
    pub coupled_additions: usize,
    pub bridged_additions: usize,
    pub source_files: usize,
    pub doc_files: usize,
    /// Raw cosine similarity of the top semantic result (before RRF).
    /// Missing from older servers, defaults to 0.0.
    #[serde(default)]
    pub top_semantic_score: f32,
    /// Detected query intent (e.g. "BugFix", "Operational").
    #[serde(default)]
    pub intent: Option<String>,
    /// Recommended gate boost for detected intent.
    #[serde(default)]
    pub gate_boost: Option<f32>,
    /// Files the injection HELD BACK — dropped by the chunks cap or cut off by
    /// the line budget before they could be rendered.
    ///
    /// Both caps existed already; neither told the model. The chunks cap wrote
    /// its count to stderr, which a hook's stdout-only injection channel never
    /// carries, and the budget stop emitted no marker at all. So an agent read
    /// a context block that looked complete and had no way to learn otherwise —
    /// the retrieval-side version of a gate believed to be passing.
    ///
    /// Zero means nothing was held back, and that is a claim worth being able
    /// to make: an omitted field would read as "complete" only by luck.
    #[serde(default)]
    pub omitted_files: usize,
}

/// Response from the /read endpoint
#[derive(Deserialize)]
pub struct ReadChunkResponse {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub actual_start_line: u32,
    pub actual_end_line: u32,
    pub content: String,
    pub language: String,
}

/// Response from the /related endpoint
#[derive(Deserialize)]
pub struct RelatedResponse {
    pub file: String,
    pub related: Vec<RelatedFile>,
}

#[derive(Deserialize)]
pub struct RelatedFile {
    pub path: String,
    pub score: f32,
    pub co_changes: u32,
}

/// Response from the /refs endpoint
#[derive(Deserialize)]
pub struct FindRefsResponse {
    pub symbol: String,
    pub definition: Option<SymbolDefinitionOutput>,
    pub usage_count: usize,
    pub usages: Vec<SymbolUsageOutput>,
}

#[derive(Deserialize)]
pub struct SymbolDefinitionOutput {
    pub name: String,
    pub chunk_type: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: String,
}

#[derive(Deserialize)]
pub struct SymbolUsageOutput {
    pub file_path: String,
    pub line: u32,
    pub context: String,
}

/// Response from the /symbols endpoint
#[derive(Deserialize)]
pub struct ListSymbolsResponse {
    pub file: String,
    pub count: usize,
    pub symbols: Vec<SymbolItemOutput>,
}

#[derive(Deserialize)]
pub struct SymbolItemOutput {
    pub name: String,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: String,
}

/// Response from the /hotspots endpoint
#[derive(Deserialize)]
pub struct HotspotsResponse {
    pub count: usize,
    pub since: String,
    pub hotspots: Vec<HotspotItem>,
}

#[derive(Deserialize)]
pub struct HotspotItem {
    pub file: String,
    pub score: f32,
    pub churn: u32,
    pub complexity: f32,
    pub language: String,
}

/// Response from the /impact endpoint
#[derive(Deserialize)]
pub struct ImpactResponse {
    pub target: String,
    pub mode: String,
    pub depth: u32,
    pub count: usize,
    pub results: Vec<ImpactResultItem>,
}

#[derive(Deserialize)]
pub struct ImpactResultItem {
    pub file: String,
    pub signal: String,
    pub score: f32,
    pub reason: String,
}

/// Response from the /review endpoint
#[derive(Deserialize)]
pub struct ReviewResponse {
    pub diff_description: String,
    pub changed_files: Vec<ReviewChangedFile>,
    pub budget: ContextBudgetInfo,
    pub files: Vec<ContextFileOutput>,
    pub summary: ContextSummaryOutput,
}

#[derive(Deserialize)]
pub struct ReviewChangedFile {
    pub path: String,
    pub status: String,
    pub added_lines: usize,
    pub removed_lines: usize,
}

/// Response from the /similar endpoint
#[derive(Deserialize)]
pub struct SimilarResponse {
    pub mode: String,
    pub threshold: f32,
    pub target: Option<String>,
    pub count: usize,
    pub results: Vec<SimilarResultItem>,
    pub clusters: Vec<SimilarClusterItem>,
}

#[derive(Deserialize)]
pub struct SimilarResultItem {
    pub file_path: String,
    pub name: Option<String>,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub similarity: f32,
    pub language: String,
    pub explanation: String,
}

#[derive(Deserialize)]
pub struct SimilarClusterItem {
    pub representative: SimilarChunkRef,
    pub avg_similarity: f32,
    pub member_count: usize,
    pub members: Vec<SimilarResultItem>,
}

#[derive(Deserialize)]
pub struct SimilarChunkRef {
    pub file_path: String,
    pub name: Option<String>,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub language: String,
}

/// Response from the /prime endpoint
#[derive(Deserialize)]
pub struct PrimeResponse {
    pub primer: String,
    pub section: Option<String>,
    pub initialized: bool,
    pub stats: Option<PrimeStats>,
}

#[derive(Deserialize)]
pub struct PrimeStats {
    pub total_files: u64,
    pub total_chunks: u64,
    pub total_embeddings: u64,
    pub languages: Vec<PrimeLanguageStats>,
    pub last_indexed: Option<String>,
}

#[derive(Deserialize)]
pub struct PrimeLanguageStats {
    pub language: String,
    pub file_count: u64,
    pub chunk_count: u64,
}

mod beads;
pub use beads::{BeadResultItem, SearchBeadsResponse};

/// A feedback record from the server
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct FeedbackRecord {
    pub id: i64,
    pub injection_id: String,
    pub timestamp: String,
    pub agent: String,
    pub rating: String,
    pub reason: String,
}

/// Aggregated feedback statistics from the server
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct FeedbackStats {
    pub total_injections: u64,
    pub total_feedback: u64,
    pub useful: u64,
    pub noise: u64,
    pub harmful: u64,
    #[serde(default)]
    pub actioned: u64,
    #[serde(default)]
    pub unactioned: u64,
    #[serde(default)]
    pub lineage_records: u64,
}

/// Feedback statistics grouped by bundle or bead
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct GroupedStatsEntry {
    pub key: String,
    pub injections: u64,
    pub feedback: u64,
    pub useful: u64,
    pub noise: u64,
    pub harmful: u64,
}

/// A lineage record from the server
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct LineageRecord {
    pub id: i64,
    pub timestamp: String,
    pub action_type: String,
    pub bead: Option<String>,
    pub commit_hash: Option<String>,
    pub description: String,
    pub agent: Option<String>,
    #[serde(default)]
    pub feedback_ids: Vec<i64>,
}

/// Error response from the server
#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

impl Client {
    /// Create a new client pointing at the given server URL.
    pub fn new(base_url: &str) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            base_url,
            http: reqwest::Client::new(),
        }
    }

    /// Search via the remote server.
    pub async fn search(
        &self,
        query: &str,
        mode: &str,
        chunk_type: Option<&str>,
        limit: usize,
        repo: Option<&str>,
        role: Option<&str>,
    ) -> Result<SearchResponse> {
        let url = format!("{}/search", self.base_url);
        let mut params: Vec<(&str, String)> = vec![
            ("q", query.to_string()),
            ("mode", mode.to_string()),
            ("limit", limit.to_string()),
        ];
        if let Some(t) = chunk_type {
            params.push(("type", t.to_string()));
        }
        if let Some(r) = repo {
            params.push(("repo", r.to_string()));
        }
        if let Some(r) = role {
            params.push(("role", r.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Get index status from the remote server.
    pub async fn status(&self) -> Result<StatusResponse> {
        let url = format!("{}/status", self.base_url);
        self.get_json::<StatusResponse>(&url, &[]).await
    }

    /// Get a specific chunk by ID from the remote server.
    pub async fn get_chunk(&self, id: &str) -> Result<ChunkResponse> {
        let url = format!("{}/chunk/{}", self.base_url, id);
        self.get_json::<ChunkResponse>(&url, &[]).await
    }

    /// Grep via the remote server.
    pub async fn grep(
        &self,
        pattern: &str,
        ignore_case: bool,
        regex: bool,
        chunk_type: Option<&str>,
        limit: usize,
        repo: Option<&str>,
    ) -> Result<GrepResponse> {
        let url = format!("{}/grep", self.base_url);
        let mut params: Vec<(&str, String)> = vec![
            ("pattern", pattern.to_string()),
            ("limit", limit.to_string()),
        ];
        if ignore_case {
            params.push(("ignore_case", "true".to_string()));
        }
        if regex {
            params.push(("regex", "true".to_string()));
        }
        if let Some(t) = chunk_type {
            params.push(("type", t.to_string()));
        }
        if let Some(r) = repo {
            params.push(("repo", r.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Assemble context via the remote server.
    pub async fn context(
        &self,
        query: &str,
        budget: Option<usize>,
        depth: Option<u32>,
        max_coupled: Option<usize>,
        limit: Option<usize>,
        coupling_threshold: Option<f32>,
        repo: Option<&str>,
        role: Option<&str>,
        repo_affinity: Option<&str>,
    ) -> Result<ContextResponse> {
        self.context_with_weights(
            query,
            budget,
            depth,
            max_coupled,
            limit,
            coupling_threshold,
            repo,
            role,
            repo_affinity,
            None,
            None,
            None,
        )
        .await
    }

    /// Assemble context with per-request scoring weight overrides.
    pub async fn context_with_weights(
        &self,
        query: &str,
        budget: Option<usize>,
        depth: Option<u32>,
        max_coupled: Option<usize>,
        limit: Option<usize>,
        coupling_threshold: Option<f32>,
        repo: Option<&str>,
        role: Option<&str>,
        repo_affinity: Option<&str>,
        semantic_weight: Option<f32>,
        doc_demotion: Option<f32>,
        recency_weight: Option<f32>,
    ) -> Result<ContextResponse> {
        let url = format!("{}/context", self.base_url);
        let mut params: Vec<(&str, String)> = vec![("q", query.to_string())];
        if let Some(b) = budget {
            params.push(("budget", b.to_string()));
        }
        if let Some(d) = depth {
            params.push(("depth", d.to_string()));
        }
        if let Some(mc) = max_coupled {
            params.push(("max_coupled", mc.to_string()));
        }
        if let Some(l) = limit {
            params.push(("limit", l.to_string()));
        }
        if let Some(ct) = coupling_threshold {
            params.push(("coupling_threshold", ct.to_string()));
        }
        if let Some(r) = repo {
            params.push(("repo", r.to_string()));
        }
        if let Some(r) = role {
            params.push(("role", r.to_string()));
        }
        if let Some(ra) = repo_affinity {
            params.push(("repo_affinity", ra.to_string()));
        }
        if let Some(sw) = semantic_weight {
            params.push(("semantic_weight", sw.to_string()));
        }
        if let Some(dd) = doc_demotion {
            params.push(("doc_demotion", dd.to_string()));
        }
        if let Some(rw) = recency_weight {
            params.push(("recency_weight", rw.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Read a file chunk via the remote server.
    pub async fn read_chunk(
        &self,
        file: &str,
        start_line: u32,
        end_line: u32,
        context: Option<u32>,
    ) -> Result<ReadChunkResponse> {
        let url = format!("{}/read", self.base_url);
        let mut params: Vec<(&str, String)> = vec![
            ("file", file.to_string()),
            ("start_line", start_line.to_string()),
            ("end_line", end_line.to_string()),
        ];
        if let Some(c) = context {
            params.push(("context", c.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Find related files via the remote server.
    pub async fn related(
        &self,
        file: &str,
        limit: usize,
        threshold: Option<f32>,
    ) -> Result<RelatedResponse> {
        let url = format!("{}/related", self.base_url);
        let mut params: Vec<(&str, String)> =
            vec![("file", file.to_string()), ("limit", limit.to_string())];
        if let Some(t) = threshold {
            params.push(("threshold", t.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Find symbol references via the remote server.
    pub async fn find_refs(
        &self,
        symbol: &str,
        symbol_type: Option<&str>,
        limit: usize,
        repo: Option<&str>,
    ) -> Result<FindRefsResponse> {
        let url = format!("{}/refs", self.base_url);
        let mut params: Vec<(&str, String)> =
            vec![("symbol", symbol.to_string()), ("limit", limit.to_string())];
        if let Some(t) = symbol_type {
            params.push(("type", t.to_string()));
        }
        if let Some(r) = repo {
            params.push(("repo", r.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// List symbols in a file via the remote server.
    pub async fn list_symbols(
        &self,
        file: &str,
        repo: Option<&str>,
    ) -> Result<ListSymbolsResponse> {
        let url = format!("{}/symbols", self.base_url);
        let mut params: Vec<(&str, String)> = vec![("file", file.to_string())];
        if let Some(r) = repo {
            params.push(("repo", r.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Get hotspots via the remote server.
    pub async fn hotspots(
        &self,
        since: Option<&str>,
        limit: usize,
        threshold: Option<f32>,
    ) -> Result<HotspotsResponse> {
        let url = format!("{}/hotspots", self.base_url);
        let mut params: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(s) = since {
            params.push(("since", s.to_string()));
        }
        if let Some(t) = threshold {
            params.push(("threshold", t.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Run impact analysis via the remote server.
    pub async fn impact(
        &self,
        target: &str,
        depth: Option<u32>,
        mode: Option<&str>,
        limit: usize,
        threshold: Option<f32>,
        repo: Option<&str>,
    ) -> Result<ImpactResponse> {
        let url = format!("{}/impact", self.base_url);
        let mut params: Vec<(&str, String)> =
            vec![("target", target.to_string()), ("limit", limit.to_string())];
        if let Some(d) = depth {
            params.push(("depth", d.to_string()));
        }
        if let Some(m) = mode {
            params.push(("mode", m.to_string()));
        }
        if let Some(t) = threshold {
            params.push(("threshold", t.to_string()));
        }
        if let Some(r) = repo {
            params.push(("repo", r.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Get review context via the remote server.
    pub async fn review(
        &self,
        diff: Option<&str>,
        budget: Option<usize>,
        depth: Option<u32>,
        repo: Option<&str>,
    ) -> Result<ReviewResponse> {
        let url = format!("{}/review", self.base_url);
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(d) = diff {
            params.push(("diff", d.to_string()));
        }
        if let Some(b) = budget {
            params.push(("budget", b.to_string()));
        }
        if let Some(d) = depth {
            params.push(("depth", d.to_string()));
        }
        if let Some(r) = repo {
            params.push(("repo", r.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Find similar code via the remote server.
    pub async fn similar(
        &self,
        target: Option<&str>,
        scan: bool,
        threshold: Option<f32>,
        limit: usize,
        repo: Option<&str>,
        cross_repo: bool,
    ) -> Result<SimilarResponse> {
        let url = format!("{}/similar", self.base_url);
        let mut params: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(t) = target {
            params.push(("target", t.to_string()));
        }
        if scan {
            params.push(("scan", "true".to_string()));
        }
        if let Some(t) = threshold {
            params.push(("threshold", t.to_string()));
        }
        if let Some(r) = repo {
            params.push(("repo", r.to_string()));
        }
        if cross_repo {
            params.push(("cross_repo", "true".to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Get project primer via the remote server.
    pub async fn prime(&self, section: Option<&str>, brief: bool) -> Result<PrimeResponse> {
        let url = format!("{}/prime", self.base_url);
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(s) = section {
            params.push(("section", s.to_string()));
        }
        if brief {
            params.push(("brief", "true".to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Search beads via the remote server.
    #[allow(clippy::too_many_arguments)]
    pub async fn search_beads(
        &self,
        query: &str,
        priority: Option<i32>,
        status: Option<&str>,
        assignee: Option<&str>,
        rig: Option<&str>,
        issue_type: Option<&str>,
        label: Option<&str>,
        limit: usize,
        enrich: Option<bool>,
        compact: Option<bool>,
    ) -> Result<SearchBeadsResponse> {
        let url = format!("{}/beads", self.base_url);
        let mut params: Vec<(&str, String)> =
            vec![("q", query.to_string()), ("limit", limit.to_string())];
        if let Some(p) = priority {
            params.push(("priority", p.to_string()));
        }
        if let Some(s) = status {
            params.push(("status", s.to_string()));
        }
        if let Some(a) = assignee {
            params.push(("assignee", a.to_string()));
        }
        if let Some(r) = rig {
            params.push(("rig", r.to_string()));
        }
        if let Some(t) = issue_type {
            params.push(("issue_type", t.to_string()));
        }
        if let Some(l) = label {
            params.push(("label", l.to_string()));
        }
        if let Some(e) = enrich {
            params.push(("enrich", e.to_string()));
        }
        if let Some(c) = compact {
            params.push(("compact", c.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Store an injection record on the server (for feedback reference).
    /// Best-effort: callers should ignore errors to avoid blocking the hook.
    pub async fn store_injection(
        &self,
        injection_id: &str,
        session_id: Option<&str>,
        agent: Option<&str>,
        query: &str,
        files: &[String],
        total_chunks: usize,
        budget_lines: usize,
    ) -> Result<()> {
        self.store_injection_with_output(
            injection_id,
            session_id,
            agent,
            query,
            files,
            total_chunks,
            budget_lines,
            None,
        )
        .await
    }

    pub async fn store_injection_with_output(
        &self,
        injection_id: &str,
        session_id: Option<&str>,
        agent: Option<&str>,
        query: &str,
        files: &[String],
        total_chunks: usize,
        budget_lines: usize,
        formatted_output: Option<&str>,
    ) -> Result<()> {
        self.store_scoped_injection_with_output(
            injection_id,
            session_id,
            agent,
            query,
            files,
            total_chunks,
            budget_lines,
            formatted_output,
            None,
            None,
            None,
        )
        .await
    }

    /// Store an injection with a verified NA grounding scope.
    #[allow(clippy::too_many_arguments)]
    pub async fn store_scoped_injection_with_output(
        &self,
        injection_id: &str,
        session_id: Option<&str>,
        agent: Option<&str>,
        query: &str,
        files: &[String],
        total_chunks: usize,
        budget_lines: usize,
        formatted_output: Option<&str>,
        scope: Option<&str>,
        faction_id: Option<&str>,
        grounding_id: Option<&str>,
    ) -> Result<()> {
        let url = format!("{}/injections", self.base_url);
        let mut body = serde_json::json!({
            "injection_id": injection_id,
            "session_id": session_id,
            "agent": agent,
            "query": query,
            "files": files,
            "total_chunks": total_chunks,
            "budget_lines": budget_lines,
        });
        if let Some(out) = formatted_output {
            body["formatted_output"] = serde_json::Value::String(out.to_string());
        }
        if let Some(value) = scope {
            body["scope"] = serde_json::Value::String(value.to_string());
        }
        if let Some(value) = faction_id {
            body["faction_id"] = serde_json::Value::String(value.to_string());
        }
        if let Some(value) = grounding_id {
            body["grounding_id"] = serde_json::Value::String(value.to_string());
        }
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to store injection")?;
        if !resp.status().is_success() {
            let status = resp.status();
            anyhow::bail!("Failed to store injection: HTTP {}", status);
        }
        Ok(())
    }

    /// Invoke an HTTP command by name with key=value params.
    /// Returns the raw JSON response from the /cmd/{name} endpoint.
    pub async fn invoke_command(
        &self,
        name: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value> {
        let url = format!("{}/cmd/{}", self.base_url, name);
        self.get_json(&url, params).await
    }

    /// List HTTP commands registered on the server.
    pub async fn list_http_commands(&self) -> Result<serde_json::Value> {
        let url = format!("{}/cmd", self.base_url);
        self.get_json(&url, &[]).await
    }

    /// Submit feedback on an injection via the remote server.
    pub async fn feedback_submit(
        &self,
        injection_id: &str,
        rating: &str,
        reason: &str,
        agent: &str,
    ) -> Result<serde_json::Value> {
        self.post_value(
            "/feedback",
            &serde_json::json!({
                "injection_id": injection_id,
                "rating": rating,
                "reason": reason,
                "agent": agent,
            }),
        )
        .await
    }

    /// List feedback records via the remote server.
    pub async fn feedback_list(
        &self,
        injection_id: Option<&str>,
        rating: Option<&str>,
        agent: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<FeedbackRecord>> {
        let url = format!("{}/feedback", self.base_url);
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(id) = injection_id {
            params.push(("injection_id", id.to_string()));
        }
        if let Some(r) = rating {
            params.push(("rating", r.to_string()));
        }
        if let Some(a) = agent {
            params.push(("agent", a.to_string()));
        }
        if let Some(l) = limit {
            params.push(("limit", l.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Get aggregated feedback statistics via the remote server.
    pub async fn feedback_stats(&self) -> Result<FeedbackStats> {
        let url = format!("{}/feedback/stats", self.base_url);
        self.get_json::<FeedbackStats>(&url, &[]).await
    }

    /// Get feedback statistics grouped by bundle or bead via the remote server.
    pub async fn feedback_stats_grouped(&self, group_by: &str) -> Result<Vec<GroupedStatsEntry>> {
        let url = format!("{}/feedback/stats", self.base_url);
        self.get_json::<Vec<GroupedStatsEntry>>(&url, &[("group_by", group_by.to_string())])
            .await
    }

    /// Store a lineage record via the remote server.
    pub async fn lineage_store(
        &self,
        feedback_ids: &[i64],
        action_type: &str,
        bead: Option<&str>,
        commit_hash: Option<&str>,
        description: &str,
        agent: Option<&str>,
    ) -> Result<LineageRecord> {
        let value = self
            .post_value(
                "/feedback/lineage",
                &serde_json::json!({
                    "feedback_ids": feedback_ids,
                    "action_type": action_type,
                    "bead": bead,
                    "commit_hash": commit_hash,
                    "description": description,
                    "agent": agent,
                }),
            )
            .await?;
        serde_json::from_value(value).context("Failed to parse lineage response")
    }

    /// List lineage records via the remote server.
    pub async fn lineage_list(
        &self,
        feedback_id: Option<i64>,
        bead: Option<&str>,
        commit_hash: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<LineageRecord>> {
        let url = format!("{}/feedback/lineage", self.base_url);
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(id) = feedback_id {
            params.push(("feedback_id", id.to_string()));
        }
        if let Some(b) = bead {
            params.push(("bead", b.to_string()));
        }
        if let Some(c) = commit_hash {
            params.push(("commit_hash", c.to_string()));
        }
        if let Some(l) = limit {
            params.push(("limit", l.to_string()));
        }
        self.get_json(&url, &params).await
    }

    /// Return the base URL (for display/logging).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Fetch one stored injection: its candidate file list and the block that
    /// was actually rendered. `hook expand` derives the withheld set from those
    /// two rather than from a second record of what was dropped — one source,
    /// so there is nothing to disagree with.
    pub async fn injection_detail(&self, injection_id: &str) -> Result<serde_json::Value> {
        let url = format!("{}/injections/{}", self.base_url, injection_id);
        self.get_json(&url, &[]).await
    }

    /// GET an endpoint and return the server's own JSON, untouched.
    ///
    /// For `mcp::remote`. The typed helpers above model only the fields and
    /// params the CLI exposes, so routing an MCP tool through one of them
    /// would silently narrow both the request and the response (aegis-wbbycq).
    pub async fn get_value(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        self.get_json(&url, params).await
    }

    /// POST a JSON body to an endpoint and return the server's own JSON.
    pub async fn post_value(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .await
            .context("Failed to connect to bobbin server")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body: ErrorBody = resp.json().await.unwrap_or(ErrorBody {
                error: format!("HTTP {}", status),
            });
            anyhow::bail!("Server error ({}): {}", status, body.error);
        }

        resp.json().await.context("Failed to parse server response")
    }

    /// Internal helper: GET with query params, parse JSON response.
    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        params: &[(&str, String)],
    ) -> Result<T> {
        let resp = self
            .http
            .get(url)
            .query(params)
            .send()
            .await
            .context("Failed to connect to bobbin server")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body: ErrorBody = resp.json().await.unwrap_or(ErrorBody {
                error: format!("HTTP {}", status),
            });
            anyhow::bail!("Server error ({}): {}", status, body.error);
        }

        resp.json().await.context("Failed to parse server response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_strips_trailing_slash() {
        let client = Client::new("http://localhost:3030/");
        assert_eq!(client.base_url(), "http://localhost:3030");
    }

    #[test]
    fn test_client_preserves_url_without_trailing_slash() {
        let client = Client::new("http://localhost:3030");
        assert_eq!(client.base_url(), "http://localhost:3030");
    }

    #[test]
    fn test_deserialize_search_response() {
        let json = r#"{
            "query": "test",
            "mode": "hybrid",
            "count": 1,
            "results": [{
                "file_path": "src/main.rs",
                "name": "main",
                "chunk_type": "function",
                "start_line": 1,
                "end_line": 10,
                "score": 0.95,
                "match_type": "semantic",
                "language": "rust",
                "content_preview": "fn main() {}"
            }]
        }"#;
        let resp: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.query, "test");
        assert_eq!(resp.mode, "hybrid");
        assert_eq!(resp.count, 1);
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].file_path, "src/main.rs");
        assert_eq!(resp.results[0].score, 0.95);
    }

    #[test]
    fn test_deserialize_status_response() {
        let json = r#"{
            "status": "ok",
            "index": {
                "total_files": 42,
                "total_chunks": 100,
                "total_embeddings": 100,
                "languages": [{"language": "rust", "file_count": 42, "chunk_count": 100}],
                "last_indexed": 1700000000,
                "index_size_bytes": 1024
            }
        }"#;
        let resp: StatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.index.total_files, 42);
        assert_eq!(resp.index.total_chunks, 100);
        assert_eq!(resp.index.languages.len(), 1);
        assert_eq!(resp.index.languages[0].language, "rust");
    }

    #[test]
    fn test_deserialize_search_response_null_optional_fields() {
        let json = r#"{
            "query": "test",
            "mode": "keyword",
            "count": 1,
            "results": [{
                "file_path": "src/lib.rs",
                "name": null,
                "chunk_type": "function",
                "start_line": 5,
                "end_line": 15,
                "score": 0.5,
                "match_type": null,
                "language": "rust",
                "content_preview": "pub fn foo()"
            }]
        }"#;
        let resp: SearchResponse = serde_json::from_str(json).unwrap();
        assert!(resp.results[0].name.is_none());
        assert!(resp.results[0].match_type.is_none());
    }

    #[tokio::test]
    async fn test_client_connection_refused() {
        let client = Client::new("http://127.0.0.1:19999");
        let result = client.status().await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Failed to connect") || err.contains("error"),
            "Unexpected error: {}",
            err
        );
    }
}
