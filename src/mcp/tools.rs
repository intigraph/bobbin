//! MCP tool definitions for Bobbin.
//!
//! This module defines the request/response types for each MCP tool.

use serde::{Deserialize, Serialize};

/// Request for semantic search
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchRequest {
    /// The search query (natural language description of what you're looking for)
    #[schemars(
        description = "Natural language search query describing the code you're looking for"
    )]
    pub query: String,

    /// Filter by chunk type (incl. commit, issue/bead, doc)
    #[schemars(
        description = "Filter by chunk type: function, method, class, struct, enum, interface, module, impl, trait, doc, section, table, code_block, commit, issue (alias: bead), other"
    )]
    pub r#type: Option<String>,

    /// Maximum number of results (default: 10)
    #[schemars(description = "Maximum number of results to return (default: 10)")]
    pub limit: Option<usize>,

    /// Search mode: hybrid (default), semantic, or keyword
    #[schemars(
        description = "Search mode: 'hybrid' (combines semantic+keyword, default), 'semantic' (vector similarity), or 'keyword' (full-text)"
    )]
    pub mode: Option<String>,

    /// Filter to a specific repository (searches all repos if omitted)
    #[schemars(
        description = "Filter results to a specific repository name. Omit to search across all indexed repos."
    )]
    pub repo: Option<String>,

    /// Include only chunks with these tags (comma-separated)
    #[schemars(
        description = "Include only chunks tagged with these tags (comma-separated, e.g. 'core,api')"
    )]
    pub tag: Option<String>,

    /// Exclude chunks with these tags (comma-separated)
    #[schemars(
        description = "Exclude chunks tagged with these tags (comma-separated, e.g. 'test,generated')"
    )]
    pub exclude_tag: Option<String>,

    /// Scope search to a named context bundle
    #[schemars(
        description = "Scope results to a named context bundle (e.g. 'context/pipeline', 'hook'). Omit to search all files."
    )]
    pub bundle: Option<String>,
}

/// Response for semantic search
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchResponse {
    pub query: String,
    pub mode: String,
    pub count: usize,
    pub results: Vec<SearchResultItem>,
}

/// A single search result item
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchResultItem {
    /// Stable chunk ID — pass to chunk_neighbors to follow relationships
    pub id: String,
    pub file_path: String,
    pub name: Option<String>,
    pub chunk_type: String,
    /// Source kind: "code", "issue", or "commit" — helps distinguish beads issues from source code
    pub source: String,
    /// Repository name this result belongs to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    pub match_type: Option<String>,
    pub language: String,
    pub content_preview: String,
}

/// Request for keyword/regex search (grep)
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GrepRequest {
    /// Pattern to search for (FTS query or regex with regex=true)
    #[schemars(
        description = "Pattern to search for. Supports full-text search queries, or regex if regex=true"
    )]
    pub pattern: String,

    /// Case insensitive search
    #[schemars(description = "Enable case-insensitive search")]
    pub ignore_case: Option<bool>,

    /// Use extended regex matching
    #[schemars(description = "Enable extended regex matching (post-filters FTS results)")]
    pub regex: Option<bool>,

    /// Filter by chunk type
    #[schemars(
        description = "Filter by chunk type: function, method, class, struct, enum, interface, module, impl, trait, doc, section, table, code_block, commit, issue (alias: bead), other"
    )]
    pub r#type: Option<String>,

    /// Maximum number of results (default: 10)
    #[schemars(description = "Maximum number of results to return (default: 10)")]
    pub limit: Option<usize>,

    /// Filter to a specific repository (searches all repos if omitted)
    #[schemars(
        description = "Filter results to a specific repository name. Omit to search across all indexed repos."
    )]
    pub repo: Option<String>,
}

/// Response for grep search
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct GrepResponse {
    pub pattern: String,
    pub count: usize,
    pub results: Vec<GrepResultItem>,
}

/// A single grep result item
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct GrepResultItem {
    pub file_path: String,
    pub name: Option<String>,
    pub chunk_type: String,
    /// Source kind: "code", "issue", or "commit"
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    pub language: String,
    pub content_preview: String,
    pub matching_lines: Vec<MatchingLine>,
}

/// A line that matches the search pattern
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MatchingLine {
    pub line_number: u32,
    pub content: String,
}

