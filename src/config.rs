use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Main configuration for Bobbin
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub index: IndexConfig,
    pub embedding: EmbeddingConfig,
    pub search: SearchConfig,
    pub git: GitConfig,
    pub context: ContextTuning,
    pub feedback: FeedbackTuning,
    pub dependencies: DependencyConfig,
    pub hooks: HooksConfig,
    pub beads: BeadsConfig,
    pub archive: ArchiveConfig,
    /// SQL database sources (`[[sql.sources]]`)
    #[serde(default)]
    pub sql: SqlConfig,
    pub access: AccessConfig,
    pub sources: SourcesConfig,
    pub groups: Vec<GroupConfig>,
    pub file_types: Vec<FileTypeRule>,
    /// Quipu knowledge graph endpoint (e.g. "http://quipu.example").
    /// When set, search results are annotated with entity spotlight data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quipu_endpoint: Option<String>,
    /// Push the governed chunk graph on every index run as a diffed snapshot
    /// replacement. When `quipu_endpoint` is set this uses its authenticated
    /// `/knot` transport; otherwise it uses the embedded Quipu store.
    /// Opt-in: requires the `knowledge` feature and snapshot replacement.
    #[serde(default)]
    pub quipu_push_chunks: bool,
    /// Run the inferred-track extractor (W3.B) over markdown prose during
    /// indexing and land the candidates in the QUARANTINED camayoc
    /// crew:inferred plane (trust rank 0), as a diffed snapshot replacement.
    /// Opt-in: requires the `knowledge` feature, a quipu revision with strict
    /// /knot graph routing (>= 22b3569 — the push refuses on older stores
    /// rather than letting facts masquerade in ROOT), and the plane
    /// registered + trust-labelled by camayoc's `planes.py ensure`. Like
    /// `quipu_push_chunks`, the snapshot reflects the files indexed this run.
    #[serde(default)]
    pub quipu_push_inferred: bool,
}

/// Configuration for remote server (thin-client mode)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ServerConfig {
    /// Remote bobbin HTTP server URL (e.g. "http://search.example")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Bind address for `bobbin serve --http` (default: "0.0.0.0").
    /// Use "127.0.0.1" to restrict to localhost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,
    /// Filesystem prefix for indexed repos on the server.
    /// Used to normalize absolute paths in search results back to
    /// repo-relative paths (e.g., "/var/lib/bobbin/repos/").
    /// If unset, absolute paths are returned as-is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_path_prefix: Option<String>,
}

/// Global configuration loaded from ~/.config/bobbin/config.toml.
///
/// Uses the same Config struct — supports all config options.
/// This provides defaults for all bobbin invocations on this machine.
/// Per-repo config and CLI flags take precedence.
pub type GlobalConfig = Config;

