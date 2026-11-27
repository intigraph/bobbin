//! Seeds board change feed (`/changes` for the board graph).
//!
//! [`changes_page`] pulls ONE bounded page of `/changes` for the board graph
//! and reduces it to the set of item ids it touched plus the next cursor. The
//! caller re-indexes exactly those ids through the existing single-bead path
//! (`fetch_bead`), which removes an item that became closed, deleted or an
//! inbox pointer. The feed is a hint to re-read canonical state; record bodies
//! are never indexed directly.
//!
//! Feed semantics, observed live on quipu.svc 2026-11-27: `since` is
//! EXCLUSIVE (`since=X` returns tx > X), `next_tx` is the last tx in the page
//! and is the next call's `since`, and `limit` is honoured at whole-tx
//! granularity (one tx can return thousands of records). Comment subjects are
//! `https://seeds.local/item/<id>/comment/<n>` and map back to `<id>`.
//!
//! STATUS: [`changes_page`] has no in-tree caller yet. It is the public read
//! half for the feed consumer (durable received/applied cursor) that lands
//! in a later slice of aegis-8dda.4; until then it is unused API.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeSet;

use super::{base_url, graph_for, ITEM_PREFIX};
use crate::config::BeadsConfig;

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
