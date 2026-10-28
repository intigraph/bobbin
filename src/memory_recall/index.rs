//! Keeping the memory mirror current: decoding `mem_index_since` pages,
//! applying them, and the single-flight refresh that runs off the hook path.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use super::*;

/// Rows per page; the query returns one more as the overflow sentinel.
pub const PAGE_LIMIT: usize = 50;
/// Most pages one refresh will read (50 * 40 = 2,000 changes).
const MAX_PAGES: usize = 40;
/// Timeout for one `/ask` page. The refresh runs detached, off the hook path.
const FETCH_TIMEOUT_SECS: u64 = 10;
/// A lock older than the longest possible refresh belongs to a dead process.
const LOCK_STALE_SECS: u64 = MAX_PAGES as u64 * FETCH_TIMEOUT_SECS + 60;

/// Another refresh holds the lock; this one did nothing.
#[derive(Debug)]
pub struct RefreshBusy;

impl std::fmt::Display for RefreshBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another memory refresh is already running")
    }
}

impl std::error::Error for RefreshBusy {}

/// The refresh lock: a file created with `create_new` (atomic exclusive
/// create) next to the mirror and held by the refreshing process until it
/// drops. A crashed holder leaves the file; after [`LOCK_STALE_SECS`] the
/// next refresh takes it over.
#[derive(Debug)]
pub struct RefreshLock {
    path: PathBuf,
}

/// Where the lock for a mirror lives.
#[must_use]
pub fn lock_path(mirror: &Path) -> PathBuf {
    mirror.with_extension("lock")
}

fn lock_age_secs(lock: &Path) -> Option<u64> {
    let modified = std::fs::metadata(lock).and_then(|m| m.modified()).ok()?;
    Some(modified.elapsed().map(|d| d.as_secs()).unwrap_or(0))
}

/// `true` while a live refresh holds the lock.
#[must_use]
pub fn refresh_running(mirror: &Path) -> bool {
    lock_age_secs(&lock_path(mirror)).is_some_and(|age| age < LOCK_STALE_SECS)
}

impl RefreshLock {
    /// Take the lock, or `None` when a live refresh holds it.
    #[must_use]
    pub fn acquire(mirror: &Path) -> Option<Self> {
        Self::acquire_with(mirror, LOCK_STALE_SECS)
    }

    pub(super) fn acquire_with(mirror: &Path, stale_after: u64) -> Option<Self> {
        let path = lock_path(mirror);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        for attempt in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let _ = writeln!(f, "{} {}", std::process::id(), now_secs());
                    return Some(Self { path });
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists
                        && attempt == 0
                        && lock_age_secs(&path).is_some_and(|age| age >= stale_after) =>
                {
                    // A dead holder's lock. Remove it and retry once; if two
                    // processes race here, create_new still lets only one win.
                    let _ = std::fs::remove_file(&path);
                }
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What one row does to the mirror.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Upsert(MemoryItem),
    Remove(String),
}

/// One decoded page.
#[derive(Debug, Default)]
pub struct Page {
    pub changes: Vec<Change>,
    /// `(cursor, iri)` of the last row kept (not the sentinel): the position
    /// the next page starts after.
    pub last_cursor: Option<(String, String)>,
    /// The overflow sentinel was present.
    pub overflow: bool,
}

/// A cell from quipu's `/ask` rows: a bare string, a number, or a
/// `{"value": …}` object for typed and language-tagged literals.
fn cell(row: &serde_json::Value, key: &str) -> Option<String> {
    let v = row.get(key)?;
    let s = match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Object(o) => o.get("value")?.as_str()?.to_string(),
        _ => return None,
    };
    Some(s)
}

