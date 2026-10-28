use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::OutputConfig;
use crate::config::Config;
use crate::storage::{MetadataStore, VectorStore};

#[path = "hook_breadcrumb_triggers.rs"]
mod breadcrumb_triggers;
#[path = "hook_breadcrumbs.rs"]
mod breadcrumbs;
#[path = "hook_memory.rs"]
mod hook_memory;
#[path = "hook_memory_anchor.rs"]
mod memory_anchor;
#[path = "hook_primer.rs"]
mod primer;
use primer::extract_brief;

/// Detect the git repo name from a directory by walking up to find `.git`.
/// Returns the directory name containing `.git` (e.g. "aegis" for /home/user/gt/aegis/crew/ian/).
fn detect_repo_name(dir: &Path) -> Option<String> {
    let mut current = dir;
    loop {
        if current.join(".git").exists() {
            return current.file_name()?.to_str().map(|s| s.to_string());
        }
        current = current.parent()?;
    }
}

/// Detect the server-side repo name from a working directory.
///
/// In Gas Town, workspaces follow the pattern `/<town>/<rig>/crew/<name>/`
/// or `/<town>/<rig>/polecats/<name>/`. The rig name (e.g., "bobbin", "aegis")
/// is the repo name on the server, while `detect_repo_name` would return "strider"
/// or "ian" (the git root directory). This function walks up looking for known
/// Gas Town directory patterns.
///
/// Falls back to `detect_repo_name` if no Gas Town structure is found.
fn detect_server_repo_name(dir: &Path) -> Option<String> {
    // Walk up looking for a parent named "crew" or "polecats" — the grandparent is the rig/repo
    let mut current = dir.to_path_buf();
    for _ in 0..10 {
        if let Some(name) = current.file_name().and_then(|n| n.to_str()) {
            if name == "crew" || name == "polecats" {
                // Parent of crew/polecats is the rig name (= server repo name)
                return current
                    .parent()?
                    .file_name()?
                    .to_str()
                    .map(|s| s.to_string());
            }
        }
        if !current.pop() {
            break;
        }
    }
    // Fallback: use git root dir name
    detect_repo_name(dir)
}

/// Operational prompts that never need context (git, cargo, slash commands...).
/// Shared by the local and remote inject paths and the memory step.
const BUILTIN_SKIP_PREFIXES: &[&str] = &[
    "git ",
    "git push",
    "git pull",
    "git status",
    "git diff",
    "git log",
    "git commit",
    "git add",
    "git stash",
    "git rebase",
    "git merge",
    "bd ",
    "gt ",
    "cargo ",
    "go test",
    "go build",
    "go run",
    "npm ",
    "make ",
    "docker ",
    "kubectl ",
    "/", // Slash commands (Claude Code skills)
];

/// Strip XML tag blocks from prompt text that pollute semantic search.
/// System boilerplate, tool schemas, previous injections, and tool call output
/// all add noise to embedding queries without providing useful search signal.
fn strip_system_tags(text: &str) -> String {
    let result = text.to_string();
    // System boilerplate (hook output, nudge metadata, task reminders)
    let result = strip_xml_block(&result, "system-reminder");
    let result = strip_xml_block(&result, "task-notification");
    // Tool name lists and schemas (large JSON blobs)
    let result = strip_xml_block(&result, "available-deferred-tools");
    let result = strip_xml_block(&result, "functions");
    // Previous bobbin injection output re-submitted in prompts
    let result = strip_xml_block(&result, "bobbin-context");
    // Tool call/result output from Claude (XML tool use blocks)
    let result = strip_xml_block(&result, "function_calls");
    let result = strip_xml_block(&result, "function_results");
    let result = strip_xml_block(&result, "antml:function_calls");
    let result = strip_xml_block(&result, "antml:invoke");
    // Example blocks from system prompts
    let result = strip_xml_block(&result, "example");
    let result = strip_xml_block(&result, "example_agent_descriptions");
    // Claude's internal reasoning blocks (re-submitted in context)
    let result = strip_xml_block(&result, "antml:thinking");
    // System prompt metadata blocks
    let result = strip_xml_block(&result, "fast_mode_info");
    let result = strip_xml_block(&result, "types");
    result
}

/// Strip all occurrences of `<tag>...</tag>` from text.
fn strip_xml_block(text: &str, tag: &str) -> String {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(start) = remaining.find(&open) {
        result.push_str(&remaining[..start]);
        if let Some(end) = remaining[start..].find(&close) {
            remaining = &remaining[start + end + close.len()..];
        } else {
            remaining = "";
            break;
        }
    }
    result.push_str(remaining);
    result
}

/// Detect short prompts that are bead/issue commands (e.g., "remove bo-qq5h",
/// "show aegis-abc", "close gt-xyz"). These are operational commands that don't
/// benefit from search context injection. Bead IDs match: prefix-alphanumeric.
fn is_bead_command(prompt: &str) -> bool {
    if prompt.len() > 60 {
        return false;
    }
    let words: Vec<&str> = prompt.split_whitespace().collect();
    if words.len() > 5 {
        return false;
    }
    words.iter().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        if let Some(dash_pos) = w.find('-') {
            let prefix = &w[..dash_pos];
            let suffix = &w[dash_pos + 1..];
            !prefix.is_empty()
                && prefix.chars().all(|c| c.is_ascii_lowercase())
                && !suffix.is_empty()
                && suffix.len() >= 3
                && suffix.chars().all(|c| c.is_ascii_alphanumeric())
        } else {
            false
        }
    })
}

/// Detect automated messages that don't benefit from semantic search.
/// Auto-patrol nudges, reactor alerts, and similar machine-generated messages
/// produce noise injections (matching docs about "escalation", "patrol", etc.)
/// rather than useful context for the agent's actual work.
fn is_automated_message(prompt: &str) -> bool {
    // Trim leading whitespace — prompts may start with \n from nudge/hook wrappers
    let prompt = prompt.trim_start();
    // Check first 500 chars for efficiency (patterns appear early in messages)
    let check = if prompt.len() > 500 {
        &prompt[..500]
    } else {
        prompt
    };

    // Auto-patrol nudge patterns (from crew-patrol.sh / gt nudge)
    if check.contains("Auto-patrol: pick up") || check.contains("PATROL LOOP") {
        return true;
    }
    if check.contains("RANGER PATROL:") || check.contains("PATROL:") {
        return true;
    }

    // Reactor alert patterns
    if check.contains("[reactor]")
        && (check.contains("ESCALATION:")
            || check.contains("P1 bead:")
            || check.contains("P0 bead:"))
    {
        return true;
    }

    // Repeated automated work nudges (pattern: same message duplicated many times)
    if check.contains("WORK: You are") && check.contains("Keep working until context") {
        return true;
    }

    // Startup/handoff messages — these contain system boilerplate, not domain queries
    if check.contains("HANDOFF COMPLETE") && check.contains("You are the NEW session") {
        return true;
    }
    if check.contains("STARTUP PROTOCOL") && check.contains("gt hook") {
        return true;
    }

    // Marshal/dog automated checks
    if check.contains("Marshal check:") && check.contains("You appear idle") {
        return true;
    }

    // Queued nudge wrappers (system envelope, not user intent)
    if check.contains("QUEUED NUDGE") && check.contains("background notification") {
        return true;
    }

    // Session start hook output (system boilerplate injected at conversation start)
    if check.contains("SessionStart:startup hook")
        || check.contains("[GAS TOWN]") && check.contains("session:")
    {
        return true;
    }

    // Reactor alert nudges (always have "[reactor] P" followed by priority + "bead:")
    if check.contains("[reactor] P") && check.contains("bead:") {
        return true;
    }

    // Agent role announcements ("Crew ian, checking in.", "aegis Crew mel, checking in.")
    if check.contains("checking in") && check.contains("Crew ") {
        return true;
    }

    // System reminder blocks (hook output injected into prompts)
    if check.starts_with("<system-reminder>") || check.starts_with("[GAS TOWN]") {
        return true;
    }

    // Handoff mail content — "Check your hook and mail" directives
    if check.contains("Check your hook") && check.contains("mail") && check.contains("then act") {
        return true;
    }

    // Handoff continuation — "[GAS TOWN] crew" + "handoff" patterns
    if check.contains("[GAS TOWN]") && check.contains("handoff") {
        return true;
    }

    // Tool loaded / tool result acknowledgments (no domain content)
    let trimmed = check.trim();
    if trimmed == "Tool loaded."
        || trimmed == "Acknowledged."
        || trimmed == "Continue."
        || trimmed == "OK"
        || trimmed == "ok"
        || trimmed == "Go ahead."
        || trimmed == "Proceed."
        || trimmed.starts_with("Tool loaded")
        || trimmed.starts_with("Human: Tool loaded")
    {
        return true;
    }

    // Crew role assignment / WORK directives (automated dispatching)
    if check.contains("Your differentiated work:") && check.contains("Keep working until") {
        return true;
    }

    // Overseer work assignment nudges (repeated automated directive)
    if check.contains("WORK: You are") {
        return true;
    }

    // "IMPORTANT: After completing" task continuation reminders
    if check.contains("IMPORTANT: After completing your current task") {
        return true;
    }

    // Repeated WORK directives with partial match (without "Keep working" suffix)
    if check.contains("WORK: You are") && check.contains("differentiated work") {
        return true;
    }

    // System reminder about task tools (injected by harness, not user)
    if check.contains("task tools haven't been used recently") {
        return true;
    }

    // Molecule/convoy status checks (orchestration, not domain work)
    if check.contains("gt mol status") && check.contains("gt hook") {
        return true;
    }

    // Very short prompts that are just confirmations (< 15 chars, no technical terms)
    if trimmed.len() < 15
        && !trimmed.contains('_')
        && !trimmed.contains('.')
        && !trimmed.contains("::")
        && trimmed.split_whitespace().count() <= 3
    {
        let lower = trimmed.to_lowercase();
        let confirmation_words = [
            "yes", "no", "ok", "sure", "thanks", "done", "good", "fine", "right", "correct",
            "agreed", "continue", "proceed", "next", "go", "yep", "nope", "ack", "roger", "noted",
        ];
        if confirmation_words
            .iter()
            .any(|w| lower == *w || lower.starts_with(w))
        {
            return true;
        }
    }

    false
}

#[derive(Args)]
pub struct HookArgs {
    #[command(subcommand)]
    command: HookCommands,
}

#[derive(Subcommand)]
enum HookCommands {
    /// Install Claude Code hooks into settings.json
    Install(InstallArgs),

    /// Remove bobbin hooks from Claude Code settings
    Uninstall(UninstallArgs),

    /// Show installed hooks and current config values
    Status(StatusArgs),

    /// Handle UserPromptSubmit events (internal, called by Claude Code)
    InjectContext(InjectContextArgs),

    /// Redeem an injection_id: show the files an injection HELD BACK.
    Expand(ExpandArgs),

    /// Handle SessionStart compact events (internal, called by Claude Code)
    SessionContext(SessionContextArgs),

    /// Output bobbin primer + live stats at SessionStart (internal, called by Claude Code)
    PrimeContext(PrimeContextArgs),

    /// Handle PostToolUse events for file edits (internal, called by Claude Code)
    PostToolUse(PostToolUseArgs),

    /// Handle PostToolUseFailure events (internal, called by Claude Code)
    PostToolUseFailure(PostToolUseFailureArgs),

    /// Install a post-commit git hook for automatic indexing
    InstallGitHook(InstallGitHookArgs),

    /// Remove the bobbin post-commit git hook
    UninstallGitHook(UninstallGitHookArgs),

    /// Generate hot-topics.md from injection frequency data
    HotTopics(HotTopicsArgs),

    /// Refresh the local memory mirror from quipu (spawned by inject-context
    /// when the mirror is stale; also safe from cron or a chaski event)
    MemoryRefresh(hook_memory::MemoryRefreshArgs),
}

#[derive(Args)]
struct InstallArgs {
    /// Install globally (~/.claude/settings.json) instead of project-local
    #[arg(long)]
    global: bool,

    /// Minimum relevance score to include in injected context
    #[arg(long)]
    threshold: Option<f32>,

    /// Maximum lines of injected context
    #[arg(long)]
    budget: Option<usize>,
}

#[derive(Args)]
struct UninstallArgs {
    /// Uninstall from global settings instead of project-local
    #[arg(long)]
    global: bool,
}

#[derive(Args)]
struct StatusArgs {
    /// Directory to check (defaults to current directory)
    #[arg(default_value = ".")]
    path: PathBuf,
}

/// The files an injection stored as candidates but never rendered.
///
/// A file counts as SHOWN when its path appears in the rendered block. That is
/// a substring test over output we generated ourselves, not a heuristic over
/// arbitrary text — the renderer writes the path verbatim.
///
/// Free-standing rather than inline so the arithmetic — which is the part that
/// can be wrong — is testable without a live injection store.
fn withheld_of<'a>(files: &'a [String], rendered: &str) -> Vec<&'a String> {
    files.iter().filter(|f| !rendered.contains(*f)).collect()
}

#[derive(Args)]
struct ExpandArgs {
    /// The injection_id printed in the context header.
    #[arg(long)]
    injection: String,

    /// Maximum files to return.
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

/// `bobbin hook expand --injection <id>` — the pull half of progressive
/// disclosure for CODE context.
///
/// An injection header already carries `injection_id` and, since the omission
/// counters landed, an honest `omitted` count. A count with no way to redeem it
/// is a signpost pointing at a door nobody can open, so this is the door: the
/// stored injection record holds the full candidate list (`files`) and exactly
/// what was rendered (`formatted_output`), and the difference is what the
/// budget held back.
///
/// NO SCHEMA CHANGE, deliberately. The two fields were already stored for the
/// feedback path; deriving the withheld set from them means an injection made
/// before this command existed is still expandable, and there is no second
/// record of what was dropped that could disagree with the first.
async fn run_expand(args: ExpandArgs, output: OutputConfig) -> Result<()> {
    let Some(server_url) = output.server.clone().or_else(|| {
        super::find_bobbin_root()
            .and_then(|root| Config::load(&Config::config_path(&root)).ok())
            .and_then(|c| c.server.url)
    }) else {
        // COULD NOT LOOK, not "nothing was held back". A caller that cannot
        // distinguish them would read an unreachable server as a complete
        // injection, which is the exact misreading the omission counters exist
        // to prevent.
        anyhow::bail!("no bobbin server configured (BOBBIN_SERVER) — cannot expand");
    };
    let client = crate::http::client::Client::new(&server_url);
    let detail: serde_json::Value = client.injection_detail(&args.injection).await?;

    let rendered = detail
        .get("formatted_output")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let files: Vec<String> = detail
        .get("files")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let withheld = withheld_of(&files, rendered);

    if withheld.is_empty() {
        // Say so rather than printing nothing. Silence here is
        // indistinguishable from a failed lookup, and the whole point of this
        // command is to make "there is more" and "that was everything"
        // different observable states.
        if !output.quiet {
            println!(
                "Injection {} held nothing back: all {} candidate file(s) were shown.",
                args.injection,
                files.len()
            );
        }
        return Ok(());
    }

    let shown = withheld.len().min(args.limit);
    let mut out = format!(
        "Injection {} held back {} file(s) of {} candidates",
        args.injection,
        withheld.len(),
        files.len()
    );
    if shown < withheld.len() {
        // The expansion has a budget too, and it says so. A pull surface that
        // truncated silently would reintroduce, one level down, the defect it
        // exists to fix.
        out.push_str(&format!(" (showing {shown}; raise --limit for the rest)"));
    }
    out.push_str(":\n");
    for f in withheld.iter().take(shown) {
        out.push_str(&format!("- {f}\n"));
    }
    if !output.quiet {
        print!("{out}");
    }
    Ok(())
}

#[derive(Args)]
struct InjectContextArgs {
    /// Minimum relevance score (overrides config)
    #[arg(long)]
    threshold: Option<f32>,

    /// Maximum lines of context (overrides config)
    #[arg(long)]
    budget: Option<usize>,

    /// Content display mode: full, preview, or none (overrides config)
    #[arg(long)]
    content_mode: Option<String>,

    /// Minimum prompt length to trigger injection (overrides config)
    #[arg(long)]
    min_prompt_length: Option<usize>,

    /// Minimum raw semantic similarity to inject context at all (overrides config)
    #[arg(long)]
    gate_threshold: Option<f32>,

    /// Force injection even if results match previous session (disables dedup)
    #[arg(long)]
    no_dedup: bool,

    /// Include documentation files in injection output (overrides config)
    #[arg(long)]
    show_docs: Option<bool>,

    /// Injection output format: standard, minimal, verbose, or xml (overrides config)
    #[arg(long)]
    format_mode: Option<String>,
}

#[derive(Args)]
struct SessionContextArgs {
    /// Maximum lines of context (overrides config)
    #[arg(long)]
    budget: Option<usize>,
}

#[derive(Args)]
struct PrimeContextArgs {}

#[derive(Args)]
struct PostToolUseArgs {
    /// Maximum lines of context (overrides config)
    #[arg(long)]
    budget: Option<usize>,
}

#[derive(Args)]
struct PostToolUseFailureArgs {
    /// Maximum lines of context (overrides config)
    #[arg(long)]
    budget: Option<usize>,
}

#[derive(Args)]
struct InstallGitHookArgs {}

#[derive(Args)]
struct UninstallGitHookArgs {}

#[derive(Args)]
struct HotTopicsArgs {
    /// Regenerate even if injection count hasn't reached threshold
    #[arg(long)]
    force: bool,

    /// Directory to operate on (defaults to current directory)
    #[arg(default_value = ".")]
    path: PathBuf,
}

#[derive(Serialize)]
struct HookStatusOutput {
    hooks_installed: bool,
    git_hook_installed: bool,
    config: HookConfigOutput,
    injection_count: u64,
    last_injection_time: Option<String>,
    last_session_id: Option<String>,
}

#[derive(Serialize)]
struct HookConfigOutput {
    threshold: f32,
    budget: usize,
    content_mode: String,
    min_prompt_length: usize,
    gate_threshold: f32,
    dedup_enabled: bool,
}

pub async fn run(args: HookArgs, output: OutputConfig) -> Result<()> {
    match args.command {
        HookCommands::Install(a) => run_install(a, output).await,
        HookCommands::Uninstall(a) => run_uninstall(a, output).await,
        HookCommands::Status(a) => run_status(a, output).await,
        HookCommands::InjectContext(a) => run_inject_context(a, output).await,
        HookCommands::Expand(a) => run_expand(a, output).await,
        HookCommands::SessionContext(a) => run_session_context(a, output).await,
        HookCommands::PrimeContext(a) => run_prime_context(a, output).await,
        HookCommands::PostToolUse(a) => run_post_tool_use(a, output).await,
        HookCommands::PostToolUseFailure(a) => run_post_tool_use_failure(a, output).await,
        HookCommands::InstallGitHook(a) => run_install_git_hook(a, output).await,
        HookCommands::UninstallGitHook(a) => run_uninstall_git_hook(a, output).await,
        HookCommands::HotTopics(a) => run_hot_topics(a, output).await,
        HookCommands::MemoryRefresh(a) => hook_memory::run_refresh(a, output).await,
    }
}

/// Resolve the target settings.json path.
/// --global → ~/.claude/settings.json
/// otherwise → <git-root>/.claude/settings.json
pub(super) fn resolve_settings_path(global: bool) -> Result<PathBuf> {
    if global {
        let home = std::env::var("HOME").context("HOME not set")?;
        Ok(PathBuf::from(home).join(".claude").join("settings.json"))
    } else {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("Failed to run git rev-parse")?;
        if !output.status.success() {
            anyhow::bail!("Not in a git repository. Use --global or run from a git repo.");
        }
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PathBuf::from(root).join(".claude").join("settings.json"))
    }
}

/// Build the bobbin hook entries for Claude Code settings.json.
/// When server_url is Some, bakes BOBBIN_SERVER into the hook commands
/// so agents don't need local initialization.
pub(super) fn bobbin_hook_entries_with_server(server_url: Option<&str>) -> serde_json::Value {
    let prefix = match server_url {
        Some(url) => format!("BOBBIN_SERVER={} ", url),
        None => String::new(),
    };
    json!({
        "hooks": {
            "UserPromptSubmit": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": format!("{}bobbin hook inject-context || true", prefix),
                            "timeout": 10,
                            "statusMessage": "Loading code context..."
                        }
                    ]
                }
            ],
            "SessionStart": [
                {
                    "matcher": "compact",
                    "hooks": [
                        {
                            "type": "command",
                            "command": format!("{}bobbin hook session-context || true", prefix),
                            "timeout": 10,
                            "statusMessage": "Recovering project context..."
                        }
                    ]
                }
            ],
            "PostToolUse": [
                {
                    "matcher": "Write|Edit|Bash|Grep|Glob|Read",
                    "hooks": [
                        {
                            "type": "command",
                            "command": format!("{}bobbin hook post-tool-use || true", prefix),
                            "timeout": 10,
                            "statusMessage": "Analyzing file changes..."
                        }
                    ]
                }
            ],
            "PostToolUseFailure": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": format!("{}bobbin hook post-tool-use-failure || true", prefix),
                            "timeout": 10,
                            "statusMessage": "Searching for related context..."
                        }
                    ]
                }
            ]
        }
    })
}

/// Build hook entries without server URL (backwards-compatible).
fn bobbin_hook_entries() -> serde_json::Value {
    bobbin_hook_entries_with_server(None)
}

/// Check whether a shell command invokes `bobbin hook ...`.
///
/// Project-level hooks (e.g. the quarterdeck shared config) rarely use the
/// bare `bobbin hook ...` form. They wrap it with env-var prefixes, absolute
/// paths, and `|| true` suffixes, e.g.:
///   `BOBBIN_SERVER=http://search.example /home/user/.local/bin/bobbin hook inject-context || true`
/// A plain `starts_with("bobbin hook ")` check misses all of these, which made
/// `bobbin hook status` report "not installed" for working project hooks (GH#10).
///
/// We look for the `bobbin hook ` token at a command boundary (start of string
/// or preceded by `/`, whitespace, or an env-assignment `=`) so paths like
/// `/usr/bin/bobbin hook` match while a different binary such as `mybobbin`
/// does not.
fn command_invokes_bobbin_hook(cmd: &str) -> bool {
    cmd.match_indices("bobbin hook ")
        .any(|(i, _)| i == 0 || matches!(cmd.as_bytes()[i - 1], b'/' | b' ' | b'\t' | b'='))
}

/// Check if a hook group entry contains a bobbin command.
fn is_bobbin_hook_group(group: &serde_json::Value) -> bool {
    if let Some(hooks) = group.get("hooks").and_then(|h| h.as_array()) {
        hooks.iter().any(|h| {
            h.get("command")
                .and_then(|c| c.as_str())
                .map(command_invokes_bobbin_hook)
                .unwrap_or(false)
        })
    } else {
        false
    }
}

/// Merge bobbin hooks into an existing settings object.
/// Preserves non-bobbin hooks in each event array.
fn merge_hooks(settings: &mut serde_json::Value) {
    let bobbin = bobbin_hook_entries();
    merge_hooks_with(settings, &bobbin);
}

pub(super) fn merge_hooks_with(settings: &mut serde_json::Value, bobbin: &serde_json::Value) {
    let bobbin_hooks = bobbin.get("hooks").unwrap().as_object().unwrap();

    // Ensure settings.hooks exists as an object
    if settings.get("hooks").is_none() || !settings["hooks"].is_object() {
        settings["hooks"] = json!({});
    }

    for (event_name, bobbin_entries) in bobbin_hooks {
        let bobbin_arr = bobbin_entries.as_array().unwrap();

        if let Some(existing) = settings["hooks"].get_mut(event_name) {
            if let Some(arr) = existing.as_array_mut() {
                // Remove old bobbin entries, then append new ones
                arr.retain(|entry| !is_bobbin_hook_group(entry));
                arr.extend(bobbin_arr.iter().cloned());
            } else {
                // Event key exists but isn't an array — replace
                settings["hooks"][event_name] = serde_json::Value::Array(bobbin_arr.clone());
            }
        } else {
            settings["hooks"][event_name] = serde_json::Value::Array(bobbin_arr.clone());
        }
    }
}

/// Remove bobbin hooks from a settings object.
/// Returns true if any hooks were removed.
fn remove_bobbin_hooks(settings: &mut serde_json::Value) -> bool {
    let mut removed = false;
    if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for (_event, entries) in hooks.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                let before = arr.len();
                arr.retain(|entry| !is_bobbin_hook_group(entry));
                if arr.len() < before {
                    removed = true;
                }
            }
        }
        // Clean up empty event arrays
        hooks.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    }
    // Remove empty hooks object
    if let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) {
        if hooks.is_empty() {
            settings.as_object_mut().unwrap().remove("hooks");
        }
    }
    removed
}

/// Check whether bobbin hooks are present in a settings.json Value.
fn has_bobbin_hooks(settings: &serde_json::Value) -> bool {
    if let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) {
        for (_event, entries) in hooks {
            if let Some(arr) = entries.as_array() {
                if arr.iter().any(is_bobbin_hook_group) {
                    return true;
                }
            }
        }
    }
    false
}

/// Read a settings.json file, returning empty object if missing.
pub(super) fn read_settings(path: &Path) -> Result<serde_json::Value> {
    if path.exists() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if content.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))
    } else {
        Ok(json!({}))
    }
}

/// Write settings.json, creating parent directories as needed.
pub(super) fn write_settings(path: &Path, settings: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(settings).context("Failed to serialize settings")?;
    std::fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
}