/// Request for context assembly
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ContextRequest {
    /// Natural language description of the task
    #[schemars(description = "Natural language task description to assemble context for")]
    pub query: String,

    /// Maximum lines of content to include (default: 500)
    #[schemars(
        description = "Maximum lines of code content to include in the context bundle (default: 500)"
    )]
    pub budget: Option<usize>,

    /// Coupling expansion depth (default: 1, 0 = no coupling)
    #[schemars(
        description = "Depth of temporal coupling expansion. 0 disables coupling, 1 expands one level (default: 1)"
    )]
    pub depth: Option<u32>,

    /// Max coupled files per seed file (default: 3)
    #[schemars(
        description = "Maximum number of coupled files to include per seed file (default: 3)"
    )]
    pub max_coupled: Option<usize>,

    /// Max initial search results (default: 20)
    #[schemars(
        description = "Maximum number of initial search results to use as seeds (default: 20)"
    )]
    pub limit: Option<usize>,

    /// Min coupling score threshold (default: 0.1)
    #[schemars(
        description = "Minimum coupling score threshold for including related files (default: 0.1)"
    )]
    pub coupling_threshold: Option<f32>,

    /// Filter to specific repository
    #[schemars(
        description = "Filter results to a specific repository name. Omit to search across all indexed repos."
    )]
    pub repo: Option<String>,

    /// Include only chunks with these tags (comma-separated)
    #[schemars(
        description = "Include only chunks tagged with these tags (comma-separated, e.g. 'core,api')"
    )]
    pub tag: Option<String>,

    /// Exclude chunks with these tags (comma-separated)
    #[schemars(
        description = "Exclude chunks tagged with these tags (comma-separated, e.g. 'test,generated')"
    )]
    pub exclude_tag: Option<String>,

    /// Scope context to a named context bundle
    #[schemars(
        description = "Scope results to a named context bundle (e.g. 'context/pipeline', 'hook'). Omit to search all files."
    )]
    pub bundle: Option<String>,
}

/// Response for context assembly
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextResponse {
    pub query: String,
    pub budget: ContextBudgetInfo,
    pub files: Vec<ContextFileOutput>,
    pub summary: ContextSummaryOutput,
}

/// Budget information in context response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextBudgetInfo {
    pub max_lines: usize,
    pub used_lines: usize,
}

/// A file in the context response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextFileOutput {
    pub path: String,
    pub language: String,
    pub relevance: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub coupled_to: Vec<String>,
    pub chunks: Vec<ContextChunkOutput>,
    /// Repository name this file belongs to (from index metadata)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// A chunk in the context response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextChunkOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Summary statistics in context response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextSummaryOutput {
    pub total_files: usize,
    pub total_chunks: usize,
    pub direct_hits: usize,
    pub coupled_additions: usize,
    pub bridged_additions: usize,
    pub source_files: usize,
    pub doc_files: usize,
}

/// Request for finding related files
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RelatedRequest {
    /// File path to find related files for
    #[schemars(description = "File path (relative to repo root) to find related files for")]
    pub file: String,

    /// Maximum number of results (default: 10)
    #[schemars(description = "Maximum number of related files to return (default: 10)")]
    pub limit: Option<usize>,

    /// Minimum score threshold (default: 0.0)
    #[schemars(description = "Minimum coupling score threshold (0.0-1.0, default: 0.0)")]
    pub threshold: Option<f32>,

    /// Seed file's repo, to disambiguate cross-repo coupling in a shared store
    #[schemars(
        description = "Repo the file belongs to (optional; disambiguates cross-repo coupling lookups in a multi-repo store)"
    )]
    pub repo: Option<String>,
}

/// Response for related files
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RelatedResponse {
    pub file: String,
    pub related: Vec<RelatedFile>,
}

/// A file related to the query file
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RelatedFile {
    pub path: String,
    pub score: f32,
    pub co_changes: u32,
    /// Repo the file lives in — set only for cross-repo coupled files (bo-oqny).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// Request for test↔source coverage mapping
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TestCoverageRequest {
    /// File path to map coverage for
    #[schemars(
        description = "File path (relative to repo root). A source file returns the test files that cover it; a test file returns the source files it covers."
    )]
    pub file: String,

    /// Maximum number of results (default: 10)
    #[schemars(description = "Maximum number of coverage links to return (default: 10)")]
    pub limit: Option<usize>,

    /// Minimum score threshold (default: 0.0)
    #[schemars(description = "Minimum coupling score threshold (0.0-1.0, default: 0.0)")]
    pub threshold: Option<f32>,
}

/// Response for test↔source coverage mapping
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TestCoverageResponse {
    pub file: String,
    /// "test" when `file` is a source file (links are tests); "source" when
    /// `file` is a test file (links are the sources it covers).
    pub link_kind: String,
    pub links: Vec<CoverageLinkOutput>,
}

/// A single test↔source coverage link
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CoverageLinkOutput {
    pub path: String,
    pub score: f32,
    pub co_changes: u32,
}

/// Request for finding symbol references
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindRefsRequest {
    /// Symbol name to find references for
    #[schemars(
        description = "Exact symbol name to find references for (e.g., 'parse_config', 'Config', 'handle_request')"
    )]
    pub symbol: String,

    /// Filter by symbol type (function, struct, trait, etc.)
    #[schemars(
        description = "Filter by symbol type: function, method, class, struct, enum, interface, module, impl, trait"
    )]
    pub r#type: Option<String>,

    /// Maximum number of usage results (default: 20)
    #[schemars(description = "Maximum number of usage results to return (default: 20)")]
    pub limit: Option<usize>,

    /// Filter to a specific repository
    #[schemars(
        description = "Filter results to a specific repository name. Omit to search across all indexed repos."
    )]
    pub repo: Option<String>,
}

