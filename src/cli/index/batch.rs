//! The file-batch commit boundary shared by normal and resumed indexing.
use crate::storage::{MetadataStore, VectorStore};
use crate::types::Chunk;
use anyhow::Result;

pub(super) struct Batch<'a> {
    pub chunks: &'a [Chunk],
    pub embeddings: &'a [Vec<f32>],
    pub contexts: &'a [Option<String>],
    pub hashes: &'a [&'a str],
    pub timestamps: &'a [&'a str],
    pub file_hashes: &'a [(String, String)],
}

pub(super) async fn persist(
    vectors: &mut VectorStore,
    metadata: &MetadataStore,
    repo: &str,
    batch: Batch<'_>,
) -> Result<()> {
    persist_observed(vectors, metadata, repo, batch, |_| Ok(())).await
}

pub(super) async fn persist_observed(
    vectors: &mut VectorStore,
    metadata: &MetadataStore,
    repo: &str,
    batch: Batch<'_>,
    mut observe: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    // Delete by actual path unconditionally: an interrupted insert may have
    // vectors but no hash, even when this invocation did not create the file.
    let paths = batch
        .file_hashes
        .iter()
        .map(|(p, _)| p.clone())
        .collect::<Vec<_>>();
    vectors.delete_by_file(&paths, Some(repo)).await?;
    observe("AfterBatchDelete")?;
    vectors
        .insert_bulk(
            batch.chunks,
            batch.embeddings,
            batch.contexts,
            repo,
            batch.hashes,
            batch.timestamps,
        )
        .await?;
    observe("AfterVectorInsert")?;
    let hashes = batch
        .file_hashes
        .iter()
        .map(|(p, h)| (p.as_str(), h.as_str()))
        .collect::<Vec<_>>();
    metadata.set_file_hashes_bulk(repo, &hashes)?;
    observe("AfterHashCommit")?;
    Ok(())
}
