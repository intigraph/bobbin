//! The seam between "a thing that produces chunks" and the index pipeline.
//!
//! Five source integrations grew as bespoke blocks inside `cli/index.rs`
//! (files, git commits, beads, archives, PDFs-inside-files), each
//! re-implementing fetch/hash/delete/insert. This module extracts the two
//! reusable lifecycles so a new non-filesystem source (SQL, logs, metrics)
//! is one trait impl and a config block, not another copy of the pipeline:
//!
//! - [`ChunkSource`] + [`index_hashed_source`] — the beads content-hash
//!   incremental with removal sweep. Beads, SQL rows, and (since the W4.P1
//!   follow-up) archive records run through it.
//! - [`WatermarkSource`] + [`index_watermark_source`] — the append-only
//!   watermark increment. Git commits run through it: history only grows,
//!   so one stored watermark replaces per-item hashes and no removal sweep
//!   applies.
//!
//! The file source deliberately stays bespoke: it streams per-file, builds
//! contextual-embedding windows, and owns the deletion sweep against the
//! walk — a genuinely different lifecycle.

use anyhow::Result;

use crate::index::Embedder;
use crate::storage::{MetadataStore, VectorStore};
use crate::types::Chunk;

/// A non-filesystem producer of chunks with stable per-item identity.
pub trait ChunkSource {
    /// Display name for progress output.
    fn name(&self) -> &str;

    /// Repo key that namespaces both the LanceDB rows and the hash
    /// bookkeeping — distinct from any source repo's name so the corpus
    /// is shared across index runs without touching repo incremental state.
    fn repo_key(&self) -> &str;

    /// Source label stamped on inserted rows (e.g. "beads", "sql").
    fn source_label(&self) -> &str;

    /// Fetch the CURRENT full corpus. Each chunk must carry a stable
    /// `id` and `file_path` (the per-item key hashes are stored under).
    fn fetch(&self) -> impl std::future::Future<Output = Result<Vec<Chunk>>> + Send;
}

/// Outcome of one incremental source-index pass, for the caller to report.
pub struct SourceIndexReport {
    /// Chunks (re)embedded and inserted this pass.
    pub indexed: usize,
    /// Chunks skipped because their content hash was unchanged.
    pub unchanged: usize,
    /// Previously indexed items that disappeared from the corpus.
    pub removed: usize,
}

/// Content-hash incremental indexing with a removal sweep — the beads
/// pattern, generalized:
///
/// 1. fetch the full current corpus;
/// 2. (re)embed only items whose content hash changed;
/// 3. delete items that vanished since the last pass;
/// 4. persist hashes only after a successful insert, so a failed run
///    retries rather than silently skipping.
pub async fn index_hashed_source<S: ChunkSource>(
    source: &S,
    vector_store: &mut VectorStore,
    metadata_store: &MetadataStore,
    embedder: &Embedder,
    force: bool,
) -> Result<SourceIndexReport> {
    index_hashed_source_strict(source, vector_store, metadata_store, embedder, force, false).await
}

