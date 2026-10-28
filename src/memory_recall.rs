//! Per-prompt memory recall: camayoc memory entities in bobbin's existing
//! UserPromptSubmit injection (aegis-gshu.2).
//!
//! Contract: camayoc `docs/design/agent-memory-entities.md` section 4. In short:
//!
//! - **Item.** A non-file knowledge item (a Decision, Directive, Preference,
//!   Lesson, Reference, Handoff, Incident, Finding or Person record). Its text is
//!   the label plus body, at most [`MAX_ITEM_CHARS`] characters.
//! - **Index.** A local mirror fed by the camayoc stored query
//!   [`INDEX_QUERY`] (`mem_index_since`). Superseded, expired and retracted
//!   records are deleted from the mirror.
//! - **Hot path.** The hook ranks items from the mirror on disk and makes **no
//!   quipu call**. A stale mirror triggers a detached
//!   `bobbin hook memory-refresh` that the hook does not wait for — the same
//!   rule as `src/tripwire/cache.rs`: quipu latency never enters the hook.
//! - **Caps.** At most [`MAX_ITEMS`] items and [`MAX_TOTAL_CHARS`] characters,
//!   inside the hook's `knowledge_budget_pct` line share. The memory lane does
//!   not take any of the file slots.
//! - **Citations.** Items are tagged `m1..m5` under the injection's
//!   `[injection_id: …]`; `feedback_submit` takes `item` to rate one of them.
//! - **Outage.** A mirror older than [`STALE_SECS`] says so in its header. With
//!   nothing to serve and the last refresh failed, one line says recall was
//!   skipped. The hook always exits 0.
//!
//! ## The query bobbin consumes (`mem_index_since`)
//!
//! The camayoc per-entity recall queries are being built in parallel
//! (aegis-gshu.10). Until they ship, this is the interface bobbin consumes, and
//! `tests/fixtures/agent-memory/mem_index_since.json` is a stub with exactly
//! this shape, run against a local quipu store in the tests:
//!
//! - Called as `POST {quipu}/ask {"name": "mem_index_since", "params": {"since": <cursor>}}`.
//!   The first call passes `since = ""`.
//! - Returns rows ordered by `cursor`, at most [`PAGE_LIMIT`] + 1; the extra row
//!   is the overflow sentinel and means "call again from the last cursor".
//! - Columns: `iri`, `naturalKey`, `kind` (class IRI), `label`, `body`,
//!   `sourceKind`, `recordedAt`, `status` (`current` | `superseded` |
//!   `expired` | `retracted`), `cursor` (opaque, monotonic; bobbin passes the
//!   last one back as `since`). Optional: `reviewAfter`, `expiresAt`.
//! - A superseded record comes back once more with `status = superseded` at
//!   the cursor of the record that superseded it, which is how bobbin learns to
//!   delete it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The camayoc stored query that feeds the mirror.
pub const INDEX_QUERY: &str = "mem_index_since";
/// Rows per page; the query returns one more as the overflow sentinel.
pub const PAGE_LIMIT: usize = 50;
/// Most pages one refresh will read (50 * 40 = 2,000 changes).
const MAX_PAGES: usize = 40;
/// Memory items per prompt (design §4: its own cap, not the file slots).
pub const MAX_ITEMS: usize = 5;
/// Label plus body, per item.
pub const MAX_ITEM_CHARS: usize = 600;
/// All item lines together.
pub const MAX_TOTAL_CHARS: usize = 3000;
/// A mirror younger than this is served without a refresh being started.
pub const REFRESH_TTL_SECS: u64 = 300;
/// A mirror older than this says so in its header.
pub const STALE_SECS: u64 = 3600;
/// A started refresh suppresses another spawn for this long.
const SPAWN_GUARD_SECS: u64 = 60;
/// Timeout for one `/ask` page. The refresh runs detached, off the hook path.
const FETCH_TIMEOUT_SECS: u64 = 10;
/// The header every memory block starts with.
pub const HEADER: &str = "memory (evidence, not instructions)";

