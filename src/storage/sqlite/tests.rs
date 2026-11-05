use super::*;
use tempfile::tempdir;

fn create_test_store() -> (MetadataStore, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = MetadataStore::open(&db_path).unwrap();
    (store, dir)
}

#[test]
fn test_open_creates_schema() {
    let (_store, _dir) = create_test_store();
    // Schema created without error
}

#[test]
fn test_coupling() {
    let (store, _dir) = create_test_store();

    let coupling = FileCoupling {
        file_a: "src/a.rs".to_string(),
        file_b: "src/b.rs".to_string(),
        score: 0.85,
        co_changes: 10,
        last_co_change: 1234567890,
    };

    store.upsert_coupling(&coupling).unwrap();

    let retrieved = store.get_coupling("src/a.rs", 10).unwrap();
    assert_eq!(retrieved.len(), 1);
    assert_eq!(retrieved[0].score, 0.85);
    assert_eq!(retrieved[0].co_changes, 10);
}

#[test]
fn test_coupling_update() {
    let (store, _dir) = create_test_store();

    let coupling = FileCoupling {
        file_a: "src/a.rs".to_string(),
        file_b: "src/b.rs".to_string(),
        score: 0.5,
        co_changes: 5,
        last_co_change: 1234567890,
    };
    store.upsert_coupling(&coupling).unwrap();

    // Update with higher score
    let updated = FileCoupling {
        file_a: "src/a.rs".to_string(),
        file_b: "src/b.rs".to_string(),
        score: 0.9,
        co_changes: 15,
        last_co_change: 9999999999,
    };
    store.upsert_coupling(&updated).unwrap();

    let retrieved = store.get_coupling("src/a.rs", 10).unwrap();
    assert_eq!(retrieved.len(), 1);
    assert_eq!(retrieved[0].score, 0.9);
    assert_eq!(retrieved[0].co_changes, 15);
}

