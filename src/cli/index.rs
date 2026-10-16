#[path = "index_encoding.rs"]
mod encoding;
use encoding::read_indexable_content;

use anyhow::{bail, Context, Result};
use colored::Colorize;
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::OutputConfig;
use crate::config::{Config, ContextualEmbeddingConfig};
use crate::index::{beads, embedder, resolver, Embedder, Parser};
use crate::storage::{MetadataStore, VectorStore};
use crate::types::{Chunk, ImportDependency, ImportEdge};

mod args;
pub use args::IndexArgs;
mod maintenance;
pub use maintenance::MaintenanceMode;
use maintenance::{maintenance_lock_wait, Decision, MaintenanceReport};
#[cfg(feature = "knowledge")]
mod graph_push;
mod profile;
use profile::ProfileStats;

/// The repo key for bead-issue chunks and their file hashes. Distinct from any
/// source repo name so bead state never collides with a source repo that
/// happens to be named "beads"; shared across index runs because the bead
/// corpus is global, not per source repo.
pub(crate) const BEADS_HASH_REPO: &str = "beads-issues";

/// The repo key for archive-record chunks and their content hashes. Same
/// rationale as [`BEADS_HASH_REPO`]: the archive corpus is global (configured
/// sources, not files of any one source repo), so its rows and hash
/// bookkeeping must not collide with per-repo incremental state.
const ARCHIVE_HASH_REPO: &str = "archive-records";

#[derive(Serialize)]
struct IndexOutput {
    status: String,
    files_indexed: usize,
    chunks_created: usize,
    deleted_files: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_files: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_chunks: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    imports_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    imports_resolved: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    imports_unresolved: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commits_indexed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    beads_indexed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sql_indexed: Option<usize>,
    /// Archive seam report (bobbin-d5e): present whenever the archive source
    /// ran, even at 0, so a consumer can tell "nothing re-embedded" from
    /// "archives disabled".
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_indexed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_unchanged: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_removed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<usize>,
    /// What the prune/compact step actually did — `"prune=ran compact=ran"` or
    /// `"prune=skipped_lock_held compact=skipped_lock_held"`. Present so a
    /// consumer of `--json` can tell a sweep that reclaimed from one that was
    /// starved; they used to be identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    maintenance: Option<String>,
}

/// Result of indexing a single file
struct FileIndexResult {
    path: String,
    hash: String,
    chunks: Vec<Chunk>,
    /// Context-enriched text for each chunk (None = use chunk content directly)
    contexts: Vec<Option<String>>,
}

