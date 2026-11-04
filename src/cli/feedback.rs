use anyhow::Result;
use clap::{Args, Subcommand};
use colored::Colorize;

use super::OutputConfig;

#[path = "feedback_render.rs"]
mod render;

#[path = "feedback_remote.rs"]
mod remote;

#[cfg(feature = "knowledge")]
#[path = "feedback_review.rs"]
mod review;

#[derive(Args)]
pub struct FeedbackArgs {
    #[command(subcommand)]
    command: FeedbackCommand,
}

#[derive(Subcommand)]
enum FeedbackCommand {
    /// Preview (or explicitly apply) reviews for locally unused memory entities
    #[cfg(feature = "knowledge")]
    ReviewDue {
        /// Quipu endpoint; must match a fresh local memory mirror
        #[arg(long)]
        endpoint: String,
        /// Move reviewAfter to now through the normal authenticated graph API
        #[arg(long)]
        apply: bool,
        /// Maximum entities to consider (1..100)
        #[arg(long, default_value = "20", value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
    },
    /// Submit feedback on an injection
    Submit {
        /// Injection ID to give feedback on
        #[arg(long)]
        injection: String,

        /// Rating: useful, noise, harmful, or used (used needs --item)
        #[arg(long)]
        rating: String,

        /// Cited memory item (m1..m5) inside the injection, to rate one item
        #[arg(long)]
        item: Option<String>,

        /// Optional reason for the rating
        #[arg(long, default_value = "")]
        reason: String,

        /// Agent identity submitting feedback
        #[arg(long, env = "BD_ACTOR", default_value = "")]
        agent: String,
    },

    /// List feedback records
    List {
        /// Filter by injection ID
        #[arg(long)]
        injection: Option<String>,

        /// Filter by rating
        #[arg(long)]
        rating: Option<String>,

        /// Filter by agent
        #[arg(long)]
        agent: Option<String>,

        /// Maximum number of records
        #[arg(long, short = 'n', default_value = "20")]
        limit: usize,
    },

    /// Show aggregated feedback statistics
    Stats {
        /// Group statistics by bundle name
        #[arg(long)]
        by_bundle: bool,

        /// Group statistics by bead ID
        #[arg(long)]
        by_bead: bool,
    },

    /// Manage lineage records (feedback → fix traceability)
    Lineage(LineageArgs),
}

#[derive(Args)]
struct LineageArgs {
    #[command(subcommand)]
    command: LineageCommand,
}

#[derive(Subcommand)]
enum LineageCommand {
    /// Record a lineage action that resolves feedback
    Store {
        /// Feedback record IDs this action resolves (comma-separated)
        #[arg(long, value_delimiter = ',')]
        feedback_ids: Vec<i64>,

        /// Type of action taken
        #[arg(long)]
        action_type: String,

        /// Associated bead ID
        #[arg(long)]
        bead: Option<String>,

        /// Git commit hash
        #[arg(long)]
        commit: Option<String>,

        /// Description of what was done
        #[arg(long)]
        description: String,

        /// Agent that created this record
        #[arg(long, env = "BD_ACTOR")]
        agent: Option<String>,
    },

    /// List lineage records
    List {
        /// Filter by feedback ID
        #[arg(long)]
        feedback_id: Option<i64>,

        /// Filter by bead ID
        #[arg(long)]
        bead: Option<String>,

        /// Filter by commit hash
        #[arg(long)]
        commit: Option<String>,

        /// Maximum number of records
        #[arg(long, short = 'n', default_value = "20")]
        limit: usize,
    },
}