/// Decode an `/ask` response body.
pub fn decode_page(body: &serde_json::Value) -> Result<Page> {
    let rows = body
        .get("rows")
        .and_then(|r| r.as_array())
        .context("mem_index_since reply has no rows array")?;
    let overflow = rows.len() > PAGE_LIMIT;
    let mut page = Page {
        overflow,
        ..Page::default()
    };
    // One record can arrive as several rows (one per rdf:type); keep the
    // first row per IRI within a page, in cursor order.
    let mut seen: HashSet<String> = HashSet::new();
    for row in rows.iter().take(PAGE_LIMIT) {
        let Some(iri) = cell(row, "iri").filter(|s| !s.is_empty()) else {
            continue;
        };
        if let Some(c) = cell(row, "cursor") {
            page.last_cursor = Some((c, iri.clone()));
        }
        if !seen.insert(iri.clone()) {
            continue;
        }
        let status = cell(row, "status").unwrap_or_else(|| "current".into());
        if status != "current" {
            page.changes.push(Change::Remove(iri));
            continue;
        }
        let item = MemoryItem {
            natural_key: cell(row, "naturalKey").unwrap_or_default(),
            kind: local_name(&cell(row, "kind").unwrap_or_default()).to_string(),
            label: cell(row, "label").unwrap_or_default(),
            body: cell(row, "body").unwrap_or_default(),
            source_kind: cell(row, "sourceKind").unwrap_or_default(),
            recorded_at: cell(row, "recordedAt").unwrap_or_default(),
            review_after: cell(row, "reviewAfter"),
            expires_at: cell(row, "expiresAt"),
            iri,
        };
        page.changes.push(Change::Upsert(item));
    }
    Ok(page)
}

/// Apply decoded changes. Returns `(upserted, removed)`.
pub fn apply(mirror: &mut Mirror, changes: Vec<Change>, now_rfc3339: &str) -> (usize, usize) {
    let (mut up, mut rm) = (0, 0);
    for change in changes {
        match change {
            Change::Upsert(item) if item.is_expired(now_rfc3339) => {
                if mirror.items.remove(&item.iri).is_some() {
                    rm += 1;
                }
            }
            Change::Upsert(item) => {
                mirror.items.insert(item.iri.clone(), item);
                up += 1;
            }
            Change::Remove(iri) => {
                if mirror.items.remove(&iri).is_some() {
                    rm += 1;
                }
            }
        }
    }
    (up, rm)
}

// ---------------------------------------------------------------------------
// Refresh (off the hot path)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Refresh (off the hot path)
// ---------------------------------------------------------------------------

/// What a refresh did.
#[derive(Debug, Default, Serialize)]
pub struct RefreshReport {
    pub pages: usize,
    pub upserted: usize,
    pub removed: usize,
    pub items: usize,
    pub cursor: String,
}

/// Optional read bearer: `QUIPU_AUTH_TOKEN`, then `QUIPU_AUTH_TOKEN_FILE`,
/// then `~/.config/quipu/token`. Reads only; a missing token is not an error
/// here (the server decides whether `/ask` needs one).
fn read_token() -> Option<String> {
    if let Some(t) = std::env::var("QUIPU_AUTH_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(t);
    }
    let file = std::env::var_os("QUIPU_AUTH_TOKEN_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            directories::BaseDirs::new().map(|d| d.home_dir().join(".config/quipu/token"))
        })?;
    std::fs::read_to_string(file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// One `/ask` page.
async fn fetch_page(
    client: &reqwest::Client,
    endpoint: &str,
    since: &(String, String),
    token: Option<&str>,
) -> Result<serde_json::Value> {
    let mut req = client
        .post(format!("{endpoint}/ask"))
        .json(&serde_json::json!({
            "name": INDEX_QUERY,
            "params": { "since": since.0, "since_iri": since.1 },
        }));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.context("quipu /ask unreachable")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "quipu /ask {INDEX_QUERY} returned HTTP {status}: {}",
            text.chars().take(160).collect::<String>()
        );
    }
    serde_json::from_str(&text).context("quipu /ask reply is not JSON")
}

