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
//! See [`feed`]: one bounded `/changes` page reduced to touched item ids.
//!
//! Comments are not projected yet: seeds stores them as separate subjects and
//! this slice keeps the read bounded to item facts. `include_comments` is
//! therefore ignored for this source, and the source label says so.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;

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

const PREFIXES: &str = "PREFIX s: <https://schema.org/> \
     PREFIX sd: <https://seeds.local/ontology/> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// Keyword rows allowed per item of a page before the read refuses.
pub(super) const KEYWORDS_PER_ITEM: usize = 64;

fn values_ids(ids: &[String]) -> String {
    let mut q = String::from("VALUES ?id { ");
    for id in ids {
        q.push_str(&literal(id));
        q.push(' ');
    }
    q.push_str("} ");
    q
}

/// One SELECT per batch of known ids: exactly ONE row per item, so `LIMIT`
/// bounds items, not a cross product of multi-valued OPTIONALs. Seeds writes each
/// scalar fact once per item; `SAMPLE` makes that assumption explicit and
/// `?n_*` counts let [`fold_rows`] refuse an item that breaks it. Keywords
/// are read by [`keywords_query`], not GROUP_CONCAT, so a label containing a
/// separator cannot split.
pub(super) fn items_query(ids: &[String]) -> String {
    let mut q = String::from(PREFIXES);
    q.push_str(
        "SELECT ?id (SAMPLE(?status_) AS ?status) (COUNT(DISTINCT ?status_) AS ?n_status) \
         (SAMPLE(?title_) AS ?title) (COUNT(DISTINCT ?title_) AS ?n_title) \
         (SAMPLE(?description_) AS ?description) \
         (COUNT(DISTINCT ?description_) AS ?n_description) \
         (SAMPLE(?priority_) AS ?priority) (SAMPLE(?type_) AS ?type) \
         (SAMPLE(?assignee_) AS ?assignee) (SAMPLE(?owner_) AS ?owner) \
         (SAMPLE(?notes_) AS ?notes) (COUNT(DISTINCT ?notes_) AS ?n_notes) \
         (SAMPLE(?created_) AS ?created) WHERE { ",
    );
    q.push_str(&values_ids(ids));
    q.push_str(
        "?s s:identifier ?id ; sd:status ?status_ . \
         OPTIONAL { ?s rdfs:label ?title_ } \
         OPTIONAL { ?s s:description ?description_ } \
         OPTIONAL { ?s sd:priority ?priority_ } \
         OPTIONAL { ?s sd:issueType ?type_ } \
         OPTIONAL { ?s s:agent ?assignee_ } \
         OPTIONAL { ?s s:creator ?owner_ } \
         OPTIONAL { ?s sd:notes ?notes_ } \
         OPTIONAL { ?s s:dateCreated ?created_ } ",
    );
    q.push_str(&format!(
        "}} GROUP BY ?id ORDER BY ?id LIMIT {}",
        ids.len().max(1)
    ));
    q
}

/// One keyset page of item ids. Aggregating over the whole graph before
/// `LIMIT` exceeds quipu's query timeout, so enumeration pages bare ids and
/// [`items_query`] aggregates only that page through `VALUES`.
pub(super) fn id_page_query(after: Option<&str>, limit: usize) -> String {
    let filter = after
        .map(|a| format!("FILTER(STR(?id) > {}) ", literal(a)))
        .unwrap_or_default();
    format!(
        "{PREFIXES}SELECT ?id WHERE {{ ?s s:identifier ?id ; sd:status ?st . {filter}}} \
         ORDER BY ?id LIMIT {limit}"
    )
}