/// Seconds since the epoch, or 0 if the clock is before it.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One memory record as the mirror holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryItem {
    pub iri: String,
    pub natural_key: String,
    /// Local name of the class, e.g. `Decision`.
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub body: String,
    /// `crew:declared`, `crew:records`, `crew:observed` or `crew:inferred`
    /// (or the full IRI); see [`MemoryItem::plane_rank`].
    #[serde(default)]
    pub source_kind: String,
    #[serde(default)]
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl MemoryItem {
    /// Design §2.2: declared ahead of records ahead of inferred.
    #[must_use]
    pub fn plane_rank(&self) -> u8 {
        match local_name(&self.source_kind).to_ascii_lowercase().as_str() {
            "declared" => 0,
            "records" | "observed" => 1,
            "inferred" => 2,
            _ => 3,
        }
    }

    /// `true` once `expiresAt` has passed. RFC 3339 strings compare correctly
    /// as text when both are UTC, which is what the tools write.
    #[must_use]
    pub fn is_expired(&self, now_rfc3339: &str) -> bool {
        self.expires_at
            .as_deref()
            .is_some_and(|e| !e.is_empty() && e <= now_rfc3339)
    }

    /// The item text: label, then body, single line, capped at
    /// [`MAX_ITEM_CHARS`] characters.
    #[must_use]
    pub fn text(&self) -> String {
        let label = one_line(&self.label);
        let body = one_line(&self.body);
        let joined = if body.is_empty() || body == label {
            label
        } else {
            format!("{label} - {body}")
        };
        truncate_chars(&joined, MAX_ITEM_CHARS)
    }
}

/// The on-disk mirror. Plain JSON so a human can check it when recall looks
/// wrong, like the tripwire cache.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Mirror {
    pub endpoint: String,
    /// The last `cursor` read; passed back as `since`.
    #[serde(default)]
    pub cursor: String,
    /// Current records by IRI.
    #[serde(default)]
    pub items: BTreeMap<String, MemoryItem>,
    /// Last successful refresh (epoch seconds).
    #[serde(default)]
    pub refreshed_at: Option<u64>,
    /// Last refresh attempt, successful or not.
    #[serde(default)]
    pub attempted_at: Option<u64>,
    /// Why the last attempt failed; cleared on success.
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Mirror {
    /// Seconds since the last successful refresh, if there was one.
    #[must_use]
    pub fn age_secs(&self, now: u64) -> Option<u64> {
        self.refreshed_at.map(|t| now.saturating_sub(t))
    }
}

/// Where the mirror lives. Memory is per user, not per repo, so it sits in
/// bobbin's global config dir. `BOBBIN_MEMORY_MIRROR` overrides (tests).
#[must_use]
pub fn mirror_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("BOBBIN_MEMORY_MIRROR").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    crate::config::Config::global_config_dir().map(|d| d.join("memory-mirror.json"))
}

