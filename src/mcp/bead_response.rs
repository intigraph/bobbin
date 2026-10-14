//! Shared local MCP bead projection, including snapshot provenance.
use super::server::{clean_bead_snippet, extract_field};
use super::tools::{BeadResultItem, SearchBeadsResponse};
use crate::config::BeadsConfig;
use crate::index::beads::LiveBeadMetadata;
use crate::types::SearchResult;
use std::collections::HashMap;

pub(super) fn build(
    query: String,
    filtered: &[SearchResult],
    metadata: &HashMap<String, LiveBeadMetadata>,
    config: &BeadsConfig,
    compact: bool,
) -> SearchBeadsResponse {
    let results: Vec<BeadResultItem> = filtered
        .iter()
        .map(|r| {
            let parts: Vec<&str> = r.chunk.file_path.splitn(3, ':').collect();
            let rig = if parts.len() >= 2 { parts[1] } else { "" };
            let bead_id = if parts.len() == 3 {
                parts[2]
            } else {
                &r.chunk.file_path
            };

            let match_type = r
                .match_type
                .as_ref()
                .map(|mt| format!("{:?}", mt).to_lowercase())
                .unwrap_or_else(|| "hybrid".to_string());

            if let Some(meta) = metadata.get(bead_id) {
                let snippet = if compact {
                    None
                } else {
                    Some(clean_bead_snippet(&r.chunk.content, 200))
                };

                BeadResultItem {
                    bead_id: bead_id.to_string(),
                    metadata_provenance:
                        crate::index::beads::provenance::BeadMetadataProvenance::for_result(
                            config, rig, true,
                        ),
                    title: meta.title.clone(),
                    priority: format!("P{}", meta.priority),
                    status: meta.status.clone(),
                    issue_type: meta.issue_type.clone(),
                    assignee: meta
                        .assignee
                        .clone()
                        .unwrap_or_else(|| "unassigned".to_string()),
                    owner: meta.owner.clone(),
                    rig: rig.to_string(),
                    labels: meta.labels.clone(),
                    created_at: meta.created_at.clone(),
                    relevance_score: r.score,
                    match_type,
                    snippet,
                }
            } else {
                let content = &r.chunk.content;
                let snippet = if compact {
                    None
                } else {
                    Some(clean_bead_snippet(content, 200))
                };

                BeadResultItem {
                    bead_id: bead_id.to_string(),
                    metadata_provenance:
                        crate::index::beads::provenance::BeadMetadataProvenance::for_result(
                            config, rig, false,
                        ),
                    title: r.chunk.name.clone().unwrap_or_default(),
                    priority: extract_field(content, "Priority: "),
                    status: extract_field(content, "Status: "),
                    issue_type: "task".to_string(),
                    assignee: extract_field(content, "Assignee: "),
                    owner: String::new(),
                    rig: rig.to_string(),
                    labels: Vec::new(),
                    created_at: None,
                    relevance_score: r.score,
                    match_type,
                    snippet,
                }
            }
        })
        .collect();
    SearchBeadsResponse {
        query,
        count: results.len(),
        warnings: crate::index::beads::provenance::search_warnings(),
        results,
    }
}

#[cfg(test)]
#[path = "bead_response_tests.rs"]
mod tests;
