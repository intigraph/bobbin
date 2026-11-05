#[path = "context_capture.rs"]
pub mod capture;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::index::git::GitAnalyzer;
use crate::index::Embedder;
use crate::search::hybrid::apply_recency_boost;
use crate::storage::{MetadataStore, VectorStore};
use crate::types::{classify_file_with_rules, Chunk, ChunkType, FileCategory, MatchType};

/// How bridging (doc→source, commit→source) affects search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeMode {
    /// No bridging at all (baseline)
    Off,
    /// Add discovered files as Bridged results (current behavior)
    Inject,
    /// Boost RRF scores of files already in search results
    Boost,
    /// Boost existing + inject undiscovered files
    BoostInject,
}

impl Default for BridgeMode {
    fn default() -> Self {
        BridgeMode::Inject
    }
}

impl std::fmt::Display for BridgeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeMode::Off => write!(f, "off"),
            BridgeMode::Inject => write!(f, "inject"),
            BridgeMode::Boost => write!(f, "boost"),
            BridgeMode::BoostInject => write!(f, "boost_inject"),
        }
    }
}

impl std::str::FromStr for BridgeMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "off" => Ok(BridgeMode::Off),
            "inject" => Ok(BridgeMode::Inject),
            "boost" => Ok(BridgeMode::Boost),
            "boost_inject" | "boost+inject" => Ok(BridgeMode::BoostInject),
            _ => anyhow::bail!(
                "Unknown bridge_mode '{}'. Use: off, inject, boost, boost_inject",
                s
            ),
        }
    }
}

/// Unit in which the context-assembly budget is counted. Agents have *token*
/// budgets, but a 50-line dense-code chunk costs far more tokens than 50 lines
/// of comments. `Token` makes injection size predictable against the model
/// window; `Line` preserves the historical line-count behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BudgetUnit {
    /// Count budget in source lines (`end_line - start_line + 1`). Historical default.
    #[default]
    Line,
    /// Count budget in estimated tokens (see [`estimate_tokens`]).
    Token,
}

impl std::fmt::Display for BudgetUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BudgetUnit::Line => write!(f, "line"),
            BudgetUnit::Token => write!(f, "token"),
        }
    }
}

impl std::str::FromStr for BudgetUnit {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "line" | "lines" => Ok(BudgetUnit::Line),
            "token" | "tokens" => Ok(BudgetUnit::Token),
            _ => anyhow::bail!("Unknown budget_unit '{}'. Use: line, token", s),
        }
    }
}

/// Estimate the token count of a text fragment.
///
/// Uses the standard `chars / 4` heuristic (≈4 chars per token for English +
/// code) rather than a real tokenizer — it needs no model files, is
/// deterministic, and is accurate enough for budget *accounting* (we only need
/// relative sizing to keep injection under the model window, not exact counts).
/// Non-empty input always costs at least 1 token.
pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    if chars == 0 {
        0
    } else {
        chars.div_ceil(4)
    }
}

/// Cost of a chunk against the assembly budget, in the configured unit.
/// `Line` uses the chunk's line span; `Token` estimates from its content.
fn chunk_cost(unit: BudgetUnit, content: &str, start_line: u32, end_line: u32) -> usize {
    match unit {
        BudgetUnit::Line => (end_line.saturating_sub(start_line) + 1) as usize,
        BudgetUnit::Token => estimate_tokens(content),
    }
}

/// Recover a repo-relative file path from a live knowledge-graph entity IRI.
///
/// Delegates to [`crate::iri::entity_iri_file_path`], which lives beside the
/// minters so the parser and the producers cannot drift apart. It had drifted:
/// this function used to strip `http://aegis.gastown.local/code/` and the
/// `{name}-L{line}` / `S{line}` suffixes — the SUPERSEDED lane (`iri.rs`
/// module docs), measured at 0 live instances and minted by nothing in this
/// repo — so knowledge expansion matched no entity at all while its own tests
/// passed over the dead lane. That is exactly the silent-miss class the doc
/// comment here already named; a round-trip test against the real constructors
/// (`iri::tests::parses_what_the_minters_mint`) is what now prevents a third
/// lane.
#[cfg_attr(not(feature = "knowledge"), allow(dead_code))]
fn file_path_from_entity_iri(iri: &str) -> Option<String> {
    crate::iri::entity_iri_file_path(iri)
}

/// Configuration for context assembly
pub struct ContextConfig {
    /// Opt-in diagnostic capture; never enabled by the production hook.
    pub capture_candidates: bool,
    pub budget_lines: usize,
    /// Unit the budget is enforced in: `Line` (count source lines) or `Token`
    /// (estimate tokens per chunk). When `Token`, `budget_lines` and all derived
    /// caps are interpreted as token counts. Default: `Line`.
    pub budget_unit: BudgetUnit,
    pub depth: u32,
    pub max_coupled: usize,
    pub coupling_threshold: f32,
    pub semantic_weight: f32,
    pub content_mode: ContentMode,
    pub search_limit: usize,
    /// Demotion factor for Documentation/Config files in search ranking.
    /// Applied as a multiplier to RRF scores: 1.0 = no demotion, 0.3 = 30% score.
    /// Source/Test files are unaffected. Default: 0.3 — kept in sync with
    /// `SearchConfig::doc_demotion`, the single source of truth.
    pub doc_demotion: f32,
    /// Half-life for recency decay in days (0.0 = disabled)
    pub recency_half_life_days: f32,
    /// Recency weight (0.0 = disabled, 0.3 = default)
    pub recency_weight: f32,
    /// RRF constant k. Default: 60.0.
    pub rrf_k: f32,
    /// Bridging strategy: off, inject, boost, or boost_inject. Default: inject.
    pub bridge_mode: BridgeMode,
    /// Score multiplier for bridge-boosted files: final_score *= (1.0 + factor).
    /// Only used in Boost and BoostInject modes. Default: 0.3.
    pub bridge_boost_factor: f32,
    /// Additional SQL WHERE filter (e.g. tag include/exclude clauses).
    /// Combined with repo filter via AND.
    pub extra_filter: Option<String>,
    /// Tags configuration for effect-based scoring (replaces doc_demotion when present).
    pub tags_config: Option<crate::tags::TagsConfig>,
    /// Role for resolving scoped tag effects (e.g. "aegis/crew/sentinel").
    pub role: Option<String>,
    /// Configurable file type classification rules (from `[[file_types]]` config).
    /// First match wins; unmatched files fall through to built-in heuristics.
    pub file_type_rules: Vec<crate::config::FileTypeRule>,
    /// Repo name to boost in scoring (soft affinity from agent's cwd).
    /// Results from this repo get score multiplied by `repo_affinity_boost`.
    pub repo_affinity: Option<String>,
    /// Score multiplier for files matching `repo_affinity`. Default: 2.0.
    pub repo_affinity_boost: f32,
    /// Personalized PageRank ranking weight (0.0 = disabled). When > 0 and a
    /// Quipu store is wired in (`knowledge` feature), results get a bounded
    /// graph-connectivity boost seeded by the top hybrid hits:
    /// `score *= 1.0 + ppr_weight * ppr_score`.
    pub ppr_weight: f32,
    /// Maximum number of bridged files to fetch chunks for. Default: 3.
    /// Prevents bridge explosion from doc→source blame chains.
    pub max_bridged_files: usize,
    /// Maximum chunks per bridged file. Default: 2.
    /// Only the first N chunks (sorted by start_line) are kept per file.
    pub max_bridged_chunks_per_file: usize,
    /// Filesystem prefix for indexed repos on the server (e.g. "/var/lib/bobbin/repos/").
    /// Used to normalize absolute paths back to repo-relative for dedup.
    pub repo_path_prefix: Option<String>,
    /// Repo root, used to turn the absolute `file_path`s carried by search results
    /// back into the repo-relative form the Quipu coupling graph is keyed by.
    /// Required for PPR seeding: without it every seed IRI misses and PPR is a
    /// silent no-op. `git_analyzer` is not wired on the CLI path, so this cannot
    /// be derived from it here. See bobbin-jdlkh.
    pub repo_root: Option<std::path::PathBuf>,
    /// Cross-agent feedback scores: file_path → aggregate feedback score.
    /// Files rated "useful" by other agents for similar queries get boosted.
    /// Populated by FeedbackStore::file_feedback_scores() before assembly.
    pub feedback_scores: Option<HashMap<String, f32>>,
    /// Maximum feedback boost multiplier. The actual boost is
    /// `min(score * feedback_boost_weight, feedback_boost_max)`.
    /// Default: 0.3 (30% max boost from feedback).
    pub feedback_boost_max: f32,
    /// Weight multiplier for feedback scores. Default: 0.2.
    pub feedback_boost_weight: f32,
    /// Knowledge expansion configuration (requires `knowledge` feature).
    /// When set, the pipeline queries Quipu for related entities after bridging.
    pub knowledge_budget_pct: f32,
    /// Maximum graph traversal hops for knowledge expansion. Default: 2.
    pub knowledge_max_hops: u32,
    /// Percentage of the budget reserved for structural neighbors of
    /// documentation hits (parent section + adjacent chunks via
    /// next_chunk/part_of edges). 0 disables the leg. Default: 10.0.
    pub neighbor_budget_pct: f32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            capture_candidates: false,
            budget_lines: 500,
            budget_unit: BudgetUnit::Line,
            depth: 1,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.3,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            rrf_k: 60.0,
            bridge_mode: BridgeMode::default(),
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            repo_root: None,
            feedback_scores: None,
            feedback_boost_max: 0.3,
            feedback_boost_weight: 0.2,
            knowledge_budget_pct: 15.0,
            knowledge_max_hops: 2,
            neighbor_budget_pct: 10.0,
        }
    }
}

/// How much content to include in output
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentMode {
    /// Include full chunk content
    Full,
    /// Include first 3 lines + "..."
    Preview,
    /// No content, paths/metadata only
    None,
}

/// The assembled context bundle
#[derive(Debug, Serialize)]
pub struct ContextBundle {
    /// Private diagnostic state, excluded from the existing wire format.
    #[serde(skip)]
    pub capture: Option<capture::AssemblyCapture>,
    pub query: String,
    pub files: Vec<ContextFile>,
    pub budget: BudgetInfo,
    pub summary: ContextSummary,
}