/// Configuration for indexing behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    /// Glob patterns to include
    pub include: Vec<String>,
    /// Glob patterns to exclude (in addition to .gitignore)
    pub exclude: Vec<String>,
    /// Whether to respect .gitignore
    pub use_gitignore: bool,
    /// Lines per chunk for the line-based (unknown-language) chunker. Default: 50.
    pub chunk_size: usize,
    /// Overlapping lines between consecutive line-based chunks. Default: 10.
    /// Must be smaller than `chunk_size`.
    pub chunk_overlap: usize,
    /// Enable multimodal ingest (currently PDF text extraction). When true, the
    /// indexer also walks `**/*.pdf`, extracts their text via a pure-Rust
    /// extractor, and chunks it like a plain-text document (language = "pdf").
    /// Default: false — opt-in so the default behavior/dep profile is unchanged.
    pub multimodal: bool,
    /// Enable the generic documents source (currently HTML text extraction).
    /// When true, the indexer also walks `**/*.html` / `**/*.htm`, converts
    /// them to markdown-ish text with a deterministic dependency-free tag
    /// stripper, and chunks the result with the markdown chunker (language =
    /// "html"). Default: false — opt-in, mirroring `multimodal`.
    pub documents: bool,
    /// Derive semantic entities (CodeModule/CodeSymbol/Document/Section) from
    /// parsed chunks into the local entities table on every index run —
    /// deterministic extraction, live-lane IRIs. Opt-in: each entity's short
    /// text is embedded, adding a second (small) embed pass per run.
    pub entities: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            include: vec![
                "**/*.rs".into(),
                "**/*.ts".into(),
                "**/*.tsx".into(),
                "**/*.js".into(),
                "**/*.jsx".into(),
                "**/*.py".into(),
                "**/*.go".into(),
                // Java + C++ have wired tree-sitter parsers; include the
                // extensions parser.rs recognizes (.c/.h are line-based "c" and
                // intentionally left out — bo-esn4).
                "**/*.java".into(),
                "**/*.cpp".into(),
                "**/*.cc".into(),
                "**/*.hpp".into(),
                // Infrastructure-as-code, config and scripts. No tree-sitter parser
                // is wired for these, so they fall through to line-based chunking
                // (like markdown) and are searchable via embeddings. Without them an
                // ansible/IaC repo is invisible to code search — its roles,
                // templates, playbooks and inventory are all yaml/jinja (bobbin-ywzq8).
                "**/*.yml".into(),
                "**/*.yaml".into(),
                "**/*.j2".into(),
                "**/*.tf".into(),
                "**/*.sh".into(),
                "**/*.md".into(),
            ],
            exclude: vec![
                "**/node_modules/**".into(),
                "**/target/**".into(),
                "**/dist/**".into(),
                "**/.git/**".into(),
                "**/build/**".into(),
                "**/__pycache__/**".into(),
                "**/CONTRIBUTING.md".into(),
                "**/contributing.md".into(),
                "**/searchindex*.js".into(),
                "**/*.min.js".into(),
                "**/*.min.css".into(),
                "**/.scratch/**".into(),
                "**/vendor/**".into(),
                "**/.venv/**".into(),
                "**/book/book/**".into(),
            ],
            use_gitignore: true,
            chunk_size: 50,
            chunk_overlap: 10,
            multimodal: false,
            documents: false,
            entities: false,
        }
    }
}

/// Configuration for embedding generation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// Embedding backend: "onnx" (default) or "openai-api"
    pub backend: EmbeddingBackend,
    /// Model name (built-in ONNX model name or API model identifier)
    pub model: String,
    /// Batch size for embedding generation
    pub batch_size: usize,
    /// Embedding dimensions (auto-detected for built-in ONNX models, required for custom/API)
    pub dimensions: Option<usize>,
    /// Enable GPU acceleration for ONNX inference (CUDA).
    /// Also controllable via BOBBIN_GPU=1 env var.
    pub gpu: bool,
    /// OpenAI-compatible API settings (required when backend = "openai-api")
    pub api: Option<ApiEmbeddingConfig>,
    /// Custom local ONNX model settings (optional, for non-built-in models)
    pub custom_model: Option<CustomModelConfig>,
    /// Contextual embedding settings
    pub context: ContextualEmbeddingConfig,
}

/// Embedding backend type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingBackend {
    /// Local ONNX runtime inference
    Onnx,
    /// OpenAI-compatible embedding API (works with Ollama, vLLM, LiteLLM, etc.)
    OpenaiApi,
}

impl Default for EmbeddingBackend {
    fn default() -> Self {
        Self::Onnx
    }
}

/// Configuration for OpenAI-compatible embedding API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEmbeddingConfig {
    /// API endpoint URL (e.g. "http://localhost:11434/v1/embeddings")
    pub url: String,
    /// API key — literal value or "env:VAR_NAME" to read from environment
    pub api_key: Option<String>,
}

impl ApiEmbeddingConfig {
    /// Resolve the API key, supporting "env:VAR_NAME" syntax
    pub fn resolve_api_key(&self) -> Option<String> {
        self.api_key.as_ref().and_then(|key| {
            if let Some(var_name) = key.strip_prefix("env:") {
                std::env::var(var_name).ok()
            } else if key.is_empty() {
                None
            } else {
                Some(key.clone())
            }
        })
    }
}