#[test]
fn test_cross_repo_coupling_roundtrip() {
    use crate::types::CrossRepoCoupling;
    let (store, _dir) = create_test_store();

    let edge = CrossRepoCoupling {
        repo_a: "api".to_string(),
        path_a: "contract.rs".to_string(),
        repo_b: "web".to_string(),
        path_b: "client.ts".to_string(),
        score: 0.8,
        co_changes: 4,
        last_co_change: 1000,
    };
    store.upsert_cross_repo_coupling(&edge).unwrap();

    // Match on the seed's exact (repo, path), either side.
    let from_a = store
        .get_cross_repo_coupling(Some("api"), "contract.rs", 10)
        .unwrap();
    assert_eq!(from_a.len(), 1);
    assert_eq!(from_a[0].repo_b, "web");
    let from_b = store
        .get_cross_repo_coupling(Some("web"), "client.ts", 10)
        .unwrap();
    assert_eq!(from_b.len(), 1);

    // Wrong repo for the path -> no match (paths collide across repos).
    let wrong_repo = store
        .get_cross_repo_coupling(Some("other"), "contract.rs", 10)
        .unwrap();
    assert!(wrong_repo.is_empty());

    // Path-only match (seed repo unknown).
    let by_path = store
        .get_cross_repo_coupling(None, "contract.rs", 10)
        .unwrap();
    assert_eq!(by_path.len(), 1);

    // Upsert on the same canonical PK updates rather than duplicates.
    store
        .upsert_cross_repo_coupling(&CrossRepoCoupling {
            score: 0.95,
            co_changes: 9,
            ..edge.clone()
        })
        .unwrap();
    let updated = store
        .get_cross_repo_coupling(Some("api"), "contract.rs", 10)
        .unwrap();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].co_changes, 9);

    store.clear_cross_repo_coupling().unwrap();
    assert!(store
        .get_cross_repo_coupling(Some("api"), "contract.rs", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn test_meta() {
    let (store, _dir) = create_test_store();

    assert!(store.get_meta("model").unwrap().is_none());

    store.set_meta("model", "all-MiniLM-L6-v2").unwrap();
    assert_eq!(
        store.get_meta("model").unwrap(),
        Some("all-MiniLM-L6-v2".to_string())
    );

    store.set_meta("model", "bge-small-en-v1.5").unwrap();
    assert_eq!(
        store.get_meta("model").unwrap(),
        Some("bge-small-en-v1.5".to_string())
    );
}

#[test]
fn test_clear_coupling() {
    let (store, _dir) = create_test_store();

    store
        .upsert_coupling(&FileCoupling {
            file_a: "a.rs".to_string(),
            file_b: "b.rs".to_string(),
            score: 0.5,
            co_changes: 3,
            last_co_change: 0,
        })
        .unwrap();

    assert_eq!(store.get_coupling("a.rs", 10).unwrap().len(), 1);
    store.clear_coupling().unwrap();
    assert_eq!(store.get_coupling("a.rs", 10).unwrap().len(), 0);
}

#[test]
fn test_transaction() {
    let (store, _dir) = create_test_store();

    store.begin_transaction().unwrap();
    store
        .upsert_coupling(&FileCoupling {
            file_a: "a.rs".to_string(),
            file_b: "b.rs".to_string(),
            score: 0.5,
            co_changes: 3,
            last_co_change: 0,
        })
        .unwrap();
    store.commit().unwrap();

    assert_eq!(store.get_coupling("a.rs", 10).unwrap().len(), 1);
}

#[test]
fn test_file_hash_roundtrip() {
    let (store, _dir) = create_test_store();

    assert!(store.get_file_hash("r", "src/main.rs").unwrap().is_none());

    store.set_file_hash("r", "src/main.rs", "abc123").unwrap();
    assert_eq!(
        store.get_file_hash("r", "src/main.rs").unwrap(),
        Some("abc123".to_string())
    );

    // Update hash
    store.set_file_hash("r", "src/main.rs", "def456").unwrap();
    assert_eq!(
        store.get_file_hash("r", "src/main.rs").unwrap(),
        Some("def456".to_string())
    );
}

#[test]
fn test_same_path_in_two_repos_is_two_rows() {
    // THE multi-repo collision regression: 19 indexed repos each have a README.md, and the
    // old path-only key gave them ONE shared row — first writer owned it, every
    // other repo's copy was compared against the wrong hash and silently
    // skipped. The same path in different repos must be independent state.
    let (store, _dir) = create_test_store();

    store
        .set_file_hash("quipu", "README.md", "hash_quipu")
        .unwrap();
    store
        .set_file_hash("goldblum", "README.md", "hash_goldblum")
        .unwrap();

    assert_eq!(
        store.get_file_hash("quipu", "README.md").unwrap(),
        Some("hash_quipu".to_string()),
        "goldblum's write must not clobber quipu's row"
    );
    assert_eq!(
        store.get_file_hash("goldblum", "README.md").unwrap(),
        Some("hash_goldblum".to_string())
    );
    // And a repo that never wrote the path sees ABSENT, not another repo's hash.
    assert!(store
        .get_file_hash("yupana", "README.md")
        .unwrap()
        .is_none());
}

#[test]
fn test_file_hashes_bulk() {
    let (store, _dir) = create_test_store();

    let entries = vec![
        ("src/a.rs", "hash_a"),
        ("src/b.rs", "hash_b"),
        ("src/c.rs", "hash_c"),
    ];
    store.set_file_hashes_bulk("r", &entries).unwrap();

    assert_eq!(
        store.get_file_hash("r", "src/a.rs").unwrap(),
        Some("hash_a".to_string())
    );
    assert_eq!(
        store.get_file_hash("r", "src/b.rs").unwrap(),
        Some("hash_b".to_string())
    );
    assert_eq!(
        store.get_file_hash("r", "src/c.rs").unwrap(),
        Some("hash_c".to_string())
    );
}

#[test]
fn test_delete_file_hashes() {
    let (store, _dir) = create_test_store();

    store.set_file_hash("r", "src/a.rs", "hash_a").unwrap();
    store.set_file_hash("r", "src/b.rs", "hash_b").unwrap();
    store.set_file_hash("r", "src/c.rs", "hash_c").unwrap();

    store
        .delete_file_hashes(Some("r"), &["src/a.rs".to_string(), "src/c.rs".to_string()])
        .unwrap();

    assert!(store.get_file_hash("r", "src/a.rs").unwrap().is_none());
    assert_eq!(
        store.get_file_hash("r", "src/b.rs").unwrap(),
        Some("hash_b".to_string())
    );
    assert!(store.get_file_hash("r", "src/c.rs").unwrap().is_none());
}

#[test]
fn test_delete_file_hashes_scoped_spares_other_repos() {
    // A scoped delete for repo A's path must not touch repo B's row for the
    // same path; an unscoped (None) delete removes both.
    let (store, _dir) = create_test_store();

    store.set_file_hash("a", "README.md", "ha").unwrap();
    store.set_file_hash("b", "README.md", "hb").unwrap();

    store
        .delete_file_hashes(Some("a"), &["README.md".to_string()])
        .unwrap();
    assert!(store.get_file_hash("a", "README.md").unwrap().is_none());
    assert_eq!(
        store.get_file_hash("b", "README.md").unwrap(),
        Some("hb".to_string()),
        "scoped delete must spare the other repo's row"
    );

    store.set_file_hash("a", "README.md", "ha").unwrap();
    store
        .delete_file_hashes(None, &["README.md".to_string()])
        .unwrap();
    assert!(store.get_file_hash("a", "README.md").unwrap().is_none());
    assert!(store.get_file_hash("b", "README.md").unwrap().is_none());
}

#[test]
fn test_delete_file_hashes_exceeds_bind_var_limit() {
    // Regression for bobbin #43: pruning more files than SQLITE_MAX_VARIABLE_NUMBER
    // (32766) in one pass must not abort. Insert > limit rows, delete them all.
    let (store, _dir) = create_test_store();

    let n = 40_000;
    let paths: Vec<String> = (0..n).map(|i| format!("src/file_{i}.rs")).collect();
    let entries: Vec<(&str, &str)> = paths.iter().map(|p| (p.as_str(), "h")).collect();
    store.set_file_hashes_bulk("r", &entries).unwrap();

    // A single unbatched IN (?) would exceed the variable limit and fail here.
    store.delete_file_hashes(Some("r"), &paths).unwrap();

    assert!(store.get_file_hash("r", "src/file_0.rs").unwrap().is_none());
    assert!(store
        .get_file_hash("r", &format!("src/file_{}.rs", n - 1))
        .unwrap()
        .is_none());
    assert!(store.get_all_indexed_files("r").unwrap().is_empty());
}

#[test]
fn test_clear_file_hashes_is_scoped_to_one_repo() {
    let (store, _dir) = create_test_store();

    store.set_file_hash("a", "src/a.rs", "hash_a").unwrap();
    store.set_file_hash("b", "src/b.rs", "hash_b").unwrap();

    store.clear_file_hashes("a").unwrap();

    assert!(store.get_file_hash("a", "src/a.rs").unwrap().is_none());
    assert_eq!(
        store.get_file_hash("b", "src/b.rs").unwrap(),
        Some("hash_b".to_string()),
        "--force on repo a must not wipe repo b's incremental state"
    );
}

#[test]
fn test_migration_drops_path_keyed_hashes_and_global_watermarks() {
    // A pre-fix database: path-keyed file_hashes (collided, unattributable) and
    // the global watermark keys. Opening it must drop both — the rows cannot be
    // re-keyed after the fact, and the global SHA belongs to at most one repo.
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE file_hashes (file_path TEXT PRIMARY KEY, hash TEXT NOT NULL);
            INSERT INTO file_hashes VALUES ('README.md', 'quipus_hash_owning_everyones_row');
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
            INSERT INTO meta VALUES ('last_indexed_commit', 'deadbeef');
            INSERT INTO meta VALUES ('last_coupling_commit', 'deadbeef');
            INSERT INTO meta VALUES ('coupling_depth', '5000');
            INSERT INTO meta VALUES ('embedding_model', 'kept');
            INSERT INTO meta VALUES ('repo_source:quipu', '/repos/quipu');
            "#,
        )
        .unwrap();
    }

    let store = MetadataStore::open(&db_path).unwrap();

    // The collided row is gone for every repo — absent, never the wrong hash.
    assert!(store.get_file_hash("quipu", "README.md").unwrap().is_none());
    assert!(store
        .get_file_hash("goldblum", "README.md")
        .unwrap()
        .is_none());
    // New-schema writes work.
    store.set_file_hash("quipu", "README.md", "h").unwrap();
    assert_eq!(
        store.get_file_hash("quipu", "README.md").unwrap(),
        Some("h".to_string())
    );

    // Global watermarks removed; unrelated meta keys survive.
    assert!(store.get_meta("last_indexed_commit").unwrap().is_none());
    assert!(store.get_meta("last_coupling_commit").unwrap().is_none());
    assert!(store.get_meta("coupling_depth").unwrap().is_none());
    assert_eq!(
        store.get_meta("embedding_model").unwrap(),
        Some("kept".to_string())
    );
    assert_eq!(
        store.get_meta("repo_source:quipu").unwrap(),
        Some("/repos/quipu".to_string())
    );

    // Reopening an already-migrated DB is a no-op (idempotent).
    drop(store);
    let store = MetadataStore::open(&db_path).unwrap();
    assert_eq!(
        store.get_file_hash("quipu", "README.md").unwrap(),
        Some("h".to_string()),
        "re-migration must not re-drop the new-schema table"
    );
}