/// A file included in the context bundle
#[derive(Debug, Serialize)]
pub struct ContextFile {
    pub path: String,
    pub language: String,
    pub relevance: FileRelevance,
    pub category: FileCategory,
    pub score: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub coupled_to: Vec<String>,
    pub chunks: Vec<ContextChunk>,
    /// Repository name this file belongs to (from index metadata)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// A chunk within a context file
#[derive(Debug, Clone, Serialize)]
pub struct ContextChunk {
    /// Stable chunk ID — lets an agent follow relationships from context
    /// output (e.g. via the chunk_neighbors MCP tool)
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub chunk_type: ChunkType,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_type: Option<MatchType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Budget information for the context bundle
#[derive(Debug, Serialize)]
pub struct BudgetInfo {
    pub max_lines: usize,
    pub used_lines: usize,
    /// Lines used by pinned chunks (subset of used_lines)
    #[serde(skip_serializing_if = "is_zero")]
    pub pinned_lines: usize,
}

fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Summary statistics for the context bundle
#[derive(Debug, Serialize)]
pub struct ContextSummary {
    pub total_files: usize,
    pub total_chunks: usize,
    pub direct_hits: usize,
    pub coupled_additions: usize,
    pub bridged_additions: usize,
    pub source_files: usize,
    pub doc_files: usize,
    /// Raw cosine similarity of the top semantic search result (before RRF normalization).
    /// Used by the gate_threshold check to decide whether to inject context at all.
    pub top_semantic_score: f32,
    /// Number of pinned chunks injected
    #[serde(skip_serializing_if = "is_zero")]
    pub pinned_chunks: usize,
    /// Number of knowledge graph chunks injected
    #[serde(skip_serializing_if = "is_zero")]
    pub knowledge_additions: usize,
    /// Number of structural-neighbor chunks injected (chunk-edge expansion)
    #[serde(skip_serializing_if = "is_zero")]
    pub structural_additions: usize,
}

/// How a file was found
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRelevance {
    Direct,
    Coupled,
    /// Found via git blame provenance bridging from a documentation chunk
    Bridged,
    /// Pinned via tag effect — bypasses relevance threshold
    Pinned,
    /// Found via Quipu knowledge graph expansion
    Knowledge,
    /// Structural neighbor of a documentation hit — parent section or
    /// adjacent chunk via the next_chunk/part_of edge graph
    Structural,
}

/// How a seed chunk was discovered
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SeedSource {
    /// Found via hybrid search (semantic + keyword)
    Search { match_type: MatchType },
    /// Found via git diff overlap
    Diff {
        status: String,
        added_lines: usize,
        removed_lines: usize,
    },
}

/// An externally-provided seed for context assembly.
///
/// Wraps a `Chunk` with a relevance score and provenance information.
/// Both search-based and diff-based workflows produce `SeedChunk`s
/// that feed into `ContextAssembler::assemble_from_seeds()`.
#[derive(Debug, Clone, Serialize)]
pub struct SeedChunk {
    pub chunk: Chunk,
    pub score: f32,
    pub source: SeedSource,
    /// Repository name this chunk belongs to (from index metadata)
    pub repo: Option<String>,
}

/// Assembles task-relevant context from search and git history
pub struct ContextAssembler {
    embedder: Embedder,
    vector_store: VectorStore,
    metadata_store: MetadataStore,
    config: ContextConfig,
    git_analyzer: Option<GitAnalyzer>,
    #[cfg(feature = "knowledge")]
    quipu_store: Option<quipu::Store>,
}

/// Internal struct for seed search results
struct SeedResult {
    chunk_id: String,
    file_path: String,
    language: String,
    name: Option<String>,
    chunk_type: ChunkType,
    start_line: u32,
    end_line: u32,
    content: String,
    score: f32,
    match_type: Option<MatchType>,
    indexed_at: Option<i64>,
    repo: Option<String>,
    tags: String,
    /// Whether this chunk has a pin tag effect (bypasses threshold, injected first)
    is_pinned: bool,
}

/// Internal struct for coupled chunk information
struct CoupledChunkInfo {
    chunk_id: String,
    file_path: String,
    language: String,
    name: Option<String>,
    chunk_type: ChunkType,
    start_line: u32,
    end_line: u32,
    content: String,
    coupling_score: f32,
    coupled_to: String,
}

/// Internal struct for structural-neighbor expansion results (chunk edges)
struct NeighborChunkInfo {
    chunk_id: String,
    file_path: String,
    language: String,
    name: Option<String>,
    chunk_type: ChunkType,
    start_line: u32,
    end_line: u32,
    content: String,
    /// Derived from the anchoring seed's score
    neighbor_score: f32,
    /// Human-readable provenance: relation + the anchor chunk's name
    discovered_via: String,
}

/// Internal struct for knowledge graph expansion results
struct KnowledgeChunkInfo {
    chunk_id: String,
    file_path: String,
    language: String,
    name: Option<String>,
    chunk_type: ChunkType,
    start_line: u32,
    end_line: u32,
    content: String,
    /// Relevance score based on graph distance and embedding similarity
    knowledge_score: f32,
    /// Entity IRI that led to discovering this chunk
    discovered_via: String,
}

impl ContextAssembler {
    pub fn new(
        embedder: Embedder,
        vector_store: VectorStore,
        metadata_store: MetadataStore,
        config: ContextConfig,
    ) -> Self {
        Self {
            embedder,
            vector_store,
            metadata_store,
            config,
            git_analyzer: None,
            #[cfg(feature = "knowledge")]
            quipu_store: None,
        }
    }

    /// Set the git analyzer for doc→source provenance bridging
    pub fn with_git_analyzer(mut self, git_analyzer: GitAnalyzer) -> Self {
        self.git_analyzer = Some(git_analyzer);
        self
    }

    /// Set the Quipu knowledge graph store for knowledge expansion
    #[cfg(feature = "knowledge")]
    pub fn with_quipu_store(mut self, store: quipu::Store) -> Self {
        self.quipu_store = Some(store);
        self
    }

    /// Replace the context config (used by calibration to vary params without reopening stores)
    pub fn set_config(&mut self, config: ContextConfig) {
        self.config = config;
    }

    /// Classify a file path using config rules, falling back to built-in heuristics.
    fn classify(&self, path: &str) -> FileCategory {
        classify_file_with_rules(path, &self.config.file_type_rules)
    }

    /// Assemble a context bundle for the given query using hybrid search.
    ///
    /// This is the standard entry point: runs hybrid search to find seeds,
    /// then expands via coupling and applies budget constraints.
    pub async fn assemble(&mut self, query: &str, repo: Option<&str>) -> Result<ContextBundle> {
        // Phase 1: Seed via hybrid search
        let (seed_results, top_semantic_score, commit_chunks) =
            self.run_hybrid_search(query, repo).await?;

        // Convert internal SeedResults to public SeedChunks
        let seeds: Vec<SeedChunk> = seed_results
            .into_iter()
            .map(|r| SeedChunk {
                chunk: Chunk {
                    id: r.chunk_id,
                    file_path: r.file_path,
                    language: r.language,
                    name: r.name,
                    chunk_type: r.chunk_type,
                    start_line: r.start_line,
                    end_line: r.end_line,
                    content: r.content,
                    tags: String::new(),
                },
                score: r.score,
                source: SeedSource::Search {
                    match_type: r.match_type.unwrap_or(MatchType::Hybrid),
                },
                repo: r.repo,
            })
            .collect();

        let mut bundle = self
            .assemble_from_seeds_with_commits(query, seeds, commit_chunks, repo)
            .await?;
        bundle.summary.top_semantic_score = top_semantic_score;
        Ok(bundle)
    }

    /// Assemble a context bundle from externally-provided seed chunks.
    ///
    /// This is the generalized entry point that accepts pre-computed seeds
    /// (from search, diff analysis, or any other source) and runs coupling
    /// expansion + budget assembly on them.
    pub async fn assemble_from_seeds(
        &mut self,
        query: &str,
        seeds: Vec<SeedChunk>,
        repo: Option<&str>,
    ) -> Result<ContextBundle> {
        self.assemble_from_seeds_with_commits(query, seeds, vec![], repo)
            .await
    }

    /// Internal: assemble from seeds with optional commit chunks for commit→source bridging.
    async fn assemble_from_seeds_with_commits(
        &mut self,
        query: &str,
        seeds: Vec<SeedChunk>,
        commit_chunks: Vec<Chunk>,
        repo: Option<&str>,
    ) -> Result<ContextBundle> {
        // Collect unique files from seeds
        let seed_files: HashSet<String> = seeds.iter().map(|s| s.chunk.file_path.clone()).collect();

        // Convert SeedChunks to internal SeedResults for assembly
        let seed_results: Vec<SeedResult> = seeds
            .into_iter()
            .map(|s| {
                let match_type = match &s.source {
                    SeedSource::Search { match_type } => Some(*match_type),
                    SeedSource::Diff { .. } => None,
                };
                SeedResult {
                    chunk_id: s.chunk.id,
                    file_path: s.chunk.file_path,
                    language: s.chunk.language,
                    name: s.chunk.name,
                    chunk_type: s.chunk.chunk_type,
                    start_line: s.chunk.start_line,
                    end_line: s.chunk.end_line,
                    content: s.chunk.content,
                    score: s.score,
                    match_type,
                    indexed_at: None, // External seeds don't carry indexed_at
                    repo: s.repo,
                    tags: s.chunk.tags,
                    is_pinned: false,
                }
            })
            .collect();

        // Phase 2: Expand via temporal coupling
        let coupled_chunks = self.expand_coupling(&seed_files, repo).await?;

        // Phase 2b: Collect bridge signals (commit→source + doc→source)
        let bridge_files = self
            .collect_bridge_signals(&seed_results, &commit_chunks, repo)
            .await?;

        // Phase 2c: Apply bridge boost to seed scores if in Boost/BoostInject mode
        let seed_results = if matches!(
            self.config.bridge_mode,
            BridgeMode::Boost | BridgeMode::BoostInject
        ) {
            let factor = self.config.bridge_boost_factor;
            seed_results
                .into_iter()
                .map(|mut r| {
                    if bridge_files.contains(&r.file_path) {
                        r.score *= 1.0 + factor;
                    }
                    r
                })
                .collect()
        } else {
            seed_results
        };

        // Phase 2d: Collect bridged chunks for injection (Inject/BoostInject modes)
        let bridged_chunks = if matches!(
            self.config.bridge_mode,
            BridgeMode::Inject | BridgeMode::BoostInject
        ) {
            self.fetch_bridged_chunks(&seed_results, &bridge_files, repo)
                .await?
        } else {
            vec![]
        };

        // Phase 2e: Knowledge expansion via Quipu graph traversal
        #[cfg(feature = "knowledge")]
        let knowledge_chunks = self.expand_knowledge(query, &seed_results, repo).await?;
        #[cfg(not(feature = "knowledge"))]
        let knowledge_chunks: Vec<KnowledgeChunkInfo> = vec![];

        // Phase 2f: Structural neighbors of documentation hits via chunk edges
        let neighbor_chunks = self.expand_chunk_edges(&seed_results, repo).await?;

        // Phase 3: Assemble with budget
        assemble_bundle(
            query,
            &self.config,
            seed_results,
            coupled_chunks,
            bridged_chunks,
            knowledge_chunks,
            neighbor_chunks,
        )
    }

    /// Expand documentation seeds through the deterministic chunk-edge graph:
    /// the containing parent (part_of, outbound) and the adjacent chunks
    /// (next_chunk, both directions). Neighbor information as edges, not as a
    /// raw-line window — the retrieval-time counterpart of the index-time
    /// `full_context` enrichment.
    async fn expand_chunk_edges(
        &mut self,
        seed_results: &[SeedResult],
        repo: Option<&str>,
    ) -> Result<Vec<NeighborChunkInfo>> {
        if self.config.neighbor_budget_pct <= 0.0 {
            return Ok(vec![]);
        }

        let seed_ids: HashSet<&str> = seed_results.iter().map(|s| s.chunk_id.as_str()).collect();
        // Top documentation seeds only: code hits already get AST/coupling
        // expansion; adjacency is where doc retrieval loses the thread.
        let mut doc_seeds: Vec<&SeedResult> = seed_results
            .iter()
            .filter(|s| self.classify(&s.file_path) == FileCategory::Documentation)
            .collect();
        doc_seeds.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        doc_seeds.truncate(3);

        let mut neighbors = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for seed in doc_seeds {
            let edges = self
                .vector_store
                .get_edges_for_chunk(&seed.chunk_id, repo)
                .await
                .unwrap_or_default();
            let anchor_name = seed.name.clone().unwrap_or_else(|| seed.file_path.clone());

            for edge in &edges {
                let (relation, neighbor_id) =
                    match (edge.edge_type, edge.source_chunk == seed.chunk_id) {
                        (crate::types::ChunkEdgeType::PartOf, true) => {
                            ("parent of", &edge.target_chunk)
                        }
                        (crate::types::ChunkEdgeType::NextChunk, true) => {
                            ("follows", &edge.target_chunk)
                        }
                        (crate::types::ChunkEdgeType::NextChunk, false) => {
                            ("precedes", &edge.source_chunk)
                        }
                        _ => continue,
                    };
                if seed_ids.contains(neighbor_id.as_str()) || !seen.insert(neighbor_id.clone()) {
                    continue;
                }
                let Ok(Some(chunk)) = self.vector_store.get_chunk_by_id(neighbor_id).await else {
                    // Stale edge target (chunk re-hashed after an edit) — skip.
                    continue;
                };
                neighbors.push(NeighborChunkInfo {
                    chunk_id: chunk.id,
                    file_path: chunk.file_path,
                    language: chunk.language,
                    name: chunk.name,
                    chunk_type: chunk.chunk_type,
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    content: chunk.content,
                    neighbor_score: seed.score * 0.4,
                    discovered_via: format!("{relation} {anchor_name}"),
                });
            }
        }

        Ok(neighbors)
    }