/// Configuration for custom local ONNX models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomModelConfig {
    /// Path to ONNX model file
    pub model_path: String,
    /// Path to tokenizer.json file
    pub tokenizer_path: String,
    /// Maximum sequence length (default: 512)
    pub max_seq_len: Option<usize>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            backend: EmbeddingBackend::default(),
            model: "all-MiniLM-L6-v2".into(),
            batch_size: 32,
            dimensions: None,
            gpu: false,
            api: None,
            custom_model: None,
            context: ContextualEmbeddingConfig::default(),
        }
    }
}

// GPU resolution for the embedding backend — split out under the
// file-size ratchet; the inherent impl lives in `config_gpu.rs`.
#[path = "config_gpu.rs"]
mod gpu;

/// Configuration for contextual embeddings
///
/// When enabled for a language, chunks are embedded with surrounding context
/// (N lines before/after) for better retrieval quality. The original chunk
/// content is stored for display; the enriched text is stored in full_context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextualEmbeddingConfig {
    /// Number of context lines to include before and after a chunk
    pub context_lines: usize,
    /// Languages where contextual embedding is enabled
    pub enabled_languages: Vec<String>,
}

impl Default for ContextualEmbeddingConfig {
    fn default() -> Self {
        Self {
            context_lines: 5,
            enabled_languages: vec!["markdown".into()],
        }
    }
}

/// Configuration for search behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Default number of results to return
    pub default_limit: usize,
    /// Weight for semantic vs keyword search (0.0 = keyword only, 1.0 = semantic only)
    pub semantic_weight: f32,
    /// Half-life for recency decay in days. After this many days, a result's
    /// recency boost drops to 50%. Set to 0.0 to disable recency boosting.
    pub recency_half_life_days: f32,
    /// How much recency affects final score (0.0 = no effect, 1.0 = full effect).
    /// The final score is: `score * (1.0 - weight + weight * decay)` so at
    /// max weight=1.0, a very old result loses up to 100% of its score.
    /// At default 0.3, the maximum penalty for old results is 30%.
    pub recency_weight: f32,
    /// RRF (Reciprocal Rank Fusion) constant `k`. Controls how quickly scores
    /// decay with rank. Lower values make top ranks more dominant; higher values
    /// flatten the distribution. Default: 60.0 (standard RRF).
    pub rrf_k: f32,
    /// Demotion factor for Documentation/Config files in search ranking.
    /// Applied as a multiplier to RRF scores: 1.0 = no demotion, 0.0 = full demotion.
    /// Source/Test files are unaffected. Default: 0.3 (demote doc/config to 30%).
    /// This is the single source of truth — `ContextConfig::doc_demotion` defaults
    /// to the same value and every assembly path overrides it from here.
    pub doc_demotion: f32,
    /// Personalized PageRank ranking weight (0.0 = disabled). Requires the
    /// `knowledge` feature + a populated Quipu coupling graph. Bounded boost:
    /// `score *= 1.0 + ppr_weight * ppr_score`. The DEFAULT is feature-gated: a
    /// provisional 0.3 on `knowledge` builds (so the opt-in integration actually
    /// re-ranks), and 0.0 without the feature — a non-zero weight bail!s in a
    /// non-knowledge binary. The 0.3 is to be tuned by the GH#56 calibration sweep.
    #[serde(default)]
    pub ppr_weight: f32,
    /// Optional cross-encoder reranking stage ([search.reranker]). Absent =
    /// off. See `search::rerank::RerankerConfig` for the knobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reranker: Option<crate::search::rerank::RerankerConfig>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_limit: 10,
            semantic_weight: 0.9,
            recency_half_life_days: 30.0,
            recency_weight: 0.3,
            rrf_k: 60.0,
            doc_demotion: 0.3,
            // Feature-gated: knowledge builds default to a provisional non-zero PPR
            // weight so the opt-in integration re-ranks; non-knowledge builds keep
            // 0.0 (a non-zero weight bail!s without the feature). Tune from GH#56.
            ppr_weight: if cfg!(feature = "knowledge") {
                0.3
            } else {
                0.0
            },
            reranker: None,
        }
    }
}

/// Configuration for dependency analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DependencyConfig {
    /// Enable dependency extraction and storage
    pub enabled: bool,
    /// Enable import path resolution
    pub resolve_imports: bool,
}