/// Response for finding symbol references
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FindRefsResponse {
    pub symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<SymbolDefinitionOutput>,
    pub usage_count: usize,
    pub usages: Vec<SymbolUsageOutput>,
}

/// A symbol definition in the response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SymbolDefinitionOutput {
    pub name: String,
    pub chunk_type: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: String,
}

/// A symbol usage in the response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SymbolUsageOutput {
    pub file_path: String,
    pub line: u32,
    pub context: String,
}

/// Request for listing symbols in a file
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListSymbolsRequest {
    /// File path (relative to repo root) to list symbols for
    #[schemars(
        description = "File path (relative to repo root) to list symbols for (e.g., 'src/main.rs', 'lib/config.ts')"
    )]
    pub file: String,

    /// Filter to a specific repository
    #[schemars(
        description = "Filter results to a specific repository name. Omit to search across all indexed repos."
    )]
    pub repo: Option<String>,
}

/// Response for listing symbols in a file
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ListSymbolsResponse {
    pub file: String,
    pub count: usize,
    pub symbols: Vec<SymbolItemOutput>,
}

/// A symbol in the list symbols response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SymbolItemOutput {
    pub name: String,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: String,
}

/// Request for reading a specific code chunk
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadChunkRequest {
    /// File path containing the chunk
    #[schemars(description = "File path (relative to repo root) containing the code chunk")]
    pub file: String,

    /// Start line of the chunk
    #[schemars(description = "Starting line number of the code chunk")]
    pub start_line: u32,

    /// End line of the chunk
    #[schemars(description = "Ending line number of the code chunk")]
    pub end_line: u32,

    /// Number of context lines to include before and after
    #[schemars(
        description = "Number of context lines to include before and after the chunk (default: 0)"
    )]
    pub context: Option<u32>,
}

/// Response for reading a code chunk
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ReadChunkResponse {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub actual_start_line: u32,
    pub actual_end_line: u32,
    pub content: String,
    pub language: String,
}

/// Request for identifying code hotspots
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HotspotsRequest {
    /// Time window for churn analysis (e.g. "6 months ago", "1 year ago")
    #[schemars(
        description = "Time window for churn analysis (default: '1 year ago'). Examples: '6 months ago', '3 months ago', '2 years ago'"
    )]
    pub since: Option<String>,

    /// Maximum number of hotspots to return (default: 20)
    #[schemars(description = "Maximum number of hotspots to return (default: 20)")]
    pub limit: Option<usize>,

    /// Minimum hotspot score threshold (0.0-1.0, default: 0.0)
    #[schemars(
        description = "Minimum hotspot score threshold (0.0-1.0, default: 0.0). Higher values filter to only the most critical hotspots."
    )]
    pub threshold: Option<f32>,
}

/// Response for code hotspots
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct HotspotsResponse {
    pub count: usize,
    pub since: String,
    pub hotspots: Vec<HotspotItem>,
}

/// A single hotspot entry
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct HotspotItem {
    pub file: String,
    pub score: f32,
    pub churn: u32,
    pub complexity: f32,
    pub language: String,
}

/// Request for project primer/overview
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PrimeRequest {
    /// Show only a specific section (e.g. "architecture", "commands", "mcp tools")
    #[schemars(
        description = "Optional section name to show. Available: 'what bobbin does', 'architecture', 'supported languages', 'key commands', 'mcp tools', 'quick start', 'configuration'. Omit to show the full primer."
    )]
    pub section: Option<String>,

    /// Show a brief (compact) overview only
    #[schemars(
        description = "If true, show only the title and first section for a compact overview"
    )]
    pub brief: Option<bool>,
}

/// Response for project primer/overview
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PrimeResponse {
    pub primer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    pub initialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<PrimeStats>,
}

/// Live index statistics included in prime response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PrimeStats {
    pub total_files: u64,
    pub total_chunks: u64,
    pub total_embeddings: u64,
    pub languages: Vec<PrimeLanguageStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed: Option<String>,
}

/// Per-language stats in prime response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PrimeLanguageStats {
    pub language: String,
    pub file_count: u64,
    pub chunk_count: u64,
}

