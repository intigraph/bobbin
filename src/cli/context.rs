#[path = "context_capture.rs"]
mod capture;

use anyhow::{bail, Context, Result};
use clap::Args;
use colored::Colorize;
use std::path::PathBuf;

use super::OutputConfig;
use crate::access::RepoFilter;
use crate::config::Config;
use crate::index::Embedder;
use crate::search::context::{
    BridgeMode, ContentMode, ContextAssembler, ContextBundle, ContextConfig, FileRelevance,
};
use crate::storage::{MetadataStore, VectorStore};
use crate::tags::{build_tag_exclude_filter, build_tag_include_filter};

#[derive(Args)]
pub struct ContextArgs {
    /// Write a private assembly diagnostic (not a hook replay), refusing overwrite
    #[arg(long)]
    capture_assembly: Option<PathBuf>,
    /// Natural language description of the task
    query: String,

    /// Maximum lines of content to include
    #[arg(long, short = 'b', default_value = "500")]
    budget: usize,

    /// Content mode: full, preview, none
    #[arg(long, short = 'c')]
    content: Option<CliContentMode>,

    /// Coupling expansion depth (0 = no coupling)
    #[arg(long, short = 'd', default_value = "1")]
    depth: u32,

    /// Max coupled files per seed file
    #[arg(long, default_value = "3")]
    max_coupled: usize,

    /// Max initial search results
    #[arg(long, short = 'n', default_value = "20")]
    limit: usize,

    /// Min coupling score threshold (overrides `[context].coupling_threshold`)
    #[arg(long)]
    coupling_threshold: Option<f32>,

    /// Override semantic weight (0.0=keyword-only, 1.0=semantic-only)
    #[arg(long)]
    semantic_weight: Option<f32>,

    /// Override doc demotion factor (0.0=full demotion, 1.0=no demotion)
    #[arg(long)]
    doc_demotion: Option<f32>,

    /// Override RRF constant k (lower=top ranks dominate, higher=flatter)
    #[arg(long)]
    rrf_k: Option<f32>,

    /// Personalized PageRank ranking weight (0.0=off). Requires a build with
    /// the `knowledge` feature + a populated Quipu coupling graph.
    #[arg(long)]
    ppr_weight: Option<f32>,

    /// Filter to specific repository
    #[arg(long, short = 'r')]
    repo: Option<String>,

    /// Include only chunks with this tag (can be repeated)
    #[arg(long = "tag")]
    tags: Vec<String>,

    /// Exclude chunks with this tag (can be repeated)
    #[arg(long = "exclude-tag")]
    exclude_tags: Vec<String>,

    /// Directory to search in
    #[arg(default_value = ".")]
    path: PathBuf,
}

/// Content mode for CLI argument parsing
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum CliContentMode {
    /// Include full chunk content
    Full,
    /// Include first 3 lines preview
    Preview,
    /// Paths/metadata only, no content
    None,
}

impl From<CliContentMode> for ContentMode {
    fn from(m: CliContentMode) -> Self {
        match m {
            CliContentMode::Full => ContentMode::Full,
            CliContentMode::Preview => ContentMode::Preview,
            CliContentMode::None => ContentMode::None,
        }
    }
}