impl Default for DependencyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            resolve_imports: true,
        }
    }
}

/// Configuration for git analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitConfig {
    /// Enable temporal coupling analysis
    pub coupling_enabled: bool,
    /// How many commits back to analyze
    pub coupling_depth: usize,
    /// Minimum co-changes to establish coupling
    pub coupling_threshold: u32,
    /// Weight on co-change frequency in the coupling score (0.0–1.0). The
    /// recency component is weighted `1.0 - coupling_freq_weight`. Default: 0.7
    /// (70% frequency, 30% recency).
    pub coupling_freq_weight: f32,
    /// Recency knee in days for coupling score decay: a pair last changed
    /// together `coupling_recency_days` ago scores 0.5 on the recency component
    /// (`1/(1 + days/coupling_recency_days)`). Default: 30.0.
    pub coupling_recency_days: f32,
    /// Enable semantic commit indexing (embed commit messages for search)
    pub commits_enabled: bool,
    /// How many commits back to index for semantic search (0 = all)
    pub commits_depth: usize,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            coupling_enabled: true,
            coupling_depth: 5000,
            coupling_threshold: 3,
            coupling_freq_weight: 0.7,
            coupling_recency_days: 30.0,
            commits_enabled: true,
            commits_depth: 0,
        }
    }
}

/// Tuning knobs for context assembly — the bridging + knowledge-expansion
/// pipeline that is Bobbin's core differentiator. These were previously
/// hardcoded (and inconsistent) at each context call site; surfacing them in
/// `[context]` lets operators tune behavior without recompiling. Defaults match
/// the tuned production values used by the hook + HTTP injection paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextTuning {
    /// Score multiplier for bridge-boosted files: `final_score *= (1.0 + factor)`.
    /// Only used in Boost/BoostInject bridge modes. Default: 0.3.
    pub bridge_boost_factor: f32,
    /// Maximum number of bridged files to fetch chunks for. Default: 2.
    /// Prevents bridge explosion from doc→source blame chains.
    pub max_bridged_files: usize,
    /// Maximum chunks per bridged file (first N by start_line). Default: 1.
    pub max_bridged_chunks_per_file: usize,
    /// Minimum temporal-coupling score (float, 0.0–1.0) for a coupled file to be
    /// included in context. DISTINCT from `git.coupling_threshold` (an integer
    /// minimum co-change *count* applied during indexing). Default: 0.1.
    pub coupling_threshold: f32,
    /// Percentage of the line budget reserved for knowledge-graph expansion
    /// (requires the `knowledge` feature). Default: 15.0.
    pub knowledge_budget_pct: f32,
    /// Maximum graph-traversal hops for knowledge expansion. Default: 2.
    pub knowledge_max_hops: u32,
    /// Percentage of the line budget reserved for structural neighbors of
    /// documentation hits (parent section + adjacent chunks via the
    /// next_chunk/part_of edge graph). 0 disables the leg. Default: 10.0.
    pub neighbor_budget_pct: f32,
    /// Unit the context budget is enforced in: `"line"` (count source lines) or
    /// `"token"` (estimate tokens per chunk, ~chars/4). Token mode makes injection
    /// size predictable against the model window. Default: `"line"`.
    pub budget_unit: crate::search::context::BudgetUnit,
}

impl Default for ContextTuning {
    fn default() -> Self {
        Self {
            bridge_boost_factor: 0.3,
            max_bridged_files: 2,
            max_bridged_chunks_per_file: 1,
            coupling_threshold: 0.1,
            knowledge_budget_pct: 15.0,
            knowledge_max_hops: 2,
            neighbor_budget_pct: 10.0,
            budget_unit: crate::search::context::BudgetUnit::Line,
        }
    }
}

/// Tuning knobs for cross-agent feedback boosting. Files rated "useful" by other
/// agents for similar queries get a bounded score boost during context assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedbackTuning {
    /// Maximum feedback boost multiplier. Actual boost is
    /// `min(score * boost_weight, boost_max)`. Default: 0.3 (30% max).
    pub boost_max: f32,
    /// Weight multiplier applied to raw feedback scores. Default: 0.2.
    pub boost_weight: f32,
}

