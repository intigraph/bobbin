//! A reread of an export is not a read of the current tracker.
//! # arming: library HTTP and MCP bead-search responses; no scheduled scan.
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{BeadsConfig, BeadsSource};

pub const INDEX_WARNING: &str = "Bead discovery uses the last indexed corpus, not the current tracker. New or changed beads may be absent; zero results do not establish absence from the active board.";
const SNAPSHOT_WARNING: &str = "Metadata is from an exported JSONL snapshot, not live tracker state. This includes retained exports from retired trackers. Verify status, priority and assignee against the active board before routing work. File modification time does not prove export freshness or current-board authority.";
const STORE_WARNING: &str = "Metadata was queried from the configured database; its authority as the active board has not been verified. A retained retired database can answer successfully. Verify current state with the active tracker before routing work.";
const UNKNOWN_WARNING: &str = "Metadata source and as-of time are unreported. These fields may be stale indexed content or a retired-store snapshot; verify current state with the active tracker before routing work.";

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BeadMetadataProvenance {
    pub source: String,
    /// No backend currently supplies a verified corpus snapshot timestamp.
    pub as_of: Option<String>,
    /// When Bobbin observed the configured source, not when its facts were true.
    pub observed_at: Option<String>,
    pub export_file_modified_at: Option<String>,
    pub current_board_verified: bool,
    pub warning: String,
}

impl Default for BeadMetadataProvenance {
    fn default() -> Self {
        Self {
            source: "unreported".into(),
            as_of: None,
            observed_at: None,
            export_file_modified_at: None,
            current_board_verified: false,
            warning: UNKNOWN_WARNING.into(),
        }
    }
}

impl BeadMetadataProvenance {
    pub fn for_result(config: &BeadsConfig, rig: &str, enriched: bool) -> Self {
        let mut result = Self {
            observed_at: Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
            ..Self::default()
        };
        if !enriched {
            result.source = "indexed_content".into();
            result.warning = format!("{INDEX_WARNING} {UNKNOWN_WARNING}");
        } else {
            match config.source {
                BeadsSource::Jsonl => {
                    result.source = "jsonl_snapshot".into();
                    result.warning = SNAPSHOT_WARNING.into();
                    result.export_file_modified_at = config
                        .jsonl_paths
                        .get(&format!("beads_{rig}"))
                        .and_then(|path| std::fs::metadata(path).ok())
                        .and_then(|meta| meta.modified().ok())
                        .map(|time| {
                            DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Millis, true)
                        });
                }
                BeadsSource::Dolt => {
                    result.source = "configured_database".into();
                    result.warning = STORE_WARNING.into();
                }
            }
        }
        result
    }
}

pub fn search_warnings() -> Vec<String> {
    vec![INDEX_WARNING.into()]
}

/// Preserve remote fields, but don't let an older server omit the warning.
pub fn annotate_remote(mut value: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid bead search response: expected object"))?;
    let results = object
        .get_mut("results")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("invalid bead search response: missing results array"))?;
    let mut unreported = false;
    for item in results {
        let item = item
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("invalid bead search result: expected object"))?;
        if !item.contains_key("metadata_provenance") {
            item.insert(
                "metadata_provenance".into(),
                serde_json::to_value(BeadMetadataProvenance::default())?,
            );
            unreported = true;
        }
    }
    let warnings = object
        .entry("warnings")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid bead search warnings: expected array"))?;
    if !warnings.iter().any(|w| w.as_str() == Some(INDEX_WARNING)) {
        warnings.push(INDEX_WARNING.into());
    }
    if unreported {
        warnings.push(UNKNOWN_WARNING.into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_file_mtime_cannot_make_a_retired_snapshot_authoritative() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            r#"{"id":"old-1","assignee":"former","status":"open"}"#,
        )
        .unwrap();
        let mut config = BeadsConfig {
            enabled: true,
            source: BeadsSource::Jsonl,
            ..Default::default()
        };
        config
            .jsonl_paths
            .insert("beads_example".into(), file.path().to_string_lossy().into());
        let p = BeadMetadataProvenance::for_result(&config, "example", true);
        assert_eq!(p.source, "jsonl_snapshot");
        assert!(p.export_file_modified_at.is_some());
        assert!(p.observed_at.is_some());
        assert!(p.as_of.is_none());
        assert!(!p.current_board_verified);
        assert!(p.warning.contains("retired"));
        assert!(p.warning.contains("not live"));
    }

    #[test]
    fn indexed_or_missing_metadata_never_inherits_configured_source() {
        let config = BeadsConfig {
            source: BeadsSource::Jsonl,
            ..Default::default()
        };
        let p = BeadMetadataProvenance::for_result(&config, "example", false);
        assert_eq!(p.source, "indexed_content");
        assert!(p.as_of.is_none());
        assert!(p.export_file_modified_at.is_none());
        assert!(!p.current_board_verified);
    }

    #[test]
    fn successful_database_query_is_not_active_board_authority() {
        let p = BeadMetadataProvenance::for_result(&BeadsConfig::default(), "example", true);
        assert_eq!(p.source, "configured_database");
        assert!(!p.current_board_verified);
        assert!(p.warning.contains("retired database"));
    }

    #[test]
    fn old_remote_server_gets_explicit_unknown_provenance() {
        let old = serde_json::json!({"count":1,"results":[{"bead_id":"old-1","assignee":"former","extra":"preserved"}]});
        let after = annotate_remote(old).unwrap();
        assert_eq!(after["results"][0]["assignee"], "former");
        assert_eq!(after["results"][0]["extra"], "preserved");
        assert_eq!(
            after["results"][0]["metadata_provenance"]["source"],
            "unreported"
        );
        assert!(after["warnings"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn empty_and_current_remote_responses_keep_warnings_and_provenance() {
        let empty = annotate_remote(serde_json::json!({"count":0,"results":[]})).unwrap();
        assert_eq!(empty["warnings"][0], INDEX_WARNING);
        let p = serde_json::to_value(BeadMetadataProvenance::for_result(
            &BeadsConfig::default(),
            "example",
            false,
        ))
        .unwrap();
        let current =
            serde_json::json!({"results":[{"metadata_provenance":p}],"warnings":["existing"]});
        let after = annotate_remote(current).unwrap();
        assert_eq!(after["results"][0]["metadata_provenance"], p);
        assert_eq!(after["warnings"][0], "existing");
        assert!(annotate_remote(serde_json::json!({"error":"failed"})).is_err());
    }
}
