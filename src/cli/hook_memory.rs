//! Memory lane of `bobbin hook inject-context` (aegis-gshu.2).
//!
//! Runs after the file-context step, inside the same injection: when that step
//! printed an `[injection_id: …]`, the memory block joins it; otherwise the
//! block carries its own marker. Recall reads only the local mirror
//! (`crate::memory_recall`); a stale mirror starts a detached refresh that
//! this hook never waits for. Any failure here is silent to the prompt.

use std::path::PathBuf;
use std::sync::Mutex;

use clap::Args;

use super::{
    find_bobbin_root, generate_context_injection_id, is_automated_message, is_bead_command,
    strip_system_tags, HookInput, OutputConfig, BUILTIN_SKIP_PREFIXES,
};
use crate::config::Config;
use crate::memory_recall as mem;

/// The id the file-context step printed this run, if any.
static LAST_INJECTION_ID: Mutex<Option<String>> = Mutex::new(None);

/// Called by `generate_context_injection_id`, so the memory block can join the
/// injection the file step already printed.
pub(super) fn note_injection_id(id: &str) {
    if let Ok(mut g) = LAST_INJECTION_ID.lock() {
        *g = Some(id.to_string());
    }
}

fn take_injection_id() -> Option<String> {
    LAST_INJECTION_ID.lock().ok().and_then(|mut g| g.take())
}

/// CLI overrides shared with the file step.
pub(super) struct Overrides {
    pub min_prompt_length: Option<usize>,
    pub budget: Option<usize>,
}

/// The text memory recall ranks against, or `None` when the file step would
/// skip this prompt too (short, operational prefix, automated, only system
/// tags, short bead command).
pub(super) fn memory_query(
    prompt: &str,
    min_prompt_length: usize,
    extra_prefixes: &[String],
) -> Option<String> {
    let prompt = prompt.trim();
    if prompt.len() < min_prompt_length {
        return None;
    }
    let lower = prompt.to_lowercase();
    let matches_prefix = |pl: &str| -> bool {
        if pl.len() <= 5 && !pl.ends_with(' ') {
            lower == pl
        } else {
            lower.starts_with(pl)
        }
    };
    if BUILTIN_SKIP_PREFIXES.iter().any(|p| matches_prefix(p))
        || extra_prefixes
            .iter()
            .any(|p| matches_prefix(&p.to_lowercase()))
    {
        return None;
    }
    if is_automated_message(prompt) {
        return None;
    }
    let clean = strip_system_tags(prompt);
    let query = clean.trim();
    if query.is_empty() || is_bead_command(query) {
        return None;
    }
    Some(query.to_string())
}

/// The memory step. `BOBBIN_MEMORY_RECALL=0` turns it off.
pub(super) async fn run(raw: &str, overrides: &Overrides, server: Option<&str>) {
    if std::env::var("BOBBIN_MEMORY_RECALL").is_ok_and(|v| v == "0") {
        return;
    }
    let Ok(input) = serde_json::from_str::<HookInput>(raw) else {
        return;
    };
    let cwd = if input.cwd.is_empty() {
        std::env::current_dir().unwrap_or_default()
    } else {
        PathBuf::from(&input.cwd)
    };
    let root = find_bobbin_root(&cwd);
    let config = root
        .as_ref()
        .and_then(|r| Config::load(&Config::config_path(r)).ok())
        .unwrap_or_default();
    // No quipu configured: no memory lane, no output, no cost.
    let Some(endpoint) = crate::tripwire::endpoint(&config) else {
        return;
    };
    let hooks = &config.hooks;
    let min_len = overrides
        .min_prompt_length
        .unwrap_or(hooks.min_prompt_length);
    let Some(query) = memory_query(&input.prompt, min_len, &hooks.skip_prefixes) else {
        return;
    };
    let Some(path) = mem::mirror_path() else {
        return;
    };
    let mirror = mem::load_mirror(&path);
    let now = mem::now_secs();
    mem::maybe_spawn_refresh(&path, mirror.as_ref(), &endpoint, now);

    let budget = overrides.budget.unwrap_or(hooks.budget);
    match mem::recall_for_prompt(
        &query,
        mirror.as_ref(),
        &endpoint,
        budget,
        config.context.knowledge_budget_pct,
        now,
    ) {
        mem::Recall::Silent => {}
        mem::Recall::Skipped(line) => print!("{line}"),
        mem::Recall::Items { text, items } => {
            let (injection_id, own) = match take_injection_id() {
                Some(id) => (id, false),
                None => (generate_context_injection_id(&query), true),
            };
            let out = if own {
                format!("[injection_id: {injection_id}]\n{text}")
            } else {
                text
            };
            print!("{out}");
            let session = (!input.session_id.is_empty()).then_some(input.session_id.as_str());
            record(
                server,
                root.as_deref(),
                &injection_id,
                own,
                session,
                &query,
                budget,
                &out,
                &items,
            )
            .await;
        }
    }
}