impl Default for FeedbackTuning {
    fn default() -> Self {
        Self {
            boost_max: 0.3,
            boost_weight: 0.2,
        }
    }
}

#[path = "config_hooks.rs"]
mod hooks;
pub use hooks::{HooksConfig, KeywordRepoRule, VALID_FORMAT_MODES};

/// Which store bead content is read from.
///
/// `Dolt` is the original MySQL-protocol backend. `Jsonl` reads the br store's
/// exported `issues.jsonl`, which is what the crew store became at the clbx2
/// cutover (2026-08-29) — see aegis-205llh, where bobbin was found still
/// answering bead search out of the frozen Dolt snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BeadsSource {
    /// Dolt / MySQL protocol (`host`, `port`, `user`, `databases`).
    #[default]
    Dolt,
    /// br JSONL export, one path per configured database (`jsonl_paths`).
    Jsonl,
}

/// Configuration for beads (issue tracker) integration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BeadsConfig {
    /// Enable beads indexing
    pub enabled: bool,
    /// Which store to read beads from (default: `dolt`)
    pub source: BeadsSource,
    /// Dolt server hostname
    pub host: String,
    /// Dolt server port
    pub port: u16,
    /// Dolt user
    pub user: String,
    /// Database names to index (e.g., ["beads_aegis", "beads_gastown"]).
    ///
    /// This is the rig identity for BOTH sources, not a Dolt-only field: chunk
    /// keys are `beads:<rig>:<id>` with `rig` derived from these names, so the
    /// list must stay the same across a source switch or the whole corpus
    /// re-keys and the removal sweep deletes the old rows.
    pub databases: Vec<String>,
    /// For `source = "jsonl"`: database name -> path of that rig's
    /// `issues.jsonl`. Every entry in `databases` must have one; a missing
    /// path is an error rather than an empty rig, because "indexed zero beads"
    /// and "this rig is not configured" are indistinguishable downstream.
    pub jsonl_paths: std::collections::BTreeMap<String, String>,
    /// Include comments in indexed content
    pub include_comments: bool,
    /// Include closed beads
    pub include_closed: bool,
    /// Skip beads older than this many days (0 = no limit)
    pub max_age_days: u32,
    /// Exclude beads carrying any of these labels (e.g. ["security", "escalation"]).
    /// Defense-in-depth: keeps sensitive beads out of the index entirely, so they
    /// can never surface regardless of access rules. Case-insensitive.
    pub exclude_labels: Vec<String>,
}

impl Default for BeadsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            source: BeadsSource::default(),
            host: "localhost".into(),
            port: 3306,
            user: "root".into(),
            databases: vec![],
            jsonl_paths: std::collections::BTreeMap::new(),
            include_comments: true,
            include_closed: false,
            max_age_days: 90,
            exclude_labels: vec![],
        }
    }
}

/// Configuration for archive indexing (human directives, agent memories, etc.)
///
/// Archives are directories of markdown files with YAML frontmatter.
/// Each source is identified by a schema string in the frontmatter and
/// tagged with a configurable label for filtering in search results.
///
/// Example config:
/// ```toml
/// [archive]
/// enabled = true
///
/// [[archive.sources]]
/// name = "hla"
/// path = "/mnt/hla/records"
/// schema = "human-intent"
/// name_field = "channel"
///
/// [[archive.sources]]
/// name = "pensieve"
/// path = "/mnt/pensieve/records"
/// schema = "agent-memory"
/// name_field = "agent"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveConfig {
    /// Enable archive indexing
    pub enabled: bool,
    /// Archive sources to index
    pub sources: Vec<ArchiveSource>,
    /// HMAC-SHA256 secret for push notifications (empty = reject all webhooks)
    pub webhook_secret: String,
}