/// Request for impact analysis
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ImpactRequest {
    /// File path or file:function target to analyze
    #[schemars(
        description = "File path or file:function target (e.g. 'src/auth.rs' or 'src/auth.rs:login_handler')"
    )]
    pub target: String,

    /// Transitive impact depth (1-3, default: 1)
    #[schemars(
        description = "Transitive expansion depth (1 = direct only, 2-3 = expand through results with score decay). Default: 1"
    )]
    pub depth: Option<u32>,

    /// Signal mode: combined, coupling, semantic, deps
    #[schemars(
        description = "Which signals to use: 'combined' (default, uses all), 'coupling' (git co-change only), 'semantic' (vector similarity only), 'deps' (dependency graph, not yet available)"
    )]
    pub mode: Option<String>,

    /// Maximum number of results (default: 15)
    #[schemars(description = "Maximum number of results to return (default: 15)")]
    pub limit: Option<usize>,

    /// Minimum impact score threshold (0.0-1.0, default: 0.1)
    #[schemars(description = "Minimum impact score threshold (0.0-1.0, default: 0.1)")]
    pub threshold: Option<f32>,

    /// Filter to a specific repository
    #[schemars(
        description = "Filter results to a specific repository name. Omit to search across all indexed repos."
    )]
    pub repo: Option<String>,
}

/// Response for impact analysis
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ImpactResponse {
    pub target: String,
    pub mode: String,
    pub depth: u32,
    pub count: usize,
    pub results: Vec<ImpactResultItem>,
}

/// A single impact result
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ImpactResultItem {
    pub file: String,
    pub signal: String,
    pub score: f32,
    pub reason: String,
}

/// Request for similarity search
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SimilarRequest {
    /// Target to find similar code for (file:name chunk ref or free text). Required unless scan is true.
    #[schemars(
        description = "Target to find similar code for. Use 'file.rs:function_name' for a chunk reference, or free text like 'error handling with retry'. Required unless scan=true."
    )]
    pub target: Option<String>,

    /// Scan entire codebase for near-duplicate clusters
    #[schemars(
        description = "If true, scan entire codebase for near-duplicate clusters instead of searching for a specific target"
    )]
    pub scan: Option<bool>,

    /// Minimum cosine similarity threshold (default: 0.85)
    #[schemars(
        description = "Minimum cosine similarity threshold (0.0-1.0, default: 0.85 for single target, 0.90 for scan)"
    )]
    pub threshold: Option<f32>,

    /// Maximum number of results or clusters (default: 10)
    #[schemars(
        description = "Maximum number of results (single target) or clusters (scan mode) to return (default: 10)"
    )]
    pub limit: Option<usize>,

    /// Filter to a specific repository
    #[schemars(
        description = "Filter results to a specific repository name. Omit to search across all indexed repos."
    )]
    pub repo: Option<String>,

    /// In scan mode, compare chunks across different repos
    #[schemars(
        description = "In scan mode, compare chunks across different repos (default: false)"
    )]
    pub cross_repo: Option<bool>,
}

/// Response for similarity search
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SimilarResponse {
    pub mode: String,
    pub threshold: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub count: usize,
    pub results: Vec<SimilarResultItem>,
    pub clusters: Vec<SimilarClusterItem>,
}

/// A single similarity result
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SimilarResultItem {
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub similarity: f32,
    pub language: String,
    pub explanation: String,
}

/// A duplicate cluster from scan mode
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SimilarClusterItem {
    pub representative: SimilarChunkRef,
    pub avg_similarity: f32,
    pub member_count: usize,
    pub members: Vec<SimilarResultItem>,
}

/// A chunk reference in a cluster
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SimilarChunkRef {
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub language: String,
}

/// Request for diff-aware review context
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReviewRequest {
    /// Diff specification: "unstaged", "staged", a branch name (prefixed with "branch:"), or a commit range like "HEAD~3..HEAD"
    #[schemars(
        description = "What to diff. Use 'unstaged' for working tree changes, 'staged' for staged changes, 'branch:<name>' to compare a branch against main, or a commit range like 'HEAD~3..HEAD'."
    )]
    pub diff: Option<String>,

    /// Maximum lines of context to include (default: 500)
    #[schemars(
        description = "Maximum lines of code content to include in the review context (default: 500)"
    )]
    pub budget: Option<usize>,

    /// Coupling expansion depth (default: 1, 0 = no coupling)
    #[schemars(
        description = "Depth of temporal coupling expansion. 0 disables coupling, 1 expands one level (default: 1)"
    )]
    pub depth: Option<u32>,

    /// Filter coupled files to a specific repository
    #[schemars(
        description = "Filter results to a specific repository name. Omit to search across all indexed repos."
    )]
    pub repo: Option<String>,
}

/// Response for review context
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ReviewResponse {
    pub diff_description: String,
    pub changed_files: Vec<ReviewChangedFile>,
    pub budget: ContextBudgetInfo,
    pub files: Vec<ContextFileOutput>,
    pub summary: ContextSummaryOutput,
}

/// A changed file in the review diff
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ReviewChangedFile {
    pub path: String,
    pub status: String,
    pub added_lines: usize,
    pub removed_lines: usize,
}

