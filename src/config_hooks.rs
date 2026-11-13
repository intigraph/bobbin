//! Prompt hook configuration and keyword preferences.

use serde::{Deserialize, Serialize};

/// Configuration for Claude Code hooks integration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Default prompt retrieval group; absent preserves unrestricted retrieval.
    #[serde(default)]
    pub search_group: Option<String>,
    /// Role glob -> retrieval groups. Most-specific matching glob wins.
    #[serde(default)]
    pub roles: std::collections::BTreeMap<String, Vec<String>>,
    /// Minimum relevance score to include in injected context
    pub threshold: f32,
    /// Maximum lines of injected context
    pub budget: usize,
    /// Content display mode: "full", "preview", or "none"
    pub content_mode: String,
    /// Skip injection for prompts shorter than this
    pub min_prompt_length: usize,
    /// Minimum raw semantic similarity to inject context at all.
    /// If the top semantic search result scores below this threshold,
    /// the entire injection is skipped (the query isn't relevant enough).
    pub gate_threshold: f32,
    /// Skip injection when search results haven't changed since last injection
    pub dedup_enabled: bool,
    /// Include documentation files in injection output (default: true).
    /// When false, doc files are excluded from output but still used for
    /// provenance bridging to discover relevant source files.
    pub show_docs: bool,
    /// Injection output format: "standard", "minimal", "verbose", or "xml".
    /// Controls how chunks are presented to agents.
    #[serde(default = "default_format_mode")]
    pub format_mode: String,
    /// Enable session-level progressive reducing: track injected chunks across
    /// turns and only inject new/changed chunks (delta injection).
    #[serde(default = "default_true")]
    pub reducing_enabled: bool,
    /// Keyword-triggered repo scoping rules. When a query matches keywords,
    /// search is scoped to the matched repos instead of all repos.
    #[serde(default)]
    pub keyword_repos: Vec<KeywordRepoRule>,
    /// Score multiplier for files from the agent's current repo (soft affinity).
    /// Default: 2.0. Set to 1.0 to disable.
    #[serde(default = "default_repo_affinity_boost")]
    pub repo_affinity_boost: f32,
    /// Prompt agents to rate injections every N injections (0 = disabled).
    /// Default: 5.
    #[serde(default = "default_feedback_prompt_interval")]
    pub feedback_prompt_interval: u64,
    /// Prompt prefixes that skip injection entirely (case-insensitive).
    /// Use for operational commands that never need context (e.g., "git push",
    /// "bd ready", "gt hook"). Matched as prefix of trimmed prompt.
    #[serde(default)]
    pub skip_prefixes: Vec<String>,
    /// When true, matched bundles inject their member file/ref lists inline
    /// instead of just a signpost. Default: true.
    #[serde(default = "default_true")]
    pub bundle_auto_inject: bool,
    /// Maximum lines per bundle for auto-injection (default: 30).
    /// Controls how much bundle content is included inline.
    #[serde(default = "default_bundle_inject_lines")]
    pub bundle_inject_lines: usize,
    /// Maximum number of bundles to auto-inject per query (default: 2).
    #[serde(default = "default_bundle_max_inject")]
    pub bundle_max_inject: usize,
}

/// A rule that maps query keywords to repository names.
/// When any keyword matches (case-insensitive substring), the matched repos
/// are added to the search scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordRepoRule {
    /// Keywords to match against the query (case-insensitive substring match)
    pub keywords: Vec<String>,
    /// Repository names to include when keywords match
    pub repos: Vec<String>,
}

fn default_format_mode() -> String {
    "standard".into()
}

fn default_repo_affinity_boost() -> f32 {
    2.0
}

fn default_feedback_prompt_interval() -> u64 {
    5
}

fn default_true() -> bool {
    true
}

fn default_bundle_inject_lines() -> usize {
    30
}

fn default_bundle_max_inject() -> usize {
    2
}

/// Valid format modes for injection output.
pub const VALID_FORMAT_MODES: &[&str] = &["standard", "minimal", "verbose", "xml"];

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            search_group: None,
            roles: Default::default(),
            threshold: 0.5,
            budget: 300,
            content_mode: "full".into(),
            min_prompt_length: 20,
            gate_threshold: 0.45,
            dedup_enabled: true,
            show_docs: true,
            format_mode: "standard".into(),
            reducing_enabled: true,
            keyword_repos: vec![],
            repo_affinity_boost: 2.0,
            feedback_prompt_interval: 5,
            skip_prefixes: vec![],
            bundle_auto_inject: true,
            bundle_inject_lines: 30,
            bundle_max_inject: 2,
        }
    }
}

impl HooksConfig {
    /// Resolve keyword-triggered repos from a query string.
    /// Returns a deduplicated list of repo names matched by any keyword rule.
    pub fn resolve_keyword_repos(&self, query: &str) -> Vec<String> {
        if self.keyword_repos.is_empty() {
            return vec![];
        }
        let query_lower = query.to_lowercase();
        let mut repos: Vec<String> = Vec::new();
        for rule in &self.keyword_repos {
            if rule
                .keywords
                .iter()
                .any(|kw| query_lower.contains(&kw.to_lowercase()))
            {
                for repo in &rule.repos {
                    if !repos.contains(repo) {
                        repos.push(repo.clone());
                    }
                }
            }
        }
        repos
    }
}
