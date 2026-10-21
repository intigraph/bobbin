//! Native parity controls and an opt-in disposable-store timing experiment.
use super::*;
use std::time::Instant;

fn edge(path: &str, number: usize) -> ChunkEdge {
    ChunkEdge {
        source_chunk: format!("source-{number}"),
        target_chunk: format!("target-{number}"),
        source_name: "source".into(),
        target_name: "target".into(),
        edge_type: ChunkEdgeType::NextChunk,
        file_path: path.into(),
    }
}

async fn snapshot(store: &VectorStore) -> Vec<(String, String, String, String)> {
    let table = store.chunk_edges_table.as_ref().unwrap();
    let count = table.count_rows(None).await.unwrap();
    assert!(count < 10_000);
    let batches: Vec<RecordBatch> = table
        .query()
        .limit(10_000)
        .execute()
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let mut rows = Vec::new();
    for batch in &batches {
        let ids = string_column(batch, "source_chunk").unwrap();
        let paths = string_column(batch, "file_path").unwrap();
        let repos = string_column(batch, "repo").unwrap();
        let payload = VectorStore::batches_to_chunk_edges(std::slice::from_ref(batch)).unwrap();
        for i in 0..batch.num_rows() {
            rows.push((
                ids.value(i).into(),
                paths.value(i).into(),
                repos.value(i).into(),
                serde_json::to_string(&payload[i]).unwrap(),
            ));
        }
    }
    assert_eq!(rows.len(), count);
    rows.sort();
    rows
}

async fn compare(number: usize) {
    let root = tempfile::tempdir().unwrap();
    let mut serial = VectorStore::open(&root.path().join("serial"))
        .await
        .unwrap();
    let mut batch = VectorStore::open(&root.path().join("batch")).await.unwrap();
    let repo = "fixture's-repository";
    let paths: Vec<String> = (0..number).map(|i| format!("src/fixture'{i}.rs")).collect();
    let mut target_edges: Vec<_> = paths.iter().enumerate().map(|(i, p)| edge(p, i)).collect();
    target_edges.push(edge("src/keep.rs", number));
    target_edges.push(edge("/legacy/absolute.rs", number + 1));
    let foreign: Vec<_> = paths.iter().enumerate().map(|(i, p)| edge(p, i)).collect();
    for store in [&mut serial, &mut batch] {
        store.upsert_chunk_edges(&target_edges, repo).await.unwrap();
        store.upsert_chunk_edges(&foreign, "foreign").await.unwrap();
    }
    assert_eq!(snapshot(&serial).await, snapshot(&batch).await);
    println!(
        "JXG_INPUT {}",
        serde_json::json!({"paths": paths, "repo": repo, "target": target_edges, "foreign": foreign})
    );

    let serial_before = serial
        .chunk_edges_table
        .as_ref()
        .unwrap()
        .version()
        .await
        .unwrap();
    let start = Instant::now();
    serial.clear_absolute_path_chunk_edges().await.unwrap();
    for path in &paths {
        serial
            .clear_file_chunk_edges(path, Some(repo))
            .await
            .unwrap();
    }
    let serial_seconds = start.elapsed().as_secs_f64();
    let serial_commits = serial
        .chunk_edges_table
        .as_ref()
        .unwrap()
        .version()
        .await
        .unwrap()
        - serial_before;

    let batch_before = batch
        .chunk_edges_table
        .as_ref()
        .unwrap()
        .version()
        .await
        .unwrap();
    let start = Instant::now();
    batch.clear_absolute_path_chunk_edges().await.unwrap();
    batch
        .clear_files_chunk_edges(&paths, Some(repo))
        .await
        .unwrap();
    let batch_seconds = start.elapsed().as_secs_f64();
    let batch_commits = batch
        .chunk_edges_table
        .as_ref()
        .unwrap()
        .version()
        .await
        .unwrap()
        - batch_before;
    assert_eq!(serial_commits, number as u64 + 1);
    assert_eq!(batch_commits, number.div_ceil(100) as u64 + 1);
    let serial_rows = snapshot(&serial).await;
    let batch_rows = snapshot(&batch).await;
    assert_eq!(serial_rows, batch_rows);
    assert_eq!(serial_rows.len(), number + 1);
    assert_eq!(
        serial_rows.iter().filter(|r| r.2 == "foreign").count(),
        number
    );
    assert_eq!(serial_rows.iter().filter(|r| r.2 == repo).count(), 1);
    assert!(serial_rows
        .iter()
        .any(|r| r.1 == "src/keep.rs" && r.2 == repo));
    assert!(serial
        .get_chunk_edges(&paths[0], Some(repo))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        batch
            .get_chunk_edges(&paths[0], Some("foreign"))
            .await
            .unwrap()
            .len(),
        1
    );
    // Empty selection must issue no delete and preserve the complete result.
    let empty: Vec<String> = Vec::new();
    let before_empty = batch
        .chunk_edges_table
        .as_ref()
        .unwrap()
        .version()
        .await
        .unwrap();
    batch
        .clear_files_chunk_edges(&empty, Some(repo))
        .await
        .unwrap();
    assert_eq!(
        batch
            .chunk_edges_table
            .as_ref()
            .unwrap()
            .version()
            .await
            .unwrap(),
        before_empty
    );
    assert_eq!(snapshot(&batch).await, batch_rows);
    println!(
        "JXG_RESULT {}",
        serde_json::json!({"files":number,"serial_delete_commits":serial_commits,"batch_delete_commits":batch_commits,"serial_seconds":serial_seconds,"batch_seconds":batch_seconds,"rows_equal":true,"remaining":batch_rows.len(),"fixture_only":true})
    );
}

