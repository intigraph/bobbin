use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;
use clap::{Args, Subcommand};

use super::OutputConfig;
use crate::breadcrumb::{Breadcrumb, BreadcrumbStore};

#[derive(Args)]
pub struct BreadcrumbArgs {
    #[command(subcommand)]
    command: BreadcrumbCommands,
}

#[derive(Subcommand)]
enum BreadcrumbCommands {
    /// Create a durable named context shortcut
    Create(BreadcrumbCreateArgs),
    /// List saved breadcrumbs
    List(BreadcrumbListArgs),
    /// Recall a breadcrumb and update its usage metadata
    Recall(BreadcrumbRecallArgs),
    /// Delete a breadcrumb
    Delete(BreadcrumbRecallArgs),
}

#[derive(Args)]
pub struct BreadcrumbCreateArgs {
    /// Short name using lowercase letters, digits, and hyphens
    name: String,
    /// Semantic query captured by the breadcrumb
    query: String,
    /// Description for future agents
    description: String,
    /// Files always associated with this breadcrumb
    #[arg(long, value_delimiter = ',')]
    pin: Vec<String>,
    /// Categorization tags
    #[arg(long, value_delimiter = ',')]
    tag: Vec<String>,
    /// Case-insensitive literal keywords for prompt and tool hooks
    #[arg(long = "on", value_delimiter = ',')]
    keywords: Vec<String>,
    /// Days before expiry; zero means never expires
    #[arg(long, default_value_t = 0)]
    ttl_days: u32,
    /// Repository directory containing (or to contain) .bobbin
    #[arg(long, default_value = ".")]
    path: PathBuf,
}

#[derive(Args)]
pub struct BreadcrumbRecallArgs {
    /// Breadcrumb name
    name: String,
    /// Repository directory containing .bobbin
    #[arg(long, default_value = ".")]
    path: PathBuf,
}

#[derive(Args)]
struct BreadcrumbListArgs {
    /// Repository directory containing .bobbin
    #[arg(long, default_value = ".")]
    path: PathBuf,
}

pub fn run(args: BreadcrumbArgs, output: OutputConfig) -> Result<()> {
    match args.command {
        BreadcrumbCommands::Create(create) => run_mark(create, output),
        BreadcrumbCommands::List(list) => run_list(list.path, output),
        BreadcrumbCommands::Recall(recall) => run_recall(recall, output),
        BreadcrumbCommands::Delete(delete) => {
            let path = delete.path.clone();
            run_delete_at(delete, path, output)
        }
    }
}

pub fn run_mark(args: BreadcrumbCreateArgs, output: OutputConfig) -> Result<()> {
    let path = args.path.clone();
    run_create_at(args, path, output)
}

pub fn run_recall(args: BreadcrumbRecallArgs, output: OutputConfig) -> Result<()> {
    let path = args.path.clone();
    run_recall_at(args, path, output)
}

fn run_create_at(args: BreadcrumbCreateArgs, path: PathBuf, output: OutputConfig) -> Result<()> {
    let store = store(path);
    let breadcrumb = Breadcrumb {
        name: args.name,
        description: args.description,
        query: args.query,
        pinned_files: clean_values(args.pin),
        tags: clean_values(args.tag),
        keywords: clean_values(args.keywords),
        created_by: creator_identity(),
        created_at: Utc::now(),
        last_recalled: None,
        recall_count: 0,
        ttl_days: args.ttl_days,
    };
    store.create(breadcrumb.clone())?;
    if output.json {
        println!("{}", serde_json::to_string_pretty(&breadcrumb)?);
    } else if !output.quiet {
        println!("Created breadcrumb '{}'", breadcrumb.name);
    }
    Ok(())
}

fn run_list(path: PathBuf, output: OutputConfig) -> Result<()> {
    let breadcrumbs = store(path).load()?;
    if output.json {
        let values: Vec<_> = breadcrumbs.values().collect();
        println!("{}", serde_json::to_string_pretty(&values)?);
    } else if breadcrumbs.is_empty() {
        if !output.quiet {
            println!("No breadcrumbs saved.");
        }
    } else {
        for breadcrumb in breadcrumbs.values() {
            println!("{}: {}", breadcrumb.name, breadcrumb.description);
        }
    }
    Ok(())
}

fn run_recall_at(args: BreadcrumbRecallArgs, path: PathBuf, output: OutputConfig) -> Result<()> {
    let breadcrumb = store(path).recall(&args.name, Utc::now())?;
    if output.json {
        println!("{}", serde_json::to_string_pretty(&breadcrumb)?);
    } else if !output.quiet {
        println!("{}: {}", breadcrumb.name, breadcrumb.description);
        println!("Query: {}", breadcrumb.query);
        if !breadcrumb.pinned_files.is_empty() {
            println!("Pinned files:");
            for file in &breadcrumb.pinned_files {
                println!("  {}", file);
            }
        }
    }
    Ok(())
}

fn run_delete_at(args: BreadcrumbRecallArgs, path: PathBuf, output: OutputConfig) -> Result<()> {
    let removed = store(path).delete(&args.name)?;
    if output.json {
        println!("{}", serde_json::to_string_pretty(&removed)?);
    } else if !output.quiet {
        println!("Deleted breadcrumb '{}'", removed.name);
    }
    Ok(())
}

fn store(path: PathBuf) -> BreadcrumbStore {
    let root = path.canonicalize().unwrap_or(path);
    BreadcrumbStore::new(&root)
}

fn clean_values(values: Vec<String>) -> Vec<String> {
    let mut values: Vec<_> = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    values.sort();
    values.dedup();
    values
}

fn creator_identity() -> String {
    ["GT_ROLE", "BD_ACTOR", "USER"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "unknown".to_string())
}
