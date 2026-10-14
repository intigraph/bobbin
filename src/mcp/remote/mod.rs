//! Remote backend for the MCP server.
//!
//! `bobbin serve` used to serve only a repo-local index, whatever the config
//! said. `bobbin connect --global <url>` configured a remote, the
//! CLI honoured it, and the MCP server ignored it — so in a directory with no
//! local index `bobbin serve` refused to start at all, and in one with a stale
//! or near-empty index it silently answered from the wrong corpus while the
//! CLI answered the same query from the fleet index (aegis-wbbycq).
//!
//! This module is the other half of the fix: when a server URL resolves, each
//! MCP tool that has a server endpoint is answered by that endpoint instead of
//! by a local store. The proxy deliberately returns the server's own JSON
//! rather than re-projecting it — see `Client::get_value` for why.
//!
//! Tools with no server endpoint (`test_coverage`, `chunk_neighbors`,
//! `commit_search`) are NOT proxied. They keep working against a local index
//! when one exists, and say so individually when one does not, rather than the
//! whole server refusing to start.

use rmcp::model::{CallToolResult, Content};
use rmcp::ErrorData as McpError;

use super::tools::*;
use crate::http::client::Client;

/// A configured remote bobbin server, and the tool calls that route to it.
pub(super) struct RemoteBackend {
    client: Client,
    url: String,
    /// Access-filter role, resolved exactly as the CLI resolves it. Forwarded
    /// to every endpoint that accepts `role`; omitting it would widen results
    /// beyond what the same query returns from the CLI, which is the whole
    /// point of routing these tools through the server at all.
    role: String,
}

#[cfg(test)]
mod beads_tests;
/// Accumulates query parameters, skipping absent ones.
mod params;
#[cfg(test)]
#[path = "params_tests.rs"]
mod params_tests;

use params::*;

impl RemoteBackend {
    pub(super) fn new(url: String, role: String) -> Self {
        let client = Client::new(&url);
        let url = client.base_url().to_string();
        Self { client, url, role }
    }

    pub(super) fn url(&self) -> &str {
        &self.url
    }

    fn role_param(&self, params: Params) -> Params {
        params.set("role", &self.role)
    }

    fn ok(value: serde_json::Value) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_string_pretty(&value)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Errors name the server, because the whole class of bug this module
    /// fixes is a tool answering from a corpus the caller did not expect.
    /// "connection refused" without the URL leaves the reader unable to tell
    /// a dead remote from a misconfigured one.
    fn fail(&self, tool: &str, err: anyhow::Error) -> McpError {
        McpError::internal_error(
            format!(
                "bobbin {} could not reach the configured server {}: {:#}",
                tool, self.url, err
            ),
            None,
        )
    }

    async fn get(
        &self,
        tool: &str,
        path: &str,
        params: Params,
    ) -> Result<CallToolResult, McpError> {
        match self.client.get_value(path, &params.into_vec()).await {
            Ok(value) => Self::ok(value),
            Err(e) => Err(self.fail(tool, e)),
        }
    }

    async fn post(
        &self,
        tool: &str,
        path: &str,
        body: serde_json::Value,
    ) -> Result<CallToolResult, McpError> {
        match self.client.post_value(path, &body).await {
            Ok(value) => Self::ok(value),
            Err(e) => Err(self.fail(tool, e)),
        }
    }

    // -----------------------------------------------------------------------
    // Search & retrieval
    // -----------------------------------------------------------------------

    pub(super) async fn search(&self, req: &SearchRequest) -> Result<CallToolResult, McpError> {
        let p = search_params(req);
        self.get("search", "/search", self.role_param(p)).await
    }

    pub(super) async fn grep(&self, req: &GrepRequest) -> Result<CallToolResult, McpError> {
        let p = grep_params(req);
        self.get("grep", "/grep", self.role_param(p)).await
    }

    pub(super) async fn context(&self, req: &ContextRequest) -> Result<CallToolResult, McpError> {
        let p = context_params(req);
        self.get("context", "/context", self.role_param(p)).await
    }

    pub(super) async fn read_chunk(
        &self,
        req: &ReadChunkRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = read_chunk_params(req);
        self.get("read_chunk", "/read", p).await
    }

    // -----------------------------------------------------------------------
    // Structure & history
    // -----------------------------------------------------------------------

