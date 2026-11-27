//! Seeds bead backend: the LIVE board, read from its Quipu named graph.
//!
//! Seeds (`sd`) keeps every work item as facts in a Quipu graph
//! (`https://seeds.local/project/<rig>`), one subject per item at
//! `https://seeds.local/item/<id>`. The `jsonl` backend reads a retired br
//! export, which is how bead search came to serve closed items as open and
//! retired `inbox:` pointers as work (aegis-8dda.4). This backend reads the
//! board itself, so the indexed status/priority/assignee are the board's.
//!
//! ## Bounded reads, loud failures
//!
//! Enumeration is keyset-paged on the item identifier with an explicit page
//! size. A page Quipu reports as `truncated`, a non-2xx reply, or a missing
//! `rows` field is an ERROR — never an empty rig — because "zero beads" and
//! "the read failed" are indistinguishable once the run is over.
//!
//! ## Change feed
//!
//! [`changes_page`] pulls ONE bounded page of `/changes` for the board graph
//! and reduces it to the set of item ids it touched plus the next cursor. The
//! caller re-indexes exactly those ids through the existing single-bead path
//! (`fetch_bead`), which removes an item that became closed, deleted or an
//! inbox pointer. The feed is a hint to re-read canonical state; record bodies
//! are never indexed directly.
//!
//! Comments are not projected yet: seeds stores them as separate subjects and
//! this slice keeps the read bounded to item facts. `include_comments` is
//! therefore ignored for this source, and the source label says so.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

use super::keys::{bead_visible, rig_of};
use super::{bead_excluded, rows_to_chunks, BeadRow, CommentRow, LiveBeadMetadata};
use crate::config::BeadsConfig;
use crate::types::Chunk;

/// Subject prefix of every seeds work item.
pub(super) const ITEM_PREFIX: &str = "https://seeds.local/item/";
/// Principal IRI prefix; stripped so assignee/owner read as bare names.
const PRINCIPAL_PREFIX: &str = "https://seeds.local/principal/";

/// The graph a database's board lives in: the configured override, else the
/// seeds convention `https://seeds.local/project/<rig>`.
pub(super) fn graph_for(config: &BeadsConfig, db_name: &str) -> String {
    match config.seeds.graphs.get(db_name) {
        Some(g) if !g.trim().is_empty() => g.clone(),
        _ => format!("https://seeds.local/project/{}", rig_of(db_name)),
    }
}

fn base_url(config: &BeadsConfig) -> Result<&str> {
    let url = config.seeds.url.trim().trim_end_matches('/');
    if url.is_empty() {
        bail!(
            "beads source = \"seeds\" but [beads.seeds] url is empty \
             (set [beads.seeds] url = \"http://quipu.svc\")"
        );
    }
    Ok(url)
}

/// A SPARQL string literal; ids come from config or the feed, never trusted.
fn literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One SELECT per item page. Optional fields use OPTIONAL; multi-valued
/// keywords are folded per item by [`fold_rows`], not by GROUP_CONCAT, so a
/// label containing a separator cannot split.
pub(super) fn items_query(after: Option<&str>, only: Option<&[String]>, limit: usize) -> String {
    let mut q = String::from(
        "PREFIX s: <https://schema.org/> \
         PREFIX sd: <https://seeds.local/ontology/> \
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
         SELECT ?id ?title ?description ?status ?priority ?type ?assignee ?owner \
         ?notes ?created ?keyword WHERE { ",
    );
    if let Some(ids) = only {
        q.push_str("VALUES ?id { ");
        for id in ids {
            q.push_str(&literal(id));
            q.push(' ');
        }
        q.push_str("} ");
    }
    q.push_str(
        "?s s:identifier ?id ; sd:status ?status . \
         OPTIONAL { ?s rdfs:label ?title } \
         OPTIONAL { ?s s:description ?description } \
         OPTIONAL { ?s sd:priority ?priority } \
         OPTIONAL { ?s sd:issueType ?type } \
         OPTIONAL { ?s s:agent ?assignee } \
         OPTIONAL { ?s s:creator ?owner } \
         OPTIONAL { ?s sd:notes ?notes } \
         OPTIONAL { ?s s:dateCreated ?created } \
         OPTIONAL { ?s s:keywords ?keyword } ",
    );
    if let Some(a) = after {
        q.push_str(&format!("FILTER(STR(?id) > {}) ", literal(a)));
    }
    q.push_str(&format!("}} ORDER BY ?id LIMIT {limit}"));
    q
}

/// A binding's lexical form: plain JSON scalar or Quipu's `{value, datatype}`.
fn lexical(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Object(o) => o
            .get("value")
            .or_else(|| o.get("ref"))
            .and_then(|x| lexical(Some(x))),
        _ => None,
    }
}

