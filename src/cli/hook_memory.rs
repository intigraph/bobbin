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
    let hooks = &config.hooks;
    let min_len = overrides
        .min_prompt_length
        .unwrap_or(hooks.min_prompt_length);
    let Some(query) = memory_query(&input.prompt, min_len, &hooks.skip_prefixes) else {
        return;
    };
    let backlog = backlog_header();
    let Some(endpoint) = crate::tripwire::endpoint(&config) else {
        if let Some(line) = backlog {
            println!("{line}");
        }
        return;
    };
    let Some(path) = mem::mirror_path() else {
        if let Some(line) = backlog {
            println!("{line}");
        }
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
        mem::Recall::Silent => {
            if let Some(line) = backlog {
                println!("{line}");
            }
        }
        mem::Recall::Skipped(line) => {
            if let Some(header) = backlog {
                println!("{header}");
            }
            print!("{line}");
        }
        mem::Recall::Items { text, items } => {
            let text = match backlog {
                Some(header) => text.replacen(mem::HEADER, &header, 1),
                None => text,
            };
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

// Camayoc owns the queue. Count its bounded local JSONL without a subprocess,
// network request, mutation or any queued text entering the injection.
fn backlog_count(path: &std::path::Path) -> Option<usize> {
    use std::io::Read;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    };
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES || (!bytes.is_empty() && !bytes.ends_with(b"\n")) {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let mut count = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let row: serde_json::Value = serde_json::from_str(line).ok()?;
        if !row.is_object() {
            return None;
        }
        count += 1;
    }
    Some(count)
}

fn backlog_header() -> Option<String> {
    let root = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".local/state"))
        })?;
    backlog_status(backlog_total(&root))
}

fn backlog_total(root: &std::path::Path) -> Option<usize> {
    [
        "memory-queue.jsonl",
        "capture-spool.inflight.jsonl",
        "capture-spool.jsonl",
    ]
    .into_iter()
    .try_fold(0usize, |total, name| {
        total.checked_add(backlog_count(&root.join("camayoc").join(name))?)
    })
}

fn backlog_status(count: Option<usize>) -> Option<String> {
    match count {
        Some(0) => None,
        Some(count) => Some(format!(
            "{}; camayoc backlog: {count} queued (cy flush)",
            mem::HEADER
        )),
        None => Some(format!(
            "{}; camayoc backlog: UNKNOWN (local queue unreadable)",
            mem::HEADER
        )),
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
            "cursor": mirror.as_ref().map(|m| m.cursor.clone()),
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

#[cfg(test)]
mod backlog_tests {
    use super::{backlog_count, backlog_status, backlog_total};

    #[test]
    fn header_counts_pending_and_never_hides_unknown_as_zero() {
        assert_eq!(backlog_status(Some(0)), None);
        assert!(backlog_status(Some(3))
            .unwrap()
            .contains("3 queued (cy flush)"));
        assert!(backlog_status(None).unwrap().contains("backlog: UNKNOWN"));
    }

    #[test]
    fn legacy_inflight_is_pending_and_unreadable_prefix_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("camayoc");
        std::fs::create_dir(&state).unwrap();
        let prefix = state.join("capture-spool.inflight.jsonl");
        std::fs::write(&prefix, "{}\n").unwrap();
        assert_eq!(backlog_total(dir.path()), Some(1));
        std::fs::write(state.join("capture-spool.jsonl"), "{}\n{}\n").unwrap();
        assert_eq!(backlog_total(dir.path()), Some(3));
        std::fs::write(&prefix, "{partial").unwrap();
        assert_eq!(backlog_total(dir.path()), None);
        assert_eq!(std::fs::read_to_string(&prefix).unwrap(), "{partial");
    }

    #[test]
    fn count_is_local_bounded_and_does_not_expose_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory-queue.jsonl");
        assert_eq!(backlog_count(&path), Some(0));
        std::fs::write(
            &path,
            "{\"entity\":\"decision\",\"args\":{\"chose\":\"private body\"}}\n{}\n",
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(backlog_count(&path), Some(2));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::write(&path, "{partial").unwrap();
        assert_eq!(backlog_count(&path), None);
        std::fs::write(&path, "not-json\n").unwrap();
        assert_eq!(backlog_count(&path), None);
        assert_eq!(backlog_count(dir.path()), None);
    }
}