async fn run_install(args: InstallArgs, output: OutputConfig) -> Result<()> {
    let settings_path = resolve_settings_path(args.global)?;

    // Detect server URL to bake into hooks (for multi-agent setups)
    let server_url = output.server.clone().or_else(|| {
        // Check config if available
        if let Some(repo_root) = super::find_bobbin_root() {
            let config_path = Config::config_path(&repo_root);
            if let Ok(config) = Config::load(&config_path) {
                return config.server.url;
            }
        }
        None
    });

    let mut settings = read_settings(&settings_path)?;
    if server_url.is_some() {
        // Use server-aware hooks
        let bobbin = bobbin_hook_entries_with_server(server_url.as_deref());
        merge_hooks_with(&mut settings, &bobbin);
    } else {
        merge_hooks(&mut settings);
    }
    write_settings(&settings_path, &settings)?;

    if output.json {
        let result = json!({
            "status": "installed",
            "path": settings_path.display().to_string(),
            "global": args.global,
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if !output.quiet {
        let scope = if args.global { "global" } else { "project" };
        println!("{} Bobbin hooks installed ({})", "✓".green(), scope.cyan());
        println!(
            "  Location: {}",
            settings_path.display().to_string().dimmed()
        );
        println!("  UserPromptSubmit:    {}", "inject-context".cyan());
        println!(
            "  SessionStart:        {}",
            "session-context (compact)".cyan()
        );
        println!(
            "  PostToolUse:         {}",
            "post-tool-use (Write|Edit|Bash|Grep|Glob)".cyan()
        );
        println!("  PostToolUseFailure:  {}", "post-tool-use-failure".cyan());
        if let Some(ref url) = server_url {
            println!("  Server:              {}", url.cyan());
        }
    }

    Ok(())
}

async fn run_uninstall(args: UninstallArgs, output: OutputConfig) -> Result<()> {
    let settings_path = resolve_settings_path(args.global)?;

    if !settings_path.exists() {
        if output.json {
            let result = json!({
                "status": "not_installed",
                "path": settings_path.display().to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else if !output.quiet {
            println!("No hooks to remove ({})", settings_path.display());
        }
        return Ok(());
    }

    let mut settings = read_settings(&settings_path)?;
    let removed = remove_bobbin_hooks(&mut settings);
    write_settings(&settings_path, &settings)?;

    if output.json {
        let result = json!({
            "status": if removed { "uninstalled" } else { "not_installed" },
            "path": settings_path.display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if !output.quiet {
        if removed {
            println!(
                "{} Bobbin hooks removed from {}",
                "✓".green(),
                settings_path.display()
            );
        } else {
            println!("No bobbin hooks found in {}", settings_path.display());
        }
    }

    Ok(())
}

async fn run_status(args: StatusArgs, output: OutputConfig) -> Result<()> {
    let start = args
        .path
        .canonicalize()
        .with_context(|| format!("Invalid path: {}", args.path.display()))?;
    // Resolve the bobbin root by walking up to the first ancestor holding
    // `.bobbin/config.toml`, matching the resolver the inject path uses
    // (bobbin #42, Bug B). Using `args.path` verbatim made the reported
    // injection count depend on CWD: from a sub-dir the status read a
    // non-existent hook_state.json and defaulted to 0, while injection recorded
    // to the true root. Fall back to `start` when not inside a bobbin tree.
    let repo_root = find_bobbin_root(&start).unwrap_or(start);

    let config = Config::load(&Config::config_path(&repo_root)).unwrap_or_default();

    let hooks_cfg = &config.hooks;

    // Check Claude Code hooks — walk up directory tree like Claude Code does.
    // Check project-local first, then parent directories.
    let (hooks_installed, hooks_found_at) = {
        let mut found = false;
        let mut found_path: Option<PathBuf> = None;
        let mut current = repo_root.clone();
        loop {
            let settings = current.join(".claude").join("settings.json");
            if settings.exists() {
                if read_settings(&settings)
                    .map(|s| has_bobbin_hooks(&s))
                    .unwrap_or(false)
                {
                    found = true;
                    found_path = Some(settings);
                    break;
                }
            }
            if !current.pop() {
                break;
            }
        }
        // Also check global ~/.claude/settings.json
        if !found {
            if let Ok(home) = std::env::var("HOME") {
                let global = PathBuf::from(home).join(".claude").join("settings.json");
                if global.exists() {
                    if read_settings(&global)
                        .map(|s| has_bobbin_hooks(&s))
                        .unwrap_or(false)
                    {
                        found = true;
                        found_path = Some(global);
                    }
                }
            }
        }
        (found, found_path)
    };

    // Check git post-commit hook
    let git_hook_path = repo_root.join(".git").join("hooks").join("post-commit");
    let git_hook_content = if git_hook_path.exists() {
        std::fs::read_to_string(&git_hook_path).ok()
    } else {
        None
    };
    let git_hook_installed = git_hook_content
        .as_deref()
        .map(|c| c.contains(GIT_HOOK_START_MARKER))
        .unwrap_or(false);
    // Hook installed before the bead auto-link integration (bo-5em9) lacks the
    // auto-link line; flag it so users re-run install-git-hook.
    let git_hook_stale = git_hook_installed
        && git_hook_content
            .as_deref()
            .map(|c| !c.contains(GIT_HOOK_AUTOLINK_MARKER))
            .unwrap_or(false);

    // Load runtime state
    let state = load_hook_state(&repo_root);

    if output.json {
        let status = HookStatusOutput {
            hooks_installed,
            git_hook_installed,
            config: HookConfigOutput {
                threshold: hooks_cfg.threshold,
                budget: hooks_cfg.budget,
                content_mode: hooks_cfg.content_mode.clone(),
                min_prompt_length: hooks_cfg.min_prompt_length,
                gate_threshold: hooks_cfg.gate_threshold,
                dedup_enabled: hooks_cfg.dedup_enabled,
            },
            injection_count: state.injection_count,
            last_injection_time: if state.last_injection_time.is_empty() {
                None
            } else {
                Some(state.last_injection_time.clone())
            },
            last_session_id: if state.last_session_id.is_empty() {
                None
            } else {
                Some(state.last_session_id.clone())
            },
        };
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else if !output.quiet {
        println!("{} Hook configuration", "⚡".bold());
        println!();
        println!(
            "  Threshold:        {}",
            hooks_cfg.threshold.to_string().cyan()
        );
        println!(
            "  Budget:           {} lines",
            hooks_cfg.budget.to_string().cyan()
        );
        println!("  Content mode:     {}", hooks_cfg.content_mode.cyan());
        println!(
            "  Min prompt len:   {}",
            hooks_cfg.min_prompt_length.to_string().cyan()
        );
        println!(
            "  Gate threshold:   {}",
            hooks_cfg.gate_threshold.to_string().cyan()
        );
        println!(
            "  Dedup enabled:    {}",
            if hooks_cfg.dedup_enabled {
                "yes".green()
            } else {
                "no".yellow()
            }
        );
        println!();
        let hooks_str = if hooks_installed {
            if let Some(ref p) = hooks_found_at {
                if p.starts_with(&repo_root) {
                    "installed".green()
                } else {
                    format!("installed (via {})", p.display()).green()
                }
            } else {
                "installed".green()
            }
        } else {
            "not installed".yellow()
        };
        let git_str = if git_hook_stale {
            "installed (stale — re-run `bobbin hook install-git-hook`)".yellow()
        } else if git_hook_installed {
            "installed".green()
        } else {
            "not installed".yellow()
        };
        println!("  Claude Code hooks: {}", hooks_str);
        println!("  Git post-commit:   {}", git_str);
        println!();
        println!("{} Injection stats", "📊".bold());
        println!();
        println!(
            "  Injection count:  {}",
            state.injection_count.to_string().cyan()
        );
        if !state.last_injection_time.is_empty() {
            println!("  Last injected:    {}", state.last_injection_time.cyan());
        } else {
            println!("  Last injected:    {}", "never".dimmed());
        }
        if !state.last_session_id.is_empty() {
            println!("  Session ID:       {}", state.last_session_id.dimmed());
        }
    }

    Ok(())
}

async fn run_inject_context(args: InjectContextArgs, output: OutputConfig) -> Result<()> {
    // stdin is read once: the file-context step and the memory step below both
    // need the hook input.
    let raw = match std::io::read_to_string(std::io::stdin()) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("bobbin inject-context: failed to read stdin: {e}");
            return Ok(());
        }
    };
    let memory_overrides = hook_memory::Overrides {
        min_prompt_length: args.min_prompt_length,
        budget: args.budget,
    };
    // Route to remote handler if --server is set
    if let Some(ref server_url) = output.server {
        if let Err(e) = inject_context_remote(args, &output, server_url, &raw).await {
            eprintln!("bobbin inject-context (remote): {:#}", e);
        }
    } else if let Err(e) = inject_context_inner(args, &raw).await {
        // Never block user prompts — any error exits silently
        eprintln!("bobbin inject-context: {:#}", e);
    }
    // Memory recall (aegis-gshu.2): its own lane inside the same injection,
    // read from the local mirror only. Never fails the hook.
    hook_memory::run(&raw, &memory_overrides, output.server.as_deref()).await;
    Ok(())
}

/// Remote-server implementation of inject-context.
/// Uses HTTP client to search instead of opening local stores.
async fn inject_context_remote(
    args: InjectContextArgs,
    output: &OutputConfig,
    server_url: &str,
    raw: &str,
) -> Result<()> {
    // 1. Parse the hook input (read once by the caller)
    let input: HookInput = serde_json::from_str(raw).context("Failed to parse stdin JSON")?;

    // 2. Load config for hook settings (use defaults if not found)
    let cwd = if input.cwd.is_empty() {
        std::env::current_dir().context("Failed to get cwd")?
    } else {
        PathBuf::from(&input.cwd)
    };
    let config = find_bobbin_root(&cwd)
        .and_then(|root| Config::load(&Config::config_path(&root)).ok())
        .unwrap_or_default();
    let hooks_cfg = &config.hooks;

    // Apply CLI overrides
    let min_prompt_length = args
        .min_prompt_length
        .unwrap_or(hooks_cfg.min_prompt_length);
    let budget = args.budget.unwrap_or(hooks_cfg.budget);
    let format_mode = args
        .format_mode
        .as_deref()
        .unwrap_or(&hooks_cfg.format_mode);

    // Resolve repo root and metrics source early (needed for metrics in all paths)
    let repo_root = find_bobbin_root(&cwd).unwrap_or_else(|| cwd.clone());
    let metrics_source = crate::metrics::resolve_source(None, Some(&input.session_id));
    let hook_start = std::time::Instant::now();
    let client = crate::http::client::Client::new(server_url);

    // The trusted NA harness has already consulted the graph once at the turn
    // boundary. Verify and inject that local evidence before ordinary code
    // retrieval, so Quipu latency never enters this hook's hot path.
    if let Ok(Some(grounding)) = verified_na_grounding(input.grounding.as_ref()) {
        let injection_id = generate_context_injection_id(&grounding.grounding_id);
        let out = format!("{}[injection_id: {}]\n", grounding.text, injection_id);
        print!("{out}");
        let session_id = (!input.session_id.is_empty()).then_some(input.session_id.as_str());
        let _ = client
            .store_scoped_injection_with_output(
                &injection_id,
                session_id,
                None,
                "NA turn-boundary grounding",
                &[],
                grounding.text.lines().count(),
                grounding.text.lines().count(),
                Some(&out),
                Some("na"),
                Some(&grounding.faction_id),
                Some(&grounding.grounding_id),
            )
            .await;
    }

    // 3. Check min prompt length
    let prompt = input.prompt.trim();
    if prompt.len() < min_prompt_length {
        return Ok(());
    }

    // 3b. Check skip prefixes (operational commands that never need context).
    // Built-in prefixes always apply; user-configured prefixes extend them.
    let prompt_lower = prompt.to_lowercase();
    let matches_prefix = |pl: &str| -> bool {
        if pl.len() <= 5 && !pl.ends_with(' ') {
            prompt_lower == pl
        } else {
            prompt_lower.starts_with(pl)
        }
    };
    if BUILTIN_SKIP_PREFIXES.iter().any(|p| matches_prefix(p))
        || hooks_cfg
            .skip_prefixes
            .iter()
            .any(|p| matches_prefix(&p.to_lowercase()))
    {
        return Ok(());
    }

    // 3c. Skip injection for automated messages (patrol nudges, reactor alerts, etc.)
    if is_automated_message(prompt) {
        eprintln!("bobbin: skipped (automated message detected)");
        return Ok(());
    }

    // 3d. Clean prompt: strip <system-reminder>...</system-reminder> blocks that contain
    // system boilerplate (hook output, nudge metadata, task reminders). These pollute
    // semantic search with irrelevant terms like "patrol", "hook", "system", "reminder".
    let clean_prompt = strip_system_tags(prompt);
    let search_query = if clean_prompt.trim().is_empty() {
        // If stripping tags removed everything, the prompt was purely system content
        eprintln!("bobbin: skipped (prompt is only system tags)");
        return Ok(());
    } else {
        clean_prompt.trim()
    };

    // 3e. Short bead-command skip: "remove bo-qq5h", "show aegis-abc", etc.
    if is_bead_command(search_query) {
        eprintln!("bobbin: skipped (short bead command detected)");
        return Ok(());
    }

    // 3f. Truncate long prompts for search quality — embedding models lose focus
    // on very long inputs. Keep last 500 chars (most recent/relevant content).
    let search_query = if search_query.len() > 500 {
        // Find a word boundary near the cutpoint to avoid splitting mid-word
        let cutoff = search_query.len() - 500;
        match search_query[cutoff..].find(' ') {
            Some(pos) => &search_query[cutoff + pos + 1..],
            None => &search_query[cutoff..],
        }
    } else {
        search_query
    };

    // 3f-bis. The WORK ITEM, folded into the query when the harness published
    // one. Retrieval here is otherwise per-prompt and stateless, which is why
    // agents rediscover the same context every session
    // (yupana/docs/work-scoped-governance.md §3). Keyed on the item it becomes
    // cumulative: the second session on an item starts where the first
    // finished.
    //
    // APPENDED, never substituted. The prompt is what the agent asked for and
    // stays the dominant signal; the item is a bias, and one that must not be
    // able to drown a question that has moved on from it. An UNKNOWN plate
    // leaves the query byte-identical to today's — see `crate::plate`, which
    // abstains rather than guessing, because a wrong item would skew every
    // retrieval in the session while looking exactly like it was working.
    let work_item = crate::plate::current();
    let item_scoped_query = crate::plate::scope_query(search_query, work_item.as_deref());
    if matches!(&item_scoped_query, std::borrow::Cow::Owned(_)) {
        eprintln!(
            "bobbin: scoping retrieval to work item {}",
            work_item.as_deref().expect("owned query requires an item")
        );
    }
    let search_query = item_scoped_query.as_ref();

    // 3g. Query intent classification: adjust gate threshold for operational queries
    let intent = crate::search::intent::classify_intent(search_query);
    let intent_adj = crate::search::intent::intent_adjustments(intent);

    // 3h. Skip injection entirely for Operational intent. Agents running shell
    // commands (gt hook, bd ready, git push, etc.) never benefit from code context.
    // The Operational gate boost (0.15) is insufficient because semantic scores
    // for hook/mail/status keywords match gastown infrastructure code at 0.7+.
    if intent == crate::search::intent::QueryIntent::Operational {
        eprintln!("bobbin: skipped (operational intent: {:?})", intent);
        return Ok(());
    }

    let budget =
        breadcrumb_triggers::prompt(&repo_root, &input, budget, &config, Some(server_url)).await;

    // 4. Assemble context via remote server (uses full ContextAssembler on server
    //    side, including coupling expansion and provenance bridging).
    //    Falls back to /search if /context returns 404 (e.g., Traefik proxy
    //    not forwarding the endpoint).
    let role = crate::access::RepoFilter::resolve_role(None);

    // Keyword-triggered repo scoping: when query matches configured keywords,
    // scope search to matched repos instead of searching all repos.
    let keyword_repos = hooks_cfg.resolve_keyword_repos(search_query);
    let repo_filter = if keyword_repos.is_empty() {
        None
    } else {
        Some(keyword_repos.join(","))
    };

    // Repo affinity: boost results from the agent's current repo
    let repo_affinity = detect_repo_name(&cwd);

    // Compute per-request scoring overrides from intent classification.
    // Only send overrides when intent adjustments differ from defaults (factor != 1.0).
    let search_cfg = &config.search;
    let semantic_weight_override = if (intent_adj.semantic_weight_factor - 1.0).abs() > f32::EPSILON
    {
        // Direct multiplication: factor < 1.0 = more keyword, > 1.0 = more semantic
        Some((search_cfg.semantic_weight * intent_adj.semantic_weight_factor).clamp(0.0, 1.0))
    } else {
        None
    };
    let doc_demotion_override = if (intent_adj.doc_demotion_factor - 1.0).abs() > f32::EPSILON {
        // Effect-space, via the shared helper. See apply_doc_demotion_factor for
        // why multiplying doc_demotion directly moves it the wrong way (bobbin-lpp).
        Some(crate::search::intent::apply_doc_demotion_factor(
            search_cfg.doc_demotion,
            intent_adj.doc_demotion_factor,
            0.0,
        ))
    } else {
        None
    };
    let recency_weight_override = if (intent_adj.recency_weight_factor - 1.0).abs() > f32::EPSILON {
        // Direct multiplication: factor > 1.0 = prefer recent, < 1.0 = less recency
        Some((search_cfg.recency_weight * intent_adj.recency_weight_factor).clamp(0.0, 1.0))
    } else {
        None
    };

    // Intent-aware coupling threshold: Navigation/Operational queries need
    // tighter coupling to avoid noise; Architecture queries benefit from looser.
    let coupling_threshold = intent_adj.coupling_threshold.unwrap_or(0.15);

    let context_result = client
        .context_with_weights(
            search_query,
            Some(budget),
            Some(1),  // depth: 1 level of coupling expansion
            Some(2),  // max_coupled: 2 coupled files per seed (was 3, tightened to reduce noise)
            Some(12), // search_limit: 12 initial results (was 15, tightened for precision)
            Some(coupling_threshold),
            repo_filter.as_deref(),
            Some(&role),
            repo_affinity.as_deref(),
            semantic_weight_override,
            doc_demotion_override,
            recency_weight_override,
        )
        .await;

    // Apply intent-based gate boost (operational queries get a higher bar)
    let base_gate = args.gate_threshold.unwrap_or(hooks_cfg.gate_threshold);
    let gate = base_gate + intent_adj.gate_boost;

    match context_result {
        Ok(resp) => {
            if resp.files.is_empty() {
                return Ok(());
            }

            // Gate check: use raw cosine score from server (not RRF-normalized chunk scores)
            let top_score = if resp.summary.top_semantic_score > 0.0 {
                resp.summary.top_semantic_score
            } else {
                // Fallback for older servers that don't return top_semantic_score
                resp.files
                    .iter()
                    .flat_map(|f| f.chunks.iter())
                    .map(|c| c.score)
                    .fold(0.0_f32, f32::max)
            };
            if top_score < gate {
                if output.verbose {
                    eprintln!(
                        "bobbin: skipped (score={:.3} < gate={:.3}, intent={:?})",
                        top_score, gate, intent,
                    );
                }
                crate::metrics::emit(
                    &repo_root,
                    &crate::metrics::event(
                        &metrics_source,
                        "hook_gate_skip",
                        "hook inject-context-remote",
                        hook_start.elapsed().as_millis() as u64,
                        serde_json::json!({
                            "query": &prompt[..prompt.len().min(200)],
                            "top_score": top_score,
                            "gate_threshold": gate,
                            "intent": format!("{:?}", intent),
                            "gate_boost": intent_adj.gate_boost,
                        }),
                    ),
                );
                return Ok(());
            }

            // Session dedup: filter out chunks already injected in this session
            let repo_root = find_bobbin_root(&cwd).unwrap_or_else(|| cwd.clone());
            let mut ledger = SessionLedger::load(&repo_root, &input.session_id);
            let reducing_enabled = hooks_cfg.reducing_enabled && !input.session_id.is_empty();

            // Destructure to avoid partial-move issues
            let crate::http::client::ContextResponse {
                query: resp_query,
                budget: resp_budget,
                files: mut resp_files,
                summary: resp_summary,
            } = resp;

            if reducing_enabled {
                // Filter chunks already seen, remove empty files
                for file in resp_files.iter_mut() {
                    let original_len = file.chunks.len();
                    file.chunks.retain(|c| {
                        let key = chunk_key(&file.path, c.start_line, c.end_line);
                        !ledger.contains(&key)
                    });
                    if file.chunks.len() < original_len {
                        eprintln!(
                            "bobbin: dedup removed {}/{} chunks from {}",
                            original_len - file.chunks.len(),
                            original_len,
                            file.path,
                        );
                    }
                }
                resp_files.retain(|f| !f.chunks.is_empty());
                if resp_files.is_empty() {
                    eprintln!("bobbin: all chunks already injected this session, skipping");
                    crate::metrics::emit(
                        &repo_root,
                        &crate::metrics::event(
                            &metrics_source,
                            "hook_reducing_skip",
                            "hook inject-context-remote",
                            hook_start.elapsed().as_millis() as u64,
                            serde_json::json!({
                                "query": &prompt[..prompt.len().min(200)],
                            }),
                        ),
                    );
                    return Ok(());
                }
            }

            // Cross-repo filename dedup: when the same filename appears from multiple
            // repos, keep only the one from the agent's repo (or highest scoring).
            // This prevents e.g. testing.md from 5 repos all appearing in results.
            {
                let mut seen_filenames: HashMap<String, usize> = HashMap::new();
                let mut to_remove = Vec::new();
                for (idx, file) in resp_files.iter().enumerate() {
                    let filename = file
                        .path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&file.path)
                        .to_string();
                    if let Some(&prev_idx) = seen_filenames.get(&filename) {
                        // Duplicate filename — keep the one from agent's repo, or higher score
                        let prev = &resp_files[prev_idx];
                        let prev_is_affinity = repo_affinity.as_ref().map_or(false, |ra| {
                            prev.repo.as_deref() == Some(ra.as_str())
                                || prev.path.contains(ra.as_str())
                        });
                        let curr_is_affinity = repo_affinity.as_ref().map_or(false, |ra| {
                            file.repo.as_deref() == Some(ra.as_str())
                                || file.path.contains(ra.as_str())
                        });
                        if curr_is_affinity && !prev_is_affinity {
                            // Current is from agent's repo, remove previous
                            to_remove.push(prev_idx);
                            seen_filenames.insert(filename, idx);
                        } else if !curr_is_affinity && prev_is_affinity {
                            // Previous is from agent's repo, remove current
                            to_remove.push(idx);
                        } else if file.score > prev.score {
                            // Same affinity status, keep higher score
                            to_remove.push(prev_idx);
                            seen_filenames.insert(filename, idx);
                        } else {
                            to_remove.push(idx);
                        }
                    } else {
                        seen_filenames.insert(filename, idx);
                    }
                }
                if !to_remove.is_empty() {
                    eprintln!(
                        "bobbin: cross-repo dedup removed {} duplicate filenames",
                        to_remove.len()
                    );
                    to_remove.sort_unstable();
                    to_remove.dedup();
                    for idx in to_remove.into_iter().rev() {
                        resp_files.remove(idx);
                    }
                }
            }

            // Filter out files already in agent context (CLAUDE.md, AGENTS.md, etc.)
            // and static project docs that waste injection budget.
            {
                let before = resp_files.len();
                resp_files.retain(|f| {
                    let filename = f.path.rsplit('/').next().unwrap_or(&f.path);
                    !matches!(
                        filename,
                        "CLAUDE.md"
                            | "AGENTS.md"
                            | "@AGENTS.md"
                            | "CLAUDE.local.md"
                            | "MEMORY.md"
                            | "README.md"
                            | "CONTRIBUTING.md"
                            | "LICENSE.md"
                            | "QUICKSTART.md"
                            | "FAQ.md"
                            | "INSTALLING.md"
                            | "UNINSTALLING.md"
                            | "TROUBLESHOOTING.md"
                            | "RELEASING.md"
                            | "SETUP.md"
                    )
                });
                let removed = before - resp_files.len();
                if removed > 0 {
                    eprintln!(
                        "bobbin: filtered {} already-in-context files (CLAUDE.md etc.)",
                        removed
                    );
                }
            }

            // Filter out design doc directories — static planning/design docs
            // produce high noise (e.g. 463 _plans/ docs overwhelming real results).
            // These are reference material, not active code context.
            {
                let before = resp_files.len();
                let design_dirs = [
                    "/_plans/",
                    "/_design/",
                    "/_roadmap/",
                    "/_specs/",
                    "/audit/",
                    "/docs/tasks/",
                    "/docs/plans/",
                    "/docs/design/",
                    "/docs/designs/",
                    "/docs/runbooks/",
                    "/crew/",
                    "/polecats/",
                    "/memory/",
                    "/.beads/",
                    "/session-notes/",
                    "/sessions/",
                ];
                let test_dirs = [
                    "/tests/",
                    "/test/",
                    "/__tests__/",
                    "/spec/",
                    "/specs/",
                    "/testdata/",
                    "/fixtures/",
                    "/examples/",
                    "/example/",
                    "/samples/",
                    "/demo/",
                    "/demos/",
                ];
                let infra_dirs = [
                    "/.github/workflows/",
                    "/.github/actions/",
                    "/terraform/",
                    "/ansible/",
                    "/helm/",
                    "/deploy/",
                    "/.circleci/",
                    "/.gitlab-ci",
                ];
                let design_files = [
                    "ROADMAP.md",
                    "DESIGN.md",
                    "ARCHITECTURE.md",
                    "VISION.md",
                    "PRD.md",
                    "CHANGELOG.md",
                ];
                resp_files.retain(|f| {
                    let path_lower = f.path.to_lowercase();
                    // Skip if path contains a design/planning directory
                    if design_dirs.iter().any(|d| path_lower.contains(d)) {
                        return false;
                    }
                    // Skip test/example directories
                    if test_dirs.iter().any(|d| path_lower.contains(d)) {
                        return false;
                    }
                    // Skip CI/infrastructure paths
                    if infra_dirs.iter().any(|d| path_lower.contains(d)) {
                        return false;
                    }
                    // Skip known design doc filenames
                    let filename = f.path.rsplit('/').next().unwrap_or(&f.path);
                    if design_files
                        .iter()
                        .any(|d| filename.eq_ignore_ascii_case(d))
                    {
                        return false;
                    }
                    // Skip test file patterns (catches test files outside /test/ dirs)
                    let fname_lower = filename.to_lowercase();
                    if fname_lower.ends_with("_test.go")
                        || fname_lower.ends_with("_test.rs")
                        || fname_lower.ends_with(".test.ts")
                        || fname_lower.ends_with(".test.js")
                        || fname_lower.ends_with(".spec.ts")
                        || fname_lower.ends_with(".spec.js")
                        || fname_lower.starts_with("test_")
                        || matches!(
                            filename,
                            "Dockerfile"
                                | "docker-compose.yml"
                                | "docker-compose.yaml"
                                | "Makefile"
                                | "Justfile"
                                | "Taskfile.yml"
                        )
                    {
                        return false;
                    }
                    // Skip lock files and generated output
                    if matches!(
                        filename,
                        "Cargo.lock"
                            | "package-lock.json"
                            | "yarn.lock"
                            | "pnpm-lock.yaml"
                            | "go.sum"
                            | "Gemfile.lock"
                            | "poetry.lock"
                            | "composer.lock"
                            | "Pipfile.lock"
                    ) {
                        return false;
                    }
                    // Skip vendored/generated directories
                    if path_lower.contains("/vendor/")
                        || path_lower.contains("/node_modules/")
                        || path_lower.contains("/third_party/")
                        || path_lower.contains("/dist/")
                        || path_lower.contains("/build/")
                        || path_lower.contains("/target/")
                    {
                        return false;
                    }
                    true
                });
                let removed = before - resp_files.len();
                if removed > 0 {
                    eprintln!(
                        "bobbin: filtered {} noise path files (design/test/infra)",
                        removed
                    );
                }
            }

            // Cross-repo non-affinity penalty: non-affinity results need a higher
            // score to survive. This prevents leakage of unrelated code from other
            // repos (e.g. gastown Go code in bobbin context). The penalty scales by
            // intent — Architecture/Config queries get a smaller penalty (cross-repo
            // docs are sometimes relevant), while General/BugFix get a larger one.
            // Language mismatch adds an extra penalty (e.g. Go results in a Rust repo).
            {
                use crate::search::intent::QueryIntent;
                let cross_repo_penalty = match intent {
                    QueryIntent::Architecture | QueryIntent::Configuration => 0.04,
                    QueryIntent::Navigation => 0.06,
                    QueryIntent::Implementation | QueryIntent::BugFix => 0.08,
                    QueryIntent::General => 0.10,
                    QueryIntent::Operational => 0.12, // Operational rarely needs cross-repo
                };
                if let Some(ref affinity) = repo_affinity {
                    // Detect dominant language from affinity-repo results
                    let affinity_lang: Option<String> = {
                        let mut lang_counts: std::collections::HashMap<&str, usize> =
                            std::collections::HashMap::new();
                        for f in resp_files.iter() {
                            let is_aff = f.repo.as_deref() == Some(affinity.as_str())
                                || f.path.contains(affinity.as_str());
                            if is_aff && !f.language.is_empty() && f.language != "markdown" {
                                *lang_counts.entry(&f.language).or_insert(0) += 1;
                            }
                        }
                        lang_counts
                            .into_iter()
                            .max_by_key(|(_, count)| *count)
                            .filter(|(_, count)| *count >= 2) // Need at least 2 files to establish dominance
                            .map(|(lang, _)| lang.to_string())
                    };

                    let before = resp_files.len();
                    let non_affinity_gate = gate + cross_repo_penalty;
                    // Language mismatch adds 0.05 extra penalty on top of cross-repo penalty
                    let lang_mismatch_penalty: f32 = 0.05;
                    resp_files.retain(|f| {
                        let is_affinity = f.repo.as_deref() == Some(affinity.as_str())
                            || f.path.contains(affinity.as_str());
                        if is_affinity {
                            true // Always keep affinity results
                        } else {
                            // Check for language mismatch
                            let effective_gate = if let Some(ref aff_lang) = affinity_lang {
                                if !f.language.is_empty()
                                    && f.language != "markdown"
                                    && f.language != *aff_lang
                                {
                                    non_affinity_gate + lang_mismatch_penalty
                                } else {
                                    non_affinity_gate
                                }
                            } else {
                                non_affinity_gate
                            };
                            // Non-affinity must have at least one chunk above the effective gate
                            f.chunks.iter().any(|c| c.score >= effective_gate)
                        }
                    });
                    let removed = before - resp_files.len();
                    if removed > 0 {
                        eprintln!(
                            "bobbin: cross-repo gate filtered {} non-affinity files (gate={:.3}, lang={:?}, intent={:?})",
                            removed, non_affinity_gate, affinity_lang, intent,
                        );
                    }
                }
            }

            // Max chunks cap: prevent context flooding when many files pass the gate.
            // Keep files in order (highest relevance first), drop trailing files once
            // total chunk count exceeds the cap.
            // What the cap held back, carried rather than only printed. The
            // eprintln below stays — it is useful to an operator — but a hook's
            // stderr is not injected, so until this counter existed the MODEL
            // saw a context block that looked complete and had no way to learn
            // it was not.
            let mut omitted_files = 0usize;
            {
                let max_chunks: usize = 12; // Cap at 12 chunks per injection
                let mut running = 0usize;
                let mut keep = resp_files.len();
                for (i, f) in resp_files.iter().enumerate() {
                    running += f.chunks.len();
                    if running > max_chunks {
                        keep = i + 1; // Keep this file (partially over) but drop the rest
                        break;
                    }
                }
                if keep < resp_files.len() {
                    let dropped = resp_files.len() - keep;
                    omitted_files = dropped;
                    eprintln!(
                        "bobbin: chunks cap dropped {} trailing files ({} chunks > {})",
                        dropped, running, max_chunks
                    );
                    resp_files.truncate(keep);
                }
            }

            // Rebuild response with updated counts
            let total_chunks: usize = resp_files.iter().map(|f| f.chunks.len()).sum();
            let resp = crate::http::client::ContextResponse {
                query: resp_query,
                budget: resp_budget,
                files: resp_files,
                summary: crate::http::client::ContextSummaryOutput {
                    total_files: 0, // set below
                    total_chunks,
                    omitted_files,
                    ..resp_summary
                },
            };
            let resp = crate::http::client::ContextResponse {
                summary: crate::http::client::ContextSummaryOutput {
                    total_files: resp.files.len(),
                    ..resp.summary
                },
                ..resp
            };

            // Generate injection_id and format structured context output
            let injection_id = generate_context_injection_id(prompt);

            // Check for bundle keyword matches (local tags.toml, then global)
            let mut tags_config = find_bobbin_root(&cwd)
                .map(|root| {
                    crate::tags::TagsConfig::load_or_default(&crate::tags::TagsConfig::tags_path(
                        &root,
                    ))
                })
                .unwrap_or_default();
            if tags_config.bundles.is_empty() {
                if let Some(global_dir) = Config::global_config_dir() {
                    let global_tags = global_dir.join("tags.toml");
                    if global_tags.exists() {
                        let global = crate::tags::TagsConfig::load_or_default(&global_tags);
                        if !global.bundles.is_empty() {
                            tags_config.bundles = global.bundles;
                        }
                    }
                }
            }
            let matched_bundles: Vec<crate::tags::BundleConfig> = tags_config
                .match_bundle_keywords(search_query)
                .into_iter()
                .map(|(b, _)| b.clone())
                .collect();

            let out = format_context_response_with_bundles(
                &resp,
                budget,
                hooks_cfg.show_docs,
                &injection_id,
                format_mode,
                &matched_bundles,
                hooks_cfg.bundle_auto_inject,
                hooks_cfg.bundle_inject_lines,
                hooks_cfg.bundle_max_inject,
            );
            // Read from the graph here rather than from the server's reply, so
            // this path and the local one surface identical boundaries and no
            // index host can suppress one. See `src/tripwire/mod.rs`.
            let out = crate::tripwire::with_boundaries(
                out,
                &config,
                resp.files.iter().map(|f| f.path.clone()).collect(),
                &repo_root,
                format_mode,
            )
            .await;
            print!("{}", out);

            // Record injected chunks in session ledger
            if reducing_enabled {
                let chunk_keys: Vec<String> = resp
                    .files
                    .iter()
                    .flat_map(|f| {
                        f.chunks
                            .iter()
                            .map(|c| chunk_key(&f.path, c.start_line, c.end_line))
                    })
                    .collect();
                ledger.record(&chunk_keys, &injection_id);
            }

            // Emit injection metric
            let files_json: Vec<String> = resp.files.iter().map(|f| f.path.clone()).collect();
            let total_chunks: usize = resp.files.iter().map(|f| f.chunks.len()).sum();
            crate::metrics::emit(
                &repo_root,
                &crate::metrics::event(
                    &metrics_source,
                    "hook_injection",
                    "hook inject-context-remote",
                    hook_start.elapsed().as_millis() as u64,
                    serde_json::json!({
                        "query": &prompt[..prompt.len().min(200)],
                        "top_score": top_score,
                        "gate_threshold": gate,
                        "intent": format!("{:?}", intent),
                        "gate_boost": intent_adj.gate_boost,
                        "semantic_weight_override": semantic_weight_override,
                        "doc_demotion_override": doc_demotion_override,
                        "recency_weight_override": recency_weight_override,
                        "files_returned": &files_json,
                        "chunks_returned": total_chunks,
                        "injection_id": &injection_id,
                    }),
                ),
            );

            // Store injection payload server-side (best-effort, don't block)
            let session_id = if input.session_id.is_empty() {
                None
            } else {
                Some(input.session_id.as_str())
            };
            let _ = client
                .store_injection_with_output(
                    &injection_id,
                    session_id,
                    None, // agent resolved server-side or by feedback submitter
                    prompt,
                    &files_json,
                    total_chunks,
                    budget,
                    Some(&out),
                )
                .await;

            // Mirror the local inject path: advance the client-side counter so
            // `hook status` reflects remote injections too (bobbin #42, Bug A).
            // Without this, hook_state.json is only ever written by the local
            // path, which is never taken when BOBBIN_SERVER is set — freezing
            // the reported injection count at its last local value.
            let mut state = load_hook_state(&repo_root);
            state.injection_count += 1;
            state.last_injection_time = chrono::Utc::now().to_rfc3339();
            save_hook_state(&repo_root, &state);

            Ok(())
        }
        Err(_) => {
            // Fallback: /context endpoint unavailable, use /search
            let session_id = if input.session_id.is_empty() {
                None
            } else {
                Some(input.session_id.as_str())
            };
            inject_context_remote_search_fallback(
                &client,
                search_query,
                budget,
                hooks_cfg.show_docs,
                gate,
                output,
                Some(&role),
                session_id,
                format_mode,
                repo_filter.as_deref(),
            )
            .await
        }
    }
}

/// Format a ContextResponse into structured text for injection, with optional bundle annotations.
fn format_context_response_with_bundles(
    resp: &crate::http::client::ContextResponse,
    budget: usize,
    show_docs: bool,
    injection_id: &str,
    format_mode: &str,
    matched_bundles: &[crate::tags::BundleConfig],
    bundle_auto_inject: bool,
    bundle_inject_lines: usize,
    bundle_max_inject: usize,
) -> String {
    format_context_response_inner(
        resp,
        budget,
        show_docs,
        injection_id,
        format_mode,
        matched_bundles,
        bundle_auto_inject,
        bundle_inject_lines,
        bundle_max_inject,
    )
}

fn format_context_response_inner(
    resp: &crate::http::client::ContextResponse,
    budget: usize,
    show_docs: bool,
    injection_id: &str,
    format_mode: &str,
    matched_bundles: &[crate::tags::BundleConfig],
    bundle_auto_inject: bool,
    bundle_inject_lines: usize,
    bundle_max_inject: usize,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    // Header with summary and injection_id for feedback reference
    match format_mode {
        "xml" => {
            let _ = writeln!(
                out,
                "<bobbin-context files=\"{}\" direct=\"{}\" coupled=\"{}\" bridged=\"{}\" chunks=\"{}\" budget=\"{}\" omitted=\"{}\" injection_id=\"{}\">",
                resp.summary.total_files,
                resp.summary.direct_hits,
                resp.summary.coupled_additions,
                resp.summary.bridged_additions,
                resp.summary.total_chunks,
                budget,
                resp.summary.omitted_files,
                injection_id,
            );
        }
        "minimal" => {
            let _ = writeln!(
                out,
                "# Bobbin context ({} files, {}/{} lines, {} omitted) [injection_id: {}]",
                resp.summary.total_files,
                resp.summary.total_chunks,
                budget,
                resp.summary.omitted_files,
                injection_id,
            );
        }
        _ => {
            let _ = writeln!(
                out,
                "Bobbin found {} relevant files ({} direct, {} coupled, {} bridged, {}/{} budget lines, {} held back) [injection_id: {}]:",
                resp.summary.total_files,
                resp.summary.direct_hits,
                resp.summary.coupled_additions,
                resp.summary.bridged_additions,
                resp.summary.total_chunks,
                budget,
                resp.summary.omitted_files,
                injection_id,
            );
        }
    }

    // Bundle annotations — either inline content or signpost
    if !matched_bundles.is_empty() {
        let inject_count = matched_bundles.len().min(bundle_max_inject);
        let _ = writeln!(out);
        for (i, bundle) in matched_bundles.iter().enumerate() {
            let _ = writeln!(
                out,
                "📦 bundle:{} — \"{}\"",
                bundle.name, bundle.description
            );
            if bundle_auto_inject && i < inject_count {
                // Render compact inline content: refs, files, docs, sub-bundles
                let mut lines_used = 0;
                let max_lines = bundle_inject_lines;

                // Refs (most valuable — specific symbols)
                if !bundle.refs.is_empty() && lines_used < max_lines {
                    let _ = writeln!(out, "   Refs:");
                    lines_used += 1;
                    for ref_str in &bundle.refs {
                        if lines_used >= max_lines {
                            break;
                        }
                        if let Some(parsed) = crate::tags::BundleRef::parse(ref_str) {
                            let _ = writeln!(out, "   - {}", parsed.display_l0());
                        } else {
                            let _ = writeln!(out, "   - {}", ref_str);
                        }
                        lines_used += 1;
                    }
                }

                // Files
                if !bundle.files.is_empty() && lines_used < max_lines {
                    let _ = writeln!(out, "   Files:");
                    lines_used += 1;
                    for f in &bundle.files {
                        if lines_used >= max_lines {
                            break;
                        }
                        let _ = writeln!(out, "   - {}", f);
                        lines_used += 1;
                    }
                }

                // Docs
                if !bundle.docs.is_empty() && lines_used < max_lines {
                    let _ = writeln!(out, "   Docs:");
                    lines_used += 1;
                    for d in &bundle.docs {
                        if lines_used >= max_lines {
                            break;
                        }
                        let _ = writeln!(out, "   - {}", d);
                        lines_used += 1;
                    }
                }

                // Beads
                if !bundle.beads.is_empty() && lines_used < max_lines {
                    let _ = writeln!(out, "   Beads:");
                    lines_used += 1;
                    for b in &bundle.beads {
                        if lines_used >= max_lines {
                            break;
                        }
                        let _ = writeln!(out, "   - bead:{}", b);
                        lines_used += 1;
                    }
                }

                // Includes (other bundles)
                if !bundle.includes.is_empty() && lines_used < max_lines {
                    let _ = writeln!(out, "   Includes: {}", bundle.includes.join(", "));
                }

                let _ = writeln!(
                    out,
                    "   → `bobbin bundle show {} --deep` for full source",
                    bundle.name
                );
            } else {
                let _ = writeln!(
                    out,
                    "   → `bobbin bundle show {}` for full context",
                    bundle.name
                );
            }
        }
    }

    // Partition files by type
    let is_doc = |path: &str| -> bool {
        path.ends_with(".md")
            || path.ends_with(".txt")
            || path.ends_with(".rst")
            || path.ends_with(".adoc")
            || path.contains("/docs/")
    };

    let source_files: Vec<_> = resp.files.iter().filter(|f| !is_doc(&f.path)).collect();
    let doc_files: Vec<_> = resp.files.iter().filter(|f| is_doc(&f.path)).collect();

    let mut line_count = out.lines().count();
    // Files the line budget cut off, so the marker below can name a number
    // rather than the output simply ending.
    let mut cut_by_budget = 0usize;

    if !source_files.is_empty() {
        match format_mode {
            "xml" => { /* no section header in xml mode */ }
            "minimal" => { /* no section header in minimal mode */ }
            _ => {
                let _ = write!(out, "\n=== Source Files ===\n");
                line_count += 2;
            }
        }
        cut_by_budget += format_remote_file_chunks(
            &mut out,
            &source_files,
            budget,
            &mut line_count,
            format_mode,
        );
    }

    if show_docs && !doc_files.is_empty() {
        match format_mode {
            "xml" => { /* no section header in xml mode */ }
            "minimal" => { /* no section header in minimal mode */ }
            _ => {
                let _ = write!(out, "\n=== Documentation ===\n");
                line_count += 2;
            }
        }
        cut_by_budget +=
            format_remote_file_chunks(&mut out, &doc_files, budget, &mut line_count, format_mode);
    }

    // The budget cut is reported HERE rather than in the header, because the
    // header is written before any chunk is rendered and cannot know. A marker
    // at the point of the cut is also the more honest placement: it says where
    // the output stopped being complete.
    if cut_by_budget > 0 {
        let _ = write!(
            out,
            "\n[bobbin: line budget of {budget} reached — {cut_by_budget} more file(s) matched \
             but are not shown. This context is INCOMPLETE; ask for the rest by \
             injection_id if you need it.]\n"
        );
    }

    if format_mode == "xml" {
        let _ = write!(out, "</bobbin-context>\n");
    }

    out
}

/// Fallback: use /search when /context endpoint is unavailable (e.g., behind
/// a Traefik proxy that only forwards certain paths).
async fn inject_context_remote_search_fallback(
    client: &crate::http::client::Client,
    prompt: &str,
    budget: usize,
    show_docs: bool,
    gate: f32,
    output: &OutputConfig,
    role: Option<&str>,
    session_id: Option<&str>,
    format_mode: &str,
    repo_filter: Option<&str>,
) -> Result<()> {
    let resp = client
        .search(prompt, "hybrid", repo_filter, 10, None, role)
        .await
        .context("Remote search failed")?;

    if resp.results.is_empty() {
        return Ok(());
    }

    // Gate check
    let top_score = resp.results.first().map(|r| r.score).unwrap_or(0.0);
    if top_score < gate {
        if output.verbose {
            eprintln!(
                "bobbin: skipped (score={:.3} < gate={:.3})",
                top_score, gate,
            );
        }
        return Ok(());
    }

    let result_count = resp.results.iter().filter(|r| r.score >= gate).count();
    if result_count == 0 {
        return Ok(());
    }

    let injection_id = generate_context_injection_id(prompt);
    let mut out = format_search_fallback_header(result_count, &injection_id, format_mode);

    let mut line_count = out.lines().count();
    for result in &resp.results {
        if result.score < gate {
            continue;
        }

        // Skip docs if show_docs is false
        if !show_docs && (result.file_path.ends_with(".md") || result.file_path.contains("/docs/"))
        {
            continue;
        }

        let name = result
            .name
            .as_ref()
            .map(|n| format!(" {}", n))
            .unwrap_or_default();
        let chunk_section = format_search_chunk(
            &result.file_path,
            result.start_line,
            result.end_line,
            &name,
            &result.chunk_type,
            result.score,
            &result.content_preview,
            "",
            format_mode,
        );

        let chunk_line_count = chunk_section.lines().count();
        if line_count + chunk_line_count > budget {
            break;
        }
        line_count += chunk_line_count;
        out.push_str(&chunk_section);
    }

    if format_mode == "xml" {
        out.push_str("</bobbin-context>\n");
    }

    print!("{}", out);

    // Store injection payload server-side (best-effort)
    let files_json: Vec<String> = resp
        .results
        .iter()
        .filter(|r| r.score >= 0.005)
        .map(|r| r.file_path.clone())
        .collect();
    let _ = client
        .store_injection_with_output(
            &injection_id,
            session_id,
            None,
            prompt,
            &files_json,
            result_count,
            budget,
            Some(&out),
        )
        .await;

    Ok(())
}

/// Format chunks from remote context response files into output string.
///
/// Returns the number of files it did NOT render because the line budget ran
/// out. The budget stop used to be a bare `return`: the output simply ended,
/// and an agent reading it could not distinguish "that was everything" from
/// "we ran out of room". Reporting the cut is the whole difference between a
/// budget and a silent loss.
#[must_use]
fn format_remote_file_chunks(
    out: &mut String,
    files: &[&crate::http::client::ContextFileOutput],
    budget: usize,
    line_count: &mut usize,
    format_mode: &str,
) -> usize {
    use std::fmt::Write;

    for (idx, file) in files.iter().enumerate() {
        // Build display path with repo prefix when available and not already present
        let display_path = match &file.repo {
            Some(repo)
                if !file.path.starts_with("repos/")
                    && !file.path.starts_with("/")
                    && !file.path.starts_with("beads:") =>
            {
                format!("repos/{}/{}", repo, file.path)
            }
            _ => file.path.clone(),
        };

        // Show coupling info if present
        let relevance_info = if file.relevance == "structural" {
            format!(" [{}]", file.coupled_to.join(", "))
        } else if !file.coupled_to.is_empty() {
            format!(" [coupled via {}]", file.coupled_to.join(", "))
        } else if file.relevance == "bridged" {
            " [bridged from docs]".to_string()
        } else {
            String::new()
        };

        for chunk in &file.chunks {
            let name = chunk
                .name
                .as_ref()
                .map(|n| format!(" {}", n))
                .unwrap_or_default();
            let content_str = chunk.content.as_deref().unwrap_or("");
            let chunk_section = format_search_chunk(
                &display_path,
                chunk.start_line,
                chunk.end_line,
                &name,
                &chunk.chunk_type,
                chunk.score,
                content_str,
                &relevance_info,
                format_mode,
            );

            let chunk_line_count = chunk_section.lines().count();
            if *line_count + chunk_line_count > budget {
                // Everything from this file onward is unrendered, including the
                // rest of this one — counted as a whole file because a partly
                // shown file is still a file the agent has not fully seen.
                return files.len() - idx;
            }
            *line_count += chunk_line_count;
            let _ = write!(out, "{}", chunk_section);
        }
    }
    0
}

/// Format a single chunk according to the injection format mode.
fn format_search_chunk(
    path: &str,
    start_line: u32,
    end_line: u32,
    name: &str,
    chunk_type: &str,
    score: f32,
    content: &str,
    relevance_info: &str,
    format_mode: &str,
) -> String {
    let content_suffix = if content.ends_with('\n') { "" } else { "\n" };
    match format_mode {
        "minimal" => {
            // Clean, minimal format — just path and content, no metadata noise
            format!(
                "\n# {} (lines {}-{})\n{}{}",
                path, start_line, end_line, content, content_suffix,
            )
        }
        "verbose" => {
            // Standard format + explicit name/type on separate line for clarity
            let mut s = format!(
                "\n--- {}:{}-{}{} ({}, score {:.2}){} ---\n",
                path, start_line, end_line, name, chunk_type, score, relevance_info,
            );
            if !name.is_empty() {
                s.push_str(&format!("  // {}{}\n", chunk_type, name));
            }
            s.push_str(content);
            s.push_str(content_suffix);
            s
        }
        "xml" => {
            // XML-structured format for LLMs that may parse structure better
            let name_attr = if name.is_empty() {
                String::new()
            } else {
                format!(" name=\"{}\"", name.trim())
            };
            let rel_attr = if relevance_info.is_empty() {
                String::new()
            } else {
                format!(
                    " relevance=\"{}\"",
                    relevance_info.trim().trim_matches(|c| c == '[' || c == ']')
                )
            };
            format!(
                "<file path=\"{}\" lines=\"{}-{}\" type=\"{}\" score=\"{:.2}\"{}{}>
{}{}</file>\n",
                path,
                start_line,
                end_line,
                chunk_type,
                score,
                name_attr,
                rel_attr,
                content,
                content_suffix,
            )
        }
        _ => {
            // "standard" — the current default format
            format!(
                "\n--- {}:{}-{}{} ({}, score {:.2}){} ---\n{}{}",
                path,
                start_line,
                end_line,
                name,
                chunk_type,
                score,
                relevance_info,
                content,
                content_suffix,
            )
        }
    }
}

/// Format the header for search fallback injection.
fn format_search_fallback_header(
    result_count: usize,
    injection_id: &str,
    format_mode: &str,
) -> String {
    match format_mode {
        "xml" => format!(
            "<bobbin-context chunks=\"{}\" mode=\"search-fallback\" injection_id=\"{}\">\n",
            result_count, injection_id,
        ),
        "minimal" => format!(
            "# Bobbin context ({} chunks, search fallback) [injection_id: {}]\n",
            result_count, injection_id,
        ),
        _ => {
            let mut out = format!(
                "Bobbin found {} relevant chunks (via search fallback) [injection_id: {}]:\n",
                result_count, injection_id,
            );
            out.push_str("\n=== Source Files ===\n");
            out
        }
    }
}

/// Claude Code UserPromptSubmit hook input (subset of fields we need)
#[derive(Deserialize)]
struct HookInput {
    /// The user's prompt text
    #[serde(default)]
    prompt: String,
    /// Working directory when the hook was invoked
    #[serde(default)]
    cwd: String,
    /// Claude Code session ID (used as metrics source identity)
    #[serde(default)]
    session_id: String,
    /// Trusted turn-boundary grounding reference supplied by the NA harness.
    #[serde(default)]
    grounding: Option<NaGroundingRef>,
}

#[derive(Clone, Deserialize)]
struct NaGroundingRef {
    scope: Option<String>,
    grounding_id: Option<String>,
    faction_id: Option<String>,
    worldview_sha256: Option<String>,
}

#[derive(Deserialize)]
struct NaGroundingEvidence {
    graph: String,
    query: String,
    entities: Vec<String>,
    turn: u64,
    outcome: String,
    faction_id: String,
    worldview_sha256: String,
}

struct VerifiedNaGrounding {
    faction_id: String,
    grounding_id: String,
    text: String,
}

/// Resolve the NA harness reference locally. Unknown, partial, tampered, and
/// cross-faction evidence all abstain: no unscoped fact reaches an injection.
fn verified_na_grounding(
    reference: Option<&NaGroundingRef>,
) -> Result<Option<VerifiedNaGrounding>> {
    let cache = std::env::var_os("YUPANA_GROUNDING_CACHE_DIR")
        .or_else(|| std::env::var_os("HANK_GROUNDING_CACHE_DIR"))
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .map(|p| p.join("yupana/grounding"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".local/state/yupana/grounding"))
        });
    verified_na_grounding_at(reference, cache.as_deref())
}

fn verified_na_grounding_at(
    reference: Option<&NaGroundingRef>,
    cache: Option<&Path>,
) -> Result<Option<VerifiedNaGrounding>> {
    let Some(reference) = reference else {
        return Ok(None);
    };
    if reference.scope.as_deref() != Some("na") {
        anyhow::bail!("NA grounding scope is missing or unknown");
    }
    let grounding_id = reference
        .grounding_id
        .as_deref()
        .context("NA grounding_id missing")?;
    let faction_id = reference
        .faction_id
        .as_deref()
        .context("NA faction_id missing")?;
    let worldview = reference
        .worldview_sha256
        .as_deref()
        .context("NA worldview hash missing")?;
    let digest = grounding_id
        .strip_prefix("sha256:")
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("NA grounding_id is not sha256:<hex>")?;
    let cache = cache.context("NA grounding cache is not configured")?;
    let body = std::fs::read(cache.join(format!("{digest}.json")))?;
    use sha2::{Digest, Sha256};
    if hex::encode(Sha256::digest(&body)) != digest.to_ascii_lowercase() {
        anyhow::bail!("NA grounding evidence digest mismatch");
    }
    let evidence: NaGroundingEvidence = serde_json::from_slice(&body)?;
    if evidence.faction_id != faction_id || evidence.worldview_sha256 != worldview {
        anyhow::bail!("NA grounding evidence crosses faction or worldview scope");
    }
    let entities = if evidence.entities.is_empty() {
        "(none)".to_string()
    } else {
        evidence.entities.join(", ")
    };
    let text = format!(
        "NA turn grounding [injection scope: faction={faction_id}, grounding_id={grounding_id}]\nGraph: {}\nTurn: {}\nOutcome: {}\nEntities: {}\nQuery: {}\n",
        evidence.graph, evidence.turn, evidence.outcome, entities, evidence.query,
    );
    Ok(Some(VerifiedNaGrounding {
        faction_id: faction_id.to_string(),
        grounding_id: grounding_id.to_string(),
        text,
    }))
}

/// Generate a unique injection_id for a context injection.
/// Format: `inj-<8 hex chars>` (compact, unique per query+time).
fn generate_context_injection_id(query: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(query.as_bytes());
    hasher.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes(),
    );
    let hash = hex::encode(hasher.finalize());
    let id = format!("inj-{}", &hash[..8]);
    hook_memory::note_injection_id(&id);
    id
}

/// Claude Code PostToolUse hook input
#[derive(Deserialize)]
struct PostToolUseInput {
    /// Tool name (e.g., "Write", "Edit", "Bash")
    #[serde(default)]
    tool_name: String,
    /// Tool input parameters
    #[serde(default)]
    tool_input: serde_json::Value,
    /// Working directory when the hook was invoked
    #[serde(default)]
    cwd: String,
    /// Claude Code session ID
    #[serde(default)]
    session_id: String,
    /// What the tool actually FOUND. Claude Code has always sent this; the field was
    /// simply never declared, so serde dropped it silently (bobbin issue #51 F1).
    /// `#[serde(default)]` keeps every existing payload parsing to `Value::Null`.
    #[serde(default)]
    tool_response: serde_json::Value,
    #[serde(default)]
    grounding: Option<NaGroundingRef>,
}

/// Claude Code PostToolUseFailure hook input
#[derive(Deserialize)]
struct PostToolUseFailureInput {
    /// Tool name (e.g., "Bash", "Write", "Edit")
    #[serde(default)]
    tool_name: String,
    /// Tool input parameters
    #[serde(default)]
    tool_input: serde_json::Value,
    /// Error message from the failed tool
    #[serde(default)]
    error: String,
    /// Working directory when the hook was invoked
    #[serde(default)]
    cwd: String,
    /// Claude Code session ID
    #[serde(default)]
    session_id: String,
}

/// Walk up from `start` looking for a directory containing `.bobbin/config.toml`.
fn find_bobbin_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if Config::config_path(&dir).exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Format a context bundle into a compact text block for Claude Code injection.
///
/// Produces a plain-text summary of relevant code chunks, enforcing a hard line
/// budget on the output. The `threshold` filters out low-scoring chunks. The
/// output budget is taken from `bundle.budget.max_lines`.
fn format_context_for_injection(
    bundle: &crate::search::context::ContextBundle,
    threshold: f32,
    show_docs: bool,
    injection_id: Option<&str>,
    format_mode: &str,
) -> String {
    use std::fmt::Write;

    let budget = bundle.budget.max_lines;
    let mut out = String::new();

    // Format header based on mode
    match format_mode {
        "xml" => {
            let inj_attr = injection_id
                .map(|id| format!(" injection_id=\"{}\"", id))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "<bobbin-context files=\"{}\" source=\"{}\" docs=\"{}\" lines=\"{}/{}\"{}>\n",
                bundle.summary.total_files,
                bundle.summary.source_files,
                bundle.summary.doc_files,
                bundle.budget.used_lines,
                bundle.budget.max_lines,
                inj_attr,
            );
        }
        "minimal" => {
            let inj_suffix = injection_id
                .map(|id| format!(" [injection_id: {}]", id))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "# Bobbin context ({} files, {}/{} lines){}",
                bundle.summary.total_files,
                bundle.budget.used_lines,
                bundle.budget.max_lines,
                inj_suffix,
            );
        }
        _ => {
            let header = if let Some(inj_id) = injection_id {
                format!(
                    "Bobbin found {} relevant files ({} source, {} docs, {}/{} budget lines) [injection_id: {}]:",
                    bundle.summary.total_files,
                    bundle.summary.source_files,
                    bundle.summary.doc_files,
                    bundle.budget.used_lines,
                    bundle.budget.max_lines,
                    inj_id,
                )
            } else {
                format!(
                    "Bobbin found {} relevant files ({} source, {} docs, {}/{} budget lines):",
                    bundle.summary.total_files,
                    bundle.summary.source_files,
                    bundle.summary.doc_files,
                    bundle.budget.used_lines,
                    bundle.budget.max_lines,
                )
            };
            out.push_str(&header);
            out.push('\n');
        }
    }

    // Partition files: source/test/custom first, then docs/config
    let source_files: Vec<_> = bundle
        .files
        .iter()
        .filter(|f| !f.category.is_doc_like())
        .collect();
    let doc_files: Vec<_> = bundle
        .files
        .iter()
        .filter(|f| f.category.is_doc_like())
        .collect();

    // Emit source files section
    if !source_files.is_empty() {
        if format_mode != "xml" && format_mode != "minimal" {
            let _ = write!(out, "\n=== Source Files ===\n");
        }
        format_file_chunks(&mut out, &source_files, threshold, budget, format_mode);
    }

    // Emit documentation section (if show_docs is true)
    if show_docs && !doc_files.is_empty() {
        if format_mode != "xml" && format_mode != "minimal" {
            let _ = write!(out, "\n=== Documentation ===\n");
        }
        format_file_chunks(&mut out, &doc_files, threshold, budget, format_mode);
    }

    if format_mode == "xml" {
        let _ = write!(out, "</bobbin-context>\n");
    }

    // Final enforcement: trim to budget
    let lines: Vec<&str> = out.lines().collect();
    if lines.len() > budget {
        lines[..budget].join("\n") + "\n"
    } else {
        out
    }
}

/// Format chunks from a list of files into the output string, respecting budget.
fn format_file_chunks(
    out: &mut String,
    files: &[&crate::search::context::ContextFile],
    threshold: f32,
    budget: usize,
    format_mode: &str,
) {
    use std::fmt::Write;

    // Track line count incrementally to avoid O(n²) recounting
    let mut current_lines = out.lines().count();

    for file in files {
        // Build display path with repo prefix when available and not already present
        let display_path = match &file.repo {
            Some(repo)
                if !file.path.starts_with("repos/")
                    && !file.path.starts_with("/")
                    && !file.path.starts_with("beads:") =>
            {
                format!("repos/{}/{}", repo, file.path)
            }
            _ => file.path.clone(),
        };

        for chunk in &file.chunks {
            if chunk.score < threshold {
                continue;
            }
            let name = chunk
                .name
                .as_ref()
                .map(|n| format!(" {}", n))
                .unwrap_or_default();
            let content_str = chunk.content.as_deref().unwrap_or("");
            let chunk_type_str = serde_json::to_string(&chunk.chunk_type)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let chunk_section = format_search_chunk(
                &display_path,
                chunk.start_line,
                chunk.end_line,
                &name,
                &chunk_type_str,
                chunk.score,
                content_str,
                "",
                format_mode,
            );

            // Check if adding this chunk would exceed budget
            let chunk_line_count = chunk_section.lines().count();
            if current_lines + chunk_line_count > budget {
                return;
            }
            current_lines += chunk_line_count;
            let _ = write!(out, "{}", chunk_section);
        }
    }
}

/// Persistent state for hook dedup and frequency tracking.
/// Stored in `.bobbin/hook_state.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HookState {
    #[serde(default)]
    last_session_id: String,
    #[serde(default)]
    last_injected_chunks: Vec<String>,
    #[serde(default)]
    last_injection_time: String,
    #[serde(default)]
    injection_count: u64,
    #[serde(default)]
    chunk_frequencies: HashMap<String, ChunkFrequency>,
    #[serde(default)]
    file_frequencies: HashMap<String, u64>,
    #[serde(default)]
    hot_topics_generated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkFrequency {
    count: u64,
    file: String,
    name: Option<String>,
}

fn hook_state_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".bobbin").join("hook_state.json")
}