fn principal(v: Option<&Value>) -> Option<String> {
    lexical(v).map(|s| s.strip_prefix(PRINCIPAL_PREFIX).unwrap_or(&s).to_string())
}

/// One seeds item as projected for indexing.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct SeedItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: i32,
    pub issue_type: String,
    pub assignee: Option<String>,
    pub owner: String,
    pub notes: String,
    pub created_at: Option<String>,
    pub labels: Vec<String>,
}

/// Fold SPARQL rows (one per item × keyword) into items, in row order.
///
/// Returns the folded items and the last id seen, which is the keyset cursor
/// for the next page. An item's rows may straddle a page boundary when it has
/// many keywords; the caller re-reads that id rather than trusting a partial.
pub(super) fn fold_rows(rows: &[Value]) -> (Vec<SeedItem>, Option<String>) {
    let mut items: Vec<SeedItem> = Vec::new();
    let mut slot: HashMap<String, usize> = HashMap::new();
    let mut last = None;
    for row in rows {
        let Some(id) = lexical(row.get("id")) else {
            continue;
        };
        last = Some(id.clone());
        let i = *slot.entry(id.clone()).or_insert_with(|| {
            items.push(SeedItem {
                id: id.clone(),
                title: lexical(row.get("title")).unwrap_or_default(),
                description: lexical(row.get("description")).unwrap_or_default(),
                status: lexical(row.get("status")).unwrap_or_default(),
                priority: lexical(row.get("priority"))
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(2),
                issue_type: lexical(row.get("type")).unwrap_or_else(|| "task".into()),
                assignee: principal(row.get("assignee")).filter(|a| !a.is_empty()),
                owner: principal(row.get("owner")).unwrap_or_default(),
                notes: lexical(row.get("notes")).unwrap_or_default(),
                created_at: lexical(row.get("created")),
                labels: Vec::new(),
            });
            items.len() - 1
        });
        if let Some(k) = lexical(row.get("keyword")) {
            if !items[i].labels.contains(&k) {
                items[i].labels.push(k);
            }
        }
    }
    (items, last)
}

async fn post_query(base: &str, graph: &str, query: String) -> Result<Vec<Value>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("building HTTP client for seeds board")?;
    let resp = client
        .post(format!("{base}/query"))
        .json(&serde_json::json!({ "query": query, "graph": graph, "verbose": true }))
        .send()
        .await
        .with_context(|| format!("POST /query for seeds graph {graph}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!(
            "seeds board query for {graph} returned HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    parse_query_reply(&text, graph)
}

/// Rows of a `/query` reply, refusing anything that is not provably complete.
pub(super) fn parse_query_reply(text: &str, graph: &str) -> Result<Vec<Value>> {
    let v: Value = serde_json::from_str(text)
        .with_context(|| format!("parsing seeds board reply for {graph}"))?;
    if v.get("truncated").and_then(Value::as_bool) == Some(true) {
        bail!("seeds board query for {graph} was truncated; refusing a partial board");
    }
    match v.get("rows").and_then(Value::as_array) {
        Some(rows) => Ok(rows.clone()),
        None => bail!("seeds board reply for {graph} has no rows field"),
    }
}

/// Read every item of one graph (or only `ids`), page by page.
async fn read_items(
    config: &BeadsConfig,
    graph: &str,
    ids: Option<&[String]>,
) -> Result<Vec<SeedItem>> {
    let base = base_url(config)?;
    let page = config.seeds.page_size.max(1);
    let mut out: Vec<SeedItem> = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let rows = post_query(base, graph, items_query(after.as_deref(), ids, page)).await?;
        let full = rows.len() >= page;
        let (mut items, last) = fold_rows(&rows);
        if full && items.len() > 1 {
            // The last item may be cut mid-keywords; re-read it next page.
            items.pop();
            after = items.last().map(|i| i.id.clone());
        } else if full {
            bail!(
                "seeds item {} in {graph} has more keyword rows than seeds.page_size={page}",
                last.unwrap_or_default()
            );
        }
        out.extend(items);
        if !full {
            return Ok(out);
        }
    }
}

/// Fetch one rig's beads from its live seeds graph, optionally one id.
///
/// `only_id` narrows WITHOUT relaxing visibility, as on the other backends,
/// so `index-bead <id>` removes an item that closed or became a pointer.
pub(super) async fn fetch_from_graph(
    config: &BeadsConfig,
    db_name: &str,
    only_id: Option<&str>,
) -> Result<Vec<Chunk>> {
    let graph = graph_for(config, db_name);
    let only: Option<Vec<String>> = only_id.map(|i| vec![i.to_string()]);
    let items = read_items(config, &graph, only.as_deref()).await?;
    Ok(items_to_chunks(config, db_name, items))
}

