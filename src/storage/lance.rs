use anyhow::{Context, Result};
use arrow::array::{
    Array, ArrayRef, BooleanArray, FixedSizeListArray, Float32Array, RecordBatch,
    RecordBatchIterator, StringArray, UInt32Array,
};
use arrow::datatypes::{DataType, Field, FieldRef, Schema, SchemaRef};
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::table::{CompactionOptions, Duration, OptimizeAction};
use lancedb::{connect, Connection, Table};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::types::{
    Chunk, ChunkEdge, ChunkEdgeType, ChunkType, Entity, EntitySearchResult, FileMetadata,
    ImportDependency, IndexStats, LanguageStats, MatchType, SearchResult,
};

/// Table name for chunk storage
const TABLE_NAME: &str = "chunks";

/// Table name for dependency storage
const DEPS_TABLE_NAME: &str = "dependencies";

/// Table name for chunk-level relationship edges
const CHUNK_EDGES_TABLE_NAME: &str = "chunk_edges";

/// Table name for knowledge graph entity embeddings
const ENTITIES_TABLE_NAME: &str = "entities";

/// Limit for queries that need all rows. LanceDB 0.17 defaults to limit=10
/// (DEFAULT_TOP_K) for all queries including plain scans, so we must set an
/// explicit large limit when scanning the full table. Using i64::MAX is safe
/// because the lance scanner casts to i64 internally.
const SCAN_ALL_LIMIT: usize = i64::MAX as usize;

/// Default embedding dimension (for backward compatibility)
const DEFAULT_EMBEDDING_DIM: i32 = 384;

const FTS_RECOVERY_DELAYS_MS: [u64; 3] = [50, 100, 200];

async fn recover_fts_query<T, Q, QF, R, RF, S, SF>(
    mut query: Q,
    mut rebuild: R,
    mut sleep: S,
) -> Result<T>
where
    Q: FnMut() -> QF,
    QF: Future<Output = Result<T>>,
    R: FnMut() -> RF,
    RF: Future<Output = Result<()>>,
    S: FnMut(u64) -> SF,
    SF: Future<Output = ()>,
{
    let first_err = match query().await {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };

    for delay_ms in FTS_RECOVERY_DELAYS_MS {
        // The invalidation may be coming from another process. Waiting before
        // rebuilding is what gives that writer a chance to finish; sleeping
        // after our own failed cycle leaves the first retry just as racy as it
        // was before bounded recovery existed.
        sleep(delay_ms).await;
        if let Err(error) = rebuild().await {
            tracing::warn!(error = %error, delay_ms, "FTS index rebuild attempt failed");
            continue;
        }
        match query().await {
            Ok(value) => return Ok(value),
            Err(error) => tracing::warn!(
                error = %error,
                delay_ms,
                "FTS query still unavailable after index rebuild"
            ),
        }
    }

    Err(first_err).context("Failed to collect FTS results after bounded index recovery")
}

/// Target rows per compacted fragment.
///
/// `CompactionOptions::default()` uses 1,048,576. That default is the whole
/// bug for a store our size: compaction treats every fragment with FEWER rows
/// than this as a candidate, so on a ~152k-row table *every* fragment always
/// qualifies and the target is effectively "rewrite the entire store into one
/// fragment". Peak memory is therefore store-scale BY CONSTRUCTION — it grows
/// with the corpus and never with the amount of new work — which is why the
/// nightly reindex was OOM-killed at a 16G cap, and still at 8G after the
/// store had shrunk to 3.3G. Bounding this makes compaction rewrite a bounded
/// slice at a time instead.
const COMPACT_TARGET_ROWS_PER_FRAGMENT: usize = 65_536;

/// Scanner batch size while reading input fragments during compaction.
/// Unset, lance uses a default tuned for throughput; our rows carry full chunk
/// TEXT (the 3.3G store is ~230MB of vectors and the rest text), so in-flight
/// batches dominate compaction memory.
const COMPACT_SCAN_BATCH_SIZE: usize = 1_024;

/// Compaction options with an explicit memory bound.
///
/// Every knob here exists to keep peak RSS a function of the BATCH, not of the
/// corpus. See [`COMPACT_TARGET_ROWS_PER_FRAGMENT`] for why the library
/// defaults cannot be used on a store of this shape.
fn bounded_compaction_options() -> CompactionOptions {
    CompactionOptions {
        target_rows_per_fragment: COMPACT_TARGET_ROWS_PER_FRAGMENT,
        batch_size: Some(COMPACT_SCAN_BATCH_SIZE),
        // One compaction task at a time. The default is the compute-CPU count,
        // which multiplies peak memory by that factor; on a host shared with
        // other memory-hungry services that multiplier is what turns a large
        // compaction into an OOM of the whole unit.
        num_threads: Some(1),
        ..CompactionOptions::default()
    }
}

/// Cross-process record of the last COMPLETED maintenance sweep, written into
/// the dataset directory beside `.maintenance.lock`.
///
/// It exists because the read-path throttle it feeds was per-PROCESS, and the
/// heaviest contender for the maintenance lock is a short-lived CLI: every
/// `bobbin status` is a fresh process whose in-memory "last compacted" is
/// "never", so every one of them took the store-wide lock. See
/// [`VectorStore::compact_if_stale`].
const MAINTENANCE_STATUS_FILE: &str = ".maintenance.json";

/// How long the read path lets a compaction stay deferred. Shared by the
/// in-process throttle and the cross-process one, so a `bobbin status` storm
/// and a long-lived server obey the same budget.
const READ_PATH_COMPACT_INTERVAL_SECS: u64 = 300;

/// How often to re-try the maintenance lock while waiting for it.
const LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// How a caller wants to acquire the store-wide maintenance lock.
///
/// The distinction is the whole point of this type: the two kinds of caller
/// have opposite correct behaviour, and collapsing them is what let a routine
/// `bobbin status` starve the only job that prunes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockWait {
    /// **Opportunistic** — try once, give up immediately if another
    /// participant holds the lock. Correct for the READ path: a stats query
    /// compacts as a courtesy, and queueing behind someone else's sweep would
    /// make every request pay for it.
    NoWait,
    /// **Scheduled** — poll for the lock up to this long before giving up.
    /// Correct for the reindex/watch maintenance step, which is the ONLY thing
    /// that prunes: it must not be pre-empted by an opportunistic contender.
    UpTo(std::time::Duration),
}

/// What a maintenance sweep actually DID.
///
/// `compact`/`prune` used to return `Ok(())` whether they swept every table or
/// skipped entirely because the lock was held, so a starved nightly was
/// indistinguishable from a successful one — for months. A caller that cannot
/// tell the difference cannot report it, and nothing did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceOutcome {
    /// The lock was held by us and every table was swept.
    Ran,
    /// Another participant held the maintenance lock; NOTHING was reclaimed.
    SkippedLockHeld { waited: std::time::Duration },
    /// The store has no tables open — nothing to sweep.
    NoTables,
}

impl MaintenanceOutcome {
    /// True only when the sweep actually touched the store.
    pub fn ran(self) -> bool {
        matches!(self, Self::Ran)
    }

    /// True when the sweep was starved by a lock contender — the case worth
    /// reporting, because the caller's exit status will not show it.
    pub fn skipped_lock_held(self) -> bool {
        matches!(self, Self::SkippedLockHeld { .. })
    }

    /// Stable machine-readable label for JSON output and logs.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ran => "ran",
            Self::SkippedLockHeld { .. } => "skipped_lock_held",
            Self::NoTables => "no_tables",
        }
    }
}

/// Wall-clock record of the last successful maintenance, read from
/// [`MAINTENANCE_STATUS_FILE`]. Unix seconds; `None` means "never, as far as
/// this store's directory knows".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MaintenanceStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_compact_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prune_unix: Option<u64>,
}

/// Seconds since the unix epoch, or 0 if the clock is before it.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Extract a named string column from a RecordBatch, returning a Result instead of panicking.
fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .with_context(|| format!("missing column '{name}' in RecordBatch"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("column '{name}' is not a StringArray"))
}

/// Unified chunk storage using LanceDB (vectors + metadata + FTS + dependencies)
pub struct VectorStore {
    conn: Connection,
    table: Option<Table>,
    /// Dependency graph table
    deps_table: Option<Table>,
    /// Chunk-level relationship edges table
    chunk_edges_table: Option<Table>,
    /// Knowledge graph entity embeddings table
    entities_table: Option<Table>,
    /// Embedding dimension used by this store
    embedding_dim: i32,
    /// Whether FTS index has been created for this session
    fts_indexed: AtomicBool,
    /// Whole-corpus FTS builds THIS store asked Lance for (the global metric is
    /// `bobbin_fts_build_attempts_total`; this one is per store, for tests).
    fts_build_attempts: AtomicU64,
    /// The dataset directory, for the cross-process maintenance lock file.
    db_path: PathBuf,
    /// Baseline for the read-path compaction throttle.
    opened_at: std::time::Instant,
    /// Seconds-since-open of the last read-path compaction attempt (0 = never).
    last_compact_secs: AtomicU64,
}

/// True for Lance commit conflicts whose own error text says to retry the latest version.
fn is_commit_conflict<E: std::fmt::Display>(err: &E) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("commit conflict") || msg.contains("concurrent commit")
}

/// Narrow discriminator for Lance's FTS incremental-index worker panic.
/// The join boundary is stable across Lance releases; requiring both the
/// inverted-index source path and a worker panic avoids treating ordinary
/// compaction failures as rebuildable index state.
fn is_fts_compaction_panic<E: std::fmt::Display>(err: &E) -> bool {
    let msg = format!("{err:#}");
    msg.contains("scalar/inverted/builder.rs") && msg.contains("panicked")
}

/// Retry a table write that hit a commit conflict: refresh the table handle to
/// the latest version and re-run, at most twice, with a short backoff. Every
/// participant compacts/writes the same tables, so occasional conflicts are
/// expected operation, not errors — before this, the archive-chunk write
/// failed on EVERY incremental run behind a success message.
macro_rules! retry_on_conflict {
    ($table:expr, $op:expr) => {{
        let mut attempt: u32 = 0;
        loop {
            match $op.await {
                Ok(v) => break Ok(v),
                Err(e) if attempt < 2 && is_commit_conflict(&e) => {
                    attempt += 1;
                    let _ = $table.checkout_latest().await;
                    tokio::time::sleep(std::time::Duration::from_millis(100 * u64::from(attempt)))
                        .await;
                }
                Err(e) => break Err(e),
            }
        }
    }};
}

impl VectorStore {
    /// Open or create a vector store at the given path
    pub async fn open(path: &Path) -> Result<Self> {
        Self::open_with_dim(path, DEFAULT_EMBEDDING_DIM).await
    }

    /// Open or create a vector store with a specific embedding dimension
    pub async fn open_with_dim(path: &Path, embedding_dim: i32) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let path_str = path.to_str().context("non-UTF8 path for LanceDB")?;
        let conn = connect(path_str)
            .execute()
            .await
            .with_context(|| format!("Failed to open LanceDB at: {}", path.display()))?;

        // Check if table exists
        let tables = conn
            .table_names()
            .execute()
            .await
            .context("Failed to list tables")?;

        let table = if tables.contains(&TABLE_NAME.to_string()) {
            let t = conn
                .open_table(TABLE_NAME)
                .execute()
                .await
                .context("Failed to open chunks table")?;

            // Schema migration: if the on-disk schema doesn't match the
            // expected schema (e.g. a new column was added), drop the table
            // so the next insert recreates it with the correct schema.
            let table_schema = t
                .schema()
                .await
                .context("Failed to read chunks table schema")?;
            let expected = Self::build_schema(embedding_dim);
            let on_disk: Vec<&str> = table_schema
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();
            let expected_fields: Vec<&str> = expected
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();

            if on_disk != expected_fields {
                eprintln!("bobbin: chunks table schema changed — dropping for re-creation");
                eprintln!("  on-disk fields:  {:?}", on_disk);
                eprintln!("  expected fields: {:?}", expected_fields);
                conn.drop_table(TABLE_NAME, &[])
                    .await
                    .context("Failed to drop outdated chunks table")?;
                None
            } else {
                Some(t)
            }
        } else {
            None
        };