/// Strict source lifecycle for checkpointed generations. Ordinary callers retain
/// their historical best-effort behavior; a generation must surface store errors.
pub async fn index_hashed_source_strict<S: ChunkSource>(
    source: &S,
    vector_store: &mut VectorStore,
    metadata_store: &MetadataStore,
    embedder: &Embedder,
    force: bool,
    strict: bool,
) -> Result<SourceIndexReport> {
    let chunks = source.fetch().await?;
    let repo_key = source.repo_key();
    let now = chrono::Utc::now().timestamp().to_string();

    if force {
        require_if_strict(metadata_store.clear_file_hashes(repo_key), strict)?;
    }

    let mut to_index = Vec::new();
    let mut current_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut new_hashes: Vec<(String, String)> = Vec::new();
    for c in &chunks {
        current_paths.insert(c.file_path.clone());
        let h = content_hash(&c.content);
        let stored = metadata_store.get_file_hash(repo_key, &c.file_path);
        let stored = if strict {
            stored?
        } else {
            stored.ok().flatten()
        };
        let unchanged = stored.as_deref() == Some(h.as_str());
        if !unchanged {
            to_index.push(c.clone());
        }
        new_hashes.push((c.file_path.clone(), h));
    }

    let indexed = metadata_store.get_all_indexed_files(repo_key);
    let indexed = if strict {
        indexed?
    } else {
        indexed.unwrap_or_default()
    };
    let removed: Vec<String> = indexed
        .into_iter()
        .filter(|p| !current_paths.contains(p))
        .collect();
    if !removed.is_empty() {
        require_if_strict(
            vector_store.delete_by_file(&removed, Some(repo_key)).await,
            strict,
        )?;
        require_if_strict(
            metadata_store.delete_file_hashes(Some(repo_key), &removed),
            strict,
        )?;
    }

    if to_index.is_empty() {
        return Ok(SourceIndexReport {
            indexed: 0,
            unchanged: chunks.len(),
            removed: removed.len(),
        });
    }

    let embed_texts: Vec<String> = to_index.iter().map(|c| c.content.clone()).collect();
    let embed_refs: Vec<&str> = embed_texts.iter().map(|s| s.as_str()).collect();
    let embeddings = embedder.embed_batch(&embed_refs).await?;
    let contexts = vec![None; to_index.len()];

    // Replace just the changed chunks.
    let ids: Vec<String> = to_index.iter().map(|c| c.id.clone()).collect();
    require_if_strict(vector_store.delete(&ids).await, strict)?;
    vector_store
        .insert(
            &to_index,
            &embeddings,
            &contexts,
            repo_key,
            source.source_label(),
            &now,
        )
        .await?;

    // Persist hashes only after a successful insert.
    let hash_refs: Vec<(&str, &str)> = new_hashes
        .iter()
        .map(|(p, h)| (p.as_str(), h.as_str()))
        .collect();
    require_if_strict(
        metadata_store.set_file_hashes_bulk(repo_key, &hash_refs),
        strict,
    )?;

    Ok(SourceIndexReport {
        indexed: to_index.len(),
        unchanged: chunks.len() - to_index.len(),
        removed: removed.len(),
    })
}

/// An append-only producer of chunks tracked by a single watermark rather
/// than per-item hashes.
///
/// Git commits are the archetype: history only grows, so "everything newer
/// than the watermark" is the whole increment, unchanged items never need
/// re-checking, and no removal sweep applies. A rewritten history is the
/// `force` path's job — it refetches and replaces the full corpus.
pub trait WatermarkSource {
    /// Display name for progress output.
    fn name(&self) -> &str;

    /// Repo the inserted rows belong to. Unlike hashed sources, a
    /// watermarked source may live inside an ordinary repo's corpus
    /// (commit chunks sit alongside their repo's file chunks).
    fn repo(&self) -> &str;

    /// Source label stamped on inserted rows (e.g. "git-commits").
    fn source_label(&self) -> &str;

    /// Metadata key the watermark persists under (per-repo — a shared key
    /// was the original commit-corpus defect).
    fn watermark_key(&self) -> String;

    /// Fetch chunks strictly newer than `since` (`None` = the full corpus),
    /// plus the new watermark to persist after a successful insert (`None`
    /// keeps the stored watermark).
    fn fetch_since(&self, since: Option<&str>) -> Result<(Vec<Chunk>, Option<String>)>;
}

/// Watermark incremental indexing — the git-commits pattern, generalized:
///
/// 1. read the stored watermark (`force` ignores it and refetches all);
/// 2. fetch and embed only the increment;
/// 3. on `force`, delete the refetched ids first so the pass replaces
///    rather than duplicates;
/// 4. persist the watermark only after a successful insert, so a failed
///    run retries the same increment rather than silently skipping it.
///
/// Returns the number of chunks indexed.
pub async fn index_watermark_source<S: WatermarkSource>(
    source: &S,
    vector_store: &mut VectorStore,
    metadata_store: &MetadataStore,
    embedder: &Embedder,
    force: bool,
) -> Result<usize> {
    index_watermark_source_strict(source, vector_store, metadata_store, embedder, force, false)
        .await
}

