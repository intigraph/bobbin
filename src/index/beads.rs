//! Bead (issue) indexing — the source-agnostic half.
//!
//! Two backends live under `beads/`: [`dolt`] (MySQL protocol, the original
//! store) and [`jsonl`] (the br export, which is what the crew store became at
//! the clbx2 cutover). This module owns everything they MUST agree on —
//! chunk identity, content assembly, label exclusion — and dispatches on
//! `config.source`.
//!
//! Why the shared half is not simply duplicated per backend: bobbin spent
//! 18 days answering bead search out of the frozen Dolt snapshot
//! (aegis-205llh) without reporting anything unusual. A second set of chunk
//! keys would have produced the same shape of failure one layer in — two
//! corpora, each invisible to the other's removal sweep.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use crate::config::{BeadsConfig, BeadsSource};
use crate::types::{Chunk, ChunkType};

/// Live metadata for a bead, fetched from the configured store.
#[derive(Debug, Clone)]
pub struct LiveBeadMetadata {
    pub status: String,
    pub priority: i32,
    pub assignee: Option<String>,
    pub title: String,
    pub issue_type: String,
    pub owner: String,
    pub labels: Vec<String>,
    pub created_at: Option<String>,
}

/// A single bead (issue) as fetched from whichever store is configured.
#[derive(Debug)]
struct BeadRow {
    id: String,
    title: String,
    description: String,
    status: String,
    priority: i32,
    assignee: Option<String>,
    notes: String,
    /// Raw `metadata` JSON column (may be empty/`{}`).
    metadata: String,
}

/// Stable content hash for a bead chunk, used for incremental indexing
/// (skip re-embedding beads whose assembled content is unchanged).
pub fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// True if a bead `metadata` JSON string carries meaningful content worth
/// indexing (i.e. not empty, `{}`, or `null`).
fn metadata_is_meaningful(metadata: &str) -> bool {
    let t = metadata.trim();
    !(t.is_empty() || t == "{}" || t == "null")
}

/// True if a bead carries any excluded label (case-insensitive). Used to keep
/// sensitive beads (e.g. `security`, `escalation`) out of the index entirely.
fn bead_excluded(labels: &[String], exclude_labels: &[String]) -> bool {
    if exclude_labels.is_empty() {
        return false;
    }
    labels
        .iter()
        .any(|l| exclude_labels.iter().any(|x| x.eq_ignore_ascii_case(l)))
}

/// Assemble the embeddable text for a bead from its fields, comments, and labels.
///
/// Kept as a pure function so the content layout can be unit-tested without a
/// live store, and so both backends assemble byte-identical content.
fn build_bead_content(issue: &BeadRow, comments: &[&CommentRow], labels: &[String]) -> String {
    let mut content = format!("{}\n\n{}", issue.title, issue.description);

    if !issue.notes.is_empty() {
        content.push_str("\n\nNotes:\n");
        content.push_str(&issue.notes);
    }

    // Labels (e.g. `b:<slug>`, `pitch`) — valuable for semantic + filtered search.
    if !labels.is_empty() {
        content.push_str("\n\nLabels: ");
        content.push_str(&labels.join(", "));
    }

    // Structured metadata JSON (guidance/lineage records, source refs, etc.).
    if metadata_is_meaningful(&issue.metadata) {
        content.push_str("\n\nMetadata:\n");
        content.push_str(issue.metadata.trim());
    }

    // Append metadata
    content.push_str(&format!(
        "\n\nStatus: {} | Priority: P{} | Assignee: {}",
        issue.status,
        issue.priority,
        issue.assignee.as_deref().unwrap_or("unassigned")
    ));

    // Append comments
    if !comments.is_empty() {
        content.push_str("\n\nComments:");
        for c in comments {
            content.push_str(&format!("\n--- {} ---\n{}", c.author, c.text));
        }
    }

    content
}