pub async fn run(args: FeedbackArgs, output: OutputConfig) -> Result<()> {
    #[cfg(feature = "knowledge")]
    if matches!(&args.command, FeedbackCommand::ReviewDue { .. }) {
        anyhow::ensure!(output.server.is_none(), "review-due requires the authoritative local feedback database; --server is unsupported");
    }
    // Thin-client mode: proxy through remote server
    if let Some(ref server_url) = output.server {
        return remote::run(args, output.clone(), server_url).await;
    }

    // Local mode: open feedback store directly
    let bobbin_root = super::find_bobbin_root()
        .ok_or_else(|| anyhow::anyhow!("Bobbin not initialized. Run `bobbin init` first."))?;
    let store = crate::storage::feedback::FeedbackStore::open(
        &bobbin_root.join(".bobbin").join("feedback.db"),
    )?;

    match args.command {
        #[cfg(feature = "knowledge")]
        FeedbackCommand::ReviewDue {
            endpoint,
            apply,
            limit,
        } => {
            let report = review::run(&store, &endpoint, apply, limit as usize).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        FeedbackCommand::Submit {
            injection,
            rating,
            item,
            reason,
            agent,
        } => {
            let input = crate::storage::feedback::FeedbackInput {
                injection_id: injection.clone(),
                agent: agent.clone(),
                rating: rating.clone(),
                reason: reason.clone(),
                item: item.clone(),
            };
            store.store_feedback(&input)?;
            if !output.quiet {
                eprintln!(
                    "{} Feedback recorded: {} for {}",
                    "✓".green(),
                    rating,
                    injection
                );
            }
            if output.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "ok",
                        "injection_id": injection,
                        "rating": rating,
                    })
                );
            }
        }
        FeedbackCommand::List {
            injection,
            rating,
            agent,
            limit,
        } => {
            let query = crate::storage::feedback::FeedbackQuery {
                injection_id: injection,
                rating,
                agent,
                limit: Some(limit),
            };
            let records = store.list_feedback(&query)?;
            if output.json {
                println!("{}", serde_json::to_string(&records)?);
            } else if records.is_empty() {
                eprintln!("No feedback records found.");
            } else {
                for r in &records {
                    render::print_record(
                        r.id,
                        &r.injection_id,
                        r.item.as_deref(),
                        &r.rating,
                        &r.agent,
                        &r.reason,
                    );
                }
                eprintln!("\n{} record(s)", records.len());
            }
        }
        FeedbackCommand::Stats { by_bundle, by_bead } => {
            if by_bundle || by_bead {
                let entries = if by_bundle {
                    store.stats_by_bundle()?
                } else {
                    store.stats_by_bead()?
                };
                if output.json {
                    println!("{}", serde_json::to_string(&entries)?);
                } else if entries.is_empty() {
                    eprintln!("No injection data found.");
                } else {
                    let label = if by_bundle { "Bundle" } else { "Bead" };
                    println!(
                        "{:<30} {:>5} {:>5} {:>6} {:>5} {:>7}",
                        label, "Inj", "Fb", "Good", "Noise", "Harmful"
                    );
                    println!("{}", "-".repeat(65));
                    for e in &entries {
                        println!(
                            "{:<30} {:>5} {:>5} {:>6} {:>5} {:>7}",
                            e.key,
                            e.injections,
                            e.feedback,
                            format!("{}", e.useful).green(),
                            format!("{}", e.noise).yellow(),
                            format!("{}", e.harmful).red(),
                        );
                    }
                }
            } else {
                let stats = store.stats()?;
                if output.json {
                    println!("{}", serde_json::to_string(&stats)?);
                } else {
                    println!("Feedback Statistics");
                    println!("  Injections:  {}", stats.total_injections);
                    println!("  Feedback:    {}", stats.total_feedback);
                    println!(
                        "  Ratings:     {} useful, {} noise, {} harmful, {} used",
                        format!("{}", stats.useful).green(),
                        format!("{}", stats.noise).yellow(),
                        format!("{}", stats.harmful).red(),
                        format!("{}", stats.used).blue(),
                    );
                    for e in &stats.memory_entities {
                        render::print_entity(&e.kind, e.injected, e.used, e.noise, &e.iri);
                    }
                    println!(
                        "  Lineage:     {} actioned, {} unactioned, {} records",
                        stats.actioned, stats.unactioned, stats.lineage_records
                    );
                }
            }
        }
        FeedbackCommand::Lineage(lineage_args) => {
            run_lineage_local(&store, lineage_args, &output)?;
        }
    }
    Ok(())
}

fn run_lineage_local(
    store: &crate::storage::feedback::FeedbackStore,
    args: LineageArgs,
    output: &OutputConfig,
) -> Result<()> {
    match args.command {
        LineageCommand::Store {
            feedback_ids,
            action_type,
            bead,
            commit,
            description,
            agent,
        } => {
            let input = crate::storage::feedback::LineageInput {
                feedback_ids: feedback_ids.clone(),
                action_type: action_type.clone(),
                bead,
                commit_hash: commit,
                description: description.clone(),
                agent,
            };
            let id = store.store_lineage(&input)?;
            if !output.quiet {
                eprintln!(
                    "{} Lineage record #{} created for {} feedback(s)",
                    "✓".green(),
                    id,
                    feedback_ids.len(),
                );
            }
            if output.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "ok",
                        "lineage_id": id,
                        "action_type": action_type,
                        "description": description,
                    })
                );
            }
        }
        LineageCommand::List {
            feedback_id,
            bead,
            commit,
            limit,
        } => {
            let query = crate::storage::feedback::LineageQuery {
                feedback_id,
                bead,
                commit_hash: commit,
                limit: Some(limit),
            };
            let records = store.list_lineage(&query)?;
            if output.json {
                println!("{}", serde_json::to_string(&records)?);
            } else if records.is_empty() {
                eprintln!("No lineage records found.");
            } else {
                for r in &records {
                    println!(
                        "{} {} {} {}",
                        format!("#{}", r.id).dimmed(),
                        r.action_type.cyan(),
                        r.description,
                        r.bead
                            .as_deref()
                            .map(|b| format!("({})", b))
                            .unwrap_or_default()
                            .dimmed(),
                    );
                }
                eprintln!("\n{} record(s)", records.len());
            }
        }
    }
    Ok(())
}