        let deps_table = if tables.contains(&DEPS_TABLE_NAME.to_string()) {
            let t = conn
                .open_table(DEPS_TABLE_NAME)
                .execute()
                .await
                .context("Failed to open dependencies table")?;

            // Same schema migration check for dependencies table
            let table_schema = t
                .schema()
                .await
                .context("Failed to read deps table schema")?;
            let expected = Self::deps_schema();
            let on_disk: Vec<&str> = table_schema
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();
            let expected_fields: Vec<&str> = expected
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();

            if on_disk != expected_fields {
                eprintln!("bobbin: deps table schema changed — dropping for re-creation");
                conn.drop_table(DEPS_TABLE_NAME, &[])
                    .await
                    .context("Failed to drop outdated deps table")?;
                None
            } else {
                Some(t)
            }
        } else {
            None
        };

        let chunk_edges_table = if tables.contains(&CHUNK_EDGES_TABLE_NAME.to_string()) {
            let t = conn
                .open_table(CHUNK_EDGES_TABLE_NAME)
                .execute()
                .await
                .context("Failed to open chunk_edges table")?;

            let table_schema = t
                .schema()
                .await
                .context("Failed to read chunk_edges table schema")?;
            let expected = Self::chunk_edges_schema();
            let on_disk: Vec<&str> = table_schema
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();
            let expected_fields: Vec<&str> = expected
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();

            if on_disk != expected_fields {
                eprintln!("bobbin: chunk_edges table schema changed — dropping for re-creation");
                conn.drop_table(CHUNK_EDGES_TABLE_NAME, &[])
                    .await
                    .context("Failed to drop outdated chunk_edges table")?;
                None
            } else {
                Some(t)
            }
        } else {
            None
        };

        let entities_table = if tables.contains(&ENTITIES_TABLE_NAME.to_string()) {
            let t = conn
                .open_table(ENTITIES_TABLE_NAME)
                .execute()
                .await
                .context("Failed to open entities table")?;

            let table_schema = t
                .schema()
                .await
                .context("Failed to read entities table schema")?;
            let expected = Self::entities_schema(embedding_dim);
            let on_disk: Vec<&str> = table_schema
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();
            let expected_fields: Vec<&str> = expected
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();

            if on_disk != expected_fields {
                eprintln!("bobbin: entities table schema changed — dropping for re-creation");
                conn.drop_table(ENTITIES_TABLE_NAME, &[])
                    .await
                    .context("Failed to drop outdated entities table")?;
                None
            } else {
                Some(t)
            }
        } else {
            None
        };