/// A single archive source definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSource {
    /// Label for this source (used as language tag in chunks, and as path prefix).
    /// Also used as the `source` filter value in search queries.
    pub name: String,
    /// Filesystem path to the archive records directory.
    pub path: String,
    /// String to match in YAML frontmatter to identify records from this source.
    /// E.g., "human-intent" matches `schema: human-intent/v2`.
    pub schema: String,
    /// Frontmatter field to use as the name prefix in chunk names.
    /// E.g., "channel" → chunk name becomes "{channel_value}/{record_id}".
    /// If empty, chunk name is just the record ID.
    #[serde(default)]
    pub name_field: String,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sources: vec![],
            webhook_secret: String::new(),
        }
    }
}

/// Configuration for SQL database sources (roadmap W4.P2).
///
/// Each source runs a configured query against a MySQL-protocol database
/// and indexes every row as one searchable chunk with a stable identity.
/// Credentials never live in config: `url_env` names an environment
/// variable holding the connection URL.
///
/// Example config:
/// ```toml
/// [sql]
/// enabled = true
///
/// [[sql.sources]]
/// name = "tickets"
/// url_env = "BOBBIN_SQL_TICKETS_URL"
/// query = "SELECT id, title, body, status FROM tickets"
/// id_column = "id"
/// text_columns = ["title", "body"]
/// tag_columns = ["status"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SqlConfig {
    /// Enable SQL source indexing
    pub enabled: bool,
    /// SQL sources to index
    pub sources: Vec<SqlSourceConfig>,
}

/// A single SQL source definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlSourceConfig {
    /// Label for this source; rows index under repo key `sql-{name}` and
    /// chunk paths `sql:{name}:{id}`.
    pub name: String,
    /// Environment variable holding the `mysql://user:pass@host:port/db`
    /// connection URL. Never put credentials in this file.
    pub url_env: String,
    /// Query producing the rows to index. Must include `id_column`.
    pub query: String,
    /// Column holding each row's stable primary key.
    pub id_column: String,
    /// Columns whose values go into the embedded content. Empty = all.
    #[serde(default)]
    pub text_columns: Vec<String>,
    /// Columns rendered as `column:value` tags for search filtering.
    #[serde(default)]
    pub tag_columns: Vec<String>,
}

/// Configuration for role-based repository access filtering (§69)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessConfig {
    /// When true, repos not listed in any rule are visible to all roles.
    /// When false, repos must be explicitly granted.
    pub default_allow: bool,
    /// Role definitions with allow/deny lists
    pub roles: Vec<RoleConfig>,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            default_allow: true,
            roles: vec![],
        }
    }
}

/// A single role definition with repo allow/deny lists
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleConfig {
    /// Role name or pattern (e.g. "human", "aegis/*", "bobbin/polecats/*")
    pub name: String,
    /// Repos this role can see (glob patterns, e.g. ["*"], ["aegis", "bobbin"])
    #[serde(default)]
    pub allow: Vec<String>,
    /// Repos this role cannot see (glob patterns, deny takes precedence over allow)
    #[serde(default)]
    pub deny: Vec<String>,
    /// File path patterns to deny within allowed repos (glob patterns)
    #[serde(default)]
    pub deny_paths: Vec<String>,
}

/// Maps indexed repo short names to their web browse URLs.
///
/// Example config:
/// ```toml
/// [sources]
/// # Override template for ALL auto-detected git remotes (optional).
/// # If empty, bobbin auto-detects the forge type (GitHub/GitLab/Forgejo/Bitbucket)
/// # and applies the correct URL pattern automatically.
/// # remote_template = "{remote_base}/src/branch/main/{path}#L{line}"
///
/// # Fallback template for repos with no git remote (uses {repo}, {path}, {line})
/// default_url = ""
///
/// # Per-repo overrides (full URL templates, highest priority)
/// [sources.repos]
/// beads = "https://github.com/scbrown/beads/blob/main/{path}#L{line}"
///
/// # Override forge detection for specific hosts
/// [sources.forge_overrides]
/// "git.internal.example.com" = "gitlab"   # github, gitlab, forgejo, bitbucket
/// ```
///
/// URL templates support `{repo}`, `{path}`, `{line}`, and `{remote_base}` placeholders.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SourcesConfig {
    /// Template applied to auto-detected git remotes.
    /// If empty, forge type is auto-detected and the correct template applied.
    /// Placeholders: {remote_base} (web base URL), {path}, {line}
    #[serde(default)]
    pub remote_template: String,
    /// Default URL template for repos not explicitly listed and without a git remote.
    /// Placeholders: {repo}, {path}, {line}
    pub default_url: String,
    /// Per-repo URL overrides. Key = repo short name, value = URL template.
    /// These take priority over auto-detection.
    pub repos: std::collections::HashMap<String, String>,
    /// Override forge detection for specific hosts.
    /// Key = hostname, value = forge type ("github", "gitlab", "forgejo", "bitbucket").
    #[serde(default)]
    pub forge_overrides: std::collections::HashMap<String, String>,
}

