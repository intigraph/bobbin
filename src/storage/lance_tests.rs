//! Tests for the Lance vector store (sidecar of `lance.rs`).

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use tempfile::tempdir;

/// "The lock is free now", as a test means it: free once any instantaneous
/// holder is gone. Every test in this binary runs in one process, and a test
/// that spawns a child (git, the CLI) while another test has its lock file
/// open hands the child a copy of that open file, flock included; the copy
/// lives until the child execs. A just-dropped lock can therefore still be held
/// for a few microseconds, and a zero-budget acquire flakes (aegis-cur: 1306 of
/// 3000 immediate re-acquires failed alongside four threads spawning `true`,
/// none without them, and none was still held after 2s). Contention assertions
/// keep `LockWait::NoWait`: there the test itself holds the lock.
const SETTLED: LockWait = LockWait::UpTo(std::time::Duration::from_secs(10));

/// Take the maintenance lock once any instantaneous inherited holder is gone.
fn take_lock_settled(store: &VectorStore) -> std::fs::File {
    let started = std::time::Instant::now();
    loop {
        if let Some(lock) = store.try_maintenance_lock() {
            return lock;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the maintenance lock stayed held for 10s with no holder in this test"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[tokio::test]
async fn bounded_fts_recovery_waits_and_eventually_succeeds() {
    let queries = Arc::new(AtomicUsize::new(0));
    let rebuilds = Arc::new(AtomicUsize::new(0));
    let sleeps = Arc::new(std::sync::Mutex::new(Vec::new()));

    let result = recover_fts_query(
        {
            let queries = Arc::clone(&queries);
            move || {
                let attempt = queries.fetch_add(1, AtomicOrdering::SeqCst);
                async move {
                    if attempt < 2 {
                        anyhow::bail!("transient query failure")
                    }
                    Ok("recovered")
                }
            }
        },
        {
            let rebuilds = Arc::clone(&rebuilds);
            move || {
                rebuilds.fetch_add(1, AtomicOrdering::SeqCst);
                async { Ok(()) }
            }
        },
        {
            let sleeps = Arc::clone(&sleeps);
            move |delay| {
                sleeps.lock().unwrap().push(delay);
                async {}
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(result, "recovered");
    assert_eq!(queries.load(AtomicOrdering::SeqCst), 3);
    assert_eq!(rebuilds.load(AtomicOrdering::SeqCst), 2);
    assert_eq!(*sleeps.lock().unwrap(), vec![50, 100]);
}

#[tokio::test]
async fn bounded_fts_recovery_returns_original_cause_after_exhaustion() {
    let queries = Arc::new(AtomicUsize::new(0));
    let result: Result<()> = recover_fts_query(
        {
            let queries = Arc::clone(&queries);
            move || {
                let attempt = queries.fetch_add(1, AtomicOrdering::SeqCst);
                async move {
                    anyhow::bail!(if attempt == 0 {
                        "initial cause"
                    } else {
                        "later failure"
                    })
                }
            }
        },
        || async { Ok(()) },
        |_| async {},
    )
    .await;

    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("initial cause"), "{error}");
    assert!(!error.contains("later failure"), "{error}");
    assert_eq!(queries.load(AtomicOrdering::SeqCst), 4);
}

fn sample_chunk(id: &str, name: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        file_path: "src/main.rs".to_string(),
        chunk_type: ChunkType::Function,
        name: Some(name.to_string()),
        start_line: 1,
        end_line: 10,
        content: format!("fn {}() {{ }}", name),
        language: "rust".to_string(),
        tags: String::new(),
    }
}

fn sample_embedding() -> Vec<f32> {
    let mut emb: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
    let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
    emb.iter_mut().for_each(|x| *x /= norm);
    emb
}

fn no_contexts(n: usize) -> Vec<Option<String>> {
    vec![None; n]
}

#[tokio::test]
async fn test_open_creates_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let store = VectorStore::open(&path).await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
}

fn structural_edge(
    source: &str,
    target: &str,
    edge_type: ChunkEdgeType,
    file_path: &str,
) -> ChunkEdge {
    ChunkEdge {
        source_chunk: source.to_string(),
        target_chunk: target.to_string(),
        source_name: format!("{}-name", source),
        target_name: format!("{}-name", target),
        edge_type,
        file_path: file_path.to_string(),
    }
}

#[tokio::test]
async fn test_structural_edge_roundtrip_and_stats() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");
    let mut store = VectorStore::open(&path).await.unwrap();

    let edges = vec![
        structural_edge("aaa", "bbb", ChunkEdgeType::NextChunk, "docs/x.md"),
        structural_edge("bbb", "ccc", ChunkEdgeType::NextChunk, "docs/x.md"),
        structural_edge("bbb", "aaa", ChunkEdgeType::PartOf, "docs/x.md"),
        structural_edge("ddd", "eee", ChunkEdgeType::Implements, "src/y.rs"),
    ];
    store.upsert_chunk_edges(&edges, "default").await.unwrap();

    // Roundtrip by file preserves the new edge types
    let by_file = store.get_chunk_edges("docs/x.md", None).await.unwrap();
    assert_eq!(by_file.len(), 3);
    assert!(by_file
        .iter()
        .any(|e| e.edge_type == ChunkEdgeType::NextChunk));
    assert!(by_file.iter().any(|e| e.edge_type == ChunkEdgeType::PartOf));

    // By-chunk lookup sees the chunk as source and as target
    let for_bbb = store.get_edges_for_chunk("bbb", None).await.unwrap();
    assert_eq!(for_bbb.len(), 3);
    let for_ddd = store.get_edges_for_chunk("ddd", None).await.unwrap();
    assert_eq!(for_ddd.len(), 1);
    assert!(store
        .get_edges_for_chunk("zzz", None)
        .await
        .unwrap()
        .is_empty());

    // Repo scoping: filtering by the wrong repo returns nothing
    assert!(store
        .get_edges_for_chunk("bbb", Some("other-repo"))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .get_edges_for_chunk("bbb", Some("default"))
            .await
            .unwrap()
            .len(),
        3
    );

    // Stats include the new types
    let stats = store.get_chunk_edge_stats().await.unwrap();
    let get = |name: &str| {
        stats
            .iter()
            .find(|(t, _)| t == name)
            .map(|(_, c)| *c)
            .unwrap_or(0)
    };
    assert_eq!(get("next_chunk"), 2);
    assert_eq!(get("part_of"), 1);
    assert_eq!(get("implements"), 1);
}

#[tokio::test]
async fn test_insert_and_count() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![
        sample_chunk("chunk1", "main"),
        sample_chunk("chunk2", "helper"),
    ];
    let embeddings = vec![sample_embedding(), sample_embedding()];

    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(2),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    assert_eq!(store.count().await.unwrap(), 2);
}

#[tokio::test]
async fn test_search_returns_results() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![sample_chunk("chunk1", "process_data")];
    let embeddings = vec![sample_embedding()];

    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    let results = store.search(&sample_embedding(), 10, None).await.unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.id, "chunk1");
    assert_eq!(results[0].chunk.name, Some("process_data".to_string()));
    assert!(results[0].score > 0.0);
    assert_eq!(results[0].match_type, Some(MatchType::Semantic));
}

#[tokio::test]
async fn test_delete_removes_vectors() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![
        sample_chunk("chunk1", "main"),
        sample_chunk("chunk2", "helper"),
    ];
    let embeddings = vec![sample_embedding(), sample_embedding()];

    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(2),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();
    assert_eq!(store.count().await.unwrap(), 2);

    store.delete(&["chunk1".to_string()]).await.unwrap();
    assert_eq!(store.count().await.unwrap(), 1);
}

