use super::*;
use crate::config::BeadsSource;
use crate::types::{Chunk, ChunkType};

fn indexed(id: &str) -> SearchResult {
    SearchResult {
        chunk: Chunk {
            id: id.into(),
            file_path: format!("beads:example:{id}"),
            chunk_type: ChunkType::Issue,
            name: Some("Retained issue".into()),
            start_line: 0,
            end_line: 0,
            content: "Retained issue\nStatus: open | Priority: P2 | Assignee: former".into(),
            language: "beads".into(),
            tags: String::new(),
        },
        score: 0.5,
        match_type: None,
        indexed_at: Some(1_600_000_000),
        repo: None,
    }
}

#[tokio::test]
async fn frozen_export_enrichment_is_visible_in_compact_mcp_results() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), r#"{"id":"example-1","title":"Retained issue","status":"open","priority":2,"assignee":"former"}"#).unwrap();
    let mut config = BeadsConfig {
        enabled: true,
        source: BeadsSource::Jsonl,
        databases: vec!["beads_example".into()],
        ..Default::default()
    };
    config
        .jsonl_paths
        .insert("beads_example".into(), file.path().to_string_lossy().into());
    let metadata = crate::index::beads::fetch_bead_metadata(
        &config,
        &[("example".into(), "example-1".into())],
    )
    .await
    .unwrap();
    let response = build(
        "retained".into(),
        &[indexed("example-1"), indexed("example-2")],
        &metadata,
        &config,
        true,
    );
    let value = serde_json::to_value(response).unwrap();
    assert_eq!(value["count"], 2);
    assert_eq!(value["results"][0]["assignee"], "former");
    assert!(value["results"][0].get("snippet").is_none());
    let p = &value["results"][0]["metadata_provenance"];
    assert_eq!(p["source"], "jsonl_snapshot");
    assert_eq!(p["as_of"], serde_json::Value::Null);
    assert!(p["export_file_modified_at"].is_string());
    assert_eq!(p["current_board_verified"], false);
    assert!(p["warning"].as_str().unwrap().contains("retired"));
    assert_eq!(
        value["results"][1]["metadata_provenance"]["source"],
        "indexed_content"
    );
    assert!(!value["warnings"].as_array().unwrap().is_empty());
    let empty =
        serde_json::to_value(build("retained".into(), &[], &metadata, &config, true)).unwrap();
    assert_eq!(empty["count"], 0);
    assert!(!empty["warnings"].as_array().unwrap().is_empty());
}