impl Default for SourcesConfig {
    fn default() -> Self {
        Self {
            remote_template: String::new(),
            default_url: String::new(),
            repos: std::collections::HashMap::new(),
            forge_overrides: std::collections::HashMap::new(),
        }
    }
}

/// Named repo group for scoped search.
///
/// Groups define named sets of repositories that can be used to narrow
/// search scope via `--group` (CLI) or `?group=` (HTTP). Groups compose
/// with role-based access filtering — a group can only include repos the
/// caller is allowed to see.
///
/// Example config:
/// ```toml
/// [[groups]]
/// name = "infra"
/// repos = ["goldblum", "homelab-mcp", "aegis"]
///
/// [[groups]]
/// name = "apps"
/// repos = ["reckoning", "tapestry", "shanty"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupConfig {
    /// Group name (used in --group flag and ?group= param)
    pub name: String,
    /// Repository names in this group
    pub repos: Vec<String>,
}

/// Configurable file type classification rule.
///
/// Maps glob patterns to a file category name. Rules are evaluated in order;
/// first match wins. Files not matched by any rule fall through to the
/// built-in `classify_file()` heuristics.
///
/// Example config:
/// ```toml
/// [[file_types]]
/// name = "config"
/// patterns = ["*.toml", "*.yaml", "*.yml", ".env*", "Dockerfile*"]
///
/// [[file_types]]
/// name = "test"
/// patterns = ["tests/**", "*_test.go", "test_*.py"]
///
/// [[file_types]]
/// name = "documentation"
/// patterns = ["docs/**/*.md", "README*", "CHANGELOG*"]
///
/// [[file_types]]
/// name = "generated"
/// patterns = ["*.pb.go", "*.generated.ts", "migrations/*.sql"]
/// ```
///
/// Built-in category names: "source", "test", "documentation", "config".
/// Custom names (e.g., "generated", "vendor", "schema") are also supported
/// and will be stored/displayed as-is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTypeRule {
    /// Category name (e.g., "source", "test", "documentation", "config", or custom)
    pub name: String,
    /// Glob patterns that match this category (matched against repo-relative paths)
    pub patterns: Vec<String>,
}

impl Config {
    /// Load configuration from a TOML file
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))
    }

    /// Look up a group by name. Returns the repo list, or None if not found.
    pub fn resolve_group(&self, name: &str) -> Option<&[String]> {
        self.groups
            .iter()
            .find(|g| g.name == name)
            .map(|g| g.repos.as_slice())
    }

    /// Build a SQL filter clause for a group's repos: `repo IN ('a', 'b', 'c')`.
    /// Returns None if group not found or empty.
    pub fn group_filter(&self, name: &str) -> Option<String> {
        self.resolve_group(name).and_then(|repos| {
            if repos.is_empty() {
                return None;
            }
            let escaped: Vec<String> = repos
                .iter()
                .map(|r| format!("'{}'", r.replace('\'', "''")))
                .collect();
            Some(format!("repo IN ({})", escaped.join(", ")))
        })
    }

    /// Save configuration to a TOML file
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }
        std::fs::write(path, content)
            .with_context(|| format!("Failed to write config file: {}", path.display()))
    }

    /// Get the path to the bobbin data directory for a repository
    pub fn data_dir(repo_root: &Path) -> PathBuf {
        repo_root.join(".bobbin")
    }

    /// Get the config file path for a repository
    pub fn config_path(repo_root: &Path) -> PathBuf {
        Self::data_dir(repo_root).join("config.toml")
    }

    /// Get the SQLite database path
    pub fn db_path(repo_root: &Path) -> PathBuf {
        Self::data_dir(repo_root).join("index.db")
    }

    /// Get the feedback SQLite database path
    pub fn feedback_db_path(repo_root: &Path) -> PathBuf {
        Self::data_dir(repo_root).join("feedback.db")
    }

    /// Get the LanceDB directory path
    pub fn lance_path(repo_root: &Path) -> PathBuf {
        Self::data_dir(repo_root).join("vectors")
    }

    /// Get the global model cache directory
    pub fn model_cache_dir() -> Result<PathBuf> {
        let project_dirs = directories::ProjectDirs::from("dev", "bobbin", "bobbin")
            .context("Failed to determine user directories")?;
        Ok(project_dirs.cache_dir().join("models"))
    }

    /// Get the global config directory (~/.config/bobbin/)
    pub fn global_config_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "bobbin", "bobbin")
            .map(|dirs| dirs.config_dir().to_path_buf())
    }

    /// Get the global config file path (~/.config/bobbin/config.toml)
    pub fn global_config_path() -> Option<PathBuf> {
        Self::global_config_dir().map(|dir| dir.join("config.toml"))
    }
}