#[test]
fn test_bead_lineage_record_and_list() {
    let (store, _dir) = create_test_store();

    store
        .record_bead_lineage(&NewBeadLineage {
            bead_id: "bo-abc".to_string(),
            bead_type: Some("bug".to_string()),
            commit_sha: Some("deadbeef".to_string()),
            bundle_slugs: Some("search-reranking".to_string()),
            touched_files: vec!["src/search/weights.rs".to_string(), "src/a.rs".to_string()],
            action_type: Some("linked".to_string()),
            feature_id: Some("bo-feat".to_string()),
            lines_added: Some(42),
            lines_deleted: Some(7),
            touched_symbols: vec![TouchedSymbol {
                file: "src/search/weights.rs".to_string(),
                symbol: "rerank".to_string(),
                kind: "function".to_string(),
            }],
        })
        .unwrap();

    let all = store.list_bead_lineage(None, None, 10).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].bead_id, "bo-abc");
    assert_eq!(all[0].commit_sha.as_deref(), Some("deadbeef"));
    assert_eq!(all[0].touched_files.len(), 2);
    assert!(all[0].touched_files.contains(&"src/a.rs".to_string()));
    // New telemetry Phase 0 fields round-trip through record -> list.
    assert_eq!(all[0].feature_id.as_deref(), Some("bo-feat"));
    assert_eq!(all[0].lines_added, Some(42));
    assert_eq!(all[0].lines_deleted, Some(7));
    assert_eq!(all[0].touched_symbols.len(), 1);
    assert_eq!(all[0].touched_symbols[0].symbol, "rerank");
    assert_eq!(all[0].touched_symbols[0].kind, "function");

    // Filter by bead id
    let by_bead = store.list_bead_lineage(Some("bo-abc"), None, 10).unwrap();
    assert_eq!(by_bead.len(), 1);
    assert!(store
        .list_bead_lineage(Some("bo-zzz"), None, 10)
        .unwrap()
        .is_empty());

    // Filter by commit
    let by_commit = store.list_bead_lineage(None, Some("deadbeef"), 10).unwrap();
    assert_eq!(by_commit.len(), 1);
}