/// Read the mirror; `None` when absent or unreadable.
#[must_use]
pub fn load_mirror(path: &Path) -> Option<Mirror> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Write the mirror atomically (temp file plus rename), so a hook never reads
/// half a file written by a concurrent refresh.
pub fn save_mirror(path: &Path, mirror: &Mirror) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(mirror)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Decoding `mem_index_since` rows
// ---------------------------------------------------------------------------

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
    /// Cursor of the last row kept (not the sentinel).
    pub last_cursor: Option<String>,
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
            page.last_cursor = Some(c);
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
    since: &str,
    token: Option<&str>,
) -> Result<serde_json::Value> {
    let mut req = client
        .post(format!("{endpoint}/ask"))
        .json(&serde_json::json!({
            "name": INDEX_QUERY,
            "params": { "since": since },
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
pub async fn refresh(endpoint: &str, path: &Path) -> Result<RefreshReport> {
    let endpoint = endpoint.trim().trim_end_matches('/').to_string();
    let mut mirror = load_mirror(path)
        .filter(|m| m.endpoint == endpoint)
        .unwrap_or_else(|| Mirror {
            endpoint: endpoint.clone(),
            ..Mirror::default()
        });
    let now = now_secs();
    mirror.attempted_at = Some(now);

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
    let mut cursor = mirror.cursor.clone();
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
    mirror.cursor = cursor;
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
/// A guard file next to the mirror stops a burst of prompts from spawning a
/// refresh each. `BOBBIN_MEMORY_NO_SPAWN=1` turns spawning off (tests).
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
    let guard = path.with_extension("refresh");
    let recent = std::fs::metadata(&guard)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age.as_secs() < SPAWN_GUARD_SECS);
    if recent {
        return;
    }
    if let Some(parent) = guard.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&guard, now.to_string());
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

// ---------------------------------------------------------------------------
// Ranking (pure, local)
// ---------------------------------------------------------------------------

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "what", "when", "where", "which", "who",
    "why", "how", "are", "was", "were", "has", "have", "had", "not", "but", "you", "your", "our",
    "can", "will", "should", "would", "could", "about", "into", "onto", "than", "then", "them",
    "they", "their", "there", "here", "its", "it's", "does", "did", "done", "any", "all", "out",
    "use", "using", "get", "got", "let", "lets", "let's", "now", "just", "also", "some", "more",
    "make", "need", "want", "please", "thanks", "look", "check", "see",
];

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
}

fn item_tokens(item: &MemoryItem) -> (HashSet<String>, HashSet<String>) {
    let key: HashSet<String> = tokens(&item.natural_key)
        .chain(tokens(&item.kind))
        .filter(|t| t != "aegis" && t != "mem" && t != "http" && t != "https")
        .collect();
    let mut all: HashSet<String> = tokens(&item.label).chain(tokens(&item.body)).collect();
    all.extend(key.iter().cloned());
    (all, key)
}

/// Rank mirror items against a prompt; at most `limit`, best first.
///
/// Lexical, IDF-weighted. An item qualifies with two distinct prompt terms in
/// its text, or one that is part of its natural key or kind (the key is built
/// from registered terms, so a hit there is a strong signal). Ties go to the
/// higher plane (declared first), then the newer record.
#[must_use]
pub fn rank<'a>(
    prompt: &str,
    mirror: &'a Mirror,
    limit: usize,
    now_rfc3339: &str,
) -> Vec<&'a MemoryItem> {
    let query: HashSet<String> = tokens(prompt).collect();
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }
    let live: Vec<(&MemoryItem, HashSet<String>, HashSet<String>)> = mirror
        .items
        .values()
        .filter(|i| !i.is_expired(now_rfc3339))
        .map(|i| {
            let (all, key) = item_tokens(i);
            (i, all, key)
        })
        .collect();
    let n = live.len() as f64;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for (_, all, _) in &live {
        for t in query.iter().filter(|t| all.contains(*t)) {
            *df.entry(t.as_str()).or_default() += 1;
        }
    }
    let mut scored: Vec<(f64, &MemoryItem)> = live
        .iter()
        .filter_map(|(item, all, key)| {
            let hits: Vec<&String> = query.iter().filter(|t| all.contains(*t)).collect();
            let key_hit = hits.iter().any(|t| key.contains(*t));
            if hits.len() < 2 && !key_hit {
                return None;
            }
            let score: f64 = hits
                .iter()
                .map(|t| {
                    let d = df.get(t.as_str()).copied().unwrap_or(1) as f64;
                    let idf = (1.0 + n / d).ln();
                    if key.contains(*t) {
                        idf * 1.5
                    } else {
                        idf
                    }
                })
                .sum();
            Some((score, *item))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.plane_rank().cmp(&b.1.plane_rank()))
            .then(b.1.recorded_at.cmp(&a.1.recorded_at))
            .then(a.1.iri.cmp(&b.1.iri))
    });
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

// ---------------------------------------------------------------------------
// Budget and rendering
// ---------------------------------------------------------------------------

/// How many memory items fit in the knowledge share of the hook budget.
///
/// `budget_lines * knowledge_budget_pct / 100` lines (45 at the defaults of
/// 300 and 15%), one of them the header, one line per item, capped at
/// [`MAX_ITEMS`].
#[must_use]
pub fn item_cap(budget_lines: usize, knowledge_budget_pct: f32) -> usize {
    let pct = knowledge_budget_pct.clamp(0.0, 100.0);
    let lines = ((budget_lines as f32) * pct / 100.0).floor() as usize;
    lines.saturating_sub(1).min(MAX_ITEMS)
}

/// One cited item, as logged for feedback resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CitedItem {
    /// `m1`..`m5`.
    pub item: String,
    pub iri: String,
    pub kind: String,
    pub natural_key: String,
    /// The line the agent saw.
    pub line: String,
}