    /// Collect bridge signal file paths from both doc→source and commit→source paths.
    ///
    /// Returns a set of source file paths discovered through bridging:
    /// - Doc→source: blame doc chunks → commits → source files
    /// - Commit→source: parse "Files changed:" from matching commit chunks
    async fn collect_bridge_signals(
        &mut self,
        seeds: &[SeedResult],
        commit_chunks: &[Chunk],
        _repo: Option<&str>,
    ) -> Result<HashSet<String>> {
        if self.config.bridge_mode == BridgeMode::Off {
            return Ok(HashSet::new());
        }

        let mut bridge_files: HashSet<String> = HashSet::new();

        // Determine the repo root for resolving relative paths from git commands
        // to absolute paths that match the vector store's storage format.
        let repo_root = self
            .git_analyzer
            .as_ref()
            .map(|g| g.repo_root().to_path_buf());

        // Helper: resolve a relative path from git to match vector store paths.
        // git diff-tree returns relative paths, but the index stores absolute paths.
        let resolve_path = |rel_path: String| -> String {
            if let Some(root) = &repo_root {
                if !std::path::Path::new(&rel_path).is_absolute() {
                    return root.join(&rel_path).to_string_lossy().to_string();
                }
            }
            rel_path
        };

        // --- Commit→source bridging: extract file lists from commit chunk content ---
        for chunk in commit_chunks {
            for file_path in parse_commit_files(&chunk.content) {
                let category = self.classify(&file_path);
                if category == FileCategory::Source || category == FileCategory::Test {
                    bridge_files.insert(resolve_path(file_path));
                }
            }
        }

        // --- Doc→source bridging: git blame → commits → source files ---
        if let Some(git) = &self.git_analyzer {
            for seed in seeds {
                let category = self.classify(&seed.file_path);
                if category != FileCategory::Documentation {
                    continue;
                }

                let blame_entries =
                    match git.blame_lines(&seed.file_path, seed.start_line, seed.end_line) {
                        Ok(entries) => entries,
                        Err(_) => continue,
                    };

                let commit_hashes: HashSet<String> =
                    blame_entries.into_iter().map(|e| e.commit_hash).collect();

                for hash in &commit_hashes {
                    let commit_files = match git.get_commit_files(hash) {
                        Ok(files) => files,
                        Err(_) => continue,
                    };

                    for file_path in commit_files {
                        let file_category = self.classify(&file_path);
                        if file_category == FileCategory::Source
                            || file_category == FileCategory::Test
                        {
                            bridge_files.insert(resolve_path(file_path));
                        }
                    }
                }
            }
        }

        Ok(bridge_files)
    }

    /// Fetch chunks for bridged files not already in seeds (for Inject/BoostInject modes).
    async fn fetch_bridged_chunks(
        &mut self,
        seeds: &[SeedResult],
        bridge_files: &HashSet<String>,
        repo: Option<&str>,
    ) -> Result<Vec<CoupledChunkInfo>> {
        let mut bridged_chunks: Vec<CoupledChunkInfo> = Vec::new();

        // Files already in seeds — don't inject duplicates
        let seed_files: HashSet<&str> = seeds.iter().map(|s| s.file_path.as_str()).collect();

        // Use the best seed score as a proxy for bridged chunk scoring.
        // Scale down to 0.35x (was 0.5x) — bridged files are speculative.
        let best_seed_score = seeds.iter().map(|s| s.score).fold(0.0_f32, f32::max);

        let max_files = self.config.max_bridged_files;
        let max_chunks_per_file = self.config.max_bridged_chunks_per_file;
        let mut files_fetched: usize = 0;

        for file_path in bridge_files {
            if files_fetched >= max_files {
                break;
            }
            if seed_files.contains(file_path.as_str()) {
                continue;
            }

            let chunks = match self.vector_store.get_chunks_for_file(file_path, repo).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            files_fetched += 1;

            for chunk in chunks.into_iter().take(max_chunks_per_file) {
                bridged_chunks.push(CoupledChunkInfo {
                    chunk_id: chunk.id.clone(),
                    file_path: chunk.file_path.clone(),
                    language: chunk.language.clone(),
                    name: chunk.name.clone(),
                    chunk_type: chunk.chunk_type,
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    content: chunk.content.clone(),
                    coupling_score: best_seed_score * 0.35, // Bridged files get 35% of best seed (speculative)
                    coupled_to: "bridge".to_string(),
                });
            }
        }

        Ok(bridged_chunks)
    }

    /// Expand seed files via temporal coupling relationships.
    async fn expand_coupling(
        &mut self,
        seed_files: &HashSet<String>,
        repo: Option<&str>,
    ) -> Result<Vec<CoupledChunkInfo>> {
        let mut coupled_chunks: Vec<CoupledChunkInfo> = Vec::new();

        if self.config.depth > 0 {
            let mut seen_coupled_files: HashSet<String> = HashSet::new();

            for seed_file in seed_files {
                let couplings = self
                    .metadata_store
                    .get_coupling(seed_file, self.config.max_coupled)?;

                for coupling in couplings {
                    if coupling.score < self.config.coupling_threshold {
                        continue;
                    }

                    let other_file = if coupling.file_a == *seed_file {
                        &coupling.file_b
                    } else {
                        &coupling.file_a
                    };

                    // Skip files already in seed results or already fetched
                    if seed_files.contains(other_file) || seen_coupled_files.contains(other_file) {
                        continue;
                    }
                    seen_coupled_files.insert(other_file.to_string());

                    let chunks = self
                        .vector_store
                        .get_chunks_for_file(other_file, repo)
                        .await?;

                    // Limit chunks per coupled file to prevent flooding from
                    // large files. Keep first 2 chunks (sorted by start_line).
                    let max_coupled_chunks = 2;
                    for chunk in chunks.into_iter().take(max_coupled_chunks) {
                        coupled_chunks.push(CoupledChunkInfo {
                            chunk_id: chunk.id.clone(),
                            file_path: chunk.file_path.clone(),
                            language: chunk.language.clone(),
                            name: chunk.name.clone(),
                            chunk_type: chunk.chunk_type,
                            start_line: chunk.start_line,
                            end_line: chunk.end_line,
                            content: chunk.content.clone(),
                            coupling_score: coupling.score,
                            coupled_to: seed_file.clone(),
                        });
                    }
                }
            }
        }

        Ok(coupled_chunks)
    }

    /// Expand seed results via Quipu knowledge graph traversal.
    ///
    /// For each unique seed file, looks up the file's entity in Quipu by IRI,
    /// traverses N hops to discover related entities, scores neighbors by
    /// embedding similarity to the query, and converts them to KnowledgeChunkInfo
    /// items for budget allocation.
    #[cfg(feature = "knowledge")]
    async fn expand_knowledge(
        &mut self,
        query: &str,
        seed_results: &[SeedResult],
        repo: Option<&str>,
    ) -> Result<Vec<KnowledgeChunkInfo>> {
        let store = match &self.quipu_store {
            Some(s) => s,
            None => return Ok(vec![]),
        };

        if self.config.knowledge_budget_pct <= 0.0 {
            return Ok(vec![]);
        }

        let max_hops = self.config.knowledge_max_hops;
        let mut knowledge_chunks: Vec<KnowledgeChunkInfo> = Vec::new();

        // Collect unique seed file paths for entity lookup
        let seed_files: HashSet<&str> = seed_results.iter().map(|s| s.file_path.as_str()).collect();

        // Use Quipu tool_context with the query to find related entities.
        // This performs text search with link expansion — entities are returned
        // with their types, labels, facts, and relationships.
        let input = serde_json::json!({
            "query": query,
            "max_entities": 20,
            "expand_links": max_hops > 0,
        });

        let result = match quipu::tool_context(store, &input) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Knowledge graph expansion failed: {e}");
                return Ok(vec![]);
            }
        };

        // Parse the result — tool_context returns a JSON object with an "entities" array.
        // Each entity has: iri, label, type, facts (array of {predicate, object}).
        // We look for entities whose IRIs reference file paths (bobbin:code/...) and
        // fetch those files' chunks from the vector store.
        let entities = match result.get("entities").and_then(|e| e.as_array()) {
            Some(arr) => arr,
            None => return Ok(knowledge_chunks),
        };

        // Best seed score for scaling knowledge chunk scores
        let best_seed_score = seed_results.iter().map(|s| s.score).fold(0.0_f32, f32::max);

        // Score decay per hop (entities further from query get lower scores)
        let hop_decay = 0.6_f32;

        let mut files_fetched: usize = 0;
        let max_knowledge_files = 5;
        let max_chunks_per_file = 2;

        for (idx, entity) in entities.iter().enumerate() {
            if files_fetched >= max_knowledge_files {
                break;
            }

            let iri = match entity.get("iri").and_then(|v| v.as_str()) {
                Some(i) => i,
                None => continue,
            };

            let file_path = match file_path_from_entity_iri(iri) {
                Some(p) => p,
                None => continue,
            };
            let file_path = file_path.as_str();

            // Skip files already in seeds
            if seed_files.contains(file_path) {
                continue;
            }

            let entity_label = entity
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or(iri)
                .to_string();

            // Fetch chunks from the vector store for this entity's file
            let chunks = match self.vector_store.get_chunks_for_file(file_path, repo).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            if chunks.is_empty() {
                continue;
            }

            files_fetched += 1;

            // Score: base on seed relevance, decayed by position (proxy for hop distance)
            let position_decay = hop_decay.powi((idx as i32).min(max_hops as i32));
            let knowledge_score = best_seed_score * 0.3 * position_decay;

            for chunk in chunks.into_iter().take(max_chunks_per_file) {
                knowledge_chunks.push(KnowledgeChunkInfo {
                    chunk_id: chunk.id.clone(),
                    file_path: chunk.file_path.clone(),
                    language: chunk.language.clone(),
                    name: chunk.name.clone(),
                    chunk_type: chunk.chunk_type,
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    content: chunk.content.clone(),
                    knowledge_score,
                    discovered_via: entity_label.clone(),
                });
            }
        }

        Ok(knowledge_chunks)
    }

