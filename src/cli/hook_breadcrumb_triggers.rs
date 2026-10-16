//! Keyword-triggered breadcrumb context shared by prompt and tool hooks.
//! Saved queries use the existing search path; pins are references, never raw
//! filesystem reads. The store itself and recall counters are read-only here.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::breadcrumb::{Breadcrumb, Breadcrumbs};
use crate::config::Config;
use crate::reactions::{DedupKey, DedupTracker, PendingSearch};

pub(super) struct TriggerContext {
    pub text: String,
    pub reserved: usize,
    keys: Vec<DedupKey>,
    dedup: DedupTracker,
}

impl TriggerContext {
    /// Only commit after the caller actually emitted this context.
    pub fn commit(&mut self) {
        for key in self.keys.drain(..) {
            self.dedup.record(key);
        }
    }
}

fn active(bc: &Breadcrumb) -> bool {
    bc.ttl_days == 0
        || Utc::now().signed_duration_since(bc.last_recalled.unwrap_or(bc.created_at))
            < chrono::Duration::days(i64::from(bc.ttl_days))
}

fn matching(root: &Path, input: &str) -> Vec<Breadcrumb> {
    // Hook input and a hand-edited store must not create unbounded matching work.
    if input.len() > 128 * 1024 {
        return Vec::new();
    }
    let result = (|| {
        let path = root.join(".bobbin/breadcrumbs.json");
        if !std::fs::symlink_metadata(&path).ok()?.file_type().is_file() {
            return None;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // The pre-open check alone races replacement with a FIFO or link.
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let file = options.open(path).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() > 1024 * 1024 {
            return None;
        }
        serde_json::from_slice::<Breadcrumbs>(&bytes).ok()
    })();
    let Some(store) = result else {
        return Vec::new();
    };
    let input = input.to_lowercase();
    store
        .into_values()
        .filter(|bc| {
            active(bc)
                && bc
                    .keywords
                    .iter()
                    .any(|word| !word.trim().is_empty() && input.contains(&word.to_lowercase()))
        })
        .collect()
}

// Local search returns repository-relative paths; ACL parsing requires the
// same explicit repository scope used by ordinary context rendering.
fn saved_query_path_allowed(
    path: &str,
    repo: Option<&str>,
    access: &crate::access::RepoFilter,
) -> bool {
    let scoped =
        if !path.starts_with("repos/") && !path.starts_with('/') && !path.starts_with("beads:") {
            // Without repository identity a relative hit cannot be ACL-scoped.
            let Some(repo) = repo else { return false };
            format!("repos/{repo}/{path}")
        } else {
            path.to_owned()
        };
    access.is_path_allowed(&scoped)
}

fn line(text: &str) -> String {
    text.chars()
        .take(240)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub(super) async fn prompt(
    root: &Path,
    input: &super::HookInput,
    budget: usize,
    config: &Config,
    server: Option<&str>,
) -> usize {
    let clean = super::strip_system_tags(&input.prompt);
    if crate::search::intent::classify_intent(&clean)
        == crate::search::intent::QueryIntent::Operational
    {
        return budget;
    }
    let mut triggered = prepare(root, &clean, &input.session_id, budget, config, server).await;
    if !triggered.text.is_empty() {
        print!("{}", triggered.text);
        triggered.commit();
    }
    budget.saturating_sub(triggered.reserved)
}

pub(super) async fn tool(
    root: &Path,
    input: &super::PostToolUseInput,
    budget: usize,
    config: &Config,
    server: Option<&str>,
) -> TriggerContext {
    let text = serde_json::to_string(&input.tool_input).unwrap_or_default();
    prepare(root, &text, &input.session_id, budget, config, server).await
}

pub(super) async fn prepare(
    root: &Path,
    input: &str,
    session: &str,
    budget: usize,
    config: &Config,
    server: Option<&str>,
) -> TriggerContext {
    // Hash the untrusted session identifier before using the existing disk
    // tracker. Both hook surfaces use the same namespace, including restarts.
    let session = if session.is_empty() {
        String::new()
    } else {
        format!(
            "breadcrumbs-{}",
            hex::encode(Sha256::digest(session.as_bytes()))
        )
    };
    let dedup = DedupTracker::load(root, &session);
    let mut result = TriggerContext {
        text: String::new(),
        reserved: 0,
        keys: Vec::new(),
        dedup,
    };
    let limit = budget / 10 * 3 + budget % 10 * 3 / 10;
    if limit < 4 {
        return result;
    }
    let role = crate::access::RepoFilter::resolve_role(None);
    let access = crate::access::RepoFilter::from_config(&config.access, &role);
    let root_name = super::detect_repo_name(root);
    if !access.is_allowed(root_name.as_deref().unwrap_or("")) {
        return result;
    }
    let canonical_root = root.canonicalize().ok();
    let mut remaining = limit;
    for bc in matching(root, input) {
        if remaining < 4 || result.keys.len() >= 3 {
            break;
        }
        let key = DedupKey {
            rule_name: format!("breadcrumb:{}", bc.name),
            args_hash: hex::encode(Sha256::digest(serde_json::to_vec(&bc).unwrap_or_default())),
        };
        if result.dedup.has_fired(&key) {
            continue;
        }
        let mut lines = vec![
            format!("=== Breadcrumb: {} ===", line(&bc.name)),
            line(&bc.description),
            format!("Saved query: {}", line(&bc.query)),
        ];
        let mut paths = HashSet::new();
        for pin in &bc.pinned_files {
            if lines.len() >= remaining {
                break;
            }
            // Reject traversal, absolute paths and symlink escapes. Pins stay
            // repository-local even in remote mode.
            let path = Path::new(pin);
            if path.is_absolute()
                || path.components().any(|c| {
                    !matches!(
                        c,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                })
            {
                continue;
            }
            let Some(base) = canonical_root.as_ref() else {
                continue;
            };
            let Ok(target) = root.join(path).canonicalize() else {
                continue;
            };
            if !target.is_file() || !target.starts_with(base) {
                continue;
            }
            let display = target
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .to_string();
            let scoped_path = format!("repos/{}/{}", root_name.as_deref().unwrap_or(""), display);
            if !access.is_path_allowed(&scoped_path) {
                continue;
            }
            if paths.insert(display.clone()) {
                lines.push(format!("- pinned: {}", line(&display)));
            }
        }
        // Search execution has an explicit small deadline. UNKNOWN is not
        // formatted as zero hits; saved metadata/pins remain useful offline.
        if lines.len() < remaining && !bc.query.trim().is_empty() {
            let query: String = bc.query.chars().take(500).collect();
            let search = async {
                if let Some(url) = server {
                    let client = crate::http::client::Client::new(url);
                    client
                        .context_with_weights(
                            &query,
                            Some(remaining - lines.len()),
                            Some(0),
                            Some(0),
                            Some(5),
                            None,
                            None,
                            Some(&role),
                            root_name.as_deref(),
                            None,
                            None,
                            None,
                        )
                        .await
                        .ok()
                        .map(|r| r.files.into_iter().map(|f| f.path).collect::<Vec<_>>())
                } else {
                    let search = PendingSearch {
                        rule_name: key.rule_name.clone(),
                        query: query.clone(),
                        group: None,
                        tags: Vec::new(),
                        max_lines: remaining - lines.len(),
                    };
                    super::run_reaction_search(
                        &search,
                        root,
                        &Config::lance_path(root),
                        &Config::db_path(root),
                        config,
                        root,
                        search.max_lines,
                    )
                    .await
                    .map(|hits| hits.into_iter().map(|f| f.path).collect::<Vec<_>>())
                }
            };
            if let Ok(Some(hits)) =
                tokio::time::timeout(std::time::Duration::from_secs(2), search).await
            {
                for path in hits {
                    if lines.len() >= remaining {
                        break;
                    }
                    if saved_query_path_allowed(&path, root_name.as_deref(), &access)
                        && paths.insert(path.clone())
                    {
                        lines.push(format!("- related: {}", line(&path)));
                    }
                }
            }
        }
        remaining -= lines.len();
        result.text.push_str(&lines.join("\n"));
        result.text.push('\n');
        result.keys.push(key);
    }
    if !result.text.is_empty() {
        result.reserved = limit;
    }
    result
}

#[cfg(test)]
#[path = "hook_breadcrumb_triggers_tests.rs"]
mod tests;