#[test]
fn test_prior_lineage_touching_files_and_bug_causality() {
    let (store, _dir) = create_test_store();

    // A prior commit (bo-old) and a later bug-fix commit (bo-bug) both touch
    // src/a.rs. Record them, then query priors touching the bug's files.
    store
        .record_bead_lineage(&NewBeadLineage {
            bead_id: "bo-old".to_string(),
            commit_sha: Some("sha_old".to_string()),
            touched_files: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            action_type: Some("commit".to_string()),
            ..Default::default()
        })
        .unwrap();
    store
        .record_bead_lineage(&NewBeadLineage {
            bead_id: "bo-bug".to_string(),
            bead_type: Some("bug".to_string()),
            commit_sha: Some("sha_fix".to_string()),
            touched_files: vec!["src/a.rs".to_string()],
            action_type: Some("commit".to_string()),
            ..Default::default()
        })
        .unwrap();

    // json_each-driven lookup: a far-future boundary admits all prior rows.
    let priors = store
        .prior_lineage_touching_files(&["src/a.rs".to_string()], "9999-01-01T00:00:00Z")
        .unwrap();
    assert!(priors.iter().all(|p| p.file == "src/a.rs"));
    assert!(priors.iter().any(|p| p.bead_id == "bo-old"));

    // Empty file list short-circuits.
    assert!(store
        .prior_lineage_touching_files(&[], "9999-01-01T00:00:00Z")
        .unwrap()
        .is_empty());

    // distinct bead ids surface the bug's recorded type.
    let distinct = store.distinct_lineage_bead_ids().unwrap();
    assert!(distinct
        .iter()
        .any(|(id, ty)| id == "bo-bug" && ty.as_deref() == Some("bug")));

    // Upsert idempotency: same (bug, sha, file) refreshes, not duplicates.
    store
        .record_bug_causality(&NewBugCausality {
            bug_id: "bo-bug".to_string(),
            culprit_sha: Some("sha_old".to_string()),
            culprit_bead_id: Some("bo-old".to_string()),
            file: Some("src/a.rs".to_string()),
            confidence: Some(0.5),
        })
        .unwrap();
    store
        .record_bug_causality(&NewBugCausality {
            bug_id: "bo-bug".to_string(),
            culprit_sha: Some("sha_old".to_string()),
            culprit_bead_id: Some("bo-old".to_string()),
            file: Some("src/a.rs".to_string()),
            confidence: Some(0.9),
        })
        .unwrap();
    let rows = store.list_bug_causality(Some("bo-bug"), 10).unwrap();
    assert_eq!(rows.len(), 1, "upsert must not duplicate");
    assert_eq!(rows[0].confidence, Some(0.9), "confidence refreshed");
    assert_eq!(rows[0].culprit_sha.as_deref(), Some("sha_old"));
}