    /// Run hybrid search manually to avoid ownership issues with HybridSearch.
    /// Returns `(results, top_semantic_score, commit_chunks)` where `top_semantic_score`
    /// is the raw cosine similarity of the best semantic match before RRF normalization,
    /// and `commit_chunks` are matching commit chunks (for commit→source bridging).
    async fn run_hybrid_search(
        &mut self,
        query: &str,
        repo: Option<&str>,
    ) -> Result<(Vec<SeedResult>, f32, Vec<Chunk>)> {
        let fetch_limit = self.config.search_limit * 2;

        // Semantic search uses raw query (embeddings handle natural language)
        let query_embedding = self.embedder.embed(query).await?;
        let all_semantic = self
            .vector_store
            .search_filtered(
                &query_embedding,
                fetch_limit,
                repo,
                self.config.extra_filter.as_deref(),
            )
            .await?;

        // Capture top commit chunks before filtering (for commit→source bridging).
        // Limit to top 3 commits to prevent bridge explosion from many loose matches.
        let max_commit_chunks = 3;
        let mut commit_chunks: Vec<Chunk> = Vec::new();
        if self.config.bridge_mode != BridgeMode::Off {
            for r in &all_semantic {
                if r.chunk.chunk_type == ChunkType::Commit
                    && commit_chunks.len() < max_commit_chunks
                {
                    commit_chunks.push(r.chunk.clone());
                }
            }
        }

        let semantic_results: Vec<_> = all_semantic
            .into_iter()
            .filter(|r| r.chunk.chunk_type != ChunkType::Commit)
            .collect();
        // FTS uses preprocessed query (stopwords removed for better BM25)
        let keyword_query = crate::search::preprocess::preprocess_for_keywords(query);
        let all_keyword = self
            .vector_store
            .search_fts_filtered(
                &keyword_query,
                fetch_limit,
                repo,
                self.config.extra_filter.as_deref(),
            )
            .await
            .unwrap_or_default();

        // Capture commit chunks from keyword results too (deduplicate by id, respect cap)
        if self.config.bridge_mode != BridgeMode::Off {
            let seen: HashSet<String> = commit_chunks.iter().map(|c| c.id.clone()).collect();
            for r in &all_keyword {
                if commit_chunks.len() >= max_commit_chunks {
                    break;
                }
                if r.chunk.chunk_type == ChunkType::Commit && !seen.contains(&r.chunk.id) {
                    commit_chunks.push(r.chunk.clone());
                }
            }
        }

        let keyword_results: Vec<_> = all_keyword
            .into_iter()
            .filter(|r| r.chunk.chunk_type != ChunkType::Commit)
            .collect();

        // Capture raw cosine similarity of the top semantic result before RRF
        let top_semantic_score = semantic_results.first().map(|r| r.score).unwrap_or(0.0);

        // RRF combination
        let k = self.config.rrf_k;
        let keyword_weight = 1.0 - self.config.semantic_weight;
        let mut scores: HashMap<String, (SeedResult, f32)> = HashMap::new();

        for (rank, result) in semantic_results.into_iter().enumerate() {
            let rrf_score = self.config.semantic_weight / (k + rank as f32 + 1.0);
            scores.insert(
                result.chunk.id.clone(),
                (
                    SeedResult {
                        chunk_id: result.chunk.id,
                        file_path: result.chunk.file_path,
                        language: result.chunk.language,
                        name: result.chunk.name,
                        chunk_type: result.chunk.chunk_type,
                        start_line: result.chunk.start_line,
                        end_line: result.chunk.end_line,
                        content: result.chunk.content,
                        score: 0.0,
                        match_type: result.match_type,
                        indexed_at: result.indexed_at,
                        repo: result.repo,
                        tags: result.chunk.tags,
                        is_pinned: false,
                    },
                    rrf_score,
                ),
            );
        }

        for (rank, result) in keyword_results.into_iter().enumerate() {
            let rrf_score = keyword_weight / (k + rank as f32 + 1.0);
            scores
                .entry(result.chunk.id.clone())
                .and_modify(|(existing, score)| {
                    *score += rrf_score;
                    existing.match_type = Some(MatchType::Hybrid);
                })
                .or_insert((
                    SeedResult {
                        chunk_id: result.chunk.id,
                        file_path: result.chunk.file_path,
                        language: result.chunk.language,
                        name: result.chunk.name,
                        chunk_type: result.chunk.chunk_type,
                        start_line: result.chunk.start_line,
                        end_line: result.chunk.end_line,
                        content: result.chunk.content,
                        score: 0.0,
                        match_type: result.match_type,
                        indexed_at: result.indexed_at,
                        repo: result.repo,
                        tags: result.chunk.tags,
                        is_pinned: false,
                    },
                    rrf_score,
                ));
        }

        // Apply scoring adjustments: tag effects (if configured) subsume doc demotion,
        // otherwise fall back to category-based demotion. Recency boost always applies.
        let doc_demotion = self.config.doc_demotion;
        let recency_hl = self.config.recency_half_life_days;
        let recency_w = self.config.recency_weight;
        let tags_config = &self.config.tags_config;
        let role = self.config.role.as_deref();
        let ft_rules = &self.config.file_type_rules;
        let repo_affinity = self.config.repo_affinity.as_deref();
        let repo_affinity_boost = self.config.repo_affinity_boost;

        // Personalized PageRank ranking signal (GH companion ppr-ranking-signal.md).
        // Seed PPR with the current candidate files and fold a bounded
        // connectivity multiplier into the final score. Disabled (empty map) when
        // ppr_weight == 0, no `knowledge` feature, or no Quipu store wired in.
        let ppr_weight = self.config.ppr_weight;
        let ppr_scores: std::collections::HashMap<String, f32> = {
            #[cfg(feature = "knowledge")]
            {
                if ppr_weight > 0.0 {
                    if let Some(ref mut store) = self.quipu_store {
                        // The coupling exporter takes paths straight from git, so the graph
                        // is keyed by REPO-RELATIVE paths, while search results carry
                        // ABSOLUTE ones. Seed IRIs must be built in the graph's form or
                        // every seed misses, PPR returns an empty map, and the ranking comes
                        // back byte-identical with no error at all. See bobbin-jdlkh.
                        let repo_root = self.config.repo_root.clone();
                        let to_rel = |p: &str| -> String {
                            repo_root
                                .as_ref()
                                .and_then(|root| std::path::Path::new(p).strip_prefix(root).ok())
                                .map(|rel| rel.to_string_lossy().to_string())
                                .unwrap_or_else(|| p.to_string())
                        };
                        // compute_code_ppr keys its result by whatever path we hand it, but
                        // the multiplier below looks up by absolute file_path — so map back.
                        let mut rel_to_abs: std::collections::HashMap<String, String> =
                            std::collections::HashMap::new();
                        let candidates: Vec<(String, f32)> = scores
                            .values()
                            .map(|(r, s)| {
                                let rel = to_rel(&r.file_path);
                                rel_to_abs.insert(rel.clone(), r.file_path.clone());
                                (rel, *s)
                            })
                            .collect();
                        let repo = self
                            .config
                            .repo_affinity
                            .clone()
                            .or_else(|| scores.values().find_map(|(r, _)| r.repo.clone()))
                            .unwrap_or_default();
                        crate::search::ppr::compute_code_ppr(
                            store,
                            &candidates,
                            &repo,
                            crate::search::ppr::DEFAULT_SEED_K,
                            crate::search::ppr::DEFAULT_DAMPING,
                        )
                        .into_iter()
                        .map(|(rel, score)| (rel_to_abs.get(&rel).cloned().unwrap_or(rel), score))
                        .collect()
                    } else {
                        std::collections::HashMap::new()
                    }
                } else {
                    std::collections::HashMap::new()
                }
            }
            #[cfg(not(feature = "knowledge"))]
            {
                std::collections::HashMap::new()
            }
        };

        let mut combined: Vec<_> = scores
            .into_values()
            .map(|(mut result, score)| {
                let adjusted_score = if let Some(ref tc) = tags_config {
                    // Check for pin effect — pinned chunks skip category demotion
                    if tc.resolve_pin(&result.tags, role).is_some() {
                        result.is_pinned = true;
                        // Pinned chunks get raw relevance score (no demotion)
                        score
                    } else {
                        // Tag effects mode: compute product of (1+boost) for all tags
                        apply_tag_effects(
                            tc,
                            &result.tags,
                            role,
                            score,
                            doc_demotion,
                            &result.file_path,
                            ft_rules,
                        )
                    }
                } else {
                    // Legacy mode: category-based doc demotion
                    let category = classify_file_with_rules(&result.file_path, ft_rules);
                    if category.is_doc_like() {
                        score * doc_demotion
                    } else {
                        score
                    }
                };
                let after_recency =
                    apply_recency_boost(adjusted_score, result.indexed_at, recency_hl, recency_w);
                // Repo affinity: soft boost for files from the agent's current repo
                let after_affinity = if let Some(affinity) = repo_affinity {
                    if result.repo.as_deref() == Some(affinity) {
                        after_recency * repo_affinity_boost
                    } else {
                        after_recency
                    }
                } else {
                    after_recency
                };
                // Bounded PPR connectivity boost (no-op when ppr_weight == 0 or
                // the file has no PPR score).
                let final_score = if ppr_weight > 0.0 {
                    let ppr = ppr_scores.get(&result.file_path).copied().unwrap_or(0.0);
                    after_affinity * crate::search::ppr::ppr_multiplier(ppr, ppr_weight)
                } else {
                    after_affinity
                };
                (result, final_score)
            })
            .collect();
        combined.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Normalize RRF scores to [0, 1] so downstream threshold filters
        // (which expect similarity-scale scores) work correctly.
        let max_score = combined.first().map(|(_, s)| *s).unwrap_or(1.0);

        Ok((
            combined
                .into_iter()
                .take(self.config.search_limit)
                .map(|(mut result, score)| {
                    result.score = if max_score > 0.0 {
                        score / max_score
                    } else {
                        0.0
                    };
                    result
                })
                .collect(),
            top_semantic_score,
            commit_chunks,
        ))
    }
}

/// Parse file paths from a commit chunk's content field.
///
/// Commit content format: `<message>\n\nAuthor: ...\nDate: ...\n\nFiles changed:\nfile1\nfile2`
fn parse_commit_files(content: &str) -> Vec<String> {
    if let Some(idx) = content.find("Files changed:\n") {
        let files_section = &content[idx + "Files changed:\n".len()..];
        files_section
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect()
    } else {
        vec![]
    }
}

/// Format content based on content mode
fn format_content(content: &str, mode: ContentMode) -> Option<String> {
    match mode {
        ContentMode::Full => Some(content.to_string()),
        ContentMode::Preview => {
            let lines: Vec<&str> = content.lines().take(3).collect();
            let preview = lines.join("\n");
            if content.lines().count() > 3 {
                Some(format!("{}...", preview))
            } else {
                Some(preview)
            }
        }
        ContentMode::None => None,
    }
}