/// Request for searching beads (issues from the configured bead store)
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchBeadsRequest {
    /// Natural language search query
    #[schemars(
        description = "Natural language search query to find relevant beads/issues (e.g. 'cert expiry', 'disk pressure', 'auth bug')"
    )]
    pub query: String,

    /// Filter by priority (1-4)
    #[schemars(
        description = "Filter by priority level (1=P1 critical, 2=P2 high, 3=P3 medium, 4=P4 low)"
    )]
    pub priority: Option<i32>,

    /// Filter by status (open, in_progress, closed)
    #[schemars(description = "Filter by issue status: 'open', 'in_progress', 'closed'")]
    pub status: Option<String>,

    /// Filter by assignee
    #[schemars(description = "Filter by assignee (e.g. 'aegis/crew/braino')")]
    pub assignee: Option<String>,

    /// Filter by rig name
    #[schemars(
        description = "Filter by rig name (e.g. 'aegis', 'gastown'). Matches against the file_path prefix 'beads:<rig>:'."
    )]
    pub rig: Option<String>,

    /// Filter by issue type (bug, task, feature, epic, chore)
    #[schemars(description = "Filter by issue type: 'bug', 'task', 'feature', 'epic', 'chore'")]
    pub issue_type: Option<String>,

    /// Filter by label (matches if any label contains the string)
    #[schemars(
        description = "Filter by label (e.g. 'tech-debt', 'enhancement'). Matches if any label contains the filter string."
    )]
    pub label: Option<String>,

    /// Maximum number of results (default: 10)
    #[schemars(description = "Maximum number of results to return (default: 10)")]
    pub limit: Option<usize>,

    /// Reread configured-store metadata (default: true), with provenance.
    #[schemars(
        description = "If true (default), reread status/priority/assignee from the configured source. JSONL is a snapshot, not live tracker state. Source/as-of limitations are reported; verify against the active tracker. Set to false for indexed-only results."
    )]
    pub enrich: Option<bool>,

    /// Compact mode - omit snippet to reduce token usage (default: true)
    #[schemars(
        description = "If true (default), omit the snippet field to reduce token overhead. Set to false to include a content snippet."
    )]
    pub compact: Option<bool>,
}

/// Response for searching beads
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchBeadsResponse {
    pub query: String,
    pub count: usize,
    pub warnings: Vec<String>,
    pub results: Vec<BeadResultItem>,
}

/// A single bead search result
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct BeadResultItem {
    pub bead_id: String,
    pub metadata_provenance: crate::index::beads::provenance::BeadMetadataProvenance,
    pub title: String,
    pub priority: String,
    pub status: String,
    pub issue_type: String,
    pub assignee: String,
    pub owner: String,
    pub rig: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub relevance_score: f32,
    pub match_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// Request for dependency analysis
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DependenciesRequest {
    /// File path to show dependencies for
    #[schemars(description = "File path (relative to repo root) to show import dependencies for")]
    pub file: String,

    /// Show reverse dependencies (files that import this file)
    #[schemars(
        description = "If true, show reverse dependencies (files that import this file) instead of forward dependencies"
    )]
    pub reverse: Option<bool>,

    /// Show both directions (imports and dependents)
    #[schemars(description = "If true, show both forward imports and reverse dependents")]
    pub both: Option<bool>,
}

/// Response for dependency analysis
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DependenciesResponse {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imports: Option<Vec<DependencyItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_by: Option<Vec<DependencyItem>>,
}

/// A single dependency entry
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DependencyItem {
    pub path: String,
    pub dep_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub resolved: bool,
}

/// Request for file history
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FileHistoryRequest {
    /// File path to show commit history for
    #[schemars(description = "File path (relative to repo root) to show commit history for")]
    pub file: String,

    /// Maximum number of history entries (default: 20)
    #[schemars(description = "Maximum number of commit entries to return (default: 20)")]
    pub limit: Option<usize>,
}

/// Response for file history
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FileHistoryResponse {
    pub file: String,
    pub entries: Vec<FileHistoryItem>,
    pub stats: FileHistoryStats,
}

/// A single file history entry
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FileHistoryItem {
    pub date: String,
    pub author: String,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

/// Statistics about a file's history
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FileHistoryStats {
    pub total_commits: usize,
    pub authors: Vec<FileHistoryAuthor>,
    pub churn_rate: f32,
}

/// Author statistics in file history
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FileHistoryAuthor {
    pub name: String,
    pub commits: usize,
}

/// Request for index status
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StatusRequest {
    /// Show detailed statistics including language breakdown
    #[schemars(description = "If true, include per-language breakdown in the response")]
    pub detailed: Option<bool>,

    /// Filter stats to a specific repository
    #[schemars(
        description = "Filter statistics to a specific repository name. Omit to show stats for all repos."
    )]
    pub repo: Option<String>,
}

/// Response for index status
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusResponse {
    pub status: String,
    pub total_files: u64,
    pub total_chunks: u64,
    pub total_embeddings: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_stats: Option<StatusDependencyStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<StatusLanguageStats>,
}

/// Dependency statistics in status response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusDependencyStats {
    pub total: u64,
    pub resolved: u64,
}

