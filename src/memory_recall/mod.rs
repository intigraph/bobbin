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
//! (aegis-gshu.10); camayoc `queries/mem_index_since.json` implements this
//! interface, and `tests/fixtures/agent-memory/mem_index_since.json` is a stub
//! of the same shape, run against a local quipu store in the tests:
//!
//! - Called as `POST {quipu}/ask {"name": "mem_index_since", "params":
//!   {"since": <cursor>}}`. The first call passes `since = ""`.
//! - `cursor` is `<recordedAt>|<iri>`: the change time (fixed-width UTC) and
//!   the row's IRI, so it orders by time then IRI and is unique per row. Rows
//!   come back ordered by it, after `since`. A timestamp-only cursor would
//!   drop rows sharing a timestamp across a page boundary.
//! - Returns at most [`PAGE_LIMIT`] + 1 rows; the extra row is the overflow
//!   sentinel and means "call again from the last row's cursor".
//! - Columns: `iri`, `naturalKey`, `kind` (class IRI), `label`, `body`,
//!   `sourceKind`, `recordedAt`, `status` (`current` | `superseded` |
//!   `expired` | `retracted`), `cursor` (opaque to bobbin; see above).
//!   Optional: `reviewAfter`, `expiresAt`.
//! - A superseded record comes back once more with `status = superseded` at
//!   the cursor of the record that superseded it, which is how bobbin learns to
//!   delete it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The camayoc stored query that feeds the mirror.
pub const INDEX_QUERY: &str = "mem_index_since";
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
    /// The last row's `cursor`; passed back as `since`.
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
mod index;
mod recall;

pub use index::*;
pub use recall::*;

// ---------------------------------------------------------------------------
// Small text helpers
// ---------------------------------------------------------------------------

/// The part after the last `/`, `#` or `:`.
#[must_use]
pub fn local_name(iri: &str) -> &str {
    iri.rsplit(['/', '#', ':']).next().unwrap_or(iri)
}

pub(crate) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// An epoch second as RFC 3339 UTC.
pub(crate) fn fmt_epoch(t: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(t as i64, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| t.to_string())
}

#[cfg(test)]
mod tests;
