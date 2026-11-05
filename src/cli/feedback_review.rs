//! Agent-invoked maintenance, never part of the per-prompt recall hot path.

use anyhow::{Context, Result};
use serde::Serialize;

use crate::memory_recall::{self as memory, MemoryItem};
use crate::storage::feedback::FeedbackStore;

#[derive(Debug, Serialize)]
pub(super) struct Report {
    mode: &'static str,
    observed: usize,
    considered: usize,
    changes: Vec<Plan>,
}

#[derive(Debug, Serialize)]
struct Plan {
    iri: String,
    graph: String,
    previous_review_after: String,
    review_after: String,
    outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_since: Option<String>,
    last_used: Option<String>,
    #[serde(skip_serializing)]
    update: String,
}

fn iri(value: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(value).context("memory identifier is not an absolute IRI")?;
    anyhow::ensure!(
        !parsed.scheme().is_empty()
            && !value
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "<>\"{}|^`\\".contains(c)),
        "unsafe memory IRI"
    );
    Ok(format!("<{value}>"))
}

fn literal(value: &str) -> String {
    // JSON strings use the same escaping for the plain strings emitted here.
    serde_json::to_string(value).expect("string is serializable")
}

fn review_value(value: &serde_json::Value) -> Option<&str> {
    if let Some(value) = value.as_str() {
        return Some(value);
    }
    if value.get("datatype")?.as_str()? != "http://www.w3.org/2001/XMLSchema#dateTime" {
        return None;
    }
    value.get("value")?.as_str()
}

fn plan(item: &MemoryItem, now: chrono::DateTime<chrono::Utc>) -> Result<Option<Plan>> {
    let Some(previous) = item.review_after.as_deref() else {
        return Ok(None);
    };
    let previous_time =
        chrono::DateTime::parse_from_rfc3339(previous).context("invalid reviewAfter")?;
    let expired = item
        .expires_at
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .context("invalid expiresAt")?
        .is_some_and(|date| date <= now);
    if previous_time <= now || expired {
        return Ok(None);
    }
    chrono::DateTime::parse_from_rfc3339(&item.recorded_at).context("invalid recordedAt")?;
    let plane = match memory::local_name(&item.source_kind)
        .to_ascii_lowercase()
        .as_str()
    {
        "declared" => "declared",
        "records" => "records",
        "observed" => "observed",
        "inferred" => "inferred",
        _ => anyhow::bail!("unknown memory source plane"),
    };
    let graph = format!("{}crew/{plane}", crate::knowledge::quarantine::plane_ns());
    let graph_term = iri(&graph)?;
    let subject = iri(&item.iri)?;
    let key = iri(&item.natural_key)?;
    let ns = crate::iri::ONTOLOGY_NS;
    let date = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let update = format!(
        "DELETE {{ GRAPH {graph_term} {{ {subject} <{ns}reviewAfter> ?old }} }} \
         INSERT {{ GRAPH {graph_term} {{ {subject} <{ns}reviewAfter> {}^^<http://www.w3.org/2001/XMLSchema#dateTime> }} }} \
         WHERE {{ GRAPH {graph_term} {{ \
           {subject} <{ns}naturalKey> {key}; <{ns}recordedAt> ?recorded; <{ns}reviewAfter> ?old . \
           FILTER(STR(?recorded) = {} && STR(?old) = {}) \
           FILTER NOT EXISTS {{ {subject} <{ns}supersededBy> ?new }} \
           FILTER NOT EXISTS {{ ?new <{ns}supersedes> {subject} }} \
           FILTER NOT EXISTS {{ {subject} <{ns}reviewAfter> ?other . FILTER(?other != ?old) }} \
           FILTER NOT EXISTS {{ {subject} <{ns}recordedAt> ?otherRecorded . FILTER(?otherRecorded != ?recorded) }} \
           FILTER NOT EXISTS {{ {subject} <{ns}naturalKey> ?otherKey . FILTER(?otherKey != {key}) }} \
           OPTIONAL {{ {subject} <{ns}expiresAt> ?expiry }} \
           FILTER(!BOUND(?expiry) || ?expiry > {}^^<http://www.w3.org/2001/XMLSchema#dateTime>) \
         }} FILTER NOT EXISTS {{ GRAPH ?otherGraph {{ ?newVersion <{ns}supersedes> {subject} }} }} }}",
        literal(&date), literal(&item.recorded_at), literal(previous), literal(&date),
    );
    Ok(Some(Plan {
        iri: item.iri.clone(),
        graph,
        previous_review_after: previous.into(),
        review_after: date,
        outcome: "preview".into(),
        observed_since: None,
        last_used: None,
        update,
    }))
}

