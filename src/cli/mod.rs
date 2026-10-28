mod bead;
mod benchmark;
mod breadcrumb;
mod bundle;
mod calibrate;
mod completions;
mod connect;
mod context;
mod coverage;
mod deps;
#[cfg(test)]
mod eval_v2_calibration_fixture_tests;
mod feedback;
mod grep;
mod history;
mod hook;
mod hooks;
mod hotspots;
mod impact;
mod index;
mod index_bead;
mod init;
mod log;
mod ontology;
mod pack;
mod pack_reindex;
mod predict;
mod prime;
mod purge;
mod reconcile;
mod refs;
mod related;
mod review;
mod run;
mod search;
mod serve;
mod similar;
mod status;
mod tag;
mod tour;
mod watch;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "bobbin")]
#[command(about = "Local-first code context engine")]
// version carries the git sha too: "0.6.4 (a1b2c3d)" — a deployed-commit probe. `bobbin 0.6.4`
// alone is ambiguous — the tag and several commits past it share it — so a
// version-equality drift check would report CURRENT for a stale binary. The HTTP
// /version route is the preferred probe (no ssh needed); this is the on-host fallback.
#[command(version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("BOBBIN_GIT_SHA"), ")"))]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output in JSON format
    #[arg(long, global = true)]
    json: bool,

    /// Suppress non-essential output
    #[arg(long, global = true)]
    quiet: bool,

    /// Show detailed progress
    #[arg(long, global = true)]
    verbose: bool,

    /// Use a remote bobbin HTTP server instead of local storage
    #[arg(long, global = true, value_name = "URL", env = "BOBBIN_SERVER")]
    server: Option<String>,

    /// Metrics source identity (also reads BOBBIN_METRICS_SOURCE env var)
    #[arg(long, global = true, env = "BOBBIN_METRICS_SOURCE")]
    metrics_source: Option<String>,

    /// Role for access filtering (also reads BOBBIN_ROLE, GT_ROLE, BD_ACTOR env vars)
    #[arg(long, global = true, env = "BOBBIN_ROLE")]
    role: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize bobbin in the current repository
    Init(init::InitArgs),

    /// Connect to a remote bobbin server (configure URL + install hooks)
    Connect(connect::ConnectArgs),

    /// Build or update the search index
    Index(index::IndexArgs),

    /// Export, verify, or import a portable search-index pack
    Pack(pack::PackArgs),

    /// Reindex a single bead by id (incremental fast path for a bd post-write hook)
    IndexBead(index_bead::IndexBeadArgs),

    /// Calibrate search parameters against git history
    Calibrate(calibrate::CalibrateArgs),

    /// Semantic search for code
    Search(search::SearchArgs),

    /// Assemble task-relevant context from search and git history
    Context(context::ContextArgs),

    /// Show import dependencies for a file
    Deps(deps::DepsArgs),

    /// Submit, list, and manage feedback on bobbin context injections
    Feedback(feedback::FeedbackArgs),

    /// Record and inspect bead→commit workflow lineage (telemetry)
    Bead(bead::BeadArgs),

    /// Reconcile features, code changes, and bugs into one change_event view
    Reconcile(reconcile::ReconcileArgs),

    /// Predict co-changed files, bug risk, and bundle from a bead or files
    Predict(predict::PredictArgs),

    /// Keyword/regex search
    Grep(grep::GrepArgs),

    /// Find symbol references and list file symbols
    Refs(refs::RefsArgs),

    /// Find files related to a given file
    Related(related::RelatedArgs),

    /// Map test↔source coverage from temporal coupling
    Coverage(coverage::CoverageArgs),

    /// Show commit history for a file
    History(history::HistoryArgs),

    /// Search git commits semantically (find commits by what they did)
    Log(log::LogArgs),

    /// Identify code hotspots (high churn + high complexity)
    Hotspots(hotspots::HotspotsArgs),

    /// Predict which files are affected by a change
    Impact(impact::ImpactArgs),

    /// Assemble review context from a git diff
    Review(review::ReviewArgs),

    /// Find semantically similar code chunks or scan for duplicates
    Similar(similar::SimilarArgs),

    /// Show index status and statistics
    Status(status::StatusArgs),

    /// Start MCP server for AI agent integration
    Serve(serve::ServeArgs),

    /// Benchmark embedding models for comparison
    Benchmark(benchmark::BenchmarkArgs),

    /// Watch for file changes and re-index continuously
    Watch(watch::WatchArgs),

    /// Generate shell completions
    Completions(completions::CompletionsArgs),

    /// Manage Claude Code hooks for automatic context injection
    Hook(hook::HookArgs),

    /// Install bobbin's hooks into Claude Code AND Codex (st-registered when present)
    Hooks(hooks::HooksArgs),

    /// Interactive guided walkthrough of bobbin features
    Tour(tour::TourArgs),

    /// Remove all indexed data for a named repository
    Purge(purge::PurgeArgs),

    /// Show LLM-friendly project overview with live stats
    Prime(prime::PrimeArgs),

    /// Manage chunk tags (list, add rules, remove rules)
    Tag(tag::TagArgs),

    /// Explore context bundles (named, hierarchical knowledge anchors)
    Bundle(bundle::BundleArgs),

    /// Create, list, recall, and delete durable context breadcrumbs
    Bc(breadcrumb::BreadcrumbArgs),

    /// Alias for `bc create`
    Mark(breadcrumb::BreadcrumbCreateArgs),

    /// Alias for `bc recall`
    Recall(breadcrumb::BreadcrumbRecallArgs),

    /// Navigate the tag ontology: hierarchy, relationships, and domain concepts
    Ontology(ontology::OntologyArgs),

    /// Execute or manage user-defined convenience commands
    Run(run::RunArgs),

    /// Catch-all for dynamic commands (from commands.toml or HTTP /cmd)
    #[command(external_subcommand)]
    External(Vec<String>),
}