#[tokio::test]
async fn test_delete_by_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![
        Chunk {
            id: "chunk1".to_string(),
            file_path: "src/a.rs".to_string(),
            chunk_type: ChunkType::Function,
            name: Some("func_a".to_string()),
            start_line: 1,
            end_line: 5,
            content: "fn func_a() {}".to_string(),
            language: "rust".to_string(),
            tags: String::new(),
        },
        Chunk {
            id: "chunk2".to_string(),
            file_path: "src/b.rs".to_string(),
            chunk_type: ChunkType::Function,
            name: Some("func_b".to_string()),
            start_line: 1,
            end_line: 5,
            content: "fn func_b() {}".to_string(),
            language: "rust".to_string(),
            tags: String::new(),
        },
    ];
    let embeddings = vec![sample_embedding(), sample_embedding()];

    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(2),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();
    assert_eq!(store.count().await.unwrap(), 2);

    store
        .delete_by_file(&["src/a.rs".to_string()], None)
        .await
        .unwrap();
    assert_eq!(store.count().await.unwrap(), 1);

    // Repo-scoped: a delete for the same path in ANOTHER repo must spare
    // this repo's rows (the unscoped delete was removing
    // every repo's copy of a shared path).
    store
        .delete_by_file(&["src/b.rs".to_string()], Some("not-this-repo"))
        .await
        .unwrap();
    assert_eq!(
        store.count().await.unwrap(),
        1,
        "wrong-repo delete spared the row"
    );
    store
        .delete_by_file(&["src/b.rs".to_string()], Some("default"))
        .await
        .unwrap();
    assert_eq!(
        store.count().await.unwrap(),
        0,
        "right-repo delete removed it"
    );
}

#[tokio::test]
async fn test_upsert_behavior() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![sample_chunk("chunk1", "original")];
    let embeddings = vec![sample_embedding()];
    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    let chunks = vec![sample_chunk("chunk1", "updated")];
    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(1),
            "default",
            "def456",
            "1234567891",
        )
        .await
        .unwrap();

    assert_eq!(store.count().await.unwrap(), 1);

    let results = store.search(&sample_embedding(), 10, None).await.unwrap();
    assert_eq!(results[0].chunk.name, Some("updated".to_string()));
}

#[tokio::test]
async fn test_empty_operations() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    store
        .insert(&[], &[], &[], "default", "", "")
        .await
        .unwrap();

    let results = store.search(&sample_embedding(), 10, None).await.unwrap();
    assert!(results.is_empty());

    store.delete(&["nonexistent".to_string()]).await.unwrap();
}

#[tokio::test]
async fn test_reopen_persists_data() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    {
        let mut store = VectorStore::open(&path).await.unwrap();
        let chunks = vec![sample_chunk("chunk1", "persistent")];
        let embeddings = vec![sample_embedding()];
        store
            .insert(
                &chunks,
                &embeddings,
                &no_contexts(1),
                "default",
                "abc123",
                "1234567890",
            )
            .await
            .unwrap();
    }

    {
        let store = VectorStore::open(&path).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 1);

        let results = store.search(&sample_embedding(), 10, None).await.unwrap();
        assert_eq!(results[0].chunk.name, Some("persistent".to_string()));
    }
}

/// Maintenance must sweep EVERY table this store owns, not just `chunks`.
///
/// `compact`/`prune` operated on `self.table` alone, so `dependencies`,
/// `chunk_edges` and `entities` were never compacted and never pruned for
/// the life of the store. Measured consequence: the dependencies dataset
/// reached 280,895 version manifests while the chunks dataset — the one that
/// does get pruned — sat around 4,000. That is unbounded disk growth by
/// construction, and it is invisible because the store still answers queries
/// correctly the whole time.
#[tokio::test]
async fn maintenance_sweeps_every_table_not_just_chunks() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");
    let mut store = VectorStore::open(&path).await.unwrap();

    // Materialise chunks + dependencies so both tables exist.
    store
        .insert(
            &[sample_chunk("chunk1", "main")],
            &[sample_embedding()],
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();
    store
        .upsert_dependencies(&[sample_dep("src/main.rs", "src/types.rs", true)])
        .await
        .unwrap();

    let swept: Vec<&str> = store
        .maintenance_tables()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(
        swept.contains(&"chunks"),
        "chunks table missing from the maintenance sweep: {swept:?}"
    );
    assert!(
        swept.contains(&"dependencies"),
        "dependencies table is NOT swept ({swept:?}) — it will accumulate version \
         manifests forever, which is how it reached 280,895 while chunks stayed near 4,000"
    );

    // And the sweep itself must succeed over multiple tables.
    store
        .prune(SETTLED)
        .await
        .expect("prune should sweep every table");
    store
        .compact(SETTLED)
        .await
        .expect("compact should sweep every table");

    // Data survives the multi-table sweep.
    assert_eq!(
        store.get_dependencies("src/main.rs").await.unwrap().len(),
        1,
        "dependency rows lost during the maintenance sweep"
    );
}

#[test]
fn compaction_options_bound_memory_instead_of_using_library_defaults() {
    // Guards the nightly-reindex OOM fix. The failure this prevents is not
    // "compaction is slow" — it is that with the library defaults, peak
    // compaction memory is a function of the CORPUS rather than of the
    // batch, so the reindex OOMs harder as the index grows and no cap can
    // be set that stays correct.
    let opts = bounded_compaction_options();
    let default = CompactionOptions::default();

    // THE bound. The default is 1,048,576 rows; every fragment in a table
    // smaller than that is always a compaction candidate, which makes the
    // effective target "rewrite the whole store as one fragment".
    assert!(
        opts.target_rows_per_fragment < default.target_rows_per_fragment,
        "target_rows_per_fragment ({}) must be below the library default \
         ({}), or compaction rewrites the entire store in one pass and peak \
         memory scales with the corpus",
        opts.target_rows_per_fragment,
        default.target_rows_per_fragment
    );

    // Parallel compaction tasks multiply peak memory by the thread count.
    assert_eq!(
        opts.num_threads,
        Some(1),
        "compaction must run one task at a time; the default is the \
         compute-CPU count, which multiplies peak RSS by that factor"
    );

    // Rows carry full chunk text, so in-flight scan batches dominate.
    assert!(
        opts.batch_size.is_some_and(|b| b <= 8_192),
        "compaction scan batch_size must be explicitly bounded, got {:?}",
        opts.batch_size
    );
}

#[test]
fn commit_conflict_classifier_matches_lance_conflicts_only() {
    // The retry must fire on the exact failure the incident logged and
    // never mask real errors as retryable.
    let conflict = anyhow::anyhow!(
        "Commit conflict for version 3295: There was a concurrent commit \
         that conflicts with this one and it cannot be automatically resolved."
    );
    assert!(is_commit_conflict(&conflict));
    assert!(is_commit_conflict(&anyhow::anyhow!("Retryable commit conflict for version 645776: This Rewrite transaction was preempted by concurrent transaction Rewrite at version 645776. Please retry.")));
    assert!(!is_commit_conflict(&anyhow::anyhow!(
        "Failed to open table: corrupt manifest"
    )));
}