/// A comment on a bead.
#[derive(Debug)]
struct CommentRow {
    issue_id: String,
    author: String,
    text: String,
}

mod dolt;
mod jsonl;
mod keys;
pub mod provenance;
pub(crate) use keys::issues_where_clause;
pub use keys::{bead_file_path, bead_file_paths, rig_of};

/// Turn fetched rows into indexable chunks.
///
/// **Both backends call this, and nothing else constructs a bead chunk.** The
/// `file_path` it builds IS the incremental machinery's identity, so a second
/// spelling anywhere would give that backend its own corpus, invisible to the
/// other's removal sweep — the same class of silent divergence the batch and
/// single-bead paths already share `bead_file_path` to avoid.
fn rows_to_chunks(
    rig: &str,
    issues: Vec<BeadRow>,
    comments: &[CommentRow],
    labels_by_issue: &HashMap<String, Vec<String>>,
    exclude_labels: &[String],
) -> Vec<Chunk> {
    let mut comments_by_issue: HashMap<&str, Vec<&CommentRow>> = HashMap::new();
    for comment in comments {
        comments_by_issue
            .entry(comment.issue_id.as_str())
            .or_default()
            .push(comment);
    }

    issues
        .into_iter()
        .filter_map(|issue| {
            let labels = labels_by_issue.get(&issue.id).cloned().unwrap_or_default();
            // Skip beads carrying an excluded label (e.g. security).
            if bead_excluded(&labels, exclude_labels) {
                return None;
            }
            let issue_comments: Vec<&CommentRow> = comments_by_issue
                .get(issue.id.as_str())
                .cloned()
                .unwrap_or_default();

            let content = build_bead_content(&issue, &issue_comments, &labels);

            let file_path = bead_file_path(rig, &issue.id);
            let mut hasher = Sha256::new();
            hasher.update(file_path.as_bytes());
            let id = hex::encode(hasher.finalize());

            Some(Chunk {
                id,
                file_path,
                chunk_type: ChunkType::Issue,
                name: Some(issue.title),
                start_line: 0,
                end_line: 0,
                content,
                language: "beads".to_string(),
                tags: labels.join(","),
            })
        })
        .collect()
}

/// The path configured for `db_name`, or a refusal naming the gap.
///
/// A missing path is an ERROR, never an empty rig: "indexed zero beads" and
/// "this rig is not configured" are indistinguishable once the run is over,
/// and the first one is how a search silently goes blind.
fn jsonl_path_for<'a>(config: &'a BeadsConfig, db_name: &str) -> Result<&'a str> {
    match config.jsonl_paths.get(db_name) {
        Some(p) if !p.trim().is_empty() => Ok(p.as_str()),
        _ => bail!(
            "beads source = \"jsonl\" but no jsonl_paths entry for database \"{db_name}\" \
             (set [beads.jsonl_paths] {db_name} = \"/path/to/.beads/issues.jsonl\")"
        ),
    }
}

/// Fetch beads from all configured Dolt databases and convert to Chunks.
pub async fn fetch_beads(config: &BeadsConfig) -> Result<Vec<Chunk>> {
    if !config.enabled || config.databases.is_empty() {
        return Ok(vec![]);
    }

    let mut all_chunks = Vec::new();

    for db_name in &config.databases {
        let chunks = fetch_from_source(config, db_name, None)
            .await
            .with_context(|| format!("Failed to fetch beads from {}", db_name))?;
        all_chunks.extend(chunks);
    }

    Ok(all_chunks)
}

