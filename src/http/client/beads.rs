//! Typed bead transport retains freshness warnings and handles old servers.
use serde::Deserialize;

/// Response from the /beads endpoint
#[derive(Deserialize)]
pub struct SearchBeadsResponse {
    pub query: String,
    pub count: usize,
    #[serde(default = "crate::index::beads::provenance::search_warnings")]
    pub warnings: Vec<String>,
    pub results: Vec<BeadResultItem>,
}

#[derive(Deserialize)]
pub struct BeadResultItem {
    pub bead_id: String,
    #[serde(default)]
    pub metadata_provenance: crate::index::beads::provenance::BeadMetadataProvenance,
    pub title: String,
    pub priority: String,
    pub status: String,
    pub issue_type: String,
    pub assignee: String,
    pub owner: String,
    pub rig: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub created_at: Option<String>,
    pub relevance_score: f32,
    pub match_type: String,
    pub snippet: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_client_preserves_snapshot_provenance_and_marks_legacy_responses() {
        let mut wire = serde_json::json!({"query":"retained", "count":1, "results":[{
            "bead_id":"example-1", "title":"Retained", "priority":"P2", "status":"open",
            "issue_type":"task", "assignee":"former", "owner":"", "rig":"example",
            "created_at":null, "relevance_score":0.5, "match_type":"hybrid", "snippet":null
        }]});
        let old: SearchBeadsResponse = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(old.results[0].metadata_provenance.source, "unreported");
        assert!(!old.results[0].metadata_provenance.current_board_verified);
        assert!(!old.warnings.is_empty());
        let p = crate::index::beads::provenance::BeadMetadataProvenance::for_result(
            &crate::config::BeadsConfig::default(),
            "example",
            false,
        );
        wire["results"][0]["metadata_provenance"] = serde_json::to_value(&p).unwrap();
        wire["warnings"] = serde_json::json!(["snapshot warning"]);
        let current: SearchBeadsResponse = serde_json::from_value(wire).unwrap();
        assert_eq!(current.results[0].metadata_provenance.source, p.source);
        assert_eq!(current.warnings, vec!["snapshot warning"]);
    }
}