pub async fn run(args: ContextArgs, output: OutputConfig) -> Result<()> {
    let repo_root = args
        .path
        .canonicalize()
        .with_context(|| format!("Invalid path: {}", args.path.display()))?;

    let config_path = Config::config_path(&repo_root);
    if !config_path.exists() {
        bail!("{}", super::not_initialized_error(&repo_root));
    }

    let config = Config::load(&config_path).with_context(|| "Failed to load configuration")?;

    let lance_path = Config::lance_path(&repo_root);
    let db_path = Config::db_path(&repo_root);
    let model_dir = Config::model_cache_dir()?;

    let vector_store = VectorStore::open(&lance_path)
        .await
        .context("Failed to open vector store")?;

    let count = vector_store.count().await?;
    if count == 0 {
        if args.capture_assembly.is_some() {
            bail!("Cannot capture an empty index");
        }
        if output.json {
            println!(
                r#"{{"error": "empty_index", "message": "No indexed content. Run `bobbin index` first."}}"#
            );
        } else if !output.quiet {
            println!(
                "{} No indexed content. Run `bobbin index` first.",
                "!".yellow()
            );
        }
        return Ok(());
    }

    let metadata_store = MetadataStore::open(&db_path).context("Failed to open metadata store")?;

    // Check model consistency
    let current_model = config.embedding.model.as_str();
    let stored_model = metadata_store.get_meta("embedding_model")?;
    if let Some(stored) = stored_model {
        if stored != current_model {
            bail!(
                "Configured embedding model ({}) differs from indexed model ({}). Run `bobbin index` to re-index.",
                current_model,
                stored
            );
        }
    }

    let embedder = Embedder::from_config(&config.embedding, &model_dir)
        .context("Failed to load embedding model")?;

    // Determine content mode
    let content_mode = match args.content {
        Some(m) => m.into(),
        None => {
            if output.json {
                ContentMode::Full
            } else {
                ContentMode::Preview
            }
        }
    };

    // Config cascade: CLI flags > calibration.json > config.toml
    let calibration = super::calibrate::load_calibration(&repo_root);
    let cal_sw = calibration.as_ref().map(|c| c.best_config.semantic_weight);
    let cal_dd = calibration.as_ref().map(|c| c.best_config.doc_demotion);
    let cal_rrf = calibration.as_ref().map(|c| c.best_config.rrf_k);
    let cal_hl = calibration
        .as_ref()
        .and_then(|c| c.best_config.recency_half_life_days);
    let cal_rw = calibration
        .as_ref()
        .and_then(|c| c.best_config.recency_weight);
    // Build tag filters
    let mut tag_filters: Vec<String> = Vec::new();
    if !args.tags.is_empty() {
        tag_filters.push(build_tag_include_filter(&args.tags));
    }
    if !args.exclude_tags.is_empty() {
        tag_filters.push(build_tag_exclude_filter(&args.exclude_tags));
    }
    let extra_filter = if tag_filters.is_empty() {
        None
    } else {
        Some(tag_filters.join(" AND "))
    };

    let context_config = ContextConfig {
        capture_candidates: args.capture_assembly.is_some(),
        budget_lines: args.budget,
        budget_unit: config.context.budget_unit,
        depth: args.depth,
        max_coupled: args.max_coupled,
        coupling_threshold: args
            .coupling_threshold
            .unwrap_or(config.context.coupling_threshold),
        semantic_weight: args
            .semantic_weight
            .unwrap_or(cal_sw.unwrap_or(config.search.semantic_weight)),
        content_mode,
        search_limit: args.limit,
        doc_demotion: args
            .doc_demotion
            .unwrap_or(cal_dd.unwrap_or(config.search.doc_demotion)),
        recency_half_life_days: cal_hl.unwrap_or(config.search.recency_half_life_days),
        recency_weight: cal_rw.unwrap_or(config.search.recency_weight),
        rrf_k: args.rrf_k.unwrap_or(cal_rrf.unwrap_or(config.search.rrf_k)),
        ppr_weight: args.ppr_weight.unwrap_or(config.search.ppr_weight),
        bridge_mode: BridgeMode::default(),
        bridge_boost_factor: config.context.bridge_boost_factor,
        extra_filter,
        tags_config: None,
        role: None,
        file_type_rules: vec![],
        repo_affinity: None,
        repo_affinity_boost: config.hooks.repo_affinity_boost,
        max_bridged_files: config.context.max_bridged_files,
        max_bridged_chunks_per_file: config.context.max_bridged_chunks_per_file,
        knowledge_budget_pct: config.context.knowledge_budget_pct,
        neighbor_budget_pct: config.context.neighbor_budget_pct,
        knowledge_max_hops: config.context.knowledge_max_hops,
        feedback_boost_max: config.feedback.boost_max,
        feedback_boost_weight: config.feedback.boost_weight,
        repo_path_prefix: config.server.repo_path_prefix.clone(),
        // Needed to seed PPR against the coupling graph, which is keyed by
        // repo-relative paths (bobbin-jdlkh).
        repo_root: Some(repo_root.clone()),
        ..ContextConfig::default()
    };

    let ppr_enabled = args.ppr_weight.unwrap_or(config.search.ppr_weight) > 0.0;
    let mut assembler =
        ContextAssembler::new(embedder, vector_store, metadata_store, context_config);

    // Wire the Quipu store so the PPR ranking signal can run (knowledge build only).
    #[cfg(feature = "knowledge")]
    if ppr_enabled {
        let quipu_config = quipu::QuipuConfig::load(&repo_root);
        let qpath = if quipu_config.store_path.is_relative() {
            repo_root.join(&quipu_config.store_path)
        } else {
            quipu_config.store_path.clone()
        };
        match quipu::Store::open(qpath.to_string_lossy().as_ref()) {
            Ok(store) => assembler = assembler.with_quipu_store(store),
            Err(e) => {
                if !output.quiet && !output.json {
                    eprintln!(
                        "  {} PPR enabled but Quipu store unavailable; continuing without graph ranking. Use --verbose for details.",
                        "!".yellow()
                    );
                    if output.verbose {
                        eprintln!("    Quipu store error: {e:#}");
                    }
                }
            }
        }
    }
    // Without the `knowledge` feature there is no PPR compute (src/search/ppr.rs is
    // gated), so a non-zero weight would rank nothing and still exit 0. Refuse instead
    // of lying: a flag whose presence is not capability is worse than a missing flag.
    // Covers --ppr-weight and search.ppr_weight from config alike.
    #[cfg(not(feature = "knowledge"))]
    if ppr_enabled {
        bail!(
            "PPR reranking was requested (--ppr-weight or search.ppr_weight > 0.0) but this \
             binary was built WITHOUT the `knowledge` feature, so PPR is not compiled in and \
             would silently do nothing.\n\
             Rebuild with `cargo build --features knowledge` (or `just build`), or set \
             ppr_weight = 0.0 to run without PPR."
        );
    }

    let mut bundle = assembler
        .assemble(&args.query, args.repo.as_deref())
        .await
        .context("Context assembly failed")?;

    // Apply role-based access filtering to context files
    let access_filter = RepoFilter::from_config(&config.access, &output.role);
    bundle
        .files
        .retain(|f| access_filter.is_allowed(RepoFilter::repo_from_path(&f.path)));

    if let Some(path) = &args.capture_assembly {
        let diagnostic = bundle
            .capture
            .as_mut()
            .context("Assembly capture missing")?;
        diagnostic
            .candidates
            .retain(|c| access_filter.is_allowed(RepoFilter::repo_from_path(&c.path)));
        capture::write(
            path,
            &bundle,
            &repo_root,
            &config.embedding.model,
            &output.role,
        )?;
    }

    if output.json {
        print_json_output(&bundle)?;
    } else if !output.quiet {
        print_human_output(&bundle);
    }

    Ok(())
}

