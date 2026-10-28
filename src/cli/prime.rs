//! Agent orientation with no model setup, metrics writes or index maintenance.
mod live;

use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::OutputConfig;
use crate::primer::{self, PRIMER};

#[derive(Args)]
pub struct PrimeArgs {
    /// Show five to ten lines for stack orientation
    #[arg(long)]
    brief: bool,

    /// Show a section by heading prefix (case-insensitive)
    #[arg(long, value_name = "NAME")]
    section: Option<String>,

    /// Skip all live index reads
    #[arg(long = "static")]
    offline: bool,

    /// Local indexed project (defaults to current directory; never creates it)
    #[arg(default_value = ".")]
    path: PathBuf,
}

pub async fn run(args: PrimeArgs, output: OutputConfig) -> Result<()> {
    let text = match args.section.as_deref() {
        Some(query) => primer::section(query).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(2);
        }),
        None => PRIMER.to_string(),
    };
    let live = if args.offline {
        json!({"status": "skipped", "elapsed_ms": 0, "summary": {}})
    } else {
        live::summarize(&args.path).await
    };
    if output.json {
        let mut p = json!({"sections": primer::sections()});
        if !args.brief {
            p["text"] = json!(text);
        }
        println!(
            "{}",
            json!({
                "schema": "stack-prime/1", "tool": "bobbin", "version": env!("CARGO_PKG_VERSION"),
                "primer": p, "live": live,
                "status": if live["status"] == "ok" || live["status"] == "skipped" { "ok" } else { "degraded" }
            })
        );
    } else if args.brief {
        print!("{}", brief(&live));
    } else {
        print!("{text}");
        if !output.quiet && live["status"] != "skipped" {
            println!("\n## Live state\n");
            println!(
                "Local index: {} ({})",
                live["summary"]["path"], live["status"]
            );
            println!("{}", live["summary"]["scope"].as_str().unwrap_or(""));
            println!("{}", live["summary"]["index"]);
            println!("{}", live["summary"]["fix"].as_str().unwrap_or(""));
        }
    }
    Ok(())
}

fn brief(live: &Value) -> String {
    let mut lines = vec![format!(
        "bobbin {}: local code search, git coupling and context assembly.",
        env!("CARGO_PKG_VERSION")
    )];
    if live["status"] == "skipped" {
        let what = primer::section("What Bobbin is").expect("What section");
        lines.extend(
            what.lines()
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .take(5)
                .map(str::to_owned),
        );
    } else {
        lines.push(format!(
            "Local index: {} ({}).",
            live["summary"]["path"], live["status"]
        ));
        let index = &live["summary"]["index"];
        if let Some(chunks) = index["total_chunks"].as_u64() {
            lines.push(format!(
                "Chunks: {chunks}; indexed files: {}.",
                index["total_files"]
            ));
            lines.push(format!(
                "Last indexed (unix seconds): {}; age seconds: {}.",
                index["last_indexed"], index["last_indexed_age_seconds"]
            ));
            lines.push("Index time does not establish freshness against git HEAD.".into());
        } else {
            lines.push("Index freshness: unknown.".into());
            lines.push(
                "Run `bobbin index` in the project, or pass the indexed project path.".into(),
            );
        }
    }
    lines.push("Run `bobbin prime` for the full primer.".into());
    format!("{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_states_never_exceed_the_stack_limit() {
        for status in ["ok", "partial", "absent", "unreadable", "skipped"] {
            let v = json!({"status": status, "summary": {"path": "a\nb\rc", "index": {"total_chunks": 3}}});
            let text = brief(&v);
            assert!((5..=10).contains(&text.lines().count()), "{text}");
            assert!(text.ends_with("Run `bobbin prime` for the full primer.\n"));
        }
    }
}
