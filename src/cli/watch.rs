use anyhow::{bail, Context, Result};
use clap::Args;
use colored::Colorize;
use notify::{Config as NotifyConfig, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::index::build_context_windows;
use super::OutputConfig;
use crate::config::Config;
use crate::index::{embedder, Embedder, Parser};
use crate::storage::{LockWait, MetadataStore, VectorStore};

#[derive(Args)]
pub struct WatchArgs {
    /// Directory containing .bobbin/ config
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Repository name for multi-repo indexing
    #[arg(long)]
    repo: Option<String>,

    /// Source directory to index from (defaults to path)
    #[arg(long)]
    source: Option<PathBuf>,

    /// Debounce interval in milliseconds
    #[arg(long, default_value_t = 500)]
    debounce_ms: u64,

    /// Periodic full-tree reindex backstop, in seconds. Catches changes the
    /// file watcher may have dropped (missed events, high-churn bursts). Each
    /// sweep is incremental — unchanged files are skipped by hash — so it is
    /// cheap when the watcher kept up. Set to 0 to disable. (bobbin #44)
    #[arg(long, default_value_t = 900)]
    reindex_interval_secs: u64,

    /// Write PID to this file for daemon management
    #[arg(long)]
    pid_file: Option<PathBuf>,

    /// Print a systemd service unit to stdout and exit
    #[arg(long)]
    generate_systemd: bool,
}

struct ReindexStats {
    files_indexed: usize,
    chunks_created: usize,
}

pub async fn run(args: WatchArgs, output: OutputConfig) -> Result<()> {
    if args.generate_systemd {
        return print_systemd_unit(&args);
    }

    let repo_root = args
        .path
        .canonicalize()
        .with_context(|| format!("Invalid path: {}", args.path.display()))?;

    let config_path = Config::config_path(&repo_root);
    if !config_path.exists() {
        bail!("{}", super::not_initialized_error(&repo_root));
    }

    let _writer = super::index::generation::WriterLock::ordinary(&repo_root)?;
    super::index::generation::WriterLock::refuse_pending(&repo_root)?;
    let config = Config::load(&config_path)?;

    let source_root = if let Some(ref source) = args.source {
        source
            .canonicalize()
            .with_context(|| format!("Invalid source path: {}", source.display()))?
    } else {
        repo_root.clone()
    };

    // Explicit --repo covers all files; otherwise reindex_files detects each git root.
    let explicit_repo = args.repo.clone();
    let detected_repo = if explicit_repo.is_none() {
        detect_git_repo_name(&source_root)
    } else {
        None
    };
    let fallback_repo = source_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("default")
        .to_string();
    let repo_name: &str = explicit_repo
        .as_deref()
        .or(detected_repo.as_deref())
        .unwrap_or(&fallback_repo);

    // Write PID file
    if let Some(ref pid_path) = args.pid_file {
        std::fs::write(pid_path, std::process::id().to_string())
            .with_context(|| format!("Failed to write PID file: {}", pid_path.display()))?;
    }

    // Ensure embedding model is available
    let model_dir = Config::model_cache_dir()?;
    embedder::ensure_model(&model_dir, &config.embedding.model).await?;

    // Open storage and load models
    let db_path = Config::db_path(&repo_root);
    let lance_path = Config::lance_path(&repo_root);
    let metadata_store = MetadataStore::open(&db_path)?;
    let mut vector_store = VectorStore::open(&lance_path).await?;
    let embed = Embedder::load(&model_dir, &config.embedding.model)?;
    let mut parser = Parser::new()?.with_chunking(
        config.index.chunk_size,
        config.index.chunk_overlap,
        embed.max_seq().unwrap_or(0),
    );

    // Precompile include/exclude patterns
    let include_patterns: Vec<glob::Pattern> = config
        .index
        .include
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();
    let exclude_patterns: Vec<glob::Pattern> = config
        .index
        .exclude
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    // Set up file watcher
    let (notify_tx, notify_rx) = std::sync::mpsc::channel();
    let mut watcher = RecommendedWatcher::new(notify_tx, NotifyConfig::default())
        .context("Failed to create file watcher")?;
    watcher
        .watch(&source_root, RecursiveMode::Recursive)
        .with_context(|| format!("Failed to watch: {}", source_root.display()))?;

    // Bridge std::sync channel to tokio
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::task::spawn_blocking(move || {
        for event in notify_rx {
            if event_tx.send(event).is_err() {
                break;
            }
        }
    });

    if !output.quiet {
        println!(
            "{} Watching {} for changes (debounce: {}ms)",
            "~".cyan(),
            source_root.display(),
            args.debounce_ms
        );
        println!("  Press Ctrl+C to stop");
    }

    let debounce = Duration::from_millis(args.debounce_ms);
    let mut pending_changes: HashSet<PathBuf> = HashSet::new();
    let mut pending_deletes: HashSet<PathBuf> = HashSet::new();
    let mut last_event_time = Instant::now();
    let mut check_interval = tokio::time::interval(Duration::from_millis(100));

    // Lance compaction throttling: without this, each delete+insert leaves a
    // fragment/version behind and the on-disk dataset (plus the in-memory
    // manifest cache) grows without bound. Trigger when either enough files
    // have churned OR enough wall-time has passed with any churn at all.
    const COMPACT_FILE_THRESHOLD: usize = 100;
    const COMPACT_MIN_INTERVAL: Duration = Duration::from_secs(30 * 60);
    let mut compact_counter: usize = 0;
    let mut last_compact = Instant::now();

    // Periodic reindex backstop (bobbin #44). Fires one interval out (not
    // immediately — the watcher was just started fresh) and reconciles the
    // whole tree against the index, catching any events the watcher dropped.
    let reindex_enabled = args.reindex_interval_secs > 0;
    let reindex_period = Duration::from_secs(args.reindex_interval_secs.max(1));
    let mut reindex_interval =
        tokio::time::interval_at(tokio::time::Instant::now() + reindex_period, reindex_period);
    reindex_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    if !output.quiet && reindex_enabled {
        println!("  Reindex backstop: every {}s", args.reindex_interval_secs);
    }

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            biased;

            _ = tokio::signal::ctrl_c() => {
                if !output.quiet {
                    println!("\n{} Stopping watch daemon", "~".cyan());
                }
                break;
            }

            _ = sigterm.recv() => {
                if !output.quiet {
                    println!("\n{} Received SIGTERM, stopping", "~".cyan());
                }
                break;
            }

            event = event_rx.recv() => {
                match event {
                    Some(Ok(ev)) => {
                        for path in &ev.paths {
                            if !matches_patterns(path, &source_root, &include_patterns, &exclude_patterns) {
                                continue;
                            }
                            match ev.kind {
                                EventKind::Create(_) | EventKind::Modify(_) => {
                                    pending_deletes.remove(path);
                                    pending_changes.insert(path.clone());
                                    last_event_time = Instant::now();
                                }
                                EventKind::Remove(_) => {
                                    pending_changes.remove(path);
                                    pending_deletes.insert(path.clone());
                                    last_event_time = Instant::now();
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Err(e)) => {
                        if !output.quiet {
                            eprintln!("  {} Watch error: {}", "!".yellow(), e);
                        }
                    }
                    None => {
                        bail!("File watcher disconnected");
                    }
                }
            }

            _ = check_interval.tick() => {
                let has_work = !pending_changes.is_empty() || !pending_deletes.is_empty();
                if has_work && last_event_time.elapsed() >= debounce {
                    // Process deletions. Attribute each path to its repo where
                    // the tree still allows detection (detect walks up to the
                    // surviving ancestor's .git) so the delete is scoped to that
                    // repo's rows — an unscoped delete also removed every OTHER
                    // repo's copy of a same-named path from the shared index
                    //. Detection failure falls back to the old
                    // any-repo delete: over-deleting costs a re-hash, while
                    // under-deleting leaves stale rows.
                    if !pending_deletes.is_empty() {
                        let mut by_repo: HashMap<Option<String>, Vec<String>> = HashMap::new();
                        for p in pending_deletes.drain() {
                            let repo =
                                detect_git_repo_name(p.parent().unwrap_or(&source_root));
                            let rel = p
                                .strip_prefix(&source_root)
                                .unwrap_or(&p)
                                .to_string_lossy()
                                .to_string();
                            by_repo.entry(repo).or_default().push(rel);
                        }
                        for (repo, del_paths) in &by_repo {
                            match vector_store
                                .delete_by_file(del_paths, repo.as_deref())
                                .await
                            {
                                Ok(_) => {
                                    let _ = metadata_store
                                        .delete_file_hashes(repo.as_deref(), del_paths);
                                    if !output.quiet {
                                        for p in del_paths {
                                            println!("  {} Removed {}", "-".red(), p);
                                        }
                                    }
                                }
                                Err(e) => {
                                    if !output.quiet {
                                        eprintln!("  {} Delete failed: {}", "!".yellow(), e);
                                    }
                                }
                            }
                        }
                    }

                    // Process changes
                    if !pending_changes.is_empty() {
                        let paths: Vec<PathBuf> = pending_changes.drain().collect();
                        match reindex_files(
                            &paths,
                            &source_root,
                            &config,
                            repo_name,
                            &mut vector_store,
                            &metadata_store,
                            &embed,
                            &mut parser,
                            &output,
                        )
                        .await
                        {
                            Ok(stats) if stats.files_indexed > 0 => {
                                compact_counter += stats.files_indexed;
                                if !output.quiet {
                                    println!(
                                        "  {} Re-indexed {} file{} ({} chunks)",
                                        "~".cyan(),
                                        stats.files_indexed,
                                        if stats.files_indexed != 1 { "s" } else { "" },
                                        stats.chunks_created,
                                    );
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                if !output.quiet {
                                    eprintln!("  {} Re-index error: {}", "!".yellow(), e);
                                }
                            }
                        }
                    }

                    // Periodically compact+prune Lance to cap memory/disk growth.
                    // Triggered when enough files have churned OR enough time has
                    // elapsed since the last compaction (with at least one change).
                    let should_compact = compact_counter >= COMPACT_FILE_THRESHOLD
                        || (compact_counter > 0
                            && last_compact.elapsed() >= COMPACT_MIN_INTERVAL);
                    if should_compact {
                        // Prune BEFORE compact — same reason as the reindex path
                        // in cli/index.rs: prune is the cheap reclaim and must
                        // not sit downstream of the operation that can OOM, or a
                        // failing compaction permanently starves it and disk
                        // growth becomes self-perpetuating.
                        //
                        // A SHORT wait, not the reindex's long one: watch is a
                        // periodic top-up, so deferring one cycle costs little,
                        // but skipping instantly on contention is what made
                        // maintenance invisible best-effort. Long
                        // enough to outlast a read-path compaction, short enough
                        // never to sit in front of the nightly.
                        const WATCH_LOCK_WAIT: std::time::Duration =
                            std::time::Duration::from_secs(60);
                        let lock_wait = LockWait::UpTo(WATCH_LOCK_WAIT);

                        // One acquisition for prune+compact, so a contender
                        // cannot take the lock between them.
                        let starved = match vector_store.maintain(lock_wait).await {
                            Ok(o) => o.skipped_lock_held(),
                            Err(e) => {
                                if !output.quiet {
                                    eprintln!("  {} Maintenance error: {}", "!".yellow(), e);
                                }
                                false
                            }
                        };
                        // Say so. The success line below prints either way, and
                        // "Compacted lance dataset" after a skip is the exact
                        // lie this bug was made of.
                        if starved && !output.quiet {
                            eprintln!(
                                "  {} Maintenance skipped: another process held the \
                                 store lock for {}s — nothing reclaimed this cycle",
                                "!".yellow(),
                                WATCH_LOCK_WAIT.as_secs()
                            );
                        }
                        // Compaction/prune can invalidate the FTS index, which
                        // otherwise surfaces as a 500 on the next keyword/hybrid
                        // search (GH#21). Rebuild it proactively so it stays valid
                        // and covers rows added since the last build.
                        if let Err(e) = vector_store.rebuild_fts_index().await {
                            if !output.quiet {
                                eprintln!("  {} FTS reindex error: {}", "!".yellow(), e);
                            }
                        }
                        if !output.quiet && !starved {
                            println!(
                                "  {} Compacted lance dataset ({} files since last)",
                                "~".cyan(),
                                compact_counter
                            );
                        }
                        compact_counter = 0;
                        last_compact = Instant::now();
                    }
                }
            }

            _ = reindex_interval.tick(), if reindex_enabled => {
                match run_reindex_backstop(
                    &source_root,
                    &config,
                    repo_name,
                    &mut vector_store,
                    &metadata_store,
                    &embed,
                    &mut parser,
                    &output,
                )
                .await
                {
                    Ok(stats) if stats.files_indexed > 0 || stats.files_removed > 0 => {
                        compact_counter += stats.files_indexed + stats.files_removed;
                        if !output.quiet {
                            println!(
                                "  {} Backstop reconciled: {} reindexed, {} removed",
                                "~".cyan(),
                                stats.files_indexed,
                                stats.files_removed,
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        if !output.quiet {
                            eprintln!("  {} Backstop error: {}", "!".yellow(), e);
                        }
                    }
                }
            }
        }
    }

    // Cleanup PID file on exit
    if let Some(ref pid_path) = args.pid_file {
        let _ = std::fs::remove_file(pid_path);
    }

    Ok(())
}

/// Check whether a file path matches the configured include/exclude patterns.
fn matches_patterns(
    path: &Path,
    source_root: &Path,
    include: &[glob::Pattern],
    exclude: &[glob::Pattern],
) -> bool {
    if path.is_dir() {
        return false;
    }

    let rel = path
        .strip_prefix(source_root)
        .unwrap_or(path)
        .to_string_lossy();

    // Always skip internal directories
    if rel.starts_with(".git/")
        || rel.starts_with(".git\\")
        || rel.starts_with(".bobbin/")
        || rel.starts_with(".bobbin\\")
    {
        return false;
    }

    if exclude.iter().any(|p| p.matches(&rel)) {
        return false;
    }

    include.iter().any(|p| p.matches(&rel))
}

/// Re-index a set of changed files.
async fn reindex_files(
    paths: &[PathBuf],
    source_root: &Path,
    config: &Config,
    repo_name: &str,
    vector_store: &mut VectorStore,
    metadata_store: &MetadataStore,
    embed: &Embedder,
    parser: &mut Parser,
    output: &OutputConfig,
) -> Result<ReindexStats> {
    let mut stats = ReindexStats {
        files_indexed: 0,
        chunks_created: 0,
    };
    let now = chrono::Utc::now().timestamp().to_string();

    // Cache directory → repo name mappings for performance
    let mut repo_cache: HashMap<PathBuf, String> = HashMap::new();

    for path in paths {
        // Detect per-file repo name from git root (with caching)
        let dir = path.parent().unwrap_or(source_root);
        let effective_repo = if let Some(cached) = repo_cache.get(dir) {
            cached.clone()
        } else {
            let detected = detect_git_repo_name(dir).unwrap_or_else(|| repo_name.to_string());
            repo_cache.insert(dir.to_path_buf(), detected.clone());
            detected
        };

        let rel_path = path
            .strip_prefix(source_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File deleted between event and processing
                let _ = vector_store
                    .delete_by_file(&[rel_path], Some(&effective_repo))
                    .await;
                continue;
            }
            Err(_) => continue,
        };

        if content.trim().is_empty() {
            continue;
        }

        let hash = compute_hash(&content);

        // Skip unchanged files using SQLite hash lookup (scoped to this file's
        // repo — the same path in another repo is a different file)
        if let Some(stored_hash) = metadata_store.get_file_hash(&effective_repo, &rel_path)? {
            if stored_hash == hash {
                continue;
            }
        }

        // Parse under the REPO-RELATIVE path — the parser stamps chunks'
        // file_path with the path it is handed, and rel is the key every
        // delete/reconcile compares against (see cli/index.rs).
        let chunks = match parser.parse_file(Path::new(&rel_path), &content) {
            Ok(c) if !c.is_empty() => c,
            _ => continue,
        };

        let contexts = build_context_windows(&chunks, &content, &config.embedding.context);

        let embed_texts: Vec<String> = contexts
            .iter()
            .zip(chunks.iter())
            .map(|(ctx, chunk)| {
                ctx.as_ref()
                    .cloned()
                    .unwrap_or_else(|| chunk.content.clone())
            })
            .collect();
        let embed_refs: Vec<&str> = embed_texts.iter().map(|s| s.as_str()).collect();
        let embeddings = embed
            .embed_batch(&embed_refs)
            .await
            .context("Failed to generate embeddings")?;

        vector_store
            .delete_by_file(&[rel_path.clone()], Some(&effective_repo))
            .await?;
        vector_store
            .insert(
                &chunks,
                &embeddings,
                &contexts,
                &effective_repo,
                &hash,
                &now,
            )
            .await?;

        // Update SQLite hash after successful indexing
        metadata_store.set_file_hash(&effective_repo, &rel_path, &hash)?;

        stats.chunks_created += chunks.len();
        stats.files_indexed += 1;

        if output.verbose {
            println!("  {} {} ({} chunks)", "+".green(), rel_path, chunks.len());
        }
    }

    Ok(stats)
}

struct BackstopStats {
    files_indexed: usize,
    files_removed: usize,
}

/// Periodic full-tree reconciliation (bobbin #44).
///
/// Walks the whole source tree with the same collector `bobbin index` uses,
/// re-indexes any file whose content hash drifted from the stored one, and
/// prunes index rows for files that have disappeared from disk. Both halves
/// reuse the incremental paths (`reindex_files` hash-skips unchanged files),
/// so a sweep where the watcher kept up does almost no work. This is the
/// safety net for events the watcher missed (restarts, dropped events,
/// high-churn bursts).
#[allow(clippy::too_many_arguments)]
async fn run_reindex_backstop(
    source_root: &Path,
    config: &Config,
    repo_name: &str,
    vector_store: &mut VectorStore,
    metadata_store: &MetadataStore,
    embed: &Embedder,
    parser: &mut Parser,
    output: &OutputConfig,
) -> Result<BackstopStats> {
    let files = super::index::collect_files(source_root, config)?;

    // Prune index rows for files that no longer exist on disk. collect_files
    // only returns extant files, so anything indexed but absent was deleted
    // while the watcher was down (or its Remove event was dropped).
    //
    // Grouped by each file's effective repo (the same detection reindex_files
    // uses) and compared per repo. The old global compare — "every hash row
    // minus THIS root's files" — treated every other repo's rows in a shared
    // index, and the whole bead corpus (`beads:` paths, never on disk), as
    // deleted files, and removed them on every sweep.
    let mut repo_cache: HashMap<PathBuf, String> = HashMap::new();
    let mut current_by_repo: HashMap<String, HashSet<String>> = HashMap::new();
    for p in &files {
        let dir = p.parent().unwrap_or(source_root);
        let repo = if let Some(cached) = repo_cache.get(dir) {
            cached.clone()
        } else {
            let detected = detect_git_repo_name(dir).unwrap_or_else(|| repo_name.to_string());
            repo_cache.insert(dir.to_path_buf(), detected.clone());
            detected
        };
        let rel = p
            .strip_prefix(source_root)
            .unwrap_or(p)
            .to_string_lossy()
            .to_string();
        current_by_repo.entry(repo).or_default().insert(rel);
    }
    let mut files_removed = 0;
    for (repo, current) in &current_by_repo {
        let indexed = metadata_store.get_all_indexed_files(repo)?;
        let removed: Vec<String> = indexed.difference(current).cloned().collect();
        if !removed.is_empty() {
            vector_store.delete_by_file(&removed, Some(repo)).await?;
            metadata_store.delete_file_hashes(Some(repo), &removed)?;
            files_removed += removed.len();
        }
    }

    // Reindex drifted files (reindex_files skips those whose hash is unchanged).
    let stats = reindex_files(
        &files,
        source_root,
        config,
        repo_name,
        vector_store,
        metadata_store,
        embed,
        parser,
        output,
    )
    .await?;

    Ok(BackstopStats {
        files_indexed: stats.files_indexed,
        files_removed,
    })
}

/// Detect the git repo name for a directory by walking up to find `.git`.
/// Returns the directory name containing `.git` (e.g. "moonraker" for
/// /home/user/workspace/moonraker/src/main.rs).
fn detect_git_repo_name(dir: &Path) -> Option<String> {
    let mut current = dir;
    loop {
        if current.join(".git").exists() || current.join(".git").is_file() {
            return current.file_name()?.to_str().map(|s| s.to_string());
        }
        current = current.parent()?;
    }
}

/// Compute SHA256 hash of content.
fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// Print a systemd service unit file to stdout.
fn print_systemd_unit(args: &WatchArgs) -> Result<()> {
    let bobbin_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bobbin"));
    let work_dir = args
        .path
        .canonicalize()
        .unwrap_or_else(|_| args.path.clone());

    let mut exec_start = format!("{} watch", bobbin_path.display());
    if let Some(ref repo) = args.repo {
        exec_start.push_str(&format!(" --repo {}", repo));
    }
    if let Some(ref source) = args.source {
        exec_start.push_str(&format!(" --source {}", source.display()));
    }
    exec_start.push_str(&format!(" --debounce-ms {}", args.debounce_ms));
    exec_start.push_str(&format!(" {}", work_dir.display()));

    println!(
        "\
[Unit]
Description=Bobbin Watch - Continuous code indexing
After=local-fs.target

[Service]
Type=simple
ExecStart={exec_start}
WorkingDirectory={work_dir}
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=default.target",
        exec_start = exec_start,
        work_dir = work_dir.display(),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_patterns_with_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let include = vec![glob::Pattern::new("**/*.rs").unwrap()];
        let exclude = vec![glob::Pattern::new("**/target/**").unwrap()];

        // Create a file that should match
        let rs_file = root.join("main.rs");
        std::fs::write(&rs_file, "fn main() {}").unwrap();
        assert!(matches_patterns(&rs_file, &root, &include, &exclude));

        // Create a file excluded by pattern
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        let target_file = root.join("target/debug/lib.rs");
        std::fs::write(&target_file, "// artifact").unwrap();
        assert!(!matches_patterns(&target_file, &root, &include, &exclude));

        // Non-matching extension
        let txt_file = root.join("notes.txt");
        std::fs::write(&txt_file, "notes").unwrap();
        assert!(!matches_patterns(&txt_file, &root, &include, &exclude));
    }

    #[test]
    fn test_compute_hash_deterministic() {
        let h1 = compute_hash("hello world");
        let h2 = compute_hash("hello world");
        let h3 = compute_hash("different");

        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn test_matches_patterns_excludes_internal_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let include = vec![glob::Pattern::new("**/*").unwrap()];
        let exclude: Vec<glob::Pattern> = vec![];

        // .git/ paths should always be excluded
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        let git_file = root.join(".git/objects/abc");
        std::fs::write(&git_file, "git object").unwrap();
        assert!(!matches_patterns(&git_file, &root, &include, &exclude));

        // .bobbin/ paths should always be excluded
        std::fs::create_dir_all(root.join(".bobbin/vectors")).unwrap();
        let bobbin_file = root.join(".bobbin/vectors/data");
        std::fs::write(&bobbin_file, "lance data").unwrap();
        assert!(!matches_patterns(&bobbin_file, &root, &include, &exclude));
    }
}