    pub(super) async fn related(&self, req: &RelatedRequest) -> Result<CallToolResult, McpError> {
        let p = related_params(req);
        self.get("related", "/related", self.role_param(p)).await
    }

    pub(super) async fn find_refs(
        &self,
        req: &FindRefsRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = find_refs_params(req);
        self.get("find_refs", "/refs", self.role_param(p)).await
    }

    pub(super) async fn list_symbols(
        &self,
        req: &ListSymbolsRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = list_symbols_params(req);
        self.get("list_symbols", "/symbols", self.role_param(p))
            .await
    }

    pub(super) async fn dependencies(
        &self,
        req: &DependenciesRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = dependencies_params(req);
        self.get("dependencies", "/deps", self.role_param(p)).await
    }

    pub(super) async fn file_history(
        &self,
        req: &FileHistoryRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = file_history_params(req);
        self.get("file_history", "/history", self.role_param(p))
            .await
    }

    pub(super) async fn hotspots(&self, req: &HotspotsRequest) -> Result<CallToolResult, McpError> {
        let p = hotspots_params(req);
        self.get("hotspots", "/hotspots", self.role_param(p)).await
    }

    pub(super) async fn impact(&self, req: &ImpactRequest) -> Result<CallToolResult, McpError> {
        let p = impact_params(req);
        self.get("impact", "/impact", self.role_param(p)).await
    }

    pub(super) async fn similar(&self, req: &SimilarRequest) -> Result<CallToolResult, McpError> {
        let p = similar_params(req);
        self.get("similar", "/similar", self.role_param(p)).await
    }

    /// `review` diffs the CALLER's working tree, which a remote server cannot
    /// see. The endpoint exists and diffs the server's own checkout, so an
    /// unqualified proxy here would answer a different question than the one
    /// asked — the exact failure mode of aegis-wbbycq. Left to the caller as a
    /// named refusal instead.
    pub(super) fn review_unavailable(&self) -> McpError {
        McpError::invalid_params(
            format!(
                "review inspects the working tree of the machine it runs on, and this MCP \
                 server is proxying to {}. The remote server would diff ITS checkout, not \
                 yours. Run `bobbin review` in your repo, or run this MCP server against a \
                 local index (`bobbin init` + `bobbin index`, then `bobbin serve --no-remote`).",
                self.url
            ),
            None,
        )
    }

    // -----------------------------------------------------------------------
    // Beads & archive
    // -----------------------------------------------------------------------

    pub(super) async fn search_beads(
        &self,
        req: &SearchBeadsRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = search_beads_params(req);
        let value = self
            .client
            .get_value("/beads", &p.into_vec())
            .await
            .map_err(|e| self.fail("search_beads", e))?;
        let value = crate::index::beads::provenance::annotate_remote(value)
            .map_err(|e| self.fail("search_beads", e))?;
        Self::ok(value)
    }

    pub(super) async fn archive_search(
        &self,
        req: &ArchiveSearchRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = archive_search_params(req);
        self.get("archive_search", "/archive/search", p).await
    }

    pub(super) async fn archive_recent(
        &self,
        req: &ArchiveRecentRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = archive_recent_params(req);
        self.get("archive_recent", "/archive/recent", p).await
    }

    // -----------------------------------------------------------------------
    // Admin & feedback
    // -----------------------------------------------------------------------

    pub(super) async fn prime(&self, req: &PrimeRequest) -> Result<CallToolResult, McpError> {
        let p = prime_params(req);
        self.get("prime", "/prime", p).await
    }

    pub(super) async fn status(&self, req: &StatusRequest) -> Result<CallToolResult, McpError> {
        // `/status` takes no parameters. `repo` would be accepted and ignored,
        // which reads as "this repo has the whole index in it".
        if req.repo.is_some() {
            return Err(McpError::invalid_params(
                format!(
                    "status has no per-repo filter on a remote server ({}) — it reports the \
                     whole index. Use the `search` tool with repo=<name>, or the server's \
                     /repos endpoint, for per-repo figures.",
                    self.url
                ),
                None,
            ));
        }
        self.get("status", "/status", Params::new()).await
    }

    pub(super) async fn feedback_submit(
        &self,
        req: &FeedbackSubmitRequest,
        agent: &str,
    ) -> Result<CallToolResult, McpError> {
        let body = serde_json::json!({
            "injection_id": req.injection_id,
            "rating": req.rating,
            "reason": req.reason.clone().unwrap_or_default(),
            "agent": agent,
        });
        self.post("feedback_submit", "/feedback", body).await
    }