/// Watermark source with strict replacement error propagation for generations.
pub async fn index_watermark_source_strict<S: WatermarkSource>(
    source: &S,
    vector_store: &mut VectorStore,
    metadata_store: &MetadataStore,
    embedder: &Embedder,
    force: bool,
    strict: bool,
) -> Result<usize> {
    let stored = metadata_store.get_meta(&source.watermark_key())?;
    let since = if force { None } else { stored.as_deref() };

    let (chunks, new_watermark) = source.fetch_since(since)?;
    if chunks.is_empty() {
        return Ok(0);
    }

    // Embed the whole increment in one call; `Embedder::embed_batch` owns
    // the slicing (under `force` this is the entire corpus, not an
    // increment — handing it to the model unsliced OOM-killed the nightly).
    let embed_texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
    let embed_refs: Vec<&str> = embed_texts.iter().map(|s| s.as_str()).collect();
    let embeddings = embedder.embed_batch(&embed_refs).await?;
    let contexts = vec![None; chunks.len()];
    let now = chrono::Utc::now().timestamp().to_string();

    // A force pass refetches everything, so replace rather than duplicate.
    if force {
        let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
        require_if_strict(vector_store.delete(&ids).await, strict)?;
    }

    vector_store
        .insert(
            &chunks,
            &embeddings,
            &contexts,
            source.repo(),
            source.source_label(),
            &now,
        )
        .await?;

    if let Some(watermark) = new_watermark {
        metadata_store.set_meta(&source.watermark_key(), &watermark)?;
    }

    Ok(chunks.len())
}

/// What one single-item pass did, for the caller to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOutcome {
    /// (Re)embedded and inserted.
    Indexed,
    /// Content hash unchanged — nothing re-embedded.
    Unchanged,
    /// Gone from the source and swept out of the index.
    Removed,
    /// Gone from the source and not in the index either — nothing to do.
    Absent,
}

/// Index ONE item of a hashed source, addressed by its stable key.
///
/// The single-item counterpart to [`index_hashed_source`], for a post-write
/// trigger that knows exactly which item changed and must not pay for a full
/// corpus fetch.
///
/// **It deliberately does not reuse [`index_hashed_source`], and that is the
/// whole point of it existing.** That function's removal sweep deletes every
/// previously indexed key absent from the fetch it was handed. Handing it a
/// one-item fetch would therefore delete the entire rest of the corpus on
/// every single-bead reindex — silently, since the sweep is the same code path
/// that legitimately removes vanished items. So the sweep is narrowed here to
/// the one key the caller named, and nothing else in `repo_key` is touched.
///
/// `chunk: None` means the item is gone from the source. That is not an error
/// and not a no-op: it removes the item from the index, which is how a bead
/// that was just closed (and so no longer passes the visibility rules) leaves
/// the corpus at the moment its post-write hook fires.
///
/// Hashes are persisted only after a successful insert, matching the batch
/// path, so a failed run retries rather than recording a lie.
pub async fn index_hashed_item(
    repo_key: &str,
    source_label: &str,
    key: &str,
    chunk: Option<&Chunk>,
    vector_store: &mut VectorStore,
    metadata_store: &MetadataStore,
    embedder: &Embedder,
    force: bool,
) -> Result<ItemOutcome> {
    let Some(chunk) = chunk else {
        let known = metadata_store
            .get_file_hash(repo_key, key)
            .ok()
            .flatten()
            .is_some();
        if !known {
            return Ok(ItemOutcome::Absent);
        }
        let keys = [key.to_string()];
        vector_store
            .delete_by_file(&keys, Some(repo_key))
            .await
            .ok();
        metadata_store
            .delete_file_hashes(Some(repo_key), &keys)
            .ok();
        return Ok(ItemOutcome::Removed);
    };

    let hash = content_hash(&chunk.content);
    if !force {
        let stored = metadata_store.get_file_hash(repo_key, key).ok().flatten();
        if stored.as_deref() == Some(hash.as_str()) {
            return Ok(ItemOutcome::Unchanged);
        }
    }

    let embeddings = embedder.embed_batch(&[chunk.content.as_str()]).await?;
    let now = chrono::Utc::now().timestamp().to_string();
    let items = [chunk.clone()];

    // Replace rather than duplicate: the chunk id is derived from `key`, so the
    // prior row for this item has exactly this id.
    vector_store.delete(&[chunk.id.clone()]).await.ok();
    vector_store
        .insert(
            &items,
            &embeddings,
            &vec![None; 1],
            repo_key,
            source_label,
            &now,
        )
        .await?;

    metadata_store
        .set_file_hashes_bulk(repo_key, &[(key, hash.as_str())])
        .ok();

    Ok(ItemOutcome::Indexed)
}

/// Stable content hash for change detection (shared by all hashed sources).
pub fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(content.as_bytes()))
}

fn require_if_strict(result: Result<()>, strict: bool) -> Result<()> {
    if strict {
        result
    } else {
        Ok(())
    }
}