        Ok(Self {
            conn,
            table,
            deps_table,
            chunk_edges_table,
            entities_table,
            embedding_dim,
            fts_indexed: AtomicBool::new(false),
            fts_build_attempts: AtomicU64::new(0),
            db_path: path.to_path_buf(),
            opened_at: std::time::Instant::now(),
            last_compact_secs: AtomicU64::new(0),
        })
    }

    /// Get the embedding dimension of this store
    pub fn embedding_dim(&self) -> i32 {
        self.embedding_dim
    }

    /// Get the inner field for the vector FixedSizeList
    fn vector_field() -> FieldRef {
        Arc::new(Field::new("item", DataType::Float32, true))
    }

    /// Get the Arrow schema for chunk records
    fn schema(&self) -> Schema {
        Self::build_schema(self.embedding_dim)
    }

    /// Build the Arrow schema for chunk records with a given embedding dimension.
    /// Static so it can be called before `self` is fully constructed (e.g. during
    /// schema migration checks in `open_with_dim`).
    fn build_schema(embedding_dim: i32) -> Schema {
        Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(Self::vector_field(), embedding_dim),
                false,
            ),
            Field::new("repo", DataType::Utf8, false),
            Field::new("file_path", DataType::Utf8, false),
            Field::new("file_hash", DataType::Utf8, false),
            Field::new("language", DataType::Utf8, false),
            Field::new("chunk_type", DataType::Utf8, false),
            Field::new("chunk_name", DataType::Utf8, true),
            Field::new("start_line", DataType::UInt32, false),
            Field::new("end_line", DataType::UInt32, false),
            Field::new("content", DataType::Utf8, false),
            Field::new("full_context", DataType::Utf8, true),
            Field::new("indexed_at", DataType::Utf8, false),
            Field::new("tags", DataType::Utf8, false),
        ])
    }

    /// Convert chunks and embeddings to a RecordBatch
    ///
    /// `full_contexts` contains the context-enriched text used for embedding.
    /// `None` entries mean the chunk was embedded using its content directly.
    fn to_record_batch(
        &self,
        chunks: &[Chunk],
        embeddings: &[Vec<f32>],
        full_contexts: &[Option<String>],
        repo: &str,
        file_hash: &str,
        indexed_at: &str,
    ) -> Result<RecordBatch> {
        let file_hashes: Vec<&str> = chunks.iter().map(|_| file_hash).collect();
        let indexed_ats: Vec<&str> = chunks.iter().map(|_| indexed_at).collect();
        self.to_record_batch_bulk(
            chunks,
            embeddings,
            full_contexts,
            repo,
            &file_hashes,
            &indexed_ats,
        )
    }

    fn to_record_batch_bulk(
        &self,
        chunks: &[Chunk],
        embeddings: &[Vec<f32>],
        full_contexts: &[Option<String>],
        repo: &str,
        file_hashes: &[&str],
        indexed_at_values: &[&str],
    ) -> Result<RecordBatch> {
        let schema = Arc::new(self.schema());

        let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
        let repos: Vec<&str> = chunks.iter().map(|_| repo).collect();
        let file_paths: Vec<&str> = chunks.iter().map(|c| c.file_path.as_str()).collect();
        let languages: Vec<&str> = chunks.iter().map(|c| c.language.as_str()).collect();
        let chunk_types: Vec<&str> = chunks
            .iter()
            .map(|c| chunk_type_to_str(&c.chunk_type))
            .collect();
        let chunk_names: Vec<Option<&str>> = chunks.iter().map(|c| c.name.as_deref()).collect();
        let start_lines: Vec<u32> = chunks.iter().map(|c| c.start_line).collect();
        let end_lines: Vec<u32> = chunks.iter().map(|c| c.end_line).collect();
        let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let full_context_refs: Vec<Option<&str>> =
            full_contexts.iter().map(|c| c.as_deref()).collect();
        let indexed_ats: Vec<&str> = indexed_at_values.to_vec();
        let tags: Vec<&str> = chunks.iter().map(|c| c.tags.as_str()).collect();

        // Flatten embeddings for FixedSizeList
        let flat_embeddings: Vec<f32> = embeddings.iter().flatten().copied().collect();
        let embedding_values: ArrayRef = Arc::new(Float32Array::from(flat_embeddings));
        let vector_array = FixedSizeListArray::try_new(
            Self::vector_field(),
            self.embedding_dim,
            embedding_values,
            None,
        )
        .context("Failed to create vector array")?;

        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(vector_array),
            Arc::new(StringArray::from(repos)),
            Arc::new(StringArray::from(file_paths)),
            Arc::new(StringArray::from(file_hashes.to_vec())),
            Arc::new(StringArray::from(languages)),
            Arc::new(StringArray::from(chunk_types)),
            Arc::new(StringArray::from(chunk_names)),
            Arc::new(UInt32Array::from(start_lines)),
            Arc::new(UInt32Array::from(end_lines)),
            Arc::new(StringArray::from(contents)),
            Arc::new(StringArray::from(full_context_refs)),
            Arc::new(StringArray::from(indexed_ats)),
            Arc::new(StringArray::from(tags)),
        ];

        RecordBatch::try_new(schema, columns).context("Failed to create record batch")
    }

    /// Create a boxed RecordBatchReader from a batch.
    ///
    /// Boxed because lancedb 0.27's `add`/`create_table` take `impl Scannable`,
    /// which is implemented for `Box<dyn RecordBatchReader + Send>` but not for
    /// a bare `RecordBatchIterator`.
    fn batch_to_reader(
        batch: RecordBatch,
        schema: SchemaRef,
    ) -> Box<dyn arrow_array::RecordBatchReader + Send> {
        Box::new(RecordBatchIterator::new(std::iter::once(Ok(batch)), schema))
    }

    /// Insert chunks with their embeddings
    ///
    /// `full_contexts` contains the context-enriched text used for embedding.
    /// `None` entries mean the chunk was embedded using its content directly.
    pub async fn insert(
        &mut self,
        chunks: &[Chunk],
        embeddings: &[Vec<f32>],
        full_contexts: &[Option<String>],
        repo: &str,
        file_hash: &str,
        indexed_at: &str,
    ) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        if chunks.len() != embeddings.len() {
            anyhow::bail!(
                "Chunks and embeddings must have same length: {} vs {}",
                chunks.len(),
                embeddings.len()
            );
        }

        if chunks.len() != full_contexts.len() {
            anyhow::bail!(
                "Chunks and full_contexts must have same length: {} vs {}",
                chunks.len(),
                full_contexts.len()
            );
        }

        let schema = Arc::new(self.schema());
        let batch = self.to_record_batch(
            chunks,
            embeddings,
            full_contexts,
            repo,
            file_hash,
            indexed_at,
        )?;

        match &self.table {
            Some(table) => {
                // Delete existing records with same IDs first (upsert behavior)
                let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
                self.delete(&ids).await?;

                // Add new records (fresh reader per attempt — the reader is
                // consumed by a failed add, and RecordBatch clones are Arc-cheap)
                retry_on_conflict!(
                    table,
                    table
                        .add(Self::batch_to_reader(batch.clone(), schema.clone()))
                        .execute()
                )
                .context("Failed to add chunks")?;
            }
            None => {
                // Create table with initial data
                let reader = Self::batch_to_reader(batch, schema);
                let table = self
                    .conn
                    .create_table(TABLE_NAME, reader)
                    .execute()
                    .await
                    .context("Failed to create chunks table")?;
                self.table = Some(table);
            }
        }

        // Invalidate FTS index since data changed
        self.fts_indexed.store(false, Ordering::Relaxed);

        Ok(())
    }

    /// Bulk insert chunks from multiple files in a single Lance write.
    ///
    /// Unlike `insert`, this accepts per-chunk file hashes to support mixed-file
    /// batches. Callers must pre-delete stale rows (e.g. via `delete_by_file`).
    pub async fn insert_bulk(
        &mut self,
        chunks: &[Chunk],
        embeddings: &[Vec<f32>],
        full_contexts: &[Option<String>],
        repo: &str,
        file_hashes: &[&str],
        indexed_at_values: &[&str],
    ) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        if chunks.len() != embeddings.len()
            || chunks.len() != full_contexts.len()
            || chunks.len() != file_hashes.len()
            || chunks.len() != indexed_at_values.len()
        {
            anyhow::bail!(
                "insert_bulk: length mismatch — chunks={}, embeddings={}, contexts={}, hashes={}, indexed_at={}",
                chunks.len(),
                embeddings.len(),
                full_contexts.len(),
                file_hashes.len(),
                indexed_at_values.len()
            );
        }

        let schema = Arc::new(self.schema());
        let batch = self.to_record_batch_bulk(
            chunks,
            embeddings,
            full_contexts,
            repo,
            file_hashes,
            indexed_at_values,
        )?;

        match &self.table {
            Some(table) => {
                retry_on_conflict!(
                    table,
                    table
                        .add(Self::batch_to_reader(batch.clone(), schema.clone()))
                        .execute()
                )
                .context("Failed to bulk-add chunks")?;
            }
            None => {
                let reader = Self::batch_to_reader(batch, schema);
                let table = self
                    .conn
                    .create_table(TABLE_NAME, reader)
                    .execute()
                    .await
                    .context("Failed to create chunks table")?;
                self.table = Some(table);
            }
        }

        self.fts_indexed.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Try to take the cross-process maintenance lock (advisory flock on a
    /// file inside the dataset directory). `None` means another bobbin
    /// participant — the server, the indexer, a CLI run — is already
    /// compacting/pruning this store, and the correct move is to SKIP:
    /// a second concurrent compaction contributes nothing but the commit
    /// conflict that interrupts the first, refragments the table, and makes
    /// the next compaction longer — the positive feedback loop that took the
    /// search server down and grew the store 2.9G -> 41G.
    /// The lock releases when the returned handle drops.
    fn try_maintenance_lock(&self) -> Option<std::fs::File> {
        use fs4::FileExt;
        let lock_path = self.db_path.join(".maintenance.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .ok()?;
        match file.try_lock_exclusive() {
            Ok(()) => Some(file),
            Err(_) => None,
        }
    }

    /// Acquire the maintenance lock according to `wait`.
    ///
    /// `Err(waited)` means we gave up; `waited` is how long we spent trying,
    /// which is the number a caller needs in order to say something truthful
    /// about the skip.
    ///
    /// The wait is a POLL, not a blocking `flock`, for two reasons: it stays
    /// bounded (a blocking acquire behind a wedged holder is the two-day-hang
    /// failure mode this fleet already has scar tissue for), and it yields to
    /// the async runtime instead of parking a worker thread.
    async fn acquire_maintenance_lock(
        &self,
        wait: LockWait,
    ) -> std::result::Result<std::fs::File, std::time::Duration> {
        let started = std::time::Instant::now();
        let budget = match wait {
            LockWait::NoWait => std::time::Duration::ZERO,
            LockWait::UpTo(d) => d,
        };
        loop {
            if let Some(file) = self.try_maintenance_lock() {
                return Ok(file);
            }
            let waited = started.elapsed();
            if waited >= budget {
                return Err(waited);
            }
            // Never sleep past the budget — a caller that asked for 60s must
            // not be held for 60.5s.
            let remaining = budget - waited;
            tokio::time::sleep(LOCK_POLL_INTERVAL.min(remaining)).await;
        }
    }

    /// Path of the cross-process maintenance status record.
    fn maintenance_status_path(&self) -> PathBuf {
        self.db_path.join(MAINTENANCE_STATUS_FILE)
    }

    /// Last completed maintenance for this store, as recorded on disk by
    /// whichever process last swept it.
    ///
    /// This is the operator-facing signal: "time since last successful
    /// maintenance" is alertable, and unlike a caller's exit status it does not
    /// go green when the sweep was skipped. Best-effort — a missing or
    /// unreadable file reads as "never".
    pub fn maintenance_status(&self) -> MaintenanceStatus {
        std::fs::read_to_string(self.maintenance_status_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Stamp a completed sweep into the status record.
    ///
    /// Only ever called while HOLDING the maintenance lock, which is what makes
    /// the read-modify-write safe across processes without a second lock.
    /// Best-effort: failing to record maintenance must not fail maintenance.
    fn record_maintenance(&self, compacted: bool, pruned: bool) {
        let mut status = self.maintenance_status();
        let now = now_unix();
        if compacted {
            status.last_compact_unix = Some(now);
        }
        if pruned {
            status.last_prune_unix = Some(now);
        }
        if let Ok(json) = serde_json::to_string(&status) {
            let _ = std::fs::write(self.maintenance_status_path(), json);
        }
    }

    /// Compact fragmented data files.
    ///
    /// LanceDB creates a new fragment for every add/delete. With per-file upserts
    /// this leads to heavy fragmentation that hurts read performance. Compaction
    /// merges small fragments into larger ones.
    ///
    /// Memory is bounded by [`bounded_compaction_options`] — with
    /// `CompactionOptions::default()` this OOM-killed the nightly reindex.
    ///
    /// Gated on the maintenance lock: at most ONE compaction runs at a time
    /// across every process sharing the store; contenders skip (Ok) rather
    /// than queue — compaction is best-effort maintenance, and an interrupted
    /// compaction is strictly worse than a deferred one.
    /// Every table this store owns, for maintenance sweeps.
    ///
    /// `compact`/`prune` used to touch ONLY `self.table`, so `dependencies`,
    /// `chunk_edges` and `entities` were never compacted and never pruned — not
    /// once, ever. Their version manifests therefore grew monotonically for the
    /// life of the store: `dependencies` was measured at 280,895 manifest
    /// versions while `chunks`, which does get pruned, sat around 4,000. Any new
    /// table MUST be added here or it silently inherits that unbounded growth.
    fn maintenance_tables(&self) -> Vec<(&'static str, &Table)> {
        [
            ("chunks", self.table.as_ref()),
            ("dependencies", self.deps_table.as_ref()),
            ("chunk_edges", self.chunk_edges_table.as_ref()),
            ("entities", self.entities_table.as_ref()),
        ]
        .into_iter()
        .filter_map(|(name, t)| t.map(|t| (name, t)))
        .collect()
    }

    pub async fn compact(&self, wait: LockWait) -> Result<MaintenanceOutcome> {
        let tables = self.maintenance_tables();
        if tables.is_empty() {
            return Ok(MaintenanceOutcome::NoTables);
        }

        let _lock = match self.acquire_maintenance_lock(wait).await {
            Ok(lock) => lock,
            Err(waited) => return Ok(MaintenanceOutcome::SkippedLockHeld { waited }),
        };

        let r = self.compact_locked(&tables).await;
        // Stamped while still holding the lock, and only for a sweep that
        // actually ran — a skip must never look like maintenance.
        self.record_maintenance(true, false);
        r.map(|()| MaintenanceOutcome::Ran)
    }

    /// The full SCHEDULED sweep — prune, then compact — under a SINGLE
    /// acquisition of the maintenance lock.
    ///
    /// Two reasons this is not just `prune().await; compact().await`:
    ///
    /// 1. **One wait, not two.** The nightly runs one `index` per repo (27 of
    ///    them here), and each waits for the lock. Charging each of prune and
    ///    compact its own full wait budget doubles the worst case for every
    ///    repo, and the second wait is nearly always redundant — a contender
    ///    that held the lock through the first will usually hold it through the
    ///    second.
    /// 2. **No steal window.** Between two separate acquisitions an
    ///    opportunistic contender can take the lock, so the run could prune and
    ///    then silently fail to compact. Holding it across both makes the
    ///    prune-before-compact ordering actually hold end to end.
    pub async fn maintain(&self, wait: LockWait) -> Result<MaintenanceOutcome> {
        let tables = self.maintenance_tables();
        if tables.is_empty() {
            return Ok(MaintenanceOutcome::NoTables);
        }

        let _lock = match self.acquire_maintenance_lock(wait).await {
            Ok(lock) => lock,
            Err(waited) => return Ok(MaintenanceOutcome::SkippedLockHeld { waited }),
        };

        // PRUNE FIRST. Prune is the cheap reclaim (it drops version manifests
        // and unreferenced fragment files without reading data into RAM);
        // compact rewrites rows and is the one that can die on memory. With
        // compact first, a compaction that fails takes the cheap reclaim down
        // with it and the next compaction is bigger — self-perpetuating.
        let pruned = self.prune_locked(&tables).await;
        let compacted = self.compact_locked(&tables).await;
        // THEN fold rows added since each index was built into it
        // (aegis-4h7zw8). Lance never adds new rows to an existing index on its
        // own: it text-scans them on every FTS query, so without this the
        // keyword leg slows with every incremental reindex. A failure here
        // leaves search exactly as it was (slow, still correct), so it is
        // reported loudly but does not fail the sweep.
        if let Err(e) = self.optimize_indices_locked(&tables).await {
            tracing::warn!(error = %e, "index optimize failed; unindexed rows stay text-scanned");
            eprintln!("bobbin: index optimize failed (search stays correct but slower): {e:#}");
        }
        self.record_maintenance(compacted.is_ok(), pruned.is_ok());
        pruned.and(compacted).map(|()| MaintenanceOutcome::Ran)
    }

    /// Compact every table. CALLER MUST HOLD the maintenance lock.
    ///
    /// One table's failure must not skip the rest — they are independent
    /// datasets and a partial sweep still reclaims.
    async fn compact_locked(&self, tables: &[(&'static str, &Table)]) -> Result<()> {
        let mut first_err = None;
        for (name, table) in tables {
            let mut r = retry_on_conflict!(
                table,
                table.optimize(OptimizeAction::Compact {
                    options: bounded_compaction_options(),
                    remap_options: None,
                })
            )
            .with_context(|| format!("Failed to compact {name} table"));

            // Lance's incremental FTS-index remap can panic while compacting
            // the chunks table. A full replacement build is the upstream-safe
            // recovery path: it discards the broken incremental generation,
            // after which the same bounded compaction can proceed. Never apply
            // this to unrelated compaction errors (I/O, schema, OOM, etc.).
            if *name == "chunks" && r.as_ref().err().is_some_and(|e| is_fts_compaction_panic(e)) {
                tracing::warn!(
                    error = %r.as_ref().expect_err("checked above"),
                    "FTS incremental compaction panicked; rebuilding the FTS index and retrying once"
                );
                if let Err(rebuild_err) = self.rebuild_fts_index().await {
                    r = Err(rebuild_err
                        .context("Failed to rebuild FTS index after incremental compaction panic"));
                } else {
                    r = retry_on_conflict!(
                        table,
                        table.optimize(OptimizeAction::Compact {
                            options: bounded_compaction_options(),
                            remap_options: None,
                        })
                    )
                    .with_context(|| "Failed to compact chunks table after FTS rebuild");
                }
            }
            if let Err(e) = r {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Prune every table. CALLER MUST HOLD the maintenance lock.
    async fn prune_locked(&self, tables: &[(&'static str, &Table)]) -> Result<()> {
        let mut first_err = None;
        for (name, table) in tables {
            let r = retry_on_conflict!(
                table,
                table.optimize(OptimizeAction::Prune {
                    older_than: Some(Duration::try_hours(1).expect("valid delta")),
                    delete_unverified: Some(true),
                    error_if_tagged_old_versions: None,
                })
            )
            .with_context(|| format!("Failed to prune old versions of {name} table"));
            if let Err(e) = r {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Remove old dataset versions to reclaim disk space.
    /// Removes versions older than 1 hour by default.
    /// Gated on the maintenance lock exactly like [`Self::compact`].
    pub async fn prune(&self, wait: LockWait) -> Result<MaintenanceOutcome> {
        let tables = self.maintenance_tables();
        if tables.is_empty() {
            return Ok(MaintenanceOutcome::NoTables);
        }

        let _lock = match self.acquire_maintenance_lock(wait).await {
            Ok(lock) => lock,
            Err(waited) => return Ok(MaintenanceOutcome::SkippedLockHeld { waited }),
        };

        let r = self.prune_locked(&tables).await;
        self.record_maintenance(false, true);
        r.map(|()| MaintenanceOutcome::Ran)
    }

    /// Read-path compaction throttle: attempt a compaction at most once per
    /// interval per process. `get_stats` is called by every search / grep /
    /// status surface, and compacting on each call meant a long compaction on
    /// a fragmented table ran inline in EVERY request while other
    /// participants interrupted it. Between attempts, a slightly
    /// stale scan beats an unavailable server.
    /// The read path is also throttled ACROSS processes, because the in-process
    /// throttle below cannot see the contender that actually mattered.
    ///
    /// `bobbin status` is a fresh process on every invocation, so its
    /// `last_compact_secs` is always the "never" sentinel and it always fell
    /// through to a lock attempt. The incremental service runs `bobbin status`
    /// on every cycle, so the store-wide maintenance lock was being taken by
    /// short-lived readers on a schedule — and the nightly, the only job that
    /// prunes, skipped in silence when it lost the race. Measured: a supervised
    /// reindex that lost the lock reclaimed nothing (dependency versions went
    /// UP, 280,899 -> 280,971); the identical command with the contender
    /// stopped took dependencies 3.2G -> 117M.
    ///
    /// Reading the shared record first means a status call that another
    /// participant already compacted for does not touch the lock at all.
    async fn compact_if_stale(&self) {
        let now_wall = now_unix();
        if let Some(last_wall) = self.maintenance_status().last_compact_unix {
            if now_wall.saturating_sub(last_wall) < READ_PATH_COMPACT_INTERVAL_SECS {
                return;
            }
        }
        // Clamped to >= 1 so a recorded value is never the "never" sentinel 0.
        let now = self.opened_at.elapsed().as_secs().max(1);
        let last = self.last_compact_secs.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < READ_PATH_COMPACT_INTERVAL_SECS {
            return;
        }
        // Record the attempt whether it ran or was skipped (lock held
        // elsewhere): either way, hammering the lock on every request is the
        // behavior this throttle exists to stop.
        self.last_compact_secs.store(now, Ordering::Relaxed);
        // NoWait is load-bearing: the read path is opportunistic and must yield
        // to scheduled maintenance, never queue in front of it.
        self.compact(LockWait::NoWait).await.ok();
    }

    /// Search for similar vectors using approximate nearest neighbor search.
    /// Optionally filter by repo name and/or an additional SQL WHERE clause.
    pub async fn search(
        &self,
        query_embedding: &[f32],
        limit: usize,
        repo: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        self.search_filtered(query_embedding, limit, repo, None)
            .await
    }

    /// Search with an additional SQL filter (e.g., "language IN ('hla', 'pensieve')").
    pub async fn search_filtered(
        &self,
        query_embedding: &[f32],
        limit: usize,
        repo: Option<&str>,
        filter: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        if limit == 0 {
            return Ok(vec![]);
        }

        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let mut query = table
            .vector_search(query_embedding.to_vec())
            .context("Failed to create vector search")?;

        let combined = Self::combine_filters(repo, filter);
        if let Some(ref f) = combined {
            query = query.only_if(f.clone());
        }

        let results = match query.limit(limit).execute().await {
            Ok(r) => r,
            Err(e) => {
                // LanceDB raises "k must be positive" when a filter yields 0 matching
                // rows in the index partition. Return empty results instead of panicking.
                let msg = e.to_string();
                if msg.contains("k must be positive") || msg.contains("must be positive") {
                    return Ok(vec![]);
                }
                return Err(e).context("Failed to execute vector search");
            }
        };

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect search results")?;

        Self::batches_to_results(&batches, MatchType::Semantic)
    }

    /// Full-text search on content and chunk_name.
    /// Optionally filter by repo name.
    pub async fn search_fts(
        &self,
        query: &str,
        limit: usize,
        repo: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        self.search_fts_filtered(query, limit, repo, None).await
    }

    /// Full-text search with an additional SQL filter.
    pub async fn search_fts_filtered(
        &self,
        query: &str,
        limit: usize,
        repo: Option<&str>,
        filter: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        if limit == 0 {
            return Ok(vec![]);
        }

        // Ensure FTS index exists (must be called before borrowing self.table)
        self.ensure_fts_index().await?;

        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let combined = Self::combine_filters(repo, filter);

        // First attempt. `full_text_search` requires a valid FTS index — if the
        // index is missing or was invalidated (e.g. by compaction/prune, see
        // #16), the query fails. Rather than surface a 500, self-heal: rebuild
        // the FTS index over current rows and retry with bounded backoff. This also fixes the
        // case where ensure_fts_index() marked the index ready after a failed
        // create, and refreshes coverage of rows added since the last build.
        let batches = recover_fts_query(
            || Self::run_fts_query(table, query, limit, combined.as_deref()),
            || self.rebuild_fts_index(),
            |delay_ms| tokio::time::sleep(std::time::Duration::from_millis(delay_ms)),
        )
        .await?;
        Self::batches_to_fts_results(&batches)
    }

    /// Execute a single FTS query and collect its result batches.
    async fn run_fts_query(
        table: &lancedb::Table,
        query: &str,
        limit: usize,
        filter: Option<&str>,
    ) -> Result<Vec<RecordBatch>> {
        let mut q = table
            .query()
            .full_text_search(FullTextSearchQuery::new(query.to_string()));
        if let Some(f) = filter {
            q = q.only_if(f.to_string());
        }
        let results = q
            .limit(limit)
            .execute()
            .await
            .context("Failed to execute FTS search")?;
        results
            .try_collect()
            .await
            .context("Failed to collect FTS results")
    }

    /// Combine repo and extra filter into a single SQL WHERE clause.
    fn combine_filters(repo: Option<&str>, filter: Option<&str>) -> Option<String> {
        let repo_clause = repo.map(|r| {
            if r.contains(',') {
                // Multi-repo: repo IN ('a', 'b', 'c')
                let escaped: Vec<String> = r
                    .split(',')
                    .map(|s| format!("'{}'", s.trim().replace('\'', "''")))
                    .collect();
                format!("repo IN ({})", escaped.join(", "))
            } else {
                format!("repo = '{}'", r.replace('\'', "''"))
            }
        });
        match (repo_clause, filter) {
            (Some(r), Some(f)) => Some(format!("{} AND ({})", r, f)),
            (Some(r), None) => Some(r),
            (None, Some(f)) => Some(f.to_string()),
            (None, None) => None,
        }
    }

    /// Convert RecordBatches to SearchResults (for vector search with _distance)
    fn batches_to_results(
        batches: &[RecordBatch],
        match_type: MatchType,
    ) -> Result<Vec<SearchResult>> {
        let mut search_results = Vec::new();

        for batch in batches {
            let ids = batch
                .column_by_name("id")
                .context("Missing id column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("id column has wrong type")?;

            let file_paths = batch
                .column_by_name("file_path")
                .context("Missing file_path column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("file_path column has wrong type")?;

            let chunk_names = batch
                .column_by_name("chunk_name")
                .context("Missing chunk_name column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_name column has wrong type")?;

            let chunk_types = batch
                .column_by_name("chunk_type")
                .context("Missing chunk_type column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_type column has wrong type")?;

            let start_lines = batch
                .column_by_name("start_line")
                .context("Missing start_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("start_line column has wrong type")?;

            let end_lines = batch
                .column_by_name("end_line")
                .context("Missing end_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("end_line column has wrong type")?;

            let contents = batch
                .column_by_name("content")
                .context("Missing content column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("content column has wrong type")?;

            let languages = batch
                .column_by_name("language")
                .context("Missing language column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("language column has wrong type")?;

            // LanceDB adds _distance column for search results
            let distances = batch
                .column_by_name("_distance")
                .context("Missing _distance column")?
                .as_any()
                .downcast_ref::<Float32Array>()
                .context("_distance column has wrong type")?;

            // indexed_at is optional (may not be present in older indices)
            let indexed_ats = batch
                .column_by_name("indexed_at")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            // repo is optional (present in all current indices)
            let repos = batch
                .column_by_name("repo")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            // tags is optional (may not be present in older indices)
            let tags_col = batch
                .column_by_name("tags")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                let chunk = Chunk {
                    id: ids.value(i).to_string(),
                    file_path: file_paths.value(i).to_string(),
                    chunk_type: str_to_chunk_type(chunk_types.value(i)),
                    name: if chunk_names.is_null(i) {
                        None
                    } else {
                        Some(chunk_names.value(i).to_string())
                    },
                    start_line: start_lines.value(i),
                    end_line: end_lines.value(i),
                    content: contents.value(i).to_string(),
                    language: languages.value(i).to_string(),
                    tags: tags_col
                        .map(|arr| arr.value(i).to_string())
                        .unwrap_or_default(),
                };

                let distance = distances.value(i);
                let score = 1.0 / (1.0 + distance);

                let indexed_at = indexed_ats.and_then(|arr| arr.value(i).parse::<i64>().ok());

                let repo = repos.map(|r| r.value(i).to_string());

                search_results.push(SearchResult {
                    chunk,
                    score,
                    match_type: Some(match_type),
                    indexed_at,
                    repo,
                });
            }
        }

        Ok(search_results)
    }

    /// Convert RecordBatches to SearchResults (for FTS, using _score)
    fn batches_to_fts_results(batches: &[RecordBatch]) -> Result<Vec<SearchResult>> {
        let mut search_results = Vec::new();

        for batch in batches {
            let ids = batch
                .column_by_name("id")
                .context("Missing id column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("id column has wrong type")?;

            let file_paths = batch
                .column_by_name("file_path")
                .context("Missing file_path column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("file_path column has wrong type")?;

            let chunk_names = batch
                .column_by_name("chunk_name")
                .context("Missing chunk_name column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_name column has wrong type")?;

            let chunk_types = batch
                .column_by_name("chunk_type")
                .context("Missing chunk_type column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_type column has wrong type")?;

            let start_lines = batch
                .column_by_name("start_line")
                .context("Missing start_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("start_line column has wrong type")?;

            let end_lines = batch
                .column_by_name("end_line")
                .context("Missing end_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("end_line column has wrong type")?;

            let contents = batch
                .column_by_name("content")
                .context("Missing content column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("content column has wrong type")?;

            let languages = batch
                .column_by_name("language")
                .context("Missing language column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("language column has wrong type")?;

            // FTS returns _score (BM25 relevance score)
            let scores = batch
                .column_by_name("_score")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

            // indexed_at is optional (may not be present in older indices)
            let indexed_ats = batch
                .column_by_name("indexed_at")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            // repo is optional (present in all current indices)
            let repos = batch
                .column_by_name("repo")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            // tags is optional (may not be present in older indices)
            let tags_col = batch
                .column_by_name("tags")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                let chunk = Chunk {
                    id: ids.value(i).to_string(),
                    file_path: file_paths.value(i).to_string(),
                    chunk_type: str_to_chunk_type(chunk_types.value(i)),
                    name: if chunk_names.is_null(i) {
                        None
                    } else {
                        Some(chunk_names.value(i).to_string())
                    },
                    start_line: start_lines.value(i),
                    end_line: end_lines.value(i),
                    content: contents.value(i).to_string(),
                    language: languages.value(i).to_string(),
                    tags: tags_col
                        .map(|arr| arr.value(i).to_string())
                        .unwrap_or_default(),
                };

                let score = scores.map(|s| s.value(i)).unwrap_or(1.0);

                let indexed_at = indexed_ats.and_then(|arr| arr.value(i).parse::<i64>().ok());

                let repo = repos.map(|r| r.value(i).to_string());

                search_results.push(SearchResult {
                    chunk,
                    score,
                    match_type: Some(MatchType::Keyword),
                    indexed_at,
                    repo,
                });
            }
        }

        Ok(search_results)
    }

    /// Get the stored embedding vector for a chunk by its ID
    pub async fn get_chunk_embedding(&self, chunk_id: &str) -> Result<Option<Vec<f32>>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(None),
        };

        let filter = format!("id = '{}'", chunk_id.replace('\'', "''"));

        let results = table
            .query()
            .only_if(filter)
            .select(lancedb::query::Select::Columns(vec!["vector".to_string()]))
            .limit(1)
            .execute()
            .await
            .context("Failed to query chunk embedding")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect chunk embedding")?;

        for batch in &batches {
            if batch.num_rows() == 0 {
                continue;
            }

            let vectors = batch
                .column_by_name("vector")
                .context("Missing vector column")?
                .as_any()
                .downcast_ref::<FixedSizeListArray>()
                .context("vector column has wrong type")?;

            let value_arr = vectors.value(0);
            let values = value_arr
                .as_any()
                .downcast_ref::<Float32Array>()
                .context("vector values have wrong type")?;

            return Ok(Some(values.values().to_vec()));
        }

        Ok(None)
    }

    /// Get a single chunk by its ID
    pub async fn get_chunk_by_id(&self, chunk_id: &str) -> Result<Option<Chunk>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(None),
        };

        let filter = format!("id = '{}'", chunk_id.replace('\'', "''"));

        let results = table
            .query()
            .only_if(filter)
            .execute()
            .await
            .context("Failed to query chunk by ID")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect chunk by ID")?;

        for batch in &batches {
            if batch.num_rows() == 0 {
                continue;
            }

            let ids = batch
                .column_by_name("id")
                .context("Missing id column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("id column has wrong type")?;
            let file_paths = batch
                .column_by_name("file_path")
                .context("Missing file_path column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("file_path column has wrong type")?;
            let chunk_names = batch
                .column_by_name("chunk_name")
                .context("Missing chunk_name column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_name column has wrong type")?;
            let chunk_types = batch
                .column_by_name("chunk_type")
                .context("Missing chunk_type column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_type column has wrong type")?;
            let start_lines = batch
                .column_by_name("start_line")
                .context("Missing start_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("start_line column has wrong type")?;
            let end_lines = batch
                .column_by_name("end_line")
                .context("Missing end_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("end_line column has wrong type")?;
            let contents = batch
                .column_by_name("content")
                .context("Missing content column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("content column has wrong type")?;
            let languages = batch
                .column_by_name("language")
                .context("Missing language column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("language column has wrong type")?;
            let tags_col = batch
                .column_by_name("tags")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            return Ok(Some(Chunk {
                id: ids.value(0).to_string(),
                file_path: file_paths.value(0).to_string(),
                chunk_type: str_to_chunk_type(chunk_types.value(0)),
                name: if chunk_names.is_null(0) {
                    None
                } else {
                    Some(chunk_names.value(0).to_string())
                },
                start_line: start_lines.value(0),
                end_line: end_lines.value(0),
                content: contents.value(0).to_string(),
                language: languages.value(0).to_string(),
                tags: tags_col
                    .map(|arr| arr.value(0).to_string())
                    .unwrap_or_default(),
            }));
        }

        Ok(None)
    }

    /// Delete vectors by chunk IDs
    pub async fn delete(&self, chunk_ids: &[String]) -> Result<()> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(()),
        };

        if chunk_ids.is_empty() {
            return Ok(());
        }

        // Batch the IN clause: a single `id IN (...)` with hundreds/thousands of
        // literals overflows LanceDB/datafusion's expression handling and fails
        // ("Failed to delete chunks"). This bit large bead batches (e.g. 632
        // beads) — insert()'s upsert delete failed, so those beads never stored.
        // Chunking keeps each filter small and reliable.
        const DELETE_BATCH: usize = 100;
        for batch in chunk_ids.chunks(DELETE_BATCH) {
            let escaped_ids: Vec<String> = batch
                .iter()
                .map(|id| format!("'{}'", id.replace('\'', "''")))
                .collect();
            let filter = format!("id IN ({})", escaped_ids.join(", "));
            retry_on_conflict!(table, table.delete(&filter)).context("Failed to delete chunks")?;
        }

        Ok(())
    }

    /// Get total chunk count
    pub async fn count(&self) -> Result<u64> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(0),
        };

        let count = table
            .count_rows(None)
            .await
            .context("Failed to count chunks")?;

        Ok(count as u64)
    }

    /// Delete all chunks for files matching the given paths.
    ///
    /// `repo = Some(..)` confines the delete to that repo's rows. Paths are
    /// repo-relative, so the same path names a different file in every indexed
    /// repo — an unscoped delete for repo A's `README.md` also removed repo
    /// B's, leaving B silently unsearchable until its next reindex
    ///. `None` keeps the old any-repo behavior for callers that
    /// genuinely cannot attribute the path.
    pub async fn delete_by_file(&self, file_paths: &[String], repo: Option<&str>) -> Result<()> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(()),
        };

        if file_paths.is_empty() {
            return Ok(());
        }

        let repo_clause = repo
            .map(|r| format!("repo = '{}' AND ", r.replace('\'', "''")))
            .unwrap_or_default();

        // Batch the IN clause (see delete() — large IN overflows the query engine).
        const DELETE_BATCH: usize = 100;
        for batch in file_paths.chunks(DELETE_BATCH) {
            let escaped_paths: Vec<String> = batch
                .iter()
                .map(|p| format!("'{}'", p.replace('\'', "''")))
                .collect();
            let filter = format!("{}file_path IN ({})", repo_clause, escaped_paths.join(", "));
            retry_on_conflict!(table, table.delete(&filter))
                .context("Failed to delete chunks by file")?;
        }

        Ok(())
    }

    /// Delete all chunks belonging to a specific repo.
    pub async fn delete_by_repo(&self, repo: &str) -> Result<()> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(()),
        };

        let filter = format!("repo = '{}'", repo.replace('\'', "''"));
        retry_on_conflict!(table, table.delete(&filter))
            .context("Failed to delete chunks by repo")?;

        Ok(())
    }

    /// Check if a file needs reindexing by comparing hash.
    ///
    /// `repo = Some(..)` scopes the check to that repo's rows: an unscoped
    /// match can find another repo's copy of the same path holding the same
    /// content and conclude "already indexed" while THIS repo has no rows at
    /// all.
    pub async fn needs_reindex(
        &self,
        file_path: &str,
        current_hash: &str,
        repo: Option<&str>,
    ) -> Result<bool> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(true), // No data yet, needs indexing
        };

        // Query for any chunk from this file and check the file_hash
        let repo_clause = repo
            .map(|r| format!("repo = '{}' AND ", r.replace('\'', "''")))
            .unwrap_or_default();
        let filter = format!(
            "{}file_path = '{}'",
            repo_clause,
            file_path.replace('\'', "''")
        );
        let results = table
            .query()
            .only_if(filter)
            .limit(1)
            .execute()
            .await
            .context("Failed to query file hash")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect file hash results")?;

        if batches.is_empty() || batches[0].num_rows() == 0 {
            return Ok(true); // File not indexed yet
        }

        let file_hashes = batches[0]
            .column_by_name("file_hash")
            .context("Missing file_hash column")?
            .as_any()
            .downcast_ref::<StringArray>()
            .context("file_hash column has wrong type")?;

        let stored_hash = file_hashes.value(0);
        Ok(stored_hash != current_hash)
    }

    /// Get all indexed file paths, optionally filtered by repo
    pub async fn get_all_file_paths(&self, repo: Option<&str>) -> Result<Vec<String>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let mut q = table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "file_path".to_string()
            ]))
            .limit(SCAN_ALL_LIMIT);

        if let Some(repo_name) = repo {
            q = q.only_if(format!("repo = '{}'", repo_name.replace('\'', "''")));
        }

        let results = q.execute().await.context("Failed to query file paths")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect file paths")?;

        let mut paths = std::collections::HashSet::new();
        for batch in &batches {
            let file_paths = batch
                .column_by_name("file_path")
                .context("Missing file_path column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("file_path column has wrong type")?;

            for i in 0..batch.num_rows() {
                paths.insert(file_paths.value(i).to_string());
            }
        }

        Ok(paths.into_iter().collect())
    }

    /// Get all unique repo names in the index
    pub async fn get_all_repos(&self) -> Result<Vec<String>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let results = table
            .query()
            .select(lancedb::query::Select::Columns(vec!["repo".to_string()]))
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query repos")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect repos")?;

        let mut repos = std::collections::HashSet::new();
        for batch in &batches {
            let repo_col = batch
                .column_by_name("repo")
                .context("Missing repo column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("repo column has wrong type")?;

            for i in 0..batch.num_rows() {
                repos.insert(repo_col.value(i).to_string());
            }
        }

        let mut repo_list: Vec<String> = repos.into_iter().collect();
        repo_list.sort();
        Ok(repo_list)
    }

    /// Get all unique languages in the index
    pub async fn get_all_languages(&self) -> Result<Vec<String>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let results = table
            .query()
            .select(lancedb::query::Select::Columns(
                vec!["language".to_string()],
            ))
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query languages")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect languages")?;

        let mut langs = std::collections::HashSet::new();
        for batch in &batches {
            let lang_col = batch
                .column_by_name("language")
                .context("Missing language column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("language column has wrong type")?;

            for i in 0..batch.num_rows() {
                let val = lang_col.value(i);
                if !val.is_empty() {
                    langs.insert(val.to_string());
                }
            }
        }

        let mut lang_list: Vec<String> = langs.into_iter().collect();
        lang_list.sort();
        Ok(lang_list)
    }

    /// Get all unique chunk types in the index
    pub async fn get_all_chunk_types(&self) -> Result<Vec<String>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let results = table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "chunk_type".to_string()
            ]))
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query chunk types")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect chunk types")?;

        let mut types = std::collections::HashSet::new();
        for batch in &batches {
            let type_col = batch
                .column_by_name("chunk_type")
                .context("Missing chunk_type column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_type column has wrong type")?;

            for i in 0..batch.num_rows() {
                let val = type_col.value(i);
                if !val.is_empty() {
                    types.insert(val.to_string());
                }
            }
        }

        let mut type_list: Vec<String> = types.into_iter().collect();
        type_list.sort();
        Ok(type_list)
    }

    /// Get file metadata from a chunk record
    pub async fn get_file(&self, file_path: &str) -> Result<Option<FileMetadata>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(None),
        };

        let filter = format!("file_path = '{}'", file_path.replace('\'', "''"));
        let results = table
            .query()
            .only_if(filter)
            .select(lancedb::query::Select::Columns(vec![
                "file_path".to_string(),
                "language".to_string(),
                "file_hash".to_string(),
                "indexed_at".to_string(),
            ]))
            .limit(1)
            .execute()
            .await
            .context("Failed to query file metadata")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect file metadata")?;

        if batches.is_empty() || batches[0].num_rows() == 0 {
            return Ok(None);
        }

        let batch = &batches[0];
        let file_paths = string_column(batch, "file_path")?;
        let languages = string_column(batch, "language")?;
        let file_hashes = string_column(batch, "file_hash")?;
        let indexed_ats = string_column(batch, "indexed_at")?;

        let indexed_at_str = indexed_ats.value(0);
        let indexed_at = indexed_at_str.parse::<i64>().unwrap_or(0);

        Ok(Some(FileMetadata {
            path: file_paths.value(0).to_string(),
            language: Some(languages.value(0).to_string()),
            mtime: 0, // Not stored in LanceDB (derived from filesystem)
            hash: file_hashes.value(0).to_string(),
            indexed_at,
        }))
    }

    /// Get all chunks for a specific file path, ordered by start_line
    pub async fn get_chunks_for_file(
        &self,
        file_path: &str,
        repo: Option<&str>,
    ) -> Result<Vec<Chunk>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let mut filter = format!("file_path = '{}'", file_path.replace('\'', "''"));
        if let Some(repo_name) = repo {
            filter.push_str(&format!(" AND repo = '{}'", repo_name.replace('\'', "''")));
        }

        let results = table
            .query()
            .only_if(filter)
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query chunks for file")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect chunks for file")?;

        let mut chunks = Vec::new();
        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .context("Missing id column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("id column has wrong type")?;
            let file_paths = batch
                .column_by_name("file_path")
                .context("Missing file_path column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("file_path column has wrong type")?;
            let chunk_names = batch
                .column_by_name("chunk_name")
                .context("Missing chunk_name column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_name column has wrong type")?;
            let chunk_types = batch
                .column_by_name("chunk_type")
                .context("Missing chunk_type column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_type column has wrong type")?;
            let start_lines = batch
                .column_by_name("start_line")
                .context("Missing start_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("start_line column has wrong type")?;
            let end_lines = batch
                .column_by_name("end_line")
                .context("Missing end_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("end_line column has wrong type")?;
            let contents = batch
                .column_by_name("content")
                .context("Missing content column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("content column has wrong type")?;
            let languages = batch
                .column_by_name("language")
                .context("Missing language column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("language column has wrong type")?;
            let tags_col = batch
                .column_by_name("tags")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                chunks.push(Chunk {
                    id: ids.value(i).to_string(),
                    file_path: file_paths.value(i).to_string(),
                    chunk_type: str_to_chunk_type(chunk_types.value(i)),
                    name: if chunk_names.is_null(i) {
                        None
                    } else {
                        Some(chunk_names.value(i).to_string())
                    },
                    start_line: start_lines.value(i),
                    end_line: end_lines.value(i),
                    content: contents.value(i).to_string(),
                    language: languages.value(i).to_string(),
                    tags: tags_col
                        .map(|arr| arr.value(i).to_string())
                        .unwrap_or_default(),
                });
            }
        }

        chunks.sort_by_key(|c| c.start_line);
        Ok(chunks)
    }

    /// Get all chunks with their embedding vectors, optionally filtered by repo.
    /// Returns (Chunk, embedding, repo_name) tuples for bulk operations like duplicate scanning.
    pub async fn get_all_chunks_with_embeddings(
        &self,
        repo: Option<&str>,
    ) -> Result<Vec<(Chunk, Vec<f32>, String)>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let mut q = table.query().limit(SCAN_ALL_LIMIT);

        if let Some(repo_name) = repo {
            q = q.only_if(format!("repo = '{}'", repo_name.replace('\'', "''")));
        }

        let results = q
            .execute()
            .await
            .context("Failed to query all chunks with embeddings")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect chunks with embeddings")?;

        let mut items = Vec::new();
        for batch in &batches {
            let ids = string_column(batch, "id")?;
            let file_paths = string_column(batch, "file_path")?;
            let chunk_names = string_column(batch, "chunk_name")?;
            let chunk_types = string_column(batch, "chunk_type")?;
            let contents = string_column(batch, "content")?;
            let languages = string_column(batch, "language")?;
            let repos = string_column(batch, "repo")?;
            let start_lines = batch
                .column_by_name("start_line")
                .context("Missing start_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("start_line column has wrong type")?;
            let end_lines = batch
                .column_by_name("end_line")
                .context("Missing end_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("end_line column has wrong type")?;
            let vectors = batch
                .column_by_name("vector")
                .context("Missing vector column")?
                .as_any()
                .downcast_ref::<FixedSizeListArray>()
                .context("vector column has wrong type")?;
            let tags_col = batch
                .column_by_name("tags")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                let chunk = Chunk {
                    id: ids.value(i).to_string(),
                    file_path: file_paths.value(i).to_string(),
                    chunk_type: str_to_chunk_type(chunk_types.value(i)),
                    name: if chunk_names.is_null(i) {
                        None
                    } else {
                        Some(chunk_names.value(i).to_string())
                    },
                    start_line: start_lines.value(i),
                    end_line: end_lines.value(i),
                    content: contents.value(i).to_string(),
                    language: languages.value(i).to_string(),
                    tags: tags_col
                        .map(|arr| arr.value(i).to_string())
                        .unwrap_or_default(),
                };

                let value_arr = vectors.value(i);
                let values = value_arr
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .context("vector values have wrong type")?;
                let embedding = values.values().to_vec();

                let repo_name = repos.value(i).to_string();
                items.push((chunk, embedding, repo_name));
            }
        }

        Ok(items)
    }

    /// Get all chunks whose chunk_name matches the given name, optionally filtered by repo
    pub async fn get_chunks_by_name(&self, name: &str, repo: Option<&str>) -> Result<Vec<Chunk>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let mut filter = format!("chunk_name = '{}'", name.replace('\'', "''"));
        if let Some(repo_name) = repo {
            filter.push_str(&format!(" AND repo = '{}'", repo_name.replace('\'', "''")));
        }

        let results = table
            .query()
            .only_if(filter)
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query chunks by name")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect chunks by name")?;

        let mut chunks = Vec::new();
        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .context("Missing id column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("id column has wrong type")?;
            let file_paths = batch
                .column_by_name("file_path")
                .context("Missing file_path column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("file_path column has wrong type")?;
            let chunk_names = batch
                .column_by_name("chunk_name")
                .context("Missing chunk_name column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_name column has wrong type")?;
            let chunk_types = batch
                .column_by_name("chunk_type")
                .context("Missing chunk_type column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("chunk_type column has wrong type")?;
            let start_lines = batch
                .column_by_name("start_line")
                .context("Missing start_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("start_line column has wrong type")?;
            let end_lines = batch
                .column_by_name("end_line")
                .context("Missing end_line column")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("end_line column has wrong type")?;
            let contents = batch
                .column_by_name("content")
                .context("Missing content column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("content column has wrong type")?;
            let languages = batch
                .column_by_name("language")
                .context("Missing language column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("language column has wrong type")?;
            let tags_col = batch
                .column_by_name("tags")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                chunks.push(Chunk {
                    id: ids.value(i).to_string(),
                    file_path: file_paths.value(i).to_string(),
                    chunk_type: str_to_chunk_type(chunk_types.value(i)),
                    name: if chunk_names.is_null(i) {
                        None
                    } else {
                        Some(chunk_names.value(i).to_string())
                    },
                    start_line: start_lines.value(i),
                    end_line: end_lines.value(i),
                    content: contents.value(i).to_string(),
                    language: languages.value(i).to_string(),
                    tags: tags_col
                        .map(|arr| arr.value(i).to_string())
                        .unwrap_or_default(),
                });
            }
        }

        Ok(chunks)
    }

    /// Get index statistics, optionally filtered by repo
    ///
    /// Compacts the table first to merge fragments. LanceDB scans on heavily
    /// fragmented tables (from per-file delete+insert cycles) can return
    /// incomplete results. Compaction is idempotent and fast when already done.
    pub async fn get_stats(&self, repo: Option<&str>) -> Result<IndexStats> {
        let table = match &self.table {
            Some(t) => t,
            None => {
                return Ok(IndexStats {
                    total_files: 0,
                    total_chunks: 0,
                    total_embeddings: 0,
                    languages: vec![],
                    last_indexed: None,
                    index_size_bytes: 0,
                });
            }
        };

        // Compact before scanning — fragmented tables return incomplete scan
        // results in LanceDB 0.17. Throttled + lock-gated: get_stats is on
        // every request path, and an unconditional compact here was the
        // runaway-contention trigger.
        self.compact_if_stale().await;

        let repo_filter = repo.map(|r| format!("repo = '{}'", r.replace('\'', "''")));

        let total_chunks = table.count_rows(repo_filter.clone()).await.unwrap_or(0) as u64;

        // Scan all rows for per-language aggregation.
        let mut q = table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "file_path".to_string(),
                "language".to_string(),
                "indexed_at".to_string(),
            ]))
            .limit(SCAN_ALL_LIMIT);

        if let Some(ref filter) = repo_filter {
            q = q.only_if(filter.clone());
        }

        let results = q.execute().await.context("Failed to query stats")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect stats")?;

        let mut file_set = std::collections::HashSet::new();
        let mut lang_files: std::collections::HashMap<String, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        let mut lang_chunks: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        let mut last_indexed: Option<i64> = None;
        let mut scanned_rows: u64 = 0;

        for batch in &batches {
            let file_paths = string_column(batch, "file_path")?;
            let languages = string_column(batch, "language")?;
            let indexed_ats = string_column(batch, "indexed_at")?;

            for i in 0..batch.num_rows() {
                let fp = file_paths.value(i).to_string();
                let lang = languages.value(i).to_string();
                let ts = indexed_ats.value(i).parse::<i64>().unwrap_or(0);

                file_set.insert(fp.clone());

                lang_files.entry(lang.clone()).or_default().insert(fp);
                *lang_chunks.entry(lang).or_insert(0) += 1;

                if last_indexed.is_none() || Some(ts) > last_indexed {
                    last_indexed = Some(ts);
                }
            }
            scanned_rows += batch.num_rows() as u64;
        }

        // Sanity check: scan should have found all rows. If not, the
        // per-language breakdown is inaccurate (use count_rows as truth).
        if scanned_rows < total_chunks {
            eprintln!(
                "warning: stats scan returned {scanned_rows} rows but count_rows reports \
                 {total_chunks} — per-language breakdown may be incomplete"
            );
        }

        let languages: Vec<LanguageStats> = lang_files
            .iter()
            .map(|(lang, files)| LanguageStats {
                language: lang.clone(),
                file_count: files.len() as u64,
                chunk_count: *lang_chunks.get(lang).unwrap_or(&0),
            })
            .collect();

        Ok(IndexStats {
            total_files: file_set.len() as u64,
            total_chunks,
            total_embeddings: total_chunks,
            languages,
            last_indexed,
            index_size_bytes: 0, // LanceDB doesn't expose this easily
        })
    }

    // ── Tag query methods ─────────────────────────────────────────────

    /// Get all unique tags in use with their chunk counts
    pub async fn get_tag_counts(&self) -> Result<Vec<(String, usize)>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let results = table
            .query()
            .select(lancedb::query::Select::Columns(vec!["tags".to_string()]))
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query tags")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect tag data")?;

        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for batch in &batches {
            if let Some(tags_col) = batch
                .column_by_name("tags")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            {
                for i in 0..batch.num_rows() {
                    let tags_str = tags_col.value(i);
                    if !tags_str.is_empty() {
                        for tag in tags_str.split(',') {
                            *counts.entry(tag.to_string()).or_insert(0) += 1;
                        }
                    }
                }
            }
        }

        let mut result: Vec<(String, usize)> = counts.into_iter().collect();
        result.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(result)
    }

    /// Get file paths that have a specific tag
    pub async fn get_files_by_tag(&self, tag: &str) -> Result<Vec<String>> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        // Use LIKE filter for comma-separated tags column
        let filter = format!(
            "tags = '{}' OR tags LIKE '{},%' OR tags LIKE '%,{}' OR tags LIKE '%,{},%'",
            tag.replace('\'', "''"),
            tag.replace('\'', "''"),
            tag.replace('\'', "''"),
            tag.replace('\'', "''"),
        );

        let results = table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "file_path".to_string()
            ]))
            .only_if(filter)
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query files by tag")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect tag results")?;

        let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for batch in &batches {
            let file_paths = string_column(batch, "file_path")?;
            for i in 0..batch.num_rows() {
                files.insert(file_paths.value(i).to_string());
            }
        }

        Ok(files.into_iter().collect())
    }

    /// Count tagged vs untagged chunks
    pub async fn count_tagged_chunks(&self) -> Result<(u64, u64)> {
        let table = match &self.table {
            Some(t) => t,
            None => return Ok((0, 0)),
        };

        let total = table.count_rows(None).await.unwrap_or(0) as u64;
        let untagged = table
            .count_rows(Some("tags = ''".to_string()))
            .await
            .unwrap_or(0) as u64;

        Ok((total - untagged, untagged))
    }

    // ── Dependency graph methods ──────────────────────────────────────

    /// Arrow schema for the dependencies table
    fn deps_schema() -> Schema {
        Schema::new(vec![
            Field::new("file_a", DataType::Utf8, false),
            Field::new("file_b", DataType::Utf8, false),
            Field::new("dep_type", DataType::Utf8, false),
            Field::new("import_statement", DataType::Utf8, false),
            Field::new("symbol", DataType::Utf8, true),
            Field::new("resolved", DataType::Boolean, false),
        ])
    }

    /// Convert ImportDependency slice to a RecordBatch
    fn deps_to_record_batch(deps: &[ImportDependency]) -> Result<RecordBatch> {
        let schema = Arc::new(Self::deps_schema());

        let file_as: Vec<&str> = deps.iter().map(|d| d.file_a.as_str()).collect();
        let file_bs: Vec<&str> = deps.iter().map(|d| d.file_b.as_str()).collect();
        let dep_types: Vec<&str> = deps.iter().map(|d| d.dep_type.as_str()).collect();
        let stmts: Vec<&str> = deps.iter().map(|d| d.import_statement.as_str()).collect();
        let symbols: Vec<Option<&str>> = deps.iter().map(|d| d.symbol.as_deref()).collect();
        let resolved: Vec<bool> = deps.iter().map(|d| d.resolved).collect();

        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(file_as)),
            Arc::new(StringArray::from(file_bs)),
            Arc::new(StringArray::from(dep_types)),
            Arc::new(StringArray::from(stmts)),
            Arc::new(StringArray::from(symbols)),
            Arc::new(BooleanArray::from(resolved)),
        ];

        RecordBatch::try_new(schema, columns).context("Failed to create deps record batch")
    }

    /// Insert or replace dependency edges (batch)
    pub async fn upsert_dependencies(&mut self, deps: &[ImportDependency]) -> Result<()> {
        if deps.is_empty() {
            return Ok(());
        }

        let schema = Arc::new(Self::deps_schema());
        let batch = Self::deps_to_record_batch(deps)?;

        match &self.deps_table {
            Some(table) => {
                retry_on_conflict!(
                    table,
                    table
                        .add(Self::batch_to_reader(batch.clone(), schema.clone()))
                        .execute()
                )
                .context("Failed to add dependencies")?;
            }
            None => {
                let reader = Self::batch_to_reader(batch, schema);
                let table = self
                    .conn
                    .create_table(DEPS_TABLE_NAME, reader)
                    .execute()
                    .await
                    .context("Failed to create dependencies table")?;
                self.deps_table = Some(table);
            }
        }

        Ok(())
    }

    /// Get dependencies from a file (what does this file import?)
    pub async fn get_dependencies(&self, file_path: &str) -> Result<Vec<ImportDependency>> {
        let table = match &self.deps_table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let filter = format!("file_a = '{}'", file_path.replace('\'', "''"));
        let results = table
            .query()
            .only_if(filter)
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query dependencies")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect dependencies")?;

        Self::batches_to_deps(&batches)
    }

    /// Get reverse dependencies (what files import this file?)
    pub async fn get_dependents(&self, file_path: &str) -> Result<Vec<ImportDependency>> {
        let table = match &self.deps_table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let filter = format!("file_b = '{}'", file_path.replace('\'', "''"));
        let results = table
            .query()
            .only_if(filter)
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query dependents")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect dependents")?;

        Self::batches_to_deps(&batches)
    }

    /// Clear all dependency data for a single file (for re-indexing)
    pub async fn clear_file_dependencies(&self, file_path: &str) -> Result<()> {
        let table = match &self.deps_table {
            Some(t) => t,
            None => return Ok(()),
        };

        let filter = format!("file_a = '{}'", file_path.replace('\'', "''"));
        retry_on_conflict!(table, table.delete(&filter))
            .context("Failed to clear file dependencies")?;

        Ok(())
    }

    /// Clear all dependency data
    pub async fn clear_dependencies(&self) -> Result<()> {
        let table = match &self.deps_table {
            Some(t) => t,
            None => return Ok(()),
        };

        retry_on_conflict!(table, table.delete("file_a IS NOT NULL"))
            .context("Failed to clear dependencies")?;

        Ok(())
    }

    /// Get dependency statistics: (total_edges, resolved_count)
    pub async fn get_dependency_stats(&self) -> Result<(u64, u64)> {
        let table = match &self.deps_table {
            Some(t) => t,
            None => return Ok((0, 0)),
        };

        let total = table.count_rows(None).await.unwrap_or(0) as u64;
        let resolved = table
            .count_rows(Some("resolved = true".to_string()))
            .await
            .unwrap_or(0) as u64;

        Ok((total, resolved))
    }

    // ── Chunk edges (symbol-level relationships) ──────────────────────────

    /// Arrow schema for the chunk_edges table
    fn chunk_edges_schema() -> Schema {
        Schema::new(vec![
            Field::new("source_chunk", DataType::Utf8, false),
            Field::new("target_chunk", DataType::Utf8, false),
            Field::new("source_name", DataType::Utf8, false),
            Field::new("target_name", DataType::Utf8, false),
            Field::new("edge_type", DataType::Utf8, false),
            Field::new("file_path", DataType::Utf8, false),
            // Scopes rel-path/edge lookups per repo: identical relative paths
            // in two indexed repos must not cross-contaminate neighbors.
            // (Adding this field drops any pre-existing 6-column table on
            // open — cheap for derived data; `bobbin index --force` restores
            // full edge coverage.)
            Field::new("repo", DataType::Utf8, false),
        ])
    }

    /// Convert ChunkEdge slice to a RecordBatch
    fn chunk_edges_to_record_batch(edges: &[ChunkEdge], repo: &str) -> Result<RecordBatch> {
        let schema = Arc::new(Self::chunk_edges_schema());

        let source_chunks: Vec<&str> = edges.iter().map(|e| e.source_chunk.as_str()).collect();
        let target_chunks: Vec<&str> = edges.iter().map(|e| e.target_chunk.as_str()).collect();
        let source_names: Vec<&str> = edges.iter().map(|e| e.source_name.as_str()).collect();
        let target_names: Vec<&str> = edges.iter().map(|e| e.target_name.as_str()).collect();
        let edge_types: Vec<String> = edges.iter().map(|e| e.edge_type.to_string()).collect();
        let edge_type_refs: Vec<&str> = edge_types.iter().map(|s| s.as_str()).collect();
        let file_paths: Vec<&str> = edges.iter().map(|e| e.file_path.as_str()).collect();
        let repos: Vec<&str> = edges.iter().map(|_| repo).collect();

        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(source_chunks)),
            Arc::new(StringArray::from(target_chunks)),
            Arc::new(StringArray::from(source_names)),
            Arc::new(StringArray::from(target_names)),
            Arc::new(StringArray::from(edge_type_refs)),
            Arc::new(StringArray::from(file_paths)),
            Arc::new(StringArray::from(repos)),
        ];

        RecordBatch::try_new(schema, columns).context("Failed to create chunk_edges record batch")
    }

    /// Insert chunk edges (batch)
    pub async fn upsert_chunk_edges(&mut self, edges: &[ChunkEdge], repo: &str) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }

        let schema = Arc::new(Self::chunk_edges_schema());
        let batch = Self::chunk_edges_to_record_batch(edges, repo)?;

        match &self.chunk_edges_table {
            Some(table) => {
                retry_on_conflict!(
                    table,
                    table
                        .add(Self::batch_to_reader(batch.clone(), schema.clone()))
                        .execute()
                )
                .context("Failed to add chunk edges")?;
            }
            None => {
                let reader = Self::batch_to_reader(batch, schema);
                let table = self
                    .conn
                    .create_table(CHUNK_EDGES_TABLE_NAME, reader)
                    .execute()
                    .await
                    .context("Failed to create chunk_edges table")?;
                self.chunk_edges_table = Some(table);
            }
        }
        Ok(())
    }

    /// Get chunk edges from a file, optionally scoped to one repo
    pub async fn get_chunk_edges(
        &self,
        file_path: &str,
        repo: Option<&str>,
    ) -> Result<Vec<ChunkEdge>> {
        let table = match &self.chunk_edges_table {
            Some(t) => t,
            None => return Ok(Vec::new()),
        };

        let mut filter = format!("file_path = '{}'", file_path.replace('\'', "''"));
        if let Some(r) = repo {
            filter.push_str(&format!(" AND repo = '{}'", r.replace('\'', "''")));
        }
        let results = table
            .query()
            .only_if(filter)
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query chunk edges")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect chunk edges")?;

        Self::batches_to_chunk_edges(&batches)
    }

    /// Get all chunk edges of a specific type
    pub async fn get_chunk_edges_by_type(
        &self,
        edge_type: ChunkEdgeType,
    ) -> Result<Vec<ChunkEdge>> {
        let table = match &self.chunk_edges_table {
            Some(t) => t,
            None => return Ok(Vec::new()),
        };

        let filter = format!("edge_type = '{}'", edge_type);
        let results = table
            .query()
            .only_if(filter)
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query chunk edges by type")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect chunk edges by type")?;

        Self::batches_to_chunk_edges(&batches)
    }

    /// One-time hygiene: delete edge rows keyed by an absolute file path.
    ///
    /// Edges were historically extracted under the absolute walk path while
    /// chunks were parsed under repo-relative paths, so absolute-keyed rows
    /// can never join against the chunks table. Idempotent and cheap; called
    /// on every index run.
    pub async fn clear_absolute_path_chunk_edges(&self) -> Result<()> {
        let table = match &self.chunk_edges_table {
            Some(t) => t,
            None => return Ok(()),
        };

        retry_on_conflict!(table, table.delete("file_path LIKE '/%'"))
            .context("Failed to sweep absolute-path chunk edges")?;

        Ok(())
    }

    /// Clear all chunk edges of one type, optionally scoped to one repo.
    ///
    /// This is what makes `similar --scan --persist` idempotent: each
    /// persist run replaces the previous `similar_to` edge set (within its
    /// repo scope) instead of accumulating duplicates across runs.
    pub async fn clear_chunk_edges_by_type(
        &self,
        edge_type: ChunkEdgeType,
        repo: Option<&str>,
    ) -> Result<()> {
        let table = match &self.chunk_edges_table {
            Some(t) => t,
            None => return Ok(()),
        };

        let mut filter = format!("edge_type = '{}'", edge_type);
        if let Some(r) = repo {
            filter.push_str(&format!(" AND repo = '{}'", r.replace('\'', "''")));
        }
        retry_on_conflict!(table, table.delete(&filter))
            .context("Failed to clear chunk edges by type")?;

        Ok(())
    }

    /// Get chunk edge statistics: total edges by type
    pub async fn get_chunk_edge_stats(&self) -> Result<Vec<(String, u64)>> {
        let table = match &self.chunk_edges_table {
            Some(t) => t,
            None => return Ok(Vec::new()),
        };

        let mut stats = Vec::new();
        for edge_type in ChunkEdgeType::ALL {
            let filter = format!("edge_type = '{}'", edge_type);
            let count = table.count_rows(Some(filter)).await.unwrap_or(0) as u64;
            if count > 0 {
                stats.push((edge_type.to_string(), count));
            }
        }
        Ok(stats)
    }

    /// Get all edges where the given chunk is source or target.
    ///
    /// By-id (rather than by-file) is the read shape the MCP tool needs:
    /// callers hold a chunk ID from search results, and the query stays
    /// correct once cross-file edges exist.
    pub async fn get_edges_for_chunk(
        &self,
        chunk_id: &str,
        repo: Option<&str>,
    ) -> Result<Vec<ChunkEdge>> {
        let table = match &self.chunk_edges_table {
            Some(t) => t,
            None => return Ok(Vec::new()),
        };

        let escaped = chunk_id.replace('\'', "''");
        let mut filter = format!(
            "(source_chunk = '{}' OR target_chunk = '{}')",
            escaped, escaped
        );
        if let Some(r) = repo {
            filter.push_str(&format!(" AND repo = '{}'", r.replace('\'', "''")));
        }
        let results = table
            .query()
            .only_if(filter)
            .limit(SCAN_ALL_LIMIT)
            .execute()
            .await
            .context("Failed to query edges for chunk")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect edges for chunk")?;

        Self::batches_to_chunk_edges(&batches)
    }

    /// Convert RecordBatches to ChunkEdge structs
    fn batches_to_chunk_edges(batches: &[RecordBatch]) -> Result<Vec<ChunkEdge>> {
        let mut edges = Vec::new();

        for batch in batches {
            let source_chunks = string_column(batch, "source_chunk")?;
            let target_chunks = string_column(batch, "target_chunk")?;
            let source_names = string_column(batch, "source_name")?;
            let target_names = string_column(batch, "target_name")?;
            let edge_types = string_column(batch, "edge_type")?;
            let file_paths = string_column(batch, "file_path")?;

            for i in 0..batch.num_rows() {
                let edge_type = match edge_types.value(i) {
                    "implements" => ChunkEdgeType::Implements,
                    "impl_for" => ChunkEdgeType::ImplFor,
                    "tests" => ChunkEdgeType::Tests,
                    "extends" => ChunkEdgeType::Extends,
                    "next_chunk" => ChunkEdgeType::NextChunk,
                    "part_of" => ChunkEdgeType::PartOf,
                    "similar_to" => ChunkEdgeType::SimilarTo,
                    other => {
                        eprintln!("Unknown chunk edge type: {}", other);
                        continue;
                    }
                };
                edges.push(ChunkEdge {
                    source_chunk: source_chunks.value(i).to_string(),
                    target_chunk: target_chunks.value(i).to_string(),
                    source_name: source_names.value(i).to_string(),
                    target_name: target_names.value(i).to_string(),
                    edge_type,
                    file_path: file_paths.value(i).to_string(),
                });
            }
        }

        Ok(edges)
    }

    // ── Entity embeddings table ──────────────────────────────────────────

    /// Build the Arrow schema for the entities table.
    fn entities_schema(embedding_dim: i32) -> Schema {
        Schema::new(vec![
            Field::new("entity_iri", DataType::Utf8, false),
            Field::new("text", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(Self::vector_field(), embedding_dim),
                false,
            ),
            Field::new("entity_type", DataType::Utf8, false),
            Field::new("repo", DataType::Utf8, true),
        ])
    }

    /// Convert Entity slice + embeddings to a RecordBatch
    fn entities_to_record_batch(
        entities: &[Entity],
        embeddings: &[Vec<f32>],
        embedding_dim: i32,
    ) -> Result<RecordBatch> {
        let schema = Arc::new(Self::entities_schema(embedding_dim));

        let iris: Vec<&str> = entities.iter().map(|e| e.entity_iri.as_str()).collect();
        let texts: Vec<&str> = entities.iter().map(|e| e.text.as_str()).collect();
        let entity_types: Vec<&str> = entities.iter().map(|e| e.entity_type.as_str()).collect();
        let repos: Vec<Option<&str>> = entities.iter().map(|e| e.repo.as_deref()).collect();

        let flat_embeddings: Vec<f32> = embeddings.iter().flatten().copied().collect();
        let embedding_values: ArrayRef = Arc::new(Float32Array::from(flat_embeddings));
        let vector_array = FixedSizeListArray::try_new(
            Self::vector_field(),
            embedding_dim,
            embedding_values,
            None,
        )
        .context("Failed to create entity vector array")?;

        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(iris)),
            Arc::new(StringArray::from(texts)),
            Arc::new(vector_array),
            Arc::new(StringArray::from(entity_types)),
            Arc::new(StringArray::from(repos)),
        ];

        RecordBatch::try_new(schema, columns).context("Failed to create entities record batch")
    }

    /// Insert or update entity embeddings.
    ///
    /// Existing entities with matching IRIs are deleted first (upsert).
    pub async fn upsert_entities(
        &mut self,
        entities: &[Entity],
        embeddings: &[Vec<f32>],
    ) -> Result<()> {
        if entities.is_empty() {
            return Ok(());
        }

        if entities.len() != embeddings.len() {
            anyhow::bail!(
                "Entities and embeddings must have same length: {} vs {}",
                entities.len(),
                embeddings.len()
            );
        }

        let schema = Arc::new(Self::entities_schema(self.embedding_dim));
        let batch = Self::entities_to_record_batch(entities, embeddings, self.embedding_dim)?;

        match &self.entities_table {
            Some(table) => {
                // Delete existing entities with same IRIs (upsert)
                let iris: Vec<&str> = entities.iter().map(|e| e.entity_iri.as_str()).collect();
                self.delete_entities(&iris).await?;

                retry_on_conflict!(
                    table,
                    table
                        .add(Self::batch_to_reader(batch.clone(), schema.clone()))
                        .execute()
                )
                .context("Failed to add entities")?;
            }
            None => {
                let reader = Self::batch_to_reader(batch, schema);
                let table = self
                    .conn
                    .create_table(ENTITIES_TABLE_NAME, reader)
                    .execute()
                    .await
                    .context("Failed to create entities table")?;
                self.entities_table = Some(table);
            }
        }

        Ok(())
    }

    /// Search entities by vector similarity.
    pub async fn search_entities(
        &self,
        query_embedding: &[f32],
        limit: usize,
        repo: Option<&str>,
    ) -> Result<Vec<EntitySearchResult>> {
        if limit == 0 {
            return Ok(vec![]);
        }

        let table = match &self.entities_table {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let mut query = table
            .vector_search(query_embedding.to_vec())
            .context("Failed to create entity vector search")?;

        if let Some(repo_name) = repo {
            query = query.only_if(format!("repo = '{}'", repo_name.replace('\'', "''")));
        }

        let results = match query.limit(limit).execute().await {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("k must be positive") || msg.contains("must be positive") {
                    return Ok(vec![]);
                }
                return Err(e).context("Failed to execute entity vector search");
            }
        };

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect entity search results")?;

        let mut search_results = Vec::new();
        for batch in &batches {
            let iris = string_column(batch, "entity_iri")?;
            let texts = string_column(batch, "text")?;
            let entity_types = string_column(batch, "entity_type")?;
            let repos = string_column(batch, "repo")?;
            let distances = batch
                .column_by_name("_distance")
                .context("Missing _distance column")?
                .as_any()
                .downcast_ref::<Float32Array>()
                .context("_distance column has wrong type")?;

            for i in 0..batch.num_rows() {
                let distance = distances.value(i);
                let score = 1.0 / (1.0 + distance);
                search_results.push(EntitySearchResult {
                    entity_iri: iris.value(i).to_string(),
                    text: texts.value(i).to_string(),
                    entity_type: entity_types.value(i).to_string(),
                    repo: if repos.is_null(i) {
                        None
                    } else {
                        Some(repos.value(i).to_string())
                    },
                    score,
                });
            }
        }

        Ok(search_results)
    }

    /// Get the stored embedding vector for an entity by its IRI.
    pub async fn get_entity_embedding(&self, entity_iri: &str) -> Result<Option<Vec<f32>>> {
        let table = match &self.entities_table {
            Some(t) => t,
            None => return Ok(None),
        };

        let filter = format!("entity_iri = '{}'", entity_iri.replace('\'', "''"));

        let results = table
            .query()
            .only_if(filter)
            .select(lancedb::query::Select::Columns(vec!["vector".to_string()]))
            .limit(1)
            .execute()
            .await
            .context("Failed to query entity embedding")?;

        let batches: Vec<RecordBatch> = results
            .try_collect()
            .await
            .context("Failed to collect entity embedding")?;

        for batch in &batches {
            if batch.num_rows() == 0 {
                continue;
            }

            let vectors = batch
                .column_by_name("vector")
                .context("Missing vector column")?
                .as_any()
                .downcast_ref::<FixedSizeListArray>()
                .context("vector column has wrong type")?;

            let value_arr = vectors.value(0);
            let values = value_arr
                .as_any()
                .downcast_ref::<Float32Array>()
                .context("vector values have wrong type")?;

            return Ok(Some(values.values().to_vec()));
        }

        Ok(None)
    }

    /// Delete entities by their IRIs.
    pub async fn delete_entities(&self, entity_iris: &[&str]) -> Result<()> {
        let table = match &self.entities_table {
            Some(t) => t,
            None => return Ok(()),
        };

        if entity_iris.is_empty() {
            return Ok(());
        }

        let escaped: Vec<String> = entity_iris
            .iter()
            .map(|iri| format!("'{}'", iri.replace('\'', "''")))
            .collect();
        let filter = format!("entity_iri IN ({})", escaped.join(", "));

        retry_on_conflict!(table, table.delete(&filter)).context("Failed to delete entities")?;

        Ok(())
    }

    /// Delete all entities belonging to a specific repo.
    pub async fn delete_entities_by_repo(&self, repo: &str) -> Result<()> {
        let table = match &self.entities_table {
            Some(t) => t,
            None => return Ok(()),
        };

        let filter = format!("repo = '{}'", repo.replace('\'', "''"));
        retry_on_conflict!(table, table.delete(&filter))
            .context("Failed to delete entities by repo")?;

        Ok(())
    }

    /// Get total entity count.
    pub async fn count_entities(&self) -> Result<u64> {
        let table = match &self.entities_table {
            Some(t) => t,
            None => return Ok(0),
        };

        let count = table
            .count_rows(None)
            .await
            .context("Failed to count entities")?;

        Ok(count as u64)
    }

    /// Convert RecordBatches to ImportDependency structs
    fn batches_to_deps(batches: &[RecordBatch]) -> Result<Vec<ImportDependency>> {
        let mut deps = Vec::new();

        for batch in batches {
            let file_as = string_column(batch, "file_a")?;
            let file_bs = string_column(batch, "file_b")?;
            let dep_types = string_column(batch, "dep_type")?;
            let stmts = string_column(batch, "import_statement")?;
            let symbols = string_column(batch, "symbol")?;
            let resolved_col = batch
                .column_by_name("resolved")
                .context("Missing resolved column")?
                .as_any()
                .downcast_ref::<BooleanArray>()
                .context("resolved column has wrong type")?;

            for i in 0..batch.num_rows() {
                deps.push(ImportDependency {
                    file_a: file_as.value(i).to_string(),
                    file_b: file_bs.value(i).to_string(),
                    dep_type: dep_types.value(i).to_string(),
                    import_statement: stmts.value(i).to_string(),
                    symbol: if symbols.is_null(i) {
                        None
                    } else {
                        Some(symbols.value(i).to_string())
                    },
                    resolved: resolved_col.value(i),
                });
            }
        }

        Ok(deps)
    }
}