/// Load hook state from disk. Returns default on any error.
fn load_hook_state(repo_root: &Path) -> HookState {
    let path = hook_state_path(repo_root);
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HookState::default(),
    }
}

/// Save hook state to disk. Errors are swallowed (never block prompts).
fn save_hook_state(repo_root: &Path, state: &HookState) {
    let path = hook_state_path(repo_root);
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&path, json);
    }
}

/// Compute a session ID from the context bundle's chunks.
///
/// Takes the chunk composite keys (`file:start:end`), filters by threshold,
/// keeps the **ten highest-scoring**, then sorts those alphabetically,
/// concatenates with `|`, and returns the first 16 hex chars of the SHA-256
/// hash.
///
/// The two sorts are both load-bearing and their order is the whole point
/// (bobbin-zhx). This previously sorted alphabetically and *then* truncated,
/// so which chunks entered the fingerprint was decided by path lexicography —
/// a chunk under `src/a…` displaced a far more relevant one under `src/z…`,
/// and `c.score` was in scope and discarded. Selection must be by score; the
/// final re-sort must happen **after** the truncate, so the hash depends only
/// on the *set* of chunks selected and not on their score order. Sorting by
/// score and hashing in that order would churn the fingerprint every time two
/// chunks' scores swapped without the selected set changing.
///
/// Ties break by key. `sort_by` is stable, so without that the tie-break would
/// fall through to bundle iteration order and make the fingerprint depend on
/// assembly-pipeline ordering rather than on content.
fn compute_session_id(bundle: &crate::search::context::ContextBundle, threshold: f32) -> String {
    use sha2::{Digest, Sha256};
    use std::cmp::Ordering;

    let mut scored: Vec<(f32, String)> = bundle
        .files
        .iter()
        .flat_map(|f| {
            f.chunks
                .iter()
                .filter(|c| c.score >= threshold)
                .map(move |c| {
                    (
                        c.score,
                        format!("{}:{}:{}", f.path, c.start_line, c.end_line),
                    )
                })
        })
        .collect();

    // f32 has no total order, hence partial_cmp with a fallback rather than
    // sort_by_key. Descending by score, then ascending by key for determinism.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    scored.truncate(10);

    let mut keys: Vec<String> = scored.into_iter().map(|(_, key)| key).collect();
    keys.sort(); // AFTER the truncate — hash over the set, not the score order

    let joined = keys.join("|");
    let hash = Sha256::digest(joined.as_bytes());
    hex::encode(&hash[..8]) // 8 bytes = 16 hex chars
}

// ---------------------------------------------------------------------------
// Session Ledger: tracks chunks injected across turns for progressive reducing
// ---------------------------------------------------------------------------

/// A record of a chunk that was injected in a previous turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerEntry {
    chunk_key: String,
    injection_id: String,
    turn: u64,
}

/// Session-level ledger tracking all chunks injected so far.
/// Stored as JSONL at `.bobbin/session/<cc_session_id>/ledger.jsonl`.
struct SessionLedger {
    entries: HashSet<String>, // chunk_keys for fast lookup
    turn: u64,
    path: Option<PathBuf>,
}

impl SessionLedger {
    /// Load ledger for a Claude Code session. Returns empty ledger if session_id
    /// is empty or file doesn't exist.
    fn load(repo_root: &Path, cc_session_id: &str) -> Self {
        if cc_session_id.is_empty() {
            return Self {
                entries: HashSet::new(),
                turn: 0,
                path: None,
            };
        }
        let dir = repo_root
            .join(".bobbin")
            .join("session")
            .join(cc_session_id);
        let path = dir.join("ledger.jsonl");

        let mut entries = HashSet::new();
        let mut max_turn = 0u64;

        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    if let Ok(entry) = serde_json::from_str::<LedgerEntry>(line) {
                        if entry.turn > max_turn {
                            max_turn = entry.turn;
                        }
                        entries.insert(entry.chunk_key);
                    }
                }
            }
        }

        Self {
            entries,
            turn: max_turn,
            path: Some(path),
        }
    }

    /// Check if a chunk was already injected in a previous turn.
    fn contains(&self, chunk_key: &str) -> bool {
        self.entries.contains(chunk_key)
    }

    /// Record newly injected chunks. Appends to the JSONL file.
    fn record(&mut self, chunk_keys: &[String], injection_id: &str) {
        let new_turn = self.turn + 1;
        self.turn = new_turn;

        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Append new entries
            let mut lines = String::new();
            for key in chunk_keys {
                if let Ok(json) = serde_json::to_string(&LedgerEntry {
                    chunk_key: key.clone(),
                    injection_id: injection_id.to_string(),
                    turn: new_turn,
                }) {
                    lines.push_str(&json);
                    lines.push('\n');
                }
                self.entries.insert(key.clone());
            }
            if !lines.is_empty() {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = f.write_all(lines.as_bytes());
                }
            }
        } else {
            // In-memory only (no session_id)
            for key in chunk_keys {
                self.entries.insert(key.clone());
            }
        }
    }

    /// Clear the ledger (used on compaction reset).
    fn clear(repo_root: &Path, cc_session_id: &str) {
        if cc_session_id.is_empty() {
            return;
        }
        let path = repo_root
            .join(".bobbin")
            .join("session")
            .join(cc_session_id)
            .join("ledger.jsonl");
        let _ = std::fs::remove_file(&path);
    }

    /// Number of unique chunks tracked.
    fn len(&self) -> usize {
        self.entries.len()
    }

    /// Get unique file paths from previously injected chunks.
    /// Chunk keys are formatted as "file_path:start_line:end_line".
    fn injected_files(&self) -> Vec<String> {
        let mut files: HashSet<String> = HashSet::new();
        for key in &self.entries {
            // Find the last two colons to extract the file path
            // Keys are "path:start:end" — path itself may contain colons on Windows
            if let Some(last_colon) = key.rfind(':') {
                if let Some(second_colon) = key[..last_colon].rfind(':') {
                    let file = &key[..second_colon];
                    files.insert(file.to_string());
                }
            }
        }
        files.into_iter().collect()
    }
}

/// Build a chunk key from file path and chunk line range.
fn chunk_key(file_path: &str, start_line: u32, end_line: u32) -> String {
    format!("{}:{}:{}", file_path, start_line, end_line)
}

/// One injecting turn on a non-prompt event class (bobbin-aa0).
///
/// Prompt submission had all of this and the other injecting paths had none of
/// it: the failure handler and the post-tool-use handler generated no injection
/// identifier, stored no injection record — so nothing they delivered could be
/// rated through feedback — and never touched the `SessionLedger`, so they
/// could re-deliver chunks the prompt path had already sent and the delta
/// filtering could not see it.
///
/// This exists as one type rather than as three copies of the same six lines
/// because three copies is how the paths diverged in the first place. It owns
/// the whole turn: which chunks are still fresh, which were claimed, and the
/// single identifier they are all recorded under.
///
/// A turn with no session id degrades to in-memory dedup within the turn. That
/// is deliberate — the caller should not have to branch on whether Claude Code
/// supplied a session id, and de-duplicating a single turn against itself is
/// still worth doing.
struct InjectionTurn {
    ledger: SessionLedger,
    injection_id: String,
    /// Keys claimed this turn, in claim order.
    claimed: Vec<String>,
    /// Claimed set, for within-turn dedup before `record` folds them in.
    claimed_set: HashSet<String>,
    /// False when session reducing is off: nothing is filtered, but the turn
    /// still carries an identifier and still stores a record.
    filtering: bool,
}

impl InjectionTurn {
    /// Open a turn. `query` seeds the injection identifier, exactly as on the
    /// prompt path, so identifiers are the same shape across every event class.
    fn open(repo_root: &Path, cc_session_id: &str, reducing_enabled: bool, query: &str) -> Self {
        let filtering = reducing_enabled && !cc_session_id.is_empty();
        let ledger = if filtering {
            SessionLedger::load(repo_root, cc_session_id)
        } else {
            SessionLedger {
                entries: HashSet::new(),
                turn: 0,
                path: None,
            }
        };
        Self {
            ledger,
            injection_id: generate_context_injection_id(query),
            claimed: Vec::new(),
            claimed_set: HashSet::new(),
            filtering,
        }
    }

    fn injection_id(&self) -> &str {
        &self.injection_id
    }

    /// Claim a chunk for this turn. Returns false if it has already been
    /// delivered — in an earlier turn by any path, or earlier in this one — in
    /// which case the caller must not inject it.
    ///
    /// Within-turn dedup applies even when `filtering` is off, because emitting
    /// the same chunk twice in one response is never wanted.
    fn claim(&mut self, file_path: &str, start_line: u32, end_line: u32) -> bool {
        let key = chunk_key(file_path, start_line, end_line);
        if self.claimed_set.contains(&key) {
            return false;
        }
        if self.filtering && self.ledger.contains(&key) {
            return false;
        }
        self.claimed_set.insert(key.clone());
        self.claimed.push(key);
        true
    }

    /// Claim a whole file, for paths that suggest files rather than inject
    /// chunk bodies (the post-tool-use related-files listing).
    ///
    /// Uses the `path:0:0` marker convention that complementary expansion
    /// already writes to this ledger, so a file suggested here and a file
    /// suggested there collide as they should. Note the deliberate asymmetry:
    /// a marker does **not** suppress a later chunk-level injection of the same
    /// file, because naming a file and showing its contents are different
    /// deliveries and the second is still worth making.
    fn claim_file(&mut self, file_path: &str) -> bool {
        self.claim(file_path, 0, 0)
    }

    fn is_empty(&self) -> bool {
        self.claimed.is_empty()
    }

    fn len(&self) -> usize {
        self.claimed.len()
    }

    /// Distinct file paths among the claimed chunks, for the injection record.
    ///
    /// Parses back out of the composite key rather than tracking paths
    /// alongside, so there is one representation of "what was claimed" and it
    /// cannot drift. Splits on the last two colons, matching
    /// `SessionLedger::injected_files` — paths may themselves contain colons.
    fn claimed_files(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut files = Vec::new();
        for key in &self.claimed {
            if let Some(last) = key.rfind(':') {
                if let Some(second) = key[..last].rfind(':') {
                    let path = &key[..second];
                    if seen.insert(path.to_string()) {
                        files.push(path.to_string());
                    }
                }
            }
        }
        files
    }

    /// Write the claimed chunks to the ledger under this turn's identifier.
    ///
    /// Call this only after the response has actually been emitted. Recording
    /// before output would suppress those chunks on the next turn even if this
    /// one bailed out, which is worse than not recording at all.
    fn commit(&mut self) {
        if self.claimed.is_empty() {
            return;
        }
        let keys = std::mem::take(&mut self.claimed);
        self.ledger.record(&keys, &self.injection_id);
        self.claimed = keys;
    }
}

/// Tracks recent prompts within a session to build conversation-aware queries.
///
/// Stored as JSONL at `.bobbin/session/<cc_session_id>/prompts.jsonl`.
/// Each entry records a cleaned prompt and timestamp, limited to the most recent
/// N entries (configurable, default 5). When building a search query, recent
/// prompts are combined with the current prompt to capture conversational trajectory.
struct PromptHistory {
    entries: Vec<PromptEntry>,
    path: Option<PathBuf>,
    max_entries: usize,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct PromptEntry {
    prompt: String,
    timestamp: u64, // unix epoch seconds
}

impl PromptHistory {
    /// Load prompt history for a Claude Code session.
    fn load(repo_root: &Path, cc_session_id: &str, max_entries: usize) -> Self {
        if cc_session_id.is_empty() {
            return Self {
                entries: Vec::new(),
                path: None,
                max_entries,
            };
        }
        let dir = repo_root
            .join(".bobbin")
            .join("session")
            .join(cc_session_id);
        let path = dir.join("prompts.jsonl");

        let mut entries = Vec::new();
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    if let Ok(entry) = serde_json::from_str::<PromptEntry>(line) {
                        entries.push(entry);
                    }
                }
            }
        }

        // Keep only the most recent max_entries
        if entries.len() > max_entries {
            entries = entries.split_off(entries.len() - max_entries);
        }

        Self {
            entries,
            path: Some(path),
            max_entries,
        }
    }

    /// Record a new prompt. Appends to the JSONL file and maintains window size.
    fn record(&mut self, prompt: &str) {
        let entry = PromptEntry {
            prompt: prompt.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };

        self.entries.push(entry.clone());

        // Trim to max_entries
        if self.entries.len() > self.max_entries {
            self.entries = self
                .entries
                .split_off(self.entries.len() - self.max_entries);
        }

        // Append to file
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string(&entry) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(f, "{}", json);
                }
            }
        }
    }

    /// Build a trajectory-aware search query from recent prompts + current prompt.
    ///
    /// Combines the current prompt with up to N recent prompts, weighted by recency.
    /// Recent prompts are truncated to keep the combined query under a character limit.
    /// Returns the enriched query string.
    fn build_trajectory_query(&self, current_prompt: &str, max_chars: usize) -> String {
        if self.entries.is_empty() {
            return current_prompt.to_string();
        }

        // Current prompt gets full weight (placed last, most influential for embeddings)
        let current_len = current_prompt.len();
        if current_len >= max_chars {
            return current_prompt.to_string();
        }

        let remaining = max_chars - current_len;

        // Collect recent prompts (excluding any that match the current prompt)
        let recent: Vec<&str> = self
            .entries
            .iter()
            .rev()
            .filter(|e| e.prompt != current_prompt)
            .map(|e| e.prompt.as_str())
            .take(3) // Use at most 3 recent prompts
            .collect();

        if recent.is_empty() {
            return current_prompt.to_string();
        }

        // Allocate remaining budget across recent prompts (more recent = more budget)
        let mut context_parts: Vec<String> = Vec::new();
        let per_prompt_budget = remaining / recent.len();

        for prompt in recent.iter().rev() {
            // Reverse back to chronological order
            let truncated = if prompt.len() > per_prompt_budget {
                // Take the last per_prompt_budget chars (most relevant part)
                let cutoff = prompt.len() - per_prompt_budget;
                match prompt[cutoff..].find(' ') {
                    Some(pos) => &prompt[cutoff + pos + 1..],
                    None => &prompt[cutoff..],
                }
            } else {
                prompt
            };
            if !truncated.is_empty() {
                context_parts.push(truncated.to_string());
            }
        }

        if context_parts.is_empty() {
            current_prompt.to_string()
        } else {
            // Format: "context from history... | current prompt"
            // The pipe separator helps embeddings distinguish trajectory from current focus
            format!("{} | {}", context_parts.join(" "), current_prompt)
        }
    }
}

/// Generate `.bobbin/hot-topics.md` from injection frequency data.
fn generate_hot_topics(state: &HookState, output_path: &Path) -> Result<()> {
    use std::fmt::Write;

    let mut md = String::new();
    writeln!(md, "# Hot Topics (auto-generated by bobbin)").unwrap();
    writeln!(md).unwrap();

    let timestamp = if state.last_injection_time.is_empty() {
        "never".to_string()
    } else {
        // Truncate to minute precision for readability
        state
            .last_injection_time
            .get(..16)
            .unwrap_or(&state.last_injection_time)
            .replace('T', " ")
            + " UTC"
    };
    writeln!(md, "Last updated: {}", timestamp).unwrap();
    writeln!(md, "Based on {} context injections.", state.injection_count).unwrap();
    writeln!(md).unwrap();

    // Frequently referenced chunks, sorted by count descending
    let mut chunks: Vec<(&String, &ChunkFrequency)> = state.chunk_frequencies.iter().collect();
    chunks.sort_by(|a, b| b.1.count.cmp(&a.1.count));
    chunks.truncate(20);

    writeln!(md, "## Frequently Referenced Code").unwrap();
    writeln!(md).unwrap();
    if chunks.is_empty() {
        writeln!(md, "No injection data yet.").unwrap();
    } else {
        writeln!(md, "| Rank | File | Symbol | Injections |").unwrap();
        writeln!(md, "|------|------|--------|------------|").unwrap();
        for (i, (_key, freq)) in chunks.iter().enumerate() {
            let symbol = freq.name.as_deref().unwrap_or("-");
            writeln!(
                md,
                "| {} | {} | {} | {} |",
                i + 1,
                freq.file,
                symbol,
                freq.count
            )
            .unwrap();
        }
    }
    writeln!(md).unwrap();

    // Most referenced files, sorted by count descending
    let mut files: Vec<(&String, &u64)> = state.file_frequencies.iter().collect();
    files.sort_by(|a, b| b.1.cmp(a.1));
    files.truncate(10);

    writeln!(md, "## Most Referenced Files").unwrap();
    writeln!(md).unwrap();
    if files.is_empty() {
        writeln!(md, "No injection data yet.").unwrap();
    } else {
        writeln!(md, "| File | Total Injections |").unwrap();
        writeln!(md, "|------|-----------------|").unwrap();
        for (file, count) in &files {
            writeln!(md, "| {} | {} |", file, count).unwrap();
        }
    }
    writeln!(md).unwrap();

    writeln!(md, "## Notes").unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "- Chunks appearing here are candidates for pinning in CLAUDE.md or session context."
    )
    .unwrap();
    writeln!(
        md,
        "- Regenerated every 10 injections. Run `bobbin hook hot-topics` to force refresh."
    )
    .unwrap();

    std::fs::write(output_path, md).context("Failed to write hot-topics.md")?;
    Ok(())
}