#[test]
fn fts_compaction_panic_classifier_is_narrow() {
    let incident = anyhow::anyhow!(
        "LanceError(IO): task 85132 panicked with message Option unwrap, \
         lance-index-4.0.0/src/scalar/inverted/builder.rs:316:30"
    );
    assert!(is_fts_compaction_panic(&incident));
    assert!(!is_fts_compaction_panic(&anyhow::anyhow!(
        "Failed to compact chunks table: out of memory"
    )));
    assert!(!is_fts_compaction_panic(&anyhow::anyhow!(
        "scalar/inverted/builder.rs returned an ordinary error"
    )));
}

#[tokio::test]
async fn maintenance_lock_is_exclusive_and_compact_skips_when_held() {
    // The single-compactor guarantee: while ANY participant holds the
    // maintenance lock, another participant's compact() must SKIP —
    // return Ok fast, not queue and not error. A second concurrent
    // compaction is what interrupted the first and set off the
    // fragmentation feedback loop that took the server down.
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();
    let chunks = vec![sample_chunk("chunk1", "main")];
    let embeddings = vec![sample_embedding()];
    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    // A second handle on the same store, as a second process would have.
    let other = VectorStore::open(&path).await.unwrap();

    let held = store
        .try_maintenance_lock()
        .expect("first participant takes the lock");
    assert!(
        other.try_maintenance_lock().is_none(),
        "the lock must be exclusive across handles"
    );
    // compact() under contention: Ok (skipped), not an error, not a hang.
    // And the outcome must SAY it skipped — an Ok that is indistinguishable
    // from a real sweep is the defect itself.
    let outcome = other
        .compact(LockWait::NoWait)
        .await
        .expect("contended compact skips");
    assert!(
        outcome.skipped_lock_held(),
        "contended compact must report the skip, got {outcome:?}"
    );
    let outcome = other
        .prune(LockWait::NoWait)
        .await
        .expect("contended prune skips");
    assert!(
        outcome.skipped_lock_held(),
        "contended prune must report the skip, got {outcome:?}"
    );

    drop(held);
    // Releasing the lock frees the next participant.
    drop(take_lock_settled(&other));
    // And with the lock free, a real compaction runs clean.
    assert!(
        other
            .compact(SETTLED)
            .await
            .expect("uncontended compact runs")
            .ran(),
        "an uncontended compact must report that it ran"
    );
}

#[tokio::test]
async fn scheduled_maintenance_waits_for_the_lock_instead_of_skipping() {
    // The nightly is the ONLY job that prunes, and it used to
    // skip — silently, returning Ok — whenever any other participant held
    // the maintenance lock. A routine `bobbin status` was enough. The
    // scheduled path must WAIT and then actually run.
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();
    store
        .insert(
            &[sample_chunk("chunk1", "main")],
            &[sample_embedding()],
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    let other = VectorStore::open(&path).await.unwrap();
    let held = store.try_maintenance_lock().expect("take the lock");

    // Release it shortly, as a read-path compaction would.
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        drop(held);
    });

    let outcome = other
        .prune(LockWait::UpTo(std::time::Duration::from_secs(10)))
        .await
        .expect("waiting prune succeeds");
    releaser.await.unwrap();
    assert!(
        outcome.ran(),
        "scheduled prune must WAIT out a contender and run, got {outcome:?}"
    );

    // A wait that expires still reports the skip rather than pretending.
    let held = take_lock_settled(&store);
    let outcome = other
        .compact(LockWait::UpTo(std::time::Duration::from_millis(200)))
        .await
        .expect("expired wait is not an error");
    assert!(
        outcome.skipped_lock_held(),
        "an expired wait must report the skip, got {outcome:?}"
    );
    drop(held);
}

#[tokio::test]
async fn maintain_takes_the_lock_once_and_charges_one_wait_budget() {
    // The nightly runs one `index` per repo, so the wait budget is paid
    // once PER REPO. Charging prune and compact a full budget each would
    // double that for all 27 of them, and a second acquisition leaves a
    // window for a contender to steal the lock between the prune and the
    // compaction it is supposed to precede.
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();
    store
        .insert(
            &[sample_chunk("chunk1", "main")],
            &[sample_embedding()],
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    let other = VectorStore::open(&path).await.unwrap();
    let held = store.try_maintenance_lock().expect("take the lock");

    let budget = std::time::Duration::from_millis(400);
    let started = std::time::Instant::now();
    let outcome = other
        .maintain(LockWait::UpTo(budget))
        .await
        .expect("expired wait is not an error");
    let elapsed = started.elapsed();
    drop(held);

    assert!(
        outcome.skipped_lock_held(),
        "a starved sweep must say so, got {outcome:?}"
    );
    assert!(
        elapsed < budget * 2,
        "maintain must charge ONE wait budget, not one per operation \
         (waited {elapsed:?} against a {budget:?} budget)"
    );

    // Uncontended, one call records BOTH operations.
    let outcome = other.maintain(SETTLED).await.expect("sweep runs");
    assert!(
        outcome.ran(),
        "uncontended maintain must run, got {outcome:?}"
    );
    let status = other.maintenance_status();
    assert!(
        status.last_prune_unix.is_some() && status.last_compact_unix.is_some(),
        "one maintain records both operations, got {status:?}"
    );
}

#[tokio::test]
async fn maintenance_status_is_shared_across_processes_and_throttles_the_read_path() {
    // The read-path throttle was per-PROCESS, which is no throttle at all
    // for `bobbin status`: every invocation is a fresh process whose
    // in-memory "last compacted" is never, so every one took the
    // store-wide lock. The incremental service runs it on a schedule —
    // that is what starved the nightly.
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();
    store
        .insert(
            &[sample_chunk("chunk1", "main")],
            &[sample_embedding()],
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    assert_eq!(
        store.maintenance_status(),
        MaintenanceStatus::default(),
        "a never-swept store reports no maintenance"
    );

    store
        .compact(SETTLED)
        .await
        .expect("compact runs")
        .ran()
        .then_some(())
        .expect("compact ran");

    let status = store.maintenance_status();
    assert!(
        status.last_compact_unix.is_some(),
        "a completed sweep is recorded on disk"
    );
    assert!(
        status.last_prune_unix.is_none(),
        "compact must not claim a prune it did not do"
    );

    // A DIFFERENT handle — standing in for the next `bobbin status`
    // process — sees the record and must not take the lock at all.
    let fresh = VectorStore::open(&path).await.unwrap();
    assert_eq!(
        fresh.maintenance_status(),
        status,
        "the record is visible to a fresh process"
    );
    // Hold the lock so any attempt would be a visible skip.
    let guard = take_lock_settled(&store);
    fresh.compact_if_stale().await;
    drop(guard);
    // If compact_if_stale had attempted, it would have recorded an attempt
    // in its in-process throttle; the cross-process gate short-circuits
    // before that.
    assert_eq!(
        fresh.last_compact_secs.load(Ordering::Relaxed),
        0,
        "a recently-maintained store must not be re-attempted by the read path"
    );
}

#[tokio::test]
async fn test_needs_reindex() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    // No data yet - needs reindex
    assert!(store
        .needs_reindex("src/main.rs", "abc123", None)
        .await
        .unwrap());

    let chunks = vec![sample_chunk("chunk1", "main")];
    let embeddings = vec![sample_embedding()];
    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    // Same hash - no reindex needed
    assert!(!store
        .needs_reindex("src/main.rs", "abc123", None)
        .await
        .unwrap());

    // Different hash - needs reindex
    assert!(store
        .needs_reindex("src/main.rs", "different", None)
        .await
        .unwrap());

    // Different file - needs reindex
    assert!(store
        .needs_reindex("src/other.rs", "abc123", None)
        .await
        .unwrap());

    // Repo-scoped: another repo holding the same path+hash
    // must not satisfy THIS repo's check — unscoped it reads "already
    // indexed" while this repo has no rows at all.
    assert!(
        !store
            .needs_reindex("src/main.rs", "abc123", Some("default"))
            .await
            .unwrap(),
        "the owning repo is satisfied"
    );
    assert!(
        store
            .needs_reindex("src/main.rs", "abc123", Some("other-repo"))
            .await
            .unwrap(),
        "a repo with no rows needs indexing even though the path+hash exists under 'default'"
    );
}

#[tokio::test]
async fn test_get_stats() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![
        Chunk {
            id: "chunk1".to_string(),
            file_path: "src/main.rs".to_string(),
            chunk_type: ChunkType::Function,
            name: Some("main".to_string()),
            start_line: 1,
            end_line: 10,
            content: "fn main() {}".to_string(),
            language: "rust".to_string(),
            tags: String::new(),
        },
        Chunk {
            id: "chunk2".to_string(),
            file_path: "src/main.rs".to_string(),
            chunk_type: ChunkType::Function,
            name: Some("helper".to_string()),
            start_line: 12,
            end_line: 20,
            content: "fn helper() {}".to_string(),
            language: "rust".to_string(),
            tags: String::new(),
        },
        Chunk {
            id: "chunk3".to_string(),
            file_path: "src/script.py".to_string(),
            chunk_type: ChunkType::Function,
            name: Some("run".to_string()),
            start_line: 1,
            end_line: 5,
            content: "def run(): pass".to_string(),
            language: "python".to_string(),
            tags: String::new(),
        },
    ];
    let embeddings = vec![sample_embedding(), sample_embedding(), sample_embedding()];

    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(3),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    let stats = store.get_stats(None).await.unwrap();
    assert_eq!(stats.total_files, 2);
    assert_eq!(stats.total_chunks, 3);

    let rust_stats = stats
        .languages
        .iter()
        .find(|l| l.language == "rust")
        .unwrap();
    assert_eq!(rust_stats.file_count, 1);
    assert_eq!(rust_stats.chunk_count, 2);

    let python_stats = stats
        .languages
        .iter()
        .find(|l| l.language == "python")
        .unwrap();
    assert_eq!(python_stats.file_count, 1);
    assert_eq!(python_stats.chunk_count, 1);
}