pub async fn run(args: IndexArgs, output: OutputConfig) -> Result<()> {
    let start_time = Instant::now();

    let repo_root = args
        .path
        .canonicalize()
        .with_context(|| format!("Invalid path: {}", args.path.display()))?;

    let config_path = Config::config_path(&repo_root);
    if !config_path.exists() {
        bail!("{}", super::not_initialized_error(&repo_root));
    }

    let mut config = Config::load(&config_path).with_context(|| "Failed to load configuration")?;
    if args.no_quipu_publish {
        (config.quipu_push_chunks, config.quipu_push_inferred) = (false, false);
    }
    args::apply_include(&mut config, &args.include_globs)?;

    // Load tags config for tag resolution during indexing
    let tags_config =
        crate::tags::TagsConfig::load_or_default(&crate::tags::TagsConfig::tags_path(&repo_root));

    // Load feedback auto-tags (feedback:hot, feedback:cold) from feedback store
    let feedback_tags = {
        let feedback_db = Config::feedback_db_path(&repo_root);
        if feedback_db.exists() {
            crate::storage::feedback::FeedbackStore::open(&feedback_db)
                .and_then(|store| store.get_feedback_tags())
                .unwrap_or_default()
        } else {
            HashMap::new()
        }
    };

    // Source directory: --source overrides the default (which is the bobbin home path)
    let source_root = if let Some(ref source) = args.source {
        source
            .canonicalize()
            .with_context(|| format!("Invalid source path: {}", source.display()))?
    } else {
        repo_root.clone()
    };

    // Repo name: explicit --repo > auto-detect from source directory name
    let repo_name = if let Some(ref name) = args.repo {
        name.as_str()
    } else {
        source_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default")
    };

    let db_path = Config::db_path(&repo_root);
    let lance_path = Config::lance_path(&repo_root);
    let model_dir = Config::model_cache_dir()?;

    if output.verbose && !output.quiet && !output.json {
        println!("  Checking embedding model...");
    }
    embedder::ensure_model_for_config(&model_dir, &config.embedding)
        .await
        .context("Failed to ensure embedding model is available")?;

    // Determine embedding dimension
    let embedding_dim = embedder::resolve_dimension(&config.embedding)?;

    // Open storage
    let metadata_store = MetadataStore::open(&db_path).context("Failed to open metadata store")?;
    let mut vector_store = VectorStore::open_with_dim(&lance_path, embedding_dim as i32)
        .await
        .context("Failed to open vector store")?;

    // Check for model change and migration
    let current_model = config.embedding.model.as_str();
    let stored_model = metadata_store.get_meta("embedding_model")?;

    if let Some(stored) = stored_model {
        if stored != current_model {
            if !output.quiet && !output.json {
                println!(
                    "{} Embedding model changed from {} to {}. Re-indexing...",
                    "!".yellow(),
                    stored,
                    current_model
                );
            }

            // Re-create vector store (wipe all data)
            drop(vector_store);
            if lance_path.exists() {
                std::fs::remove_dir_all(&lance_path).with_context(|| {
                    format!("Failed to remove vector store at {}", lance_path.display())
                })?;
            }
            vector_store = VectorStore::open_with_dim(&lance_path, embedding_dim as i32)
                .await
                .context("Failed to re-open vector store")?;
        }
    }

    metadata_store.set_meta("embedding_model", current_model)?;

    // Store repo → source path mapping for calibrate and other commands
    metadata_store.set_meta(
        &format!("repo_source:{}", repo_name),
        &source_root.to_string_lossy(),
    )?;

    let embed = Embedder::from_config(&config.embedding, &model_dir)
        .context("Failed to load embedding model")?;
    let mut parser = Parser::new()
        .context("Failed to initialize parser")?
        .with_chunking(
            config.index.chunk_size,
            config.index.chunk_overlap,
            embed.max_seq().unwrap_or(0),
        );

    // When forcing, delete all existing chunks for this repo first to prevent
    // unbounded LanceDB growth (each --force reindex was duplicating all data).
    if args.force {
        vector_store.delete_by_repo(repo_name).await?;
    }

    // Get existing indexed files from LanceDB (filtered by repo)
    let existing_files: HashSet<String> = if args.force {
        HashSet::new()
    } else {
        vector_store
            .get_all_file_paths(Some(repo_name))
            .await?
            .into_iter()
            .collect()
    };

    let files_to_index = collect_files(&source_root, &config)?;

    if output.verbose && !output.quiet && !output.json {
        println!("  Found {} files matching patterns", files_to_index.len());
    }

    // Track files that no longer exist (for cleanup)
    let current_files: HashSet<String> = files_to_index
        .iter()
        .map(|p| {
            p.strip_prefix(&source_root)
                .unwrap_or(p)
                .to_string_lossy()
                .to_string()
        })
        .collect();

    // `git:` rows are commit chunks — they are never files on disk, so without
    // the filter every incremental run judged the whole commit corpus
    // "deleted", removed it, and the since-watermark commit pass then had
    // nothing to re-embed: the corpus silently eroded to the newest run's
    // commits (the commit-search corpus damage).
    let deleted_files: Vec<String> = existing_files
        .difference(&current_files)
        .filter(|p| !p.starts_with("git:"))
        .cloned()
        .collect();

    // Clean up deleted files
    if !deleted_files.is_empty() {
        if output.verbose && !output.quiet && !output.json {
            println!("  Cleaning up {} deleted files...", deleted_files.len());
        }
        vector_store
            .delete_by_file(&deleted_files, Some(repo_name))
            .await?;
        metadata_store.delete_file_hashes(Some(repo_name), &deleted_files)?;
        // Also clear import dependencies and chunk edges for deleted files
        if config.dependencies.enabled {
            for file in &deleted_files {
                vector_store.clear_file_dependencies(file).await?;
                vector_store
                    .clear_file_chunk_edges(file, Some(repo_name))
                    .await?;
            }
        }
    }

    // When forcing, clear this repo's SQLite file hashes so everything gets
    // re-indexed (scoped: --force on one repo must not wipe the others').
    if args.force {
        metadata_store.clear_file_hashes(repo_name)?;
    }

    // Filter files that need indexing (incremental by default)
    let mut files_needing_index = Vec::new();
    let mut skipped_count = 0usize;

    for file_path in &files_to_index {
        let rel_path = file_path
            .strip_prefix(&source_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        if !args.force {
            // Unreadable (I/O or failed extraction): the indexing loop records it in
            // `errors` as under --force, instead of aborting the run (aegis-1v555n.1).
            if let Ok(content) = read_indexable_content(file_path, &config) {
                let stored = metadata_store.get_file_hash(repo_name, &rel_path)?;
                if stored.as_deref() == Some(compute_hash(&content).as_str()) {
                    skipped_count += 1;
                    continue;
                }
            }
        }

        files_needing_index.push(file_path.clone());
    }

    if output.verbose && !output.quiet && !output.json && skipped_count > 0 {
        println!("  Skipping {} unchanged files", skipped_count);
    }

    let total_files = files_needing_index.len();

    // Non-file sources (beads, commits, archives, SQL) can have work even with 0
    // changed source files (a dedicated `--include-beads` pass, or a no-change
    // incremental). Compute those flags up front so the up-to-date fast path only
    // fires when there is genuinely nothing else to do; otherwise fall through
    // (the file-embedding loops below are no-ops at 0 files) so the other sources
    // still index. See bo-f61; archives/SQL joined the gate with bobbin-d5e.
    let commits_enabled = config.git.commits_enabled;
    let include_beads =
        (args.include_beads || config.beads.enabled) && !config.beads.databases.is_empty();
    let archive_enabled = config.archive.enabled && !config.archive.sources.is_empty();
    let sql_enabled = config.sql.enabled && !config.sql.sources.is_empty();

    if total_files == 0
        && !commits_enabled
        && !include_beads
        && !archive_enabled
        && !sql_enabled
        && !(cfg!(feature = "knowledge") && config.quipu_push_chunks && !current_files.is_empty())
    {
        if output.json {
            let json_output = IndexOutput {
                status: "up_to_date".to_string(),
                files_indexed: 0,
                chunks_created: 0,
                deleted_files: deleted_files.len(),
                total_files: None,
                total_chunks: None,
                imports_total: None,
                imports_resolved: None,
                imports_unresolved: None,
                commits_indexed: None,
                beads_indexed: None,
                sql_indexed: None,
                archive_indexed: None,
                archive_unchanged: None,
                archive_removed: None,
                elapsed_ms: None,
                errors: None,
                // This early return never reaches the maintenance step.
                maintenance: None,
            };
            println!("{}", serde_json::to_string_pretty(&json_output)?);
        } else if !output.quiet {
            println!("{} Index is up to date", "✓".green());
        }
        return Ok(());
    }

    let progress = if !output.quiet && !output.json {
        let pb = ProgressBar::new(total_files as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
                )
                .unwrap()
                .progress_chars("#>-"),
        );
        Some(pb)
    } else {
        None
    };

    let mut indexed_files = 0;
    let mut total_chunks = 0;
    let mut errors = Vec::new();
    let mut profile = ProfileStats::default();
    let emit_progress = output.verbose && !std::io::IsTerminal::is_terminal(&std::io::stderr());

    // Adaptive batch size: GPU benefits from larger batches for throughput.
    let batch_size = if embed.is_gpu() {
        let gpu_batch = config.embedding.batch_size * 4;
        if output.verbose && !output.quiet && !output.json {
            println!(
                "  GPU detected — batch size {} ({}×4)",
                gpu_batch, config.embedding.batch_size
            );
        }
        gpu_batch
    } else {
        config.embedding.batch_size
    };
    // Build file → last-commit-timestamp map for recency tracking.
    // Falls back to empty map (all files get now()) if not in a git repo.
    let file_timestamps: HashMap<String, i64> =
        match crate::index::git::GitAnalyzer::new(&source_root) {
            Ok(analyzer) => analyzer.get_file_last_modified().unwrap_or_default(),
            Err(_) => HashMap::new(),
        };

    let mut pending_results: Vec<FileIndexResult> = Vec::new();
    let mut all_imports: Vec<ImportEdge> = Vec::new();
    let mut all_chunk_edges: Vec<crate::types::ChunkEdge> = Vec::new();
    // Content-free chunk copies for graph/entity derivation (identity
    // coordinates only — never bytes).
    let mut all_slim_chunks: Vec<Chunk> = Vec::new();
    let collect_slim_chunks =
        config.index.entities || (cfg!(feature = "knowledge") && config.quipu_push_chunks);
    // Inferred-track extraction (W3.B) runs INSIDE the file loop, against
    // full chunk content — the slim copies above are content-free by design,
    // and prose extraction needs the prose. Only the small candidate set is
    // accumulated.
    #[cfg(feature = "knowledge")]
    let inferred_extractor = crate::knowledge::inferred::BacktickCoderefExtractor::default();
    #[cfg(feature = "knowledge")]
    let mut inferred_extraction = crate::knowledge::inferred::Extraction::default();
    // Every re-parsed file gets its stored edges cleared, whether or not it
    // emits edges this run — a file whose edges all disappeared must not
    // keep its stale rows (keyed off emitted edges, it would).
    let mut edge_clear_files: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file_path in &files_needing_index {
        let rel_path = file_path
            .strip_prefix(&source_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let t_read = Instant::now();
        let content = match read_indexable_content(file_path, &config) {
            Ok(c) => c,
            Err(e) => {
                errors.push((rel_path.clone(), e.to_string()));
                if let Some(pb) = &progress {
                    pb.inc(1);
                }
                continue;
            }
        };
        profile.file_read_ms += t_read.elapsed().as_millis();

        if content.trim().is_empty() {
            if let Some(pb) = &progress {
                pb.inc(1);
            }
            continue;
        }

        let t_parse = Instant::now();
        // Parse under the REPO-RELATIVE path: the parser stamps every chunk's
        // file_path with the path it is handed, and that value is the key the
        // cleanup sweep, delete_by_file and needs_reindex later compare against
        // rel-path sets. Handing it the absolute walk path stored
        // absolute-keyed rows that never matched — one incremental run later
        // the sweep judged every row "deleted", removed them, and the
        // unchanged content hash then skipped re-embedding forever
        // (silently unsearchable behind '✓ Indexed').
        let mut chunks = match parser.parse_file(Path::new(&rel_path), &content) {
            Ok(c) => c,
            Err(e) => {
                errors.push((rel_path.clone(), format!("Parse error: {}", e)));
                if let Some(pb) = &progress {
                    pb.inc(1);
                }
                continue;
            }
        };
        profile.parse_ms += t_parse.elapsed().as_millis();

        // Extract imports from this file (if dependency tracking enabled)
        if config.dependencies.enabled {
            let file_imports = parser.extract_imports(file_path, &content);
            for mut imp in file_imports {
                // Normalize source path to relative
                imp.source_file = rel_path.clone();
                all_imports.push(imp);
            }
        }

        if chunks.is_empty() {
            if let Some(pb) = &progress {
                pb.inc(1);
            }
            continue;
        }

        let hash = compute_hash(&content);

        // Resolve tags for each chunk: convention + pattern + frontmatter + comments
        crate::tags::resolve_tags_for_chunks(
            &tags_config,
            &rel_path,
            Some(repo_name),
            &content,
            &mut chunks,
        );

        // Merge feedback auto-tags (feedback:hot, feedback:cold) from accumulated usage data
        if let Some(ftags) = feedback_tags.get(&rel_path) {
            for chunk in &mut chunks {
                let mut existing: Vec<&str> = if chunk.tags.is_empty() {
                    Vec::new()
                } else {
                    chunk.tags.split(',').collect()
                };
                for ftag in ftags {
                    let s = ftag.as_str();
                    if !existing.contains(&s) {
                        existing.push(ftag);
                    }
                }
                existing.sort();
                chunk.tags = existing.join(",");
            }
        }

        // Extract chunk-level relationship edges (implements, extends,
        // next_chunk, part_of). Under the REPO-RELATIVE path, for the same
        // reason parse_file above is: edges join against chunk IDs and
        // file_path values that are rel-path based; the absolute walk path
        // stored edges no read ever matched.
        if config.dependencies.enabled {
            let file_edges = parser.extract_chunk_edges(Path::new(&rel_path), &content, &chunks);
            all_chunk_edges.extend(file_edges);
            edge_clear_files.insert(rel_path.clone());
        }

        if collect_slim_chunks {
            all_slim_chunks.extend(chunks.iter().map(|c| {
                let mut slim = c.clone();
                slim.content = String::new();
                slim
            }));
        }

        #[cfg(feature = "knowledge")]
        if config.quipu_push_inferred {
            use crate::knowledge::inferred::InferredExtractor;
            inferred_extraction.merge(inferred_extractor.extract(&chunks, repo_name));
        }

        // Compute contextual embeddings for enabled languages
        let t_ctx = Instant::now();
        let contexts = build_context_windows(&chunks, &content, &config.embedding.context);
        profile.context_ms += t_ctx.elapsed().as_millis();

        pending_results.push(FileIndexResult {
            path: rel_path,
            hash,
            chunks,
            contexts,
        });

        let total_pending_chunks: usize = pending_results.iter().map(|r| r.chunks.len()).sum();
        if total_pending_chunks >= batch_size {
            // Collect hashes before process_batch drains the results
            let batch_hashes: Vec<(String, String)> = pending_results
                .iter()
                .map(|r| (r.path.clone(), r.hash.clone()))
                .collect();

            let (indexed, chunks_count) = process_batch(
                &mut pending_results,
                &mut vector_store,
                &embed,
                repo_name,
                &mut profile,
                &existing_files,
                &file_timestamps,
            )
            .await?;

            // Update SQLite file hashes after successful indexing
            let hash_refs: Vec<(&str, &str)> = batch_hashes
                .iter()
                .map(|(p, h)| (p.as_str(), h.as_str()))
                .collect();
            metadata_store.set_file_hashes_bulk(repo_name, &hash_refs)?;

            indexed_files += indexed;
            total_chunks += chunks_count;

            if let Some(pb) = &progress {
                pb.inc(indexed as u64);
            }
            if emit_progress {
                eprintln!(
                    "progress: {}/{} files ({} chunks)",
                    indexed_files, total_files, total_chunks
                );
            }
        }
    }

    // Process remaining files
    if !pending_results.is_empty() {
        let batch_hashes: Vec<(String, String)> = pending_results
            .iter()
            .map(|r| (r.path.clone(), r.hash.clone()))
            .collect();

        let (indexed, chunks_count) = process_batch(
            &mut pending_results,
            &mut vector_store,
            &embed,
            repo_name,
            &mut profile,
            &existing_files,
            &file_timestamps,
        )
        .await?;

        let hash_refs: Vec<(&str, &str)> = batch_hashes
            .iter()
            .map(|(p, h)| (p.as_str(), h.as_str()))
            .collect();
        metadata_store.set_file_hashes_bulk(repo_name, &hash_refs)?;

        indexed_files += indexed;
        total_chunks += chunks_count;

        if let Some(pb) = &progress {
            pb.inc(indexed as u64);
        }
        if emit_progress {
            eprintln!(
                "progress: {}/{} files ({} chunks)",
                indexed_files, total_files, total_chunks
            );
        }
    }

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    // Analyze and store git coupling if enabled
    let t_coupling = Instant::now();
    if config.git.coupling_enabled {
        // Check if coupling results are cached (HEAD unchanged, same depth)
        let head_hash = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&source_root)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            });

        let depth_str = config.git.coupling_depth.to_string();
        // Watermarks are per-repo: a single global key meant every
        // repo was judged against whatever HEAD the LAST indexed repo wrote —
        // a SHA most repos do not even contain.
        let cached_commit =
            metadata_store.get_meta(&format!("last_coupling_commit:{repo_name}"))?;
        let cached_depth = metadata_store.get_meta(&format!("coupling_depth:{repo_name}"))?;

        let coupling_cached = !args.force
            && head_hash.is_some()
            && cached_commit.as_deref() == head_hash.as_deref()
            && cached_depth.as_deref() == Some(depth_str.as_str());

        if coupling_cached {
            if output.verbose && !output.quiet && !output.json {
                println!("  Git coupling: cached (HEAD unchanged)");
            }
        } else {
            if output.verbose && !output.quiet && !output.json {
                println!("  Analyzing git coupling...");
            }

            match crate::index::git::GitAnalyzer::new(&source_root) {
                Ok(analyzer) => {
                    match analyzer.analyze_coupling(
                        config.git.coupling_depth,
                        config.git.coupling_threshold,
                        config.git.coupling_freq_weight,
                        config.git.coupling_recency_days,
                    ) {
                        Ok(couplings) => {
                            let mut count = 0;
                            metadata_store.begin_transaction()?;
                            for coupling in &couplings {
                                if metadata_store.upsert_coupling(coupling).is_ok() {
                                    count += 1;
                                }
                            }
                            metadata_store.commit()?;

                            if let Some(ref hash) = head_hash {
                                metadata_store
                                    .set_meta(&format!("last_coupling_commit:{repo_name}"), hash)?;
                                metadata_store
                                    .set_meta(&format!("coupling_depth:{repo_name}"), &depth_str)?;
                            }

                            if output.verbose && !output.quiet && !output.json {
                                println!("  Stored {} coupling relations", count);
                            }

                            // Push coupling scores to Quipu as weighted edges
                            #[cfg(feature = "knowledge")]
                            if !couplings.is_empty() {
                                let t_quipu = Instant::now();
                                match crate::knowledge::coupling::push_coupling_to_quipu(
                                    &couplings, repo_name, &repo_root,
                                ) {
                                    Ok((_tx_id, triple_count)) => {
                                        if output.verbose && !output.quiet && !output.json {
                                            println!(
                                                "  Pushed {} coupling triples to Quipu ({}ms)",
                                                triple_count,
                                                t_quipu.elapsed().as_millis()
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        if !output.quiet && !output.json {
                                            println!(
                                                "{} Failed to push coupling to Quipu: {}",
                                                "!".yellow(),
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            if !output.quiet && !output.json {
                                println!("{} Failed to analyze git coupling: {}", "!".yellow(), e);
                            }
                        }
                    }
                }
                Err(_) => {}
            }
        }
    }

    profile.git_coupling_ms = t_coupling.elapsed().as_millis();

    // Cross-repo coupling (bo-oqny): recompute group-scoped edges from the
    // bead histories of every indexed repo in each configured group. Cheap and
    // best-effort — a group only materializes once ≥2 of its repos are indexed,
    // and a failure here must not fail the index of this repo.
    if !config.groups.is_empty() {
        match crate::index::cross_repo::compute_and_store_cross_repo(&metadata_store, &config) {
            Ok(n) => {
                if output.verbose && !output.quiet && !output.json {
                    println!("  Stored {} cross-repo coupling edges", n);
                }
            }
            Err(e) => {
                if !output.quiet && !output.json {
                    println!(
                        "{} Failed to compute cross-repo coupling: {}",
                        "!".yellow(),
                        e
                    );
                }
            }
        }
    }

    // Analyze and store import dependencies
    let t_deps = Instant::now();
    let dep_count: usize = 0;
    let resolved_count: usize = 0;
    if config.dependencies.enabled && !all_imports.is_empty() {
        if output.verbose && !output.quiet && !output.json {
            println!("  Resolving {} import edges...", all_imports.len());
        }

        // Build set of all indexed file paths for resolution
        let all_indexed: HashSet<String> = current_files.clone();
        resolver::resolve_imports(&mut all_imports, &all_indexed, &source_root);

        // Clear old dependencies for re-indexed files and store new ones
        let reindexed_files: Vec<String> = all_imports
            .iter()
            .map(|e| e.source_file.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        for file in &reindexed_files {
            vector_store.clear_file_dependencies(file).await?;
        }
        let mut dep_batch: Vec<ImportDependency> = Vec::new();
        let mut dep_count = 0;
        let mut resolved_count = 0;
        for edge in &all_imports {
            let resolved = edge.resolved_path.is_some();
            dep_batch.push(ImportDependency {
                file_a: edge.source_file.clone(),
                file_b: if let Some(ref rp) = edge.resolved_path {
                    rp.clone()
                } else {
                    format!("unresolved:{}", edge.import_specifier)
                },
                dep_type: "import".to_string(),
                import_statement: edge.import_specifier.clone(),
                symbol: None,
                resolved,
            });
            dep_count += 1;
            if resolved {
                resolved_count += 1;
            }
        }
        vector_store.upsert_dependencies(&dep_batch).await?;

        if output.verbose && !output.quiet && !output.json {
            println!(
                "  Stored {} dependency edges ({} resolved)",
                dep_count, resolved_count
            );
        }
    }

    // Store chunk-level relationship edges. One-time hygiene first: edges
    // were historically stored under the absolute walk path (never matching
    // the rel-path chunk rows), so sweep any legacy absolute-keyed rows.
    vector_store.clear_absolute_path_chunk_edges().await?;
    // Clear before insert for every re-parsed file — including files that
    // emitted zero edges this run.
    for file in &edge_clear_files {
        vector_store
            .clear_file_chunk_edges(file, Some(repo_name))
            .await?;
    }
    if !all_chunk_edges.is_empty() {
        let edge_count = all_chunk_edges.len();
        vector_store
            .upsert_chunk_edges(&all_chunk_edges, repo_name)
            .await?;

        if output.verbose && !output.quiet && !output.json {
            println!("  Stored {} chunk edges", edge_count);
        }
    }

    // Derive deterministic entities from this run's chunks (opt-in, W3.A).
    // Stale entities for files deleted between runs linger until a --force
    // pass; the table is a semantic index over identities, not a ledger.
    if config.index.entities && !all_slim_chunks.is_empty() {
        let entities = crate::index::entities::build_entities(&all_slim_chunks, repo_name);
        if !entities.is_empty() {
            let texts: Vec<&str> = entities.iter().map(|e| e.text.as_str()).collect();
            match embed.embed_batch(&texts).await {
                Ok(embeddings) => {
                    if let Err(e) = vector_store.upsert_entities(&entities, &embeddings).await {
                        if !output.quiet && !output.json {
                            println!("{} Failed to store entities: {}", "!".yellow(), e);
                        }
                    } else if output.verbose && !output.quiet && !output.json {
                        println!("  Stored {} entities", entities.len());
                    }
                }
                Err(e) => {
                    if !output.quiet && !output.json {
                        println!("{} Failed to embed entities: {}", "!".yellow(), e);
                    }
                }
            }
        }
    }

    // Dependency extraction/storage is complete here. Remote graph
    // publication has its own latency and must not masquerade as dependency
    // work in --verbose profiles.
    profile.deps_ms = t_deps.elapsed().as_millis();
    let t_graph_push = Instant::now();

    // Push the chunk graph to quipu as a diffed snapshot (opt-in, W2.P4).
    #[cfg(feature = "knowledge")]
    if config.quipu_push_chunks && !current_files.is_empty() {
        // Repository-wide replacement must include unchanged files.
        let (snapshot_chunks, snapshot_edges) =
            graph_push::snapshot(&vector_store, repo_name, &current_files).await?;
        let pushed = graph_push::publish(
            (&snapshot_chunks, &snapshot_edges),
            repo_name,
            &source_root,
            (config.quipu_endpoint.as_deref(), args.graph.as_deref()),
            output.quiet || output.json,
            &metadata_store,
            args.force || args.force_publish,
        )
        .await;
        match graph_push::require(pushed) {
            Ok((_tx, count)) => {
                if output.verbose && !output.quiet && !output.json {
                    println!("  Pushed {} chunk-graph facts to quipu", count);
                }
                // Resolve mentions; report dangling/ambiguous references.
                match crate::knowledge::mentions::reconcile_mentions_at(&source_root) {
                    Ok(report) => {
                        if output.verbose && !output.quiet && !output.json {
                            println!(
                                "  Mention reconcile: {} resolved ({} new edges), {} dangling, {} ambiguous",
                                report.resolved,
                                report.edges_written,
                                report.dangling,
                                report.ambiguous
                            );
                        }
                    }
                    Err(e) => {
                        if !output.quiet && !output.json {
                            println!("{} Mention reconcile skipped: {}", "!".yellow(), e);
                        }
                    }
                }
            }
            Err(e) => return Err(e),
        }
    }

    // Land inferred-track candidates in the QUARANTINED plane (opt-in, W3.B).
    // The push itself refuses stores that cannot route the write to the
    // registered crew:inferred graph — a silent ROOT landing would serve
    // model-track claims at observed standing.
    #[cfg(feature = "knowledge")]
    if config.quipu_push_inferred {
        let stamped = crate::knowledge::quarantine::QuarantinedFacts::stamp(
            &inferred_extractor,
            &inferred_extraction,
            repo_name,
        );
        match crate::knowledge::quarantine::push_inferred_to_quipu(&stamped, &source_root) {
            Ok((_tx, count)) => {
                if output.verbose && !output.quiet && !output.json {
                    println!("  Pushed {} inferred facts to quarantine plane", count);
                }
            }
            Err(e) => {
                if !output.quiet && !output.json {
                    println!("{} Inferred quarantine push skipped: {:#}", "!".yellow(), e);
                }
            }
        }
    }

    profile.graph_push_ms = t_graph_push.elapsed().as_millis();

    // Index git commits as searchable chunks
    let t_commits = Instant::now();
    let mut commits_indexed: usize = 0;
    if commits_enabled {
        if output.verbose && !output.quiet && !output.json {
            println!("  Indexing git commits...");
        }

        // Commits run through the watermark half of the ChunkSource seam
        // (bobbin-d5e): append-only fetch since the per-repo watermark
        // (`last_indexed_commit:{repo}`), no removal sweep, watermark
        // persisted only after a successful insert. Chunk shape and the
        // `--force` replace behavior are unchanged from the bespoke block.
        if let Ok(analyzer) = crate::index::git::GitAnalyzer::new(&source_root) {
            let source = crate::index::commits::CommitsSource::new(
                analyzer,
                repo_name,
                config.git.commits_depth,
            );

            match crate::index::source::index_watermark_source(
                &source,
                &mut vector_store,
                &metadata_store,
                &embed,
                args.force,
            )
            .await
            {
                Ok(0) => {
                    if output.verbose && !output.quiet && !output.json {
                        println!("  No new commits to index");
                    }
                }
                Ok(indexed) => {
                    commits_indexed = indexed;

                    // Auto-associate beads → commits for workflow telemetry
                    // (GH#9). Only explicit Bead* trailers are recorded; runs
                    // over newly-indexed commits. Git-specific, so it stays
                    // outside the seam.
                    let mut lineage_recorded = 0usize;
                    for entry in source.take_entries() {
                        for bead_id in crate::index::git::extract_bead_refs(&entry.trailers) {
                            if metadata_store
                                .record_bead_lineage(&crate::storage::sqlite::NewBeadLineage {
                                    bead_id,
                                    commit_sha: Some(entry.hash.clone()),
                                    touched_files: entry.files.clone(),
                                    action_type: Some("commit".to_string()),
                                    ..Default::default()
                                })
                                .is_ok()
                            {
                                lineage_recorded += 1;
                            }
                        }
                    }
                    if lineage_recorded > 0 && output.verbose && !output.quiet && !output.json {
                        println!("  Recorded {} bead→commit lineage links", lineage_recorded);
                    }

                    if output.verbose && !output.quiet && !output.json {
                        println!("  Indexed {} commits", commits_indexed);
                    }
                }
                Err(e) => {
                    if !output.quiet && !output.json {
                        use crate::index::source::WatermarkSource;
                        println!("{} Failed to index {}: {}", "!".yellow(), source.name(), e);
                    }
                }
            }
        }
    }

    profile.git_commits_ms = t_commits.elapsed().as_millis();

    let mut beads_indexed: usize = 0;
    if include_beads {
        if !output.quiet && !output.json {
            let src = beads::source_label(&config.beads);
            println!("  Indexing beads from {src}...");
        }

        let mut beads_config = config.beads.clone();
        beads_config.enabled = true;

        // Beads run through the shared ChunkSource seam: content-hash
        // incremental with a removal sweep (the pattern this source
        // originated, now generalized in index::source).
        struct BeadsSource(crate::config::BeadsConfig);
        impl crate::index::source::ChunkSource for BeadsSource {
            fn name(&self) -> &str {
                "beads"
            }
            fn repo_key(&self) -> &str {
                BEADS_HASH_REPO
            }
            fn source_label(&self) -> &str {
                "beads"
            }
            async fn fetch(&self) -> Result<Vec<Chunk>> {
                crate::index::beads::fetch_beads(&self.0).await
            }
        }

        match crate::index::source::index_hashed_source(
            &BeadsSource(beads_config),
            &mut vector_store,
            &metadata_store,
            &embed,
            args.force,
        )
        .await
        {
            Ok(report) => {
                beads_indexed = report.indexed;
                if output.verbose && !output.quiet && !output.json {
                    if report.indexed == 0 && report.unchanged == 0 && report.removed == 0 {
                        println!("  No beads to index");
                    } else if report.indexed == 0 {
                        println!(
                            "  Beads up to date ({} unchanged, {} removed)",
                            report.unchanged, report.removed
                        );
                    } else {
                        println!(
                            "  Indexed {} beads ({} unchanged, {} removed)",
                            report.indexed, report.unchanged, report.removed
                        );
                    }
                }
            }
            Err(e) => {
                if !output.quiet && !output.json {
                    println!("{} Failed to index beads: {}", "!".yellow(), e);
                }
            }
        }
    }

    // Index configured SQL sources (roadmap W4.P2) through the same seam.
    let mut sql_indexed: usize = 0;
    if config.sql.enabled && !config.sql.sources.is_empty() {
        for source_config in &config.sql.sources {
            if !output.quiet && !output.json {
                println!("  Indexing SQL source '{}'...", source_config.name);
            }
            let source = match crate::index::sql::SqlSource::new(source_config) {
                Ok(s) => s,
                Err(e) => {
                    if !output.quiet && !output.json {
                        println!("{} {}", "!".yellow(), e);
                    }
                    continue;
                }
            };
            match crate::index::source::index_hashed_source(
                &source,
                &mut vector_store,
                &metadata_store,
                &embed,
                args.force,
            )
            .await
            {
                Ok(report) => {
                    sql_indexed += report.indexed;
                    if output.verbose && !output.quiet && !output.json {
                        println!(
                            "  SQL '{}': {} indexed, {} unchanged, {} removed",
                            source_config.name, report.indexed, report.unchanged, report.removed
                        );
                    }
                }
                Err(e) => {
                    if !output.quiet && !output.json {
                        println!(
                            "{} SQL source '{}' failed: {}",
                            "!".yellow(),
                            source_config.name,
                            e
                        );
                    }
                }
            }
        }
    }

    // Index archive records if enabled (configured sources).
    //
    // Archives run through the shared ChunkSource seam (bobbin-d5e): content-
    // hash incremental with a removal sweep, replacing the old full-replace
    // block that re-embedded every record on every run and never deleted a
    // record that disappeared. Record ids and chunk shape are unchanged, so
    // the first seam run replaces the legacy rows in place (all hashes are
    // new under ARCHIVE_HASH_REPO, which re-embeds everything once and
    // deletes each old row by id before inserting its replacement).
    let mut archive_report: Option<crate::index::source::SourceIndexReport> = None;
    if archive_enabled {
        if !output.quiet && !output.json {
            println!("  Indexing archives...");
        }

        struct ArchiveSource(crate::config::ArchiveConfig);
        impl crate::index::source::ChunkSource for ArchiveSource {
            fn name(&self) -> &str {
                "archive"
            }
            fn repo_key(&self) -> &str {
                ARCHIVE_HASH_REPO
            }
            fn source_label(&self) -> &str {
                "archive"
            }
            async fn fetch(&self) -> Result<Vec<Chunk>> {
                crate::index::archive::fetch_archive(&self.0)
            }
        }

        let source = ArchiveSource(config.archive.clone());
        match crate::index::source::index_hashed_source(
            &source,
            &mut vector_store,
            &metadata_store,
            &embed,
            args.force,
        )
        .await
        {
            Ok(report) => {
                if output.verbose && !output.quiet && !output.json {
                    if report.indexed == 0 && report.unchanged == 0 && report.removed == 0 {
                        println!("  No archive records to index");
                    } else if report.indexed == 0 {
                        println!(
                            "  Archive up to date ({} unchanged, {} removed)",
                            report.unchanged, report.removed
                        );
                    } else {
                        println!(
                            "  Indexed {} archive records ({} unchanged, {} removed)",
                            report.indexed, report.unchanged, report.removed
                        );
                    }
                }
                archive_report = Some(report);
            }
            Err(e) => {
                if !output.quiet && !output.json {
                    use crate::index::source::ChunkSource;
                    println!("{} Failed to index {}: {}", "!".yellow(), source.name(), e);
                }
            }
        }
    }
    let archive_indexed = archive_report.as_ref().map_or(0, |r| r.indexed);

    let t_compact = Instant::now();
    // PRUNE FIRST, THEN COMPACT. This order is load-bearing, not stylistic.
    //
    // Prune is cheap: it deletes whole version manifests and the fragment files
    // no live version references, without reading vector or text data into RAM.
    // Compact is expensive: it rewrites rows. With compact first, a compaction
    // that OOMs takes the cheap reclaim down with it — prune never runs, disk
    // keeps growing, the next compaction is bigger, and the cycle is
    // self-perpetuating. That is exactly what happened: the nightly reindex was
    // OOM-killed for months, so pruning (its job) never ran, and the store grew
    // to 29G / 1694 fragments / 2537 versions.
    //
    // Pruning first also shrinks the input compaction has to consider.
    //
    // WAIT for the maintenance lock, do not skip on contention. This is the
    // only job that prunes; every other participant that takes this lock is
    // opportunistic and can be deferred without cost, so the scheduled sweep is
    // the one that must not be pre-empted. Before this, a `bobbin status` from
    // the incremental service holding the lock made the whole maintenance step
    // a no-op that still exited 0.
    //
    // ONE acquisition for prune+compact (`maintain`), not one each: the nightly
    // runs an `index` per repo, so a per-operation budget would double the
    // worst-case wait for every one of them, and a separate acquisition leaves
    // a window for a contender to steal the lock between the prune and the
    // compaction it is supposed to precede.
    // `--maintenance auto` lets a frequent incremental refresh skip a sweep the
    // shared record says ran recently (aegis-91w); see `maintenance.rs`.
    let maintenance = match maintenance::decide(
        args.maintenance,
        args.force,
        vector_store.maintenance_status(),
        crate::storage::now_unix(),
        args.maintenance_interval_hours.saturating_mul(3600),
    ) {
        Decision::Run => {
            MaintenanceReport::from_result(vector_store.maintain(maintenance_lock_wait()).await)?
        }
        skip => MaintenanceReport::skipped(skip),
    };
    profile.compact_ms = t_compact.elapsed().as_millis();

    // A starved sweep is reported LOUDLY on stderr even under --quiet/--json.
    // The whole defect was that this step could do nothing and leave no trace
    // anywhere an operator looks; a silent skip is what let the store grow for
    // months while the nightly "ran" every night.
    maintenance.warn_if_starved();

    let elapsed = start_time.elapsed();

    if output.verbose && !output.json {
        profile.print(elapsed);
    }

    // Build import stats for output (only if deps were processed)
    let (imports_total, imports_resolved, imports_unresolved) = if dep_count > 0 {
        (
            Some(dep_count),
            Some(resolved_count),
            Some(dep_count - resolved_count),
        )
    } else {
        (None, None, None)
    };

    if output.json {
        let stats = vector_store.get_stats(Some(repo_name)).await?;
        let json_output = IndexOutput {
            status: "indexed".to_string(),
            files_indexed: indexed_files,
            chunks_created: total_chunks,
            deleted_files: deleted_files.len(),
            total_files: Some(stats.total_files),
            total_chunks: Some(stats.total_chunks),
            imports_total,
            imports_resolved,
            imports_unresolved,
            commits_indexed: if commits_indexed > 0 {
                Some(commits_indexed)
            } else {
                None
            },
            sql_indexed: if sql_indexed > 0 {
                Some(sql_indexed)
            } else {
                None
            },
            beads_indexed: if beads_indexed > 0 {
                Some(beads_indexed)
            } else {
                None
            },
            archive_indexed: archive_report.as_ref().map(|r| r.indexed),
            archive_unchanged: archive_report.as_ref().map(|r| r.unchanged),
            archive_removed: archive_report.as_ref().map(|r| r.removed),
            elapsed_ms: Some(elapsed.as_millis()),
            errors: Some(errors.len()),
            maintenance: Some(maintenance.json_label()),
        };
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else if !output.quiet {
        println!(
            "{} Indexed {} files ({} chunks) in {:.2}s",
            "✓".green(),
            indexed_files,
            total_chunks,
            elapsed.as_secs_f64()
        );

        if commits_indexed > 0 {
            println!("  Commits: {} indexed for semantic search", commits_indexed);
        }

        if beads_indexed > 0 {
            println!("  Beads: {} indexed", beads_indexed);
        }
        if sql_indexed > 0 {
            println!("  SQL: {} rows indexed", sql_indexed);
        }
        if archive_indexed > 0 {
            println!("  Archive: {} records indexed", archive_indexed);
        }

        if dep_count > 0 {
            println!(
                "  Imports: {} total, {} resolved, {} unresolved",
                dep_count,
                resolved_count,
                dep_count - resolved_count
            );
        }

        if !deleted_files.is_empty() {
            println!("  Cleaned up {} deleted files", deleted_files.len());
        }

        if !errors.is_empty() {
            println!("\n{} {} files had errors:", "!".yellow(), errors.len());
            for (path, err) in errors.iter().take(5) {
                println!("  {}: {}", path, err);
            }
            if errors.len() > 5 {
                println!("  ... and {} more", errors.len() - 5);
            }
        }

        // Tag metrics
        if let Ok((tagged, untagged)) = vector_store.count_tagged_chunks().await {
            if tagged > 0 {
                println!("  Tags: {} tagged, {} untagged chunks", tagged, untagged);
            }
        }

        if output.verbose {
            let stats = vector_store.get_stats(Some(repo_name)).await?;
            println!("\nIndex statistics:");
            println!("  Total files:  {}", stats.total_files);
            println!("  Total chunks: {}", stats.total_chunks);
            for lang in &stats.languages {
                println!(
                    "  {}: {} files, {} chunks",
                    lang.language, lang.file_count, lang.chunk_count
                );
            }
        }
    }

    // Auto-calibrate if needed (unless --skip-calibrate)
    if !args.skip_calibrate {
        use super::calibrate::{
            load_calibration, CalibrateArgs, CalibrationGuard, DefaultCalibrationGuard,
        };

        // Calibration state (calibration.json) lives in the bobbin HOME's .bobbin data
        // dir — read it from repo_root, NOT source_root. Reading from source_root (no
        // .bobbin there) always returned None, so the guard recalibrated EVERY run
        // (bobbin-ewtu2 defect 1a).
        let calibration = load_calibration(&repo_root);
        // Capture a lightweight snapshot for the guard check
        let guard_snapshot = super::calibrate::capture_snapshot_from_index(
            vector_store.count().await.unwrap_or(0) as usize,
        );
        let guard = DefaultCalibrationGuard;
        if guard.should_recalibrate(&guard_snapshot, calibration.as_ref()) {
            if !output.quiet && !output.json {
                eprintln!("  Calibrating search parameters...");
            }
            // path=repo_root (home) so calibrate's init check finds .bobbin/config.toml;
            // source=source_root so it samples the indexed git tree; repo carries the
            // multi-repo name. Passing source_root as path was bobbin-ewtu2 defect 1b.
            let cal_args = CalibrateArgs::default_for_auto(
                repo_root.clone(),
                args.repo.clone(),
                Some(source_root.clone()),
            );
            // Use quiet output — calibration is a background step, not the primary command
            let cal_output = OutputConfig {
                json: false,
                quiet: true,
                verbose: false,
                server: None,
                role: "default".to_string(),
            };
            if let Err(e) = super::calibrate::run(cal_args, cal_output).await {
                if !output.quiet && !output.json {
                    eprintln!("  {} Auto-calibration failed: {}", "!".yellow(), e);
                }
            }
        } else if !output.quiet && !output.json && output.verbose {
            eprintln!("  Calibration: skipped (no significant changes)");
        }
    }

    Ok(())
}

/// Collect all files to index based on configuration patterns
pub(crate) fn collect_files(repo_root: &Path, config: &Config) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    let mut include_globs: Vec<String> = config.index.include.clone();
    if config.index.multimodal {
        // Opt-in: also walk multimodal file types (PDFs) so the extractor can
        // turn them into searchable text. Kept separate from the default
        // include list so toggling the flag is the only knob users need.
        for ext in crate::index::multimodal::MULTIMODAL_EXTENSIONS {
            include_globs.push(format!("**/*.{ext}"));
        }
    }
    if config.index.documents {
        // Same opt-in shape as multimodal: toggling the flag is the only
        // knob, no include-pattern editing required.
        for ext in crate::index::documents::DOCUMENT_EXTENSIONS {
            include_globs.push(format!("**/*.{ext}"));
        }
    }
    let include_patterns: Vec<glob::Pattern> = include_globs
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    let exclude_patterns: Vec<glob::Pattern> = config
        .index
        .exclude
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    let mut builder = WalkBuilder::new(repo_root);
    builder
        .hidden(true)
        .git_ignore(config.index.use_gitignore)
        .git_global(config.index.use_gitignore)
        .git_exclude(config.index.use_gitignore);

    for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(true) {
            continue;
        }

        let path = entry.path();
        let rel_path = path
            .strip_prefix(repo_root)
            .unwrap_or(path)
            .to_string_lossy();

        let excluded = exclude_patterns.iter().any(|p| p.matches(&rel_path));
        if excluded {
            continue;
        }

        let included = include_patterns.iter().any(|p| p.matches(&rel_path));

        if included {
            files.push(path.to_path_buf());
        }
    }

    Ok(files)
}

/// Process a batch of files: generate embeddings and store in LanceDB.
///
/// Collects all chunks across files into one big embedding batch to minimize
/// ONNX session overhead, then splits results back to per-file for storage.
async fn process_batch(
    results: &mut Vec<FileIndexResult>,
    vector_store: &mut VectorStore,
    embed: &Embedder,
    repo: &str,
    profile: &mut ProfileStats,
    existing_files: &HashSet<String>,
    file_timestamps: &HashMap<String, i64>,
) -> Result<(usize, usize)> {
    if results.is_empty() {
        return Ok((0, 0));
    }

    let fallback_ts = chrono::Utc::now().timestamp();

    // Collect all embed texts across files into one batch
    let mut all_texts: Vec<String> = Vec::new();
    let mut file_chunk_counts: Vec<usize> = Vec::new();

    for result in results.iter() {
        let texts: Vec<String> = result
            .contexts
            .iter()
            .zip(result.chunks.iter())
            .map(|(ctx, chunk)| {
                ctx.as_ref()
                    .cloned()
                    .unwrap_or_else(|| chunk.content.clone())
            })
            .collect();
        file_chunk_counts.push(texts.len());
        all_texts.extend(texts);
    }

    let all_refs: Vec<&str> = all_texts.iter().map(|s| s.as_str()).collect();

    // Embed all chunks in one batch
    let t_embed = Instant::now();
    let (all_embeddings, embed_timing) = embed
        .embed_batch_timed(&all_refs)
        .await
        .context("Failed to generate embeddings")?;
    profile.embed_ms += t_embed.elapsed().as_millis();
    profile.embed_tokenize_ms += embed_timing.tokenize_ms;
    profile.embed_inference_ms += embed_timing.inference_ms;
    profile.embed_pooling_ms += embed_timing.pooling_ms;
    profile.total_chunks_embedded += all_refs.len();
    profile.total_batches += 1;

    // Batch-delete only files that already exist in the index
    let t_del = Instant::now();
    let file_paths: Vec<String> = results
        .iter()
        .map(|r| r.path.clone())
        .filter(|p| existing_files.contains(p))
        .collect();
    if !file_paths.is_empty() {
        vector_store.delete_by_file(&file_paths, Some(repo)).await?;
    }
    profile.delete_ms += t_del.elapsed().as_millis();

    // Accumulate all chunks, embeddings, contexts, per-chunk file hashes,
    // and per-chunk indexed_at timestamps for a single bulk Lance insert.
    let mut all_chunks: Vec<Chunk> = Vec::new();
    let mut all_contexts: Vec<Option<String>> = Vec::new();
    let mut all_hashes: Vec<String> = Vec::new();
    let mut all_indexed_at: Vec<String> = Vec::new();
    let mut indexed_count = 0;
    let mut chunks_count = 0;

    for (result, &chunk_count) in results.drain(..).zip(file_chunk_counts.iter()) {
        let ts = file_timestamps
            .get(&result.path)
            .copied()
            .unwrap_or(fallback_ts)
            .to_string();
        for _ in 0..chunk_count {
            all_hashes.push(result.hash.clone());
            all_indexed_at.push(ts.clone());
        }
        all_chunks.extend(result.chunks);
        all_contexts.extend(result.contexts);
        chunks_count += chunk_count;
        indexed_count += 1;
    }

    let t_ins = Instant::now();
    let hash_refs: Vec<&str> = all_hashes.iter().map(|s| s.as_str()).collect();
    let ts_refs: Vec<&str> = all_indexed_at.iter().map(|s| s.as_str()).collect();
    vector_store
        .insert_bulk(
            &all_chunks,
            &all_embeddings,
            &all_contexts,
            repo,
            &hash_refs,
            &ts_refs,
        )
        .await
        .context("Failed to store chunks")?;
    profile.insert_ms += t_ins.elapsed().as_millis();

    Ok((indexed_count, chunks_count))
}

/// Build context windows for chunks based on contextual embedding config.
///
/// For chunks in enabled languages, extracts N lines before and after the chunk
/// from the file content to create enriched embedding text. Returns `None` for
/// chunks where contextual embedding is disabled (they'll be embedded with their
/// content directly).
pub(crate) fn build_context_windows(
    chunks: &[Chunk],
    file_content: &str,
    config: &ContextualEmbeddingConfig,
) -> Vec<Option<String>> {
    if config.context_lines == 0 || config.enabled_languages.is_empty() {
        return vec![None; chunks.len()];
    }

    let file_lines: Vec<&str> = file_content.lines().collect();
    let n = config.context_lines;

    chunks
        .iter()
        .map(|chunk| {
            if !config.enabled_languages.contains(&chunk.language) {
                return None;
            }

            // start_line and end_line are 1-based
            let start = chunk.start_line as usize;
            let end = chunk.end_line as usize;

            let ctx_start = start.saturating_sub(n).max(1);
            let ctx_end = (end + n).min(file_lines.len());

            // Extract context lines (converting from 1-based to 0-based index)
            let prefix_lines = &file_lines[(ctx_start - 1)..(start - 1).min(file_lines.len())];
            let suffix_lines = if end < file_lines.len() {
                &file_lines[end..ctx_end]
            } else {
                &[]
            };

            // Only produce full_context if it actually adds surrounding lines
            if prefix_lines.is_empty() && suffix_lines.is_empty() {
                return None;
            }

            let mut parts = Vec::new();
            if !prefix_lines.is_empty() {
                parts.push(prefix_lines.join("\n"));
            }
            parts.push(chunk.content.clone());
            if !suffix_lines.is_empty() {
                parts.push(suffix_lines.join("\n"));
            }

            Some(parts.join("\n"))
        })
        .collect()
}

/// Compute SHA256 hash of content
fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