/// Convert ChunkType to string for storage
fn chunk_type_to_str(ct: &ChunkType) -> &'static str {
    match ct {
        ChunkType::Function => "function",
        ChunkType::Method => "method",
        ChunkType::Class => "class",
        ChunkType::Struct => "struct",
        ChunkType::Enum => "enum",
        ChunkType::Interface => "interface",
        ChunkType::Module => "module",
        ChunkType::Impl => "impl",
        ChunkType::Trait => "trait",
        ChunkType::Doc => "doc",
        ChunkType::Section => "section",
        ChunkType::Table => "table",
        ChunkType::CodeBlock => "code_block",
        ChunkType::Commit => "commit",
        ChunkType::Issue => "issue",
        ChunkType::Other => "other",
    }
}

/// Parse string back to ChunkType
fn str_to_chunk_type(s: &str) -> ChunkType {
    match s {
        "function" => ChunkType::Function,
        "method" => ChunkType::Method,
        "class" => ChunkType::Class,
        "struct" => ChunkType::Struct,
        "enum" => ChunkType::Enum,
        "interface" => ChunkType::Interface,
        "module" => ChunkType::Module,
        "impl" => ChunkType::Impl,
        "trait" => ChunkType::Trait,
        "doc" => ChunkType::Doc,
        "section" => ChunkType::Section,
        "table" => ChunkType::Table,
        "code_block" => ChunkType::CodeBlock,
        "commit" => ChunkType::Commit,
        "issue" => ChunkType::Issue,
        _ => ChunkType::Other,
    }
}

#[path = "lance_fts.rs"]
mod fts;
#[cfg(test)]
use fts::should_rebuild_fts_after;

#[cfg(test)]
#[path = "lance_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lance_edge_tests.rs"]
mod edge_tests;

#[path = "lance_edges.rs"]
mod edges;