/// Regression test: delete_by_file + insert per file (the real indexing
/// pattern) creates heavy fragmentation. Without compaction, the stats
/// scan must still return all rows — not just the LanceDB default 10.
#[tokio::test]
async fn test_get_stats_fragmented_upserts() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    // Phase 1: Initial insert of 30 files across 3 languages
    let languages = ["rust", "python", "markdown"];
    for i in 0..30 {
        let lang = languages[i % languages.len()];
        let ext = match lang {
            "rust" => "rs",
            "python" => "py",
            _ => "md",
        };
        let fp = format!("src/file_{i}.{ext}");

        let chunk = Chunk {
            id: format!("chunk_{i}"),
            file_path: fp.clone(),
            chunk_type: ChunkType::Function,
            name: Some(format!("func_{i}")),
            start_line: 1,
            end_line: 10,
            content: format!("content of chunk {i}"),
            language: lang.to_string(),
            tags: String::new(),
        };

        store
            .insert(
                &[chunk],
                &[sample_embedding()],
                &no_contexts(1),
                "default",
                &format!("hash_{i}_v1"),
                "1000",
            )
            .await
            .unwrap();
    }

    // Phase 2: Re-index all files (delete_by_file + insert), simulating
    // incremental re-indexing WITHOUT compaction in between.
    for i in 0..30 {
        let lang = languages[i % languages.len()];
        let ext = match lang {
            "rust" => "rs",
            "python" => "py",
            _ => "md",
        };
        let fp = format!("src/file_{i}.{ext}");

        // This is the real-world pattern from cli/index.rs
        store.delete_by_file(&[fp.clone()], None).await.unwrap();

        let chunk = Chunk {
            id: format!("chunk_{i}"),
            file_path: fp,
            chunk_type: ChunkType::Function,
            name: Some(format!("func_{i}_v2")),
            start_line: 1,
            end_line: 10,
            content: format!("updated content of chunk {i}"),
            language: lang.to_string(),
            tags: String::new(),
        };

        store
            .insert(
                &[chunk],
                &[sample_embedding()],
                &no_contexts(1),
                "default",
                &format!("hash_{i}_v2"),
                "2000",
            )
            .await
            .unwrap();
    }

    // NO compaction — stats must still be correct
    let stats = store.get_stats(None).await.unwrap();
    assert_eq!(stats.total_chunks, 30, "total chunks from count_rows");
    assert_eq!(stats.total_files, 30, "total files from scan");

    let total_lang_chunks: u64 = stats.languages.iter().map(|l| l.chunk_count).sum();
    assert_eq!(
        total_lang_chunks, 30,
        "per-language chunk sum must equal total — got breakdown: {:?}",
        stats.languages
    );

    assert_eq!(stats.languages.len(), 3);
    for lang_stat in &stats.languages {
        assert_eq!(
            lang_stat.chunk_count, 10,
            "{} should have 10 chunks",
            lang_stat.language
        );
        assert_eq!(
            lang_stat.file_count, 10,
            "{} should have 10 files",
            lang_stat.language
        );
    }
}

/// Regression test: many per-file inserts caused get_stats to return only
/// 10 rows because LanceDB Query defaults to limit=10 (DEFAULT_TOP_K).
#[tokio::test]
async fn test_get_stats_many_inserts() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    // Simulate per-file inserts (the real-world pattern that triggers the bug)
    let languages = ["rust", "python", "markdown", "typescript"];
    for i in 0..20 {
        let lang = languages[i % languages.len()];
        let chunk = Chunk {
            id: format!("frag_chunk_{i}"),
            file_path: format!("src/file_{i}.{}", if lang == "rust" { "rs" } else { lang }),
            chunk_type: if lang == "markdown" {
                ChunkType::Section
            } else {
                ChunkType::Function
            },
            name: Some(format!("func_{i}")),
            start_line: 1,
            end_line: 10,
            content: format!("content of chunk {i}"),
            language: lang.to_string(),
            tags: String::new(),
        };

        store
            .insert(
                &[chunk],
                &[sample_embedding()],
                &no_contexts(1),
                "default",
                &format!("hash_{i}"),
                "1234567890",
            )
            .await
            .unwrap();
    }

    let stats = store.get_stats(None).await.unwrap();
    assert_eq!(stats.total_chunks, 20);
    assert_eq!(stats.total_files, 20);

    // Per-language breakdown must account for ALL chunks, not just 10
    let total_lang_chunks: u64 = stats.languages.iter().map(|l| l.chunk_count).sum();
    assert_eq!(
        total_lang_chunks, 20,
        "per-language chunk sum must equal total — got breakdown: {:?}",
        stats.languages
    );

    // 20 chunks across 4 languages = 5 each
    assert_eq!(stats.languages.len(), 4);
    for lang_stat in &stats.languages {
        assert_eq!(
            lang_stat.chunk_count, 5,
            "{} should have 5 chunks",
            lang_stat.language
        );
        assert_eq!(
            lang_stat.file_count, 5,
            "{} should have 5 files",
            lang_stat.language
        );
    }
}