async fn run_hot_topics(args: HotTopicsArgs, _output: OutputConfig) -> Result<()> {
    let cwd = if args.path == Path::new(".") {
        std::env::current_dir().context("Failed to get cwd")?
    } else {
        args.path.clone()
    };

    let repo_root = find_bobbin_root(&cwd).context("Bobbin not initialized in this directory")?;
    let state = load_hook_state(&repo_root);

    if !args.force && state.injection_count == 0 {
        println!("No injection data yet. Run with --force to generate anyway.");
        return Ok(());
    }

    let output_path = repo_root.join(".bobbin").join("hot-topics.md");
    generate_hot_topics(&state, &output_path)?;
    println!(
        "Generated {} ({} injections, {} chunks tracked)",
        output_path.display(),
        state.injection_count,
        state.chunk_frequencies.len()
    );
    Ok(())
}

/// Inner implementation that can return errors (caller swallows them).
async fn inject_context_inner(args: InjectContextArgs, raw: &str) -> Result<()> {
    use crate::index::Embedder;
    use crate::search::context::{BridgeMode, ContentMode, ContextAssembler, ContextConfig};
    use crate::storage::{MetadataStore, VectorStore};

    let hook_start = std::time::Instant::now();

    // 1. Parse the hook input (read once by the caller)
    let input: HookInput = serde_json::from_str(raw).context("Failed to parse stdin JSON")?;

    // 2. Resolve effective config
    let cwd = if input.cwd.is_empty() {
        std::env::current_dir().context("Failed to get cwd")?
    } else {
        PathBuf::from(&input.cwd)
    };

    let repo_root = find_bobbin_root(&cwd).context("Bobbin not initialized")?;
    let metrics_source = crate::metrics::resolve_source(
        None, // no CLI flag in hook context
        if input.session_id.is_empty() {
            None
        } else {
            Some(&input.session_id)
        },
    );
    let config =
        Config::load(&Config::config_path(&repo_root)).context("Failed to load bobbin config")?;
    let hooks_cfg = &config.hooks;

    // Apply CLI overrides
    let min_prompt_length = args
        .min_prompt_length
        .unwrap_or(hooks_cfg.min_prompt_length);
    let threshold = args.threshold.unwrap_or(hooks_cfg.threshold);
    let budget = args.budget.unwrap_or(hooks_cfg.budget);
    let content_mode_str = args
        .content_mode
        .as_deref()
        .unwrap_or(&hooks_cfg.content_mode);
    let content_mode = match content_mode_str {
        "full" => ContentMode::Full,
        "none" => ContentMode::None,
        _ => ContentMode::Preview,
    };
    let format_mode = args
        .format_mode
        .as_deref()
        .unwrap_or(&hooks_cfg.format_mode);

    // 3. Check min prompt length
    let prompt = input.prompt.trim();
    if prompt.len() < min_prompt_length {
        return Ok(());
    }

    // 3b. Check skip prefixes (operational commands that never need context).
    // Built-in prefixes always apply; user-configured prefixes extend them.
    let prompt_lower = prompt.to_lowercase();
    let matches_prefix = |pl: &str| -> bool {
        if pl.len() <= 5 && !pl.ends_with(' ') {
            prompt_lower == pl
        } else {
            prompt_lower.starts_with(pl)
        }
    };
    if BUILTIN_SKIP_PREFIXES.iter().any(|p| matches_prefix(p))
        || hooks_cfg
            .skip_prefixes
            .iter()
            .any(|p| matches_prefix(&p.to_lowercase()))
    {
        return Ok(());
    }

    // 3c. Skip injection for automated messages (patrol nudges, reactor alerts, etc.)
    // These are machine-generated and don't benefit from semantic search — they just
    // produce noise injections matching docs about "escalation", "patrol", etc.
    if is_automated_message(prompt) {
        eprintln!("bobbin: skipped (automated message detected)");
        return Ok(());
    }

    // 3d. Strip system tags and truncate prompt for search quality
    let clean_prompt = strip_system_tags(prompt);
    let search_query = if clean_prompt.trim().is_empty() {
        eprintln!("bobbin: skipped (prompt is only system tags)");
        return Ok(());
    } else {
        clean_prompt.trim()
    };
    // 3e. Short bead-command skip (local mode, mirrors remote mode)
    if is_bead_command(search_query) {
        eprintln!("bobbin: skipped (short bead command detected)");
        return Ok(());
    }

    let search_query = if search_query.len() > 500 {
        let cutoff = search_query.len() - 500;
        match search_query[cutoff..].find(' ') {
            Some(pos) => &search_query[cutoff + pos + 1..],
            None => &search_query[cutoff..],
        }
    } else {
        search_query
    };

    // 3f. Conversation-aware query: enrich with recent prompt history
    let mut prompt_history = PromptHistory::load(&repo_root, &input.session_id, 5);
    let trajectory_query = prompt_history.build_trajectory_query(search_query, 700);
    prompt_history.record(search_query);
    let search_query = trajectory_query.as_str();

    // The local path consumes the SAME published answer and uses the SAME
    // additive retrieval prior as remote mode. This was the missing half of
    // 72691f7: --server was work-item-aware while local injection silently
    // remained per-prompt and stateless.
    let work_item = crate::plate::current();
    let item_scoped_query = crate::plate::scope_query(search_query, work_item.as_deref());
    if matches!(&item_scoped_query, std::borrow::Cow::Owned(_)) {
        eprintln!(
            "bobbin: scoping retrieval to work item {}",
            work_item.as_deref().expect("owned query requires an item")
        );
    }
    let search_query = item_scoped_query.as_ref();

    let budget = breadcrumb_triggers::prompt(&repo_root, &input, budget, &config, None).await;

    // 4. Open stores
    let lance_path = Config::lance_path(&repo_root);
    let db_path = Config::db_path(&repo_root);
    let model_dir = Config::model_cache_dir()?;

    let vector_store = VectorStore::open(&lance_path)
        .await
        .context("Failed to open vector store")?;

    if vector_store.count().await? == 0 {
        return Ok(());
    }

    let metadata_store = MetadataStore::open(&db_path).context("Failed to open metadata store")?;

    // 5. Check model consistency
    let current_model = config.embedding.model.as_str();
    if let Some(stored) = metadata_store.get_meta("embedding_model")? {
        if stored != current_model {
            return Ok(()); // Model mismatch — skip silently
        }
    }

    let embedder = Embedder::from_config(&config.embedding, &model_dir)
        .context("Failed to load embedding model")?;

    // 6. Assemble context (config cascade: calibration.json > config.toml > intent adjustments)
    let calibration = crate::cli::calibrate::load_calibration(&repo_root);
    let cal_sw = calibration.as_ref().map(|c| c.best_config.semantic_weight);
    let cal_dd = calibration.as_ref().map(|c| c.best_config.doc_demotion);
    let cal_rrf = calibration.as_ref().map(|c| c.best_config.rrf_k);
    let cal_hl = calibration
        .as_ref()
        .and_then(|c| c.best_config.recency_half_life_days);
    let cal_rw = calibration
        .as_ref()
        .and_then(|c| c.best_config.recency_weight);
    let cal_budget = calibration
        .as_ref()
        .and_then(|c| c.best_config.budget_lines);
    let cal_sl = calibration
        .as_ref()
        .and_then(|c| c.best_config.search_limit);
    let cal_bm = calibration.as_ref().and_then(|c| c.best_config.bridge_mode);
    let cal_bbf = calibration
        .as_ref()
        .and_then(|c| c.best_config.bridge_boost_factor);

    // Query intent classification: adjust search parameters based on prompt type
    let intent = crate::search::intent::classify_intent(search_query);
    let adj = crate::search::intent::intent_adjustments(intent);

    // Skip injection entirely for Operational intent (matches remote mode).
    // Agents running shell commands never benefit from code context.
    if intent == crate::search::intent::QueryIntent::Operational {
        eprintln!("bobbin: skipped (operational intent: {:?})", intent);
        return Ok(());
    }

    // Base values from calibration or config
    let base_sw = cal_sw.unwrap_or(config.search.semantic_weight);
    let base_dd = cal_dd.unwrap_or(config.search.doc_demotion);
    let base_rw = cal_rw.unwrap_or(config.search.recency_weight);

    // Cross-agent feedback: compute file-level boost scores from prior ratings.
    // Files rated "useful" by any agent for similar queries get a score boost.
    let feedback_scores = {
        let feedback_db_path = Config::feedback_db_path(&repo_root);
        crate::storage::feedback::FeedbackStore::open(&feedback_db_path)
            .ok()
            .and_then(|fb| fb.file_feedback_scores(search_query, 0.15).ok())
            .filter(|m| !m.is_empty())
    };

    let context_config = ContextConfig {
        budget_lines: cal_budget.unwrap_or(budget).min(budget),
        budget_unit: config.context.budget_unit,
        depth: 1,
        max_coupled: 2, // Tightened from 3 to reduce coupled noise (matches remote mode)
        coupling_threshold: adj
            .coupling_threshold
            .unwrap_or(config.context.coupling_threshold),
        semantic_weight: (base_sw * adj.semantic_weight_factor).clamp(0.0, 1.0),
        content_mode,
        search_limit: cal_sl.unwrap_or(12), // Tightened from 20 for precision (matches remote mode)
        // bobbin-lpp: this raw-multiplied, which INVERTED the intent — BugFix's
        // 1.5 factor raised the multiplier and so weakened demotion, the
        // opposite of what src/search/intent.rs asserts it means. Same helper
        // as the remote path now, so the two cannot drift apart again.
        doc_demotion: crate::search::intent::apply_doc_demotion_factor(
            base_dd,
            adj.doc_demotion_factor,
            0.01,
        ),
        recency_half_life_days: cal_hl.unwrap_or(config.search.recency_half_life_days),
        recency_weight: (base_rw * adj.recency_weight_factor).clamp(0.0, 1.0),
        rrf_k: cal_rrf.unwrap_or(config.search.rrf_k),
        bridge_mode: cal_bm.unwrap_or(BridgeMode::default()),
        bridge_boost_factor: cal_bbf.unwrap_or(config.context.bridge_boost_factor),
        extra_filter: None,
        tags_config: None,
        role: None,
        file_type_rules: config.file_types.clone(),
        repo_affinity: detect_repo_name(&cwd),
        repo_affinity_boost: config.hooks.repo_affinity_boost,
        max_bridged_files: config.context.max_bridged_files,
        max_bridged_chunks_per_file: config.context.max_bridged_chunks_per_file,
        knowledge_budget_pct: config.context.knowledge_budget_pct,
        neighbor_budget_pct: config.context.neighbor_budget_pct,
        knowledge_max_hops: config.context.knowledge_max_hops,
        feedback_boost_max: config.feedback.boost_max,
        feedback_boost_weight: config.feedback.boost_weight,
        repo_path_prefix: config.server.repo_path_prefix.clone(),
        feedback_scores,
        ..ContextConfig::default()
    };

    let mut assembler =
        ContextAssembler::new(embedder, vector_store, metadata_store, context_config);
    if let Ok(git) = crate::index::git::GitAnalyzer::new(&repo_root) {
        assembler = assembler.with_git_analyzer(git);
    }
    let bundle = assembler
        .assemble(search_query, None)
        .await
        .context("Context assembly failed")?;

    // 7. Gate check: skip entire injection if top semantic score is too low
    //    Intent-based gate boost applied (operational queries get higher bar)
    let base_gate = args.gate_threshold.unwrap_or(hooks_cfg.gate_threshold);
    let gate = base_gate + adj.gate_boost;
    if bundle.summary.top_semantic_score < gate {
        eprintln!(
            "bobbin: skipped (semantic={:.2} < gate={:.2})",
            bundle.summary.top_semantic_score, gate
        );
        crate::metrics::emit(
            &repo_root,
            &crate::metrics::event(
                &metrics_source,
                "hook_gate_skip",
                "hook inject-context",
                hook_start.elapsed().as_millis() as u64,
                serde_json::json!({
                    "query": prompt,
                    "top_score": bundle.summary.top_semantic_score,
                    "gate_threshold": gate,
                }),
            ),
        );
        return Ok(());
    }

    // 7b. Role-based access filtering
    let role = crate::access::RepoFilter::resolve_role(None);
    let access_filter = crate::access::RepoFilter::from_config(&config.access, &role);
    let mut bundle = bundle;
    bundle
        .files
        .retain(|f| access_filter.is_allowed(crate::access::RepoFilter::repo_from_path(&f.path)));

    // 7c. Filter out files already in agent context (CLAUDE.md, AGENTS.md, etc.)
    // and static product docs that waste injection budget.
    {
        let before = bundle.files.len();
        bundle.files.retain(|f| {
            let filename = f.path.rsplit('/').next().unwrap_or(&f.path);
            !matches!(
                filename,
                "CLAUDE.md"
                    | "AGENTS.md"
                    | "@AGENTS.md"
                    | "CLAUDE.local.md"
                    | "MEMORY.md"
                    | "README.md"
                    | "CONTRIBUTING.md"
                    | "LICENSE.md"
                    | "QUICKSTART.md"
                    | "FAQ.md"
                    | "INSTALLING.md"
                    | "UNINSTALLING.md"
                    | "TROUBLESHOOTING.md"
                    | "RELEASING.md"
                    | "SETUP.md"
            )
        });
        let removed = before - bundle.files.len();
        if removed > 0 {
            eprintln!(
                "bobbin: filtered {} already-in-context files (CLAUDE.md etc.)",
                removed
            );
        }
    }

    // 7d. Filter out design doc and audit directories — static planning/design docs
    // produce high noise. Keep in sync with remote mode and context.rs is_noise_path.
    {
        let before = bundle.files.len();
        let design_dirs = [
            "/_plans/",
            "/_design/",
            "/_roadmap/",
            "/_specs/",
            "/audit/",
            "/docs/tasks/",
            "/docs/plans/",
            "/docs/design/",
            "/docs/designs/",
            "/docs/runbooks/",
            "/crew/",
            "/polecats/",
            "/memory/",
            "/.beads/",
            "/session-notes/",
            "/sessions/",
        ];
        let test_dirs = [
            "/tests/",
            "/test/",
            "/__tests__/",
            "/spec/",
            "/specs/",
            "/testdata/",
            "/fixtures/",
            "/examples/",
            "/example/",
            "/samples/",
            "/demo/",
            "/demos/",
        ];
        let infra_dirs = [
            "/.github/workflows/",
            "/.github/actions/",
            "/terraform/",
            "/ansible/",
            "/helm/",
            "/deploy/",
            "/.circleci/",
            "/.gitlab-ci",
        ];
        let design_files = [
            "ROADMAP.md",
            "DESIGN.md",
            "ARCHITECTURE.md",
            "VISION.md",
            "PRD.md",
            "CHANGELOG.md",
        ];
        bundle.files.retain(|f| {
            let path_lower = f.path.to_lowercase();
            if design_dirs.iter().any(|d| path_lower.contains(d)) {
                return false;
            }
            if test_dirs.iter().any(|d| path_lower.contains(d)) {
                return false;
            }
            if infra_dirs.iter().any(|d| path_lower.contains(d)) {
                return false;
            }
            let filename = f.path.rsplit('/').next().unwrap_or(&f.path);
            if design_files
                .iter()
                .any(|d| filename.eq_ignore_ascii_case(d))
            {
                return false;
            }
            // Skip test file patterns (catches test files outside /test/ dirs)
            let fname_lower = filename.to_lowercase();
            if fname_lower.ends_with("_test.go")
                || fname_lower.ends_with("_test.rs")
                || fname_lower.ends_with(".test.ts")
                || fname_lower.ends_with(".test.js")
                || fname_lower.ends_with(".spec.ts")
                || fname_lower.ends_with(".spec.js")
                || fname_lower.starts_with("test_")
                || matches!(
                    filename,
                    "Dockerfile"
                        | "docker-compose.yml"
                        | "docker-compose.yaml"
                        | "Makefile"
                        | "Justfile"
                        | "Taskfile.yml"
                )
            {
                return false;
            }
            // Skip lock files and generated output
            if matches!(
                filename,
                "Cargo.lock"
                    | "package-lock.json"
                    | "yarn.lock"
                    | "pnpm-lock.yaml"
                    | "go.sum"
                    | "Gemfile.lock"
                    | "poetry.lock"
                    | "composer.lock"
                    | "Pipfile.lock"
            ) {
                return false;
            }
            // Skip vendored/generated directories
            if path_lower.contains("/vendor/")
                || path_lower.contains("/node_modules/")
                || path_lower.contains("/third_party/")
                || path_lower.contains("/dist/")
                || path_lower.contains("/build/")
                || path_lower.contains("/target/")
            {
                return false;
            }
            true
        });
        let removed = before - bundle.files.len();
        if removed > 0 {
            eprintln!(
                "bobbin: filtered {} noise path files (design/test/infra)",
                removed
            );
        }
    }

    // 7e. Cross-repo non-affinity gate: non-affinity results need a higher
    // score to survive. Mirrors the remote mode gate logic.
    // Language mismatch adds an extra penalty (e.g. Go results in a Rust repo).
    {
        use crate::search::intent::QueryIntent;
        let repo_affinity = detect_repo_name(&cwd);
        let cross_repo_penalty = match intent {
            QueryIntent::Architecture | QueryIntent::Configuration => 0.04,
            QueryIntent::Navigation => 0.06,
            QueryIntent::Implementation | QueryIntent::BugFix => 0.08,
            QueryIntent::General => 0.10,
            QueryIntent::Operational => 0.12,
        };
        if let Some(ref affinity) = repo_affinity {
            // Detect dominant language from affinity-repo results
            let affinity_lang: Option<String> = {
                let mut lang_counts: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for f in bundle.files.iter() {
                    let is_aff = f.repo.as_deref() == Some(affinity.as_str())
                        || f.path.contains(affinity.as_str());
                    if is_aff && !f.language.is_empty() && f.language != "markdown" {
                        *lang_counts.entry(&f.language).or_insert(0) += 1;
                    }
                }
                lang_counts
                    .into_iter()
                    .max_by_key(|(_, count)| *count)
                    .filter(|(_, count)| *count >= 2)
                    .map(|(lang, _)| lang.to_string())
            };

            let before = bundle.files.len();
            let non_affinity_gate = gate + cross_repo_penalty;
            let lang_mismatch_penalty: f32 = 0.05;
            bundle.files.retain(|f| {
                let is_affinity = f.repo.as_deref() == Some(affinity.as_str())
                    || f.path.contains(affinity.as_str());
                if is_affinity {
                    true
                } else {
                    let effective_gate = if let Some(ref aff_lang) = affinity_lang {
                        if !f.language.is_empty()
                            && f.language != "markdown"
                            && f.language != *aff_lang
                        {
                            non_affinity_gate + lang_mismatch_penalty
                        } else {
                            non_affinity_gate
                        }
                    } else {
                        non_affinity_gate
                    };
                    f.chunks.iter().any(|c| c.score >= effective_gate)
                }
            });
            let removed = before - bundle.files.len();
            if removed > 0 {
                eprintln!(
                    "bobbin: cross-repo gate filtered {} non-affinity files (gate={:.3}, lang={:?}, intent={:?})",
                    removed, non_affinity_gate, affinity_lang, intent,
                );
            }
        }
    }

    // 8. Session reducing: filter out chunks already injected in this session
    let reducing_enabled = hooks_cfg.reducing_enabled && !input.session_id.is_empty();
    let dedup_enabled = !args.no_dedup && hooks_cfg.dedup_enabled;
    let dedup_session_id = compute_session_id(&bundle, threshold);

    let mut ledger = if reducing_enabled {
        SessionLedger::load(&repo_root, &input.session_id)
    } else {
        SessionLedger {
            entries: HashSet::new(),
            turn: 0,
            path: None,
        }
    };

    // Count total chunks before reducing (for metrics)
    let total_chunks_before: usize = bundle
        .files
        .iter()
        .flat_map(|f| f.chunks.iter())
        .filter(|c| c.score >= threshold)
        .count();
    let _previously_injected = if reducing_enabled { ledger.len() } else { 0 };

    if reducing_enabled && ledger.len() > 0 {
        // Filter out chunks already in the ledger
        for file in &mut bundle.files {
            file.chunks.retain(|c| {
                let key = chunk_key(&file.path, c.start_line, c.end_line);
                !ledger.contains(&key)
            });
        }
        bundle.files.retain(|f| !f.chunks.is_empty());
    } else if dedup_enabled && !reducing_enabled {
        // Fallback: binary dedup when reducing is disabled
        let s = load_hook_state(&repo_root);
        if s.last_session_id == dedup_session_id && !dedup_session_id.is_empty() {
            eprintln!("bobbin: skipped (session unchanged)");
            crate::metrics::emit(
                &repo_root,
                &crate::metrics::event(
                    &metrics_source,
                    "hook_dedup_skip",
                    "hook inject-context",
                    hook_start.elapsed().as_millis() as u64,
                    serde_json::json!({ "query": prompt }),
                ),
            );
            return Ok(());
        }
    }

    // Count new chunks after reducing
    let new_chunks: usize = bundle
        .files
        .iter()
        .flat_map(|f| f.chunks.iter())
        .filter(|c| c.score >= threshold)
        .count();
    let reduced_count = total_chunks_before.saturating_sub(new_chunks);

    // 9. Output context (only if we have new chunks) — or try complementary expansion
    if bundle.files.is_empty() || new_chunks == 0 {
        if reducing_enabled && reduced_count > 0 {
            // 9a. Complementary expansion: find coupled files the agent hasn't seen yet
            let previously_seen_files = ledger.injected_files();
            let seen_set: HashSet<&str> =
                previously_seen_files.iter().map(|s| s.as_str()).collect();

            let mut complementary_files: Vec<(String, f32)> = Vec::new();
            // Reopen metadata store (original was moved into ContextAssembler)
            let comp_metadata = MetadataStore::open(&db_path).ok();
            if let Some(ref comp_ms) = comp_metadata {
                for seen_file in &previously_seen_files {
                    if let Ok(coupled) = comp_ms.get_coupling(seen_file, 5) {
                        for c in coupled {
                            let other = if c.file_a == *seen_file {
                                &c.file_b
                            } else {
                                &c.file_a
                            };
                            if !seen_set.contains(other.as_str()) && c.score >= 0.1 {
                                complementary_files.push((other.clone(), c.score));
                            }
                        }
                    }
                }
            } // end if let Some(comp_ms)

            // Deduplicate by path, then sort and truncate — see the helper for why
            // the previous dedup_by silently kept duplicates (bobbin-au4).
            let complementary_files = dedupe_complementary(complementary_files, 5);

            if !complementary_files.is_empty() {
                // Format complementary suggestion as context
                use std::fmt::Write as FmtWrite;
                let mut comp_context = String::new();
                let _ = writeln!(comp_context, "## Complementary Files");
                let _ = writeln!(
                    comp_context,
                    "You've been working with files that are coupled to these (not yet viewed):\n"
                );
                for (file, score) in &complementary_files {
                    let _ = writeln!(comp_context, "- `{}` (coupling: {:.2})", file, score);
                }

                let injection_id = generate_context_injection_id(prompt);
                let response = serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
                        "additionalContext": comp_context,
                    }
                });
                println!("{}", response);

                // Record complementary files in ledger (as pseudo-entries to avoid re-suggesting)
                let comp_keys: Vec<String> = complementary_files
                    .iter()
                    .map(|(f, _)| chunk_key(f, 0, 0)) // marker entries
                    .collect();
                ledger.record(&comp_keys, &injection_id);

                crate::metrics::emit(
                    &repo_root,
                    &crate::metrics::event(
                        &metrics_source,
                        "hook_complementary_expansion",
                        "hook inject-context",
                        hook_start.elapsed().as_millis() as u64,
                        serde_json::json!({
                            "query": prompt,
                            "total_chunks": total_chunks_before,
                            "previously_injected": reduced_count,
                            "complementary_files": complementary_files.len(),
                        }),
                    ),
                );
                return Ok(());
            }

            eprintln!(
                "bobbin: skipped (all {} chunks previously injected, no complementary files)",
                reduced_count
            );
            crate::metrics::emit(
                &repo_root,
                &crate::metrics::event(
                    &metrics_source,
                    "hook_reducing_skip",
                    "hook inject-context",
                    hook_start.elapsed().as_millis() as u64,
                    serde_json::json!({
                        "query": prompt,
                        "total_chunks": total_chunks_before,
                        "previously_injected": reduced_count,
                    }),
                ),
            );
        }
        return Ok(());
    }

    let show_docs = args.show_docs.unwrap_or(hooks_cfg.show_docs);
    let injection_id = generate_context_injection_id(prompt);
    let context_text = format_context_for_injection(
        &bundle,
        threshold,
        show_docs,
        Some(&injection_id),
        format_mode,
    );
    // Governed path boundaries spanning what we are about to inject. Same call
    // as the thin-client path below, so the two cannot disagree about which
    // wires apply (see `src/tripwire/mod.rs`). A no-op when no quipu endpoint
    // is configured — an ungoverned deployment pays nothing here.
    let context_text = crate::tripwire::with_boundaries(
        context_text,
        &config,
        bundle.files.iter().map(|f| f.path.clone()).collect(),
        &repo_root,
        format_mode,
    )
    .await;

    // If reducing is active and we filtered some chunks, show delta stats
    if reducing_enabled && reduced_count > 0 {
        eprintln!(
            "bobbin: injecting {} new chunks ({} previously injected, turn {})",
            new_chunks,
            reduced_count,
            ledger.turn + 1
        );
    }

    print!("{}", context_text);

    // Store injection record locally (best-effort)
    let feedback_db_path = Config::feedback_db_path(&repo_root);
    if let Ok(fb_store) = crate::storage::feedback::FeedbackStore::open(&feedback_db_path) {
        let files_json: Vec<String> = bundle.files.iter().map(|f| f.path.clone()).collect();
        let session_id = if input.session_id.is_empty() {
            None
        } else {
            Some(input.session_id.as_str())
        };
        let _ = fb_store.store_injection_with_output(
            &injection_id,
            session_id,
            None,
            prompt,
            &files_json,
            new_chunks,
            bundle.budget.max_lines,
            Some(&context_text),
        );
    }

    // 10. Update hook state + session ledger
    let mut state = load_hook_state(&repo_root);
    let all_chunk_keys: Vec<String> = bundle
        .files
        .iter()
        .flat_map(|f| {
            f.chunks
                .iter()
                .filter(|c| c.score >= threshold)
                .map(move |c| (f.path.clone(), c))
        })
        .map(|(path, c)| {
            let key = chunk_key(&path, c.start_line, c.end_line);
            let freq = state
                .chunk_frequencies
                .entry(key.clone())
                .or_insert(ChunkFrequency {
                    count: 0,
                    file: path.clone(),
                    name: c.name.clone(),
                });
            freq.count += 1;
            *state.file_frequencies.entry(path).or_insert(0) += 1;
            key
        })
        .collect();

    // Record in session ledger for progressive reducing
    if reducing_enabled {
        ledger.record(&all_chunk_keys, &injection_id);
    }

    state.last_session_id = dedup_session_id;
    state.last_injected_chunks = all_chunk_keys;
    state.last_injection_time = chrono::Utc::now().to_rfc3339();
    state.injection_count += 1;
    save_hook_state(&repo_root, &state);

    // 10b. Feedback prompt: periodically remind about unrated injections
    let prompt_interval = hooks_cfg.feedback_prompt_interval;
    if prompt_interval > 0
        && state.injection_count % prompt_interval == 0
        && !input.session_id.is_empty()
    {
        if let Ok(fb_store) = crate::storage::feedback::FeedbackStore::open(&feedback_db_path) {
            if let Ok(unrated) = fb_store.unrated_injections_for_session(&input.session_id) {
                if !unrated.is_empty() {
                    let sample: Vec<&str> = unrated.iter().take(3).map(|s| s.as_str()).collect();
                    eprintln!(
                        "bobbin: {} unrated injections this session. Rate with: bobbin feedback submit --injection {} --rating <useful|noise|harmful>",
                        unrated.len(),
                        sample.join(" or ")
                    );
                }
            }
        }
    }

    // 10c. Emit hook_injection metric (with reducing stats)
    let injected_files: Vec<&str> = bundle.files.iter().map(|f| f.path.as_str()).collect();
    crate::metrics::emit(
        &repo_root,
        &crate::metrics::event(
            &metrics_source,
            "hook_injection",
            "hook inject-context",
            hook_start.elapsed().as_millis() as u64,
            serde_json::json!({
                "query": prompt,
                "files_returned": injected_files,
                "chunks_returned": new_chunks,
                "top_score": bundle.summary.top_semantic_score,
                "budget_lines_used": bundle.budget.used_lines,
                "source_files": bundle.summary.source_files,
                "doc_files": bundle.summary.doc_files,
                "bridged_additions": bundle.summary.bridged_additions,
                "reducing": {
                    "enabled": reducing_enabled,
                    "total_before": total_chunks_before,
                    "new_chunks": new_chunks,
                    "previously_injected": reduced_count,
                    "ledger_size": ledger.len(),
                    "turn": ledger.turn,
                },
            }),
        ),
    );

    // 11. Auto-generate hot topics every 10 injections
    if state.injection_count % 10 == 0 && state.injection_count > state.hot_topics_generated_at {
        let hot_topics_path = repo_root.join(".bobbin").join("hot-topics.md");
        if generate_hot_topics(&state, &hot_topics_path).is_ok() {
            // Update the generation marker (re-load to avoid stale writes)
            let mut updated = load_hook_state(&repo_root);
            updated.hot_topics_generated_at = state.injection_count;
            save_hook_state(&repo_root, &updated);
            eprintln!(
                "bobbin: regenerated hot-topics.md ({} injections)",
                state.injection_count
            );
        }
    }

    Ok(())
}

async fn run_prime_context(_args: PrimeContextArgs, _output: OutputConfig) -> Result<()> {
    // Never block session start — swallow all errors
    match run_prime_context_inner().await {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("bobbin prime-context: {}", e);
            Ok(())
        }
    }
}

async fn run_prime_context_inner() -> Result<()> {
    let hook_start = std::time::Instant::now();

    // 1. Read stdin JSON (may be empty for some Claude Code versions)
    let input_str = std::io::read_to_string(std::io::stdin()).context("Failed to read stdin")?;

    let session_id = if input_str.trim().is_empty() {
        String::new()
    } else {
        let input: SessionStartInput =
            serde_json::from_str(&input_str).unwrap_or(SessionStartInput {
                source: String::new(),
                cwd: String::new(),
                session_id: String::new(),
            });
        input.session_id
    };

    // 2. Find repo root
    let cwd = std::env::current_dir().context("Failed to get cwd")?;
    let repo_root = find_bobbin_root(&cwd).context("Bobbin not initialized")?;

    let metrics_source = crate::metrics::resolve_source(
        None,
        if session_id.is_empty() {
            None
        } else {
            Some(&session_id)
        },
    );

    // 3. Build primer text (brief version + live stats)
    let primer = include_str!("../../docs/primer.md");
    let brief = extract_brief(primer);

    // 4. Gather live stats
    let lance_path = Config::lance_path(&repo_root);
    let stats_text = if let Ok(store) = VectorStore::open(&lance_path).await {
        if let Ok(stats) = store.get_stats(None).await {
            let mut lines = vec![format!(
                "- {} files, {} chunks indexed",
                stats.total_files, stats.total_chunks
            )];
            if !stats.languages.is_empty() {
                let langs: Vec<String> = stats
                    .languages
                    .iter()
                    .map(|l| format!("{} ({} files)", l.language, l.file_count))
                    .collect();
                lines.push(format!("- Languages: {}", langs.join(", ")));
            }
            lines.join("\n")
        } else {
            "- Index stats unavailable".to_string()
        }
    } else {
        "- Vector store not accessible".to_string()
    };

    // 5. Compose output
    let mut context = format!(
        "{}\n\n## Index Status\n{}\n\n## Available Commands\n\
        - `bobbin search <query>` — semantic + keyword hybrid search\n\
        - `bobbin context <query>` — task-aware context assembly with budget control\n\
        - `bobbin grep <pattern>` — keyword/regex search\n\
        - `bobbin related <file>` — find co-changing files via git coupling\n\
        - `bobbin refs <symbol>` — find symbol definitions and references\n\
        - `bobbin impact <file>` — predict affected files\n\
        - `bobbin hotspots` — high-churn, high-complexity files",
        brief, stats_text,
    );
    let discovery = breadcrumbs::discovery(&repo_root, 100);
    if !discovery.is_empty() {
        context.push_str("\n\n");
        context.push_str(&discovery);
    }

    // 6. Output hook response JSON
    let response = HookResponse {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "SessionStart".to_string(),
            additional_context: context,
        },
    };

    println!("{}", serde_json::to_string(&response)?);

    // 7. Emit metric
    crate::metrics::emit(
        &repo_root,
        &crate::metrics::event(
            &metrics_source,
            "hook_prime_context",
            "hook prime-context",
            hook_start.elapsed().as_millis() as u64,
            serde_json::Value::Null,
        ),
    );

    Ok(())
}

async fn run_post_tool_use(_args: PostToolUseArgs, _output: OutputConfig) -> Result<()> {
    // Never block tool completion — any error exits silently
    match run_post_tool_use_inner(_args, _output.server.as_deref()).await {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("bobbin post-tool-use: {:#}", e);
            Ok(())
        }
    }
}

/// Extract a search query from a grep/rg/find bash command.
/// Returns None if the command doesn't look like a search command.
fn extract_search_query_from_bash(command: &str) -> Option<String> {
    let cmd = command.trim();

    // Match: grep [-flags] "pattern" or grep [-flags] pattern
    // Also matches: rg, git grep
    // Strategy: find the command, skip flags (start with -), take the next arg as pattern
    let search_cmds = ["grep", "rg"];
    for search_cmd in &search_cmds {
        // Find the command (could be prefixed with env vars, pipes, etc.)
        // Look for the command as a word boundary
        if let Some(pos) = cmd.find(search_cmd) {
            // Make sure it's a command start (beginning, after pipe, after &&, after ;, after space)
            if pos > 0 {
                let before = cmd[..pos].chars().last().unwrap_or(' ');
                if !before.is_whitespace() && before != '|' && before != ';' && before != '&' {
                    continue;
                }
            }
            // Extract everything after the command name
            let after_cmd = &cmd[pos + search_cmd.len()..];
            if let Some(pattern) = extract_pattern_from_args(after_cmd) {
                return Some(pattern);
            }
        }
    }

    // Match: find . -name "pattern" — extract the name pattern
    if let Some(pos) = cmd.find("find") {
        if pos == 0
            || cmd[..pos].chars().last().map_or(true, |c| {
                c.is_whitespace() || c == '|' || c == ';' || c == '&'
            })
        {
            let after_cmd = &cmd[pos + 4..];
            if let Some(pattern) = extract_find_pattern(after_cmd) {
                return Some(pattern);
            }
        }
    }

    None
}

