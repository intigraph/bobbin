//! Thin-client feedback forwarding.

use super::*;

pub(super) async fn run(args: FeedbackArgs, output: OutputConfig, server_url: &str) -> Result<()> {
    let client = crate::http::client::Client::new(server_url);

    match args.command {
        #[cfg(feature = "knowledge")]
        FeedbackCommand::ReviewDue { .. } => anyhow::bail!("review-due does not support --server"),
        FeedbackCommand::Submit {
            injection,
            rating,
            item,
            reason,
            agent,
        } => {
            let resp = client
                .feedback_submit(&injection, &rating, &reason, &agent, item.as_deref())
                .await?;
            if output.json {
                println!("{}", serde_json::to_string(&resp)?);
            } else if !output.quiet {
                eprintln!(
                    "{} Feedback recorded: {} for {}",
                    "✓".green(),
                    rating,
                    injection
                );
            }
        }
        FeedbackCommand::List {
            injection,
            rating,
            agent,
            limit,
        } => {
            let records = client
                .feedback_list(
                    injection.as_deref(),
                    rating.as_deref(),
                    agent.as_deref(),
                    Some(limit),
                )
                .await?;
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
                let group_by = if by_bundle { "bundle" } else { "bead" };
                let entries = client.feedback_stats_grouped(group_by).await?;
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
                let stats = client.feedback_stats().await?;
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
            run_lineage_remote(&client, lineage_args, &output).await?;
        }
    }
    Ok(())
}

async fn run_lineage_remote(
    client: &crate::http::client::Client,
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
            let resp = client
                .lineage_store(
                    &feedback_ids,
                    &action_type,
                    bead.as_deref(),
                    commit.as_deref(),
                    &description,
                    agent.as_deref(),
                )
                .await?;
            if output.json {
                println!("{}", serde_json::to_string(&resp)?);
            } else if !output.quiet {
                eprintln!(
                    "{} Lineage record #{} created for {} feedback(s)",
                    "✓".green(),
                    resp.id,
                    feedback_ids.len(),
                );
            }
        }
        LineageCommand::List {
            feedback_id,
            bead,
            commit,
            limit,
        } => {
            let records = client
                .lineage_list(feedback_id, bead.as_deref(), commit.as_deref(), Some(limit))
                .await?;
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