#[tokio::test]
async fn test_get_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![sample_chunk("chunk1", "main")];
    let embeddings = vec![sample_embedding()];
    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    let file = store.get_file("src/main.rs").await.unwrap();
    assert!(file.is_some());
    let file = file.unwrap();
    assert_eq!(file.path, "src/main.rs");
    assert_eq!(file.hash, "abc123");

    let no_file = store.get_file("nonexistent.rs").await.unwrap();
    assert!(no_file.is_none());
}

#[tokio::test]
async fn test_multi_repo_search() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    // Insert chunks into two different repos
    let chunk_a = Chunk {
        id: "repo_a_chunk1".to_string(),
        file_path: "src/main.rs".to_string(),
        chunk_type: ChunkType::Function,
        name: Some("main_a".to_string()),
        start_line: 1,
        end_line: 10,
        content: "fn main_a() {}".to_string(),
        language: "rust".to_string(),
        tags: String::new(),
    };
    let chunk_b = Chunk {
        id: "repo_b_chunk1".to_string(),
        file_path: "src/main.rs".to_string(),
        chunk_type: ChunkType::Function,
        name: Some("main_b".to_string()),
        start_line: 1,
        end_line: 10,
        content: "fn main_b() {}".to_string(),
        language: "rust".to_string(),
        tags: String::new(),
    };

    store
        .insert(
            &[chunk_a],
            &[sample_embedding()],
            &no_contexts(1),
            "repo_a",
            "hash_a",
            "100",
        )
        .await
        .unwrap();
    store
        .insert(
            &[chunk_b],
            &[sample_embedding()],
            &no_contexts(1),
            "repo_b",
            "hash_b",
            "200",
        )
        .await
        .unwrap();

    // Search all repos
    let all_results = store.search(&sample_embedding(), 10, None).await.unwrap();
    assert_eq!(all_results.len(), 2);

    // Search specific repo
    let repo_a_results = store
        .search(&sample_embedding(), 10, Some("repo_a"))
        .await
        .unwrap();
    assert_eq!(repo_a_results.len(), 1);
    assert_eq!(repo_a_results[0].chunk.name, Some("main_a".to_string()));

    let repo_b_results = store
        .search(&sample_embedding(), 10, Some("repo_b"))
        .await
        .unwrap();
    assert_eq!(repo_b_results.len(), 1);
    assert_eq!(repo_b_results[0].chunk.name, Some("main_b".to_string()));

    // Get all repos
    let repos = store.get_all_repos().await.unwrap();
    assert_eq!(repos.len(), 2);
    assert!(repos.contains(&"repo_a".to_string()));
    assert!(repos.contains(&"repo_b".to_string()));

    // Get stats filtered by repo
    let stats_a = store.get_stats(Some("repo_a")).await.unwrap();
    assert_eq!(stats_a.total_chunks, 1);

    let stats_all = store.get_stats(None).await.unwrap();
    assert_eq!(stats_all.total_chunks, 2);

    // Get file paths filtered by repo
    let paths_a = store.get_all_file_paths(Some("repo_a")).await.unwrap();
    assert_eq!(paths_a.len(), 1);
}

#[tokio::test]
async fn test_delete_large_batch_no_in_clause_overflow() {
    // Regression: a single `id IN (...)` with hundreds of ids overflowed the
    // query engine ("Failed to delete chunks"), so large bead batches (632)
    // failed to store via insert()'s upsert-delete. delete() now batches.
    let dir = tempdir().unwrap();
    let mut store = VectorStore::open(&dir.path().join("vectors"))
        .await
        .unwrap();

    let n = 250usize; // > DELETE_BATCH (100) -> exercises multi-batch delete
    let chunks: Vec<Chunk> = (0..n)
        .map(|i| Chunk {
            id: format!("c{i}"),
            file_path: format!("beads:rig:bo-{i}"),
            chunk_type: ChunkType::Issue,
            name: Some(format!("bead {i}")),
            start_line: 0,
            end_line: 0,
            content: format!("bead content {i}"),
            language: "beads".to_string(),
            tags: String::new(),
        })
        .collect();
    let embs: Vec<Vec<f32>> = (0..n).map(|_| sample_embedding()).collect();
    store
        .insert(
            &chunks,
            &embs,
            &no_contexts(n),
            "beads-issues",
            "beads",
            "100",
        )
        .await
        .expect("insert 250 beads should succeed (upsert-delete batched)");

    // Delete all 250 by id — must not overflow the IN clause.
    let ids: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
    store.delete(&ids).await.expect("batched delete of 250 ids");

    // Re-insert should also succeed (upsert path deletes 250 first).
    store
        .insert(
            &chunks,
            &embs,
            &no_contexts(n),
            "beads-issues",
            "beads",
            "101",
        )
        .await
        .expect("re-insert 250 beads should succeed");
}

#[tokio::test]
async fn test_fts_search_survives_compaction() {
    // Regression for GH#21: keyword/FTS search 500'd ("Failed to collect FTS
    // results") when the FTS index was missing or invalidated by compaction.
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");
    let mut store = VectorStore::open(&path).await.unwrap();

    let chunk = Chunk {
        id: "c1".to_string(),
        file_path: "src/auth.rs".to_string(),
        chunk_type: ChunkType::Function,
        name: Some("authenticate".to_string()),
        start_line: 1,
        end_line: 10,
        content: "fn authenticate(user: &str) -> bool { validate_token(user) }".to_string(),
        language: "rust".to_string(),
        tags: String::new(),
    };
    store
        .insert(
            &[chunk],
            &[sample_embedding()],
            &no_contexts(1),
            "repo",
            "h",
            "100",
        )
        .await
        .unwrap();

    // FTS works initially.
    let r1 = store.search_fts("authenticate", 10, None).await.unwrap();
    assert_eq!(r1.len(), 1, "FTS should find the chunk before compaction");

    // Compaction can invalidate the FTS index; search must still succeed
    // (self-heal rebuild) rather than 500.
    store.compact(SETTLED).await.unwrap();
    let r2 = store.search_fts("authenticate", 10, None).await.unwrap();
    assert_eq!(r2.len(), 1, "FTS should still work after compaction");

    // Explicit rebuild is idempotent and keeps search working.
    let rebuilds_before = crate::operational_metrics::fts_rebuild_total();
    store.rebuild_fts_index().await.unwrap();
    assert!(
        crate::operational_metrics::fts_rebuild_total() >= rebuilds_before + 1,
        "a successful rebuild must increment its operational counter"
    );
    let r3 = store.search_fts("validate_token", 10, None).await.unwrap();
    assert_eq!(r3.len(), 1, "FTS should work after explicit rebuild");
}

