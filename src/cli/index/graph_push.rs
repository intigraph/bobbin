use anyhow::Context;

pub(super) fn require(pushed: anyhow::Result<(i64, usize)>) -> anyhow::Result<(i64, usize)> {
    pushed.context("chunk-graph publication incomplete: dropped_pushes=1; refusing a success exit")
}

/// Read the complete current file graph, including files skipped by the parser.
/// Walk membership excludes deleted/ignored files and non-file index sources.
/// Repository scoping must apply to both chunks and edges.
#[cfg(feature = "knowledge")]
pub(super) async fn snapshot(
    store: &crate::storage::VectorStore,
    repo: &str,
    files: &std::collections::HashSet<String>,
) -> anyhow::Result<(Vec<crate::types::Chunk>, Vec<crate::types::ChunkEdge>)> {
    let mut files: Vec<_> = files.iter().collect();
    files.sort();
    let mut chunks = Vec::new();
    let mut edges = Vec::new();
    for file in files {
        let mut stored = store.get_chunks_for_file(file, Some(repo)).await?;
        for chunk in &mut stored {
            // The serializer needs the observed heading level even for unchanged
            // files. Keep only its bounded ATX syntax, never title/body text.
            chunk.content = if chunk.chunk_type == crate::types::ChunkType::Section {
                crate::knowledge::chunks::heading_depth::section_heading_prefix(&chunk.content)
                    .unwrap_or("")
                    .to_owned()
            } else {
                String::new()
            };
        }
        chunks.extend(stored);
        edges.extend(store.get_chunk_edges(file, Some(repo)).await?);
    }
    edges.sort_by_cached_key(|edge| serde_json::to_string(edge).expect("serializable edge"));
    Ok((chunks, edges))
}

/// Refuse to publish a repository into a named graph when this index has already
/// published it to ROOT at the same destination.
///
/// Snapshot replacement is GRAPH-scoped (it retracts only the target graph's prior facts
/// under this producer key), so moving a ROOT-published repository to a named graph would
/// leave its whole ROOT snapshot behind as an orphaned copy, silently (review on
/// bobbin#148). Retract the ROOT snapshot first, or keep publishing to ROOT. The check can
/// only see publishes this index recorded; a repository published to ROOT from a
/// different index is not visible here.
#[cfg(feature = "knowledge")]
fn refuse_root_to_graph_copy(
    metadata: &crate::storage::MetadataStore,
    root_key: &str,
    repo: &str,
    target_graph: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(graph) = target_graph {
        if metadata.get_meta(root_key)?.is_some() {
            anyhow::bail!(
                "refusing to publish {repo} into <{graph}>: this index already published it to \
                 ROOT at the same destination, and snapshot replacement is graph-scoped, so the \
                 ROOT copy would be left behind. Retract the ROOT snapshot first, or keep ROOT."
            );
        }
    }
    Ok(())
}