impl Commands {
    fn name(&self) -> &'static str {
        match self {
            Commands::Init(_) => "init",
            Commands::Connect(_) => "connect",
            Commands::Index(_) => "index",
            Commands::Pack(_) => "pack",
            Commands::IndexBead(_) => "index-bead",
            Commands::Calibrate(_) => "calibrate",
            Commands::Search(_) => "search",
            Commands::Context(_) => "context",
            Commands::Deps(_) => "deps",
            Commands::Feedback(_) => "feedback",
            Commands::Bead(_) => "bead",
            Commands::Reconcile(_) => "reconcile",
            Commands::Predict(_) => "predict",
            Commands::Grep(_) => "grep",
            Commands::Refs(_) => "refs",
            Commands::Related(_) => "related",
            Commands::Coverage(_) => "coverage",
            Commands::History(_) => "history",
            Commands::Log(_) => "log",
            Commands::Hotspots(_) => "hotspots",
            Commands::Impact(_) => "impact",
            Commands::Review(_) => "review",
            Commands::Similar(_) => "similar",
            Commands::Status(_) => "status",
            Commands::Serve(_) => "serve",
            Commands::Benchmark(_) => "benchmark",
            Commands::Watch(_) => "watch",
            Commands::Completions(_) => "completions",
            Commands::Hook(_) => "hook",
            Commands::Hooks(_) => "hooks",
            Commands::Tour(_) => "tour",
            Commands::Purge(_) => "purge",
            Commands::Prime(_) => "prime",
            Commands::Tag(_) => "tag",
            Commands::Bundle(_) => "bundle",
            Commands::Bc(_) => "bc",
            Commands::Mark(_) => "mark",
            Commands::Recall(_) => "recall",
            Commands::Ontology(_) => "ontology",
            Commands::Run(_) => "run",
            Commands::External(ref args) => {
                // Leak a string so we can return &'static str
                // (only called once per invocation, acceptable)
                if let Some(name) = args.first() {
                    Box::leak(name.clone().into_boxed_str())
                } else {
                    "external"
                }
            }
        }
    }
}

impl Cli {
    pub fn is_prime(&self) -> bool {
        matches!(self.command, Commands::Prime(_))
    }