#[tokio::test]
async fn test_get_chunks_for_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![
        Chunk {
            id: "chunk1".to_string(),
            file_path: "src/a.rs".to_string(),
            chunk_type: ChunkType::Function,
            name: Some("func_b".to_string()),
            start_line: 20,
            end_line: 30,
            content: "fn func_b() {}".to_string(),
            language: "rust".to_string(),
            tags: String::new(),
        },
        Chunk {
            id: "chunk2".to_string(),
            file_path: "src/a.rs".to_string(),
            chunk_type: ChunkType::Function,
            name: Some("func_a".to_string()),
            start_line: 1,
            end_line: 10,
            content: "fn func_a() {}".to_string(),
            language: "rust".to_string(),
            tags: String::new(),
        },
        Chunk {
            id: "chunk3".to_string(),
            file_path: "src/b.rs".to_string(),
            chunk_type: ChunkType::Function,
            name: Some("other".to_string()),
            start_line: 1,
            end_line: 5,
            content: "fn other() {}".to_string(),
            language: "rust".to_string(),
            tags: String::new(),
        },
    ];
    let embeddings = vec![sample_embedding(), sample_embedding(), sample_embedding()];

    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(3),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    // Get chunks for src/a.rs - should return 2 chunks sorted by start_line
    let result = store.get_chunks_for_file("src/a.rs", None).await.unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].start_line, 1); // func_a first
    assert_eq!(result[1].start_line, 20); // func_b second

    // Get chunks for src/b.rs - should return 1 chunk
    let result = store.get_chunks_for_file("src/b.rs", None).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, Some("other".to_string()));

    // Get chunks for unknown file - should return empty
    let result = store.get_chunks_for_file("unknown.rs", None).await.unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_insert_with_full_context() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![
        sample_chunk("chunk1", "main"),
        sample_chunk("chunk2", "helper"),
    ];
    let embeddings = vec![sample_embedding(), sample_embedding()];
    let contexts = vec![
        Some("// context before\nfn main() { }\n// context after".to_string()),
        None, // No context for this chunk
    ];

    store
        .insert(
            &chunks,
            &embeddings,
            &contexts,
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    assert_eq!(store.count().await.unwrap(), 2);

    // Verify chunks are searchable
    let results = store.search(&sample_embedding(), 10, None).await.unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn test_custom_embedding_dimension() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    // Open with custom 768-dim instead of default 384
    let mut store = VectorStore::open_with_dim(&path, 768).await.unwrap();
    assert_eq!(store.embedding_dim(), 768);

    let chunks = vec![sample_chunk("chunk1", "main")];
    // Create a 768-dim embedding
    let mut emb: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
    let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
    emb.iter_mut().for_each(|x| *x /= norm);

    store
        .insert(
            &chunks,
            &[emb.clone()],
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    assert_eq!(store.count().await.unwrap(), 1);

    let results = store.search(&emb, 10, None).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, Some("main".to_string()));
}

#[tokio::test]
async fn test_get_chunk_embedding() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let chunks = vec![sample_chunk("chunk1", "main")];
    let embedding = sample_embedding();

    store
        .insert(
            &chunks,
            &[embedding.clone()],
            &no_contexts(1),
            "default",
            "abc123",
            "1234567890",
        )
        .await
        .unwrap();

    // Retrieve the stored embedding
    let retrieved = store.get_chunk_embedding("chunk1").await.unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.len(), 384);

    // Verify values match what we inserted
    for (a, b) in embedding.iter().zip(retrieved.iter()) {
        assert!((a - b).abs() < 1e-6, "Embedding values should match");
    }

    // Non-existent chunk returns None
    let missing = store.get_chunk_embedding("nonexistent").await.unwrap();
    assert!(missing.is_none());
}

// ── Dependency tests ──────────────────────────────────────────────

fn sample_dep(file_a: &str, file_b: &str, resolved: bool) -> ImportDependency {
    ImportDependency {
        file_a: file_a.to_string(),
        file_b: file_b.to_string(),
        dep_type: "import".to_string(),
        import_statement: format!("use {};", file_b),
        symbol: None,
        resolved,
    }
}

#[tokio::test]
async fn test_deps_insert_and_query() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let deps = vec![ImportDependency {
        file_a: "src/main.rs".to_string(),
        file_b: "src/types.rs".to_string(),
        dep_type: "use".to_string(),
        import_statement: "use crate::types::Chunk;".to_string(),
        symbol: Some("Chunk".to_string()),
        resolved: true,
    }];
    store.upsert_dependencies(&deps).await.unwrap();

    let result = store.get_dependencies("src/main.rs").await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].file_b, "src/types.rs");
    assert_eq!(result[0].dep_type, "use");
    assert_eq!(result[0].symbol, Some("Chunk".to_string()));
    assert!(result[0].resolved);
}

#[tokio::test]
async fn test_deps_reverse_lookup() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let deps = vec![
        sample_dep("src/main.rs", "src/types.rs", true),
        sample_dep("src/cli/search.rs", "src/types.rs", true),
    ];
    store.upsert_dependencies(&deps).await.unwrap();

    let dependents = store.get_dependents("src/types.rs").await.unwrap();
    assert_eq!(dependents.len(), 2);
}

#[tokio::test]
async fn test_deps_clear_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let deps = vec![
        sample_dep("src/a.rs", "src/b.rs", true),
        sample_dep("src/c.rs", "src/b.rs", true),
    ];
    store.upsert_dependencies(&deps).await.unwrap();

    // Clear only a.rs deps
    store.clear_file_dependencies("src/a.rs").await.unwrap();

    assert_eq!(store.get_dependencies("src/a.rs").await.unwrap().len(), 0);
    assert_eq!(store.get_dependencies("src/c.rs").await.unwrap().len(), 1);
}

#[tokio::test]
async fn test_deps_stats() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let deps = vec![
        sample_dep("src/a.rs", "src/b.rs", true),
        sample_dep("src/a.rs", "unresolved:anyhow::Result", false),
    ];
    store.upsert_dependencies(&deps).await.unwrap();

    let (total, resolved) = store.get_dependency_stats().await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(resolved, 1);
}

#[tokio::test]
async fn test_deps_empty_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let store = VectorStore::open(&path).await.unwrap();

    let deps = store.get_dependencies("src/a.rs").await.unwrap();
    assert!(deps.is_empty());

    let (total, resolved) = store.get_dependency_stats().await.unwrap();
    assert_eq!(total, 0);
    assert_eq!(resolved, 0);
}

#[tokio::test]
async fn test_deps_persist_across_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    {
        let mut store = VectorStore::open(&path).await.unwrap();
        let deps = vec![sample_dep("src/a.rs", "src/b.rs", true)];
        store.upsert_dependencies(&deps).await.unwrap();
    }

    {
        let store = VectorStore::open(&path).await.unwrap();
        let deps = store.get_dependencies("src/a.rs").await.unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].file_b, "src/b.rs");
    }
}