pub(super) fn items_to_chunks(
    config: &BeadsConfig,
    db_name: &str,
    items: Vec<SeedItem>,
) -> Vec<Chunk> {
    let comments: Vec<CommentRow> = Vec::new();
    let mut labels_by_issue: HashMap<String, Vec<String>> = HashMap::new();
    let mut issues = Vec::new();
    for it in items {
        if !bead_visible(config, &it.status, it.created_at.as_deref()) {
            continue;
        }
        if bead_excluded(&it.labels, &config.exclude_labels) {
            continue;
        }
        if !it.labels.is_empty() {
            labels_by_issue.insert(it.id.clone(), it.labels.clone());
        }
        issues.push(BeadRow {
            id: it.id,
            title: it.title,
            description: it.description,
            status: it.status,
            priority: it.priority,
            assignee: it.assignee,
            notes: it.notes,
            metadata: String::new(),
        });
    }
    rows_to_chunks(
        rig_of(db_name),
        issues,
        &comments,
        &labels_by_issue,
        &config.exclude_labels,
    )
}

/// Live metadata for named beads, read from the board graph.
pub(super) async fn fetch_bead_metadata(
    config: &BeadsConfig,
    bead_ids: &[(String, String)],
) -> Result<HashMap<String, LiveBeadMetadata>> {
    let mut by_db: HashMap<String, Vec<String>> = HashMap::new();
    for (rig, id) in bead_ids {
        let db = format!("beads_{rig}");
        if config.databases.contains(&db) {
            by_db.entry(db).or_default().push(id.clone());
        }
    }
    let mut out = HashMap::new();
    for (db, ids) in by_db {
        for it in read_items(config, &graph_for(config, &db), Some(&ids)).await? {
            out.insert(
                it.id.clone(),
                LiveBeadMetadata {
                    status: it.status,
                    priority: it.priority,
                    assignee: it.assignee,
                    title: it.title,
                    issue_type: it.issue_type,
                    owner: it.owner,
                    labels: it.labels,
                    created_at: it.created_at.and_then(|t| t.get(..10).map(str::to_string)),
                },
            );
        }
    }
    Ok(out)
}

/// One bounded page of the board's change feed, reduced to touched item ids.
#[derive(Debug, PartialEq)]
pub struct ChangesPage {
    /// Item ids whose facts changed in this page (deduplicated, sorted).
    pub item_ids: BTreeSet<String>,
    /// Cursor for the next page (`since` of the following call).
    pub next_tx: i64,
    /// Latest committed tx Quipu reported for the feed.
    pub watermark_tx: i64,
}

/// Reduce a `/changes` reply to item ids and cursors. A reply without
/// `records`/`next_tx` is an error, never "no changes".
pub fn parse_changes(text: &str) -> Result<ChangesPage> {
    let v: Value = serde_json::from_str(text).context("parsing seeds change feed page")?;
    let records = v
        .get("records")
        .and_then(Value::as_array)
        .context("seeds change feed page has no records field")?;
    let next_tx = v
        .get("next_tx")
        .and_then(Value::as_i64)
        .context("seeds change feed page has no next_tx")?;
    let watermark_tx = v
        .get("watermark_tx")
        .and_then(Value::as_i64)
        .unwrap_or(next_tx);
    let item_ids = records
        .iter()
        .filter_map(|r| r.get("entity").and_then(Value::as_str))
        .filter_map(|e| e.strip_prefix(ITEM_PREFIX))
        // Comment/sub-resource subjects hang off the item IRI; map to the item.
        .map(|rest| rest.split(['/', '#']).next().unwrap_or(rest).to_string())
        .filter(|id| !id.is_empty())
        .collect();
    Ok(ChangesPage {
        item_ids,
        next_tx,
        watermark_tx,
    })
}

/// Pull one page of the board's change feed after `since`.
pub async fn changes_page(
    config: &BeadsConfig,
    db_name: &str,
    since: i64,
    limit: usize,
) -> Result<ChangesPage> {
    let base = base_url(config)?;
    let graph = graph_for(config, db_name);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("building HTTP client for seeds change feed")?;
    let resp = client
        .get(format!("{base}/changes"))
        .query(&[
            ("since", since.to_string()),
            ("limit", limit.max(1).to_string()),
            ("capture", "new_values".to_string()),
            ("graph", graph.clone()),
        ])
        .send()
        .await
        .with_context(|| format!("GET /changes for seeds graph {graph}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!(
            "seeds change feed for {graph} returned HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    parse_changes(&text)
}

#[cfg(test)]
#[path = "seeds_tests.rs"]
mod tests;