fn print_json_output(bundle: &ContextBundle) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(bundle)?);
    Ok(())
}

fn print_human_output(bundle: &ContextBundle) {
    if bundle.files.is_empty() {
        println!(
            "{} No context found for: {}",
            "!".yellow(),
            bundle.query.cyan()
        );
        return;
    }

    println!("{} Context for: {}", "✓".green(), bundle.query.cyan());
    println!(
        "  {} files, {} chunks ({}/{} lines)",
        bundle.summary.total_files,
        bundle.summary.total_chunks,
        bundle.budget.used_lines,
        bundle.budget.max_lines
    );
    println!();

    for file in &bundle.files {
        let relevance_info = match file.relevance {
            FileRelevance::Direct => format!("direct, score: {:.4}", file.score),
            FileRelevance::Coupled => {
                format!("coupled via {}", file.coupled_to.join(", "))
            }
            FileRelevance::Bridged => {
                format!("bridged via doc provenance, score: {:.4}", file.score)
            }
            FileRelevance::Pinned => {
                format!("pinned, score: {:.4}", file.score)
            }
            FileRelevance::Knowledge => {
                format!(
                    "knowledge via {}, score: {:.4}",
                    file.coupled_to.join(", "),
                    file.score
                )
            }
            FileRelevance::Structural => {
                format!(
                    "structural neighbor ({}), score: {:.4}",
                    file.coupled_to.join(", "),
                    file.score
                )
            }
        };

        println!("--- {} [{}] ---", file.path.blue(), relevance_info.dimmed());

        for chunk in &file.chunks {
            let name_display = chunk
                .name
                .as_ref()
                .map(|n| format!("{} ", n))
                .unwrap_or_default();

            println!(
                "  {}{}, lines {}-{}",
                name_display,
                format!("({})", chunk.chunk_type).magenta(),
                chunk.start_line,
                chunk.end_line
            );

            if let Some(ref content) = chunk.content {
                for line in content.lines() {
                    println!("  {}", line.dimmed());
                }
            }
        }

        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_mode_clap_parsing() {
        // Verify the enum variants exist and convert correctly
        assert_eq!(
            std::mem::discriminant(&ContentMode::from(CliContentMode::Full)),
            std::mem::discriminant(&ContentMode::Full)
        );
        assert_eq!(
            std::mem::discriminant(&ContentMode::from(CliContentMode::Preview)),
            std::mem::discriminant(&ContentMode::Preview)
        );
        assert_eq!(
            std::mem::discriminant(&ContentMode::from(CliContentMode::None)),
            std::mem::discriminant(&ContentMode::None)
        );
    }
}