/// Per-language stats in status response
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusLanguageStats {
    pub language: String,
    pub file_count: u64,
    pub chunk_count: u64,
}

/// Request for semantic commit search
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CommitSearchRequest {
    /// Natural language search query describing what the commit did (e.g. "added error handling", "refactored auth")
    #[schemars(
        description = "Natural language query describing what commit(s) you're looking for. Examples: 'added authentication', 'fixed memory leak', 'refactored database layer'"
    )]
    pub query: String,

    /// Filter by commit author name (substring match)
    #[schemars(
        description = "Filter to commits by a specific author (case-insensitive substring match)"
    )]
    pub author: Option<String>,

    /// Filter to commits touching a specific file (substring match on file paths)
    #[schemars(
        description = "Filter to commits that touched a specific file path (substring match)"
    )]
    pub file: Option<String>,

    /// Maximum number of results (default: 10)
    #[schemars(description = "Maximum number of commit results to return (default: 10)")]
    pub limit: Option<usize>,
}

/// Response for semantic commit search
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CommitSearchResponse {
    pub query: String,
    pub count: usize,
    pub results: Vec<CommitResultItem>,
}

/// A single commit search result
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CommitResultItem {
    pub hash: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    pub score: f32,
}

// -- Archive tool types --

/// Request for searching archive records (HLA chat logs, Pensieve agent memory)
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ArchiveSearchRequest {
    /// Natural language search query
    #[schemars(
        description = "Natural language query to search archive records (e.g. 'deploy failures', 'cert renewal')"
    )]
    pub query: String,

    /// Archive source filter (e.g. "hla" for chat logs, "pensieve" for agent memory)
    #[schemars(
        description = "Filter by archive source: 'hla' (IRC/Telegram chat logs) or 'pensieve' (agent memory records). Omit to search all sources."
    )]
    pub source: Option<String>,

    /// Filter by name/channel (e.g., channel name for HLA, agent name for Pensieve)
    #[schemars(
        description = "Filter by name field value (e.g. 'telegram' for HLA channel, 'goldblum' for Pensieve agent)"
    )]
    pub filter: Option<String>,

    /// Only include records after this date (YYYY-MM-DD)
    #[schemars(description = "Only include records after this date (YYYY-MM-DD format)")]
    pub after: Option<String>,

    /// Only include records before this date (YYYY-MM-DD)
    #[schemars(description = "Only include records before this date (YYYY-MM-DD format)")]
    pub before: Option<String>,

    /// Maximum number of results (default: 10)
    #[schemars(description = "Maximum number of results to return (default: 10)")]
    pub limit: Option<usize>,

    /// Search mode: hybrid (default), semantic, or keyword
    #[schemars(description = "Search mode: 'hybrid' (default), 'semantic', or 'keyword'")]
    pub mode: Option<String>,
}

/// Request for listing recent archive records
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ArchiveRecentRequest {
    /// Only include records after this date (YYYY-MM-DD). Defaults to 30 days ago.
    #[schemars(
        description = "Only include records after this date (YYYY-MM-DD format). Optional — defaults to 30 days ago."
    )]
    pub after: Option<String>,

    /// Archive source filter (e.g. "hla" for chat logs, "pensieve" for agent memory)
    #[schemars(
        description = "Filter by archive source: 'hla' (IRC/Telegram chat logs) or 'pensieve' (agent memory records). Omit to list from all sources."
    )]
    pub source: Option<String>,

    /// Maximum number of results (default: 20)
    #[schemars(description = "Maximum number of results to return (default: 20)")]
    pub limit: Option<usize>,
}

// -- Feedback tool types --

/// Request for submitting feedback on a bobbin context injection
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FeedbackSubmitRequest {
    /// Injection ID from [injection_id: inj-xxx] in context output. Supports both standard (inj-xxx) and reaction (inj-react-xxx) IDs.
    #[schemars(
        description = "Injection ID from [injection_id: inj-xxx] in context injection output"
    )]
    pub injection_id: String,

    /// Rating: useful, noise, or harmful
    #[schemars(
        description = "Rating for the injection quality: 'useful' (helped with task), 'noise' (irrelevant), or 'harmful' (actively misleading)"
    )]
    pub rating: String,

    /// Agent identity (auto-detected from GT_ROLE or BD_ACTOR env vars if empty)
    #[schemars(description = "Agent identity (auto-detected from environment if omitted)")]
    pub agent: Option<String>,

    /// Optional explanation (max 1000 chars)
    #[schemars(description = "Optional explanation of why this rating was given (max 1000 chars)")]
    pub reason: Option<String>,
}

/// Request for listing feedback records
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FeedbackListRequest {
    /// Filter by rating: useful, noise, or harmful
    #[schemars(description = "Filter by rating: 'useful', 'noise', or 'harmful'")]
    pub rating: Option<String>,

    /// Filter by agent name
    #[schemars(description = "Filter by agent name")]
    pub agent: Option<String>,

    /// Max results (default: 20, max: 50)
    #[schemars(description = "Maximum number of results to return (default: 20, max: 50)")]
    pub limit: Option<usize>,
}

