//! MCP Server implementation for Bobbin.
//!
//! This module provides the main MCP server that exposes Bobbin's code search
//! and analysis capabilities to AI agents via the Model Context Protocol.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use regex::Regex;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Annotated, CallToolResult, Content, GetPromptRequestParam, GetPromptResult, Implementation,
    ListPromptsResult, ListResourcesResult, PaginatedRequestParam, Prompt, PromptMessage,
    PromptMessageRole, ProtocolVersion, RawResource, ReadResourceRequestParam, ReadResourceResult,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler};

use super::remote::RemoteBackend;
use super::tools::*;
use crate::analysis::backend::{IndexBackend, StructuralBackend};
use crate::analysis::complexity::ComplexityAnalyzer;
use crate::analysis::impact::{ImpactConfig, ImpactMode, ImpactSignal};
use crate::analysis::similar::{SimilarTarget, SimilarityAnalyzer};
use crate::config::Config;
use crate::index::Embedder;
use crate::index::GitAnalyzer;
use crate::search::context::{
    BridgeMode, ContentMode, ContextAssembler, ContextConfig, FileRelevance,
};
use crate::search::{HybridSearch, SemanticSearch};
use crate::storage::{FeedbackStore, MetadataStore, VectorStore};
use crate::tags::{build_tag_exclude_filter, build_tag_include_filter};
use crate::types::{ChunkType, MatchType, SearchResult};

/// MCP Server for Bobbin code search
#[derive(Clone)]
pub struct BobbinMcpServer {
    repo_root: PathBuf,
    /// A configured remote bobbin server, resolved exactly as the CLI
    /// resolves it. When `Some`, every tool with a server endpoint answers
    /// from that server and no local index is required to start. See
    /// `mcp::remote` for what `bobbin serve` did before (aegis-wbbycq).
    remote: Option<Arc<RemoteBackend>>,
    tool_router: ToolRouter<Self>,
}

impl BobbinMcpServer {
    /// The complete tool router this server serves.
    ///
    /// The tools live in two `#[tool_router]` blocks — the local-index tools in
    /// this file and the Quipu tools in `mcp::knowledge_tools` — and this is
    /// the single place they are combined. `new()` and the annotation guard
    /// both call it, so the surface under test cannot drift from the surface
    /// served. Building the combination in two places is what silently dropped
    /// all nine knowledge tools from the guard's view the first time
    /// (aegis-fmcth7); the count assertion caught it, but only because the
    /// count was pinned.
    fn build_router() -> ToolRouter<Self> {
        Self::tool_router() + Self::knowledge_router() + Self::local_graph_router()
    }

    /// The tools this server advertises, for the annotation guard — the
    /// macro-generated routers are private to their own modules.
    #[cfg(test)]
    pub(crate) fn advertised_tools() -> Vec<rmcp::model::Tool> {
        Self::build_router().list_all()
    }

    pub fn new(repo_root: PathBuf) -> Result<Self> {
        Self::with_remote(repo_root, None, "default".to_string())
    }

    /// Build the server, optionally proxying to a remote bobbin server.
    ///
    /// With a remote the local-index check is deliberately skipped: requiring
    /// `bobbin init` in order to proxy is what made `bobbin serve` unusable on
    /// a machine that only ever talks to the fleet index. Without one the
    /// original check stands — a server with neither can answer nothing.
    pub fn with_remote(
        repo_root: PathBuf,
        remote_url: Option<String>,
        role: String,
    ) -> Result<Self> {
        let remote = remote_url.map(|url| Arc::new(RemoteBackend::new(url, role)));

        if remote.is_none() {
            let config_path = Config::config_path(&repo_root);
            if !config_path.exists() {
                anyhow::bail!(
                    "Bobbin not initialized in {}. Run `bobbin init` first, or configure a \
                     server with `bobbin connect <url> --global` so this MCP server can proxy \
                     to it.",
                    repo_root.display()
                );
            }
        }

        Ok(Self {
            repo_root,
            remote,
            tool_router: Self::build_router(),
        })
    }

    /// The configured remote, if any. Tools call this first.
    fn remote(&self) -> Option<&RemoteBackend> {
        self.remote.as_deref()
    }

    /// Whether a local index is available. Used only by the tools the remote
    /// server has no endpoint for.
    fn has_local_index(&self) -> bool {
        Config::config_path(&self.repo_root).exists()
    }

    /// Open the metadata store (for coupling queries)
    fn open_metadata_store(&self) -> Result<MetadataStore> {
        let db_path = Config::db_path(&self.repo_root);
        MetadataStore::open(&db_path).context("Failed to open metadata store")
    }

    /// Open the feedback store
    fn open_feedback_store(&self) -> Result<FeedbackStore> {
        let db_path = Config::feedback_db_path(&self.repo_root);
        FeedbackStore::open(&db_path).context("Failed to open feedback store")
    }

    /// Repository root for the knowledge tool implementation.
    #[cfg(feature = "knowledge")]
    pub(super) fn repo_root(&self) -> &std::path::Path {
        &self.repo_root
    }

    /// Open the vector store
    async fn open_vector_store(&self) -> Result<VectorStore> {
        let lance_path = Config::lance_path(&self.repo_root);
        VectorStore::open(&lance_path)
            .await
            .context("Failed to open vector store")
    }

    /// Get index statistics as a JSON string
    async fn get_stats_json(&self) -> Result<String> {
        let store = self.open_vector_store().await?;
        let stats = store.get_stats(None).await?;
        Ok(serde_json::to_string_pretty(&stats)?)
    }

    fn parse_chunk_type(s: &str) -> Result<ChunkType> {
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
            _ => anyhow::bail!(
                "Unknown chunk type '{}'. Valid types: function, method, class, struct, enum, interface, module, impl, trait, doc, section, table, code_block, commit, issue, other",
                s
            ),
        }
    }

    /// Build a combined SQL filter from comma-separated tag/exclude_tag params.
    fn build_tag_filter(tag: Option<&str>, exclude_tag: Option<&str>) -> Option<String> {
        let mut filters: Vec<String> = Vec::new();
        if let Some(tags) = tag {
            let tag_list: Vec<String> = tags
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            if !tag_list.is_empty() {
                filters.push(build_tag_include_filter(&tag_list));
            }
        }
        if let Some(tags) = exclude_tag {
            let tag_list: Vec<String> = tags
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            if !tag_list.is_empty() {
                filters.push(build_tag_exclude_filter(&tag_list));
            }
        }
        if filters.is_empty() {
            None
        } else {
            Some(filters.join(" AND "))
        }
    }

    fn truncate_content(content: &str, max_len: usize) -> String {
        if content.len() <= max_len {
            content.to_string()
        } else {
            let truncated: String = content.chars().take(max_len).collect();
            format!("{}...", truncated.trim_end())
        }
    }

    fn to_search_result_item(result: &SearchResult) -> SearchResultItem {
        SearchResultItem {
            id: result.chunk.id.clone(),
            file_path: result.chunk.file_path.clone(),
            name: result.chunk.name.clone(),
            chunk_type: result.chunk.chunk_type.to_string(),
            source: crate::types::source_kind(&result.chunk.chunk_type).to_string(),
            repo: result.repo.clone(),
            start_line: result.chunk.start_line,
            end_line: result.chunk.end_line,
            score: result.score,
            match_type: result.match_type.map(|mt| match mt {
                MatchType::Semantic => "semantic".to_string(),
                MatchType::Keyword => "keyword".to_string(),
                MatchType::Hybrid => "hybrid".to_string(),
            }),
            language: result.chunk.language.clone(),
            content_preview: Self::truncate_content(&result.chunk.content, 300),
        }
    }

    fn find_matching_lines(
        content: &str,
        pattern: &str,
        regex: Option<&Regex>,
        ignore_case: bool,
        start_line: u32,
    ) -> Vec<MatchingLine> {
        let lines: Vec<&str> = content.lines().collect();
        let mut results = Vec::new();

        for (idx, line) in lines.iter().enumerate() {
            let matches = if let Some(re) = regex {
                re.is_match(line)
            } else if ignore_case {
                line.to_lowercase().contains(&pattern.to_lowercase())
            } else {
                line.contains(pattern)
            };

            if matches {
                results.push(MatchingLine {
                    line_number: start_line + idx as u32,
                    content: line.to_string(),
                });
            }

            if results.len() >= 10 {
                break;
            }
        }

        results
    }

    fn read_file_lines(
        &self,
        file: &str,
        start: u32,
        end: u32,
        context: u32,
    ) -> Result<(String, u32, u32)> {
        let file_path = self.repo_root.join(file);

        if !file_path.exists() {
            anyhow::bail!("File not found: {}", file);
        }

        let content = std::fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read file: {}", file))?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len() as u32;

        let actual_start = start.saturating_sub(context).max(1);
        let actual_end = (end + context).min(total_lines);

        let start_idx = (actual_start - 1) as usize;
        let end_idx = actual_end as usize;

        let selected_lines = if end_idx <= lines.len() {
            lines[start_idx..end_idx].join("\n")
        } else {
            lines[start_idx..].join("\n")
        };

        Ok((selected_lines, actual_start, actual_end))
    }

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

    async fn build_explore_prompt(&self, focus: &str) -> Result<String> {
        let stats_json = self.get_stats_json().await?;

        let prompt_text = match focus {
            "architecture" => format!(
                "# Codebase Exploration: Architecture\n\n\
                ## Index Statistics\n\
                ```json\n{}\n```\n\n\
                ## Exploration Steps\n\n\
                1. **Understand the structure**: Use `search` with queries like \"main entry point\", \"application setup\", or \"configuration\" to find key files.\n\n\
                2. **Identify core modules**: Search for \"module\", \"service\", or \"handler\" to find the main components.\n\n\
                3. **Find interfaces**: Use `grep` to search for trait/interface definitions that define contracts between components.\n\n\
                4. **Trace dependencies**: Use `related` on key files to understand how components connect.\n\n\
                ## Suggested Queries\n\n\
                - `search(\"main function\")` - Find entry points\n\
                - `search(\"configuration handling\")` - Find config code\n\
                - `grep(\"pub struct\", type=\"struct\")` - List public data structures\n\
                - `grep(\"pub trait\", type=\"trait\")` - List public traits\n",
                stats_json
            ),

            "entry_points" => format!(
                "# Codebase Exploration: Entry Points\n\n\
                ## Index Statistics\n\
                ```json\n{}\n```\n\n\
                ## Exploration Steps\n\n\
                1. **Find main functions**: Search for \"main\", \"run\", or \"start\" functions.\n\n\
                2. **Identify CLI commands**: Look for argument parsing, subcommands, or command handlers.\n\n\
                3. **Find API endpoints**: Search for route handlers, HTTP methods, or endpoint definitions.\n\n\
                4. **Trace initialization**: Follow the startup sequence from main to understand bootstrapping.\n\n\
                ## Suggested Queries\n\n\
                - `search(\"main entry point application\")` - Find main functions\n\
                - `grep(\"fn main\", type=\"function\")` - Find main() directly\n\
                - `search(\"command line arguments parsing\")` - Find CLI handling\n\
                - `search(\"http endpoint handler\")` - Find API routes\n",
                stats_json
            ),

            "dependencies" => format!(
                "# Codebase Exploration: Dependencies\n\n\
                ## Index Statistics\n\
                ```json\n{}\n```\n\n\
                ## Exploration Steps\n\n\
                1. **External dependencies**: Check Cargo.toml, package.json, or requirements.txt for external libs.\n\n\
                2. **Internal coupling**: Use `related` on core files to see which files change together.\n\n\
                3. **Import patterns**: Use `grep` to find import/use statements and understand dependencies.\n\n\
                4. **Shared utilities**: Search for helper functions, utilities, or common modules.\n\n\
                ## Suggested Queries\n\n\
                - `related(\"src/main.rs\")` - Find files coupled to main\n\
                - `grep(\"use crate::\")` - Find internal imports (Rust)\n\
                - `grep(\"import\")` - Find imports (Python/JS/TS)\n\
                - `search(\"shared utility helper\")` - Find utility code\n",
                stats_json
            ),

            "tests" => format!(
                "# Codebase Exploration: Tests\n\n\
                ## Index Statistics\n\
                ```json\n{}\n```\n\n\
                ## Exploration Steps\n\n\
                1. **Find test files**: Look for files with test in the name or test directories.\n\n\
                2. **Test patterns**: Identify testing frameworks and patterns used.\n\n\
                3. **Coverage areas**: See which modules have corresponding tests.\n\n\
                4. **Test utilities**: Find test helpers, fixtures, and mocks.\n\n\
                ## Suggested Queries\n\n\
                - `search(\"test\")` - Find test-related code\n\
                - `grep(\"#[test]\")` - Find Rust tests\n\
                - `grep(\"def test\")` - Find Python tests\n\
                - `grep(\"describe(\")` - Find JS/TS tests\n\
                - `search(\"mock fixture test helper\")` - Find test utilities\n",
                stats_json
            ),

            _ => format!(
                "# Codebase Exploration: {}\n\n\
                ## Index Statistics\n\
                ```json\n{}\n```\n\n\
                ## Getting Started\n\n\
                Use these tools to explore:\n\n\
                1. **`search`**: Natural language queries for semantic code search\n\
                2. **`grep`**: Exact pattern matching for specific terms\n\
                3. **`related`**: Find files that change together\n\
                4. **`read_chunk`**: View specific code sections\n\n\
                ## Suggested First Steps\n\n\
                1. Check the index stats above to understand the codebase size\n\
                2. Use `search(\"{}\")` to find relevant code\n\
                3. Use `related` on interesting files to understand connections\n\
                4. Use `read_chunk` to examine specific code sections\n",
                focus, stats_json, focus
            ),
        };

        Ok(prompt_text)
    }
}