#[tokio::test]
async fn test_schema_migration_drops_stale_table() {
    // Simulate deploying a new bobbin with an extra column: create a table
    // with the OLD schema (missing "tags"), then reopen with the current
    // code which expects "tags". The store should drop the stale table.
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");
    std::fs::create_dir_all(&path).unwrap();

    let conn = lancedb::connect(path.to_str().unwrap())
        .execute()
        .await
        .unwrap();

    // Old schema: same as current but WITHOUT the "tags" field
    let old_schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(VectorStore::vector_field(), DEFAULT_EMBEDDING_DIM),
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
        // no "tags" field — this is the old schema
    ]));

    // Insert a dummy row to create the table with the old schema
    let emb = sample_embedding();
    let flat: Vec<f32> = emb.clone();
    let vector_values: ArrayRef = Arc::new(Float32Array::from(flat));
    let vector_array = FixedSizeListArray::try_new(
        VectorStore::vector_field(),
        DEFAULT_EMBEDDING_DIM,
        vector_values,
        None,
    )
    .unwrap();

    let batch = RecordBatch::try_new(
        old_schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["old1"])),
            Arc::new(vector_array),
            Arc::new(StringArray::from(vec!["testrepo"])),
            Arc::new(StringArray::from(vec!["src/old.rs"])),
            Arc::new(StringArray::from(vec!["hash1"])),
            Arc::new(StringArray::from(vec!["rust"])),
            Arc::new(StringArray::from(vec!["function"])),
            Arc::new(StringArray::from(vec![Some("main")])),
            Arc::new(UInt32Array::from(vec![1u32])),
            Arc::new(UInt32Array::from(vec![10u32])),
            Arc::new(StringArray::from(vec!["fn main() {}"])),
            Arc::new(StringArray::from(vec![None::<&str>])),
            Arc::new(StringArray::from(vec!["2026-01-01"])),
        ],
    )
    .unwrap();

    let reader: Box<dyn arrow_array::RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(vec![Ok(batch)], old_schema));
    conn.create_table(TABLE_NAME, reader)
        .execute()
        .await
        .unwrap();

    // Verify old table exists
    let tables = conn.table_names().execute().await.unwrap();
    assert!(tables.contains(&TABLE_NAME.to_string()));

    // Drop the direct connection before VectorStore opens
    drop(conn);

    // Re-open via VectorStore — should detect schema mismatch and drop table
    let mut store = VectorStore::open(&path).await.unwrap();
    // Table was dropped, so count should be 0 (or table is None)
    assert_eq!(store.count().await.unwrap(), 0);

    // Insert with new schema should succeed (table gets recreated)
    let chunks = vec![sample_chunk("new1", "main")];
    let embeddings = vec![sample_embedding()];
    store
        .insert(
            &chunks,
            &embeddings,
            &no_contexts(1),
            "testrepo",
            "hash2",
            "2026-03-02",
        )
        .await
        .unwrap();
    assert_eq!(store.count().await.unwrap(), 1);
}

// ── Entity table tests ──────────────────────────────────────────────

fn sample_entity(iri: &str, entity_type: &str) -> Entity {
    Entity {
        entity_iri: iri.to_string(),
        text: format!("Entity content for {}", iri),
        entity_type: entity_type.to_string(),
        repo: Some("testrepo".to_string()),
    }
}

#[tokio::test]
async fn test_entities_upsert_and_count() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();
    assert_eq!(store.count_entities().await.unwrap(), 0);

    let entities = vec![
        sample_entity("bobbin:code/repo/src/main.rs", "CodeModule"),
        sample_entity("bobbin:code/repo/src/main.rs::main", "CodeSymbol"),
    ];
    let embeddings = vec![sample_embedding(), sample_embedding()];

    store.upsert_entities(&entities, &embeddings).await.unwrap();
    assert_eq!(store.count_entities().await.unwrap(), 2);
}

#[tokio::test]
async fn test_entities_upsert_replaces_existing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let entities = vec![sample_entity("bobbin:code/repo/src/a.rs", "CodeModule")];
    let embeddings = vec![sample_embedding()];
    store.upsert_entities(&entities, &embeddings).await.unwrap();
    assert_eq!(store.count_entities().await.unwrap(), 1);

    // Upsert same IRI — should replace, not duplicate
    let updated = vec![Entity {
        entity_iri: "bobbin:code/repo/src/a.rs".to_string(),
        text: "Updated content".to_string(),
        entity_type: "CodeModule".to_string(),
        repo: Some("testrepo".to_string()),
    }];
    store.upsert_entities(&updated, &embeddings).await.unwrap();
    assert_eq!(store.count_entities().await.unwrap(), 1);
}

#[tokio::test]
async fn test_entities_search() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let entities = vec![
        sample_entity("bobbin:code/repo/src/a.rs", "CodeModule"),
        sample_entity("bobbin:code/repo/src/b.rs", "CodeModule"),
    ];
    let embeddings = vec![sample_embedding(), sample_embedding()];
    store.upsert_entities(&entities, &embeddings).await.unwrap();

    let results = store
        .search_entities(&sample_embedding(), 10, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].score > 0.0);
}

#[tokio::test]
async fn test_entities_search_with_repo_filter() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let entities = vec![
        sample_entity("bobbin:code/repo1/a.rs", "CodeModule"),
        Entity {
            entity_iri: "bobbin:code/repo2/b.rs".to_string(),
            text: "Other repo entity".to_string(),
            entity_type: "CodeModule".to_string(),
            repo: Some("otherrepo".to_string()),
        },
    ];
    let embeddings = vec![sample_embedding(), sample_embedding()];
    store.upsert_entities(&entities, &embeddings).await.unwrap();

    let results = store
        .search_entities(&sample_embedding(), 10, Some("testrepo"))
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entity_iri, "bobbin:code/repo1/a.rs");
}

#[tokio::test]
async fn test_entities_get_embedding() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let entities = vec![sample_entity("bobbin:code/repo/src/a.rs", "CodeModule")];
    let emb = sample_embedding();
    store
        .upsert_entities(&entities, &vec![emb.clone()])
        .await
        .unwrap();

    let retrieved = store
        .get_entity_embedding("bobbin:code/repo/src/a.rs")
        .await
        .unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.len(), 384);
    // Verify the embedding matches (within floating-point tolerance)
    assert!((retrieved[0] - emb[0]).abs() < 1e-6);
}

#[tokio::test]
async fn test_entities_delete() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let entities = vec![
        sample_entity("bobbin:code/repo/a.rs", "CodeModule"),
        sample_entity("bobbin:code/repo/b.rs", "CodeModule"),
    ];
    let embeddings = vec![sample_embedding(), sample_embedding()];
    store.upsert_entities(&entities, &embeddings).await.unwrap();
    assert_eq!(store.count_entities().await.unwrap(), 2);

    store
        .delete_entities(&["bobbin:code/repo/a.rs"])
        .await
        .unwrap();
    assert_eq!(store.count_entities().await.unwrap(), 1);
}

#[tokio::test]
async fn test_entities_delete_by_repo() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let entities = vec![
        sample_entity("bobbin:code/repo1/a.rs", "CodeModule"),
        Entity {
            entity_iri: "bobbin:code/repo2/b.rs".to_string(),
            text: "Other entity".to_string(),
            entity_type: "CodeModule".to_string(),
            repo: Some("otherrepo".to_string()),
        },
    ];
    let embeddings = vec![sample_embedding(), sample_embedding()];
    store.upsert_entities(&entities, &embeddings).await.unwrap();
    assert_eq!(store.count_entities().await.unwrap(), 2);

    store.delete_entities_by_repo("testrepo").await.unwrap();
    assert_eq!(store.count_entities().await.unwrap(), 1);
}

#[tokio::test]
async fn test_entities_nullable_repo() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut store = VectorStore::open(&path).await.unwrap();

    let entities = vec![Entity {
        entity_iri: "bobbin:bundle/cross-repo-thing".to_string(),
        text: "Cross-repo bundle".to_string(),
        entity_type: "Bundle".to_string(),
        repo: None,
    }];
    let embeddings = vec![sample_embedding()];
    store.upsert_entities(&entities, &embeddings).await.unwrap();
    assert_eq!(store.count_entities().await.unwrap(), 1);

    let results = store
        .search_entities(&sample_embedding(), 10, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].repo.is_none());
}

#[tokio::test]
async fn test_entities_persist_across_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    {
        let mut store = VectorStore::open(&path).await.unwrap();
        let entities = vec![sample_entity("bobbin:code/repo/a.rs", "CodeModule")];
        let embeddings = vec![sample_embedding()];
        store.upsert_entities(&entities, &embeddings).await.unwrap();
    }

    {
        let store = VectorStore::open(&path).await.unwrap();
        assert_eq!(store.count_entities().await.unwrap(), 1);
    }
}

