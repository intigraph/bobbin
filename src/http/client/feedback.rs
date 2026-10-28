//! Feedback wire types and the injection-items call (aegis-gshu.2).
use anyhow::{Context, Result};
use serde::Deserialize;

use super::Client;

/// A feedback record from the server
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct FeedbackRecord {
    pub id: i64,
    pub injection_id: String,
    pub timestamp: String,
    pub agent: String,
    pub rating: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iri: Option<String>,
}

/// Per-entity usage of cited memory items, from the server.
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct EntityUsage {
    pub iri: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub injected: u64,
    #[serde(default)]
    pub used: u64,
    #[serde(default)]
    pub useful: u64,
    #[serde(default)]
    pub noise: u64,
    #[serde(default)]
    pub harmful: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
}

/// Aggregated feedback statistics from the server
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct FeedbackStats {
    pub total_injections: u64,
    pub total_feedback: u64,
    pub useful: u64,
    pub noise: u64,
    pub harmful: u64,
    #[serde(default)]
    pub used: u64,
    #[serde(default)]
    pub item_feedback: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_entities: Vec<EntityUsage>,
    #[serde(default)]
    pub actioned: u64,
    #[serde(default)]
    pub unactioned: u64,
    #[serde(default)]
    pub lineage_records: u64,
}

/// Feedback statistics grouped by bundle or bead
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct GroupedStatsEntry {
    pub key: String,
    pub injections: u64,
    pub feedback: u64,
    pub useful: u64,
    pub noise: u64,
    pub harmful: u64,
}

impl Client {
    /// Log an injection's cited memory items on the server (best-effort).
    pub async fn store_injection_items(
        &self,
        injection_id: &str,
        items: &[crate::memory_recall::CitedItem],
    ) -> Result<()> {
        let url = format!(
            "{}/injections/{}/items",
            self.base_url,
            urlencoding::encode(injection_id)
        );
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "items": items }))
            .send()
            .await
            .context("Failed to store injection items")?;
        if !resp.status().is_success() {
            anyhow::bail!("Failed to store injection items: HTTP {}", resp.status());
        }
        Ok(())
    }
}