/// Assemble a context bundle from seed, coupled, bridged, and knowledge results (pure logic, no I/O)
fn assemble_bundle(
    query: &str,
    config: &ContextConfig,
    seed_results: Vec<SeedResult>,
    coupled_chunks: Vec<CoupledChunkInfo>,
    bridged_chunks: Vec<CoupledChunkInfo>,
    knowledge_chunks: Vec<KnowledgeChunkInfo>,
    neighbor_chunks: Vec<NeighborChunkInfo>,
) -> Result<ContextBundle> {
    let budget = config.budget_lines;
    let max_chunk_lines = budget / 2; // Cap individual chunks at 50% of budget
    let mut used_lines: usize = 0;
    let mut pinned_lines: usize = 0;
    let mut pinned_chunk_count: usize = 0;
    let mut seen_chunk_ids: HashSet<String> = HashSet::new();

    // Filter out commit chunks — they're useful for `bobbin log` but shouldn't
    // be injected into context. Commit messages pollute the context budget without
    // providing actionable code.
    // Also filter out archive chunks (pensieve/HLA/fieldnotes) — these belong in
    // the dedicated archive search endpoint, not in general code context injection.
    const ARCHIVE_LANGUAGES: &[&str] = &["hla", "pensieve", "fieldnotes", "archive"];
    // Archive-only repos: these repos contain only archive records (agent memory,
    // chat logs). Their .md files get indexed by the regular file indexer as
    // "markdown" and bypass the language filter above. Filter them by repo name.
    const ARCHIVE_REPOS: &[&str] = &["pensieve", "hla-records"];
    let seed_results: Vec<SeedResult> = seed_results
        .into_iter()
        .filter(|r| r.chunk_type != ChunkType::Commit)
        .filter(|r| !ARCHIVE_LANGUAGES.contains(&r.language.as_str()))
        .filter(|r| {
            !r.repo
                .as_ref()
                .is_some_and(|repo| ARCHIVE_REPOS.contains(&repo.as_str()))
        })
        .collect();

    // Path dedup: same file indexed at both relative and absolute paths
    // (e.g. "internal/cmd/foo.go" and "/data/repos/gastown/internal/cmd/foo.go").
    // Normalize by stripping the configured repo_path_prefix/<repo>/ prefix, then keep
    // the higher-scoring variant. Only dedup when original paths differ (same file,
    // different index paths) — don't remove different chunks from the same file.
    let seed_results = {
        let repo_prefix = config.repo_path_prefix.as_deref().unwrap_or("");
        let normalize = |path: &str| -> String {
            if path.starts_with(repo_prefix) {
                path[repo_prefix.len()..]
                    .find('/')
                    .map(|i| &path[repo_prefix.len() + i + 1..])
                    .unwrap_or(path)
                    .to_string()
            } else {
                path.trim_start_matches('/').to_string()
            }
        };
        // Group by (normalized_path, chunk_range) to only dedup true duplicates
        // (same relative path + same line range = same content from different index paths)
        let mut best_idx: HashMap<(String, u32, u32), usize> = HashMap::new();
        let mut remove_set: HashSet<usize> = HashSet::new();
        for (idx, result) in seed_results.iter().enumerate() {
            let relpath = normalize(&result.file_path);
            let key = (relpath, result.start_line, result.end_line);
            if let Some(&prev_idx) = best_idx.get(&key) {
                // Same relative path AND same line range — true duplicate
                if result.score > seed_results[prev_idx].score {
                    remove_set.insert(prev_idx);
                    best_idx.insert(key, idx);
                } else {
                    remove_set.insert(idx);
                }
            } else {
                best_idx.insert(key, idx);
            }
        }
        if !remove_set.is_empty() {
            seed_results
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !remove_set.contains(i))
                .map(|(_, r)| r)
                .collect::<Vec<_>>()
        } else {
            seed_results
        }
    };

    // Noise path filter: design docs, agent scaffolding, and static product docs.
    // Defined here (before seed partitioning) so it applies to ALL results, not
    // just coupled/bridged chunks. This prevents design docs, CLAUDE.md, memory
    // files, etc. from consuming seed budget when they're already in agent context.
    let is_noise_path = |path: &str, lang: &str, _repo: &Option<String>| -> bool {
        // Archive language filter
        if ARCHIVE_LANGUAGES.contains(&lang) {
            return true;
        }
        // Planning/orchestration paths — these are internal agent state, not code
        // context. Doc types with tag effects (design, runbook, audit, probe) are
        // intentionally NOT filtered here — tag scoring handles their relevance.
        let lower = path.to_lowercase();
        if lower.contains("/_plans/")
            || lower.contains("/_roadmap/")
            || lower.contains("/_specs/")
            || lower.contains("/crew/")
            || lower.contains("/polecats/")
            || lower.contains("/docs/tasks/")
            || lower.contains("/docs/plans/")
            || lower.contains("/memory/")
            || lower.contains("/.beads/")
            || lower.contains("/session-notes/")
            || lower.contains("/sessions/")
            || lower.ends_with("/roadmap.md")
            || lower.ends_with("/changelog.md")
        {
            return true;
        }
        // Test/example directories — test code scores high for implementation
        // queries but rarely provides useful context for the agent's actual task.
        if lower.contains("/tests/")
            || lower.contains("/test/")
            || lower.contains("/__tests__/")
            || lower.contains("/spec/")
            || lower.contains("/specs/")
            || lower.contains("/testdata/")
            || lower.contains("/fixtures/")
            || lower.contains("/examples/")
            || lower.contains("/example/")
            || lower.contains("/samples/")
            || lower.contains("/demo/")
            || lower.contains("/demos/")
        {
            return true;
        }
        // CI workflow paths — GitHub/GitLab/CircleCI workflows are rarely useful
        // as code context. Ansible, Terraform, and Helm have tag effects (domain:iac)
        // and are intentionally NOT hard-filtered here.
        if lower.contains("/.github/workflows/")
            || lower.contains("/.github/actions/")
            || lower.contains("/.circleci/")
            || lower.contains("/.gitlab-ci")
            || lower.contains("/.forgejo/workflows/")
        {
            return true;
        }
        // CLAUDE.md/AGENTS.md and static product docs already in agent context
        let filename = path.rsplit('/').next().unwrap_or(path);
        if matches!(
            filename,
            "CLAUDE.md"
                | "AGENTS.md"
                | "@AGENTS.md"
                | "VISION.md"
                | "PRD.md"
                | "ARCHITECTURE.md"
                | "DESIGN.md"
                | "CHANGELOG.md"
                | "MEMORY.md"
                | "README.md"
                | "CONTRIBUTING.md"
                | "LICENSE.md"
                | "QUICKSTART.md"
                | "FAQ.md"
                | "INSTALLING.md"
                | "UNINSTALLING.md"
                | "TROUBLESHOOTING.md"
                | "RELEASING.md"
                | "SETUP.md"
        ) {
            return true;
        }
        // Test file patterns (catches test files outside /test/ directories)
        let fname_lower = filename.to_lowercase();
        if fname_lower.ends_with("_test.go") || fname_lower.ends_with("_test.rs")
            || fname_lower.ends_with(".test.ts") || fname_lower.ends_with(".test.js")
            || fname_lower.ends_with(".spec.ts") || fname_lower.ends_with(".spec.js")
            || fname_lower.starts_with("test_") // Python test files
            || matches!(filename, "Dockerfile" | "docker-compose.yml" | "docker-compose.yaml"
                | "Makefile" | "Justfile" | "Taskfile.yml")
        {
            return true;
        }
        // Lock files and generated output — never useful as code context
        if matches!(
            filename,
            "Cargo.lock"
                | "package-lock.json"
                | "yarn.lock"
                | "pnpm-lock.yaml"
                | "go.sum"
                | "Gemfile.lock"
                | "poetry.lock"
                | "composer.lock"
                | "Pipfile.lock"
        ) {
            return true;
        }
        // Vendored/generated directories — third-party code wastes context
        if lower.contains("/vendor/")
            || lower.contains("/node_modules/")
            || lower.contains("/third_party/")
            || lower.contains("/dist/")
            || lower.contains("/build/")
            || lower.contains("/target/")
        {
            return true;
        }
        false
    };

    // Apply noise path filter to seed results — prevents design docs, agent
    // scaffolding, and boilerplate from consuming budget as direct hits
    let seed_results: Vec<SeedResult> = seed_results
        .into_iter()
        .filter(|r| !is_noise_path(&r.file_path, &r.language, &r.repo))
        .collect();

    // Apply cross-agent feedback boost — files rated useful for similar queries
    // get a score boost, files rated harmful get a penalty.
    let seed_results: Vec<SeedResult> = if let Some(ref fb_scores) = config.feedback_scores {
        let weight = config.feedback_boost_weight;
        let max_boost = config.feedback_boost_max;
        seed_results
            .into_iter()
            .map(|mut r| {
                if let Some(&fb_score) = fb_scores.get(&r.file_path) {
                    let boost = (fb_score * weight).clamp(-max_boost, max_boost);
                    r.score *= 1.0 + boost;
                }
                r
            })
            .collect()
    } else {
        seed_results
    };

    let mut capture = config.capture_candidates.then(|| {
        capture::snapshot(
            config,
            &seed_results,
            &coupled_chunks,
            &bridged_chunks,
            &knowledge_chunks,
            &neighbor_chunks,
            &is_noise_path,
        )
    });

    // Partition into pinned and normal results
    let (pinned_results, normal_results): (Vec<SeedResult>, Vec<SeedResult>) =
        seed_results.into_iter().partition(|r| r.is_pinned);

    // Calculate budget reserve for pinned chunks.
    // Use the max budget_reserve from any pin effect in tags config, or default
    // to 20% of total budget if pin effects don't specify a reserve.
    let pin_budget_reserve = if pinned_results.is_empty() {
        0
    } else if let Some(ref tc) = config.tags_config {
        let max_reserve = tc
            .effects
            .values()
            .filter(|e| e.pin && e.budget_reserve > 0)
            .map(|e| e.budget_reserve)
            .max()
            .unwrap_or(0);
        if max_reserve > 0 {
            max_reserve.min(budget / 2) // Never reserve more than half the budget
        } else {
            budget / 5 // Default: 20% of budget for pins
        }
    } else {
        budget / 5
    };

    if let Some(ref mut capture) = capture {
        capture.policy["pin_budget_reserve"] = pin_budget_reserve.into();
    }

    let mut context_files: Vec<ContextFile> = Vec::new();

    // Phase 0: Inject pinned chunks first (within reserved budget)
    if !pinned_results.is_empty() {
        let mut pinned_files: HashMap<String, Vec<SeedResult>> = HashMap::new();
        for result in pinned_results {
            pinned_files
                .entry(result.file_path.clone())
                .or_default()
                .push(result);
        }

        // Sort pinned files by highest score (rank among pins by raw relevance)
        let mut pinned_file_list: Vec<(String, Vec<SeedResult>)> =
            pinned_files.into_iter().collect();
        pinned_file_list.sort_by(|a, b| {
            let max_a =
                a.1.iter()
                    .map(|r| r.score)
                    .fold(f32::NEG_INFINITY, f32::max);
            let max_b =
                b.1.iter()
                    .map(|r| r.score)
                    .fold(f32::NEG_INFINITY, f32::max);
            max_b
                .partial_cmp(&max_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for (file_path, mut results) in pinned_file_list {
            if used_lines >= pin_budget_reserve {
                break;
            }
            results.sort_by_key(|r| r.start_line);

            let language = results
                .first()
                .map(|r| r.language.clone())
                .unwrap_or_default();
            let file_repo = results.first().and_then(|r| r.repo.clone());
            let file_score = results
                .iter()
                .map(|r| r.score)
                .fold(f32::NEG_INFINITY, f32::max);

            let mut file_chunks = Vec::new();
            for result in results {
                if seen_chunk_ids.contains(&result.chunk_id) {
                    continue;
                }
                let chunk_lines = chunk_cost(
                    config.budget_unit,
                    &result.content,
                    result.start_line,
                    result.end_line,
                );
                let capped_lines = chunk_lines.min(max_chunk_lines);
                if used_lines + capped_lines > pin_budget_reserve {
                    break;
                }
                seen_chunk_ids.insert(result.chunk_id.clone());
                used_lines += capped_lines;
                pinned_lines += capped_lines;
                pinned_chunk_count += 1;

                file_chunks.push(ContextChunk {
                    id: result.chunk_id.clone(),
                    name: result.name.clone(),
                    chunk_type: result.chunk_type,
                    start_line: result.start_line,
                    end_line: result.end_line,
                    score: result.score,
                    match_type: result.match_type,
                    content: format_content(&result.content, config.content_mode),
                });
            }

            if !file_chunks.is_empty() {
                context_files.push(ContextFile {
                    path: file_path.clone(),
                    language,
                    relevance: FileRelevance::Pinned,
                    category: classify_file_with_rules(&file_path, &config.file_type_rules),
                    score: file_score,
                    coupled_to: vec![],
                    chunks: file_chunks,
                    repo: file_repo,
                });
            }
        }
    }

    // Group normal seed results by file
    let mut direct_files: HashMap<String, Vec<SeedResult>> = HashMap::new();
    for result in normal_results {
        direct_files
            .entry(result.file_path.clone())
            .or_default()
            .push(result);
    }

    // Sort files by highest score
    let mut direct_file_list: Vec<(String, Vec<SeedResult>)> = direct_files.into_iter().collect();
    direct_file_list.sort_by(|a, b| {
        let max_a =
            a.1.iter()
                .map(|r| r.score)
                .fold(f32::NEG_INFINITY, f32::max);
        let max_b =
            b.1.iter()
                .map(|r| r.score)
                .fold(f32::NEG_INFINITY, f32::max);
        max_b
            .partial_cmp(&max_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut direct_hit_count: usize = 0;

    // Add direct hit chunks
    for (file_path, mut results) in direct_file_list {
        results.sort_by_key(|r| r.start_line);

        let language = results
            .first()
            .map(|r| r.language.clone())
            .unwrap_or_default();

        let file_repo = results.first().and_then(|r| r.repo.clone());

        let file_score = results
            .iter()
            .map(|r| r.score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut file_chunks = Vec::new();
        for result in results {
            if seen_chunk_ids.contains(&result.chunk_id) {
                continue;
            }

            let chunk_lines = chunk_cost(
                config.budget_unit,
                &result.content,
                result.start_line,
                result.end_line,
            );
            let capped_lines = chunk_lines.min(max_chunk_lines);

            if used_lines + capped_lines > budget {
                break;
            }

            seen_chunk_ids.insert(result.chunk_id.clone());
            used_lines += capped_lines;
            direct_hit_count += 1;

            file_chunks.push(ContextChunk {
                id: result.chunk_id.clone(),
                name: result.name.clone(),
                chunk_type: result.chunk_type,
                start_line: result.start_line,
                end_line: result.end_line,
                score: result.score,
                match_type: result.match_type,
                content: format_content(&result.content, config.content_mode),
            });
        }

        if !file_chunks.is_empty() {
            context_files.push(ContextFile {
                path: file_path.clone(),
                language,
                relevance: FileRelevance::Direct,
                category: classify_file_with_rules(&file_path, &config.file_type_rules),
                score: file_score,
                coupled_to: vec![],
                chunks: file_chunks,
                repo: file_repo,
            });
        }
    }

    // Apply the same noise path filter to coupled/bridged chunks
    let coupled_chunks: Vec<CoupledChunkInfo> = coupled_chunks
        .into_iter()
        .filter(|c| !is_noise_path(&c.file_path, &c.language, &None))
        .collect();
    let bridged_chunks: Vec<CoupledChunkInfo> = bridged_chunks
        .into_iter()
        .filter(|c| !is_noise_path(&c.file_path, &c.language, &None))
        .collect();

    // Group coupled chunks by file
    let mut coupled_files: HashMap<String, (Vec<CoupledChunkInfo>, HashSet<String>)> =
        HashMap::new();
    for chunk in coupled_chunks {
        let entry = coupled_files
            .entry(chunk.file_path.clone())
            .or_insert_with(|| (Vec::new(), HashSet::new()));
        entry.1.insert(chunk.coupled_to.clone());
        entry.0.push(chunk);
    }

    // Sort coupled files by coupling score
    let mut coupled_file_list: Vec<(String, Vec<CoupledChunkInfo>, HashSet<String>)> =
        coupled_files
            .into_iter()
            .map(|(path, (chunks, sources))| (path, chunks, sources))
            .collect();
    coupled_file_list.sort_by(|a, b| {
        let max_a =
            a.1.iter()
                .map(|c| c.coupling_score)
                .fold(f32::NEG_INFINITY, f32::max);
        let max_b =
            b.1.iter()
                .map(|c| c.coupling_score)
                .fold(f32::NEG_INFINITY, f32::max);
        max_b
            .partial_cmp(&max_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut coupled_addition_count: usize = 0;

    // Add coupled chunks
    for (file_path, mut chunks, sources) in coupled_file_list {
        if used_lines >= budget {
            break;
        }

        chunks.sort_by_key(|c| c.start_line);

        let language = chunks
            .first()
            .map(|c| c.language.clone())
            .unwrap_or_default();

        let file_score = chunks
            .iter()
            .map(|c| c.coupling_score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut file_chunks = Vec::new();
        for chunk in chunks {
            if seen_chunk_ids.contains(&chunk.chunk_id) {
                continue;
            }

            let chunk_lines = chunk_cost(
                config.budget_unit,
                &chunk.content,
                chunk.start_line,
                chunk.end_line,
            );
            let capped_lines = chunk_lines.min(max_chunk_lines);

            if used_lines + capped_lines > budget {
                break;
            }

            seen_chunk_ids.insert(chunk.chunk_id.clone());
            used_lines += capped_lines;
            coupled_addition_count += 1;

            file_chunks.push(ContextChunk {
                id: chunk.chunk_id.clone(),
                name: chunk.name.clone(),
                chunk_type: chunk.chunk_type,
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                score: chunk.coupling_score,
                match_type: None,
                content: format_content(&chunk.content, config.content_mode),
            });
        }

        if !file_chunks.is_empty() {
            let mut coupled_to: Vec<String> = sources.into_iter().collect();
            coupled_to.sort();

            context_files.push(ContextFile {
                path: file_path.clone(),
                language,
                relevance: FileRelevance::Coupled,
                category: classify_file_with_rules(&file_path, &config.file_type_rules),
                score: file_score,
                coupled_to,
                chunks: file_chunks,
                repo: None, // Coupled files don't carry repo from coupling source
            });
        }
    }

    // Group bridged chunks by file (same structure as coupled)
    let mut bridged_files: HashMap<String, (Vec<CoupledChunkInfo>, HashSet<String>)> =
        HashMap::new();
    for chunk in bridged_chunks {
        let entry = bridged_files
            .entry(chunk.file_path.clone())
            .or_insert_with(|| (Vec::new(), HashSet::new()));
        entry.1.insert(chunk.coupled_to.clone());
        entry.0.push(chunk);
    }

    let mut bridged_file_list: Vec<(String, Vec<CoupledChunkInfo>, HashSet<String>)> =
        bridged_files
            .into_iter()
            .map(|(path, (chunks, sources))| (path, chunks, sources))
            .collect();
    bridged_file_list.sort_by(|a, b| {
        let max_a =
            a.1.iter()
                .map(|c| c.coupling_score)
                .fold(f32::NEG_INFINITY, f32::max);
        let max_b =
            b.1.iter()
                .map(|c| c.coupling_score)
                .fold(f32::NEG_INFINITY, f32::max);
        max_b
            .partial_cmp(&max_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut bridged_addition_count: usize = 0;
    let bridged_budget = budget / 5; // Cap bridged content at 20% of total budget
    let mut bridged_lines: usize = 0;

    // Add bridged chunks (source files discovered via doc provenance)
    for (file_path, mut chunks, sources) in bridged_file_list {
        if used_lines >= budget || bridged_lines >= bridged_budget {
            break;
        }

        chunks.sort_by_key(|c| c.start_line);

        let language = chunks
            .first()
            .map(|c| c.language.clone())
            .unwrap_or_default();
        let file_score = chunks
            .iter()
            .map(|c| c.coupling_score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut file_chunks = Vec::new();
        for chunk in chunks {
            if seen_chunk_ids.contains(&chunk.chunk_id) {
                continue;
            }

            let chunk_lines = chunk_cost(
                config.budget_unit,
                &chunk.content,
                chunk.start_line,
                chunk.end_line,
            );
            let capped_lines = chunk_lines.min(max_chunk_lines);

            if used_lines + capped_lines > budget || bridged_lines + capped_lines > bridged_budget {
                break;
            }

            seen_chunk_ids.insert(chunk.chunk_id.clone());
            used_lines += capped_lines;
            bridged_lines += capped_lines;
            bridged_addition_count += 1;

            file_chunks.push(ContextChunk {
                id: chunk.chunk_id.clone(),
                name: chunk.name.clone(),
                chunk_type: chunk.chunk_type,
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                score: chunk.coupling_score,
                match_type: None,
                content: format_content(&chunk.content, config.content_mode),
            });
        }

        if !file_chunks.is_empty() {
            let mut coupled_to: Vec<String> = sources.into_iter().collect();
            coupled_to.sort();

            context_files.push(ContextFile {
                path: file_path.clone(),
                language,
                relevance: FileRelevance::Bridged,
                category: classify_file_with_rules(&file_path, &config.file_type_rules),
                score: file_score,
                coupled_to,
                chunks: file_chunks,
                repo: None, // Bridged files don't carry repo from bridge source
            });
        }
    }

    // Group knowledge chunks by file
    let mut knowledge_files: HashMap<String, (Vec<KnowledgeChunkInfo>, HashSet<String>)> =
        HashMap::new();
    for chunk in knowledge_chunks {
        // Apply the same noise path filter
        if is_noise_path(&chunk.file_path, &chunk.language, &None) {
            continue;
        }
        let entry = knowledge_files
            .entry(chunk.file_path.clone())
            .or_insert_with(|| (Vec::new(), HashSet::new()));
        entry.1.insert(chunk.discovered_via.clone());
        entry.0.push(chunk);
    }

    let mut knowledge_file_list: Vec<(String, Vec<KnowledgeChunkInfo>, HashSet<String>)> =
        knowledge_files
            .into_iter()
            .map(|(path, (chunks, sources))| (path, chunks, sources))
            .collect();
    knowledge_file_list.sort_by(|a, b| {
        let max_a =
            a.1.iter()
                .map(|c| c.knowledge_score)
                .fold(f32::NEG_INFINITY, f32::max);
        let max_b =
            b.1.iter()
                .map(|c| c.knowledge_score)
                .fold(f32::NEG_INFINITY, f32::max);
        max_b
            .partial_cmp(&max_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut knowledge_addition_count: usize = 0;
    let knowledge_budget = ((budget as f32) * config.knowledge_budget_pct / 100.0) as usize;
    let mut knowledge_lines: usize = 0;

    // Add knowledge chunks (entities discovered via Quipu graph traversal)
    for (file_path, mut chunks, sources) in knowledge_file_list {
        if used_lines >= budget || knowledge_lines >= knowledge_budget {
            break;
        }

        chunks.sort_by_key(|c| c.start_line);

        let language = chunks
            .first()
            .map(|c| c.language.clone())
            .unwrap_or_default();
        let file_score = chunks
            .iter()
            .map(|c| c.knowledge_score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut file_chunks = Vec::new();
        for chunk in chunks {
            if seen_chunk_ids.contains(&chunk.chunk_id) {
                continue;
            }

            let chunk_lines = chunk_cost(
                config.budget_unit,
                &chunk.content,
                chunk.start_line,
                chunk.end_line,
            );
            let capped_lines = chunk_lines.min(max_chunk_lines);

            if used_lines + capped_lines > budget
                || knowledge_lines + capped_lines > knowledge_budget
            {
                break;
            }

            seen_chunk_ids.insert(chunk.chunk_id.clone());
            used_lines += capped_lines;
            knowledge_lines += capped_lines;
            knowledge_addition_count += 1;

            file_chunks.push(ContextChunk {
                id: chunk.chunk_id.clone(),
                name: chunk.name.clone(),
                chunk_type: chunk.chunk_type,
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                score: chunk.knowledge_score,
                match_type: None,
                content: format_content(&chunk.content, config.content_mode),
            });
        }

        if !file_chunks.is_empty() {
            let mut coupled_to: Vec<String> = sources.into_iter().collect();
            coupled_to.sort();

            context_files.push(ContextFile {
                path: file_path.clone(),
                language,
                relevance: FileRelevance::Knowledge,
                category: classify_file_with_rules(&file_path, &config.file_type_rules),
                score: file_score,
                coupled_to,
                chunks: file_chunks,
                repo: None,
            });
        }
    }

    // Add structural neighbors (parent section / adjacent chunks of doc hits),
    // under their own sub-budget, after every higher-signal leg.
    let mut structural_addition_count: usize = 0;
    let neighbor_budget = ((budget as f32) * config.neighbor_budget_pct / 100.0) as usize;
    let mut neighbor_lines: usize = 0;

    let mut neighbor_files: HashMap<String, (Vec<NeighborChunkInfo>, HashSet<String>)> =
        HashMap::new();
    for chunk in neighbor_chunks {
        if is_noise_path(&chunk.file_path, &chunk.language, &None) {
            continue;
        }
        let entry = neighbor_files
            .entry(chunk.file_path.clone())
            .or_insert_with(|| (Vec::new(), HashSet::new()));
        entry.1.insert(chunk.discovered_via.clone());
        entry.0.push(chunk);
    }
    let mut neighbor_file_list: Vec<(String, Vec<NeighborChunkInfo>, HashSet<String>)> =
        neighbor_files
            .into_iter()
            .map(|(path, (chunks, sources))| (path, chunks, sources))
            .collect();
    neighbor_file_list.sort_by(|a, b| {
        let max_a =
            a.1.iter()
                .map(|c| c.neighbor_score)
                .fold(f32::NEG_INFINITY, f32::max);
        let max_b =
            b.1.iter()
                .map(|c| c.neighbor_score)
                .fold(f32::NEG_INFINITY, f32::max);
        max_b
            .partial_cmp(&max_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for (file_path, mut chunks, sources) in neighbor_file_list {
        if used_lines >= budget || neighbor_lines >= neighbor_budget {
            break;
        }

        chunks.sort_by_key(|c| c.start_line);
        let language = chunks
            .first()
            .map(|c| c.language.clone())
            .unwrap_or_default();
        let file_score = chunks
            .iter()
            .map(|c| c.neighbor_score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut file_chunks = Vec::new();
        for chunk in chunks {
            if seen_chunk_ids.contains(&chunk.chunk_id) {
                continue;
            }
            let chunk_lines = chunk_cost(
                config.budget_unit,
                &chunk.content,
                chunk.start_line,
                chunk.end_line,
            );
            let capped_lines = chunk_lines.min(max_chunk_lines);
            if used_lines + capped_lines > budget || neighbor_lines + capped_lines > neighbor_budget
            {
                break;
            }
            seen_chunk_ids.insert(chunk.chunk_id.clone());
            used_lines += capped_lines;
            neighbor_lines += capped_lines;
            structural_addition_count += 1;

            file_chunks.push(ContextChunk {
                id: chunk.chunk_id.clone(),
                name: chunk.name.clone(),
                chunk_type: chunk.chunk_type,
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                score: chunk.neighbor_score,
                match_type: None,
                content: format_content(&chunk.content, config.content_mode),
            });
        }

        if !file_chunks.is_empty() {
            let mut coupled_to: Vec<String> = sources.into_iter().collect();
            coupled_to.sort();
            context_files.push(ContextFile {
                path: file_path.clone(),
                language,
                relevance: FileRelevance::Structural,
                category: classify_file_with_rules(&file_path, &config.file_type_rules),
                score: file_score,
                coupled_to,
                chunks: file_chunks,
                repo: None,
            });
        }
    }

    let total_chunks = context_files.iter().map(|f| f.chunks.len()).sum();
    let total_files = context_files.len();
    let source_files = context_files
        .iter()
        .filter(|f| f.category == FileCategory::Source || f.category == FileCategory::Test)
        .count();
    let doc_files = context_files
        .iter()
        .filter(|f| f.category == FileCategory::Documentation)
        .count();

    Ok(ContextBundle {
        capture,
        query: query.to_string(),
        files: context_files,
        budget: BudgetInfo {
            max_lines: budget,
            used_lines,
            pinned_lines,
        },
        summary: ContextSummary {
            total_files,
            total_chunks,
            direct_hits: direct_hit_count,
            coupled_additions: coupled_addition_count,
            bridged_additions: bridged_addition_count,
            source_files,
            doc_files,
            top_semantic_score: 0.0,
            pinned_chunks: pinned_chunk_count,
            knowledge_additions: knowledge_addition_count,
            structural_additions: structural_addition_count,
        },
    })
}

/// Apply tag-based scoring effects. If a chunk has tags with configured effects,
/// compute score *= product(1 + boost) clamped to [0.01, 10.0], replacing doc demotion.
/// If no tag effects match, fall back to category-based doc demotion.
fn apply_tag_effects(
    tags_config: &crate::tags::TagsConfig,
    tags_str: &str,
    role: Option<&str>,
    score: f32,
    doc_demotion: f32,
    file_path: &str,
    file_type_rules: &[crate::config::FileTypeRule],
) -> f32 {
    if tags_str.is_empty() {
        // Untagged: fall back to category-based demotion
        let category = classify_file_with_rules(file_path, file_type_rules);
        return if category.is_doc_like() {
            score * doc_demotion
        } else {
            score
        };
    }

    let tags: Vec<&str> = tags_str.split(',').collect();
    let mut multiplier = 1.0_f32;
    let mut had_effect = false;

    for tag in &tags {
        if let Some(effect) = tags_config.resolve_effect(tag, role) {
            // Exclude effects are handled by pre-search WHERE clauses,
            // not in the scoring pipeline. Skip them here.
            if !effect.exclude {
                multiplier *= 1.0 + effect.boost;
                had_effect = true;
            }
        }
    }

    if had_effect {
        // Clamp to prevent extreme scores
        let clamped = multiplier.clamp(0.01, 10.0);
        score * clamped
    } else {
        // Tags present but no effects configured: fall back to category demotion
        let category = classify_file_with_rules(file_path, file_type_rules);
        if category.is_doc_like() {
            score * doc_demotion
        } else {
            score
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("context_budget_tests.rs");

    #[test]
    fn test_file_path_from_entity_iri_live_shapes() {
        // Module: path is one percent-encoded segment, under the LIVE base.
        assert_eq!(
            file_path_from_entity_iri(
                "http://aegis.gastown.local/ontology/code/quipu/src%2Fnamespace.rs"
            ),
            Some("src/namespace.rs".to_string())
        );
        // Symbol: yupana's `::{name}` suffix is dropped (NOT `/name-L{line}`).
        assert_eq!(
            file_path_from_entity_iri(
                "http://aegis.gastown.local/ontology/code/quipu/src%2Fstore%2Fops.rs::transact"
            ),
            Some("src/store/ops.rs".to_string())
        );
        // Section: yupana's `#{slug}` suffix is dropped (NOT `/S{line}`), on the
        // doc lane where sections are actually minted.
        assert_eq!(
            file_path_from_entity_iri(
                "http://aegis.gastown.local/ontology/doc/quipu/README.md#see-it-in-action"
            ),
            Some("README.md".to_string())
        );
        // The superseded lane no producer mints is NOT accepted.
        assert_eq!(
            file_path_from_entity_iri("http://aegis.gastown.local/code/quipu/src%2Fnamespace.rs"),
            None
        );
        // Outside the entity bases — including the old CURIE form — is None.
        assert_eq!(
            file_path_from_entity_iri("bobbin:code/repo/src/lib.rs"),
            None
        );
        assert_eq!(
            file_path_from_entity_iri("http://aegis.gastown.local/ontology/CodeModule"),
            None
        );
    }

    #[test]
    fn test_deduplication() {
        let config = ContextConfig {
            budget_lines: 500,
            depth: 0,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.5,
            rrf_k: 60.0,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            bridge_mode: BridgeMode::Inject,
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            ..ContextConfig::default()
        };

        let seeds = vec![
            make_seed("c1", "a.rs", 1, 5, 0.9),
            make_seed("c1", "a.rs", 1, 5, 0.8), // duplicate chunk ID
        ];

        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();
        assert_eq!(bundle.summary.total_chunks, 1);
    }

    #[test]
    fn test_no_coupled_when_empty() {
        let config = ContextConfig {
            budget_lines: 500,
            depth: 0,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.5,
            rrf_k: 60.0,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            bridge_mode: BridgeMode::Inject,
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            ..ContextConfig::default()
        };

        let seeds = vec![make_seed("c1", "a.rs", 1, 5, 0.9)];

        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();
        assert_eq!(bundle.summary.coupled_additions, 0);
    }

    #[test]
    fn test_file_ordering_direct_before_coupled() {
        let config = ContextConfig {
            budget_lines: 500,
            depth: 1,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.5,
            rrf_k: 60.0,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            bridge_mode: BridgeMode::Inject,
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            ..ContextConfig::default()
        };

        let seeds = vec![make_seed("c1", "a.rs", 1, 5, 0.9)];
        let coupled = vec![make_coupled("c2", "b.rs", 1, 5, 0.5, "a.rs")];

        let bundle =
            assemble_bundle("test", &config, seeds, coupled, vec![], vec![], vec![]).unwrap();

        assert_eq!(bundle.files.len(), 2);
        assert_eq!(bundle.files[0].relevance, FileRelevance::Direct);
        assert_eq!(bundle.files[1].relevance, FileRelevance::Coupled);
    }

    #[test]
    fn test_chunks_sorted_by_start_line_within_file() {
        let config = ContextConfig {
            budget_lines: 500,
            depth: 0,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.5,
            rrf_k: 60.0,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            bridge_mode: BridgeMode::Inject,
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            ..ContextConfig::default()
        };

        let seeds = vec![
            make_seed("c2", "a.rs", 20, 30, 0.8),
            make_seed("c1", "a.rs", 1, 10, 0.9),
        ];

        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.files[0].chunks[0].start_line, 1);
        assert_eq!(bundle.files[0].chunks[1].start_line, 20);
    }

    #[test]
    fn test_max_chunk_cap_at_50_percent() {
        let config = ContextConfig {
            budget_lines: 20,
            depth: 0,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.5,
            rrf_k: 60.0,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            bridge_mode: BridgeMode::Inject,
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            ..ContextConfig::default()
        };

        // A chunk of 15 lines with budget of 20 - capped at 10 (50%)
        let seeds = vec![make_seed("c1", "a.rs", 1, 15, 0.9)];

        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();
        // The chunk uses min(15, 10) = 10 lines of budget
        assert_eq!(bundle.budget.used_lines, 10);
    }

    #[test]
    fn test_commit_chunks_excluded_from_assembly() {
        let config = ContextConfig {
            budget_lines: 500,
            depth: 0,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.5,
            rrf_k: 60.0,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            bridge_mode: BridgeMode::Inject,
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            ..ContextConfig::default()
        };

        // Mix of commit and function seeds — commits should be excluded
        let seeds = vec![
            SeedResult {
                chunk_id: "commit:abc1234".to_string(),
                file_path: "git:abc1234".to_string(),
                language: "git".to_string(),
                name: Some("fix: something".to_string()),
                chunk_type: ChunkType::Commit,
                indexed_at: None,
                start_line: 0,
                end_line: 0,
                content: "fix: something\n\nAuthor: dev\nFiles: a.rs".to_string(),
                score: 0.95,
                match_type: Some(MatchType::Semantic),
                repo: None,
                tags: String::new(),
                is_pinned: false,
            },
            make_seed("c1", "a.rs", 1, 5, 0.9),
        ];

        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();

        // Only the function chunk should appear, not the commit
        assert_eq!(bundle.summary.total_chunks, 1);
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.files[0].path, "a.rs");
    }

    fn make_seed(id: &str, file: &str, start: u32, end: u32, score: f32) -> SeedResult {
        SeedResult {
            chunk_id: id.to_string(),
            file_path: file.to_string(),
            language: "rust".to_string(),
            name: Some(format!("fn_{}", id)),
            chunk_type: ChunkType::Function,
            indexed_at: None,
            start_line: start,
            end_line: end,
            content: "fn test() {}".to_string(),
            score,
            match_type: Some(MatchType::Hybrid),
            repo: None,
            tags: String::new(),
            is_pinned: false,
        }
    }

    fn make_coupled(
        id: &str,
        file: &str,
        start: u32,
        end: u32,
        coupling_score: f32,
        coupled_to: &str,
    ) -> CoupledChunkInfo {
        CoupledChunkInfo {
            chunk_id: id.to_string(),
            file_path: file.to_string(),
            language: "rust".to_string(),
            name: Some(format!("fn_{}", id)),
            chunk_type: ChunkType::Function,
            start_line: start,
            end_line: end,
            content: "fn test() {}".to_string(),
            coupling_score,
            coupled_to: coupled_to.to_string(),
        }
    }

    fn make_neighbor(id: &str, file: &str, start: u32, end: u32, via: &str) -> NeighborChunkInfo {
        NeighborChunkInfo {
            chunk_id: id.to_string(),
            file_path: file.to_string(),
            language: "markdown".to_string(),
            name: Some(format!("sec_{}", id)),
            chunk_type: ChunkType::Section,
            start_line: start,
            end_line: end,
            content: "neighbor body\n".to_string(),
            neighbor_score: 0.3,
            discovered_via: via.to_string(),
        }
    }

    #[test]
    fn test_structural_neighbors_injected_under_own_budget() {
        let config = ContextConfig {
            budget_lines: 100,
            neighbor_budget_pct: 10.0, // 10 lines for neighbors
            ..Default::default()
        };
        let seeds = vec![make_seed("s1", "docs/guide.md", 1, 5, 0.9)];
        let neighbors = vec![
            make_neighbor("n1", "docs/guide.md", 6, 9, "follows sec_s1"), // 4 lines — fits
            make_neighbor("n2", "docs/guide.md", 10, 40, "parent of sec_s1"), // 31 lines — over
        ];
        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], neighbors).unwrap();

        assert_eq!(bundle.summary.structural_additions, 1);
        let structural: Vec<_> = bundle
            .files
            .iter()
            .filter(|f| f.relevance == FileRelevance::Structural)
            .collect();
        assert_eq!(structural.len(), 1);
        // Only the fitting chunk was injected; provenance lists the file's sources.
        assert_eq!(structural[0].chunks.len(), 1);
        assert!(structural[0]
            .coupled_to
            .contains(&"follows sec_s1".to_string()));
        // Chunk identity is exposed so an agent can keep following edges.
        assert_eq!(structural[0].chunks[0].id, "n1");
        // A neighbor already injected as a seed is never duplicated.
        let seed_dup = vec![make_neighbor("s1", "docs/guide.md", 1, 5, "follows x")];
        let config2 = ContextConfig {
            budget_lines: 100,
            neighbor_budget_pct: 50.0,
            ..Default::default()
        };
        let seeds2 = vec![make_seed("s1", "docs/guide.md", 1, 5, 0.9)];
        let bundle2 =
            assemble_bundle("test", &config2, seeds2, vec![], vec![], vec![], seed_dup).unwrap();
        assert_eq!(bundle2.summary.structural_additions, 0);
    }

    #[test]
    fn test_neighbor_budget_zero_disables_leg() {
        let config = ContextConfig {
            budget_lines: 100,
            neighbor_budget_pct: 0.0,
            ..Default::default()
        };
        let seeds = vec![make_seed("s1", "docs/guide.md", 1, 5, 0.9)];
        let neighbors = vec![make_neighbor("n1", "docs/guide.md", 6, 9, "follows sec_s1")];
        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], neighbors).unwrap();
        assert_eq!(bundle.summary.structural_additions, 0);
        assert!(bundle
            .files
            .iter()
            .all(|f| f.relevance != FileRelevance::Structural));
    }

    #[test]
    fn test_parse_commit_files() {
        let content = "fix: handle edge case\n\nAuthor: dev\nDate: 2024-01-01\n\nFiles changed:\nsrc/main.rs\nsrc/lib.rs\nREADME.md";
        let files = parse_commit_files(content);
        assert_eq!(files, vec!["src/main.rs", "src/lib.rs", "README.md"]);
    }

    #[test]
    fn test_parse_commit_files_no_files_section() {
        let content = "fix: handle edge case\n\nAuthor: dev\nDate: 2024-01-01";
        let files = parse_commit_files(content);
        assert!(files.is_empty());
    }

    #[test]
    fn test_bridge_mode_roundtrip() {
        for mode in [
            BridgeMode::Off,
            BridgeMode::Inject,
            BridgeMode::Boost,
            BridgeMode::BoostInject,
        ] {
            let s = mode.to_string();
            let parsed: BridgeMode = s.parse().unwrap();
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn test_pinned_chunks_injected_first() {
        use crate::tags::{TagEffect, TagsConfig};

        let mut effects = std::collections::HashMap::new();
        effects.insert(
            "user:pin".to_string(),
            TagEffect {
                pin: true,
                budget_reserve: 20,
                ..Default::default()
            },
        );
        let tags_config = TagsConfig {
            effects,
            ..Default::default()
        };

        let config = ContextConfig {
            budget_lines: 100,
            depth: 0,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.5,
            rrf_k: 60.0,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            bridge_mode: BridgeMode::Inject,
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: Some(tags_config),
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            ..ContextConfig::default()
        };

        let mut pinned = make_seed("p1", "critical.rs", 1, 5, 0.5);
        pinned.tags = "user:pin".to_string();
        pinned.is_pinned = true;

        let normal = make_seed("n1", "normal.rs", 1, 10, 0.9);

        let seeds = vec![normal, pinned];
        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();

        // Pinned file should appear first despite lower score
        assert_eq!(bundle.files.len(), 2);
        assert_eq!(bundle.files[0].relevance, FileRelevance::Pinned);
        assert_eq!(bundle.files[0].path, "critical.rs");
        assert_eq!(bundle.files[1].relevance, FileRelevance::Direct);
        assert_eq!(bundle.files[1].path, "normal.rs");

        // Budget tracking
        assert_eq!(bundle.summary.pinned_chunks, 1);
        assert!(bundle.budget.pinned_lines > 0);
    }

    #[test]
    fn test_pinned_chunks_respect_reserved_budget() {
        use crate::tags::{TagEffect, TagsConfig};

        let mut effects = std::collections::HashMap::new();
        effects.insert(
            "user:pin".to_string(),
            TagEffect {
                pin: true,
                budget_reserve: 10,
                ..Default::default()
            },
        );
        let tags_config = TagsConfig {
            effects,
            ..Default::default()
        };

        let config = ContextConfig {
            budget_lines: 100,
            depth: 0,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.5,
            rrf_k: 60.0,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            bridge_mode: BridgeMode::Inject,
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: Some(tags_config),
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            ..ContextConfig::default()
        };

        // Pinned chunk of 15 lines but budget_reserve is 10 — should not fit
        let mut pinned = make_seed("p1", "big.rs", 1, 15, 0.8);
        pinned.tags = "user:pin".to_string();
        pinned.is_pinned = true;

        let seeds = vec![pinned];
        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();

        // 15 lines exceeds 10-line pin budget reserve → not injected
        assert_eq!(bundle.summary.pinned_chunks, 0);
        assert_eq!(bundle.budget.pinned_lines, 0);
    }

    #[test]
    fn test_no_pins_no_impact() {
        let config = ContextConfig {
            budget_lines: 100,
            depth: 0,
            max_coupled: 3,
            coupling_threshold: 0.1,
            semantic_weight: 0.7,
            content_mode: ContentMode::Full,
            search_limit: 20,
            doc_demotion: 0.5,
            rrf_k: 60.0,
            recency_half_life_days: 0.0,
            recency_weight: 0.0,
            bridge_mode: BridgeMode::Inject,
            bridge_boost_factor: 0.3,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: vec![],
            repo_affinity: None,
            repo_affinity_boost: 2.0,
            ppr_weight: 0.0,
            max_bridged_files: 3,
            max_bridged_chunks_per_file: 2,
            repo_path_prefix: None,
            ..ContextConfig::default()
        };

        let seeds = vec![
            make_seed("c1", "a.rs", 1, 5, 0.9),
            make_seed("c2", "b.rs", 1, 5, 0.7),
        ];
        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();

        assert_eq!(bundle.summary.pinned_chunks, 0);
        assert_eq!(bundle.budget.pinned_lines, 0);
        assert_eq!(bundle.files.len(), 2);
        assert!(bundle
            .files
            .iter()
            .all(|f| f.relevance == FileRelevance::Direct));
    }

    #[test]
    fn test_feedback_boost_reranks_results() {
        // Two chunks with equal scores — feedback should reorder them
        let mut feedback_scores = HashMap::new();
        feedback_scores.insert("b.rs".to_string(), 1.0); // Boost b.rs

        let config = ContextConfig {
            budget_lines: 20,
            feedback_scores: Some(feedback_scores),
            feedback_boost_weight: 0.5,
            feedback_boost_max: 0.5,
            ..ContextConfig::default()
        };

        let seeds = vec![
            make_seed("c1", "a.rs", 1, 5, 0.8),
            make_seed("c2", "b.rs", 1, 5, 0.8), // Same score as a.rs
        ];

        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();
        assert_eq!(bundle.files.len(), 2);
        // b.rs should be first (higher score after boost)
        assert_eq!(bundle.files[0].path, "b.rs");
    }

    #[test]
    fn test_feedback_penalty_demotes_results() {
        let mut feedback_scores = HashMap::new();
        feedback_scores.insert("a.rs".to_string(), -1.0); // Penalize a.rs

        let config = ContextConfig {
            budget_lines: 20,
            feedback_scores: Some(feedback_scores),
            feedback_boost_weight: 0.3,
            feedback_boost_max: 0.3,
            ..ContextConfig::default()
        };

        let seeds = vec![
            make_seed("c1", "a.rs", 1, 5, 0.9), // Higher base score but penalized
            make_seed("c2", "b.rs", 1, 5, 0.8),
        ];

        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();
        assert_eq!(bundle.files.len(), 2);
        // b.rs should now be first despite lower base score
        assert_eq!(bundle.files[0].path, "b.rs");
    }

    #[test]
    fn test_feedback_none_no_effect() {
        let config = ContextConfig {
            budget_lines: 20,
            feedback_scores: None, // No feedback
            ..ContextConfig::default()
        };

        let seeds = vec![
            make_seed("c1", "a.rs", 1, 5, 0.9),
            make_seed("c2", "b.rs", 1, 5, 0.8),
        ];

        let bundle =
            assemble_bundle("test", &config, seeds, vec![], vec![], vec![], vec![]).unwrap();
        assert_eq!(bundle.files.len(), 2);
        // Original order preserved
        assert_eq!(bundle.files[0].path, "a.rs");
    }
}

#[cfg(test)]
#[path = "context_content_tests.rs"]
mod content_tests;