/// Keywords of exactly the page's items, bounded one row past `cap` so a
/// full reply is detectable as "more than cap", never silently cut.
pub(super) fn keywords_query(ids: &[String], cap: usize) -> String {
    format!(
        "{PREFIXES}SELECT ?id ?keyword WHERE {{ {}?s s:identifier ?id ; s:keywords ?keyword . }} \
         ORDER BY ?id ?keyword LIMIT {}",
        values_ids(ids),
        cap + 1
    )
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

/// Fold [`items_query`] rows (one per item) into items, in row order.
///
/// Refuses an item whose status, title, description or notes is
/// multi-valued: `SAMPLE` would otherwise index an arbitrary one of them.
pub(super) fn fold_rows(rows: &[Value]) -> Result<Vec<SeedItem>> {
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(id) = lexical(row.get("id")) else {
            continue;
        };
        for field in ["status", "title", "description", "notes"] {
            let n = lexical(row.get(format!("n_{field}").as_str()))
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0);
            if n > 1 {
                bail!("seeds item {id} has {n} {field} values; refusing to pick one");
            }
        }
        let priority = match lexical(row.get("priority")) {
            None => 2,
            Some(p) => p.parse().unwrap_or_else(|_| {
                tracing::warn!(item = %id, priority = %p, "unparseable seeds priority; using 2");
                2
            }),
        };
        let issue_type = lexical(row.get("type")).unwrap_or_else(|| {
            tracing::debug!(item = %id, "seeds item has no issueType; using task");
            "task".into()
        });
        items.push(SeedItem {
            title: lexical(row.get("title")).unwrap_or_default(),
            description: lexical(row.get("description")).unwrap_or_default(),
            status: lexical(row.get("status")).unwrap_or_default(),
            priority,
            issue_type,
            assignee: principal(row.get("assignee")).filter(|a| !a.is_empty()),
            owner: principal(row.get("owner")).unwrap_or_default(),
            notes: lexical(row.get("notes")).unwrap_or_default(),
            created_at: lexical(row.get("created")),
            labels: Vec::new(),
            id,
        });
    }
    Ok(items)
}

/// Attach [`keywords_query`] rows to their items; more than `cap` rows is
/// an error, because the reply was cut at `cap + 1`.
pub(super) fn attach_keywords(items: &mut [SeedItem], rows: &[Value], cap: usize) -> Result<()> {
    if rows.len() > cap {
        bail!("seeds keyword read exceeded {cap} rows for one page; refusing partial labels");
    }
    let slot: HashMap<String, usize> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.id.clone(), i))
        .collect();
    for row in rows {
        let (Some(id), Some(k)) = (lexical(row.get("id")), lexical(row.get("keyword"))) else {
            continue;
        };
        if let Some(&i) = slot.get(&id) {
            if !items[i].labels.contains(&k) {
                items[i].labels.push(k);
            }
        }
    }
    Ok(())
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
    // Only an explicit `truncated: false` proves completeness; a missing
    // field is as unproven as `true` (same rule as quipu's share builder).
    if v.get("truncated").and_then(Value::as_bool) != Some(false) {
        bail!(
            "seeds board query for {graph} did not report truncated=false; \
             refusing a possibly partial board"
        );
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
    if let Some(ids) = ids {
        for batch in ids.chunks(page) {
            out.extend(read_batch(base, graph, batch).await?);
        }
        return Ok(out);
    }
    let mut after: Option<String> = None;
    loop {
        let rows = post_query(base, graph, id_page_query(after.as_deref(), page)).await?;
        let batch: Vec<String> = rows.iter().filter_map(|r| lexical(r.get("id"))).collect();
        if !batch.is_empty() {
            out.extend(read_batch(base, graph, &batch).await?);
        }
        if rows.len() < page || batch.is_empty() {
            return Ok(out);
        }
        after = batch.last().cloned();
    }
}

/// Item facts plus keywords for one batch of ids (at most `page_size`).
async fn read_batch(base: &str, graph: &str, ids: &[String]) -> Result<Vec<SeedItem>> {
    let mut items = fold_rows(&post_query(base, graph, items_query(ids)).await?)?;
    if !items.is_empty() {
        let cap = items.len().saturating_mul(KEYWORDS_PER_ITEM);
        let kw = post_query(base, graph, keywords_query(ids, cap)).await?;
        attach_keywords(&mut items, &kw, cap)?;
    }
    Ok(items)
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

#[path = "seeds_feed.rs"]
mod feed;
pub use feed::{changes_page, parse_changes, ChangesPage};

#[cfg(test)]
#[path = "seeds_tests.rs"]
mod tests;