#[tokio::test]
async fn test_entities_empty_search() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let store = VectorStore::open(&path).await.unwrap();
    let results = store
        .search_entities(&sample_embedding(), 10, None)
        .await
        .unwrap();
    assert!(results.is_empty());
}

/// aegis-mgpp28: a store opened on a table that ALREADY has an FTS index must not
/// ask Lance to build one. The server opens a fresh `VectorStore` per request, so
/// `fts_indexed` never survives between requests; before the fix every such store
/// trained the whole-corpus index before discovering it existed.
#[tokio::test]
async fn a_fresh_store_over_an_existing_fts_index_never_rebuilds_it() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");

    let mut first = VectorStore::open(&path).await.unwrap();
    first
        .insert(
            &[sample_chunk("c1", "authenticate")],
            &[sample_embedding()],
            &no_contexts(1),
            "repo",
            "h",
            "100",
        )
        .await
        .unwrap();
    first.rebuild_fts_index().await.unwrap();
    assert!(
        first.fts_build_attempts() >= 1,
        "fixture: the first store builds the index"
    );

    // A second store on the same table: the per-request shape in production.
    let mut fresh = VectorStore::open(&path).await.unwrap();
    let found = fresh.search_fts("authenticate", 10, None).await.unwrap();
    assert_eq!(found.len(), 1, "the existing index answers");
    // A write resets the store's flag; the re-check must still find the index.
    fresh
        .insert(
            &[sample_chunk("c2", "authorize")],
            &[sample_embedding()],
            &no_contexts(1),
            "repo",
            "h",
            "101",
        )
        .await
        .unwrap();
    let again = fresh.search_fts("authenticate", 10, None).await.unwrap();
    assert_eq!(again.len(), 1);
    let new_row = fresh.search_fts("authorize", 10, None).await.unwrap();
    assert_eq!(
        new_row.len(),
        1,
        "rows added after the index are still searchable"
    );
    assert_eq!(
        fresh.fts_build_attempts(),
        0,
        "a store over an existing FTS index must never train a new one"
    );
}

/// Negative control: with NO FTS index, `ensure_fts_index` must still build one,
/// so the detection above cannot be passing by never building at all.
#[tokio::test]
async fn a_store_without_an_fts_index_still_builds_it_once() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");
    let mut store = VectorStore::open(&path).await.unwrap();
    store
        .insert(
            &[sample_chunk("c1", "authenticate")],
            &[sample_embedding()],
            &no_contexts(1),
            "repo",
            "h",
            "100",
        )
        .await
        .unwrap();
    // Whatever insert did, ask a FRESH store: if no index exists it must build.
    let fresh = VectorStore::open(&path).await.unwrap();
    let had_index = VectorStore::has_content_fts_index(fresh.table.as_ref().unwrap()).await;
    fresh.ensure_fts_index().await.unwrap();
    assert_eq!(fresh.fts_build_attempts(), if had_index { 0 } else { 1 });
    let again = VectorStore::open(&path).await.unwrap();
    assert!(
        VectorStore::has_content_fts_index(again.table.as_ref().unwrap()).await,
        "an index exists afterwards"
    );
    again.ensure_fts_index().await.unwrap();
    assert_eq!(again.fts_build_attempts(), 0, "and is then never rebuilt");
}

/// aegis-4h7zw8: rows inserted after the FTS index was built are NOT covered by
/// it (Lance text-scans them on every FTS query), and `fts_coverage` says so.
/// Control: right after a build, everything is covered; with no index, None.
#[tokio::test]
async fn fts_coverage_counts_rows_added_after_the_index_as_unindexed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");
    let mut store = VectorStore::open(&path).await.unwrap();
    store
        .insert(
            &[
                sample_chunk("c1", "authenticate"),
                sample_chunk("c2", "authorize"),
            ],
            &[sample_embedding(), sample_embedding()],
            &no_contexts(2),
            "repo",
            "h",
            "100",
        )
        .await
        .unwrap();
    let bare = VectorStore::open(&path).await.unwrap();
    if !VectorStore::has_content_fts_index(bare.table.as_ref().unwrap()).await {
        assert_eq!(bare.fts_coverage().await, None, "no index, no coverage");
    }
    store.rebuild_fts_index().await.unwrap();
    let built = VectorStore::open(&path).await.unwrap();
    assert_eq!(
        built.fts_coverage().await,
        Some((2, 0)),
        "control: a fresh build covers all"
    );
    let mut later = VectorStore::open(&path).await.unwrap();
    later
        .insert(
            &[sample_chunk("c3", "authorize again")],
            &[sample_embedding()],
            &no_contexts(1),
            "repo",
            "h",
            "101",
        )
        .await
        .unwrap();
    let after = VectorStore::open(&path).await.unwrap();
    assert_eq!(
        after.fts_coverage().await,
        Some((2, 1)),
        "the new row is outside the index"
    );
}

/// aegis-4h7zw8 fix: the maintenance sweep folds rows added after the FTS
/// index was built into it, so no row is left to be text-scanned per query,
/// and keyword search still finds every row (old and new) afterwards.
#[tokio::test]
async fn maintain_folds_unindexed_rows_into_the_fts_index() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vectors");
    let mut store = VectorStore::open(&path).await.unwrap();
    store
        .insert(
            &[
                sample_chunk("c1", "authenticate"),
                sample_chunk("c2", "authorize"),
            ],
            &[sample_embedding(), sample_embedding()],
            &no_contexts(2),
            "repo",
            "h",
            "100",
        )
        .await
        .unwrap();
    store.rebuild_fts_index().await.unwrap();
    let mut later = VectorStore::open(&path).await.unwrap();
    later
        .insert(
            &[sample_chunk("c3", "tokenize")],
            &[sample_embedding()],
            &no_contexts(1),
            "repo",
            "h",
            "101",
        )
        .await
        .unwrap();
    let before = VectorStore::open(&path).await.unwrap();
    assert_eq!(
        before.fts_coverage().await,
        Some((2, 1)),
        "control: one row outside"
    );
    before.maintain(SETTLED).await.expect("sweep runs");
    let after = VectorStore::open(&path).await.unwrap();
    assert_eq!(
        after.fts_coverage().await.map(|c| c.1),
        Some(0),
        "nothing left unindexed"
    );
    for word in ["authenticate", "authorize", "tokenize"] {
        assert_eq!(
            after.search_fts(word, 10, None).await.unwrap().len(),
            1,
            "{word}"
        );
    }
}

/// wu's ask on bobbin#164: the optimize step reuses the compaction step's FTS
/// panic -> rebuild recovery, and ONLY for that panic on the chunks table.
#[test]
fn optimize_falls_back_to_an_fts_rebuild_only_for_the_builder_panic_on_chunks() {
    let panic: anyhow::Result<()> = Err(anyhow::anyhow!(
        "task 7 panicked with message at lance-index/src/scalar/inverted/builder.rs:412"
    ));
    let io: anyhow::Result<()> = Err(anyhow::anyhow!("I/O error: No space left on device"));
    let ok: anyhow::Result<()> = Ok(());
    assert!(should_rebuild_fts_after("chunks", &panic));
    assert!(
        !should_rebuild_fts_after("chunk_edges", &panic),
        "other tables: no FTS"
    );
    assert!(
        !should_rebuild_fts_after("chunks", &io),
        "a rebuild would not fix I/O"
    );
    assert!(!should_rebuild_fts_after("chunks", &ok));
}