    /// Provision only for a local index, before Tokio and the ORT singleton start.
    pub fn prepare_gpu(&self) -> Result<()> {
        if self.is_prime() {
            return Ok(());
        }
        if resolve_server_url(self.server.clone()).is_none() {
            if let Commands::Index(args) = &self.command {
                let path = crate::config::Config::config_path(&args.path);
                if !path.is_file() {
                    return Ok(());
                }
                let config = crate::config::Config::load(&path)?;
                crate::gpu_runtime::prepare(&config.embedding)?;
            }
        }
        Ok(())
    }

    pub async fn run(self) -> Result<()> {
        // Prime is local and read-only: no config/server discovery, roles,
        // command metrics, model provisioning or unrelated store opens.
        if let Commands::Prime(args) = self.command {
            return prime::run(
                args,
                OutputConfig {
                    json: self.json,
                    quiet: self.quiet,
                    verbose: self.verbose,
                    server: None,
                    role: String::new(),
                },
            )
            .await;
        }

        let resolved_role = crate::access::RepoFilter::resolve_role(self.role.as_deref());
        // Resolve server URL: --server flag / BOBBIN_SERVER env > repo config > global config
        let resolved_server = resolve_server_url(self.server);
        let output = OutputConfig {
            json: self.json,
            quiet: self.quiet,
            verbose: self.verbose,
            server: resolved_server,
            role: resolved_role,
        };

        let metrics_source = self.metrics_source.clone();
        let start = std::time::Instant::now();

        // Resolve `run` commands: either a management op (done) or a re-dispatch
        // Resolve `external` commands: try local commands.toml, then HTTP /cmd
        let (command, output) = match self.command {
            Commands::Run(args) => match run::resolve(args, &output)? {
                run::RunResult::Done => return Ok(()),
                run::RunResult::Execute(resolved_args) => {
                    let resolved = Cli::try_parse_from(&resolved_args)
                        .map_err(|e| anyhow::anyhow!("{}", e))?;
                    let resolved_output = OutputConfig {
                        json: resolved.json,
                        quiet: resolved.quiet,
                        verbose: resolved.verbose,
                        server: resolved.server,
                        role: crate::access::RepoFilter::resolve_role(resolved.role.as_deref()),
                    };
                    (resolved.command, resolved_output)
                }
            },
            Commands::External(ref args) => {
                return dispatch_external(args, &output).await;
            }
            cmd => (cmd, output),
        };

        let command_name = command.name();
        let result = dispatch_command(command, output).await;

        // Best-effort metrics emission (don't skip hooks — they emit their own events)
        if command_name != "hook" && command_name != "prime" {
            if let Some(repo_root) = find_bobbin_root() {
                let source = crate::metrics::resolve_source(metrics_source.as_deref(), None);
                let ev = crate::metrics::event(
                    &source,
                    "command",
                    command_name,
                    start.elapsed().as_millis() as u64,
                    serde_json::json!({
                        "success": result.is_ok(),
                    }),
                );
                crate::metrics::emit(&repo_root, &ev);
            }
        }

        result
    }
}

mod dispatch;
use dispatch::{dispatch_command, dispatch_external, resolve_server_url};

/// Walk up from cwd to find a directory containing `.bobbin/`.
/// Returns None if not found (bobbin not initialized).
pub fn find_bobbin_root() -> Option<std::path::PathBuf> {
    let mut current = std::env::current_dir().ok()?;
    loop {
        if current.join(".bobbin").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Generate a helpful "not initialized" error message that suggests BOBBIN_SERVER
/// when running in a multi-agent/multi-repo setup.
pub fn not_initialized_error(dir: &std::path::Path) -> String {
    let mut msg = format!(
        "Bobbin not initialized in {}. Run `bobbin init` first.",
        dir.display()
    );
    if std::env::var("BOBBIN_SERVER").is_err() {
        msg.push_str(
            "\n\nHint: If a bobbin server is running elsewhere, set BOBBIN_SERVER=<url> \
             or use --server <url> to connect without local initialization.",
        );
    }
    msg
}

/// Output configuration passed to all commands
#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
    /// Remote server URL for thin-client mode
    pub server: Option<String>,
    /// Resolved role for access filtering
    pub role: String,
}