#[tokio::test]
#[ignore = "bounded private Lance experiment; explicit invocation only, no model or production I/O"]
async fn private_serial_vs_batched_edge_clear() {
    tokio::time::timeout(std::time::Duration::from_secs(300), async {
        compare(17).await;
        compare(1179).await;
    })
    .await
    .expect("bounded private benchmark exceeded five minutes");
}

#[tokio::test]
async fn batched_edge_clear_matches_serial_across_boundaries() {
    compare(17).await;
    compare(205).await;
}

#[tokio::test]
async fn batched_edge_clear_empty_missing_table_and_unscoped() {
    let root = tempfile::tempdir().unwrap();
    let mut store = VectorStore::open(&root.path().join("edges")).await.unwrap();
    let paths = vec!["src/quoted' OR true.rs".to_string()];
    store.clear_files_chunk_edges(&[], None).await.unwrap();
    store
        .clear_files_chunk_edges(&paths, Some("absent"))
        .await
        .unwrap();
    assert!(store
        .get_chunk_edges(&paths[0], None)
        .await
        .unwrap()
        .is_empty());
    let mut selected = edge(&paths[0], 0);
    selected.edge_type = ChunkEdgeType::Implements;
    let mut kept = edge("keep.rs", 1);
    kept.edge_type = ChunkEdgeType::Tests;
    for repo in ["one", "two"] {
        store
            .upsert_chunk_edges(&[selected.clone(), kept.clone()], repo)
            .await
            .unwrap();
    }
    let before = store
        .chunk_edges_table
        .as_ref()
        .unwrap()
        .version()
        .await
        .unwrap();
    store.clear_files_chunk_edges(&[], None).await.unwrap();
    assert_eq!(
        store
            .chunk_edges_table
            .as_ref()
            .unwrap()
            .version()
            .await
            .unwrap(),
        before
    );
    store.clear_files_chunk_edges(&paths, None).await.unwrap();
    assert!(store
        .get_chunk_edges(&paths[0], None)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store.get_chunk_edges("keep.rs", None).await.unwrap(),
        vec![kept.clone(), kept]
    );
}
