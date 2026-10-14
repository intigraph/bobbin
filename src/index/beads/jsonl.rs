//! br JSONL bead backend.
//!
//! The crew bead store moved off Dolt to br (SQLite + JSONL) at the clbx2
//! cutover on 2026-08-29. br exports every issue as one JSON object per line
//! in `.beads/issues.jsonl`, which is TRACKED in the rig's git repo — so a host
//! that already clones the repo for code indexing has the bead corpus on disk
//! with no new transport, no credential, and no network dependency at all.
//!
//! ## What this reads, and what it deliberately does not invent
//!
//! The Dolt `issues` table and a br JSONL record carry the same fields with one
//! exception: br has no `metadata` column. Rather than synthesise one out of
//! br-only fields (`design`, `acceptance_criteria`, `dependencies`), this maps
//! `metadata` only when a record actually carries it, and otherwise leaves it
//! empty — which `metadata_is_meaningful` already treats as "omit the section".
//! Content assembled here is therefore byte-identical to the Dolt path for any
//! bead whose fields match.
//!
//! ## Visibility
//!
//! The SQL backend pushes `include_closed` / `max_age_days` down as a `WHERE`
//! clause. There is nothing to push a clause into here, so the same rules are
//! applied by `keys::bead_visible`, which is tested against the SQL clause on
//! one shared table of cases. Two divergent rule sets would give the two
//! backends different corpora from the same rig.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};

use super::keys::{bead_visible, rig_of};
use super::{bead_excluded, rows_to_chunks, BeadRow, CommentRow, LiveBeadMetadata};
use crate::config::BeadsConfig;
use crate::types::Chunk;

/// One `issues.jsonl` record. Unknown fields are ignored — br adds fields over
/// time and a new one must not take bead search down.
#[derive(Debug, Deserialize)]
struct JsonlIssue {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    issue_type: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    notes: String,
    /// Present only if the export carries one; br records generally do not.
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    comments: Vec<JsonlComment>,
}

#[derive(Debug, Deserialize)]
struct JsonlComment {
    #[serde(default)]
    author: String,
    #[serde(default)]
    text: String,
}

/// The id at the head of a record, without parsing the whole line.
///
/// br writes `id` first, so this resolves in a few bytes for the common case.
/// It returns `None` for any other shape, and every caller falls back to a full
/// parse on `None` — a layout change makes this slow, never wrong.
fn quick_id(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("{\"id\":\"")?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Read and parse `path`, keeping records the caller's filter admits.
///
/// Later records win on a repeated id: a JSONL export is read as a log, so the
/// last write is the current state.
fn read_records<F>(path: &str, mut keep: F) -> Result<Vec<JsonlIssue>>
where
    F: FnMut(&str) -> bool,
{
    let file = File::open(path).with_context(|| format!("open bead JSONL export at {path}"))?;
    let reader = BufReader::new(file);

    // Insertion-ordered, last-write-wins: `slot_of` remembers where an id was
    // first seen so a later record REPLACES it in place rather than appending a
    // second copy of the same bead.
    let mut records: Vec<JsonlIssue> = Vec::new();
    let mut slot_of: HashMap<String, usize> = HashMap::new();

    for (lineno, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read {path} line {}", lineno + 1))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Cheap id extraction first; a non-matching shape falls through to the
        // full parse rather than being skipped.
        if let Some(id) = quick_id(trimmed) {
            if !keep(id) {
                continue;
            }
        }
        let issue: JsonlIssue = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            // A single malformed line must not take the whole corpus down; the
            // rest of the export is still good data.
            Err(_) => continue,
        };
        if !keep(&issue.id) {
            continue;
        }
        match slot_of.get(&issue.id) {
            Some(&slot) => records[slot] = issue,
            None => {
                slot_of.insert(issue.id.clone(), records.len());
                records.push(issue);
            }
        }
    }

    Ok(records)
}

/// Fetch one rig's beads from its JSONL export, optionally narrowed to one id.
///
/// `only_id` narrows WITHOUT relaxing visibility — the same load-bearing
/// property the SQL clause has, so that `index-bead <id>` cannot re-admit a
/// bead the batch sweep filters out.
pub(super) fn fetch_from_file(
    config: &BeadsConfig,
    path: &str,
    db_name: &str,
    only_id: Option<&str>,
) -> Result<Vec<Chunk>> {
    let rig = rig_of(db_name);
    let records = read_records(path, |id| only_id.is_none_or(|want| want == id))?;

    let mut issues: Vec<BeadRow> = Vec::new();
    let mut comments: Vec<CommentRow> = Vec::new();
    let mut labels_by_issue: HashMap<String, Vec<String>> = HashMap::new();

    for rec in records {
        if !bead_visible(config, &rec.status, rec.created_at.as_deref()) {
            continue;
        }
        // Checked here as well as in rows_to_chunks so an excluded bead's
        // comments are never even assembled.
        if bead_excluded(&rec.labels, &config.exclude_labels) {
            continue;
        }
        if config.include_comments {
            for c in &rec.comments {
                comments.push(CommentRow {
                    issue_id: rec.id.clone(),
                    author: c.author.clone(),
                    text: c.text.clone(),
                });
            }
        }
        if !rec.labels.is_empty() {
            labels_by_issue.insert(rec.id.clone(), rec.labels.clone());
        }
        issues.push(BeadRow {
            id: rec.id,
            title: rec.title,
            description: rec.description,
            status: rec.status,
            priority: rec.priority,
            assignee: rec.assignee,
            notes: rec.notes,
            metadata: rec.metadata.map(|m| m.to_string()).unwrap_or_default(),
        });
    }

    Ok(rows_to_chunks(
        rig,
        issues,
        &comments,
        &labels_by_issue,
        &config.exclude_labels,
    ))
}

/// Snapshot metadata for named beads, read from the JSONL exports.
///
/// Unlike indexing, this runs per search. It streams each file once, admitting
/// only the wanted ids by the cheap head check, so the cost is a scan rather
/// than a full parse of the corpus. Rereading does not prove board freshness.
pub(super) fn fetch_bead_metadata(
    config: &BeadsConfig,
    bead_ids: &[(String, String)],
) -> Result<HashMap<String, LiveBeadMetadata>> {
    let mut result = HashMap::new();

    // Group wanted ids by database, exactly as the Dolt path does.
    let mut by_db: HashMap<String, HashSet<String>> = HashMap::new();
    for (rig, bead_id) in bead_ids {
        let db_name = format!("beads_{}", rig);
        if config.databases.contains(&db_name) {
            by_db.entry(db_name).or_default().insert(bead_id.clone());
        }
    }

    for (db_name, wanted) in &by_db {
        let path = super::jsonl_path_for(config, db_name)?;
        let records = read_records(path, |id| wanted.contains(id))?;
        for rec in records {
            result.insert(
                rec.id,
                LiveBeadMetadata {
                    status: rec.status,
                    priority: rec.priority,
                    assignee: rec.assignee,
                    title: rec.title,
                    issue_type: rec.issue_type.unwrap_or_else(|| "task".to_string()),
                    owner: rec.owner.unwrap_or_default(),
                    labels: rec.labels,
                    // The Dolt path formats created_at as %Y-%m-%d; take the
                    // date half of the RFC3339 timestamp for the same shape.
                    created_at: rec
                        .created_at
                        .and_then(|t| t.get(..10).map(str::to_string))
                        .filter(|d| !d.is_empty()),
                },
            );
        }
    }

    Ok(result)
}

#[cfg(test)]
#[path = "jsonl_tests.rs"]
mod tests;