#[test]
fn test_bead_lineage_migration_idempotent() {
    // Opening the same DB twice must not error or duplicate columns: the
    // second open re-runs migrate_bead_lineage against an already-migrated
    // table.
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("migrate.db");
    {
        let _store = MetadataStore::open(&db_path).unwrap();
    }
    let store = MetadataStore::open(&db_path).unwrap();

    // The new columns exist exactly once.
    let cols: Vec<String> = {
        let mut stmt = store
            .conn
            .prepare("PRAGMA table_info(bead_lineage)")
            .unwrap();
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    for expected in [
        "feature_id",
        "lines_added",
        "lines_deleted",
        "touched_symbols",
    ] {
        assert_eq!(
            cols.iter().filter(|c| c.as_str() == expected).count(),
            1,
            "column {expected} should exist exactly once"
        );
    }
}

#[test]
fn test_bead_lineage_migrates_legacy_table() {
    // A DB created with only the original columns (pre-bo-xrsy) must gain the
    // new columns on next open, and existing rows survive with NULL telemetry.
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"CREATE TABLE bead_lineage (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                   bead_id TEXT NOT NULL,
                   bead_type TEXT,
                   commit_sha TEXT,
                   bundle_slugs TEXT,
                   touched_files TEXT,
                   action_type TEXT
               );
               INSERT INTO bead_lineage (bead_id, commit_sha) VALUES ('bo-old', 'cafe');"#,
        )
        .unwrap();
    }
    let store = MetadataStore::open(&db_path).unwrap();
    let rows = store.list_bead_lineage(Some("bo-old"), None, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].feature_id, None);
    assert_eq!(rows[0].lines_added, None);
    assert!(rows[0].touched_symbols.is_empty());
}

#[test]
fn test_bead_lineage_ordering_and_limit() {
    let (store, _dir) = create_test_store();
    for i in 0..5 {
        store
            .record_bead_lineage(&NewBeadLineage {
                bead_id: format!("bo-{i}"),
                commit_sha: Some(format!("sha{i}")),
                ..Default::default()
            })
            .unwrap();
    }
    let recent = store.list_bead_lineage(None, None, 3).unwrap();
    assert_eq!(recent.len(), 3);
    // Most recent (highest id) first
    assert_eq!(recent[0].bead_id, "bo-4");
}

#[test]
fn test_get_all_indexed_files() {
    let (store, _dir) = create_test_store();

    store.set_file_hash("r", "src/a.rs", "hash_a").unwrap();
    store.set_file_hash("r", "src/b.rs", "hash_b").unwrap();
    store.set_file_hash("other", "src/c.rs", "hash_c").unwrap();

    let files = store.get_all_indexed_files("r").unwrap();
    assert_eq!(files.len(), 2, "listing is scoped to the requested repo");
    assert!(files.contains("src/a.rs"));
    assert!(files.contains("src/b.rs"));
}

// a purge must reset the per-repo commit watermark so the next
// incremental index rebuilds full history instead of only post-watermark
// commits. delete_meta is the mechanism; here we prove it removes exactly the
// targeted key, returns the row count, and is a safe no-op when absent.
#[test]
fn test_delete_meta_resets_watermark_exactly() {
    let (store, _dir) = create_test_store();
    store.set_meta("last_indexed_commit:repoA", "aaaa").unwrap();
    store.set_meta("last_indexed_commit:repoB", "bbbb").unwrap();

    // Delete repoA's watermark only.
    let n = store.delete_meta("last_indexed_commit:repoA").unwrap();
    assert_eq!(n, 1, "delete_meta must report the row it removed");
    assert!(
        store
            .get_meta("last_indexed_commit:repoA")
            .unwrap()
            .is_none(),
        "repoA watermark must be gone -> next index rebuilds from scratch",
    );
    assert_eq!(
        store
            .get_meta("last_indexed_commit:repoB")
            .unwrap()
            .as_deref(),
        Some("bbbb"),
        "a scoped watermark reset must spare other repos",
    );

    // Deleting an absent key is a harmless no-op (best-effort purge cleanup).
    assert_eq!(store.delete_meta("last_indexed_commit:repoA").unwrap(), 0);
}