#[cfg(feature = "knowledge")]
pub(super) async fn publish(
    graph: (&[crate::types::Chunk], &[crate::types::ChunkEdge]),
    repo: &str,
    root: &std::path::Path,
    (endpoint, target_graph): (Option<&str>, Option<&str>),
    quiet: bool,
    metadata: &crate::storage::MetadataStore,
    force: bool,
) -> anyhow::Result<(i64, usize)> {
    use sha2::{Digest, Sha256};
    let (chunks, edges) = graph;
    let destination = endpoint
        .map(|e| e.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| {
            root.join(quipu::QuipuConfig::load(root).store_path)
                .to_string_lossy()
                .into_owned()
        });
    // The target graph is part of the destination: moving a repository from ROOT to a
    // named graph (or between graphs) must publish, not read as "unchanged". ROOT keeps
    // its original key shape so existing ROOT dedupe records stay valid.
    let root_key = format!(
        "quipu_snapshot_success:v1:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&(repo, &destination))?))
    );
    refuse_root_to_graph_copy(metadata, &root_key, repo, target_graph)?;
    let key = match target_graph {
        None => root_key,
        Some(graph) => format!(
            "quipu_snapshot_success:v1:{}",
            hex::encode(Sha256::digest(serde_json::to_vec(&(
                repo,
                destination,
                graph
            ))?))
        ),
    };
    let turtle = crate::knowledge::chunks::generate_chunk_turtle(chunks, edges, repo);
    let hash = hex::encode(Sha256::digest(turtle.as_bytes()));
    let payload_bytes = turtle.len();
    drop(turtle);
    if !force && metadata.get_meta(&key)?.as_deref() == Some(hash.as_str()) {
        return Ok((0, 0));
    }
    let started = std::time::Instant::now();
    let pushed = if let Some(endpoint) = endpoint {
        if !quiet {
            println!(
                "  Publishing {} chunks and {} edges ({} bytes) to remote Quipu...",
                chunks.len(),
                edges.len(),
                payload_bytes
            );
        }
        crate::knowledge::chunks::push_chunks_to_remote_quipu(
            chunks,
            edges,
            repo,
            endpoint,
            target_graph,
        )
        .await
    } else {
        crate::knowledge::chunks::push_chunks_to_quipu(chunks, edges, repo, root, target_graph)
    }?;
    // A failed or indeterminate publication must remain eligible on the next
    // run even when file hashes were already committed by local indexing.
    metadata.set_meta(&key, &hash)?;
    if !quiet {
        println!(
            "  Published chunk snapshot: {} bytes in {} ms",
            payload_bytes,
            started.elapsed().as_millis()
        );
    }
    Ok(pushed)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "knowledge")]
    #[test]
    fn refuses_moving_a_root_published_repo_into_a_named_graph() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = crate::storage::MetadataStore::open(&dir.path().join("meta.db")).unwrap();
        let root_key = "quipu_snapshot_success:v1:test-root-key";
        // Never published to ROOT: a graph target is fine (the gascity case).
        super::refuse_root_to_graph_copy(&metadata, root_key, "r", Some("urn:g")).unwrap();
        // ROOT keeps publishing to ROOT regardless.
        metadata.set_meta(root_key, "hash").unwrap();
        super::refuse_root_to_graph_copy(&metadata, root_key, "r", None).unwrap();
        // Already on ROOT: moving to a graph would orphan the ROOT copy, so refuse.
        let err = super::refuse_root_to_graph_copy(&metadata, root_key, "r", Some("urn:g"))
            .expect_err("a ROOT-published repo must not be copied into a graph");
        assert!(err.to_string().contains("left behind"), "{err}");
    }

    use super::*;

    #[test]
    fn failure_reports_drop_and_fails_the_run() {
        let err = require(Err(anyhow::anyhow!("POST /knot timed out")))
            .expect_err("a dropped graph push must fail the index command");

        assert_eq!(
            format!("{err:#}"),
            "chunk-graph publication incomplete: dropped_pushes=1; refusing a success exit: POST /knot timed out"
        );
    }

    #[cfg(feature = "knowledge")]
    #[tokio::test]
    async fn snapshot_preserves_heading_levels_without_retaining_document_text() {
        use crate::storage::VectorStore;
        use crate::types::{Chunk, ChunkType};
        let dir = tempfile::tempdir().unwrap();
        let mut store = VectorStore::open_with_dim(&dir.path().join("vectors"), 2)
            .await
            .unwrap();
        let section = |id: &str, name: &str, content: &str| Chunk {
            id: id.into(),
            file_path: "README.md".into(),
            chunk_type: ChunkType::Section,
            name: Some(name.into()),
            start_line: 1,
            end_line: 3,
            content: content.into(),
            language: "markdown".into(),
            tags: String::new(),
        };
        store
            .insert(
                &[
                    section(
                        "a",
                        "Before you start",
                        "## Before you start\nPRIVATE_BODY_A",
                    ),
                    section("b", "Top > Deep", "   #### Deep\nPRIVATE_BODY_B"),
                ],
                &[vec![1.0, 0.0], vec![0.0, 1.0]],
                &[None, None],
                "r",
                "h",
                "2026-01-01T00:00:00Z",
            )
            .await
            .unwrap();
        let files = ["README.md".to_string()].into_iter().collect();
        let (chunks, edges) = super::snapshot(&store, "r", &files).await.unwrap();
        let turtle = crate::knowledge::chunks::generate_chunk_turtle(&chunks, &edges, "r");
        let section_block = |anchor: &str| {
            let start = turtle.find(&format!("README.md#{anchor}> a")).unwrap();
            turtle[start..].split("\n\n").next().unwrap().to_string()
        };
        assert!(
            section_block("before-you-start").contains("bobbin:headingDepth \"2\"^^xsd:integer")
        );
        assert!(section_block("deep").contains("bobbin:headingDepth \"4\"^^xsd:integer"));
        assert!(chunks
            .iter()
            .all(|chunk| chunk.content.len() <= 9
                && chunk.content.chars().all(|c| c == ' ' || c == '#')));
        assert!(!turtle.contains("PRIVATE_BODY"));
    }

    #[cfg(feature = "knowledge")]
    #[tokio::test]
    async fn replacement_retains_unchanged_files_and_removes_deleted_files() {
        use crate::storage::VectorStore;
        use crate::types::{Chunk, ChunkEdge, ChunkEdgeType, ChunkType};
        let dir = tempfile::tempdir().unwrap();
        let mut store = VectorStore::open_with_dim(&dir.path().join("vectors"), 2)
            .await
            .unwrap();
        let chunk = |id: &str, file: &str, name: &str| Chunk {
            id: id.into(),
            file_path: file.into(),
            chunk_type: ChunkType::Function,
            name: Some(name.into()),
            start_line: 1,
            end_line: 2,
            content: "private source bytes".into(),
            language: "rust".into(),
            tags: String::new(),
        };
        let original = vec![chunk("a", "a.rs", "unchanged"), chunk("b", "b.rs", "old")];
        store
            .insert(
                &original,
                &[vec![1.0, 0.0], vec![1.0, 0.0]],
                &[None, None],
                "r",
                "h",
                "2026-01-01T00:00:00Z",
            )
            .await
            .unwrap();
        let foreign = chunk("other", "a.rs", "foreign");
        store
            .insert(
                &[foreign],
                &[vec![0.0, 1.0]],
                &[None],
                "other",
                "h",
                "2026-01-01T00:00:00Z",
            )
            .await
            .unwrap();
        let edge = ChunkEdge {
            source_chunk: "a".into(),
            target_chunk: "a".into(),
            source_name: "unchanged".into(),
            target_name: "unchanged".into(),
            edge_type: ChunkEdgeType::NextChunk,
            file_path: "a.rs".into(),
        };
        store.upsert_chunk_edges(&[edge], "r").await.unwrap();
        let files = ["a.rs".to_string(), "b.rs".to_string()]
            .into_iter()
            .collect();
        let (chunks, edges) = snapshot(&store, "r", &files).await.unwrap();
        crate::knowledge::chunks::push_chunks_to_quipu(&chunks, &edges, "r", dir.path(), None)
            .unwrap();

        // Only b.rs is reparsed. The unchanged a.rs graph must survive replacement.
        store
            .delete_by_file(&["b.rs".into()], Some("r"))
            .await
            .unwrap();
        store
            .insert(
                &[chunk("b2", "b.rs", "changed")],
                &[vec![1.0, 0.0]],
                &[None],
                "r",
                "h2",
                "2026-01-02T00:00:00Z",
            )
            .await
            .unwrap();
        let (chunks, edges) = snapshot(&store, "r", &files).await.unwrap();
        assert_eq!(
            chunks.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["a", "b2"]
        );
        assert!(chunks.iter().all(|c| c.content.is_empty()));
        assert_eq!(edges.len(), 1, "unchanged-file edges must survive too");
        // Control: the previous changed-file-only payload removes a.rs.
        crate::knowledge::chunks::push_chunks_to_quipu(&chunks[1..], &[], "r", dir.path(), None)
            .unwrap();
        let graph = quipu::Store::open(dir.path().join(".bobbin/quipu/quipu.db").to_str().unwrap())
            .unwrap();
        let unchanged = graph
            .lookup(&crate::iri::symbol_iri("r", "a.rs", "unchanged"))
            .unwrap()
            .unwrap();
        assert!(!graph
            .current_facts()
            .unwrap()
            .iter()
            .any(|f| f.entity == unchanged));
        drop(graph);
        // A no-change retry can recover the complete snapshot from persisted rows.
        let (chunks, edges) = snapshot(&store, "r", &files).await.unwrap();
        crate::knowledge::chunks::push_chunks_to_quipu(&chunks, &edges, "r", dir.path(), None)
            .unwrap();
        let graph = quipu::Store::open(dir.path().join(".bobbin/quipu/quipu.db").to_str().unwrap())
            .unwrap();
        let kept = graph
            .lookup(&crate::iri::symbol_iri("r", "a.rs", "unchanged"))
            .unwrap()
            .unwrap();
        assert!(graph
            .current_facts()
            .unwrap()
            .iter()
            .any(|f| f.entity == kept));
        drop(graph);

        // Walker excludes a deleted file even if stale local rows still exist.
        let only_b = ["b.rs".to_string()].into_iter().collect();
        let (chunks, edges) = snapshot(&store, "r", &only_b).await.unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(edges.is_empty());
        crate::knowledge::chunks::push_chunks_to_quipu(&chunks, &edges, "r", dir.path(), None)
            .unwrap();
        let graph = quipu::Store::open(dir.path().join(".bobbin/quipu/quipu.db").to_str().unwrap())
            .unwrap();
        assert!(!graph
            .current_facts()
            .unwrap()
            .iter()
            .any(|f| f.entity == kept));
    }
}