/// Request for feedback statistics with optional grouping
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FeedbackStatsRequest {
    /// Group results by "bundle" or "bead". If omitted, returns aggregated totals.
    #[serde(default)]
    #[schemars(description = "Group stats by 'bundle' or 'bead'. Omit for aggregated totals.")]
    pub group_by: Option<String>,
}

/// Request for recording a lineage action that ties feedback to a fix
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FeedbackLineageStoreRequest {
    /// Feedback record IDs this action resolves (from feedback_list output)
    #[schemars(
        description = "Feedback record IDs this action resolves. Get IDs from bobbin_feedback_list output."
    )]
    pub feedback_ids: Vec<i64>,

    /// Type of action taken: access_rule, tag_effect, config_change, code_fix, or exclusion_rule
    #[schemars(
        description = "Type of action: 'code_fix' (bug fix or feature), 'config_change' (bobbin config tuning), 'tag_effect' (tag/annotation change), 'access_rule' (access control change), or 'exclusion_rule' (file exclusion)"
    )]
    pub action_type: String,

    /// Associated bead ID (e.g., "aegis-abc123")
    #[schemars(
        description = "Bead ID this action is associated with (e.g. 'aegis-abc123'). Optional."
    )]
    pub bead: Option<String>,

    /// Git commit hash
    #[schemars(description = "Git commit hash for the fix. Optional.")]
    pub commit_hash: Option<String>,

    /// Human-readable description of what was done
    #[schemars(
        description = "What was done to address the feedback (e.g. 'Added exclusion rule for generated files')"
    )]
    pub description: String,

    /// Agent identity (auto-detected from GT_ROLE or BD_ACTOR env vars if empty)
    #[schemars(description = "Agent identity (auto-detected from environment if omitted)")]
    pub agent: Option<String>,
}

/// Request for listing lineage records
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FeedbackLineageListRequest {
    /// Filter by feedback record ID
    #[schemars(description = "Filter lineage records linked to a specific feedback ID")]
    pub feedback_id: Option<i64>,

    /// Filter by bead ID
    #[schemars(description = "Filter by bead ID (e.g. 'aegis-abc123')")]
    pub bead: Option<String>,

    /// Filter by commit hash
    #[schemars(description = "Filter by git commit hash")]
    pub commit_hash: Option<String>,

    /// Max results (default: 20, max: 50)
    #[schemars(description = "Maximum number of results (default: 20, max: 50)")]
    pub limit: Option<usize>,
}

// ── Quipu knowledge graph tools ───────────────────────────────────

/// Request for knowledge graph context query
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeContextRequest {
    /// Natural language query describing what knowledge you need
    #[schemars(
        description = "Natural language query describing the knowledge graph entities you're looking for"
    )]
    pub query: String,

    /// Maximum number of entities to return (default: 20)
    #[schemars(description = "Maximum number of knowledge graph entities to return (default: 20)")]
    pub max_entities: Option<usize>,

    /// Whether to expand results by following links from direct hits (default: true)
    #[schemars(
        description = "Expand results by following graph links from direct hits (default: true)"
    )]
    pub expand_links: Option<bool>,
}

/// Scope shared by Quipu's canonical export and share producers.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeShareScopeKind {
    Root,
    Graph,
    Group,
    Construct,
}

/// Request for a canonical scoped RDF export from the configured remote Quipu.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeExportRequest {
    pub scope_kind: KnowledgeShareScopeKind,
    pub scope_value: Option<String>,
    #[schemars(description = "RDF format: ntriples (canonical) or turtle")]
    pub format: Option<String>,
    #[schemars(description = "Return only byte count, triple count, and sha256 identity")]
    pub digest_only: Option<bool>,
    pub max_bytes: Option<usize>,
}

/// Request for Quipu's canonical v1 share-bundle producer.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeShareRequest {
    pub scope_kind: KnowledgeShareScopeKind,
    pub scope_value: Option<String>,
    pub shapes: Option<Vec<String>>,
    pub no_shapes: Option<bool>,
    pub parent_share: Option<String>,
    pub turtle_view: Option<bool>,
    pub max_bytes: Option<usize>,
}

mod manifest_schema;
pub use manifest_schema::KnowledgeImportRequest;

/// Request to explicitly promote a previously staged Quipu share into ROOT.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeImportPromoteRequest {
    pub share_id: String,
    pub actor: Option<String>,
}

