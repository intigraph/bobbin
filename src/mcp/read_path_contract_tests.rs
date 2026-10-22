use super::*;

#[tokio::test]
async fn private_index_definition_is_readable_without_workspace_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    Config::default().save(&Config::config_path(root)).unwrap();
    let relative = "src/example.rs";
    let content = "fn example() { /* requested content */ }";
    let source = root.join("repos/alpha").join(relative);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, content).unwrap();
    let mut store = VectorStore::open(&Config::lance_path(root)).await.unwrap();
    store
        .insert(
            &[crate::types::Chunk {
                id: "private-example".into(),
                file_path: relative.into(),
                chunk_type: ChunkType::Function,
                name: Some("example".into()),
                start_line: 1,
                end_line: 1,
                content: content.into(),
                language: "rust".into(),
                tags: String::new(),
            }],
            &[vec![0.1; 384]],
            &[None],
            "alpha",
            "fixture-hash",
            "2026-01-01T00:00:00Z",
        )
        .await
        .unwrap();
    drop(store);
    let server = BobbinMcpServer::new(root.to_path_buf()).unwrap();
    let result = server
        .find_refs(Parameters(FindRefsRequest {
            symbol: "example".into(),
            repo: Some("alpha".into()),
            r#type: None,
            limit: Some(5),
        }))
        .await
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
    let definition = &value["definition"];
    assert_eq!(definition["file_path"], relative);
    for repo in [None, Some("alpha".to_string())] {
        let result = server
            .read_chunk(Parameters(ReadChunkRequest {
                file: definition["file_path"].as_str().unwrap().into(),
                repo,
                start_line: 1,
                end_line: 1,
                context: None,
            }))
            .await
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
        assert_eq!(value["content"], content);
    }
    assert!(server
        .read_chunk(Parameters(ReadChunkRequest {
            file: relative.into(),
            repo: Some("unknown".into()),
            start_line: 1,
            end_line: 1,
            context: None,
        }))
        .await
        .unwrap_err()
        .message
        .contains("Unknown repository"));
}