    pub(super) async fn feedback_list(
        &self,
        req: &FeedbackListRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = feedback_list_params(req);
        self.get("feedback_list", "/feedback", p).await
    }

    pub(super) async fn feedback_stats(
        &self,
        req: &FeedbackStatsRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = Params::new().opt_str("group_by", &req.group_by);
        self.get("feedback_stats", "/feedback/stats", p).await
    }

    pub(super) async fn feedback_lineage_store(
        &self,
        req: &FeedbackLineageStoreRequest,
        agent: &str,
    ) -> Result<CallToolResult, McpError> {
        let body = serde_json::json!({
            "feedback_ids": req.feedback_ids,
            "action_type": req.action_type,
            "bead": req.bead,
            "commit_hash": req.commit_hash,
            "description": req.description,
            "agent": agent,
        });
        self.post("feedback_lineage_store", "/feedback/lineage", body)
            .await
    }

    pub(super) async fn feedback_lineage_list(
        &self,
        req: &FeedbackLineageListRequest,
    ) -> Result<CallToolResult, McpError> {
        let p = feedback_lineage_list_params(req);
        self.get("feedback_lineage_list", "/feedback/lineage", p)
            .await
    }

    /// Index statistics for the `bobbin://index/stats` resource.
    pub(super) async fn stats_json(&self) -> anyhow::Result<String> {
        let value = self.client.get_value("/status", &[]).await?;
        let index = value.get("index").cloned().unwrap_or(value);
        Ok(serde_json::to_string_pretty(&index)?)
    }

    /// The refusal a local-index-only tool gives when there is no local index
    /// to fall back to. Named per tool on purpose: "bobbin is not initialized"
    /// at server-start time told the caller nothing about WHICH capability was
    /// missing, and took the other 20-odd working tools down with it.
    pub(super) fn local_only(&self, tool: &str, why: &str) -> McpError {
        McpError::invalid_params(
            format!(
                "{} needs a local bobbin index: {}. This MCP server is proxying to {}, \
                 which has no endpoint for it. Run `bobbin init` + `bobbin index` here and \
                 start the server with `bobbin serve --no-remote`, or use a tool that the \
                 remote server does serve (search, grep, context, find_refs, list_symbols, \
                 related, dependencies, file_history, hotspots, impact, similar, read_chunk, \
                 search_beads, archive_search, archive_recent, prime, status).",
                tool, why, self.url
            ),
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The role is forwarded on every endpoint that filters by it. Dropping it
    /// would return MORE than the same query returns through the CLI, which is
    /// the failure that is hardest to notice.
    #[test]
    fn the_resolved_role_reaches_the_server() {
        let backend = RemoteBackend::new("http://search.example/".into(), "aegis/crew/x".into());
        let p = backend.role_param(Params::new());
        assert_eq!(p.get("role"), Some("aegis/crew/x"));
    }

    #[test]
    fn the_base_url_loses_its_trailing_slash() {
        let backend = RemoteBackend::new("http://search.example/".into(), "default".into());
        assert_eq!(backend.url(), "http://search.example");
    }

    /// `/status` has no per-repo filter. Accepting `repo` and returning the
    /// whole-index figures would read as "this repo contains 44k files".
    #[test]
    fn status_refuses_a_repo_filter_rather_than_ignoring_it() {
        let backend = RemoteBackend::new("http://search.example".into(), "default".into());
        let err = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(backend.status(&StatusRequest {
                detailed: None,
                repo: Some("bobbin".into()),
            }))
            .unwrap_err();
        assert!(
            err.message.contains("no per-repo filter"),
            "unexpected message: {}",
            err.message
        );
    }

    /// The point of the per-tool refusal: the server still starts and the
    /// other tools still work, and the message says which capability is
    /// missing and how to get it.
    #[test]
    fn a_local_only_tool_names_itself_and_the_remedy() {
        let backend = RemoteBackend::new("http://search.example".into(), "default".into());
        let err = backend.local_only("test_coverage", "it reads local git history");
        assert!(err.message.contains("test_coverage"), "{}", err.message);
        assert!(err.message.contains("--no-remote"), "{}", err.message);
        assert!(
            err.message.contains("http://search.example"),
            "{}",
            err.message
        );
    }
}