/// Fetch the chunk for ONE bead, across every configured database (or just
/// `rig_filter`'s, when given).
///
/// Returns an empty vec when the bead does not exist, is closed/deleted/aged
/// out under the configured visibility rules, or carries an excluded label —
/// all four of which the caller must treat as "remove it from the index",
/// never as "leave whatever is there". A bead being closed is precisely when a
/// post-write trigger fires, and it is the case a naive re-embed gets wrong.
pub async fn fetch_bead(
    config: &BeadsConfig,
    bead_id: &str,
    rig_filter: Option<&str>,
) -> Result<Vec<Chunk>> {
    if !config.enabled || config.databases.is_empty() {
        return Ok(vec![]);
    }

    let mut found = Vec::new();
    for db_name in &config.databases {
        if rig_filter.is_some_and(|want| want != rig_of(db_name)) {
            continue;
        }
        let chunks = fetch_from_source(config, db_name, Some(bead_id))
            .await
            .with_context(|| format!("Failed to fetch bead {} from {}", bead_id, db_name))?;
        found.extend(chunks);
    }

    Ok(found)
}

/// How old the export at `path` is, as a short human string.
///
/// A JSONL export only moves when something writes it. That producer stopping
/// is the same failure as pointing at a dead store, and it is just as quiet —
/// so the age goes in the run output beside the path, where a reader scanning
/// a log will see "20d" without having to go and stat the file.
fn export_age(path: &str) -> String {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return "UNREADABLE".to_string();
    };
    match modified.elapsed() {
        Ok(d) if d.as_secs() < 3600 => format!("{}m old", d.as_secs() / 60),
        Ok(d) if d.as_secs() < 86_400 => format!("{}h old", d.as_secs() / 3600),
        Ok(d) => format!("{}d old", d.as_secs() / 86_400),
        // A future mtime is a clock problem, not an age; say so rather than
        // printing a plausible number.
        Err(_) => "mtime in the future".to_string(),
    }
}

/// A one-line description of where beads are being read from.
///
/// Printed by every indexing run. bobbin answered bead search out of a frozen
/// store for 18 days without reporting anything unusual (aegis-205llh); a run
/// that names its source puts the next such divergence in a log where it can
/// be seen, instead of leaving it to be inferred from the results.
pub fn source_label(config: &BeadsConfig) -> String {
    match config.source {
        BeadsSource::Dolt => format!("Dolt at {}:{}", config.host, config.port),
        BeadsSource::Jsonl => {
            let paths: Vec<String> = config
                .databases
                .iter()
                .filter_map(|db| config.jsonl_paths.get(db))
                .map(|p| format!("{p} [{}]", export_age(p)))
                .collect();
            if paths.is_empty() {
                "br JSONL (NO PATHS CONFIGURED)".to_string()
            } else {
                format!("br JSONL ({})", paths.join(", "))
            }
        }
    }
}

/// Fetch one database's beads from whichever store is configured.
async fn fetch_from_source(
    config: &BeadsConfig,
    db_name: &str,
    only_id: Option<&str>,
) -> Result<Vec<Chunk>> {
    match config.source {
        BeadsSource::Dolt => dolt::fetch_from_database(config, db_name, only_id).await,
        BeadsSource::Jsonl => {
            jsonl::fetch_from_file(config, jsonl_path_for(config, db_name)?, db_name, only_id)
        }
    }
}

/// Fetch metadata for specific beads from the configured source.
///
/// Takes a list of (rig, bead_id) pairs and returns a map from bead_id to
/// metadata. A JSONL reread is still a snapshot; it is not current-board proof.
/// Used by bead search with explicit provenance — the half that made the failure so
/// hard to see, because it kept reporting *confident* metadata from the wrong
/// store rather than failing.
pub async fn fetch_bead_metadata(
    config: &BeadsConfig,
    bead_ids: &[(String, String)], // (rig, bead_id)
) -> Result<HashMap<String, LiveBeadMetadata>> {
    if !config.enabled || bead_ids.is_empty() {
        return Ok(HashMap::new());
    }
    match config.source {
        BeadsSource::Dolt => dolt::fetch_bead_metadata(config, bead_ids).await,
        BeadsSource::Jsonl => jsonl::fetch_bead_metadata(config, bead_ids),
    }
}

#[cfg(test)]
#[path = "beads_tests.rs"]
mod tests;