/// Pull changes since the mirror's cursor and persist the mirror. On failure
/// the error is recorded in the mirror (so the hook can say recall was
/// skipped) and returned.
///
/// Only one refresh runs at a time: the process takes [`RefreshLock`] first
/// and returns [`RefreshBusy`] when another live refresh holds it. The attempt
/// is written to the mirror before any network call, so a hook that reads the
/// mirror meanwhile sees a refresh in progress and does not start another.
pub async fn refresh(endpoint: &str, path: &Path) -> Result<RefreshReport> {
    let Some(_lock) = RefreshLock::acquire(path) else {
        return Err(RefreshBusy.into());
    };
    let endpoint = endpoint.trim().trim_end_matches('/').to_string();
    let mut mirror = load_mirror(path)
        .filter(|m| m.endpoint == endpoint)
        .unwrap_or_else(|| Mirror {
            endpoint: endpoint.clone(),
            ..Mirror::default()
        });
    let now = now_secs();
    mirror.attempted_at = Some(now);
    let _ = save_mirror(path, &mirror);

    let result = pull(&endpoint, &mut mirror).await;
    match result {
        Ok(mut report) => {
            mirror.refreshed_at = Some(now);
            mirror.last_error = None;
            report.items = mirror.items.len();
            report.cursor = mirror.cursor.clone();
            save_mirror(path, &mirror)?;
            Ok(report)
        }
        Err(e) => {
            mirror.last_error = Some(short_error(&e));
            let _ = save_mirror(path, &mirror);
            Err(e)
        }
    }
}

async fn pull(endpoint: &str, mirror: &mut Mirror) -> Result<RefreshReport> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()?;
    let token = read_token();
    let now_rfc = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut report = RefreshReport::default();
    // Work on a copy: a refresh that fails halfway leaves the mirror as it was
    // (stale and saying so), never half-applied.
    let mut cursor = (mirror.cursor.clone(), mirror.cursor_iri.clone());
    let mut staged: Vec<Change> = Vec::new();
    loop {
        let body = fetch_page(&client, endpoint, &cursor, token.as_deref()).await?;
        let page = decode_page(&body)?;
        report.pages += 1;
        staged.extend(page.changes);
        let advanced = match page.last_cursor {
            Some(c) if c != cursor => {
                cursor = c;
                true
            }
            _ => false,
        };
        if !page.overflow || !advanced || report.pages >= MAX_PAGES {
            break;
        }
    }
    let (up, rm) = apply(mirror, staged, &now_rfc);
    // Records whose expiresAt passed since they were mirrored.
    let expired: Vec<String> = mirror
        .items
        .values()
        .filter(|i| i.is_expired(&now_rfc))
        .map(|i| i.iri.clone())
        .collect();
    for iri in &expired {
        mirror.items.remove(iri);
    }
    (mirror.cursor, mirror.cursor_iri) = cursor;
    report.upserted = up;
    report.removed = rm + expired.len();
    Ok(report)
}

fn short_error(e: &anyhow::Error) -> String {
    let full = format!("{e:#}");
    truncate_chars(&one_line(&full), 160)
}

/// Start a detached refresh if the mirror is older than the TTL. Never waits.
///
/// Not when a refresh is already running (its lock is held and not stale).
/// Two hooks racing past this check both spawn, but only one child gets the
/// lock; the other exits at once. `BOBBIN_MEMORY_NO_SPAWN=1` turns spawning
/// off (tests).
pub fn maybe_spawn_refresh(path: &Path, mirror: Option<&Mirror>, endpoint: &str, now: u64) {
    if std::env::var("BOBBIN_MEMORY_NO_SPAWN").is_ok_and(|v| v == "1") {
        return;
    }
    let fresh = mirror
        .filter(|m| m.endpoint == endpoint)
        .and_then(|m| m.attempted_at)
        .is_some_and(|t| now.saturating_sub(t) < REFRESH_TTL_SECS);
    if fresh {
        return;
    }
    if refresh_running(path) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .args(["hook", "memory-refresh", "--quiet", "--endpoint", endpoint])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