/// Request to write facts into the local knowledge graph (bobbin-di7 Phase 3).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeKnotRequest {
    /// Facts to assert, as Turtle.
    #[schemars(
        description = "Facts to write, as RDF Turtle. Validated against the configured SHACL shapes before anything is committed."
    )]
    pub turtle: String,

    /// Who is asserting these facts.
    #[schemars(
        description = "Actor recorded as the author of this write (provenance). Omit only if the caller genuinely has no identity to record."
    )]
    pub actor: Option<String>,

    /// Where the facts came from.
    #[schemars(
        description = "Source of the facts (e.g. a file path, a tool name). Recorded alongside the write."
    )]
    pub source: Option<String>,

    /// SHACL shapes to validate against, overriding the store's configured set.
    #[schemars(
        description = "Optional SHACL shapes (Turtle) to validate this write against instead of the store's configured shapes"
    )]
    pub shapes: Option<String>,
}

/// Request to run the quarantined-track inferred extractor over prose (W3.B).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeInferredExtractRequest {
    /// Markdown prose to extract candidate entities/relationships from.
    #[schemars(
        description = "Markdown prose to run the inferred-track extractor over. Candidates are CLAIMS, not observations."
    )]
    pub text: String,

    /// Repo name used for minted IRIs (default: "adhoc").
    #[schemars(
        description = "Repository name the prose belongs to, for IRI minting (default: adhoc)"
    )]
    pub repo: Option<String>,

    /// File path used for the source-chunk IRI (default: "adhoc.md").
    #[schemars(
        description = "File path the prose came from, for the source-chunk IRI (default: adhoc.md)"
    )]
    pub file_path: Option<String>,

    /// Also push the stamped facts into the quarantine plane (default: false).
    #[schemars(
        description = "Also land the stamped facts in the quarantined crew:inferred plane via /knot (default: false, preview only). Refused unless the embedded quipu enforces strict graph routing."
    )]
    pub push: Option<bool>,
}

/// Request for SPARQL query against the knowledge graph
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeQueryRequest {
    /// SPARQL SELECT query
    #[schemars(description = "SPARQL SELECT query to run against the knowledge graph")]
    pub query: String,

    /// Valid-at timestamp for temporal queries
    #[schemars(
        description = "ISO-8601 timestamp for temporal query (what was true at this time?)"
    )]
    pub valid_at: Option<String>,

    /// Transaction ID for as-of queries
    #[schemars(description = "Transaction ID for point-in-time query")]
    pub tx: Option<i64>,
}

/// Request to run the chunk→entity mention reconcile pass (W2.P5).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeReconcileRequest {
    /// Cap on per-mention detail entries in the response (default: 50).
    #[schemars(
        description = "Maximum per-mention detail entries to return (default: 50). The three counts are always complete."
    )]
    pub max_details: Option<usize>,
}

/// Request for chunk_neighbors: follow relationship edges from a chunk
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ChunkNeighborsRequest {
    /// Chunk ID from search results (preferred anchor)
    #[schemars(
        description = "Chunk ID (the `id` field on search results). Preferred way to identify the anchor chunk. Either this or file+line is required."
    )]
    pub id: Option<String>,

    /// File path, used with `line` to resolve the anchor chunk
    #[schemars(
        description = "Repo-relative file path — used with `line` to resolve the anchor chunk when no `id` is given"
    )]
    pub file: Option<String>,

    /// Line number within `file`
    #[schemars(
        description = "Line number within `file`; the smallest chunk containing this line becomes the anchor"
    )]
    pub line: Option<u32>,

    /// Repository name, to disambiguate `file` in multi-repo stores
    #[schemars(
        description = "Repository name — disambiguates `file` when several indexed repos contain the same relative path"
    )]
    pub repo: Option<String>,

    /// Filter to one edge type
    #[schemars(
        description = "Filter edges by type: next_chunk, part_of, implements, impl_for, extends, tests, similar_to. Omit for all types."
    )]
    pub edge_type: Option<String>,

    /// Edge direction relative to the anchor
    #[schemars(
        description = "Direction relative to the anchor: \"out\" (anchor is edge source), \"in\" (anchor is edge target), or \"both\" (default). For next_chunk: out = following chunk, in = preceding. For part_of: out = containing parent, in = children."
    )]
    pub direction: Option<String>,

    /// Maximum neighbors to return (default: 20)
    #[schemars(description = "Maximum number of neighbors to return (default: 20)")]
    pub limit: Option<usize>,
}

/// A lightweight reference to a chunk (no embedding, truncated content)
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ChunkRef {
    pub id: String,
    pub file_path: String,
    pub name: Option<String>,
    pub chunk_type: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// One neighbor reached over an edge
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct NeighborItem {
    /// Edge type connecting anchor and neighbor
    pub edge_type: String,
    /// "out" = anchor is the edge source; "in" = anchor is the edge target
    pub direction: String,
    #[serde(flatten)]
    pub chunk: ChunkRef,
    pub content_preview: String,
}

/// Response for chunk_neighbors
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ChunkNeighborsResponse {
    /// The resolved anchor chunk
    pub chunk: ChunkRef,
    pub count: usize,
    pub neighbors: Vec<NeighborItem>,
    /// Edge endpoints whose chunk no longer exists (stale IDs after edits).
    /// Non-empty output means the index needs a re-run for this file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dangling: Vec<String>,
}
