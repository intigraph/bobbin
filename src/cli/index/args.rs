//! `bobbin index` arguments. Split from `index.rs` (at its size ceiling).

use std::path::PathBuf;

use clap::Args;

#[derive(Args)]
pub struct IndexArgs {
    /// Only update changed files (now the default; kept for backwards compatibility)
    #[arg(long)]
    pub(in crate::cli) incremental: bool,

    /// Force reindex all files
    #[arg(long)]
    pub(in crate::cli) force: bool,

    /// Publish the full graph even if unchanged, without re-embedding files
    #[arg(long)]
    pub(in crate::cli) force_publish: bool,
    #[arg(long = "quipu-graph", help = "Registered quipu graph for chunks")]
    pub(in crate::cli) graph: Option<String>,

    /// Repository name for multi-repo indexing (auto-detected from source dir name)
    #[arg(long)]
    pub(in crate::cli) repo: Option<String>,

    /// Source directory to index files from (defaults to path)
    #[arg(long)]
    pub(in crate::cli) source: Option<PathBuf>,

    /// Also index beads (issues) from the configured bead store
    #[arg(long)]
    pub(in crate::cli) include_beads: bool,

    /// Do not publish this run's chunk or inferred graph to Quipu, whatever
    /// the config says. For sources whose text must stay out of the graph
    /// (e.g. a mounted document share, aegis-1v555n.1): publication is then
    /// opt-in per invocation instead of global.
    #[arg(long)]
    pub(in crate::cli) no_quipu_publish: bool,

    /// Index only files matching this glob for this run, REPLACING the config's
    /// `index.include` (repeatable). For a source whose file types differ from
    /// the repos' (e.g. a document share, aegis-1v555n.1). The run's include is
    /// the repo's include: files indexed earlier that it no longer matches are
    /// removed, exactly as if the config list had changed. The `documents` and
    /// `multimodal` config switches still add their own types.
    #[arg(long = "include", value_name = "GLOB")]
    pub(in crate::cli) include_globs: Vec<String>,

    /// Skip auto-calibration after indexing
    #[arg(long)]
    pub(in crate::cli) skip_calibrate: bool,

    /// When this run prunes and compacts the store: `always` (default),
    /// `auto` (only if the last compaction is older than
    /// --maintenance-interval-hours, or with --force) or `never`. Use `auto`
    /// for frequent incremental refreshes.
    #[arg(long, value_enum, default_value_t)]
    pub(in crate::cli) maintenance: super::maintenance::MaintenanceMode,

    /// With --maintenance auto: the age of the last compaction that triggers
    /// a new sweep.
    #[arg(long, default_value_t = 24, value_name = "HOURS")]
    pub(in crate::cli) maintenance_interval_hours: u64,

    /// Directory containing .bobbin/ config (defaults to current directory)
    #[arg(default_value = ".")]
    pub(in crate::cli) path: PathBuf,
}

/// Apply `--include`: replace the config's include list for this run. An
/// invalid glob is refused here, because `collect_files` drops a pattern it
/// cannot parse, and a typo would then index nothing and prune everything.
pub(in crate::cli) fn apply_include(
    config: &mut crate::config::Config,
    globs: &[String],
) -> anyhow::Result<()> {
    if globs.is_empty() {
        return Ok(());
    }
    for g in globs {
        glob::Pattern::new(g)
            .map_err(|e| anyhow::anyhow!("--include {g:?} is not a valid glob: {e}"))?;
    }
    config.index.include = globs.to_vec();
    Ok(())
}