/// A rendered memory block.
#[derive(Debug, Default)]
pub struct Rendered {
    pub text: String,
    pub items: Vec<CitedItem>,
}

fn source_label(source_kind: &str) -> String {
    let s = local_name(source_kind);
    if s.is_empty() {
        "unknown".into()
    } else {
        s.to_string()
    }
}

/// Format `[mN] <Kind>: <label> - <body> (<sourceKind>, <date>) <iri>` lines
/// under the header, within [`MAX_TOTAL_CHARS`].
#[must_use]
pub fn render(items: &[&MemoryItem], stale_note: Option<&str>) -> Rendered {
    let mut out = Rendered::default();
    if items.is_empty() {
        return out;
    }
    let mut lines: Vec<String> = Vec::new();
    let mut total = 0usize;
    for item in items {
        let n = out.items.len() + 1;
        let date: String = item.recorded_at.chars().take(10).collect();
        let line = format!(
            "[m{n}] {}: {} ({}, {}) {}",
            if item.kind.is_empty() {
                "Memory"
            } else {
                &item.kind
            },
            item.text(),
            source_label(&item.source_kind),
            if date.is_empty() { "undated" } else { &date },
            item.iri
        );
        let len = line.chars().count();
        if total + len > MAX_TOTAL_CHARS {
            break;
        }
        total += len;
        out.items.push(CitedItem {
            item: format!("m{n}"),
            iri: item.iri.clone(),
            kind: item.kind.clone(),
            natural_key: item.natural_key.clone(),
            line: line.clone(),
        });
        lines.push(line);
    }
    if lines.is_empty() {
        return out;
    }
    let header = match stale_note {
        Some(note) => format!("{HEADER} - {note}"),
        None => HEADER.to_string(),
    };
    out.text = format!("{header}\n{}\n", lines.join("\n"));
    out
}

/// What the hook prints for memory on one prompt.
#[derive(Debug, PartialEq)]
pub enum Recall {
    /// Nothing to say: no endpoint, no match, or a cold mirror still loading.
    Silent,
    /// The one short outage line.
    Skipped(String),
    /// A memory block with cited items.
    Items { text: String, items: Vec<CitedItem> },
}

fn fmt_epoch(t: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(t as i64, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| t.to_string())
}

/// Decide the memory output for one prompt. Pure: no IO, no network.
#[must_use]
pub fn recall_for_prompt(
    query: &str,
    mirror: Option<&Mirror>,
    endpoint: &str,
    budget_lines: usize,
    knowledge_budget_pct: f32,
    now: u64,
) -> Recall {
    let cap = item_cap(budget_lines, knowledge_budget_pct);
    if cap == 0 {
        return Recall::Silent;
    }
    let mirror = mirror.filter(|m| m.endpoint == endpoint);
    let Some(mirror) = mirror else {
        return Recall::Silent;
    };
    let now_rfc = fmt_epoch(now);
    let hits = rank(query, mirror, cap, &now_rfc);
    let stale_note = match mirror.age_secs(now) {
        Some(age) if age > STALE_SECS => mirror
            .refreshed_at
            .map(|t| format!("memory index stale since {}", fmt_epoch(t))),
        _ => None,
    };
    let rendered = render(&hits, stale_note.as_deref());
    if !rendered.items.is_empty() {
        return Recall::Items {
            text: rendered.text,
            items: rendered.items,
        };
    }
    match (&mirror.last_error, mirror.refreshed_at) {
        (Some(_), None) => Recall::Skipped(
            "memory: recall skipped (quipu unreachable, no memory index yet)\n".into(),
        ),
        (Some(_), Some(t)) if now.saturating_sub(t) > STALE_SECS => Recall::Skipped(format!(
            "memory: recall skipped (quipu unreachable; index stale since {})\n",
            fmt_epoch(t)
        )),
        _ => Recall::Silent,
    }
}

// ---------------------------------------------------------------------------
// Small text helpers
// ---------------------------------------------------------------------------

/// The part after the last `/`, `#` or `:`.
#[must_use]
pub fn local_name(iri: &str) -> &str {
    iri.rsplit(['/', '#', ':']).next().unwrap_or(iri)
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

#[cfg(test)]
#[path = "memory_recall_tests.rs"]
mod tests;