/// Extract pattern from grep/rg argument list.
/// Skips flags (starting with -), takes the first non-flag argument.
fn extract_pattern_from_args(args: &str) -> Option<String> {
    let args = args.trim();
    // Use a simple state machine to handle quoted strings
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;

    for ch in args.chars() {
        if escape_next {
            current.push(ch);
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if !in_single_quote => escape_next = true,
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    // Skip flags and flag arguments, handle -e/--regexp specially (pattern flag)
    let mut i = 0;
    let mut explicit_pattern: Option<String> = None;
    // Flags that take a value argument (next token is NOT the pattern)
    let flags_with_value = [
        "-f",
        "--file",
        "-A",
        "-B",
        "-C",
        "--context",
        "--color",
        "--colours",
        "-m",
        "--max-count",
        "--include",
        "--exclude",
        "--type",
        "-t",
        "--type-add",
        "--glob",
        "-g",
        "--max-depth",
        "--threads",
        "-j",
        "--after-context",
        "--before-context",
    ];
    while i < tokens.len() {
        let tok = &tokens[i];
        if tok == "--" {
            // Everything after -- is positional
            i += 1;
            break;
        }
        if tok == "-e" || tok == "--regexp" {
            // -e pattern — the next arg IS the pattern
            if i + 1 < tokens.len() {
                explicit_pattern = Some(tokens[i + 1].clone());
            }
            i += 2;
        } else if tok.starts_with('-') {
            // Check if this flag takes a value
            if flags_with_value.iter().any(|f| tok == f) {
                i += 2; // skip flag and its value
            } else if tok.starts_with("--") && tok.contains('=') {
                i += 1; // --flag=value
            } else {
                i += 1; // simple flag like -r, -i, -n
            }
        } else {
            break; // first positional = pattern
        }
    }

    // If -e was used, prefer that pattern
    if let Some(p) = explicit_pattern {
        let cleaned = clean_regex_for_search(&p);
        if !cleaned.is_empty() && p.len() >= 2 && p.len() <= 200 {
            return Some(cleaned);
        }
    }

    if i < tokens.len() {
        let pattern = &tokens[i];
        // Skip very short patterns (likely noise) and very long ones (likely paths)
        if pattern.len() >= 2 && pattern.len() <= 200 {
            // Clean up regex-specific syntax for semantic search
            let cleaned = clean_regex_for_search(pattern);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }

    None
}

/// Extract search intent from find command arguments.
/// Looks for -name/-iname/-path patterns.
fn extract_find_pattern(args: &str) -> Option<String> {
    let args = args.trim();
    let parts: Vec<&str> = args.split_whitespace().collect();

    for i in 0..parts.len().saturating_sub(1) {
        if parts[i] == "-name"
            || parts[i] == "-iname"
            || parts[i] == "-path"
            || parts[i] == "-ipath"
        {
            let pattern = parts[i + 1].trim_matches('"').trim_matches('\'');
            // Strip glob wildcards for semantic search
            let cleaned = pattern
                .replace("*.", "")
                .replace(".*", "")
                .replace('*', " ")
                .trim()
                .to_string();
            if cleaned.len() >= 2 {
                return Some(cleaned);
            }
        }
    }

    None
}

/// Clean regex pattern for use as a semantic search query.
/// Strips regex metacharacters and converts to readable text.
fn clean_regex_for_search(pattern: &str) -> String {
    pattern
        .replace("\\s+", " ")
        .replace("\\s*", " ")
        .replace("\\b", "")
        .replace("\\w+", "")
        .replace("\\d+", "")
        .replace(".*", " ")
        .replace(".+", " ")
        .replace("\\(", "(")
        .replace("\\)", ")")
        .replace("\\{", "{")
        .replace("\\}", "}")
        .replace("\\[", "[")
        .replace("\\]", "]")
        .replace(['(', ')', '[', ']', '{', '}', '^', '$', '|', '?', '+'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Check if a cleaned search query is meaningful enough for semantic search.
/// Rejects too-short queries, pure language keywords, and file extensions.
fn is_meaningful_search_query(query: &str) -> bool {
    let q = query.trim();
    // Too short — likely noise
    if q.len() < 3 {
        return false;
    }
    // Single token that's a common language keyword or file extension — too generic
    let tokens: Vec<&str> = q.split_whitespace().collect();
    if tokens.len() == 1 {
        let lower = tokens[0].to_lowercase();
        let noise_words = [
            "fn", "let", "var", "const", "use", "import", "from", "return", "if", "else", "for",
            "while", "match", "type", "struct", "enum", "class", "def", "func", "pub", "mod",
            "crate", "self", "super", "rs", "go", "py", "ts", "js", "tsx", "jsx", "md", "toml",
            "yaml", "yml", "json", "html", "css", "sh", "bash", "txt",
        ];
        if noise_words.contains(&lower.as_str()) {
            return false;
        }
    }
    true
}

#[path = "hook_tool_response.rs"]
mod hook_tool_response;
pub(crate) use hook_tool_response::{
    classify_dispatch, extract_files_from_tool_response, merge_coupling_for_seeds,
    render_discovered_coupling, resolve_discovered_files, DispatchMode,
};

/// PostToolUse handler: Smart dispatch based on tool type.
/// - Edit/Write: hybrid search for related files (tests, snapshots, configs)
/// - Bash(grep/rg/find): semantic search for the same query (competitive response)
/// - Any tool: reaction rules from .bobbin/reactions.toml
/// Uses ContextAssembler with full config cascade (calibration + config.toml).
/// Fast because ensure_fts_index reuses persisted index.
/// Execute one search-based reaction through a `ContextAssembler`.
///
/// Returns `None` when the search could not be run at all — no index, no
/// store, no embedder. That is deliberately distinct from `Some(vec![])`:
/// "the index has nothing for this query" is a fact about coverage that the
/// caller reports as a warning, while "the store would not open" is an
/// infrastructure failure, and reporting the second as the first would tell an
/// operator their rule's scope is wrong when it is not.
///
/// Builds its own stores rather than borrowing the dispatch path's assembler:
/// that one is consumed inside the builtin-search block and never constructed
/// at all in `ReactionsOnly`/`RefsOnly` mode, where reaction rules still fire.
/// The stores are cheap to reopen — the same argument the coupling path here
/// already makes for `MetadataStore`.
#[allow(clippy::too_many_arguments)]
async fn run_reaction_search(
    search: &crate::reactions::PendingSearch,
    repo_root: &Path,
    lance_path: &Path,
    db_path: &Path,
    config: &Config,
    cwd: &Path,
    budget: usize,
) -> Option<Vec<crate::reactions::SearchHit>> {
    use crate::index::Embedder;
    use crate::search::context::{BridgeMode, ContentMode, ContextAssembler, ContextConfig};

    if budget == 0 {
        return None;
    }

    let vector_store = match crate::storage::VectorStore::open(lance_path).await {
        Ok(vs) if vs.count().await.unwrap_or(0) > 0 => vs,
        _ => return None,
    };
    let metadata_store = MetadataStore::open(db_path).ok()?;
    let model_dir = Config::model_cache_dir().ok()?;
    let embedder = Embedder::from_config(&config.embedding, &model_dir).ok()?;

    // Scope: `search_group` resolves to a repo IN (...) clause, `search_tags`
    // to a tag include clause. Both are ANDed — a rule that names a group AND
    // tags means both, matching the HTTP surface.
    let mut clauses: Vec<String> = Vec::new();
    if let Some(ref group) = search.group {
        if let Some(g) = config.group_filter(group) {
            clauses.push(g);
        }
    }
    if !search.tags.is_empty() {
        clauses.push(crate::tags::build_tag_include_filter(&search.tags));
    }
    let extra_filter = if clauses.is_empty() {
        None
    } else {
        Some(clauses.join(" AND "))
    };

    let context_config = ContextConfig {
        budget_lines: budget,
        depth: 0,
        max_coupled: 0,
        content_mode: ContentMode::None, // File list only — reactions cite files
        bridge_mode: BridgeMode::Off,
        search_limit: 10,
        extra_filter,
        file_type_rules: config.file_types.clone(),
        repo_affinity: detect_repo_name(cwd),
        repo_affinity_boost: config.hooks.repo_affinity_boost,
        repo_path_prefix: config.server.repo_path_prefix.clone(),
        ..ContextConfig::default()
    };

    let mut assembler =
        ContextAssembler::new(embedder, vector_store, metadata_store, context_config);
    if let Ok(git) = crate::index::git::GitAnalyzer::new(repo_root) {
        assembler = assembler.with_git_analyzer(git);
    }

    let bundle = assembler.assemble(&search.query, None).await.ok()?;

    Some(
        bundle
            .files
            .iter()
            .map(|f| crate::reactions::SearchHit {
                path: f.path.clone(),
                score: f.score,
            })
            .collect(),
    )
}

async fn run_post_tool_use_inner(args: PostToolUseArgs, server: Option<&str>) -> Result<()> {
    use crate::index::Embedder;
    use crate::reactions::{self, CompiledRule, DedupTracker, ReactionConfig, ToolEvent};
    use crate::search::context::{BridgeMode, ContentMode, ContextAssembler, ContextConfig};

    let hook_start = std::time::Instant::now();

    // 1. Read stdin JSON
    let input: PostToolUseInput =
        serde_json::from_reader(std::io::stdin().lock()).context("Failed to parse stdin JSON")?;

    // 2. Dispatch based on tool type
    let mode = classify_dispatch(&input.tool_name, &input.tool_input);

    // 2b. Build ToolEvent for reaction matching
    let tool_event = ToolEvent {
        tool_name: input.tool_name.clone(),
        tool_input: input.tool_input.clone(),
    };

    // 3. Resolve bobbin root and config
    let cwd = if input.cwd.is_empty() {
        std::env::current_dir().context("Failed to get cwd")?
    } else {
        PathBuf::from(&input.cwd)
    };

    let repo_root = match find_bobbin_root(&cwd) {
        Some(r) => r,
        None => return Ok(()), // Not a bobbin-indexed project
    };

    let config = Config::load(&Config::config_path(&repo_root)).unwrap_or_default();
    let budget = args.budget.unwrap_or(config.hooks.budget / 2); // Use half budget for post-tool

    let mut triggered =
        breadcrumb_triggers::tool(&repo_root, &input, budget, &config, server).await;
    let budget = budget.saturating_sub(triggered.reserved);

    // 3a'. Upgrade a search dispatch to DiscoveredFiles when the tool_response named
    // files that really exist here. This runs after repo_root because "is this a file
    // in this repo" is the filter that makes the Bash arm usable at all. If nothing
    // resolves, the mode is left exactly as it was — no existing behaviour changes.
    let mode = match mode {
        DispatchMode::SearchQuery {
            query,
            original_cmd,
        } => {
            let files = resolve_discovered_files(
                extract_files_from_tool_response(&input.tool_name, &input.tool_response),
                &cwd,
                &repo_root,
            );
            if files.is_empty() {
                DispatchMode::SearchQuery {
                    query,
                    original_cmd,
                }
            } else {
                DispatchMode::DiscoveredFiles {
                    files,
                    query,
                    original_cmd,
                }
            }
        }
        other => other,
    };

    let metrics_source = crate::metrics::resolve_source(
        None,
        if input.session_id.is_empty() {
            None
        } else {
            Some(&input.session_id)
        },
    );

    // 3b. Resolve role for reaction filtering
    let role = crate::access::RepoFilter::resolve_role(None);

    // 3b'. Load reaction rules (builtins + user overrides) and compile them
    let reaction_config = ReactionConfig::load_for_repo(&repo_root).with_builtins();
    let compiled_rules: Vec<CompiledRule> = reaction_config
        .reactions
        .into_iter()
        .filter_map(|r| {
            CompiledRule::compile(r)
                .map_err(|e| {
                    eprintln!("bobbin: skipping reaction rule: {}", e);
                    e
                })
                .ok()
        })
        .collect();
    let has_reactions = !compiled_rules.is_empty();

    // 3c. Session dedup tracker for reactions
    let mut dedup = DedupTracker::load(&repo_root, &input.session_id);

    // For ReactionsOnly mode with no reaction rules, nothing to do
    if matches!(mode, DispatchMode::ReactionsOnly) && !has_reactions && triggered.text.is_empty() {
        return Ok(());
    }

    // 4. Determine query and context based on dispatch mode
    let (query, rel_path, is_edit_mode, is_refs_only, is_reactions_only) = match &mode {
        DispatchMode::EditRelated { file_path } => {
            let abs_path = if Path::new(file_path.as_str()).is_absolute() {
                PathBuf::from(file_path)
            } else {
                cwd.join(file_path)
            };
            let rel = abs_path
                .strip_prefix(&repo_root)
                .unwrap_or(abs_path.as_path())
                .to_string_lossy()
                .to_string();
            let q = format!("files related to {}", rel);
            (q, Some(rel), true, false, false)
        }
        DispatchMode::SearchQuery { query, .. } | DispatchMode::DiscoveredFiles { query, .. } => {
            (query.clone(), None, false, false, false)
        }
        DispatchMode::RefsOnly { file_path } => {
            let abs_path = if Path::new(file_path.as_str()).is_absolute() {
                PathBuf::from(file_path)
            } else {
                cwd.join(file_path)
            };
            let rel = abs_path
                .strip_prefix(&repo_root)
                .unwrap_or(abs_path.as_path())
                .to_string_lossy()
                .to_string();
            ("".to_string(), Some(rel), false, true, false)
        }
        DispatchMode::ReactionsOnly => ("".to_string(), None, false, false, true),
    };

    // 5. Open stores
    let db_path = Config::db_path(&repo_root);
    let lance_path = Config::lance_path(&repo_root);

    // For RefsOnly (Read), we skip the search/related logic entirely
    let mut context = triggered.text.clone();
    use std::fmt::Write;
    let mut lines_used: usize = 0;
    let mut coupled_count: usize = 0;
    let mut search_file_count: usize = 0;

    // bobbin-aa0: this handler suggested files with no injection identifier and
    // no ledger contact, so it re-suggested files the prompt path had already
    // delivered and nothing it produced could be rated.
    let dispatch_query = match &mode {
        DispatchMode::EditRelated { file_path } => file_path.clone(),
        DispatchMode::SearchQuery { query, .. } | DispatchMode::DiscoveredFiles { query, .. } => {
            query.clone()
        }
        DispatchMode::RefsOnly { file_path } => file_path.clone(),
        DispatchMode::ReactionsOnly => input.tool_name.clone(),
    };
    // The files the search actually FOUND (empty for every other dispatch mode), used
    // both as coupling seeds and as the switch for the budget split below.
    let discovered_files: Vec<String> = match &mode {
        DispatchMode::DiscoveredFiles { files, .. } => files.clone(),
        _ => Vec::new(),
    };

    let mut turn = InjectionTurn::open(
        &repo_root,
        &input.session_id,
        config.hooks.reducing_enabled,
        &dispatch_query,
    );

    if !is_refs_only && !is_reactions_only {
        let model_dir = Config::model_cache_dir()?;

        // Try to open stores — failure skips builtin search but reactions still fire
        let builtin_result: Option<()> = 'builtin: {
            let vector_store = match VectorStore::open(&lance_path).await {
                Ok(vs) if vs.count().await.unwrap_or(0) > 0 => vs,
                _ => break 'builtin None,
            };

            let metadata_store = match MetadataStore::open(&db_path) {
                Ok(ms) => ms,
                Err(_) => break 'builtin None,
            };

            let embedder = match Embedder::from_config(&config.embedding, &model_dir) {
                Ok(e) => e,
                Err(_) => break 'builtin None,
            };

            // 6. Query coupled files. Coupling is file-based, so the seeds are the
            //    edited file (Edit mode) and/or the files the search actually FOUND
            //    (DiscoveredFiles, issue #51 F1 — previously there were no seeds at all
            //    for a search dispatch, so a search got no coupling).
            let coupling_seeds: Vec<String> = rel_path
                .iter()
                .cloned()
                .chain(discovered_files.iter().cloned())
                .collect();
            let coupled = merge_coupling_for_seeds(&coupling_seeds, |seed| {
                metadata_store.get_coupling(seed, 5).unwrap_or_default()
            });

            // 7. Hybrid search — uses calibrated config for search quality.
            let calibration = crate::cli::calibrate::load_calibration(&repo_root);
            let cal_sw = calibration.as_ref().map(|c| c.best_config.semantic_weight);
            let cal_dd = calibration.as_ref().map(|c| c.best_config.doc_demotion);
            let cal_rrf = calibration.as_ref().map(|c| c.best_config.rrf_k);
            let cal_hl = calibration
                .as_ref()
                .and_then(|c| c.best_config.recency_half_life_days);
            let cal_rw = calibration
                .as_ref()
                .and_then(|c| c.best_config.recency_weight);
            let cal_sl = calibration
                .as_ref()
                .and_then(|c| c.best_config.search_limit);

            let context_config = ContextConfig {
                budget_lines: budget,
                depth: 0,       // No recursive expansion for post-tool
                max_coupled: 0, // We handle coupling separately above
                coupling_threshold: 0.1,
                semantic_weight: cal_sw.unwrap_or(config.search.semantic_weight),
                content_mode: ContentMode::None, // File list only, no content
                search_limit: cal_sl.unwrap_or(10), // Smaller default for speed
                doc_demotion: cal_dd.unwrap_or(config.search.doc_demotion),
                recency_half_life_days: cal_hl.unwrap_or(config.search.recency_half_life_days),
                recency_weight: cal_rw.unwrap_or(config.search.recency_weight),
                rrf_k: cal_rrf.unwrap_or(config.search.rrf_k),
                bridge_mode: BridgeMode::Off, // No bridging for post-tool
                bridge_boost_factor: 0.0,
                extra_filter: None,
                tags_config: None,
                role: None,
                file_type_rules: config.file_types.clone(),
                repo_affinity: detect_repo_name(&cwd),
                repo_affinity_boost: config.hooks.repo_affinity_boost,
                max_bridged_files: 3,
                max_bridged_chunks_per_file: 2,
                repo_path_prefix: config.server.repo_path_prefix.clone(),
                ..ContextConfig::default()
            };

            let mut assembler =
                ContextAssembler::new(embedder, vector_store, metadata_store, context_config);
            if let Ok(git) = crate::index::git::GitAnalyzer::new(&repo_root) {
                assembler = assembler.with_git_analyzer(git);
            }

            let bundle = match assembler.assemble(&query, None).await {
                Ok(b) => b,
                Err(_) => {
                    // Search failed — still report coupling if available
                    if coupled.is_empty() {
                        break 'builtin None;
                    }
                    crate::search::context::ContextBundle {
                        capture: None,
                        query: query.clone(),
                        files: vec![],
                        budget: crate::search::context::BudgetInfo {
                            max_lines: budget,
                            used_lines: 0,
                            pinned_lines: 0,
                        },
                        summary: crate::search::context::ContextSummary {
                            total_files: 0,
                            total_chunks: 0,
                            direct_hits: 0,
                            coupled_additions: 0,
                            bridged_additions: 0,
                            source_files: 0,
                            doc_files: 0,
                            top_semantic_score: 0.0,
                            pinned_chunks: 0,
                            knowledge_additions: 0,
                            structural_additions: 0,
                        },
                    }
                }
            };

            // Filter out the edited file itself and low-score results
            // For non-edit search modes, apply a stricter score threshold to reduce noise
            let min_score = if is_edit_mode { 0.0 } else { 0.005 };
            let search_files: Vec<_> = bundle
                .files
                .iter()
                .filter(|f| {
                    // Score gate: skip low-relevance results (especially for search tools)
                    if f.score < min_score {
                        return false;
                    }
                    // Skip the edited file itself (for Edit mode)
                    if let Some(ref rp) = rel_path {
                        let f_rel = Path::new(&f.path)
                            .strip_prefix(&repo_root)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| f.path.clone());
                        f_rel != *rp
                    } else {
                        true
                    }
                })
                .collect();

            // bobbin-aa0: drop files already delivered this session before the
            // headers below decide whether there is anything to say.
            let search_files: Vec<_> = search_files
                .into_iter()
                .filter(|f| turn.claim_file(&f.path))
                .collect();

            coupled_count = coupled.len();
            search_file_count = search_files.len();

            // 8. Format output — different framing for Edit vs Search dispatch
            if is_edit_mode {
                let rp = rel_path.as_deref().unwrap_or("unknown");
                let _ = writeln!(context, "## Related Files: {}", rp);
                let _ = writeln!(
                    context,
                    "You just edited this file. Consider reviewing these related files:\n"
                );
                lines_used += 3;

                let fresh_coupled: Vec<_> = coupled
                    .iter()
                    .filter(|(coupled_file, _)| turn.claim_file(coupled_file))
                    .collect();
                if !fresh_coupled.is_empty() {
                    let _ = writeln!(context, "**Co-changing files** (from git history):");
                    lines_used += 1;
                    for (coupled_file, score) in &fresh_coupled {
                        if lines_used >= budget {
                            break;
                        }
                        let _ = writeln!(context, "- `{}` (coupling: {:.2})", coupled_file, score);
                        lines_used += 1;
                    }
                    let _ = writeln!(context);
                    lines_used += 1;
                }

                if !search_files.is_empty() {
                    let _ = writeln!(context, "**Semantically related** (from bobbin search):");
                    lines_used += 1;
                }
            } else if !search_files.is_empty() {
                // Only show search results header if we have results above the score gate
                let original_cmd = match &mode {
                    DispatchMode::SearchQuery { original_cmd, .. }
                    | DispatchMode::DiscoveredFiles { original_cmd, .. } => original_cmd.as_str(),
                    _ => "search",
                };
                let _ = writeln!(context, "## Bobbin Semantic Matches");
                let _ = writeln!(
                    context,
                    "Your search (`{}`) also matched these files semantically:\n",
                    original_cmd
                );
                lines_used += 3;
            }

            // Budget split (#51 F1): when the response named files, semantic matches
            // get 60% and the coupling of what was actually found gets the rest. With
            // no discovered files this is the whole budget, i.e. the previous behaviour.
            let search_budget = if discovered_files.is_empty() {
                budget
            } else {
                budget * 60 / 100
            };

            if !search_files.is_empty() {
                for f in &search_files {
                    if lines_used >= search_budget {
                        break;
                    }
                    let f_rel = Path::new(&f.path)
                        .strip_prefix(&repo_root)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| f.path.clone());
                    let _ = writeln!(context, "- `{}`", f_rel);
                    lines_used += 1;
                }
            }

            // 8b. Coupling for the files the search FOUND, as opposed to the files
            //     the query merely resembles. This is the whole point of reading
            //     tool_response: "related to what you found", not "related to what
            //     you asked for".
            if !discovered_files.is_empty() && lines_used < budget {
                let fresh: Vec<(String, f32)> = coupled
                    .iter()
                    .filter(|(file, _)| turn.claim_file(file))
                    .cloned()
                    .collect();
                let (section, section_lines) = render_discovered_coupling(
                    &discovered_files,
                    &fresh,
                    budget.saturating_sub(lines_used),
                    lines_used > 0,
                );
                context.push_str(&section);
                lines_used += section_lines;
            }

            Some(()) // end of labeled block
        }; // end 'builtin block
        let _ = builtin_result; // suppress unused warning
    }

    // 9. Symbol refs lookup (Edit + Read modes) — callers AND callees
    let mut refs_count: usize = 0;
    let mut callees_count: usize = 0;
    if (is_edit_mode || is_refs_only) && lines_used < budget {
        if let Some(ref rp) = rel_path {
            let refs_vs_result = VectorStore::open(&lance_path).await;
            // Scope the refs analysis in a block — if store open fails, skip refs
            // but continue to reactions below
            if let Ok(mut refs_vs) = refs_vs_result {
                use crate::analysis::refs::RefAnalyzer;

                // list_symbols needs the path as stored in the index (absolute)
                let abs_file = repo_root.join(rp);
                let abs_file_str = abs_file.to_string_lossy().to_string();

                // Pre-fetch file chunks before creating the analyzer (to avoid borrow conflicts)
                let file_chunks = refs_vs
                    .get_chunks_for_file(&abs_file_str, None)
                    .await
                    .unwrap_or_default();

                let mut analyzer = RefAnalyzer::new(&mut refs_vs);

                // List symbols in the file
                let file_symbols = analyzer
                    .list_symbols(&abs_file_str, None)
                    .await
                    .unwrap_or_else(|_| crate::analysis::refs::FileSymbols {
                        path: abs_file_str.clone(),
                        symbols: vec![],
                    });

                if !file_symbols.symbols.is_empty() {
                    // Limit to top 3 symbols (by line order — most prominent definitions first)
                    let symbols_to_check: Vec<_> = file_symbols.symbols.iter().take(3).collect();

                    // 9a. Callers: for each symbol, find where it's used (in other files)
                    let mut symbol_refs: Vec<(String, Vec<String>)> = Vec::new();
                    for sym in &symbols_to_check {
                        let refs = analyzer
                            .find_refs(&sym.name, None, 10, None)
                            .await
                            .unwrap_or_else(|_| crate::analysis::refs::SymbolRefs {
                                definition: None,
                                usages: vec![],
                            });

                        // Collect unique files where this symbol is used (excluding the file itself)
                        // Usage file_paths are absolute — convert to relative for display and comparison
                        let mut usage_files: Vec<String> = refs
                            .usages
                            .iter()
                            .map(|u| {
                                Path::new(&u.file_path)
                                    .strip_prefix(&repo_root)
                                    .map(|p| p.to_string_lossy().to_string())
                                    .unwrap_or_else(|_| u.file_path.clone())
                            })
                            .filter(|f| f != rp)
                            .collect();
                        usage_files.dedup();
                        usage_files.truncate(5);

                        if !usage_files.is_empty() {
                            symbol_refs.push((sym.name.clone(), usage_files));
                        }
                    }

                    if !symbol_refs.is_empty() {
                        refs_count = symbol_refs.len();
                        if lines_used > 0 {
                            let _ = writeln!(context);
                            lines_used += 1;
                        }

                        if is_refs_only {
                            let _ = writeln!(context, "## Symbol References: {}", rp);
                            let _ =
                                writeln!(context, "Symbols defined in this file are used in:\n");
                            lines_used += 3;
                        } else {
                            let _ = writeln!(
                                context,
                                "**Symbol references** (where symbols from this file are used):"
                            );
                            lines_used += 1;
                        }

                        for (sym_name, usage_files) in &symbol_refs {
                            if lines_used >= budget {
                                break;
                            }
                            let _ = writeln!(
                                context,
                                "- `{}` → {}",
                                sym_name,
                                usage_files
                                    .iter()
                                    .map(|f| format!("`{}`", f))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                            lines_used += 1;
                        }
                    }

                    // 9b. Callees: for each symbol, find what functions it calls
                    if lines_used < budget {
                        let mut symbol_callees: Vec<(String, Vec<(String, String)>)> = Vec::new();
                        for sym in &symbols_to_check {
                            // Find the chunk content for this symbol
                            let chunk = file_chunks
                                .iter()
                                .find(|c| c.name.as_deref() == Some(&sym.name));
                            if let Some(chunk) = chunk {
                                let callees = analyzer
                                    .find_callees(&chunk.content, Some(&sym.name), 5, None)
                                    .await
                                    .unwrap_or_default();

                                let callee_info: Vec<(String, String)> = callees
                                    .into_iter()
                                    .filter_map(|c| {
                                        let def = c.definition?;
                                        let rel_file = Path::new(&def.file_path)
                                            .strip_prefix(&repo_root)
                                            .map(|p| p.to_string_lossy().to_string())
                                            .unwrap_or_else(|_| def.file_path);
                                        Some((c.name, rel_file))
                                    })
                                    .collect();

                                if !callee_info.is_empty() {
                                    symbol_callees.push((sym.name.clone(), callee_info));
                                }
                            }
                        }

                        if !symbol_callees.is_empty() {
                            callees_count = symbol_callees.len();
                            if lines_used > 0 {
                                let _ = writeln!(context);
                                lines_used += 1;
                            }

                            if is_refs_only {
                                let _ = writeln!(context, "**Dependency chain** (functions called by symbols in this file):");
                            } else {
                                let _ = writeln!(
                                    context,
                                    "**Callees** (functions called by this file's symbols):"
                                );
                            }
                            lines_used += 1;

                            for (sym_name, callee_info) in &symbol_callees {
                                if lines_used >= budget {
                                    break;
                                }
                                let callees_str = callee_info
                                    .iter()
                                    .map(|(name, file)| format!("`{}` ({})", name, file))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                let _ =
                                    writeln!(context, "- `{}` calls → {}", sym_name, callees_str);
                                lines_used += 1;
                            }
                        }
                    }
                }
            } // end if let Ok(refs_vs)
        }
    }

    // 10. Evaluate reaction rules
    let mut reactions_fired = 0usize;
    let mut rules_fired: Vec<String> = Vec::new();
    let mut rules_deduped = 0usize;
    if has_reactions {
        let reactions_start = std::time::Instant::now();

        // Open MetadataStore for coupling reactions (may already be open above, but
        // the store is cheap to reopen and this path also serves ReactionsOnly mode)
        let reaction_metadata = MetadataStore::open(&db_path).ok();
        let reaction_budget = budget.saturating_sub(lines_used);

        // Collect search-based reactions BEFORE evaluate_reactions.
        // Order is load-bearing: `evaluate_reactions` records a dedup key for
        // every rule it fires, and `pending_searches` skips any rule whose key
        // has already fired. Calling it afterwards would find every search rule
        // already deduped and return nothing — a silent no-op indistinguishable
        // from the unwired state this fixes.
        let pending = reactions::pending_searches(&tool_event, &compiled_rules, &dedup, &role);

        let eval_result = reactions::evaluate_reactions(
            &tool_event,
            &compiled_rules,
            &mut dedup,
            reaction_metadata.as_ref(),
            reaction_budget,
            &role,
        );

        if !eval_result.output.is_empty() {
            if !context.is_empty() {
                context.push('\n');
                lines_used += 1;
            }
            context.push_str(&eval_result.output);
            lines_used += eval_result.output.lines().count();
        }

        reactions_fired = eval_result.reactions_fired;
        rules_fired = eval_result.rules_fired.clone();
        rules_deduped = eval_result.rules_deduped;

        // 10b. Execute the search-based reactions whose rule actually fired.
        //
        // Only rules present in `rules_fired` are run: `evaluate_reactions`
        // has already applied dedup, the role check and the budget, and a rule
        // it declined must not still get its files injected.
        let injection_for = |name: &str| -> Option<&String> {
            eval_result
                .rules_fired
                .iter()
                .position(|r| r == name)
                .and_then(|i| eval_result.injection_ids.get(i))
        };
        let to_run: Vec<&reactions::PendingSearch> = pending
            .iter()
            .filter(|p| eval_result.rules_fired.contains(&p.rule_name))
            .collect();

        if !to_run.is_empty() {
            for search in to_run {
                if lines_used >= budget {
                    break;
                }
                let search_start = std::time::Instant::now();
                let hits = run_reaction_search(
                    search,
                    &repo_root,
                    &lance_path,
                    &db_path,
                    &config,
                    &cwd,
                    budget.saturating_sub(lines_used).min(search.max_lines),
                )
                .await;

                // A store that could not be opened is NOT "no results" — that
                // would report an infrastructure failure as a measurement
                // about the index's coverage. Say nothing and emit nothing.
                let Some(hits) = hits else {
                    continue;
                };

                let block = reactions::format_search_results(search, &hits);
                let block_lines = block.lines().count();
                context.push_str(&block);
                lines_used += block_lines;

                let injection_id = injection_for(&search.rule_name)
                    .cloned()
                    .unwrap_or_default();
                let event_type = if hits.is_empty() {
                    "reaction_no_results"
                } else {
                    "reaction_search"
                };
                crate::metrics::emit(
                    &repo_root,
                    &crate::metrics::event(
                        &metrics_source,
                        event_type,
                        &search.rule_name,
                        search_start.elapsed().as_millis() as u64,
                        serde_json::json!({
                            "tool_name": input.tool_name,
                            "rule": search.rule_name,
                            "injection_id": injection_id,
                            "query": search.query,
                            "group": search.group,
                            "tags": search.tags,
                            "results": hits.len(),
                        }),
                    ),
                );
            }
        }

        // Emit per-rule metrics with injection_ids.
        // `duration_ms` carries the reaction-evaluation latency; it was
        // hardcoded to 0, which made every reaction look free and left no way
        // to notice a rule set getting slow.
        let reactions_ms = reactions_start.elapsed().as_millis() as u64;
        for (rule_name, inj_id) in eval_result
            .rules_fired
            .iter()
            .zip(&eval_result.injection_ids)
        {
            crate::metrics::emit(
                &repo_root,
                &crate::metrics::event(
                    &metrics_source,
                    "reaction_fired",
                    rule_name,
                    reactions_ms,
                    serde_json::json!({
                        "tool_name": input.tool_name,
                        "rule": rule_name,
                        "injection_id": inj_id,
                    }),
                ),
            );
        }
    }

    // Edit-time NA grounding uses the same trusted, content-addressed turn
    // reference as task start. It is local-only and faction-bound.
    let na_grounding = if matches!(input.tool_name.as_str(), "Edit" | "Write") {
        verified_na_grounding(input.grounding.as_ref())
            .ok()
            .flatten()
    } else {
        None
    };
    if let Some(grounding) = na_grounding.as_ref() {
        if !context.is_empty() {
            context.push('\n');
        }
        context.push_str(&grounding.text);
        turn.claim_file("urn:neuralamplifier:graph:knowledge");
    }

    // Skip if nothing useful to report across all sections
    if context.is_empty() {
        let dispatch_label = match &mode {
            DispatchMode::EditRelated { file_path } => file_path.clone(),
            DispatchMode::SearchQuery { original_cmd, .. }
            | DispatchMode::DiscoveredFiles { original_cmd, .. } => original_cmd.clone(),
            DispatchMode::RefsOnly { file_path } => file_path.clone(),
            DispatchMode::ReactionsOnly => input.tool_name.clone(),
        };
        crate::metrics::emit(
            &repo_root,
            &crate::metrics::event(
                &metrics_source,
                "hook_post_tool_use",
                "hook post-tool-use",
                hook_start.elapsed().as_millis() as u64,
                serde_json::json!({
                    "tool_name": input.tool_name,
                    "dispatch": dispatch_label,
                    "coupled_count": 0,
                    "search_files": 0,
                    "refs_count": 0,
                    "callees_count": 0,
                    "reactions_fired": 0,
                    "skipped": true,
                    "injection_id": turn.injection_id(),
                }),
            ),
        );
        return Ok(());
    }

    // 11. Output hook response JSON
    let response = HookResponse {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PostToolUse".to_string(),
            additional_context: context.clone(),
        },
    };
    println!("{}", serde_json::to_string(&response)?);

    triggered.commit();

    // 11a. Record and commit (bobbin-aa0), after the response is on stdout.
    record_non_prompt_injection(
        &repo_root,
        &mut turn,
        &input.session_id,
        &dispatch_query,
        &context,
        na_grounding.as_ref(),
    );

    // 12. Emit metric
    let dispatch_label = match &mode {
        DispatchMode::EditRelated { file_path } => file_path.clone(),
        DispatchMode::SearchQuery { original_cmd, .. }
        | DispatchMode::DiscoveredFiles { original_cmd, .. } => original_cmd.clone(),
        DispatchMode::RefsOnly { file_path } => file_path.clone(),
        DispatchMode::ReactionsOnly => input.tool_name.clone(),
    };
    crate::metrics::emit(
        &repo_root,
        &crate::metrics::event(
            &metrics_source,
            "hook_post_tool_use",
            "hook post-tool-use",
            hook_start.elapsed().as_millis() as u64,
            serde_json::json!({
                "tool_name": input.tool_name,
                "dispatch": dispatch_label,
                "coupled_count": coupled_count,
                "search_files": search_file_count,
                "refs_count": refs_count,
                "callees_count": callees_count,
                "injection_id": turn.injection_id(),
                "ledger_chunks": turn.len(),
                "reactions_fired": reactions_fired,
                "reactions_rules": rules_fired,
                "reactions_deduped": rules_deduped,
            }),
        ),
    );

    Ok(())
}

async fn run_post_tool_use_failure(
    _args: PostToolUseFailureArgs,
    _output: OutputConfig,
) -> Result<()> {
    // Route to remote handler if --server is set
    if let Some(ref server_url) = _output.server {
        return match run_post_tool_use_failure_remote(_args, server_url, &_output.role).await {
            Ok(()) => Ok(()),
            Err(e) => {
                eprintln!("bobbin post-tool-use-failure (remote): {:#}", e);
                Ok(())
            }
        };
    }
    // Never block on failure handling — any error exits silently
    match run_post_tool_use_failure_inner(_args).await {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("bobbin post-tool-use-failure: {:#}", e);
            Ok(())
        }
    }
}

/// PostToolUseFailure handler: When a tool fails, search bobbin for context
/// related to the error to help the agent recover.
async fn run_post_tool_use_failure_inner(args: PostToolUseFailureArgs) -> Result<()> {
    use crate::index::Embedder;
    use crate::search::context::{BridgeMode, ContentMode, ContextAssembler, ContextConfig};

    let hook_start = std::time::Instant::now();

    // 1. Read stdin JSON
    let input: PostToolUseFailureInput =
        serde_json::from_reader(std::io::stdin().lock()).context("Failed to parse stdin JSON")?;

    // Skip if no error message to search with
    if input.error.trim().is_empty() {
        return Ok(());
    }

    // Fast-path: Read tool directory navigation injection
    // When Read fails on EISDIR or file-not-found, inject tree output
    if input.tool_name == "Read" {
        if let Some(output) = try_directory_navigation(&input) {
            let response = HookResponse {
                hook_specific_output: HookSpecificOutput {
                    hook_event_name: "PostToolUseFailure".to_string(),
                    additional_context: output,
                },
            };
            println!("{}", serde_json::to_string(&response)?);
            return Ok(());
        }
    }

    // 2. Resolve bobbin root and config
    let cwd = if input.cwd.is_empty() {
        std::env::current_dir().context("Failed to get cwd")?
    } else {
        PathBuf::from(&input.cwd)
    };

    let repo_root = match find_bobbin_root(&cwd) {
        Some(r) => r,
        None => return Ok(()), // Not a bobbin-indexed project
    };

    let config = Config::load(&Config::config_path(&repo_root)).unwrap_or_default();
    let budget = args.budget.unwrap_or(config.hooks.budget / 2); // Use half budget for failure context

    let metrics_source = crate::metrics::resolve_source(
        None,
        if input.session_id.is_empty() {
            None
        } else {
            Some(&input.session_id)
        },
    );

    // 3. Extract command hint and error excerpt
    let command = input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let file_hint = input
        .tool_input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or(command);

    // Truncate error to avoid overwhelming the search
    let error_excerpt = if input.error.len() > 500 {
        &input.error[..500]
    } else {
        &input.error
    };

    // 4. Open stores
    let lance_path = Config::lance_path(&repo_root);
    let db_path = Config::db_path(&repo_root);
    let model_dir = Config::model_cache_dir()?;

    let vector_store = match VectorStore::open(&lance_path).await {
        Ok(vs) => vs,
        Err(_) => return Ok(()),
    };

    if vector_store.count().await? == 0 {
        return Ok(());
    }

    let metadata_store = match MetadataStore::open(&db_path) {
        Ok(ms) => ms,
        Err(_) => return Ok(()),
    };

    // 5. Check model consistency
    let current_model = config.embedding.model.as_str();
    if let Some(stored) = metadata_store.get_meta("embedding_model")? {
        if stored != current_model {
            return Ok(());
        }
    }

    // 5a. Open the injection turn (bobbin-aa0). Both branches below claim
    //     chunks through it, so neither re-delivers something the prompt path
    //     already sent, and both are recorded under one identifier that
    //     feedback can rate.
    let mut turn = InjectionTurn::open(
        &repo_root,
        &input.session_id,
        config.hooks.reducing_enabled,
        error_excerpt,
    );

    // 6. Try error-context injection: parse file paths from build/test errors
    //    and inject those files directly (faster + more precise than semantic search).
    let parsed = crate::errors::parse_error_output(&input.error, command);
    let mut direct_injection_output = String::new();
    let mut direct_files_found = 0usize;
    let mut direct_chunks_found = 0usize;
    let mut lines_used = 0usize;

    if input.tool_name == "Bash" && !parsed.refs.is_empty() {
        let repo_name = detect_repo_name(&cwd);

        for error_ref in &parsed.refs {
            if lines_used >= budget {
                break;
            }

            // Skip empty paths (symbol-only refs from test output)
            if error_ref.path.is_empty() {
                continue;
            }

            // Fetch chunks for this file from the index
            let chunks = vector_store
                .get_chunks_for_file(&error_ref.path, repo_name.as_deref())
                .await;
            let chunks = match chunks {
                Ok(c) if !c.is_empty() => c,
                _ => continue,
            };

            direct_files_found += 1;

            // If we have a specific line number, find the chunk containing it
            let relevant_chunks: Vec<_> = if let Some(line) = error_ref.line {
                let mut matching: Vec<_> = chunks
                    .iter()
                    .filter(|c| c.start_line <= line && c.end_line >= line)
                    .collect();
                // If no chunk spans the exact line, take the nearest chunk
                if matching.is_empty() {
                    matching = chunks
                        .iter()
                        .min_by_key(|c| {
                            let mid = (c.start_line + c.end_line) / 2;
                            (mid as i64 - line as i64).unsigned_abs()
                        })
                        .into_iter()
                        .collect();
                }
                matching
            } else {
                // No line number: take the first few chunks (file header / key definitions)
                chunks.iter().take(3).collect()
            };

            for chunk in &relevant_chunks {
                let chunk_lines = chunk.content.lines().count();
                if lines_used + chunk_lines + 3 > budget {
                    break;
                }

                // bobbin-aa0: skip anything already delivered this session.
                if !turn.claim(&chunk.file_path, chunk.start_line, chunk.end_line) {
                    continue;
                }

                let line_info = if let Some(line) = error_ref.line {
                    format!(" (error at line {})", line)
                } else {
                    String::new()
                };
                let symbol_info = error_ref
                    .symbol
                    .as_ref()
                    .map(|s| format!(" — symbol: `{}`", s))
                    .unwrap_or_default();

                direct_injection_output.push_str(&format!(
                    "### {}:{}-{}{}{}\n```{}\n{}\n```\n\n",
                    chunk.file_path,
                    chunk.start_line,
                    chunk.end_line,
                    line_info,
                    symbol_info,
                    chunk.language,
                    chunk.content.trim_end(),
                ));
                lines_used += chunk_lines + 3;
                direct_chunks_found += 1;
            }

            // Include coupled files for this error file (co-changing files)
            if lines_used < budget {
                if let Ok(coupling) =
                    crate::reactions::query_coupling(&metadata_store, &error_ref.path, 0.3, 3)
                {
                    for coupled in &coupling.coupled_files {
                        if lines_used >= budget {
                            break;
                        }
                        // Fetch preview of coupled file
                        let coupled_chunks = vector_store
                            .get_chunks_for_file(&coupled.path, repo_name.as_deref())
                            .await;
                        if let Ok(cc) = coupled_chunks {
                            if let Some(first) = cc.first() {
                                let chunk_lines = first.content.lines().count();
                                if lines_used + chunk_lines + 3 <= budget
                                    && turn.claim(
                                        &first.file_path,
                                        first.start_line,
                                        first.end_line,
                                    )
                                {
                                    direct_injection_output.push_str(&format!(
                                        "### {} (coupled: {:.0}% co-change rate)\n```{}\n{}\n```\n\n",
                                        first.file_path,
                                        coupled.score * 100.0,
                                        first.language,
                                        first.content.trim_end(),
                                    ));
                                    lines_used += chunk_lines + 3;
                                    direct_chunks_found += 1;
                                    direct_files_found += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 7. If direct injection found files, output them; otherwise fall back to semantic search
    let (output_text, method) = if !direct_injection_output.is_empty() {
        let header = format!(
            "Bobbin found {} source chunks in {} files referenced by this error:\n\n",
            direct_chunks_found, direct_files_found,
        );
        (format!("{}{}", header, direct_injection_output), "direct")
    } else {
        // Semantic search fallback (original behavior)
        let query = format!("{} {} error: {}", input.tool_name, file_hint, error_excerpt);
        let embedder = Embedder::from_config(&config.embedding, &model_dir)
            .context("Failed to load embedding model")?;

        let context_config = ContextConfig {
            budget_lines: budget,
            depth: 0,
            max_coupled: 0,
            coupling_threshold: 0.1,
            semantic_weight: config.search.semantic_weight,
            content_mode: ContentMode::Preview,
            search_limit: 10,
            doc_demotion: config.search.doc_demotion,
            recency_half_life_days: config.search.recency_half_life_days,
            recency_weight: config.search.recency_weight,
            rrf_k: config.search.rrf_k,
            bridge_mode: BridgeMode::Off,
            bridge_boost_factor: 0.0,
            extra_filter: None,
            tags_config: None,
            role: None,
            file_type_rules: config.file_types.clone(),
            repo_affinity: detect_repo_name(&cwd),
            repo_affinity_boost: config.hooks.repo_affinity_boost,
            max_bridged_files: 2,
            max_bridged_chunks_per_file: 1,
            repo_path_prefix: config.server.repo_path_prefix.clone(),
            ..ContextConfig::default()
        };

        let mut assembler =
            ContextAssembler::new(embedder, vector_store, metadata_store, context_config);
        let mut bundle = assembler.assemble(&query, None).await?;

        if bundle.files.is_empty() || bundle.summary.top_semantic_score < 0.3 {
            return Ok(());
        }

        // bobbin-aa0: filter against the session ledger before formatting. This
        // path passed None for the injection id and never consulted the ledger,
        // so it could re-send chunks the prompt path had already delivered and
        // nothing it injected could be rated.
        for file in &mut bundle.files {
            file.chunks
                .retain(|c| turn.claim(&file.path, c.start_line, c.end_line));
        }
        bundle.files.retain(|f| !f.chunks.is_empty());

        if bundle.files.is_empty() {
            eprintln!("bobbin: skipped (all error-context chunks previously injected)");
            return Ok(());
        }

        let context_text = format_context_for_injection(
            &bundle,
            config.hooks.threshold,
            false,
            Some(turn.injection_id()),
            &config.hooks.format_mode,
        );
        let header = format!(
            "Bobbin found {} relevant chunks for this error (via semantic search):\n\n",
            turn.len(),
        );
        (format!("{}{}", header, context_text), "semantic")
    };

    // 8. Output response
    let response = HookResponse {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PostToolUseFailure".to_string(),
            additional_context: output_text.clone(),
        },
    };
    println!("{}", serde_json::to_string(&response)?);

    // 8a. Record the injection so feedback can rate it, and commit the claimed
    //     chunks to the session ledger (bobbin-aa0). Both happen only after the
    //     response is actually on stdout — recording chunks this turn never
    //     emitted would suppress them on the next turn for nothing.
    record_non_prompt_injection(
        &repo_root,
        &mut turn,
        &input.session_id,
        error_excerpt,
        &output_text,
        None,
    );

    // 9. Emit metric
    crate::metrics::emit(
        &repo_root,
        &crate::metrics::event(
            &metrics_source,
            "hook_post_tool_use_failure",
            "hook post-tool-use-failure",
            hook_start.elapsed().as_millis() as u64,
            serde_json::json!({
                "tool_name": input.tool_name,
                "error_excerpt": &error_excerpt[..error_excerpt.len().min(200)],
                "method": method,
                "parsed_refs": parsed.refs.len(),
                "is_build_error": parsed.is_build_error,
                "direct_files": direct_files_found,
                "direct_chunks": direct_chunks_found,
                "injection_id": turn.injection_id(),
                "ledger_chunks": turn.len(),
            }),
        ),
    );

    Ok(())
}

/// Store an injection record and commit the turn's chunks to the ledger.
///
/// Shared by the failure and post-tool-use handlers (bobbin-aa0). Both are
/// best-effort: a feedback store that will not open must never turn a
/// successful injection into a failed hook, because the hook's output has
/// already been written to stdout by the time this runs.
fn record_non_prompt_injection(
    repo_root: &Path,
    turn: &mut InjectionTurn,
    cc_session_id: &str,
    query: &str,
    output_text: &str,
    grounding: Option<&VerifiedNaGrounding>,
) {
    if turn.is_empty() {
        return;
    }

    let feedback_db_path = Config::feedback_db_path(repo_root);
    if let Ok(fb_store) = crate::storage::feedback::FeedbackStore::open(&feedback_db_path) {
        let files: Vec<String> = turn.claimed_files();
        let session_id = if cc_session_id.is_empty() {
            None
        } else {
            Some(cc_session_id)
        };
        let _ = fb_store.store_scoped_injection_with_output(
            turn.injection_id(),
            session_id,
            None,
            query,
            &files,
            turn.len(),
            output_text.lines().count(),
            Some(output_text),
            grounding.map(|_| "na"),
            grounding.map(|g| g.faction_id.as_str()),
            grounding.map(|g| g.grounding_id.as_str()),
        );
    }

    turn.commit();
}

/// Remote-server implementation of PostToolUseFailure.
/// Uses HTTP client to fetch file chunks and context instead of local stores.
async fn run_post_tool_use_failure_remote(
    args: PostToolUseFailureArgs,
    server_url: &str,
    role: &str,
) -> Result<()> {
    use crate::http::client::Client;

    let hook_start = std::time::Instant::now();

    // 1. Read stdin JSON
    let input: PostToolUseFailureInput =
        serde_json::from_reader(std::io::stdin().lock()).context("Failed to parse stdin JSON")?;

    if input.error.trim().is_empty() {
        return Ok(());
    }

    // Fast-path: Read tool directory navigation
    if input.tool_name == "Read" {
        if let Some(output) = try_directory_navigation(&input) {
            let response = HookResponse {
                hook_specific_output: HookSpecificOutput {
                    hook_event_name: "PostToolUseFailure".to_string(),
                    additional_context: output,
                },
            };
            println!("{}", serde_json::to_string(&response)?);
            return Ok(());
        }
    }

    // 2. Load config for budget
    let cwd = if input.cwd.is_empty() {
        std::env::current_dir().context("Failed to get cwd")?
    } else {
        PathBuf::from(&input.cwd)
    };

    let repo_root = find_bobbin_root(&cwd);
    let config = repo_root
        .as_ref()
        .map(|r| Config::load(&Config::config_path(r)).unwrap_or_default())
        .unwrap_or_default();
    let budget = args.budget.unwrap_or(config.hooks.budget / 2);

    let command = input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let error_excerpt = if input.error.len() > 500 {
        &input.error[..500]
    } else {
        &input.error
    };

    let client = Client::new(server_url);
    let repo_affinity = detect_repo_name(&cwd);

    // 3. Try error-context injection: parse file paths from build/test errors
    let parsed = crate::errors::parse_error_output(&input.error, command);
    let mut direct_output = String::new();
    let mut direct_files = 0usize;
    let mut direct_chunks = 0usize;
    let mut lines_used = 0usize;

    // Resolve relative paths to server paths.
    // Server stores files as /var/lib/bobbin/repos/<repo>/<path>.
    // Use config repo_path_prefix if set, otherwise default to /var/lib/bobbin/repos/.
    let repo_prefix = config
        .server
        .repo_path_prefix
        .as_deref()
        .unwrap_or("/var/lib/bobbin/repos");

    // Detect the actual repo name for server path resolution.
    // In Gas Town, crew workspaces are at /<town>/<rig>/crew/<name>/ — the rig
    // name is the repo name on the server, not the git root directory.
    let server_repo = detect_server_repo_name(&cwd).or(repo_affinity.clone());

    let resolve_path = |path: &str| -> String {
        if path.starts_with('/') {
            path.to_string()
        } else if let Some(ref repo) = server_repo {
            format!("{}/{}/{}", repo_prefix, repo, path.trim_start_matches("./"))
        } else {
            path.to_string()
        }
    };

    if input.tool_name == "Bash" && !parsed.refs.is_empty() {
        for error_ref in &parsed.refs {
            if lines_used >= budget || error_ref.path.is_empty() {
                continue;
            }

            let server_path = resolve_path(&error_ref.path);

            // Use read_chunk to fetch code around the error line
            let (start, end) = if let Some(line) = error_ref.line {
                // Fetch ~40 lines around the error
                (line.saturating_sub(10), line + 30)
            } else {
                // No line number — fetch the top of the file
                (1, 50)
            };

            if let Ok(chunk) = client.read_chunk(&server_path, start, end, Some(5)).await {
                let chunk_lines = chunk.content.lines().count();
                if lines_used + chunk_lines + 3 <= budget {
                    let line_info = error_ref
                        .line
                        .map(|l| format!(" (error at line {})", l))
                        .unwrap_or_default();
                    let symbol_info = error_ref
                        .symbol
                        .as_ref()
                        .map(|s| format!(" — symbol: `{}`", s))
                        .unwrap_or_default();

                    direct_output.push_str(&format!(
                        "### {}:{}-{}{}{}\n```{}\n{}\n```\n\n",
                        chunk.file,
                        chunk.actual_start_line,
                        chunk.actual_end_line,
                        line_info,
                        symbol_info,
                        chunk.language,
                        chunk.content.trim_end(),
                    ));
                    lines_used += chunk_lines + 3;
                    direct_chunks += 1;
                    direct_files += 1;
                }
            }

            // Fetch coupled/related files
            if lines_used < budget {
                if let Ok(related) = client.related(&server_path, 3, Some(0.3)).await {
                    for rel in &related.related {
                        if lines_used >= budget {
                            break;
                        }
                        if let Ok(rel_chunk) = client.read_chunk(&rel.path, 1, 30, None).await {
                            let chunk_lines = rel_chunk.content.lines().count();
                            if lines_used + chunk_lines + 3 <= budget {
                                direct_output.push_str(&format!(
                                    "### {} (coupled: {:.0}% co-change rate)\n```{}\n{}\n```\n\n",
                                    rel.path,
                                    rel.score * 100.0,
                                    rel_chunk.language,
                                    rel_chunk.content.trim_end(),
                                ));
                                lines_used += chunk_lines + 3;
                                direct_chunks += 1;
                                direct_files += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    // 4. Direct injection or semantic search fallback
    let (output_text, method) = if !direct_output.is_empty() {
        let header = format!(
            "Bobbin found {} source chunks in {} files referenced by this error:\n\n",
            direct_chunks, direct_files,
        );
        (format!("{}{}", header, direct_output), "direct-remote")
    } else {
        // Semantic search fallback via /context endpoint
        let query = format!(
            "{} {} error: {}",
            input.tool_name,
            command,
            &error_excerpt[..error_excerpt.len().min(200)],
        );
        match client
            .context(
                &query,
                Some(budget),
                Some(0),  // depth
                Some(0),  // max_coupled
                Some(10), // limit
                None,     // coupling_threshold
                None,     // repo
                if role.is_empty() { None } else { Some(role) },
                repo_affinity.as_deref(),
            )
            .await
        {
            Ok(ctx) if !ctx.files.is_empty() => {
                let mut text = format!(
                    "Bobbin found {} relevant files for this error (via semantic search):\n\n",
                    ctx.files.len(),
                );
                for file in &ctx.files {
                    for chunk in &file.chunks {
                        text.push_str(&format!(
                            "### {}:{}-{}\n```\n{}\n```\n\n",
                            file.path,
                            chunk.start_line,
                            chunk.end_line,
                            chunk.content.as_deref().unwrap_or("").trim_end(),
                        ));
                    }
                }
                (text, "semantic-remote")
            }
            _ => return Ok(()),
        }
    };

    // 5. Output response
    let response = HookResponse {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PostToolUseFailure".to_string(),
            additional_context: output_text,
        },
    };
    println!("{}", serde_json::to_string(&response)?);

    // 6. Emit metric (local if we have a repo root)
    if let Some(ref root) = repo_root {
        let metrics_source = crate::metrics::resolve_source(
            None,
            if input.session_id.is_empty() {
                None
            } else {
                Some(&input.session_id)
            },
        );
        crate::metrics::emit(
            root,
            &crate::metrics::event(
                &metrics_source,
                "hook_post_tool_use_failure",
                "hook post-tool-use-failure",
                hook_start.elapsed().as_millis() as u64,
                serde_json::json!({
                    "tool_name": input.tool_name,
                    "error_excerpt": &error_excerpt[..error_excerpt.len().min(200)],
                    "method": method,
                    "parsed_refs": parsed.refs.len(),
                    "is_build_error": parsed.is_build_error,
                    "direct_files": direct_files,
                    "direct_chunks": direct_chunks,
                }),
            ),
        );
    }

    Ok(())
}

/// Fast-path directory navigation: when Read fails on a directory or missing file,
/// run `tree` and return the output. Returns None if not applicable.
fn try_directory_navigation(input: &PostToolUseFailureInput) -> Option<String> {
    let file_path = input.tool_input.get("file_path").and_then(|v| v.as_str())?;
    let error = &input.error;

    // Skip paths in /tmp or other irrelevant locations
    if file_path.starts_with("/tmp")
        || file_path.starts_with("/proc")
        || file_path.starts_with("/sys")
    {
        return None;
    }

    let (tree_path, header) = if error.contains("EISDIR") || error.contains("Is a directory") {
        // Read on directory: show its contents
        (
            file_path.to_string(),
            format!("{} is a directory. Contents:", file_path),
        )
    } else if error.contains("does not exist")
        || error.contains("ENOENT")
        || error.contains("No such file")
    {
        // File not found: show parent directory
        let parent = std::path::Path::new(file_path).parent()?;
        if !parent.exists() {
            return None;
        }
        (
            parent.to_string_lossy().to_string(),
            format!("File not found. Nearby files in {}:", parent.display()),
        )
    } else {
        return None;
    };

    // Run tree with depth limit
    let output = std::process::Command::new("tree")
        .args(["-L", "2", "--noreport", &tree_path])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let tree_text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = tree_text.lines().collect();

    // Cap at 20 lines
    let truncated = if lines.len() > 20 {
        let shown: Vec<&str> = lines[..20].to_vec();
        format!(
            "{}\n... and {} more entries",
            shown.join("\n"),
            lines.len() - 20
        )
    } else {
        lines.join("\n")
    };

    Some(format!("{}\n```\n{}\n```", header, truncated))
}

async fn run_session_context(args: SessionContextArgs, _output: OutputConfig) -> Result<()> {
    // Never block session start — wrap everything in a catch-all
    match run_session_context_inner(args).await {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("bobbin session-context: {}", e);
            Ok(())
        }
    }
}

/// Input JSON from Claude Code SessionStart hook
#[derive(Deserialize)]
struct SessionStartInput {
    #[serde(default)]
    source: String,
    #[serde(default)]
    cwd: String,
    /// Claude Code session ID (used as metrics source identity)
    #[serde(default)]
    session_id: String,
}

/// Output JSON for Claude Code hook response
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HookResponse {
    hook_specific_output: HookSpecificOutput,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HookSpecificOutput {
    hook_event_name: String,
    additional_context: String,
}

/// A file with its symbols for display
struct FileSymbolInfo {
    path: String,
    symbols: Vec<SymbolInfo>,
}

struct SymbolInfo {
    name: String,
}

async fn run_session_context_inner(args: SessionContextArgs) -> Result<()> {
    // 1. Read stdin JSON
    let input_str = std::io::read_to_string(std::io::stdin()).context("Failed to read stdin")?;

    // If stdin is empty, nothing to do
    if input_str.trim().is_empty() {
        return Ok(());
    }

    let input: SessionStartInput =
        serde_json::from_str(&input_str).context("Failed to parse stdin JSON")?;

    // 2. Only handle compact events
    if input.source != "compact" {
        return Ok(());
    }

    // 3. Determine repo root
    let cwd = if input.cwd.is_empty() {
        std::env::current_dir().context("Failed to get cwd")?
    } else {
        PathBuf::from(&input.cwd)
            .canonicalize()
            .context("Invalid cwd path")?
    };

    // 3b. Reset session reducing ledger on compaction — agent lost prior context
    if !input.session_id.is_empty() {
        SessionLedger::clear(&cwd, &input.session_id);
        eprintln!("bobbin: reset reducing ledger (compaction)");
    }

    // Load config (use defaults if not initialized)
    let root = find_bobbin_root(&cwd).unwrap_or(cwd.clone());
    let config = Config::load(&Config::config_path(&root)).unwrap_or_default();
    let budget = args.budget.unwrap_or(config.hooks.budget);
    let pins = memory_anchor::run(&config, budget).await;
    let remaining = budget.saturating_sub(pins.lines().count());

    // 4. Gather git signals
    let modified_files = git_status_files(&cwd).unwrap_or_default();
    let recent_commits = git_recent_commits(&cwd, 5).unwrap_or_default();
    let recently_changed_files = git_recently_changed_files(&cwd, 3).unwrap_or_default();

    let discovery = breadcrumbs::discovery(&root, remaining);
    // If git, breadcrumbs and memory pins are all empty, exit silently.
    if modified_files.is_empty()
        && recent_commits.is_empty()
        && recently_changed_files.is_empty()
        && discovery.is_empty()
        && pins.is_empty()
    {
        return Ok(());
    }

    // 5. Collect all interesting file paths (deduped)
    let mut all_files: HashSet<String> = HashSet::new();
    for f in &modified_files {
        all_files.insert(f.clone());
    }
    for f in &recently_changed_files {
        all_files.insert(f.clone());
    }

    // 6. Query bobbin for symbols and coupling (best-effort)
    let mut file_symbols: Vec<FileSymbolInfo> = Vec::new();
    let mut coupled_files: Vec<(String, String, f32)> = Vec::new(); // (path, coupled_to, score)

    let lance_path = Config::lance_path(&cwd);
    let db_path = Config::db_path(&cwd);

    if lance_path.exists() && db_path.exists() {
        let vector_store = match VectorStore::open(&lance_path).await {
            Ok(vs) => Some(vs),
            Err(e) => {
                eprintln!("bobbin: vector store unavailable: {}", e);
                None
            }
        };
        let metadata_store = match MetadataStore::open(&db_path) {
            Ok(ms) => Some(ms),
            Err(e) => {
                eprintln!("bobbin: metadata store unavailable: {}", e);
                None
            }
        };

        // Get symbols for each file
        if let Some(ref vs) = vector_store {
            for file_path in &all_files {
                if let Ok(chunks) = vs.get_chunks_for_file(file_path, None).await {
                    let symbols: Vec<SymbolInfo> = chunks
                        .iter()
                        .filter(|c| c.name.is_some())
                        .map(|c| SymbolInfo {
                            name: c.name.clone().unwrap_or_default(),
                        })
                        .collect();
                    if !symbols.is_empty() {
                        file_symbols.push(FileSymbolInfo {
                            path: file_path.clone(),
                            symbols,
                        });
                    }
                }
            }
        }

        // Get coupled files
        if let Some(ref ms) = metadata_store {
            let mut seen_coupled: HashSet<String> = HashSet::new();
            for file_path in &all_files {
                if let Ok(couplings) = ms.get_coupling(file_path, 3) {
                    for c in couplings {
                        let other = if c.file_a == *file_path {
                            &c.file_b
                        } else {
                            &c.file_a
                        };
                        if !all_files.contains(other) && !seen_coupled.contains(other) {
                            seen_coupled.insert(other.clone());
                            coupled_files.push((other.clone(), file_path.clone(), c.score));
                        }
                    }
                }
            }
        }
    }

    // Sort file symbols by path for stable output
    file_symbols.sort_by(|a, b| a.path.cmp(&b.path));
    coupled_files.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    // Pins have their own 15-line cap and reserve space in the existing
    // total budget; git/breadcrumb recovery continues after a memory outage.
    let git_budget = remaining.saturating_sub(discovery.lines().count());
    let git_context = if git_budget == 0
        || (modified_files.is_empty()
            && recent_commits.is_empty()
            && file_symbols.is_empty()
            && coupled_files.is_empty()
            && discovery.is_empty())
    {
        String::new()
    } else {
        format_session_context(
            &modified_files,
            &recent_commits,
            &file_symbols,
            &coupled_files,
            git_budget,
        )
    };
    let context_md = memory_anchor::combine(&[&pins, &git_context, &discovery], budget);

    if context_md.is_empty() {
        return Ok(());
    }

    // 8. Output hook response JSON
    let response = HookResponse {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "SessionStart".to_string(),
            additional_context: context_md,
        },
    };

    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

/// Get modified/staged/untracked files from git status
fn git_status_files(cwd: &std::path::Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .context("Failed to run git status")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            // git status --porcelain format: "XY path" where XY is a 2-char
            // status code at fixed positions 0-1, followed by a space at
            // position 2, then the path.  Do NOT trim the line first — the
            // leading space in " M" is part of the status code.
            if line.len() > 3 {
                Some(line[3..].to_string())
            } else {
                None
            }
        })
        .collect();

    Ok(files)
}

/// Get recent commit summaries
fn git_recent_commits(cwd: &std::path::Path, count: usize) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["log", "--oneline", &format!("-{}", count)])
        .current_dir(cwd)
        .output()
        .context("Failed to run git log")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().map(|l| l.to_string()).collect())
}

/// Get files changed in recent commits
fn git_recently_changed_files(cwd: &std::path::Path, depth: usize) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--name-only", &format!("HEAD~{}..HEAD", depth)])
        .current_dir(cwd)
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        // May fail if repo has fewer commits than depth
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect())
}

/// Format session context as compact markdown for Claude Code SessionStart recovery.
///
/// Produces a markdown summary of working state (modified files, recent commits,
/// coupled files) within the given line budget. Budget is enforced on output lines;
/// if truncation is needed the last line is a notice message, counted within budget.
fn format_session_context(
    modified_files: &[String],
    recent_commits: &[String],
    file_symbols: &[FileSymbolInfo],
    coupled_files: &[(String, String, f32)],
    budget: usize,
) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push("## Working Context (recovered after compaction)".to_string());
    lines.push(String::new());

    // Modified files section
    if !modified_files.is_empty() {
        lines.push("### Modified files".to_string());
        for file in modified_files {
            // Find symbols for this file
            let symbols_str = file_symbols
                .iter()
                .find(|fs| fs.path == *file)
                .map(|fs| {
                    let names: Vec<String> =
                        fs.symbols.iter().take(5).map(|s| s.name.clone()).collect();
                    if names.is_empty() {
                        String::new()
                    } else {
                        let count = fs.symbols.len();
                        let display = names.join(", ");
                        if count > 5 {
                            format!(" ({} symbols: {}, ...)", count, display)
                        } else {
                            format!(" ({} symbols: {})", count, display)
                        }
                    }
                })
                .unwrap_or_default();
            lines.push(format!("- {}{}", file, symbols_str));
        }
        lines.push(String::new());
    }

    // Recent commits section
    if !recent_commits.is_empty() {
        lines.push("### Recent commits".to_string());
        for commit in recent_commits {
            lines.push(format!("- {}", commit));
        }
        lines.push(String::new());
    }

    // File symbols for non-modified files (recently changed files that aren't modified)
    let modified_set: HashSet<&String> = modified_files.iter().collect();
    let other_symbols: Vec<&FileSymbolInfo> = file_symbols
        .iter()
        .filter(|fs| !modified_set.contains(&fs.path))
        .collect();

    if !other_symbols.is_empty() {
        lines.push("### Recently changed files".to_string());
        for fs in &other_symbols {
            let names: Vec<String> = fs.symbols.iter().take(5).map(|s| s.name.clone()).collect();
            let symbols_str = if names.is_empty() {
                String::new()
            } else {
                let count = fs.symbols.len();
                let display = names.join(", ");
                if count > 5 {
                    format!(" ({} symbols: {}, ...)", count, display)
                } else {
                    format!(" ({} symbols: {})", count, display)
                }
            };
            lines.push(format!("- {}{}", fs.path, symbols_str));
        }
        lines.push(String::new());
    }

    // Coupled files section
    if !coupled_files.is_empty() {
        lines.push("### Related files (via coupling)".to_string());
        for (path, coupled_to, score) in coupled_files.iter().take(5) {
            lines.push(format!(
                "- {} (coupled with {}, score: {:.2})",
                path, coupled_to, score
            ));
        }
        lines.push(String::new());
    }

    // Enforce budget (reserve 1 line for truncation message if needed)
    if lines.len() > budget {
        lines.truncate(budget.saturating_sub(1));
        lines.push("... (truncated to fit budget)".to_string());
    }

    lines.join("\n")
}

const GIT_HOOK_START_MARKER: &str = "# >>> bobbin post-commit hook >>>";
const GIT_HOOK_END_MARKER: &str = "# <<< bobbin post-commit hook <<<";

const GIT_HOOK_SECTION: &str = r#"# >>> bobbin post-commit hook >>>
# Auto-generated by `bobbin hook install-git-hook` — do not edit this section
if command -v bobbin >/dev/null 2>&1; then
  bobbin index --quiet &
  bobbin bead auto-link --commit HEAD --quiet >/dev/null 2>&1 &
fi
# <<< bobbin post-commit hook <<<"#;

/// Marker line that distinguishes a current hook section from a stale one
/// (installed before the bead auto-link integration, bo-5em9). `hook status`
/// uses it to nudge users to re-run `install-git-hook`.
const GIT_HOOK_AUTOLINK_MARKER: &str = "bobbin bead auto-link";

/// Find the .git/hooks directory for the current repo.
fn git_hooks_dir() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .context("Failed to run git rev-parse")?;
    if !output.status.success() {
        anyhow::bail!("Not in a git repository");
    }
    let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(git_dir).join("hooks"))
}

async fn run_install_git_hook(_args: InstallGitHookArgs, output: OutputConfig) -> Result<()> {
    let hooks_dir = git_hooks_dir()?;
    let hook_path = hooks_dir.join("post-commit");

    std::fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("Failed to create {}", hooks_dir.display()))?;

    let content = if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path)
            .with_context(|| format!("Failed to read {}", hook_path.display()))?;

        if existing.contains(GIT_HOOK_START_MARKER) {
            // Already installed — replace existing section
            let mut result = String::new();
            let mut in_bobbin_section = false;
            for line in existing.lines() {
                if line.contains(GIT_HOOK_START_MARKER) {
                    in_bobbin_section = true;
                    result.push_str(GIT_HOOK_SECTION);
                    result.push('\n');
                } else if line.contains(GIT_HOOK_END_MARKER) {
                    in_bobbin_section = false;
                    // Already included in GIT_HOOK_SECTION above
                } else if !in_bobbin_section {
                    result.push_str(line);
                    result.push('\n');
                }
            }
            result
        } else {
            // Append to existing hook
            let mut result = existing;
            if !result.ends_with('\n') {
                result.push('\n');
            }
            result.push('\n');
            result.push_str(GIT_HOOK_SECTION);
            result.push('\n');
            result
        }
    } else {
        // New hook file
        format!("#!/bin/sh\n\n{}\n", GIT_HOOK_SECTION)
    };

    std::fs::write(&hook_path, &content)
        .with_context(|| format!("Failed to write {}", hook_path.display()))?;

    // Make executable
    let perms = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&hook_path, perms)
        .with_context(|| format!("Failed to set permissions on {}", hook_path.display()))?;

    if output.json {
        let result = json!({
            "status": "installed",
            "path": hook_path.display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if !output.quiet {
        println!("{} Git post-commit hook installed", "✓".green(),);
        println!("  Location: {}", hook_path.display().to_string().dimmed());
        println!(
            "  Action:   {} after each commit",
            "bobbin index --quiet".cyan()
        );
    }

    Ok(())
}

async fn run_uninstall_git_hook(_args: UninstallGitHookArgs, output: OutputConfig) -> Result<()> {
    let hooks_dir = git_hooks_dir()?;
    let hook_path = hooks_dir.join("post-commit");

    if !hook_path.exists() {
        if output.json {
            let result = json!({
                "status": "not_installed",
                "path": hook_path.display().to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else if !output.quiet {
            println!("No post-commit hook found");
        }
        return Ok(());
    }

    let existing = std::fs::read_to_string(&hook_path)
        .with_context(|| format!("Failed to read {}", hook_path.display()))?;

    if !existing.contains(GIT_HOOK_START_MARKER) {
        if output.json {
            let result = json!({
                "status": "not_installed",
                "path": hook_path.display().to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else if !output.quiet {
            println!("No bobbin hook found in {}", hook_path.display());
        }
        return Ok(());
    }

    // Remove bobbin section
    let mut result = String::new();
    let mut in_bobbin_section = false;
    let mut prev_blank = false;
    for line in existing.lines() {
        if line.contains(GIT_HOOK_START_MARKER) {
            in_bobbin_section = true;
            // Remove preceding blank line if any
            if prev_blank && result.ends_with('\n') {
                // Trim trailing blank line
                let trimmed = result.trim_end_matches('\n');
                result = format!("{}\n", trimmed);
            }
            continue;
        }
        if line.contains(GIT_HOOK_END_MARKER) {
            in_bobbin_section = false;
            continue;
        }
        if !in_bobbin_section {
            result.push_str(line);
            result.push('\n');
            prev_blank = line.trim().is_empty();
        }
    }

    // Check if remaining content is just a shebang
    let meaningful = result
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("#!"))
        .count();

    if meaningful == 0 {
        // Nothing left — remove the file
        std::fs::remove_file(&hook_path)
            .with_context(|| format!("Failed to remove {}", hook_path.display()))?;
    } else {
        std::fs::write(&hook_path, &result)
            .with_context(|| format!("Failed to write {}", hook_path.display()))?;
    }

    if output.json {
        let result = json!({
            "status": "uninstalled",
            "path": hook_path.display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if !output.quiet {
        println!("{} Bobbin post-commit hook removed", "✓".green(),);
    }

    Ok(())
}

/// Keep the strongest coupling score per path, best first, capped at `limit`.
///
/// bobbin-au4: this was `sort_by(score desc)` followed by
/// `dedup_by(|a, b| a.0 == b.0)`. `dedup_by` only removes **consecutive**
/// equal entries, and the sort is by score rather than by path, so two entries
/// for the same file were adjacent only by coincidence. A file coupled to
/// several already-seen files therefore survived as multiple entries and could
/// consume several of the `limit` slots, crowding out distinct suggestions —
/// exactly what the truncation is meant to allocate fairly.
///
/// Ties are broken by path so the output is deterministic. Without that the
/// order would depend on `HashMap` iteration, which varies run to run and would
/// make the injected context unstable for identical inputs.
fn dedupe_complementary(files: Vec<(String, f32)>, limit: usize) -> Vec<(String, f32)> {
    let mut best_by_path: HashMap<String, f32> = HashMap::new();
    for (path, score) in files {
        best_by_path
            .entry(path)
            .and_modify(|best| {
                if score > *best {
                    *best = score;
                }
            })
            .or_insert(score);
    }

    let mut deduped: Vec<(String, f32)> = best_by_path.into_iter().collect();
    deduped.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    deduped.truncate(limit);
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- bobbin-au4: non-adjacent duplicates ate the truncation slots ---

    fn comp(pairs: &[(&str, f32)]) -> Vec<(String, f32)> {
        pairs.iter().map(|(p, s)| (p.to_string(), *s)).collect()
    }

    #[test]
    fn test_dedupe_complementary_collapses_non_adjacent_duplicates() {
        // Sorted by score these interleave, so the old dedup_by saw no two
        // equal entries adjacent and removed nothing.
        let input = comp(&[("a.rs", 0.9), ("b.rs", 0.8), ("a.rs", 0.7), ("c.rs", 0.6)]);
        let out = dedupe_complementary(input, 5);

        assert_eq!(out.len(), 3, "a.rs must appear once: {out:?}");
        assert_eq!(out[0], ("a.rs".to_string(), 0.9));
    }

    #[test]
    fn test_dedupe_complementary_keeps_the_max_score_for_a_path() {
        let out = dedupe_complementary(comp(&[("a.rs", 0.2), ("a.rs", 0.9), ("a.rs", 0.5)]), 5);
        assert_eq!(out, vec![("a.rs".to_string(), 0.9)]);
    }

    #[test]
    fn test_dedupe_complementary_does_not_let_duplicates_consume_slots() {
        // The regression, stated as the effect that matters: five slots must
        // hold five DISTINCT files. Pre-fix this returned a.rs four times and
        // one other, so four real suggestions were lost.
        let input = comp(&[
            ("a.rs", 0.99),
            ("a.rs", 0.98),
            ("a.rs", 0.97),
            ("a.rs", 0.96),
            ("b.rs", 0.50),
            ("c.rs", 0.40),
            ("d.rs", 0.30),
            ("e.rs", 0.20),
        ]);
        let out = dedupe_complementary(input, 5);

        assert_eq!(out.len(), 5);
        let paths: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]);
    }

    #[test]
    fn test_dedupe_complementary_is_deterministic_on_ties() {
        // HashMap iteration order varies run to run; equal scores must not make
        // the injected context differ between identical invocations.
        let input = comp(&[("z.rs", 0.5), ("a.rs", 0.5), ("m.rs", 0.5)]);
        let first = dedupe_complementary(input.clone(), 5);
        for _ in 0..20 {
            assert_eq!(first, dedupe_complementary(input.clone(), 5));
        }
        assert_eq!(first[0].0, "a.rs", "ties break by path");
    }

    #[test]
    fn test_dedupe_complementary_handles_empty_and_limit_zero() {
        assert!(dedupe_complementary(vec![], 5).is_empty());
        assert!(dedupe_complementary(comp(&[("a.rs", 0.9)]), 0).is_empty());
    }
    use crate::search::context::*;
    use crate::types::{classify_file, ChunkType, MatchType};

    #[test]
    fn test_command_invokes_bobbin_hook_bare() {
        assert!(command_invokes_bobbin_hook("bobbin hook inject-context"));
    }

    #[test]
    fn test_command_invokes_bobbin_hook_wrapped() {
        // Project-level / quarterdeck form: env prefix + absolute path + suffix (GH#10)
        assert!(command_invokes_bobbin_hook(
            "BOBBIN_SERVER=http://search.example /home/user/.local/bin/bobbin hook inject-context || true"
        ));
        assert!(command_invokes_bobbin_hook(
            "/usr/bin/bobbin hook session-context"
        ));
        assert!(command_invokes_bobbin_hook(
            "FOO=bar bobbin hook post-tool-use"
        ));
    }

    #[test]
    fn test_command_invokes_bobbin_hook_negatives() {
        assert!(!command_invokes_bobbin_hook("bobbin search foo"));
        assert!(!command_invokes_bobbin_hook("echo bobbin hooked")); // not "hook "
        assert!(!command_invokes_bobbin_hook("/opt/mybobbin hook x")); // different binary
        assert!(!command_invokes_bobbin_hook("notbobbin hook x"));
    }

    #[test]
    fn test_has_bobbin_hooks_detects_wrapped_project_command() {
        // Regression for GH#10: project-level hooks with wrapped commands
        // were reported as "not installed".
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": "BOBBIN_SERVER=http://search.example /home/user/.local/bin/bobbin hook inject-context || true"
                    }]
                }]
            }
        });
        assert!(has_bobbin_hooks(&settings));
    }

    #[test]
    fn test_has_bobbin_hooks_false_for_unrelated() {
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{ "type": "command", "command": "some-other-tool run" }]
                }]
            }
        });
        assert!(!has_bobbin_hooks(&settings));
    }

    #[test]
    fn test_hook_config_output_serialization() {
        let output = HookStatusOutput {
            hooks_installed: false,
            git_hook_installed: false,
            config: HookConfigOutput {
                threshold: 0.5,
                budget: 150,
                content_mode: "preview".to_string(),
                min_prompt_length: 10,
                gate_threshold: 0.75,
                dedup_enabled: true,
            },
            injection_count: 42,
            last_injection_time: Some("2026-02-08T10:30:00Z".to_string()),
            last_session_id: Some("a1b2c3d4e5f6a7b8".to_string()),
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"threshold\":0.5"));
        assert!(json.contains("\"budget\":150"));
        assert!(json.contains("\"content_mode\":\"preview\""));
        assert!(json.contains("\"gate_threshold\":0.75"));
        assert!(json.contains("\"dedup_enabled\":true"));
        assert!(json.contains("\"injection_count\":42"));
        assert!(json.contains("\"last_injection_time\":\"2026-02-08T10:30:00Z\""));
        assert!(json.contains("\"last_session_id\":\"a1b2c3d4e5f6a7b8\""));
    }

    #[test]
    fn test_hook_status_output_no_state() {
        let output = HookStatusOutput {
            hooks_installed: true,
            git_hook_installed: false,
            config: HookConfigOutput {
                threshold: 0.5,
                budget: 150,
                content_mode: "preview".to_string(),
                min_prompt_length: 10,
                gate_threshold: 0.75,
                dedup_enabled: true,
            },
            injection_count: 0,
            last_injection_time: None,
            last_session_id: None,
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"injection_count\":0"));
        assert!(json.contains("\"last_injection_time\":null"));
        assert!(json.contains("\"last_session_id\":null"));
    }

    #[test]
    fn test_hook_input_deserialization() {
        let json = r#"{"session_id":"abc","prompt":"find auth code","cwd":"/home/user/project","permission_mode":"default","hook_event_name":"UserPromptSubmit"}"#;
        let input: HookInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.prompt, "find auth code");
        assert_eq!(input.cwd, "/home/user/project");
    }

    #[test]
    fn test_hook_input_missing_fields() {
        // Extra fields are ignored, missing optional fields get defaults
        let json = r#"{"prompt":"hello"}"#;
        let input: HookInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.prompt, "hello");
        assert!(input.cwd.is_empty());
    }

    #[test]
    fn test_hook_input_empty_object() {
        let json = r#"{}"#;
        let input: HookInput = serde_json::from_str(json).unwrap();
        assert!(input.prompt.is_empty());
        assert!(input.cwd.is_empty());
    }

    #[test]
    fn test_find_bobbin_root_not_found() {
        let tmp = std::env::temp_dir().join("bobbin_test_no_root");
        std::fs::create_dir_all(&tmp).ok();
        assert!(find_bobbin_root(&tmp).is_none());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_find_bobbin_root_direct() {
        let tmp = tempfile::tempdir().unwrap();
        let bobbin_dir = tmp.path().join(".bobbin");
        std::fs::create_dir_all(&bobbin_dir).unwrap();
        std::fs::write(bobbin_dir.join("config.toml"), "").unwrap();

        let found = find_bobbin_root(tmp.path());
        assert_eq!(found, Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn test_find_bobbin_root_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let bobbin_dir = tmp.path().join(".bobbin");
        std::fs::create_dir_all(&bobbin_dir).unwrap();
        std::fs::write(bobbin_dir.join("config.toml"), "").unwrap();

        let child = tmp.path().join("src").join("lib");
        std::fs::create_dir_all(&child).unwrap();

        let found = find_bobbin_root(&child);
        assert_eq!(found, Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn test_format_context_empty_bundle() {
        let bundle = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 0,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 0,
                total_chunks: 0,
                direct_hits: 0,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.0,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };
        let result = format_context_for_injection(&bundle, 0.0, true, None, "standard");
        assert!(result.contains("0 relevant files"));
    }

    #[test]
    fn test_format_context_with_results() {
        let bundle = ContextBundle {
            capture: None,
            query: "auth handler".to_string(),
            files: vec![ContextFile {
                path: "src/auth.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/auth.rs"),
                score: 0.85,
                coupled_to: vec![],
                repo: None,
                chunks: vec![ContextChunk {
                    id: String::new(),
                    name: Some("authenticate".to_string()),
                    chunk_type: ChunkType::Function,
                    start_line: 10,
                    end_line: 25,
                    score: 0.85,
                    match_type: Some(MatchType::Hybrid),
                    content: Some("fn authenticate() {\n    // check token\n}".to_string()),
                }],
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 16,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 1,
                direct_hits: 1,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.0,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };
        let result = format_context_for_injection(&bundle, 0.5, true, None, "standard");
        assert!(result.contains("src/auth.rs:10-25"));
        assert!(result.contains("authenticate"));
        assert!(result.contains("fn authenticate()"));
        assert!(result.contains("score 0.85"));
    }

    #[test]
    fn test_format_context_with_injection_id() {
        let bundle = ContextBundle {
            capture: None,
            query: "auth handler".to_string(),
            files: vec![ContextFile {
                path: "src/auth.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/auth.rs"),
                score: 0.85,
                coupled_to: vec![],
                repo: None,
                chunks: vec![ContextChunk {
                    id: String::new(),
                    name: Some("authenticate".to_string()),
                    chunk_type: ChunkType::Function,
                    start_line: 10,
                    end_line: 25,
                    score: 0.85,
                    match_type: Some(MatchType::Hybrid),
                    content: Some("fn authenticate() {}".to_string()),
                }],
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 10,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 1,
                direct_hits: 1,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 1,
                doc_files: 0,
                top_semantic_score: 0.85,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };

        // With injection_id
        let result =
            format_context_for_injection(&bundle, 0.0, true, Some("inj-abc12345"), "standard");
        assert!(result.contains("[injection_id: inj-abc12345]"));
        assert!(result.contains("1 relevant files"));

        // Without injection_id (backward compat)
        let result = format_context_for_injection(&bundle, 0.0, true, None, "standard");
        assert!(!result.contains("injection_id"));
        assert!(result.contains("1 relevant files"));
    }

    #[test]
    fn test_generate_context_injection_id() {
        let id1 = generate_context_injection_id("hello world");
        let id2 = generate_context_injection_id("hello world");
        // Each call should produce a unique ID (timestamp differs)
        assert!(id1.starts_with("inj-"));
        assert_eq!(id1.len(), 12); // "inj-" + 8 hex chars
        assert!(id2.starts_with("inj-"));
        // IDs should differ (nanosecond timestamp)
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_format_context_threshold_filters() {
        let bundle = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![ContextFile {
                path: "src/low.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/low.rs"),
                score: 0.3,
                coupled_to: vec![],
                repo: None,
                chunks: vec![ContextChunk {
                    id: String::new(),
                    name: Some("low_score_fn".to_string()),
                    chunk_type: ChunkType::Function,
                    start_line: 1,
                    end_line: 5,
                    score: 0.3,
                    match_type: None,
                    content: Some("fn low() {}".to_string()),
                }],
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 5,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 1,
                direct_hits: 1,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.0,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };
        // With high threshold, chunk content should be filtered out
        let result = format_context_for_injection(&bundle, 0.5, true, None, "standard");
        assert!(!result.contains("low_score_fn"));
    }

    #[test]
    fn test_session_start_input_parsing() {
        let json = r#"{"source": "compact", "cwd": "/tmp/test", "session_id": "abc"}"#;
        let input: SessionStartInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.source, "compact");
        assert_eq!(input.cwd, "/tmp/test");
    }

    #[test]
    fn test_session_start_input_defaults() {
        let json = r#"{}"#;
        let input: SessionStartInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.source, "");
        assert_eq!(input.cwd, "");
    }

    #[test]
    fn test_hook_response_serialization() {
        let response = HookResponse {
            hook_specific_output: HookSpecificOutput {
                hook_event_name: "SessionStart".to_string(),
                additional_context: "test context".to_string(),
            },
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("hookSpecificOutput"));
        assert!(json.contains("hookEventName"));
        assert!(json.contains("additionalContext"));
        assert!(json.contains("SessionStart"));
        assert!(json.contains("test context"));
    }

    #[test]
    fn test_format_session_context_modified_files() {
        let modified = vec!["src/main.rs".to_string()];
        let commits: Vec<String> = vec![];
        let symbols: Vec<FileSymbolInfo> = vec![];
        let coupled: Vec<(String, String, f32)> = vec![];

        let result = format_session_context(&modified, &commits, &symbols, &coupled, 150);
        assert!(result.contains("## Working Context"));
        assert!(result.contains("### Modified files"));
        assert!(result.contains("- src/main.rs"));
    }

    #[test]
    fn test_format_session_context_with_symbols() {
        let modified = vec!["src/auth.rs".to_string()];
        let commits: Vec<String> = vec![];
        let symbols = vec![FileSymbolInfo {
            path: "src/auth.rs".to_string(),
            symbols: vec![
                SymbolInfo {
                    name: "validate_token".to_string(),
                },
                SymbolInfo {
                    name: "refresh_session".to_string(),
                },
            ],
        }];
        let coupled: Vec<(String, String, f32)> = vec![];

        let result = format_session_context(&modified, &commits, &symbols, &coupled, 150);
        assert!(result.contains("src/auth.rs (2 symbols: validate_token, refresh_session)"));
    }

    #[test]
    fn test_format_session_context_with_commits() {
        let modified: Vec<String> = vec![];
        let commits = vec![
            "a1b2c3d fix: token refresh race condition".to_string(),
            "d4e5f6g feat: add logout endpoint".to_string(),
        ];
        let symbols: Vec<FileSymbolInfo> = vec![];
        let coupled: Vec<(String, String, f32)> = vec![];

        let result = format_session_context(&modified, &commits, &symbols, &coupled, 150);
        assert!(result.contains("### Recent commits"));
        assert!(result.contains("- a1b2c3d fix: token refresh race condition"));
    }

    #[test]
    fn test_format_session_context_with_coupling() {
        let modified = vec!["src/auth.rs".to_string()];
        let commits: Vec<String> = vec![];
        let symbols: Vec<FileSymbolInfo> = vec![];
        let coupled = vec![(
            "tests/auth_test.rs".to_string(),
            "src/auth.rs".to_string(),
            0.91,
        )];

        let result = format_session_context(&modified, &commits, &symbols, &coupled, 150);
        assert!(result.contains("### Related files (via coupling)"));
        assert!(result.contains("tests/auth_test.rs (coupled with src/auth.rs, score: 0.91)"));
    }

    #[test]
    fn test_format_session_context_budget_enforcement() {
        let modified: Vec<String> = (0..100).map(|i| format!("src/file_{}.rs", i)).collect();
        let commits: Vec<String> = vec![];
        let symbols: Vec<FileSymbolInfo> = vec![];
        let coupled: Vec<(String, String, f32)> = vec![];

        let result = format_session_context(&modified, &commits, &symbols, &coupled, 10);
        let line_count = result.lines().count();
        // Budget of 10 — truncation message counts within budget
        assert!(line_count <= 10, "Expected <= 10 lines, got {}", line_count);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_format_session_context_many_symbols_truncated() {
        let modified = vec!["src/big.rs".to_string()];
        let commits: Vec<String> = vec![];
        let symbols = vec![FileSymbolInfo {
            path: "src/big.rs".to_string(),
            symbols: (0..8)
                .map(|i| SymbolInfo {
                    name: format!("fn_{}", i),
                })
                .collect(),
        }];
        let coupled: Vec<(String, String, f32)> = vec![];

        let result = format_session_context(&modified, &commits, &symbols, &coupled, 150);
        // Should show 5 symbols + "..." indicator
        assert!(result.contains("8 symbols: fn_0, fn_1, fn_2, fn_3, fn_4, ..."));
    }

    #[test]
    fn test_format_session_context_recently_changed_separate() {
        // Modified files and recently changed files should appear in different sections
        let modified = vec!["src/modified.rs".to_string()];
        let commits: Vec<String> = vec![];
        let symbols = vec![
            FileSymbolInfo {
                path: "src/modified.rs".to_string(),
                symbols: vec![SymbolInfo {
                    name: "mod_fn".to_string(),
                }],
            },
            FileSymbolInfo {
                path: "src/recent.rs".to_string(),
                symbols: vec![SymbolInfo {
                    name: "recent_fn".to_string(),
                }],
            },
        ];
        let coupled: Vec<(String, String, f32)> = vec![];

        let result = format_session_context(&modified, &commits, &symbols, &coupled, 150);
        assert!(result.contains("### Modified files"));
        assert!(result.contains("### Recently changed files"));
        assert!(result.contains("- src/recent.rs (1 symbols: recent_fn)"));
    }

    #[test]
    fn test_format_session_context_empty_produces_header_only() {
        let modified: Vec<String> = vec![];
        let commits: Vec<String> = vec![];
        let symbols: Vec<FileSymbolInfo> = vec![];
        let coupled: Vec<(String, String, f32)> = vec![];

        let result = format_session_context(&modified, &commits, &symbols, &coupled, 150);
        assert!(result.contains("## Working Context"));
        // Header line + trailing newline from blank line join
        assert!(result.lines().count() <= 2);
    }

    // --- Budget enforcement tests for inject-context formatter ---

    #[test]
    fn test_format_context_for_injection_respects_budget() {
        // Build a bundle with many chunks that would exceed a small budget
        let bundle = ContextBundle {
            capture: None,
            query: "auth".to_string(),
            files: vec![ContextFile {
                path: "src/a.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/a.rs"),
                score: 0.9,
                coupled_to: vec![],
                repo: None,
                chunks: vec![
                    ContextChunk {
                        id: String::new(),
                        name: Some("fn_a".to_string()),
                        chunk_type: ChunkType::Function,
                        start_line: 1,
                        end_line: 10,
                        score: 0.9,
                        match_type: Some(MatchType::Hybrid),
                        content: Some(
                            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10"
                                .to_string(),
                        ),
                    },
                    ContextChunk {
                        id: String::new(),
                        name: Some("fn_b".to_string()),
                        chunk_type: ChunkType::Function,
                        start_line: 20,
                        end_line: 30,
                        score: 0.8,
                        match_type: Some(MatchType::Hybrid),
                        content: Some("b1\nb2\nb3\nb4\nb5\nb6\nb7\nb8\nb9\nb10\nb11".to_string()),
                    },
                ],
            }],
            budget: BudgetInfo {
                max_lines: 15,
                used_lines: 21,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 2,
                direct_hits: 2,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.0,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };
        let result = format_context_for_injection(&bundle, 0.0, true, None, "standard");
        let line_count = result.lines().count();
        // Must not exceed max_lines budget
        assert!(
            line_count <= 15,
            "Expected <= 15 lines, got {}:\n{}",
            line_count,
            result
        );
        // Should include at least the first chunk
        assert!(result.contains("fn_a"));
    }

    #[test]
    fn test_format_context_for_injection_score_format() {
        let bundle = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![ContextFile {
                path: "src/x.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/x.rs"),
                score: 0.85,
                coupled_to: vec![],
                repo: None,
                chunks: vec![ContextChunk {
                    id: String::new(),
                    name: Some("fn_x".to_string()),
                    chunk_type: ChunkType::Function,
                    start_line: 1,
                    end_line: 3,
                    score: 0.856,
                    match_type: None,
                    content: Some("fn x() {}".to_string()),
                }],
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 3,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 1,
                direct_hits: 1,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.0,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };
        let result = format_context_for_injection(&bundle, 0.0, true, None, "standard");
        // Score should be 2 decimal places
        assert!(
            result.contains("score 0.86"),
            "Expected 2-decimal score in: {}",
            result
        );
    }

    #[test]
    fn test_format_context_show_docs_false_excludes_doc_files() {
        let bundle = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![
                ContextFile {
                    path: "src/main.rs".to_string(),
                    language: "rust".to_string(),
                    relevance: FileRelevance::Direct,
                    category: classify_file("src/main.rs"),
                    score: 0.9,
                    coupled_to: vec![],
                    repo: None,
                    chunks: vec![ContextChunk {
                        id: String::new(),
                        name: Some("main".to_string()),
                        chunk_type: ChunkType::Function,
                        start_line: 1,
                        end_line: 5,
                        score: 0.9,
                        match_type: None,
                        content: Some("fn main() {}".to_string()),
                    }],
                },
                ContextFile {
                    path: "README.md".to_string(),
                    language: "markdown".to_string(),
                    relevance: FileRelevance::Direct,
                    category: classify_file("README.md"),
                    score: 0.8,
                    coupled_to: vec![],
                    repo: None,
                    chunks: vec![ContextChunk {
                        id: String::new(),
                        name: None,
                        chunk_type: ChunkType::Module,
                        start_line: 1,
                        end_line: 10,
                        score: 0.8,
                        match_type: None,
                        content: Some("# My Project".to_string()),
                    }],
                },
            ],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 15,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 2,
                total_chunks: 2,
                direct_hits: 2,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 1,
                doc_files: 1,
                top_semantic_score: 0.0,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };

        // show_docs=true should include both
        let with_docs = format_context_for_injection(&bundle, 0.0, true, None, "standard");
        assert!(
            with_docs.contains("Source Files"),
            "Should have source section"
        );
        assert!(
            with_docs.contains("Documentation"),
            "Should have doc section"
        );
        assert!(with_docs.contains("README.md"));

        // show_docs=false should exclude documentation
        let without_docs = format_context_for_injection(&bundle, 0.0, false, None, "standard");
        assert!(
            without_docs.contains("Source Files"),
            "Should have source section"
        );
        assert!(
            !without_docs.contains("Documentation"),
            "Should not have doc section"
        );
        assert!(
            !without_docs.contains("README.md"),
            "Doc file should be excluded"
        );
        assert!(
            without_docs.contains("src/main.rs"),
            "Source file should remain"
        );
    }

    #[test]
    fn test_format_context_budget_zero() {
        let bundle = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![ContextFile {
                path: "src/a.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/a.rs"),
                score: 0.9,
                coupled_to: vec![],
                repo: None,
                chunks: vec![ContextChunk {
                    id: String::new(),
                    name: Some("fn_a".to_string()),
                    chunk_type: ChunkType::Function,
                    start_line: 1,
                    end_line: 10,
                    score: 0.9,
                    match_type: None,
                    content: Some("fn a() {}".to_string()),
                }],
            }],
            budget: BudgetInfo {
                max_lines: 0,
                used_lines: 0,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 1,
                direct_hits: 1,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 1,
                doc_files: 0,
                top_semantic_score: 0.0,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };
        // Budget 0 — should not panic and should produce empty or minimal output
        let result = format_context_for_injection(&bundle, 0.0, true, None, "standard");
        assert!(
            result.lines().count() <= 1,
            "Budget 0 should produce at most the header"
        );
    }

    #[test]
    fn test_format_context_no_content() {
        // Test formatting when content is None (ContentMode::None)
        let bundle = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![ContextFile {
                path: "src/a.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/a.rs"),
                score: 0.9,
                coupled_to: vec![],
                repo: None,
                chunks: vec![ContextChunk {
                    id: String::new(),
                    name: Some("fn_a".to_string()),
                    chunk_type: ChunkType::Function,
                    start_line: 1,
                    end_line: 10,
                    score: 0.9,
                    match_type: None,
                    content: None,
                }],
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 10,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 1,
                direct_hits: 1,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 1,
                doc_files: 0,
                top_semantic_score: 0.0,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };
        let result = format_context_for_injection(&bundle, 0.0, true, None, "standard");
        // Should still have the chunk header with file:lines
        assert!(result.contains("src/a.rs:1-10"));
        assert!(result.contains("fn_a"));
    }

    include!("hook_format_fixture.rs");

    #[test]
    fn test_format_mode_standard() {
        let bundle = make_format_test_bundle();
        let result =
            format_context_for_injection(&bundle, 0.0, true, Some("inj-test1"), "standard");
        assert!(result.contains("Bobbin found 1 relevant files"));
        assert!(result.contains("[injection_id: inj-test1]"));
        assert!(result.contains("=== Source Files ==="));
        assert!(result.contains("--- src/auth.rs:10-25"));
        assert!(result.contains("score 0.85"));
        assert!(result.contains("fn authenticate()"));
    }

    #[test]
    fn test_format_mode_minimal() {
        let bundle = make_format_test_bundle();
        let result = format_context_for_injection(&bundle, 0.0, true, Some("inj-test2"), "minimal");
        // Minimal: no section headers, no scores, no types
        assert!(result.contains("# Bobbin context"));
        assert!(result.contains("[injection_id: inj-test2]"));
        assert!(!result.contains("=== Source Files ==="));
        assert!(!result.contains("score 0.85"));
        assert!(result.contains("# src/auth.rs (lines 10-25)"));
        assert!(result.contains("fn authenticate()"));
    }

    #[test]
    fn test_format_mode_verbose() {
        let bundle = make_format_test_bundle();
        let result = format_context_for_injection(&bundle, 0.0, true, Some("inj-test3"), "verbose");
        assert!(result.contains("Bobbin found 1 relevant files"));
        assert!(result.contains("=== Source Files ==="));
        assert!(result.contains("--- src/auth.rs:10-25"));
        assert!(result.contains("score 0.85"));
        // Verbose adds explicit type/name line
        assert!(result.contains("// function authenticate"));
        assert!(result.contains("fn authenticate()"));
    }

    #[test]
    fn test_format_mode_xml() {
        let bundle = make_format_test_bundle();
        let result = format_context_for_injection(&bundle, 0.0, true, Some("inj-test4"), "xml");
        assert!(result.contains("<bobbin-context"));
        assert!(result.contains("injection_id=\"inj-test4\""));
        assert!(result.contains("</bobbin-context>"));
        assert!(result.contains("<file path=\"src/auth.rs\""));
        assert!(result.contains("lines=\"10-25\""));
        assert!(result.contains("score=\"0.85\""));
        assert!(result.contains("</file>"));
        assert!(result.contains("fn authenticate()"));
        // XML mode should NOT have section headers
        assert!(!result.contains("=== Source Files ==="));
    }

    #[test]
    fn test_format_search_chunk_all_modes() {
        let content = "fn main() {}\n";
        let standard = format_search_chunk(
            "src/main.rs",
            1,
            5,
            " main",
            "function",
            0.9,
            content,
            "",
            "standard",
        );
        assert!(standard.contains("--- src/main.rs:1-5 main (function, score 0.90) ---"));

        let minimal = format_search_chunk(
            "src/main.rs",
            1,
            5,
            " main",
            "function",
            0.9,
            content,
            "",
            "minimal",
        );
        assert!(minimal.contains("# src/main.rs (lines 1-5)"));
        assert!(!minimal.contains("score"));

        let xml = format_search_chunk(
            "src/main.rs",
            1,
            5,
            " main",
            "function",
            0.9,
            content,
            "",
            "xml",
        );
        assert!(xml.contains("<file path=\"src/main.rs\""));
        assert!(xml.contains("name=\"main\""));
        assert!(xml.contains("</file>"));
    }

    #[test]
    fn test_format_session_context_very_small_budget() {
        let modified = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let commits: Vec<String> = vec![];
        let symbols: Vec<FileSymbolInfo> = vec![];
        let coupled: Vec<(String, String, f32)> = vec![];

        // Budget of 3 — header + blank + 1 content line at most
        let result = format_session_context(&modified, &commits, &symbols, &coupled, 3);
        let line_count = result.lines().count();
        assert!(
            line_count <= 3,
            "Expected <= 3 lines, got {}:\n{}",
            line_count,
            result
        );
    }

    #[test]
    fn test_format_session_context_budget_zero() {
        let modified = vec!["src/a.rs".to_string()];
        let commits: Vec<String> = vec![];
        let symbols: Vec<FileSymbolInfo> = vec![];
        let coupled: Vec<(String, String, f32)> = vec![];

        // Budget of 0 — should still not panic
        let result = format_session_context(&modified, &commits, &symbols, &coupled, 0);
        // Should produce at most the truncation message
        assert!(result.lines().count() <= 1);
    }

    // --- Hook installer unit tests ---

    #[test]
    fn test_merge_hooks_into_empty_settings() {
        let mut settings = json!({});
        merge_hooks(&mut settings);

        assert!(settings.get("hooks").is_some());
        let hooks = &settings["hooks"];
        assert!(hooks.get("UserPromptSubmit").is_some());
        assert!(hooks.get("SessionStart").is_some());

        // Verify inject-context command
        let ups = hooks["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1);
        let cmd = ups[0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "bobbin hook inject-context || true");

        // Verify session-context command
        let ss = hooks["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 1);
        let cmd = ss[0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "bobbin hook session-context || true");
        assert_eq!(ss[0]["matcher"].as_str().unwrap(), "compact");
    }

    #[test]
    fn test_merge_hooks_preserves_existing_hooks() {
        let mut settings = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "other-tool inject",
                                "timeout": 5
                            }
                        ]
                    }
                ]
            },
            "other_key": "preserved"
        });

        merge_hooks(&mut settings);

        // other_key should still be there
        assert_eq!(settings["other_key"].as_str().unwrap(), "preserved");

        // UserPromptSubmit should have both the other tool AND bobbin
        let ups = settings["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 2);
        assert_eq!(
            ups[0]["hooks"][0]["command"].as_str().unwrap(),
            "other-tool inject"
        );
        assert_eq!(
            ups[1]["hooks"][0]["command"].as_str().unwrap(),
            "bobbin hook inject-context || true"
        );
    }

    #[test]
    fn test_merge_hooks_idempotent() {
        let mut settings = json!({});
        merge_hooks(&mut settings);
        merge_hooks(&mut settings); // Second time

        let ups = settings["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1, "Should not duplicate bobbin hooks");

        let ss = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 1, "Should not duplicate bobbin hooks");
    }

    #[test]
    fn test_is_bobbin_hook_group_true() {
        let group = json!({
            "hooks": [
                {
                    "type": "command",
                    "command": "bobbin hook inject-context",
                    "timeout": 10
                }
            ]
        });
        assert!(is_bobbin_hook_group(&group));
    }

    #[test]
    fn test_is_bobbin_hook_group_with_fallback() {
        // Old-format hooks (without || true) should still be detected
        let group = json!({
            "hooks": [
                {
                    "type": "command",
                    "command": "bobbin hook inject-context || true",
                    "timeout": 10
                }
            ]
        });
        assert!(is_bobbin_hook_group(&group));
    }

    #[test]
    fn test_is_bobbin_hook_group_false() {
        let group = json!({
            "hooks": [
                {
                    "type": "command",
                    "command": "other-tool do-thing",
                    "timeout": 5
                }
            ]
        });
        assert!(!is_bobbin_hook_group(&group));
    }

    #[test]
    fn test_remove_bobbin_hooks_leaves_others() {
        let mut settings = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            { "type": "command", "command": "other-tool inject" }
                        ]
                    },
                    {
                        "hooks": [
                            { "type": "command", "command": "bobbin hook inject-context" }
                        ]
                    }
                ],
                "SessionStart": [
                    {
                        "matcher": "compact",
                        "hooks": [
                            { "type": "command", "command": "bobbin hook session-context" }
                        ]
                    }
                ]
            }
        });

        let removed = remove_bobbin_hooks(&mut settings);
        assert!(removed);

        // other-tool should remain
        let ups = settings["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1);
        assert_eq!(
            ups[0]["hooks"][0]["command"].as_str().unwrap(),
            "other-tool inject"
        );

        // SessionStart was only bobbin, so it should be removed entirely
        assert!(settings["hooks"].get("SessionStart").is_none());
    }

    #[test]
    fn test_remove_bobbin_hooks_cleans_empty_hooks_object() {
        let mut settings = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            { "type": "command", "command": "bobbin hook inject-context" }
                        ]
                    }
                ]
            },
            "other": true
        });

        let removed = remove_bobbin_hooks(&mut settings);
        assert!(removed);

        // hooks object should be fully removed
        assert!(settings.get("hooks").is_none());
        // other keys preserved
        assert_eq!(settings["other"].as_bool().unwrap(), true);
    }

    #[test]
    fn test_remove_bobbin_hooks_none_present() {
        let mut settings = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            { "type": "command", "command": "other-tool inject" }
                        ]
                    }
                ]
            }
        });

        let removed = remove_bobbin_hooks(&mut settings);
        assert!(!removed);

        // Nothing should change
        let ups = settings["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1);
    }

    #[test]
    fn test_has_bobbin_hooks_true() {
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            { "type": "command", "command": "bobbin hook inject-context" }
                        ]
                    }
                ]
            }
        });
        assert!(has_bobbin_hooks(&settings));
    }

    #[test]
    fn test_has_bobbin_hooks_false() {
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            { "type": "command", "command": "other-tool" }
                        ]
                    }
                ]
            }
        });
        assert!(!has_bobbin_hooks(&settings));
    }

    #[test]
    fn test_has_bobbin_hooks_empty() {
        assert!(!has_bobbin_hooks(&json!({})));
    }

    #[test]
    fn test_read_settings_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent.json");
        let settings = read_settings(&path).unwrap();
        assert_eq!(settings, json!({}));
    }

    #[test]
    fn test_read_settings_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.json");
        std::fs::write(&path, "").unwrap();
        let settings = read_settings(&path).unwrap();
        assert_eq!(settings, json!({}));
    }

    #[test]
    fn test_read_settings_valid_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("valid.json");
        std::fs::write(&path, r#"{"key": "value"}"#).unwrap();
        let settings = read_settings(&path).unwrap();
        assert_eq!(settings["key"].as_str().unwrap(), "value");
    }

    #[test]
    fn test_write_settings_creates_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("deep").join("nested").join("settings.json");
        let settings = json!({"test": true});
        write_settings(&path, &settings).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["test"].as_bool().unwrap(), true);
    }

    #[test]
    fn test_merge_hooks_preserves_unrelated_events() {
        // Events that bobbin doesn't use should be completely untouched
        let mut settings = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            { "type": "command", "command": "gt tap guard pr-workflow" }
                        ],
                        "matcher": "Bash(gh pr create*)"
                    }
                ],
                "Stop": [
                    {
                        "hooks": [
                            { "type": "command", "command": "gt costs record" }
                        ],
                        "matcher": ""
                    }
                ],
                "PostToolUseFailure": [
                    {
                        "hooks": [
                            { "type": "command", "command": "dp record --source claude-code" }
                        ],
                        "matcher": ".*"
                    }
                ]
            }
        });

        merge_hooks(&mut settings);

        // Unrelated events preserved
        let hooks = &settings["hooks"];
        assert_eq!(hooks["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            hooks["PreToolUse"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap(),
            "gt tap guard pr-workflow"
        );
        assert_eq!(hooks["Stop"].as_array().unwrap().len(), 1);

        // PostToolUseFailure: original dp hook + new bobbin hook
        assert_eq!(hooks["PostToolUseFailure"].as_array().unwrap().len(), 2);
        assert_eq!(
            hooks["PostToolUseFailure"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap(),
            "dp record --source claude-code"
        );

        // Bobbin events added
        assert!(hooks["UserPromptSubmit"].is_array());
        assert!(hooks["SessionStart"].is_array());
        assert!(hooks["PostToolUse"].is_array());
    }

    #[test]
    fn test_merge_hooks_preserves_non_hook_settings() {
        // Top-level keys like statusLine must survive
        let mut settings = json!({
            "statusLine": {
                "command": "bash ~/.claude/statusline-command.sh",
                "type": "command"
            },
            "permissions": {
                "allow": ["Bash(cargo *)"]
            }
        });

        merge_hooks(&mut settings);

        assert_eq!(
            settings["statusLine"]["command"].as_str().unwrap(),
            "bash ~/.claude/statusline-command.sh"
        );
        assert_eq!(
            settings["permissions"]["allow"][0].as_str().unwrap(),
            "Bash(cargo *)"
        );
    }

    #[test]
    fn test_merge_hooks_realistic_multi_tool_settings() {
        // Mirrors a real ~/.claude/settings.json with Gas Town + dp hooks
        let mut settings = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            { "type": "command", "command": "gt mail check --inject" }
                        ],
                        "matcher": ""
                    }
                ],
                "SessionStart": [
                    {
                        "hooks": [
                            { "type": "command", "command": "gt prime --hook" }
                        ],
                        "matcher": ""
                    }
                ],
                "PreCompact": [
                    {
                        "hooks": [
                            { "type": "command", "command": "gt prime --hook" }
                        ],
                        "matcher": ""
                    }
                ],
                "Stop": [
                    {
                        "hooks": [
                            { "type": "command", "command": "gt costs record" }
                        ],
                        "matcher": ""
                    }
                ]
            },
            "statusLine": {
                "command": "bash ~/.claude/statusline-command.sh",
                "type": "command"
            }
        });

        merge_hooks(&mut settings);

        let hooks = &settings["hooks"];

        // Gas Town hooks in shared events preserved alongside bobbin
        let ups = hooks["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 2);
        assert_eq!(
            ups[0]["hooks"][0]["command"].as_str().unwrap(),
            "gt mail check --inject"
        );
        assert_eq!(
            ups[1]["hooks"][0]["command"].as_str().unwrap(),
            "bobbin hook inject-context || true"
        );

        let ss = hooks["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 2);
        assert_eq!(
            ss[0]["hooks"][0]["command"].as_str().unwrap(),
            "gt prime --hook"
        );
        assert_eq!(
            ss[1]["hooks"][0]["command"].as_str().unwrap(),
            "bobbin hook session-context || true"
        );

        // Events bobbin doesn't touch are untouched
        assert_eq!(hooks["PreCompact"].as_array().unwrap().len(), 1);
        assert_eq!(hooks["Stop"].as_array().unwrap().len(), 1);

        // Non-hook settings preserved
        assert!(settings["statusLine"].is_object());
    }

    #[test]
    fn test_merge_hooks_idempotent_with_other_tools() {
        // Merge twice with non-bobbin hooks — should not duplicate anything
        let mut settings = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            { "type": "command", "command": "gt mail check --inject" }
                        ]
                    }
                ]
            }
        });

        merge_hooks(&mut settings);
        merge_hooks(&mut settings);

        let ups = settings["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 2, "gt hook + 1 bobbin hook, no duplicates");

        let ss = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 1, "Only 1 bobbin SessionStart hook");
    }

    #[test]
    fn test_bobbin_hook_entries_structure() {
        let entries = bobbin_hook_entries();
        let hooks = entries.get("hooks").unwrap();

        // UserPromptSubmit
        let ups = hooks["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0]["hooks"][0]["type"].as_str().unwrap(), "command");
        assert_eq!(ups[0]["hooks"][0]["timeout"].as_i64().unwrap(), 10);

        // SessionStart
        let ss = hooks["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss[0]["matcher"].as_str().unwrap(), "compact");

        // PostToolUse
        let ptu = hooks["PostToolUse"].as_array().unwrap();
        assert_eq!(ptu.len(), 1);
        assert_eq!(
            ptu[0]["matcher"].as_str().unwrap(),
            "Write|Edit|Bash|Grep|Glob|Read"
        );
        assert_eq!(
            ptu[0]["hooks"][0]["command"].as_str().unwrap(),
            "bobbin hook post-tool-use || true"
        );
        assert_eq!(ptu[0]["hooks"][0]["timeout"].as_i64().unwrap(), 10);

        // PostToolUseFailure
        let ptuf = hooks["PostToolUseFailure"].as_array().unwrap();
        assert_eq!(ptuf.len(), 1);
        assert_eq!(
            ptuf[0]["hooks"][0]["command"].as_str().unwrap(),
            "bobbin hook post-tool-use-failure || true"
        );
        assert_eq!(ptuf[0]["hooks"][0]["timeout"].as_i64().unwrap(), 10);
    }

    #[test]
    fn test_post_tool_use_input_deserialization() {
        let json = r#"{"session_id":"abc","tool_name":"Write","tool_input":{"file_path":"/tmp/test.rs","content":"fn main() {}"},"cwd":"/home/user/project","hook_event_name":"PostToolUse"}"#;
        let input: PostToolUseInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.tool_name, "Write");
        assert_eq!(input.session_id, "abc");
        assert_eq!(input.cwd, "/home/user/project");
        assert_eq!(
            input.tool_input["file_path"].as_str().unwrap(),
            "/tmp/test.rs"
        );
    }

    #[test]
    fn test_post_tool_use_failure_input_deserialization() {
        let json = r#"{"session_id":"abc","tool_name":"Bash","tool_input":{"command":"cargo test"},"error":"Command exited with non-zero status code 1","cwd":"/home/user/project","hook_event_name":"PostToolUseFailure"}"#;
        let input: PostToolUseFailureInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.tool_name, "Bash");
        assert_eq!(input.error, "Command exited with non-zero status code 1");
        assert_eq!(input.cwd, "/home/user/project");
        assert_eq!(input.tool_input["command"].as_str().unwrap(), "cargo test");
    }

    #[test]
    fn test_post_tool_use_input_defaults() {
        // Minimal input - all fields should default gracefully
        let json = r#"{}"#;
        let input: PostToolUseInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.tool_name, "");
        assert_eq!(input.cwd, "");
        assert_eq!(input.session_id, "");
        assert!(input.tool_input.is_null());
    }

    #[test]
    fn test_post_tool_use_failure_input_defaults() {
        let json = r#"{}"#;
        let input: PostToolUseFailureInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.tool_name, "");
        assert_eq!(input.error, "");
        assert_eq!(input.cwd, "");
    }

    #[test]
    fn test_git_hook_section_has_markers() {
        assert!(GIT_HOOK_SECTION.contains(GIT_HOOK_START_MARKER));
        assert!(GIT_HOOK_SECTION.contains(GIT_HOOK_END_MARKER));
        assert!(GIT_HOOK_SECTION.contains("bobbin index --quiet"));
        // bo-5em9: section now also auto-links the commit to its bead.
        assert!(GIT_HOOK_SECTION.contains(GIT_HOOK_AUTOLINK_MARKER));
        assert!(GIT_HOOK_SECTION.contains("bobbin bead auto-link --commit HEAD --quiet"));
    }

    // --- Session dedup tests ---

    #[test]
    fn test_hook_state_serde_roundtrip() {
        let mut chunk_freqs = HashMap::new();
        chunk_freqs.insert(
            "src/foo.rs:10:50".to_string(),
            ChunkFrequency {
                count: 12,
                file: "src/foo.rs".to_string(),
                name: Some("InjectContextArgs".to_string()),
            },
        );
        let mut file_freqs = HashMap::new();
        file_freqs.insert("src/foo.rs".to_string(), 15);

        let state = HookState {
            last_session_id: "a1b2c3d4e5f6a7b8".to_string(),
            last_injected_chunks: vec!["src/foo.rs:10:50".to_string()],
            last_injection_time: "2026-02-08T10:30:00Z".to_string(),
            injection_count: 47,
            chunk_frequencies: chunk_freqs,
            file_frequencies: file_freqs,
            hot_topics_generated_at: 40,
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: HookState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.last_session_id, "a1b2c3d4e5f6a7b8");
        assert_eq!(parsed.injection_count, 47);
        assert_eq!(parsed.chunk_frequencies["src/foo.rs:10:50"].count, 12);
        assert_eq!(parsed.file_frequencies["src/foo.rs"], 15);
        assert_eq!(parsed.hot_topics_generated_at, 40);
    }

    #[test]
    fn test_hook_state_default() {
        let state = HookState::default();
        assert!(state.last_session_id.is_empty());
        assert!(state.last_injected_chunks.is_empty());
        assert_eq!(state.injection_count, 0);
        assert!(state.chunk_frequencies.is_empty());
        assert!(state.file_frequencies.is_empty());
    }

    #[test]
    fn test_hook_state_deserialize_corrupt_falls_back() {
        let corrupt = "{ not valid json at all }}}";
        let state: HookState = serde_json::from_str(corrupt).unwrap_or_default();
        assert!(state.last_session_id.is_empty());
        assert_eq!(state.injection_count, 0);
    }

    #[test]
    fn test_hook_state_deserialize_partial_fields() {
        // Only some fields present — rest should default
        let json = r#"{"last_session_id": "abc", "injection_count": 5}"#;
        let state: HookState = serde_json::from_str(json).unwrap();
        assert_eq!(state.last_session_id, "abc");
        assert_eq!(state.injection_count, 5);
        assert!(state.chunk_frequencies.is_empty());
        assert!(state.file_frequencies.is_empty());
    }

    #[test]
    fn test_load_save_hook_state() {
        let tmp = tempfile::tempdir().unwrap();
        let bobbin_dir = tmp.path().join(".bobbin");
        std::fs::create_dir_all(&bobbin_dir).unwrap();

        // Load from nonexistent file returns default
        let state = load_hook_state(tmp.path());
        assert!(state.last_session_id.is_empty());

        // Save and reload
        let mut state = HookState::default();
        state.last_session_id = "test123".to_string();
        state.injection_count = 3;
        save_hook_state(tmp.path(), &state);

        let loaded = load_hook_state(tmp.path());
        assert_eq!(loaded.last_session_id, "test123");
        assert_eq!(loaded.injection_count, 3);
    }

    #[test]
    fn test_find_bobbin_root_skips_stray_bobbin_dir() {
        // Bug B (bobbin #42): the resolver must require .bobbin/config.toml so
        // `hook status` and the inject path agree on the root. A stray .bobbin/
        // dir (e.g. one holding only tags.toml) between the CWD and the real
        // root must be walked past, not treated as the root — otherwise status
        // reads a hook_state.json that doesn't exist there and reports 0.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        // Real root: has .bobbin/config.toml
        std::fs::create_dir_all(root.join(".bobbin")).unwrap();
        std::fs::write(root.join(".bobbin").join("config.toml"), "").unwrap();

        // Stray sub-dir: has a .bobbin/ but no config.toml
        let stray = root.join("sub");
        std::fs::create_dir_all(stray.join(".bobbin")).unwrap();
        std::fs::write(stray.join(".bobbin").join("tags.toml"), "").unwrap();

        // Resolving from the stray dir must land on the real root.
        assert_eq!(find_bobbin_root(&stray), Some(root.clone()));
        assert_eq!(find_bobbin_root(&root), Some(root));
    }

    #[test]
    fn test_compute_session_id_deterministic() {
        let bundle = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![ContextFile {
                path: "src/a.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/a.rs"),
                score: 0.9,
                coupled_to: vec![],
                repo: None,
                chunks: vec![
                    ContextChunk {
                        id: String::new(),
                        name: Some("fn_a".to_string()),
                        chunk_type: ChunkType::Function,
                        start_line: 10,
                        end_line: 20,
                        score: 0.9,
                        match_type: None,
                        content: None,
                    },
                    ContextChunk {
                        id: String::new(),
                        name: Some("fn_b".to_string()),
                        chunk_type: ChunkType::Function,
                        start_line: 30,
                        end_line: 40,
                        score: 0.8,
                        match_type: None,
                        content: None,
                    },
                ],
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 10,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 2,
                direct_hits: 2,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.9,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };

        let id1 = compute_session_id(&bundle, 0.5);
        let id2 = compute_session_id(&bundle, 0.5);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 16); // 16 hex chars
    }

    #[test]
    fn test_compute_session_id_changes_with_different_chunks() {
        let make_bundle = |start: u32| ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![ContextFile {
                path: "src/a.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/a.rs"),
                score: 0.9,
                coupled_to: vec![],
                repo: None,
                chunks: vec![ContextChunk {
                    id: String::new(),
                    name: None,
                    chunk_type: ChunkType::Function,
                    start_line: start,
                    end_line: start + 10,
                    score: 0.9,
                    match_type: None,
                    content: None,
                }],
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 5,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 1,
                direct_hits: 1,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.9,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };

        let id1 = compute_session_id(&make_bundle(10), 0.0);
        let id2 = compute_session_id(&make_bundle(50), 0.0);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_compute_session_id_filters_by_threshold() {
        let bundle = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![ContextFile {
                path: "src/a.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/a.rs"),
                score: 0.9,
                coupled_to: vec![],
                repo: None,
                chunks: vec![
                    ContextChunk {
                        id: String::new(),
                        name: None,
                        chunk_type: ChunkType::Function,
                        start_line: 1,
                        end_line: 10,
                        score: 0.9,
                        match_type: None,
                        content: None,
                    },
                    ContextChunk {
                        id: String::new(),
                        name: None,
                        chunk_type: ChunkType::Function,
                        start_line: 20,
                        end_line: 30,
                        score: 0.3, // Below threshold
                        match_type: None,
                        content: None,
                    },
                ],
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 10,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 2,
                direct_hits: 2,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.9,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };

        // With threshold 0.5, low-score chunk is excluded
        let id_high = compute_session_id(&bundle, 0.5);
        // With threshold 0.0, both chunks included
        let id_low = compute_session_id(&bundle, 0.0);
        assert_ne!(id_high, id_low);
    }

    #[test]
    fn test_compute_session_id_empty_bundle() {
        let bundle = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 0,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 0,
                total_chunks: 0,
                direct_hits: 0,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.0,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };

        let id = compute_session_id(&bundle, 0.0);
        assert_eq!(id.len(), 16);
    }

    #[test]
    fn test_compute_session_id_top_10_limit() {
        // Create 15 chunks — only first 10 (sorted alphabetically by key) should matter
        let chunks: Vec<ContextChunk> = (0..15)
            .map(|i| ContextChunk {
                id: String::new(),
                name: None,
                chunk_type: ChunkType::Function,
                start_line: i * 10,
                end_line: i * 10 + 5,
                score: 0.9,
                match_type: None,
                content: None,
            })
            .collect();

        let bundle_all = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![ContextFile {
                path: "src/a.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/a.rs"),
                score: 0.9,
                coupled_to: vec![],
                repo: None,
                chunks: chunks.clone(),
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 50,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 15,
                direct_hits: 15,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.9,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };

        // Build a bundle with the top-10 keys (alphabetically sorted) from all 15
        let mut all_keys: Vec<String> = chunks
            .iter()
            .map(|c| format!("src/a.rs:{}:{}", c.start_line, c.end_line))
            .collect();
        all_keys.sort();
        let top_10_keys: HashSet<String> = all_keys.into_iter().take(10).collect();

        let top_10_chunks: Vec<ContextChunk> = chunks
            .iter()
            .filter(|c| {
                let key = format!("src/a.rs:{}:{}", c.start_line, c.end_line);
                top_10_keys.contains(&key)
            })
            .cloned()
            .collect();

        let bundle_ten = ContextBundle {
            capture: None,
            query: "test".to_string(),
            files: vec![ContextFile {
                path: "src/a.rs".to_string(),
                language: "rust".to_string(),
                relevance: FileRelevance::Direct,
                category: classify_file("src/a.rs"),
                score: 0.9,
                coupled_to: vec![],
                repo: None,
                chunks: top_10_chunks,
            }],
            budget: BudgetInfo {
                max_lines: 150,
                used_lines: 30,
                pinned_lines: 0,
            },
            summary: ContextSummary {
                structural_additions: 0,
                total_files: 1,
                total_chunks: 10,
                direct_hits: 10,
                coupled_additions: 0,
                bridged_additions: 0,
                source_files: 0,
                doc_files: 0,
                top_semantic_score: 0.9,
                pinned_chunks: 0,
                knowledge_additions: 0,
            },
        };

        let id_all = compute_session_id(&bundle_all, 0.0);
        let id_ten = compute_session_id(&bundle_ten, 0.0);
        assert_eq!(id_all, id_ten, "Top-10 truncation should produce same ID");
    }

    // --- Session ledger (reducing) tests ---

    #[test]
    fn test_session_ledger_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SessionLedger::load(tmp.path(), "test-session-1");
        assert_eq!(ledger.len(), 0);
        assert_eq!(ledger.turn, 0);
        assert!(!ledger.contains("src/foo.rs:10:20"));
    }

    #[test]
    fn test_session_ledger_record_and_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ledger = SessionLedger::load(tmp.path(), "test-session-2");

        let keys = vec![
            "src/foo.rs:10:20".to_string(),
            "src/bar.rs:5:15".to_string(),
        ];
        ledger.record(&keys, "inj-abc123");

        assert!(ledger.contains("src/foo.rs:10:20"));
        assert!(ledger.contains("src/bar.rs:5:15"));
        assert!(!ledger.contains("src/baz.rs:1:10"));
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger.turn, 1);
    }

    #[test]
    fn test_session_ledger_persistence() {
        let tmp = tempfile::tempdir().unwrap();

        // Record in one ledger instance
        {
            let mut ledger = SessionLedger::load(tmp.path(), "test-session-3");
            ledger.record(&["src/a.rs:1:10".to_string()], "inj-001");
            assert_eq!(ledger.turn, 1);
        }

        // Reload — entries should persist
        {
            let ledger = SessionLedger::load(tmp.path(), "test-session-3");
            assert!(ledger.contains("src/a.rs:1:10"));
            assert_eq!(ledger.len(), 1);
            assert_eq!(ledger.turn, 1);
        }
    }

    #[test]
    fn test_session_ledger_multi_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ledger = SessionLedger::load(tmp.path(), "test-session-4");

        // Turn 1
        ledger.record(
            &["src/a.rs:1:10".to_string(), "src/b.rs:1:10".to_string()],
            "inj-001",
        );
        assert_eq!(ledger.turn, 1);
        assert_eq!(ledger.len(), 2);

        // Turn 2 — new chunks plus overlap
        ledger.record(&["src/c.rs:1:10".to_string()], "inj-002");
        assert_eq!(ledger.turn, 2);
        assert_eq!(ledger.len(), 3);

        // All three chunks present
        assert!(ledger.contains("src/a.rs:1:10"));
        assert!(ledger.contains("src/b.rs:1:10"));
        assert!(ledger.contains("src/c.rs:1:10"));
    }

    #[test]
    fn test_session_ledger_clear() {
        let tmp = tempfile::tempdir().unwrap();

        // Record some data
        {
            let mut ledger = SessionLedger::load(tmp.path(), "test-session-5");
            ledger.record(&["src/a.rs:1:10".to_string()], "inj-001");
        }

        // Clear it
        SessionLedger::clear(tmp.path(), "test-session-5");

        // Reload — should be empty
        {
            let ledger = SessionLedger::load(tmp.path(), "test-session-5");
            assert_eq!(ledger.len(), 0);
            assert_eq!(ledger.turn, 0);
        }
    }

    #[test]
    fn test_session_ledger_empty_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ledger = SessionLedger::load(tmp.path(), "");
        assert!(ledger.path.is_none());

        // Should work in-memory without crashing
        ledger.record(&["src/a.rs:1:10".to_string()], "inj-001");
        assert!(ledger.contains("src/a.rs:1:10"));
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn test_chunk_key_format() {
        assert_eq!(chunk_key("src/foo.rs", 10, 20), "src/foo.rs:10:20");
        assert_eq!(
            chunk_key("/var/lib/repos/x/main.go", 1, 100),
            "/var/lib/repos/x/main.go:1:100"
        );
    }

    #[test]
    fn test_session_ledger_injected_files() {
        let mut ledger = SessionLedger {
            entries: HashSet::new(),
            turn: 0,
            path: None,
        };
        // Simulate chunk keys
        ledger.entries.insert("src/main.rs:1:10".to_string());
        ledger.entries.insert("src/main.rs:20:30".to_string());
        ledger.entries.insert("src/config.rs:5:15".to_string());
        ledger.entries.insert("tests/test_auth.rs:1:50".to_string());

        let files = ledger.injected_files();
        assert_eq!(files.len(), 3, "should have 3 unique files");
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"src/config.rs".to_string()));
        assert!(files.contains(&"tests/test_auth.rs".to_string()));
    }

    #[test]
    fn test_session_ledger_injected_files_empty() {
        let ledger = SessionLedger {
            entries: HashSet::new(),
            turn: 0,
            path: None,
        };
        assert!(ledger.injected_files().is_empty());
    }

    #[test]
    fn test_prompt_history_trajectory_empty() {
        let history = PromptHistory {
            entries: Vec::new(),
            path: None,
            max_entries: 5,
        };
        let query = history.build_trajectory_query("current prompt", 700);
        assert_eq!(query, "current prompt");
    }

    #[test]
    fn test_prompt_history_trajectory_with_history() {
        let history = PromptHistory {
            entries: vec![
                PromptEntry {
                    prompt: "how does auth work".to_string(),
                    timestamp: 100,
                },
                PromptEntry {
                    prompt: "show me the middleware".to_string(),
                    timestamp: 200,
                },
            ],
            path: None,
            max_entries: 5,
        };
        let query = history.build_trajectory_query("error handling in routes", 700);
        // Should contain history + current, separated by |
        assert!(query.contains("auth"));
        assert!(query.contains("middleware"));
        assert!(query.contains("error handling in routes"));
        assert!(query.contains(" | "));
    }

    #[test]
    fn test_prompt_history_trajectory_dedup_current() {
        let history = PromptHistory {
            entries: vec![PromptEntry {
                prompt: "same prompt".to_string(),
                timestamp: 100,
            }],
            path: None,
            max_entries: 5,
        };
        let query = history.build_trajectory_query("same prompt", 700);
        // Should NOT duplicate the current prompt
        assert_eq!(query, "same prompt");
    }

    #[test]
    fn test_prompt_history_trajectory_respects_max_chars() {
        let history = PromptHistory {
            entries: vec![PromptEntry {
                prompt: "a".repeat(500),
                timestamp: 100,
            }],
            path: None,
            max_entries: 5,
        };
        let query = history.build_trajectory_query("current", 100);
        // Total should not exceed max_chars
        assert!(query.len() <= 200, "query too long: {}", query.len());
    }

    #[test]
    fn test_prompt_history_record_and_load() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_root = tmp.path();
        let session_dir = repo_root.join(".bobbin").join("session").join("test-sess");
        std::fs::create_dir_all(&session_dir).unwrap();

        let mut history = PromptHistory::load(repo_root, "test-sess", 3);
        assert!(history.entries.is_empty());

        history.record("first prompt");
        history.record("second prompt");
        history.record("third prompt");

        // Reload from disk
        let reloaded = PromptHistory::load(repo_root, "test-sess", 3);
        assert_eq!(reloaded.entries.len(), 3);
        assert_eq!(reloaded.entries[0].prompt, "first prompt");
        assert_eq!(reloaded.entries[2].prompt, "third prompt");

        // Add one more — should maintain max_entries=3
        let mut history2 = PromptHistory::load(repo_root, "test-sess", 3);
        history2.record("fourth prompt");
        assert_eq!(history2.entries.len(), 3);
        assert_eq!(history2.entries[0].prompt, "second prompt"); // first was trimmed
        assert_eq!(history2.entries[2].prompt, "fourth prompt");
    }

    // --- The budget must say when it cut, not just stop -------------------

    fn chunk_of(lines: usize) -> crate::http::client::ContextChunkOutput {
        crate::http::client::ContextChunkOutput {
            name: Some("f".into()),
            chunk_type: "function".into(),
            start_line: 1,
            end_line: lines as u32,
            score: 0.9,
            match_type: None,
            content: Some("x\n".repeat(lines)),
        }
    }

    fn file_of(path: &str, lines: usize) -> crate::http::client::ContextFileOutput {
        crate::http::client::ContextFileOutput {
            path: path.into(),
            language: "rust".into(),
            relevance: "direct".into(),
            score: 0.9,
            coupled_to: vec![],
            chunks: vec![chunk_of(lines)],
            repo: None,
        }
    }

    /// RED. When the line budget runs out mid-render, the formatter must report
    /// how many files it never showed.
    ///
    /// The stop used to be a bare `return`: the output simply ended, and a
    /// reader could not tell "that was everything" from "we ran out of room".
    /// A budget that loses work silently is not a budget.
    #[test]
    fn a_budget_cut_reports_the_files_it_never_rendered() {
        let files = vec![
            file_of("a.rs", 40),
            file_of("b.rs", 40),
            file_of("c.rs", 40),
        ];
        let refs: Vec<&crate::http::client::ContextFileOutput> = files.iter().collect();
        let mut out = String::new();
        let mut line_count = 0usize;
        let cut = format_remote_file_chunks(&mut out, &refs, 45, &mut line_count, "xml");
        assert!(
            cut > 0,
            "the budget bit and the formatter must say so; rendered {line_count} lines"
        );
    }

    /// GREEN, and the control the test above needs. A render that fits reports
    /// zero — without this the same assertion would pass against a formatter
    /// that always claimed a cut.
    #[test]
    fn a_render_that_fits_reports_no_cut() {
        let files = vec![file_of("a.rs", 3)];
        let refs: Vec<&crate::http::client::ContextFileOutput> = files.iter().collect();
        let mut out = String::new();
        let mut line_count = 0usize;
        let cut = format_remote_file_chunks(&mut out, &refs, 10_000, &mut line_count, "xml");
        assert_eq!(cut, 0, "everything fit; nothing was held back");
        assert!(!out.is_empty(), "and it actually rendered something");
    }

    // --- hook expand: redeeming an injection_id --------------------------

    // `withheld_of` is the SHIPPED helper (super::), not a copy — a test
    // against a reimplementation would pass while the real one was wrong.

    #[test]
    fn expand_returns_exactly_the_files_the_render_left_out() {
        let files = vec![
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "src/c.rs".to_string(),
        ];
        let rendered = "<bobbin-context ...>\n// src/a.rs\ncode\n";
        let withheld = withheld_of(&files, rendered);
        assert_eq!(withheld.len(), 2);
        assert!(withheld.iter().any(|f| *f == "src/b.rs"));
        assert!(withheld.iter().any(|f| *f == "src/c.rs"));
    }

    /// The control. A render that showed everything must yield an EMPTY
    /// withheld set — without this the test above would pass against a helper
    /// that always claimed everything was held back, which is the same
    /// misinformation pointing the other way.
    #[test]
    fn expand_reports_nothing_withheld_when_everything_was_shown() {
        let files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let rendered = "// src/a.rs\n// src/b.rs\n";
        assert!(withheld_of(&files, rendered).is_empty());
    }

    fn na_fixture(faction: &str) -> (tempfile::TempDir, NaGroundingRef) {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            r#"{{"entities":["urn:fact:one"],"faction_id":"{faction}","graph":"urn:neuralamplifier:graph:knowledge","outcome":"used","query":"SELECT ?s WHERE {{ ?s ?p ?o }}","turn":42,"worldview_sha256":"sha256:world"}}"#
        );
        let digest = hex::encode(Sha256::digest(body.as_bytes()));
        std::fs::write(dir.path().join(format!("{digest}.json")), body).unwrap();
        (
            dir,
            NaGroundingRef {
                scope: Some("na".into()),
                grounding_id: Some(format!("sha256:{digest}")),
                faction_id: Some(faction.into()),
                worldview_sha256: Some("sha256:world".into()),
            },
        )
    }

    #[test]
    fn na_grounding_is_injected_only_after_content_and_scope_verify() {
        let (dir, reference) = na_fixture("gaia");
        let got = verified_na_grounding_at(Some(&reference), Some(dir.path()))
            .unwrap()
            .unwrap();
        assert_eq!(got.faction_id, "gaia");
        assert!(got.text.contains("urn:fact:one"));
    }

    #[test]
    fn cross_faction_grounding_fails_closed() {
        let (dir, mut reference) = na_fixture("gaia");
        reference.faction_id = Some("spartans".into());
        assert!(verified_na_grounding_at(Some(&reference), Some(dir.path())).is_err());
    }

    #[test]
    fn tampered_grounding_fails_closed() {
        let (dir, reference) = na_fixture("gaia");
        let digest = reference
            .grounding_id
            .as_deref()
            .unwrap()
            .trim_start_matches("sha256:");
        std::fs::write(dir.path().join(format!("{digest}.json")), b"tampered").unwrap();
        assert!(verified_na_grounding_at(Some(&reference), Some(dir.path())).is_err());
    }
}

#[cfg(test)]
#[path = "hook_session_id_tests.rs"]
mod hook_session_id_tests;

#[cfg(test)]
#[path = "hook_injection_turn_tests.rs"]
mod hook_injection_turn_tests;

#[cfg(test)]
#[path = "hook_hot_topics_tests.rs"]
mod hook_hot_topics_tests;

#[cfg(test)]
#[path = "hook_text_helpers_tests.rs"]
mod hook_text_helpers_tests;