// --- Global config helpers (on Config, used via GlobalConfig alias) ---

impl Config {
    /// Load global config from ~/.config/bobbin/config.toml.
    /// Returns default (empty) config if file doesn't exist.
    pub fn load_global() -> Config {
        Self::global_config_path()
            .and_then(|path| {
                if path.exists() {
                    std::fs::read_to_string(&path).ok()
                } else {
                    None
                }
            })
            .and_then(|content| toml::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// Save this config as the global config at ~/.config/bobbin/config.toml.
    pub fn save_global(&self) -> Result<()> {
        let path = Self::global_config_path().context("Failed to determine global config path")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create global config directory: {}",
                    parent.display()
                )
            })?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize global config")?;
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write global config: {}", path.display()))
    }

    /// Load config with full hierarchy: global defaults < per-repo config.
    ///
    /// Resolution order (last wins):
    /// 1. Compiled defaults (`Config::default()`)
    /// 2. Global config (`~/.config/bobbin/config.toml`)
    /// 3. Per-repo config (`.bobbin/config.toml`)
    ///
    /// Per-repo config is loaded as a TOML overlay — only fields explicitly
    /// set in the file override the global values. Fields absent from the
    /// per-repo file retain their global (or default) values.
    pub fn load_merged(repo_root: &Path) -> Result<Self> {
        // Start with global config (includes compiled defaults for missing fields)
        let global = Self::load_global();

        // Overlay per-repo config
        let repo_config_path = Self::config_path(repo_root);
        if repo_config_path.exists() {
            let repo_toml = std::fs::read_to_string(&repo_config_path)
                .with_context(|| format!("Failed to read {}", repo_config_path.display()))?;
            // Parse the raw TOML to see which keys are explicitly set
            let repo_table: toml::Value = toml::from_str(&repo_toml)
                .with_context(|| format!("Failed to parse {}", repo_config_path.display()))?;
            // Serialize global config to TOML value, then deep-merge repo on top
            let global_table = toml::Value::try_from(&global)
                .context("Failed to serialize global config for merge")?;
            let merged = deep_merge_toml(global_table, repo_table);
            merged
                .try_into()
                .context("Failed to deserialize merged config")
        } else {
            Ok(global)
        }
    }
}

/// Deep-merge two TOML values. `overlay` keys override `base` keys.
/// Tables are merged recursively; all other types are replaced wholesale.
fn deep_merge_toml(base: toml::Value, overlay: toml::Value) -> toml::Value {
    match (base, overlay) {
        (toml::Value::Table(mut base_map), toml::Value::Table(overlay_map)) => {
            for (key, overlay_val) in overlay_map {
                let merged = if let Some(base_val) = base_map.remove(&key) {
                    deep_merge_toml(base_val, overlay_val)
                } else {
                    overlay_val
                };
                base_map.insert(key, merged);
            }
            toml::Value::Table(base_map)
        }
        // Non-table overlay replaces base entirely
        (_base, overlay) => overlay,
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