/// Log the cited items (and, for a memory-only injection, the injection) so
/// `feedback_submit` with `item` resolves to an entity. Best-effort.
#[allow(clippy::too_many_arguments)]
async fn record(
    server: Option<&str>,
    root: Option<&std::path::Path>,
    injection_id: &str,
    own: bool,
    session: Option<&str>,
    query: &str,
    budget: usize,
    out: &str,
    items: &[mem::CitedItem],
) {
    if let Some(url) = server {
        let client = crate::http::client::Client::new(url);
        if own {
            let _ = client
                .store_injection_with_output(
                    injection_id,
                    session,
                    None,
                    query,
                    &[],
                    0,
                    budget,
                    Some(out),
                )
                .await;
        }
        let _ = client.store_injection_items(injection_id, items).await;
        return;
    }
    let Some(root) = root else {
        return;
    };
    let Ok(store) = crate::storage::feedback::FeedbackStore::open(&Config::feedback_db_path(root))
    else {
        return;
    };
    if own {
        let _ = store.store_injection_with_output(
            injection_id,
            session,
            None,
            query,
            &[],
            0,
            budget,
            Some(out),
        );
    }
    let _ = store.store_injection_items(injection_id, items);
}

/// `bobbin hook memory-refresh`
#[derive(Args)]
pub(crate) struct MemoryRefreshArgs {
    /// Quipu endpoint (default: BOBBIN_QUIPU_REMOTE, then the config's quipu_endpoint)
    #[arg(long)]
    endpoint: Option<String>,

    /// Print the mirror's state instead of refreshing
    #[arg(long)]
    status: bool,

    /// No output; exit 0 even when quipu is unreachable (used by the hook)
    #[arg(long)]
    quiet: bool,
}

pub(super) async fn run_refresh(
    args: MemoryRefreshArgs,
    output: OutputConfig,
) -> anyhow::Result<()> {
    let quiet = args.quiet || output.quiet;
    let endpoint = args.endpoint.filter(|e| !e.trim().is_empty()).or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        let config = find_bobbin_root(&cwd)
            .and_then(|r| Config::load(&Config::config_path(&r)).ok())
            .unwrap_or_default();
        crate::tripwire::endpoint(&config)
    });
    let Some(endpoint) = endpoint else {
        if !quiet {
            eprintln!("bobbin: no quipu endpoint configured (quipu_endpoint or BOBBIN_QUIPU_REMOTE); memory recall is off");
        }
        return Ok(());
    };
    let path = mem::mirror_path()
        .ok_or_else(|| anyhow::anyhow!("no config directory for the memory mirror"))?;

    if args.status {
        let mirror = mem::load_mirror(&path);
        let now = mem::now_secs();
        let summary = serde_json::json!({
            "path": path,
            "endpoint": mirror.as_ref().map(|m| m.endpoint.clone()),
            "items": mirror.as_ref().map(|m| m.items.len()).unwrap_or(0),
            "cursor": mirror.as_ref().map(|m| (m.cursor.clone(), m.cursor_iri.clone())),
            "refresh_running": mem::refresh_running(&path),
            "age_secs": mirror.as_ref().and_then(|m| m.age_secs(now)),
            "last_error": mirror.as_ref().and_then(|m| m.last_error.clone()),
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    match mem::refresh(&endpoint, &path).await {
        Ok(report) => {
            if output.json {
                println!("{}", serde_json::to_string(&report)?);
            } else if !quiet {
                eprintln!(
                    "bobbin: memory mirror {} items ({} upserted, {} removed, {} pages)",
                    report.items, report.upserted, report.removed, report.pages
                );
            }
            Ok(())
        }
        Err(e) if e.downcast_ref::<mem::RefreshBusy>().is_some() => {
            if !quiet {
                eprintln!("bobbin: {e}; nothing to do");
            }
            Ok(())
        }
        Err(_) if quiet => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_query_skips_like_the_file_step() {
        assert_eq!(memory_query("short", 20, &[]), None);
        assert_eq!(
            memory_query("git status please and more words", 20, &[]),
            None
        );
        assert_eq!(
            memory_query("docker compose up the whole stack now", 20, &[]),
            None
        );
        assert_eq!(
            memory_query("ops: restart everything right now", 20, &["ops: ".into()]),
            None
        );
        assert_eq!(
            memory_query("what rate limit did we pick for the service?", 20, &[]).as_deref(),
            Some("what rate limit did we pick for the service?")
        );
    }

    #[test]
    fn injection_id_handoff_is_taken_once() {
        note_injection_id("inj-test0001");
        assert_eq!(take_injection_id().as_deref(), Some("inj-test0001"));
        assert_eq!(take_injection_id(), None);
    }
}