pub(super) async fn run(
    store: &FeedbackStore,
    endpoint: &str,
    apply: bool,
    limit: usize,
) -> Result<Report> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    let url = reqwest::Url::parse(endpoint).context("invalid Quipu endpoint")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "endpoint must be HTTP(S) without credentials, query or fragment"
    );
    let path = memory::mirror_path().context("memory mirror path unavailable")?;
    let _lock = memory::RefreshLock::acquire(&path).context("memory refresh is already running")?;
    let mut mirror = memory::load_mirror(&path).context("memory mirror unavailable")?;
    anyhow::ensure!(
        mirror.endpoint.trim_end_matches('/') == endpoint,
        "memory mirror endpoint differs"
    );
    anyhow::ensure!(
        mirror.last_error.is_none()
            && mirror
                .refreshed_at
                .is_some_and(|time| time <= memory::now_secs())
            && mirror
                .age_secs(memory::now_secs())
                .is_some_and(|age| age <= memory::REFRESH_TTL_SECS),
        "memory mirror is stale or failed; refresh it first"
    );
    store.observe_memory_entities(endpoint, mirror.items.keys().map(String::as_str))?;
    let idle = store.idle_memory_entities(endpoint, limit)?;
    let mut report = Report {
        mode: if apply { "apply" } else { "preview" },
        observed: mirror.items.len(),
        considered: idle.len(),
        changes: Vec::new(),
    };
    let now = chrono::Utc::now();
    for candidate in idle {
        let Some(item) = mirror.items.get(&candidate.iri) else {
            continue;
        };
        if let Some(mut change) = plan(item, now)? {
            change.observed_since = Some(candidate.observed_since);
            change.last_used = candidate.last_used;
            report.changes.push(change);
        }
    }
    if apply && !report.changes.is_empty() {
        let result = apply_changes(endpoint, &mut report.changes).await;
        for change in &report.changes {
            if change.outcome == "applied_verified" {
                if let Some(item) = mirror.items.get_mut(&change.iri) {
                    item.review_after = Some(change.review_after.clone());
                }
            }
        }
        // Keep the refresh lock and preserve cursor/refreshed_at: this was
        // metadata maintenance, not an index refresh or a new memory version.
        memory::save_mirror(&path, &mirror)?;
        result?;
    }
    Ok(report)
}

async fn apply_changes(endpoint: &str, changes: &mut [Plan]) -> Result<()> {
    let token = crate::knowledge::quipu_auth::require(endpoint)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    apply_changes_with(endpoint, changes, &token, &client).await
}

async fn apply_changes_with(
    endpoint: &str,
    changes: &mut [Plan],
    token: &str,
    client: &reqwest::Client,
) -> Result<()> {
    for change in changes {
        crate::knowledge::quipu_auth::check(endpoint)?;
        let response = client
            .post(format!("{endpoint}/update"))
            .bearer_auth(token)
            .header("Content-Type", "application/sparql-update")
            .header("X-Quipu-Client", "bobbin")
            .header("X-Quipu-Purpose", "INGEST")
            .body(change.update.clone())
            .send()
            .await
            .context("review update outcome UNKNOWN; verify graph before retrying")?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(crate::knowledge::quipu_auth::rejected(endpoint));
        }
        anyhow::ensure!(
            response.status().is_success(),
            "review update refused: HTTP {}",
            response.status()
        );
        let receipt: serde_json::Value = response
            .json()
            .await
            .context("review update receipt UNKNOWN; verify graph before retrying")?;
        let graphs = receipt
            .get("graphs")
            .and_then(|g| g.as_array())
            .context("missing graph receipt")?;
        anyhow::ensure!(receipt["asserted"] == 1 && receipt["retracted"] == 1 && graphs.len() == 1
            && graphs[0]["graph"] == change.graph && graphs[0]["asserted"] == 1 && graphs[0]["retracted"] == 1
            && graphs[0].get("tx").and_then(|tx| tx.as_u64()).is_some_and(|tx| tx > 0),
            "review update did not prove one conditional change in the intended graph; verify graph before retrying");
        let query = format!(
            "SELECT ?review WHERE {{ GRAPH {} {{ {} <{}reviewAfter> ?review }} }} LIMIT 2",
            iri(&change.graph)?,
            iri(&change.iri)?,
            crate::iri::ONTOLOGY_NS
        );
        let response = client
            .post(format!("{endpoint}/query"))
            .bearer_auth(token)
            .header("X-Quipu-Client", "bobbin")
            .header("X-Quipu-Purpose", "VERIFY")
            .json(&serde_json::json!({"query":query}))
            .send()
            .await
            .context("review readback UNKNOWN")?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(crate::knowledge::quipu_auth::rejected(endpoint));
        }
        anyhow::ensure!(
            response.status().is_success(),
            "review readback refused: HTTP {}",
            response.status()
        );
        let body: serde_json::Value = bounded_json(response)
            .await
            .context("invalid review readback")?;
        let rows = body
            .get("rows")
            .and_then(|v| v.as_array())
            .context("missing review readback rows")?;
        anyhow::ensure!(
            body.get("error").is_none()
                && rows.len() == 1
                && body.get("count").and_then(|value| value.as_u64()) == Some(1)
                && review_value(&rows[0]["review"]) == Some(change.review_after.as_str())
                && body.get("truncated").and_then(|v| v.as_bool()) == Some(false),
            "review readback did not prove the new date"
        );
        change.outcome = "applied_verified".into();
    }
    // Do not falsify recordedAt or the mirror cursor to propagate metadata.
    Ok(())
}

async fn bounded_json(mut response: reqwest::Response) -> Result<serde_json::Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len().saturating_add(chunk.len()) <= 65_536,
            "review response exceeded 64 KiB"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
#[path = "feedback_review_tests.rs"]
mod tests;