#[tool_router]
impl BobbinMcpServer {
    /// Semantic search for code
    #[tool(
        description = "Search for code using natural language. Finds functions, classes, and other code elements that match the semantic meaning of your query. Best for: 'functions that handle authentication', 'error handling code', 'database connection logic'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn search(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.search(&req).await;
        }

        let limit = req.limit.unwrap_or(10);
        let mode = req.mode.as_deref().unwrap_or("hybrid");

        let type_filter = if let Some(ref t) = req.r#type {
            Some(
                Self::parse_chunk_type(t)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?,
            )
        } else {
            None
        };

        let config_path = Config::config_path(&self.repo_root);
        let config = Config::load(&config_path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let stats = vector_store
            .get_stats(None)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        if stats.total_chunks == 0 {
            return Ok(CallToolResult::success(vec![Content::text(
                "No indexed content. Run `bobbin index` first.",
            )]));
        }

        let search_limit = if type_filter.is_some() {
            limit * 3
        } else {
            limit
        };

        let repo_filter = req.repo.as_deref();

        // Build tag filter
        let mut tag_filter = Self::build_tag_filter(req.tag.as_deref(), req.exclude_tag.as_deref());

        // Apply bundle filter if specified
        if let Some(ref bundle_name) = req.bundle {
            let tags_path = crate::tags::TagsConfig::tags_path(&self.repo_root);
            let tags_config = crate::tags::TagsConfig::load_or_default(&tags_path);
            if let Some(bundle_filter) = tags_config.build_bundle_file_filter(bundle_name) {
                tag_filter = Some(match tag_filter {
                    Some(existing) => format!("{} AND {}", existing, bundle_filter),
                    None => bundle_filter,
                });
            }
        }

        let results: Vec<SearchResult> = match mode {
            "keyword" => vector_store
                .search_fts_filtered(&req.query, search_limit, repo_filter, tag_filter.as_deref())
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?,

            "semantic" | "hybrid" => {
                let model_dir = Config::model_cache_dir()
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let embedder = Embedder::from_config(&config.embedding, &model_dir)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                if mode == "semantic" {
                    let mut search = SemanticSearch::new(embedder, vector_store);
                    search
                        .search_filtered(
                            &req.query,
                            search_limit,
                            repo_filter,
                            tag_filter.as_deref(),
                        )
                        .await
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?
                } else {
                    let mut search =
                        HybridSearch::from_config(embedder, vector_store, &config.search);
                    search
                        .search_filtered(
                            &req.query,
                            search_limit,
                            repo_filter,
                            tag_filter.as_deref(),
                        )
                        .await
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?
                }
            }

            _ => {
                return Err(McpError::invalid_params(
                    format!(
                        "Invalid search mode: {}. Use 'hybrid', 'semantic', or 'keyword'",
                        mode
                    ),
                    None,
                ));
            }
        };

        let filtered: Vec<SearchResult> = if let Some(ref chunk_type) = type_filter {
            results
                .into_iter()
                .filter(|r| &r.chunk.chunk_type == chunk_type)
                .take(limit)
                .collect()
        } else {
            results.into_iter().take(limit).collect()
        };

        let response = SearchResponse {
            query: req.query,
            mode: mode.to_string(),
            count: filtered.len(),
            results: filtered.iter().map(Self::to_search_result_item).collect(),
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Keyword/regex search
    #[tool(
        description = "Search for code using exact keywords or regex patterns. Best for: finding specific function names, variable references, or pattern matching. Use ignore_case=true for case-insensitive search, regex=true for regex patterns.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn grep(
        &self,
        Parameters(req): Parameters<GrepRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.grep(&req).await;
        }

        let limit = req.limit.unwrap_or(10);
        let ignore_case = req.ignore_case.unwrap_or(false);
        let use_regex = req.regex.unwrap_or(false);

        let type_filter = if let Some(ref t) = req.r#type {
            Some(
                Self::parse_chunk_type(t)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?,
            )
        } else {
            None
        };

        let regex_pattern = if use_regex {
            let pattern = if ignore_case {
                format!("(?i){}", req.pattern)
            } else {
                req.pattern.clone()
            };
            Some(
                Regex::new(&pattern)
                    .map_err(|e| McpError::invalid_params(format!("Invalid regex: {}", e), None))?,
            )
        } else {
            None
        };

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let stats = vector_store
            .get_stats(None)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        if stats.total_chunks == 0 {
            return Ok(CallToolResult::success(vec![Content::text(
                "No indexed content. Run `bobbin index` first.",
            )]));
        }

        // Build FTS query
        let fts_query = if use_regex {
            let cleaned: String = req
                .pattern
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '_' || c == ' ' {
                        c
                    } else {
                        ' '
                    }
                })
                .collect();
            let words: Vec<&str> = cleaned
                .split_whitespace()
                .filter(|w| w.len() >= 2)
                .collect();
            if words.is_empty() {
                req.pattern.clone()
            } else {
                words.join(" OR ")
            }
        } else {
            req.pattern.clone()
        };

        let search_limit = if type_filter.is_some() || use_regex {
            limit * 5
        } else {
            limit
        };

        let results = vector_store
            .search_fts(&fts_query, search_limit, req.repo.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let filtered: Vec<SearchResult> = results
            .into_iter()
            .filter(|r| {
                if let Some(ref chunk_type) = type_filter {
                    &r.chunk.chunk_type == chunk_type
                } else {
                    true
                }
            })
            .filter(|r| {
                if let Some(ref re) = regex_pattern {
                    re.is_match(&r.chunk.content)
                        || r.chunk.name.as_ref().is_some_and(|n| re.is_match(n))
                } else {
                    true
                }
            })
            .filter(|r| {
                if !ignore_case && regex_pattern.is_none() {
                    r.chunk.content.contains(&req.pattern)
                        || r.chunk
                            .name
                            .as_ref()
                            .is_some_and(|n| n.contains(&req.pattern))
                } else {
                    true
                }
            })
            .take(limit)
            .collect();

        let response = GrepResponse {
            pattern: req.pattern.clone(),
            count: filtered.len(),
            results: filtered
                .iter()
                .map(|r| GrepResultItem {
                    file_path: r.chunk.file_path.clone(),
                    name: r.chunk.name.clone(),
                    chunk_type: r.chunk.chunk_type.to_string(),
                    source: crate::types::source_kind(&r.chunk.chunk_type).to_string(),
                    repo: r.repo.clone(),
                    start_line: r.chunk.start_line,
                    end_line: r.chunk.end_line,
                    score: r.score,
                    language: r.chunk.language.clone(),
                    content_preview: Self::truncate_content(&r.chunk.content, 200),
                    matching_lines: Self::find_matching_lines(
                        &r.chunk.content,
                        &req.pattern,
                        regex_pattern.as_ref(),
                        ignore_case,
                        r.chunk.start_line,
                    ),
                })
                .collect(),
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Assemble task-relevant context
    #[tool(
        description = "Assemble a comprehensive context bundle for a task. Given a natural language task description, combines semantic search results with temporally coupled files from git history. Returns a deduplicated, budget-aware set of relevant code chunks grouped by file. Ideal for understanding everything relevant to a task before making changes.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn context(
        &self,
        Parameters(req): Parameters<ContextRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.context(&req).await;
        }

        let config_path = Config::config_path(&self.repo_root);
        let config = Config::load(&config_path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let stats = vector_store
            .get_stats(None)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        if stats.total_chunks == 0 {
            return Ok(CallToolResult::success(vec![Content::text(
                "No indexed content. Run `bobbin index` first.",
            )]));
        }

        let metadata_store = self
            .open_metadata_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let model_dir =
            Config::model_cache_dir().map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let embedder = Embedder::from_config(&config.embedding, &model_dir)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut extra_filter =
            Self::build_tag_filter(req.tag.as_deref(), req.exclude_tag.as_deref());

        // Apply bundle filter if specified
        if let Some(ref bundle_name) = req.bundle {
            let tags_path = crate::tags::TagsConfig::tags_path(&self.repo_root);
            let tags_config = crate::tags::TagsConfig::load_or_default(&tags_path);
            if let Some(bundle_filter) = tags_config.build_bundle_file_filter(bundle_name) {
                extra_filter = Some(match extra_filter {
                    Some(existing) => format!("{} AND {}", existing, bundle_filter),
                    None => bundle_filter,
                });
            }
        }

        let context_config = ContextConfig {
            budget_lines: req.budget.unwrap_or(500),
            depth: req.depth.unwrap_or(1),
            max_coupled: req.max_coupled.unwrap_or(3),
            coupling_threshold: req.coupling_threshold.unwrap_or(0.1),
            semantic_weight: config.search.semantic_weight,
            content_mode: ContentMode::Full, // Always full content for MCP
            search_limit: req.limit.unwrap_or(20),
            doc_demotion: config.search.doc_demotion,
            recency_half_life_days: config.search.recency_half_life_days,
            recency_weight: config.search.recency_weight,
            rrf_k: config.search.rrf_k,
            bridge_mode: BridgeMode::default(),
            bridge_boost_factor: 0.3,
            extra_filter,
            tags_config: None,
            role: None,
            file_type_rules: config.file_types.clone(),
            repo_affinity: None,
            repo_affinity_boost: config.hooks.repo_affinity_boost,
            max_bridged_files: 2,
            max_bridged_chunks_per_file: 1,
            repo_path_prefix: config.server.repo_path_prefix.clone(),
            ..ContextConfig::default()
        };

        let mut assembler =
            ContextAssembler::new(embedder, vector_store, metadata_store, context_config);
        let bundle = assembler
            .assemble(&req.query, req.repo.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let response = ContextResponse {
            query: bundle.query,
            budget: ContextBudgetInfo {
                max_lines: bundle.budget.max_lines,
                used_lines: bundle.budget.used_lines,
            },
            files: bundle
                .files
                .iter()
                .map(|f| ContextFileOutput {
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
                })
                .collect(),
            summary: ContextSummaryOutput {
                total_files: bundle.summary.total_files,
                total_chunks: bundle.summary.total_chunks,
                direct_hits: bundle.summary.direct_hits,
                coupled_additions: bundle.summary.coupled_additions,
                bridged_additions: bundle.summary.bridged_additions,
                source_files: bundle.summary.source_files,
                doc_files: bundle.summary.doc_files,
            },
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Find related files
    #[tool(
        description = "Find files that are related to a given file based on git commit history. Files that frequently change together have higher coupling scores. Useful for understanding dependencies and impact analysis.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn related(
        &self,
        Parameters(req): Parameters<RelatedRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.related(&req).await;
        }

        let limit = req.limit.unwrap_or(10);
        let threshold = req.threshold.unwrap_or(0.0);

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Verify file exists in index via LanceDB
        if vector_store
            .get_file(&req.file)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .is_none()
        {
            return Err(McpError::invalid_params(
                format!("File not found in index: {}", req.file),
                None,
            ));
        }

        // Coupling data is in SQLite
        let store = self
            .open_metadata_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let couplings = store
            .get_coupling(&req.file, limit)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut related: Vec<RelatedFile> = couplings
            .into_iter()
            .filter(|c| c.score >= threshold)
            .map(|c| {
                let other_path = if c.file_a == req.file {
                    c.file_b
                } else {
                    c.file_a
                };
                RelatedFile {
                    path: other_path,
                    score: c.score,
                    co_changes: c.co_changes,
                    repo: None,
                }
            })
            .collect();

        // Cross-repo coupled files (bo-oqny) — MANDATORY access filtering applied
        // inside the helper. Role is resolved from the environment (BOBBIN_ROLE /
        // GT_ROLE / BD_ACTOR) exactly as other access-gated surfaces resolve it.
        let config = Config::load(&Config::config_path(&self.repo_root))
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let role = crate::access::RepoFilter::resolve_role(None);
        let filter = crate::access::RepoFilter::from_config(&config.access, &role);
        let cross = crate::index::cross_repo::related_cross_repo(
            &store,
            req.repo.as_deref(),
            &req.file,
            limit,
            threshold,
            &filter,
        )
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        related.extend(cross.into_iter().map(|c| RelatedFile {
            path: c.path,
            score: c.score,
            co_changes: c.co_changes,
            repo: Some(c.repo),
        }));
        related.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        related.truncate(limit);

        let response = RelatedResponse {
            file: req.file,
            related,
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Map test↔source coverage
    #[tool(
        description = "Map test↔source coverage inferred from git co-change history. Given a source file, returns the test files that change with it (the tests that likely cover it); given a test file, returns the source files it covers. Useful for: 'which tests exercise auth.rs?', 'what does test_auth.rs cover?'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn test_coverage(
        &self,
        Parameters(req): Parameters<TestCoverageRequest>,
    ) -> Result<CallToolResult, McpError> {
        if !self.has_local_index() {
            if let Some(remote) = self.remote() {
                return Err(remote.local_only(
                    "test_coverage",
                    "it infers test<->source links from the git co-change history of the \
                 checkout it runs in",
                ));
            }
        }

        let limit = req.limit.unwrap_or(10);
        let threshold = req.threshold.unwrap_or(0.0);

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Verify file exists in index via LanceDB
        if vector_store
            .get_file(&req.file)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .is_none()
        {
            return Err(McpError::invalid_params(
                format!("File not found in index: {}", req.file),
                None,
            ));
        }

        // Coupling data is in SQLite
        let store = self
            .open_metadata_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let couplings = store
            .get_coupling(&req.file, limit)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let (direction, links) = crate::index::coverage::derive_coverage(&req.file, couplings);

        let links: Vec<CoverageLinkOutput> = links
            .into_iter()
            .filter(|l| l.score >= threshold)
            .map(|l| CoverageLinkOutput {
                path: l.path,
                score: l.score,
                co_changes: l.co_changes,
            })
            .collect();

        let response = TestCoverageResponse {
            file: req.file,
            link_kind: direction.link_kind().to_string(),
            links,
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Find symbol references
    #[tool(
        description = "Find the definition and all usages of a symbol by name. Returns the definition location (file, line, signature) and all usage sites across the codebase. Best for: 'where is parse_config defined?', 'who calls handle_request?', 'find all uses of Config struct'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn find_refs(
        &self,
        Parameters(req): Parameters<FindRefsRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.find_refs(&req).await;
        }

        let limit = req.limit.unwrap_or(20);

        let mut vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut backend = IndexBackend::new(&mut vector_store);
        let refs = backend
            .find_refs(
                &req.symbol,
                req.r#type.as_deref(),
                limit,
                req.repo.as_deref(),
            )
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let response = FindRefsResponse {
            symbol: req.symbol,
            definition: refs.definition.map(|d| SymbolDefinitionOutput {
                name: d.name,
                chunk_type: d.chunk_type.to_string(),
                file_path: d.file_path,
                start_line: d.start_line,
                end_line: d.end_line,
                signature: d.signature,
            }),
            usage_count: refs.usages.len(),
            usages: refs
                .usages
                .iter()
                .map(|u| SymbolUsageOutput {
                    file_path: u.file_path.clone(),
                    line: u.line,
                    context: u.context.clone(),
                })
                .collect(),
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// List symbols in a file
    #[tool(
        description = "List all symbols (functions, structs, traits, etc.) defined in a file. Returns each symbol's name, type, line range, and signature. Best for: 'what functions are in main.rs?', 'list all structs in config.rs', 'show me the API of this module'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_symbols(
        &self,
        Parameters(req): Parameters<ListSymbolsRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.list_symbols(&req).await;
        }

        let mut vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut backend = IndexBackend::new(&mut vector_store);
        let file_symbols = backend
            .list_symbols(&req.file, req.repo.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let response = ListSymbolsResponse {
            file: file_symbols.path,
            count: file_symbols.symbols.len(),
            symbols: file_symbols
                .symbols
                .iter()
                .map(|s| SymbolItemOutput {
                    name: s.name.clone(),
                    chunk_type: s.chunk_type.to_string(),
                    start_line: s.start_line,
                    end_line: s.end_line,
                    signature: s.signature.clone(),
                })
                .collect(),
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Follow relationship edges from a chunk
    #[tool(
        description = "Follow relationship edges from a chunk: next_chunk (adjacent chunks in document order), part_of (containment: fn in impl, section under parent heading, table in section), implements, impl_for, extends, similar_to (near-duplicates persisted by `bobbin similar --scan --persist`). Anchor by chunk `id` (from search results) or by file+line. Direction 'out'+next_chunk = the following chunk, 'in' = preceding; part_of 'out' = containing parent, 'in' = children. Best for: reading the surrounding document of a search hit, walking a doc section by section, finding a section's parent or children. Requires dependency tracking (on by default); an empty result on a fresh index means the file has no edges, not a failure.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn chunk_neighbors(
        &self,
        Parameters(req): Parameters<ChunkNeighborsRequest>,
    ) -> Result<CallToolResult, McpError> {
        if !self.has_local_index() {
            if let Some(remote) = self.remote() {
                return Err(remote.local_only(
                    "chunk_neighbors",
                    "it walks chunk relationships inside the local vector store by chunk id",
                ));
            }
        }

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Resolve the anchor chunk: by ID, or by file+line (smallest
        // containing span — not first match, which would return the
        // outermost container for nested code).
        let anchor = if let Some(id) = &req.id {
            vector_store
                .get_chunk_by_id(id)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
                .ok_or_else(|| {
                    McpError::invalid_params(format!("No chunk with id '{}'", id), None)
                })?
        } else if let (Some(file), Some(line)) = (&req.file, req.line) {
            let chunks = vector_store
                .get_chunks_for_file(file, req.repo.as_deref())
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            chunks
                .into_iter()
                .filter(|c| c.start_line <= line && line <= c.end_line)
                .min_by_key(|c| c.end_line - c.start_line)
                .ok_or_else(|| {
                    McpError::invalid_params(
                        format!("No indexed chunk contains {}:{}", file, line),
                        None,
                    )
                })?
        } else {
            return Err(McpError::invalid_params(
                "Provide either `id` or both `file` and `line`",
                None,
            ));
        };

        let edge_type_filter = match req.edge_type.as_deref() {
            None => None,
            Some(s) => Some(
                crate::types::ChunkEdgeType::ALL
                    .iter()
                    .find(|t| t.to_string() == s)
                    .copied()
                    .ok_or_else(|| {
                        McpError::invalid_params(
                            format!(
                                "Unknown edge_type '{}'. One of: next_chunk, part_of, implements, impl_for, extends, tests, similar_to",
                                s
                            ),
                            None,
                        )
                    })?,
            ),
        };
        let direction = req.direction.as_deref().unwrap_or("both");
        if !matches!(direction, "out" | "in" | "both") {
            return Err(McpError::invalid_params(
                "direction must be \"out\", \"in\", or \"both\"",
                None,
            ));
        }
        let limit = req.limit.unwrap_or(20);

        let edges = vector_store
            .get_edges_for_chunk(&anchor.id, req.repo.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let chunk_ref = |c: &crate::types::Chunk| ChunkRef {
            id: c.id.clone(),
            file_path: c.file_path.clone(),
            name: c.name.clone(),
            chunk_type: c.chunk_type.to_string(),
            start_line: c.start_line,
            end_line: c.end_line,
        };

        let mut neighbors = Vec::new();
        let mut dangling = Vec::new();
        for edge in &edges {
            if let Some(t) = edge_type_filter {
                if edge.edge_type != t {
                    continue;
                }
            }
            let (edge_dir, neighbor_id) = if edge.source_chunk == anchor.id {
                ("out", &edge.target_chunk)
            } else {
                ("in", &edge.source_chunk)
            };
            if direction != "both" && edge_dir != direction {
                continue;
            }
            if neighbors.len() >= limit {
                break;
            }
            // Hydrate the neighbor; a stale ID (chunk re-hashed after an
            // edit) is reported in `dangling` rather than silently skipped.
            match vector_store
                .get_chunk_by_id(neighbor_id)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
            {
                Some(chunk) => neighbors.push(NeighborItem {
                    edge_type: edge.edge_type.to_string(),
                    direction: edge_dir.to_string(),
                    chunk: chunk_ref(&chunk),
                    content_preview: Self::truncate_content(&chunk.content, 300),
                }),
                None => dangling.push(neighbor_id.clone()),
            }
        }

        let response = ChunkNeighborsResponse {
            chunk: chunk_ref(&anchor),
            count: neighbors.len(),
            neighbors,
            dangling,
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Read a specific code chunk
    #[tool(
        description = "Read a specific section of code from a file. Specify the file path and line range. Optionally include context lines before and after.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn read_chunk(
        &self,
        Parameters(req): Parameters<ReadChunkRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.read_chunk(&req).await;
        }

        let context = req.context.unwrap_or(0);

        let (content, actual_start, actual_end) = self
            .read_file_lines(&req.file, req.start_line, req.end_line, context)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let response = ReadChunkResponse {
            file: req.file.clone(),
            start_line: req.start_line,
            end_line: req.end_line,
            actual_start_line: actual_start,
            actual_end_line: actual_end,
            content,
            language: Self::detect_language(&req.file),
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Identify code hotspots
    #[tool(
        description = "Identify code hotspots — files that are both frequently changed (high churn) and complex. Hotspots are the riskiest parts of a codebase: they change often and are hard to change safely. Score is the geometric mean of normalized churn and AST complexity. Best for: 'which files need refactoring?', 'find risky code', 'where are the maintenance bottlenecks?'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn hotspots(
        &self,
        Parameters(req): Parameters<HotspotsRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.hotspots(&req).await;
        }

        let since = req.since.as_deref().unwrap_or("1 year ago");
        let limit = req.limit.unwrap_or(20);
        let threshold = req.threshold.unwrap_or(0.0);

        let git = GitAnalyzer::new(&self.repo_root)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let churn_map = git
            .get_file_churn(Some(since))
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if churn_map.is_empty() {
            let response = HotspotsResponse {
                count: 0,
                since: since.to_string(),
                hotspots: vec![],
            };
            let json = serde_json::to_string_pretty(&response)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            return Ok(CallToolResult::success(vec![Content::text(json)]));
        }

        let mut analyzer =
            ComplexityAnalyzer::new().map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let max_churn = churn_map.values().copied().max().unwrap_or(1) as f32;
        let mut hotspots: Vec<HotspotItem> = Vec::new();

        for (file_path, churn) in &churn_map {
            let language = Self::detect_language(file_path);
            if matches!(
                language.as_str(),
                "unknown" | "markdown" | "json" | "yaml" | "toml" | "c"
            ) {
                continue;
            }

            let abs_path = self.repo_root.join(file_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let complexity = match analyzer.analyze_file(file_path, &content, &language) {
                Ok(fc) => fc.complexity,
                Err(_) => continue,
            };

            let churn_norm = (*churn as f32) / max_churn;
            let score = (churn_norm * complexity).sqrt();

            if score >= threshold {
                hotspots.push(HotspotItem {
                    file: file_path.clone(),
                    score,
                    churn: *churn,
                    complexity,
                    language,
                });
            }
        }

        hotspots.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hotspots.truncate(limit);

        let response = HotspotsResponse {
            count: hotspots.len(),
            since: since.to_string(),
            hotspots,
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Impact analysis
    #[tool(
        description = "Predict which files are affected by a change to a target file or function. Combines git co-change coupling and semantic similarity signals, with optional transitive expansion. Returns a ranked list of impacted files with signal attribution and scores. Best for: 'what breaks if I change this?', 'which files should I review after touching auth.rs?'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn impact(
        &self,
        Parameters(req): Parameters<ImpactRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.impact(&req).await;
        }

        let depth = req.depth.unwrap_or(1);
        let mode_str = req.mode.as_deref().unwrap_or("combined");
        let limit = req.limit.unwrap_or(15);
        let threshold = req.threshold.unwrap_or(0.1);

        let mode = match mode_str {
            "combined" => ImpactMode::Combined,
            "coupling" => ImpactMode::Coupling,
            "semantic" => ImpactMode::Semantic,
            "deps" => ImpactMode::Deps,
            _ => {
                return Err(McpError::invalid_params(
                    format!(
                        "Invalid mode: {}. Use: combined, coupling, semantic, deps",
                        mode_str
                    ),
                    None,
                ));
            }
        };

        let impact_config = ImpactConfig {
            mode,
            threshold,
            limit,
        };

        let config_path = Config::config_path(&self.repo_root);
        let config = Config::load(&config_path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut metadata_store = self
            .open_metadata_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let mut vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let model_dir =
            Config::model_cache_dir().map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let mut embedder = Embedder::from_config(&config.embedding, &model_dir)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut backend =
            IndexBackend::with_impact(&mut vector_store, &mut metadata_store, &mut embedder);
        let results = backend
            .impact(&req.target, &impact_config, depth, req.repo.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let signal_name = |s: &ImpactSignal| -> &'static str {
            match s {
                ImpactSignal::Coupling { .. } => "coupling",
                ImpactSignal::Semantic { .. } => "semantic",
                ImpactSignal::Dependency => "deps",
                ImpactSignal::Combined => "combined",
            }
        };

        let response = ImpactResponse {
            target: req.target,
            mode: mode_str.to_string(),
            depth,
            count: results.len(),
            results: results
                .iter()
                .map(|r| ImpactResultItem {
                    file: r.path.clone(),
                    signal: signal_name(&r.signal).to_string(),
                    score: r.score,
                    reason: r.reason.clone(),
                })
                .collect(),
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Diff-aware review context
    #[tool(
        description = "Assemble review context from a git diff. Given a diff specification (unstaged changes, staged changes, branch comparison, or commit range), finds the indexed code chunks that overlap with the changed lines and expands via temporal coupling. Returns a budget-aware context bundle with changed-file annotations. Ideal for code review: 'what do I need to understand to review these changes?'",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn review(
        &self,
        Parameters(req): Parameters<ReviewRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return Err(remote.review_unavailable());
        }

        let config_path = Config::config_path(&self.repo_root);
        let config = Config::load(&config_path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let stats = vector_store
            .get_stats(None)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        if stats.total_chunks == 0 {
            return Ok(CallToolResult::success(vec![Content::text(
                "No indexed content. Run `bobbin index` first.",
            )]));
        }

        let metadata_store = self
            .open_metadata_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let model_dir =
            Config::model_cache_dir().map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let embedder = Embedder::from_config(&config.embedding, &model_dir)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Parse diff spec from the request
        let diff_spec = parse_diff_spec(req.diff.as_deref());
        let diff_description = describe_diff_spec(&diff_spec);

        let git = GitAnalyzer::new(&self.repo_root)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let diff_files = git
            .get_diff_files(&diff_spec)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if diff_files.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                r#"{"error": "no_changes", "message": "No changes found for the specified diff."}"#,
            )]));
        }

        let seeds = crate::search::review::map_diff_to_chunks(
            &diff_files,
            &vector_store,
            req.repo.as_deref(),
        )
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let context_config = ContextConfig {
            budget_lines: req.budget.unwrap_or(500),
            depth: req.depth.unwrap_or(1),
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: config.search.semantic_weight,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: config.search.doc_demotion,
            recency_half_life_days: config.search.recency_half_life_days,
            recency_weight: config.search.recency_weight,
            rrf_k: config.search.rrf_k,
            bridge_mode: BridgeMode::default(),
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: config.file_types.clone(),
            repo_affinity: None,
            repo_affinity_boost: config.hooks.repo_affinity_boost,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: config.server.repo_path_prefix.clone(),
            ..ContextConfig::default()
        };

        let mut assembler =
            ContextAssembler::new(embedder, vector_store, metadata_store, context_config);
        let bundle = assembler
            .assemble_from_seeds(&diff_description, seeds, req.repo.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let response = ReviewResponse {
            diff_description,
            changed_files: diff_files
                .iter()
                .map(|f| ReviewChangedFile {
                    path: f.path.clone(),
                    status: f.status.to_string(),
                    added_lines: f.added_lines.len(),
                    removed_lines: f.removed_lines.len(),
                })
                .collect(),
            budget: ContextBudgetInfo {
                max_lines: bundle.budget.max_lines,
                used_lines: bundle.budget.used_lines,
            },
            files: bundle
                .files
                .iter()
                .map(|f| ContextFileOutput {
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
                })
                .collect(),
            summary: ContextSummaryOutput {
                total_files: bundle.summary.total_files,
                total_chunks: bundle.summary.total_chunks,
                direct_hits: bundle.summary.direct_hits,
                coupled_additions: bundle.summary.coupled_additions,
                bridged_additions: bundle.summary.bridged_additions,
                source_files: bundle.summary.source_files,
                doc_files: bundle.summary.doc_files,
            },
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Find similar code or scan for duplicates
    #[tool(
        description = "Find code chunks semantically similar to a target, or scan the entire codebase for near-duplicate clusters. Single-target mode: provide a chunk reference ('file.rs:function_name') or free text to find similar code. Scan mode: set scan=true to detect duplicate/near-duplicate code clusters across the codebase. Useful for: 'find code similar to this function', 'detect copy-paste duplicates', 'find redundant implementations'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn similar(
        &self,
        Parameters(req): Parameters<SimilarRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.similar(&req).await;
        }

        let scan = req.scan.unwrap_or(false);
        let limit = req.limit.unwrap_or(10);
        let cross_repo = req.cross_repo.unwrap_or(false);

        let config_path = Config::config_path(&self.repo_root);
        let config = Config::load(&config_path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let stats = vector_store
            .get_stats(None)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        if stats.total_chunks == 0 {
            return Ok(CallToolResult::success(vec![Content::text(
                "No indexed content. Run `bobbin index` first.",
            )]));
        }

        let model_dir =
            Config::model_cache_dir().map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let embedder = Embedder::from_config(&config.embedding, &model_dir)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut analyzer = SimilarityAnalyzer::new(embedder, vector_store);
        let repo_filter = req.repo.as_deref();

        let response = if scan {
            let threshold = req.threshold.unwrap_or(0.90);
            let clusters = analyzer
                .scan_duplicates(threshold, limit, repo_filter, cross_repo)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            SimilarResponse {
                mode: "scan".to_string(),
                threshold,
                target: None,
                count: clusters.len(),
                results: vec![],
                clusters: clusters
                    .iter()
                    .map(|c| SimilarClusterItem {
                        representative: SimilarChunkRef {
                            file_path: c.representative.file_path.clone(),
                            name: c.representative.name.clone(),
                            chunk_type: c.representative.chunk_type.to_string(),
                            start_line: c.representative.start_line,
                            end_line: c.representative.end_line,
                            language: c.representative.language.clone(),
                        },
                        avg_similarity: c.avg_similarity,
                        member_count: c.members.len(),
                        members: c
                            .members
                            .iter()
                            .map(|m| SimilarResultItem {
                                file_path: m.chunk.file_path.clone(),
                                name: m.chunk.name.clone(),
                                chunk_type: m.chunk.chunk_type.to_string(),
                                start_line: m.chunk.start_line,
                                end_line: m.chunk.end_line,
                                similarity: m.similarity,
                                language: m.chunk.language.clone(),
                                explanation: m.explanation.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
            }
        } else {
            let target_str = req.target.as_deref().ok_or_else(|| {
                McpError::invalid_params(
                    "Either 'target' or 'scan=true' is required".to_string(),
                    None,
                )
            })?;

            let threshold = req.threshold.unwrap_or(0.85);
            let target = Self::parse_similar_target(target_str);

            let results = analyzer
                .find_similar(&target, threshold, limit, repo_filter)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            SimilarResponse {
                mode: "single".to_string(),
                threshold,
                target: Some(target_str.to_string()),
                count: results.len(),
                results: results
                    .iter()
                    .map(|r| SimilarResultItem {
                        file_path: r.chunk.file_path.clone(),
                        name: r.chunk.name.clone(),
                        chunk_type: r.chunk.chunk_type.to_string(),
                        start_line: r.chunk.start_line,
                        end_line: r.chunk.end_line,
                        similarity: r.similarity,
                        language: r.chunk.language.clone(),
                        explanation: r.explanation.clone(),
                    })
                    .collect(),
                clusters: vec![],
            }
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Project primer/overview
    #[tool(
        description = "Get an LLM-friendly overview of Bobbin with live index statistics. Shows what Bobbin does, architecture, available commands, and MCP tools. Use 'section' to get a specific part, or 'brief' for a compact summary. Always includes live stats when the index is initialized.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn prime(
        &self,
        Parameters(req): Parameters<PrimeRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.prime(&req).await;
        }

        const PRIMER: &str = include_str!("../../docs/primer.md");

        let primer_text = if let Some(ref section) = req.section {
            Self::extract_primer_section(PRIMER, section)
        } else if req.brief.unwrap_or(false) {
            Self::extract_primer_brief(PRIMER)
        } else {
            PRIMER.to_string()
        };

        // Gather live stats
        let stats = match self.open_vector_store().await {
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
                    last_indexed: s.last_indexed.and_then(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0).map(|t| t.to_rfc3339())
                    }),
                }),
                Err(_) => None,
            },
            Err(_) => None,
        };

        let response = PrimeResponse {
            primer: primer_text,
            section: req.section,
            initialized: true, // Server only runs when initialized
            stats,
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Search indexed beads with configured-source metadata and freshness warnings.
    #[tool(
        description = "Search indexed beads using natural language, filtered by priority, status, assignee, rig, issue_type or label. Enrichment rereads the configured source; JSONL is an exported snapshot, not live tracker state. Results include metadata source, unknown as-of time and freshness warnings. Verify status/assignee with the active tracker before routing work. Compact mode omits snippets only. enrich=false uses indexed content. New beads since the last reindex may be absent; zero results do not prove absence from the current board.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn search_beads(
        &self,
        Parameters(req): Parameters<SearchBeadsRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.search_beads(&req).await;
        }

        let limit = req.limit.unwrap_or(10);
        let should_enrich = req.enrich.unwrap_or(true);
        let compact = req.compact.unwrap_or(true);

        let config_path = Config::config_path(&self.repo_root);
        let config = Config::load(&config_path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let model_dir =
            Config::model_cache_dir().map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let embedder = Embedder::from_config(&config.embedding, &model_dir)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut search = HybridSearch::new(embedder, vector_store, config.search.semantic_weight);

        // Push the Issue predicate into both halves of hybrid search.  Issue chunks
        // are a tiny fraction of the corpus, so searching the whole corpus and then
        // filtering a fixed over-fetch can return zero beads for common terms even
        // when matching Issue chunks exist.  Keep this aligned with GET /beads.
        let search_results = search
            .search_filtered(&req.query, limit, None, Some("chunk_type = 'issue'"))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut filtered: Vec<SearchResult> = search_results.into_iter().collect();

        // Apply rig filter (file_path is "beads:<rig>:<id>")
        if let Some(ref rig) = req.rig {
            let prefix = format!("beads:{}:", rig);
            filtered.retain(|r| r.chunk.file_path.starts_with(&prefix));
        }

        // Reread configured-source metadata before filtering; exports remain snapshots.
        let live_metadata = if should_enrich && config.beads.enabled {
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

            crate::index::beads::fetch_bead_metadata(&config.beads, &bead_ids)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
        } else {
            std::collections::HashMap::new()
        };

        // Apply all filters using live data when available
        let has_filters = req.status.is_some()
            || req.priority.is_some()
            || req.assignee.is_some()
            || req.issue_type.is_some()
            || req.label.is_some();
        if has_filters {
            filtered.retain(|r| {
                let bead_id = r.chunk.file_path.split(':').nth(2).unwrap_or("");

                if let Some(meta) = live_metadata.get(bead_id) {
                    if let Some(ref status) = req.status {
                        if meta.status != *status {
                            return false;
                        }
                    }
                    if let Some(priority) = req.priority {
                        if meta.priority != priority {
                            return false;
                        }
                    }
                    if let Some(ref assignee) = req.assignee {
                        let meta_assignee = meta.assignee.as_deref().unwrap_or("unassigned");
                        if !meta_assignee.contains(assignee.as_str()) {
                            return false;
                        }
                    }
                    if let Some(ref issue_type) = req.issue_type {
                        if meta.issue_type != *issue_type {
                            return false;
                        }
                    }
                    if let Some(ref label) = req.label {
                        if !meta.labels.iter().any(|l| l.contains(label.as_str())) {
                            return false;
                        }
                    }
                    true
                } else {
                    // Fall back to indexed content filtering
                    let content = &r.chunk.content;
                    if let Some(ref status) = req.status {
                        if !content.contains(&format!("Status: {}", status)) {
                            return false;
                        }
                    }
                    if let Some(priority) = req.priority {
                        if !content.contains(&format!("Priority: P{}", priority)) {
                            return false;
                        }
                    }
                    if let Some(ref assignee) = req.assignee {
                        if !content.contains(&format!("Assignee: {}", assignee)) {
                            return false;
                        }
                    }
                    // issue_type and label can't be reliably extracted from indexed content
                    true
                }
            });
        }

        // Boost relevance scores: title match and status weighting
        let query_lower = req.query.to_lowercase();
        let query_terms: Vec<&str> = query_lower.split_whitespace().collect();
        for result in &mut filtered {
            let mut boost: f32 = 1.0;

            // Title match boost: if query terms appear in the title, boost score
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

            // Status boost: open/in_progress are more actionable than closed
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

        let response = super::bead_response::build(
            req.query,
            &filtered,
            &live_metadata,
            &config.beads,
            compact,
        );

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Show import dependencies for a file
    #[tool(
        description = "Show import dependencies for a file. Returns what the file imports (forward dependencies) and/or what imports the file (reverse dependencies). Use 'reverse=true' to see dependents only, 'both=true' for both directions. Requires the index to include dependency data (enabled by default).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn dependencies(
        &self,
        Parameters(req): Parameters<DependenciesRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.dependencies(&req).await;
        }

        let reverse = req.reverse.unwrap_or(false);
        let both = req.both.unwrap_or(false);
        let show_imports = !reverse || both;
        let show_dependents = reverse || both;

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let imports = if show_imports {
            let deps = vector_store
                .get_dependencies(&req.file)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            Some(
                deps.into_iter()
                    .map(|d| DependencyItem {
                        path: if d.resolved {
                            d.file_b
                        } else {
                            d.import_statement.clone()
                        },
                        dep_type: d.dep_type,
                        symbol: d.symbol,
                        resolved: d.resolved,
                    })
                    .collect(),
            )
        } else {
            None
        };

        let imported_by = if show_dependents {
            let deps = vector_store
                .get_dependents(&req.file)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            Some(
                deps.into_iter()
                    .map(|d| DependencyItem {
                        path: d.file_a,
                        dep_type: d.dep_type,
                        symbol: d.symbol,
                        resolved: d.resolved,
                    })
                    .collect(),
            )
        } else {
            None
        };

        let response = DependenciesResponse {
            file: req.file,
            imports,
            imported_by,
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Show commit history for a specific file
    #[tool(
        description = "Show the git commit history for a specific file. Returns a list of commits that touched the file, along with statistics like author breakdown and churn rate (commits/month). Best for: 'who has worked on this file?', 'how often does this file change?', 'recent changes to config.rs'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn file_history(
        &self,
        Parameters(req): Parameters<FileHistoryRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.file_history(&req).await;
        }

        let limit = req.limit.unwrap_or(20);

        let git = GitAnalyzer::new(&self.repo_root)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let history = git
            .get_file_history(&req.file, limit)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Calculate statistics
        let mut author_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for entry in &history {
            *author_counts.entry(entry.author.clone()).or_insert(0) += 1;
        }

        let mut authors: Vec<FileHistoryAuthor> = author_counts
            .into_iter()
            .map(|(name, commits)| FileHistoryAuthor { name, commits })
            .collect();
        authors.sort_by(|a, b| b.commits.cmp(&a.commits));

        let churn_rate = if history.len() >= 2 {
            let first_ts = history.last().map(|e| e.timestamp).unwrap_or(0);
            let last_ts = history.first().map(|e| e.timestamp).unwrap_or(0);
            let days = ((last_ts - first_ts) as f32 / 86400.0).max(1.0);
            (history.len() as f32 / days) * 30.0
        } else {
            0.0
        };

        let response = FileHistoryResponse {
            file: req.file,
            entries: history
                .into_iter()
                .map(|h| FileHistoryItem {
                    date: h.date,
                    author: h.author,
                    message: h.message,
                    issues: h.issues,
                })
                .collect(),
            stats: FileHistoryStats {
                total_commits: authors.iter().map(|a| a.commits).sum(),
                authors,
                churn_rate,
            },
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Show index status and statistics
    #[tool(
        description = "Show the current index status and statistics for the bobbin instance. Returns file/chunk counts, dependency stats, repository list, and optionally a per-language breakdown. Best for: 'is the index up to date?', 'how many files are indexed?', 'what languages are in the codebase?'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn status(
        &self,
        Parameters(req): Parameters<StatusRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.status(&req).await;
        }

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let repos = vector_store
            .get_all_repos()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let stats = vector_store
            .get_stats(req.repo.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let dependency_stats =
            vector_store
                .get_dependency_stats()
                .await
                .ok()
                .and_then(|(total, resolved)| {
                    if total > 0 {
                        Some(StatusDependencyStats { total, resolved })
                    } else {
                        None
                    }
                });

        let languages = if req.detailed.unwrap_or(false) {
            stats
                .languages
                .iter()
                .map(|l| StatusLanguageStats {
                    language: l.language.clone(),
                    file_count: l.file_count,
                    chunk_count: l.chunk_count,
                })
                .collect()
        } else {
            vec![]
        };

        let response = StatusResponse {
            status: "ready".to_string(),
            total_files: stats.total_files,
            total_chunks: stats.total_chunks,
            total_embeddings: stats.total_embeddings,
            last_indexed: stats
                .last_indexed
                .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0).map(|t| t.to_rfc3339())),
            dependency_stats,
            repos,
            languages,
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Search git commits semantically to find commits by what they did
    #[tool(
        description = "Search git commit history using natural language. Finds commits by what they did, not just by message keywords. Best for: 'when was authentication added?', 'commits that changed the parser', 'who refactored the database layer?'. Filter by author or file path. Requires commits to be indexed (enabled by default in bobbin index).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn commit_search(
        &self,
        Parameters(req): Parameters<CommitSearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        if !self.has_local_index() {
            if let Some(remote) = self.remote() {
                return Err(remote.local_only(
                    "commit_search",
                    "it searches the commit index of the checkout it runs in",
                ));
            }
        }

        let limit = req.limit.unwrap_or(10);

        let config_path = Config::config_path(&self.repo_root);
        let config = Config::load(&config_path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let model_dir =
            Config::model_cache_dir().map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let embedder = Embedder::from_config(&config.embedding, &model_dir)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut search = HybridSearch::new(embedder, vector_store, config.search.semantic_weight);

        // Fetch extra results to filter down to commits
        let search_results = search
            .search(&req.query, limit * 5, None)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Filter to commit chunks, then apply author/file filters
        let filtered: Vec<SearchResult> = search_results
            .into_iter()
            .filter(|r| r.chunk.chunk_type == ChunkType::Commit)
            .filter(|r| {
                if let Some(ref author) = req.author {
                    let lower = author.to_lowercase();
                    r.chunk.content.to_lowercase().contains(&lower)
                } else {
                    true
                }
            })
            .filter(|r| {
                if let Some(ref file) = req.file {
                    r.chunk
                        .content
                        .to_lowercase()
                        .contains(&file.to_lowercase())
                } else {
                    true
                }
            })
            .take(limit)
            .collect();

        let results: Vec<CommitResultItem> = filtered
            .iter()
            .map(|r| {
                let hash = r
                    .chunk
                    .id
                    .strip_prefix("commit:")
                    .unwrap_or(&r.chunk.id)
                    .to_string();

                let content = &r.chunk.content;
                let mut message = String::new();
                let mut author = None;
                let mut date = None;
                let mut files = Vec::new();
                let mut in_files = false;

                for line in content.lines() {
                    if line.starts_with("Author: ") {
                        author = Some(line[8..].to_string());
                    } else if line.starts_with("Date: ") {
                        date = Some(line[6..].to_string());
                    } else if line == "Files changed:" {
                        in_files = true;
                    } else if in_files && !line.is_empty() {
                        files.push(line.to_string());
                    } else if !in_files && author.is_none() && !line.is_empty() {
                        if !message.is_empty() {
                            message.push(' ');
                        }
                        message.push_str(line);
                    }
                }

                if message.is_empty() {
                    message = r.chunk.name.clone().unwrap_or_default();
                }

                CommitResultItem {
                    hash,
                    message,
                    author,
                    date,
                    files,
                    score: r.score,
                }
            })
            .collect();

        let response = CommitSearchResponse {
            query: req.query,
            count: results.len(),
            results,
        };

        let json = serde_json::to_string_pretty(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Submit feedback on a bobbin context injection
    #[tool(
        description = "Submit feedback on a bobbin context injection. Rate injections as 'useful' (helped with task), 'noise' (irrelevant to current work), or 'harmful' (actively misleading). Reference the injection_id shown in [injection_id: inj-xxx] from the context injection output. Supports both standard (inj-xxx) and reaction (inj-react-xxx) injection IDs.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn feedback_submit(
        &self,
        Parameters(req): Parameters<FeedbackSubmitRequest>,
    ) -> Result<CallToolResult, McpError> {
        let injection_id = req.injection_id.trim().to_string();
        if injection_id.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "Error: injection_id required (from [injection_id: inj-xxx] in context injection output)",
            )]));
        }

        let valid_ratings = ["useful", "noise", "harmful"];
        if !valid_ratings.contains(&req.rating.as_str()) {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Error: rating must be one of: {}",
                valid_ratings.join(", ")
            ))]));
        }

        let agent = req.agent.unwrap_or_else(|| {
            std::env::var("GT_ROLE")
                .or_else(|_| std::env::var("BD_ACTOR"))
                .unwrap_or_else(|_| "unknown".to_string())
        });

        let reason = req.reason.unwrap_or_default();
        if reason.len() > 1000 {
            return Ok(CallToolResult::success(vec![Content::text(
                "Error: reason too long (max 1000 chars)",
            )]));
        }

        if let Some(remote) = self.remote() {
            let forwarded = FeedbackSubmitRequest {
                injection_id: injection_id.clone(),
                rating: req.rating.clone(),
                agent: Some(agent.clone()),
                reason: Some(reason.clone()),
            };
            return remote.feedback_submit(&forwarded, &agent).await;
        }

        let input = crate::storage::feedback::FeedbackInput {
            injection_id: injection_id.clone(),
            agent,
            rating: req.rating.clone(),
            reason,
        };

        let store = self
            .open_feedback_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        store
            .store_feedback(&input)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Feedback recorded: {} for {}",
            req.rating, injection_id
        ))]))
    }

    /// List recent bobbin feedback records
    #[tool(
        description = "List recent bobbin injection feedback records. Filter by rating (useful/noise/harmful) and/or agent name. Use to review feedback trends and identify problematic injections.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn feedback_list(
        &self,
        Parameters(req): Parameters<FeedbackListRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.feedback_list(&req).await;
        }

        let store = self
            .open_feedback_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let query = crate::storage::feedback::FeedbackQuery {
            injection_id: None,
            rating: req.rating,
            agent: req.agent,
            limit: req.limit,
        };

        let records = store
            .list_feedback(&query)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if records.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No feedback records found.",
            )]));
        }

        let mut lines = vec![format!("Feedback records ({}):", records.len())];
        for r in &records {
            let ts = if r.timestamp.len() > 19 {
                &r.timestamp[..19]
            } else {
                &r.timestamp
            };
            let mut line = format!("[{}] {} | {} | {}", r.injection_id, ts, r.rating, r.agent);
            if !r.reason.is_empty() {
                let truncated: String = r.reason.chars().take(100).collect();
                line.push_str(&format!(" | {}", truncated));
            }
            lines.push(line);
        }

        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }

    /// Get bobbin feedback statistics
    #[tool(
        description = "Get aggregated bobbin injection feedback statistics — total injections, feedback count, coverage rate, and rating breakdown (useful/noise/harmful). Use group_by='bundle' or group_by='bead' to see per-bundle or per-bead breakdowns.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn feedback_stats(
        &self,
        Parameters(req): Parameters<FeedbackStatsRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.feedback_stats(&req).await;
        }

        let store = self
            .open_feedback_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        match req.group_by.as_deref() {
            Some("bundle") | Some("bead") => {
                let group_by = req.group_by.as_deref().unwrap();
                let entries = if group_by == "bundle" {
                    store.stats_by_bundle()
                } else {
                    store.stats_by_bead()
                }
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                if entries.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "No injection data found.",
                    )]));
                }

                let label = if group_by == "bundle" {
                    "Bundle"
                } else {
                    "Bead"
                };
                let mut lines = vec![format!(
                    "{:<30} {:>5} {:>5} {:>6} {:>5} {:>7}",
                    label, "Inj", "Fb", "Good", "Noise", "Harmful"
                )];
                for e in &entries {
                    lines.push(format!(
                        "{:<30} {:>5} {:>5} {:>6} {:>5} {:>7}",
                        e.key, e.injections, e.feedback, e.useful, e.noise, e.harmful
                    ));
                }
                Ok(CallToolResult::success(vec![Content::text(
                    lines.join("\n"),
                )]))
            }
            _ => {
                let stats = store
                    .stats()
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let rate = if stats.total_injections > 0 {
                    format!(
                        "{:.0}%",
                        stats.total_feedback as f64 / stats.total_injections as f64 * 100.0
                    )
                } else {
                    "N/A".to_string()
                };

                let text = format!(
                    "Injections: {}\nFeedback: {} ({} coverage)\n  useful: {}\n  noise: {}\n  harmful: {}",
                    stats.total_injections,
                    stats.total_feedback,
                    rate,
                    stats.useful,
                    stats.noise,
                    stats.harmful
                );

                if stats.lineage_records > 0 || stats.actioned > 0 {
                    let text = format!(
                        "{}\nLineage: {} records\n  actioned: {}\n  unactioned: {}",
                        text, stats.lineage_records, stats.actioned, stats.unactioned
                    );
                    return Ok(CallToolResult::success(vec![Content::text(text)]));
                }

                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
        }
    }

    /// Record a lineage action linking feedback to a fix (commit, bead, config change)
    #[tool(
        description = "Record a lineage action that ties bobbin feedback to a concrete fix. Links one or more feedback records (by ID) to a commit, bead, or config change. This closes the feedback loop — proving that feedback led to action. Use after fixing an issue that feedback identified. Action types: 'code_fix', 'config_change', 'tag_effect', 'access_rule', 'exclusion_rule'.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn feedback_lineage_store(
        &self,
        Parameters(req): Parameters<FeedbackLineageStoreRequest>,
    ) -> Result<CallToolResult, McpError> {
        if req.feedback_ids.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "Error: feedback_ids must not be empty. Get IDs from bobbin_feedback_list.",
            )]));
        }

        let valid_actions = [
            "access_rule",
            "tag_effect",
            "config_change",
            "code_fix",
            "exclusion_rule",
        ];
        if !valid_actions.contains(&req.action_type.as_str()) {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Error: action_type must be one of: {}",
                valid_actions.join(", ")
            ))]));
        }

        let agent = req.agent.clone().unwrap_or_else(|| {
            std::env::var("GT_ROLE")
                .or_else(|_| std::env::var("BD_ACTOR"))
                .unwrap_or_else(|_| "unknown".to_string())
        });

        if let Some(remote) = self.remote() {
            return remote.feedback_lineage_store(&req, &agent).await;
        }

        let input = crate::storage::feedback::LineageInput {
            feedback_ids: req.feedback_ids.clone(),
            action_type: req.action_type.clone(),
            bead: req.bead.clone(),
            commit_hash: req.commit_hash.clone(),
            description: req.description.clone(),
            agent: Some(agent),
        };

        let store = self
            .open_feedback_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let lineage_id = store
            .store_lineage(&input)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut summary = format!(
            "Lineage #{} recorded: {} linking {} feedback record(s)",
            lineage_id,
            req.action_type,
            req.feedback_ids.len()
        );
        if let Some(ref bead) = req.bead {
            summary.push_str(&format!(" | bead: {}", bead));
        }
        if let Some(ref commit) = req.commit_hash {
            summary.push_str(&format!(" | commit: {}", commit));
        }

        Ok(CallToolResult::success(vec![Content::text(summary)]))
    }

    /// List lineage records showing how feedback was acted on
    #[tool(
        description = "List lineage records showing how bobbin feedback was acted on. Each record links feedback IDs to a concrete action (code fix, config change, etc.) with optional bead and commit references. Filter by feedback_id, bead, or commit_hash. Use to audit the feedback-to-fix pipeline and verify that feedback is being closed.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn feedback_lineage_list(
        &self,
        Parameters(req): Parameters<FeedbackLineageListRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.feedback_lineage_list(&req).await;
        }

        let store = self
            .open_feedback_store()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let query = crate::storage::feedback::LineageQuery {
            feedback_id: req.feedback_id,
            bead: req.bead,
            commit_hash: req.commit_hash,
            limit: req.limit,
        };

        let records = store
            .list_lineage(&query)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if records.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No lineage records found.",
            )]));
        }

        let mut lines = vec![format!("Lineage records ({}):", records.len())];
        for r in &records {
            let ts = if r.timestamp.len() > 19 {
                &r.timestamp[..19]
            } else {
                &r.timestamp
            };
            let mut line = format!(
                "#{} [{}] {} | {} | feedback: {:?}",
                r.id, ts, r.action_type, r.description, r.feedback_ids
            );
            if let Some(ref bead) = r.bead {
                line.push_str(&format!(" | bead: {}", bead));
            }
            if let Some(ref commit) = r.commit_hash {
                line.push_str(&format!(" | commit: {}", commit));
            }
            if let Some(ref agent) = r.agent {
                line.push_str(&format!(" | by: {}", agent));
            }
            lines.push(line);
        }

        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }

    /// Search archive records (HLA chat logs, Pensieve agent memory)
    #[tool(
        description = "Search archive records using natural language. Archives include HLA (IRC/Telegram chat logs) and Pensieve (agent memory/snapshots). Filter by source ('hla' or 'pensieve'), name/channel, and date range. Best for: 'recent deploy discussions', 'what did goldblum decide about auth?', 'telegram messages about cert renewal'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn archive_search(
        &self,
        Parameters(req): Parameters<ArchiveSearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.archive_search(&req).await;
        }

        let limit = req.limit.unwrap_or(10);
        let mode = req.mode.as_deref().unwrap_or("hybrid");

        let config_path = Config::config_path(&self.repo_root);
        let config = Config::load(&config_path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if !config.archive.enabled || config.archive.sources.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No archive sources configured. Archive search requires [[archive.sources]] in config.toml.",
            )]));
        }

        let archive_languages: Vec<String> = config
            .archive
            .sources
            .iter()
            .map(|s| s.name.clone())
            .collect();

        let vector_store = self
            .open_vector_store()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let model_dir =
            Config::model_cache_dir().map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let embedder = Embedder::from_config(&config.embedding, &model_dir)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Build language filter for archive chunks only
        let lang_filter = if archive_languages.len() == 1 {
            format!("language = '{}'", archive_languages[0].replace('\'', "''"))
        } else {
            let quoted: Vec<String> = archive_languages
                .iter()
                .map(|l| format!("'{}'", l.replace('\'', "''")))
                .collect();
            format!("language IN ({})", quoted.join(", "))
        };

        let results = match mode {
            "keyword" => vector_store
                .search_fts_filtered(&req.query, limit * 2, None, Some(&lang_filter))
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?,
            "semantic" => {
                let mut search = SemanticSearch::new(embedder, vector_store);
                search
                    .search_filtered(&req.query, limit * 2, None, Some(&lang_filter))
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
            }
            _ => {
                let mut search =
                    HybridSearch::new(embedder, vector_store, config.search.semantic_weight);
                search
                    .search_filtered(&req.query, limit * 2, None, Some(&lang_filter))
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
            }
        };

        // Post-filter
        let mut filtered: Vec<_> = results
            .into_iter()
            .filter(|r| archive_languages.contains(&r.chunk.language))
            .collect();

        // Source filter
        if let Some(ref source) = req.source {
            filtered.retain(|r| &r.chunk.language == source);
        }

        // Date filters
        if let Some(ref after) = req.after {
            filtered.retain(|r| {
                extract_archive_date(&r.chunk.file_path)
                    .is_some_and(|d| d.as_str() >= after.as_str())
            });
        }
        if let Some(ref before) = req.before {
            filtered.retain(|r| {
                extract_archive_date(&r.chunk.file_path)
                    .is_some_and(|d| d.as_str() <= before.as_str())
            });
        }

        // Name/channel filter
        if let Some(ref name_filter) = req.filter {
            filtered.retain(|r| {
                r.chunk
                    .name
                    .as_ref()
                    .is_some_and(|n| n.starts_with(&format!("{}/", name_filter)))
            });
        }

        filtered.truncate(limit);

        if filtered.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No archive results for '{}'. Try broader terms or different source/date filters.",
                req.query
            ))]));
        }

        let mut text = format!(
            "Archive search: {} results for '{}'\n\n",
            filtered.len(),
            req.query
        );
        for r in &filtered {
            let date = extract_archive_date(&r.chunk.file_path).unwrap_or_default();
            let source = &r.chunk.language;
            let id = r.chunk.name.as_deref().unwrap_or("-");
            text.push_str(&format!(
                "--- {} ({}, {}) score={:.3} ---\n",
                id, source, date, r.score
            ));
            text.push_str(&Self::truncate_content(&r.chunk.content, 500));
            text.push_str("\n\n");
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// List recent archive records
    #[tool(
        description = "List recent archive records by date. Returns records from HLA (chat logs) or Pensieve (agent memory) after a specified date. Best for: 'what happened in chat today?', 'recent agent decisions', 'show me today's Pensieve entries'.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn archive_recent(
        &self,
        Parameters(req): Parameters<ArchiveRecentRequest>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(remote) = self.remote() {
            return remote.archive_recent(&req).await;
        }

        let limit = req.limit.unwrap_or(20);

        let config_path = Config::config_path(&self.repo_root);
        let config = Config::load(&config_path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if !config.archive.enabled || config.archive.sources.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No archive sources configured.",
            )]));
        }

        // Listing recent records is a filesystem walk over the configured
        // sources — the same one `/archive/recent` serves. This used to be a
        // separate FTS query for the literal token "*", which matches nothing,
        // so the tool answered "No archive records found" for every input while
        // the HTTP endpoint returned rows for the identical query (aegis-44n1cy).
        let records = crate::index::archive::collect_recent(
            &config.archive,
            req.after.as_deref(),
            req.source.as_deref(),
            limit,
        );

        if records.is_empty() {
            let window = req
                .after
                .as_deref()
                .map(|a| format!("after {}", a))
                .unwrap_or_else(|| "in the last 30 days".to_string());
            let scope = req
                .source
                .as_deref()
                .map(|s| format!(" for source '{}'", s))
                .unwrap_or_default();
            let configured: Vec<&str> = config
                .archive
                .sources
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            // Name the sources that were actually searched: an empty result
            // for an unconfigured source is a different fact from an empty
            // window, and the caller cannot tell them apart otherwise.
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No archive records found {}{}. Configured sources: {}.",
                window,
                scope,
                configured.join(", ")
            ))]));
        }

        let mut text = format!("Archive recent: {} records\n\n", records.len());
        for r in &records {
            text.push_str(&format!(
                "--- {} ({}, {}) ---\n",
                r.id, r.source, r.timestamp
            ));
            text.push_str(&Self::truncate_content(&r.body, 500));
            text.push_str("\n\n");
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

/// Extract date from archive path like "hla:2026/03/05/file.md" → "2026-03-05"
fn extract_archive_date(path: &str) -> Option<String> {
    let after_prefix = path.split_once(':').map(|(_, rest)| rest)?;
    let parts: Vec<&str> = after_prefix.splitn(4, '/').collect();
    if parts.len() >= 3 && parts[0].len() == 4 && parts[1].len() == 2 && parts[2].len() == 2 {
        Some(format!("{}-{}-{}", parts[0], parts[1], parts[2]))
    } else {
        None
    }
}

/// Extract a field value from bead content like "Status: open | Priority: P2 | ..."
pub(super) fn extract_field(content: &str, prefix: &str) -> String {
    content
        .lines()
        .find(|line| line.contains(prefix))
        .and_then(|line| {
            let start = line.find(prefix)? + prefix.len();
            let rest = &line[start..];
            let end = rest.find(" | ").unwrap_or(rest.len());
            Some(rest[..end].trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Clean a bead snippet by removing metadata lines that are already in structured fields.
/// Returns the description/content portion only, truncated to max_len.
pub(super) fn clean_bead_snippet(content: &str, max_len: usize) -> String {
    let cleaned: String = content
        .lines()
        .filter(|line| {
            // Skip metadata lines already represented in structured fields
            let trimmed = line.trim();
            !(trimmed.starts_with("Status: ") && trimmed.contains(" | "))
                && !trimmed.starts_with("Priority: P")
                && !trimmed.starts_with("Assignee: ")
                && !trimmed.starts_with("Comments:")
                && !trimmed.starts_with("--- ")
                && !trimmed.starts_with("Notes:")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let trimmed = cleaned.trim();
    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &trimmed[..end])
    }
}

impl BobbinMcpServer {
    /// Parse a target string into a SimilarTarget for MCP.
    fn parse_similar_target(s: &str) -> SimilarTarget {
        if let Some(colon_pos) = s.find(':') {
            let before = &s[..colon_pos];
            if before.contains('.') || before.contains('/') {
                return SimilarTarget::ChunkRef(s.to_string());
            }
        }
        SimilarTarget::Text(s.to_string())
    }

    fn extract_primer_brief(primer: &str) -> String {
        let mut result = String::new();
        let mut heading_count = 0;

        for line in primer.lines() {
            if line.starts_with("## ") {
                heading_count += 1;
                if heading_count > 1 {
                    break;
                }
            }
            result.push_str(line);
            result.push('\n');
        }

        result.trim_end().to_string()
    }

    fn extract_primer_section(primer: &str, query: &str) -> String {
        let query_lower = query.to_lowercase();
        let sections = [
            "what bobbin does",
            "architecture",
            "supported languages",
            "key commands",
            "mcp tools",
            "quick start",
            "configuration",
        ];

        let target = sections
            .iter()
            .find(|s| s.contains(&query_lower.as_str()) || query_lower.contains(*s))
            .copied()
            .unwrap_or(query_lower.as_str());

        let mut result = String::new();
        let mut capturing = false;

        for line in primer.lines() {
            if line.starts_with("## ") {
                if capturing {
                    break;
                }
                let heading = line.trim_start_matches('#').trim().to_lowercase();
                if heading.contains(target) || target.contains(heading.as_str()) {
                    capturing = true;
                }
            }

            if capturing {
                result.push_str(line);
                result.push('\n');
            }
        }

        if result.is_empty() {
            format!(
                "Section '{}' not found. Available sections: {}",
                query,
                sections.join(", ")
            )
        } else {
            result.trim_end().to_string()
        }
    }
}

#[tool_handler]
impl ServerHandler for BobbinMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
            server_info: Implementation {
                name: "bobbin".to_string(),
                title: Some("Bobbin Code Search".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Bobbin is a semantic code search engine. Use the search tool for natural language queries, \
                grep for exact pattern matching, find_refs to find symbol definitions and usages, \
                list_symbols to see all symbols in a file, related for finding coupled files, read_chunk to \
                view specific code sections, dependencies for import analysis, file_history for git history of a file, \
                and status for index health. Start with `bobbin://index/stats` to see the index status."
                    .to_string(),
            ),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            meta: None,
            resources: vec![Annotated::new(
                RawResource::new("bobbin://index/stats", "Index Statistics"),
                None,
            )],
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        if request.uri == "bobbin://index/stats" {
            // Same backend as the `status` tool: a resource reporting the
            // local index while the tools answer from the remote would be
            // aegis-wbbycq in miniature.
            let stats_json = if let Some(remote) = self.remote() {
                remote
                    .stats_json()
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
            } else {
                self.get_stats_json()
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
            };

            Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(stats_json, &request.uri)],
            })
        } else {
            Err(McpError::resource_not_found(
                format!("Unknown resource: {}", request.uri),
                None,
            ))
        }
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult {
            meta: None,
            prompts: vec![Prompt::new(
                "explore_codebase",
                Some("Guided exploration of the codebase with focused prompts for understanding architecture, entry points, dependencies, and tests"),
                Some(vec![rmcp::model::PromptArgument {
                    name: "focus".to_string(),
                    title: Some("Focus Area".to_string()),
                    description: Some(
                        "Optional focus area: 'architecture', 'entry_points', 'dependencies', 'tests', or a custom query"
                            .to_string(),
                    ),
                    required: Some(false),
                }]),
            )],
            next_cursor: None,
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        if request.name != "explore_codebase" {
            return Err(McpError::invalid_params(
                format!("Unknown prompt: {}", request.name),
                None,
            ));
        }

        let focus = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("focus"))
            .and_then(|v| v.as_str())
            .unwrap_or("architecture");

        let prompt_text = self
            .build_explore_prompt(focus)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(GetPromptResult {
            description: Some(format!("Explore codebase with focus on: {}", focus)),
            messages: vec![PromptMessage::new_text(
                PromptMessageRole::User,
                prompt_text,
            )],
        })
    }
}

/// Parse a diff spec string into a DiffSpec enum.
///
/// Accepts: "unstaged" (default), "staged", "branch:<name>", or a commit range like "HEAD~3..HEAD".
fn parse_diff_spec(spec: Option<&str>) -> crate::index::git::DiffSpec {
    use crate::index::git::DiffSpec;
    match spec {
        None | Some("unstaged") | Some("") => DiffSpec::Unstaged,
        Some("staged") => DiffSpec::Staged,
        Some(s) if s.starts_with("branch:") => DiffSpec::Branch(s[7..].to_string()),
        Some(range) => DiffSpec::Range(range.to_string()),
    }
}

/// Human-readable description of a DiffSpec.
fn describe_diff_spec(spec: &crate::index::git::DiffSpec) -> String {
    use crate::index::git::DiffSpec;
    match spec {
        DiffSpec::Unstaged => "unstaged changes".to_string(),
        DiffSpec::Staged => "staged changes".to_string(),
        DiffSpec::Branch(b) => format!("branch: {}", b),
        DiffSpec::Range(r) => format!("range: {}", r),
    }
}

/// Run the MCP server on stdio transport
pub async fn run_server(
    repo_root: PathBuf,
    remote_url: Option<String>,
    role: String,
) -> Result<()> {
    use rmcp::transport::stdio;
    use rmcp::ServiceExt;

    if let Some(ref url) = remote_url {
        // stderr, not stdout — stdout is the MCP transport.
        eprintln!("Bobbin MCP server proxying to {}", url);
    }

    let server = BobbinMcpServer::with_remote(repo_root, remote_url, role)?;

    let service = server.serve(stdio()).await?;

    service.waiting().await?;

    Ok(())
}

/// Run the MCP server over Streamable HTTP transport (network-accessible).
pub async fn run_http_server(
    repo_root: PathBuf,
    port: u16,
    remote_url: Option<String>,
    role: String,
) -> Result<()> {
    use rmcp::transport::{
        streamable_http_server::{
            session::local::LocalSessionManager, tower::StreamableHttpService,
        },
        StreamableHttpServerConfig,
    };
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    // Validate config before binding
    let _ = BobbinMcpServer::with_remote(repo_root.clone(), remote_url.clone(), role.clone())?;

    let ct = CancellationToken::new();

    let root = repo_root.clone();
    let remote_for_factory = remote_url.clone();
    let role_for_factory = role.clone();
    let service: StreamableHttpService<BobbinMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                BobbinMcpServer::with_remote(
                    root.clone(),
                    remote_for_factory.clone(),
                    role_for_factory.clone(),
                )
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            },
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig {
                // STATELESS ON PURPOSE — do not flip this back to `true` (aegis-ks9cl).
                //
                // `LocalSessionManager` holds sessions in memory, and this MCP server
                // shares ONE process with the search server (see cli/serve.rs: both are
                // driven by a single `tokio::select!`). So every `systemctl restart
                // bobbin` — i.e. EVERY DEPLOY — wiped all MCP sessions, and each agent
                // session that had already completed `initialize` kept sending an
                // `Mcp-Session-Id` the new process had never issued.
                //
                // rmcp answers an unknown session id with 401 "Unauthorized: Session not
                // found" (rmcp-0.12 streamable_http_server/tower.rs:199-204). An MCP
                // client reads 401 as an auth challenge and goes looking for OAuth
                // metadata; our router only mounts /mcp, so /.well-known/* returns 404
                // with an EMPTY body, and the client surfaces the useless
                //     HTTP 404: Invalid OAuth error response ... Raw body: <EMPTY>
                // for EVERY bobbin tool, while the server itself is perfectly healthy.
                // The failure is permanent for that agent session: the client never
                // re-initializes, so the agent silently falls back to grep. Stateless
                // mode avoids that stale-session failure.
                // Stateless mode removes the session concept altogether: every POST is
                // self-contained, so a restart cannot orphan a client. It is ~free here
                // because `BobbinMcpServer::new` only checks that the config file exists
                // and builds the tool router — every store is opened per tool call.
                stateful_mode: false,
                sse_keep_alive: Some(std::time::Duration::from_secs(15)),
                cancellation_token: ct.child_token(),
            },
        );

    let router =
        axum::Router::new()
            .nest_service("/mcp", service)
            .layer(axum::middleware::from_fn(
                crate::operational_metrics::count_mcp_request,
            ));
    let config_path = crate::config::Config::config_path(&repo_root);
    let bind_host = if config_path.exists() {
        crate::config::Config::load(&config_path)
            .ok()
            .and_then(|c| c.server.bind_address)
            .unwrap_or_else(|| "0.0.0.0".to_string())
    } else {
        "0.0.0.0".to_string()
    };
    let addr = format!("{}:{}", bind_host, port);
    eprintln!("Bobbin MCP server listening on http://{}/mcp", addr);

    let tcp_listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(tcp_listener, router)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.ok();
            ct.cancel();
        })
        .await?;

    Ok(())
}
