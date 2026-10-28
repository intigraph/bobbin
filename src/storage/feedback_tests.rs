//! Tests for the feedback store (moved out of feedback.rs for the file-size ratchet).

use super::*;
use tempfile::NamedTempFile;

fn temp_store() -> (FeedbackStore, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let store = FeedbackStore::open(f.path()).unwrap();
    (store, f)
}

#[test]
fn test_store_and_list_feedback() {
    let (store, _f) = temp_store();
    let input = FeedbackInput {
        injection_id: "inj-abc123".to_string(),
        agent: "aegis/crew/test".to_string(),
        rating: "useful".to_string(),
        reason: "helped find the right file".to_string(),
        item: None,
    };
    let id = store.store_feedback(&input).unwrap();
    assert!(id > 0);

    let records = store.list_feedback(&FeedbackQuery::default()).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].injection_id, "inj-abc123");
    assert_eq!(records[0].rating, "useful");
}

#[test]
fn test_invalid_rating() {
    let (store, _f) = temp_store();
    let input = FeedbackInput {
        injection_id: "inj-abc123".to_string(),
        agent: "test".to_string(),
        rating: "bad".to_string(),
        reason: String::new(),
        item: None,
    };
    assert!(store.store_feedback(&input).is_err());
}

#[test]
fn test_stats() {
    let (store, _f) = temp_store();
    for (inj, rating) in [
        ("inj-1", "useful"),
        ("inj-1", "noise"),
        ("inj-2", "harmful"),
    ] {
        store
            .store_feedback(&FeedbackInput {
                injection_id: inj.to_string(),
                agent: "test".to_string(),
                rating: rating.to_string(),
                reason: String::new(),
                item: None,
            })
            .unwrap();
    }
    let stats = store.stats().unwrap();
    assert_eq!(stats.total_injections, 2);
    assert_eq!(stats.total_feedback, 3);
    assert_eq!(stats.useful, 1);
    assert_eq!(stats.noise, 1);
    assert_eq!(stats.harmful, 1);
}

#[test]
fn test_filter_by_rating() {
    let (store, _f) = temp_store();
    for rating in ["useful", "noise", "useful"] {
        store
            .store_feedback(&FeedbackInput {
                injection_id: "inj-1".to_string(),
                agent: "test".to_string(),
                rating: rating.to_string(),
                reason: String::new(),
                item: None,
            })
            .unwrap();
    }
    let q = FeedbackQuery {
        rating: Some("useful".to_string()),
        ..Default::default()
    };
    let records = store.list_feedback(&q).unwrap();
    assert_eq!(records.len(), 2);
}

#[test]
fn test_react_injection_id() {
    let (store, _f) = temp_store();
    let input = FeedbackInput {
        injection_id: "inj-react-abc123".to_string(),
        agent: "test".to_string(),
        rating: "useful".to_string(),
        reason: "reaction context was helpful".to_string(),
        item: None,
    };
    store.store_feedback(&input).unwrap();
    let records = store
        .list_feedback(&FeedbackQuery {
            injection_id: Some("inj-react-abc123".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].injection_id, "inj-react-abc123");
}

#[test]
fn test_store_injection() {
    let (store, _f) = temp_store();
    let files = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
    store
        .store_injection(
            "inj-test1234",
            Some("session-abc"),
            Some("aegis/crew/test"),
            "how does auth work?",
            &files,
            5,
            300,
        )
        .unwrap();

    // Verify injection was stored
    let stats = store.stats().unwrap();
    assert_eq!(stats.total_injections, 1);

    // Feedback referencing this injection should work
    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-test1234".to_string(),
            agent: "test".to_string(),
            rating: "useful".to_string(),
            reason: "found the auth module".to_string(),
            item: None,
        })
        .unwrap();
    let stats = store.stats().unwrap();
    assert_eq!(stats.total_feedback, 1);
}

#[test]
fn test_get_injection_detail() {
    let (store, _f) = temp_store();
    let files = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
    store
        .store_injection(
            "inj-detail",
            Some("sess-1"),
            Some("aegis/crew/ian"),
            "how does auth work?",
            &files,
            5,
            300,
        )
        .unwrap();
    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-detail".to_string(),
            agent: "aegis/crew/ellie".to_string(),
            rating: "useful".to_string(),
            reason: "great context".to_string(),
            item: None,
        })
        .unwrap();

    let detail = store.get_injection("inj-detail").unwrap().unwrap();
    assert_eq!(detail.injection_id, "inj-detail");
    assert_eq!(detail.query.as_deref(), Some("how does auth work?"));
    assert_eq!(detail.files.len(), 2);
    assert_eq!(detail.total_chunks, 5);
    assert_eq!(detail.budget_lines, 300);
    assert_eq!(detail.feedback.len(), 1);
    assert_eq!(detail.feedback[0].rating, "useful");

    // Non-existent injection returns None
    assert!(store.get_injection("inj-nope").unwrap().is_none());
}

#[test]
fn test_lineage_store_and_list() {
    let (store, _f) = temp_store();
    // Create feedback records first
    let fid1 = store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-1".to_string(),
            agent: "test".to_string(),
            rating: "noise".to_string(),
            reason: "init function noise".to_string(),
            item: None,
        })
        .unwrap();
    let fid2 = store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-2".to_string(),
            agent: "test".to_string(),
            rating: "noise".to_string(),
            reason: "init function noise".to_string(),
            item: None,
        })
        .unwrap();

    // Record lineage action
    let lid = store
        .store_lineage(&LineageInput {
            feedback_ids: vec![fid1, fid2],
            action_type: "tag_effect".to_string(),
            bead: Some("bo-a94q".to_string()),
            commit_hash: Some("4ded620".to_string()),
            description: "Deployed auto:init exclude".to_string(),
            agent: Some("aegis/crew/ian".to_string()),
        })
        .unwrap();
    assert!(lid > 0);

    // List all lineage
    let records = store.list_lineage(&LineageQuery::default()).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action_type, "tag_effect");
    assert_eq!(records[0].bead.as_deref(), Some("bo-a94q"));
    assert_eq!(records[0].feedback_ids, vec![fid1, fid2]);

    // Query by bead
    let records = store
        .list_lineage(&LineageQuery {
            bead: Some("bo-a94q".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(records.len(), 1);

    // Query by feedback_id
    let records = store
        .list_lineage(&LineageQuery {
            feedback_id: Some(fid1),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(records.len(), 1);
}

#[test]
fn test_lineage_stats() {
    let (store, _f) = temp_store();
    let fid1 = store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-1".to_string(),
            agent: "test".to_string(),
            rating: "noise".to_string(),
            reason: "noise".to_string(),
            item: None,
        })
        .unwrap();
    let _fid2 = store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-2".to_string(),
            agent: "test".to_string(),
            rating: "noise".to_string(),
            reason: "noise".to_string(),
            item: None,
        })
        .unwrap();

    // Before lineage: 0 actioned, 2 unactioned
    let stats = store.stats().unwrap();
    assert_eq!(stats.actioned, 0);
    assert_eq!(stats.unactioned, 2);
    assert_eq!(stats.lineage_records, 0);

    // Record lineage for fid1 only
    store
        .store_lineage(&LineageInput {
            feedback_ids: vec![fid1],
            action_type: "code_fix".to_string(),
            bead: None,
            commit_hash: Some("abc123".to_string()),
            description: "Fixed it".to_string(),
            agent: None,
        })
        .unwrap();

    let stats = store.stats().unwrap();
    assert_eq!(stats.actioned, 1);
    assert_eq!(stats.unactioned, 1);
    assert_eq!(stats.lineage_records, 1);
}

#[test]
fn test_lineage_invalid_action_type() {
    let (store, _f) = temp_store();
    let fid = store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-1".to_string(),
            agent: "test".to_string(),
            rating: "noise".to_string(),
            reason: "test".to_string(),
            item: None,
        })
        .unwrap();
    let result = store.store_lineage(&LineageInput {
        feedback_ids: vec![fid],
        action_type: "invalid".to_string(),
        bead: None,
        commit_hash: None,
        description: "test".to_string(),
        agent: None,
    });
    assert!(result.is_err());
}

#[test]
fn test_lineage_rejects_nonexistent_feedback_ids() {
    let (store, _f) = temp_store();
    let fid = store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-1".to_string(),
            agent: "test".to_string(),
            rating: "noise".to_string(),
            reason: "test".to_string(),
            item: None,
        })
        .unwrap();

    // Mix of valid and invalid feedback IDs should be rejected
    let result = store.store_lineage(&LineageInput {
        feedback_ids: vec![fid, 9999],
        action_type: "code_fix".to_string(),
        bead: None,
        commit_hash: None,
        description: "test".to_string(),
        agent: None,
    });
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("9999"),
        "Error should mention the missing ID: {}",
        err_msg
    );

    // Verify no lineage record was created (transaction-like behavior)
    let records = store.list_lineage(&LineageQuery::default()).unwrap();
    assert_eq!(
        records.len(),
        0,
        "No lineage should be created when feedback IDs are invalid"
    );
}

#[test]
fn test_file_feedback_scores_basic() {
    let (store, _f) = temp_store();

    // Create an injection with files and query
    store
        .store_injection_with_output(
            "inj-score1",
            None,
            Some("agent-a"),
            "authentication login handler",
            &["src/auth.rs".to_string(), "src/session.rs".to_string()],
            2,
            100,
            None,
        )
        .unwrap();

    // Rate it useful
    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-score1".to_string(),
            agent: "agent-b".to_string(),
            rating: "useful".to_string(),
            reason: String::new(),
            item: None,
        })
        .unwrap();

    // Query with overlapping keywords
    let scores = store
        .file_feedback_scores("authentication middleware handler", 0.1)
        .unwrap();
    assert!(!scores.is_empty(), "Should have scores for similar query");
    assert!(
        scores.get("src/auth.rs").copied().unwrap_or(0.0) > 0.0,
        "auth.rs should be boosted"
    );
    assert!(
        scores.get("src/session.rs").copied().unwrap_or(0.0) > 0.0,
        "session.rs should be boosted"
    );
}

#[test]
fn test_file_feedback_scores_harmful_penalty() {
    let (store, _f) = temp_store();

    store
        .store_injection_with_output(
            "inj-harm1",
            None,
            Some("agent-a"),
            "database connection pooling",
            &["src/db.rs".to_string()],
            1,
            100,
            None,
        )
        .unwrap();

    // Rate it harmful
    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-harm1".to_string(),
            agent: "agent-c".to_string(),
            rating: "harmful".to_string(),
            reason: "wrong file".to_string(),
            item: None,
        })
        .unwrap();

    let scores = store
        .file_feedback_scores("database connection pooling", 0.1)
        .unwrap();
    assert!(
        scores.get("src/db.rs").copied().unwrap_or(0.0) < 0.0,
        "db.rs should have negative score from harmful rating"
    );
}

#[test]
fn test_file_feedback_scores_no_overlap() {
    let (store, _f) = temp_store();

    store
        .store_injection_with_output(
            "inj-nooverlap",
            None,
            Some("agent-a"),
            "authentication login handler",
            &["src/auth.rs".to_string()],
            1,
            100,
            None,
        )
        .unwrap();

    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-nooverlap".to_string(),
            agent: "agent-b".to_string(),
            rating: "useful".to_string(),
            reason: String::new(),
            item: None,
        })
        .unwrap();

    // Query with completely different keywords — should have no overlap
    let scores = store
        .file_feedback_scores("database migration schema", 0.1)
        .unwrap();
    assert!(
        scores.is_empty(),
        "Should have no scores for unrelated query"
    );
}

#[test]
fn test_file_feedback_scores_cross_agent() {
    let (store, _f) = temp_store();

    // Two different agents rate the same injection
    store
        .store_injection_with_output(
            "inj-cross1",
            None,
            Some("agent-a"),
            "error handling retry logic",
            &["src/retry.rs".to_string()],
            1,
            100,
            None,
        )
        .unwrap();

    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-cross1".to_string(),
            agent: "agent-b".to_string(),
            rating: "useful".to_string(),
            reason: String::new(),
            item: None,
        })
        .unwrap();

    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-cross1".to_string(),
            agent: "agent-c".to_string(),
            rating: "useful".to_string(),
            reason: String::new(),
            item: None,
        })
        .unwrap();

    let scores = store
        .file_feedback_scores("error handling retry mechanism", 0.1)
        .unwrap();
    let retry_score = scores.get("src/retry.rs").copied().unwrap_or(0.0);
    assert!(
        retry_score > 0.5,
        "retry.rs should have high score from 2 useful ratings, got {}",
        retry_score
    );
}

#[test]
fn test_stats_by_bundle() {
    let (store, _f) = temp_store();
    let files = vec!["src/main.rs".to_string()];

    // Create injections with bundle_name metadata
    store
        .store_injection_full(
            "inj-b1",
            None,
            None,
            "auth query",
            &files,
            3,
            100,
            None,
            Some("auth"),
            None,
        )
        .unwrap();
    store
        .store_injection_full(
            "inj-b2",
            None,
            None,
            "auth login",
            &files,
            2,
            100,
            None,
            Some("auth"),
            None,
        )
        .unwrap();
    store
        .store_injection_full(
            "inj-b3",
            None,
            None,
            "search query",
            &files,
            4,
            200,
            None,
            Some("search"),
            None,
        )
        .unwrap();
    store
        .store_injection_full(
            "inj-b4",
            None,
            None,
            "no bundle",
            &files,
            1,
            50,
            None,
            None,
            None,
        )
        .unwrap();

    // Add feedback
    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-b1".into(),
            agent: "test".into(),
            rating: "useful".into(),
            reason: String::new(),
            item: None,
        })
        .unwrap();
    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-b2".into(),
            agent: "test".into(),
            rating: "noise".into(),
            reason: String::new(),
            item: None,
        })
        .unwrap();
    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-b3".into(),
            agent: "test".into(),
            rating: "useful".into(),
            reason: String::new(),
            item: None,
        })
        .unwrap();

    let by_bundle = store.stats_by_bundle().unwrap();
    assert!(
        by_bundle.len() >= 2,
        "should have at least auth and search groups"
    );

    let auth = by_bundle.iter().find(|e| e.key == "auth").unwrap();
    assert_eq!(auth.injections, 2);
    assert_eq!(auth.feedback, 2);
    assert_eq!(auth.useful, 1);
    assert_eq!(auth.noise, 1);

    let search = by_bundle.iter().find(|e| e.key == "search").unwrap();
    assert_eq!(search.injections, 1);
    assert_eq!(search.useful, 1);
}

#[test]
fn test_stats_by_bead() {
    let (store, _f) = temp_store();
    let files = vec!["src/main.rs".to_string()];

    store
        .store_injection_full(
            "inj-d1",
            None,
            None,
            "q1",
            &files,
            1,
            100,
            None,
            None,
            Some("aegis-abc"),
        )
        .unwrap();
    store
        .store_injection_full(
            "inj-d2",
            None,
            None,
            "q2",
            &files,
            1,
            100,
            None,
            None,
            Some("aegis-abc"),
        )
        .unwrap();
    store
        .store_injection_full(
            "inj-d3",
            None,
            None,
            "q3",
            &files,
            1,
            100,
            None,
            None,
            Some("aegis-xyz"),
        )
        .unwrap();

    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-d1".into(),
            agent: "test".into(),
            rating: "useful".into(),
            reason: String::new(),
            item: None,
        })
        .unwrap();
    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-d3".into(),
            agent: "test".into(),
            rating: "harmful".into(),
            reason: String::new(),
            item: None,
        })
        .unwrap();

    let by_bead = store.stats_by_bead().unwrap();
    let abc = by_bead.iter().find(|e| e.key == "aegis-abc").unwrap();
    assert_eq!(abc.injections, 2);
    assert_eq!(abc.feedback, 1);
    assert_eq!(abc.useful, 1);

    let xyz = by_bead.iter().find(|e| e.key == "aegis-xyz").unwrap();
    assert_eq!(xyz.injections, 1);
    assert_eq!(xyz.harmful, 1);
}

#[test]
fn scoped_na_injection_persists_scope_for_feedback_join() {
    let (store, _f) = temp_store();
    store
        .store_scoped_injection_with_output(
            "inj-na",
            Some("session"),
            None,
            "turn grounding",
            &[],
            1,
            8,
            Some("grounded"),
            Some("na"),
            Some("gaia"),
            Some("sha256:abc"),
        )
        .unwrap();
    store
        .store_feedback(&FeedbackInput {
            injection_id: "inj-na".into(),
            agent: "agent".into(),
            rating: "useful".into(),
            reason: "matched the turn".into(),
            item: None,
        })
        .unwrap();
    let detail = store.get_injection("inj-na").unwrap().unwrap();
    assert_eq!(detail.scope.as_deref(), Some("na"));
    assert_eq!(detail.faction_id.as_deref(), Some("gaia"));
    assert_eq!(detail.feedback.len(), 1);
}

#[test]
fn unscoped_na_injection_is_rejected() {
    let (store, _f) = temp_store();
    let result = store.store_scoped_injection_with_output(
        "inj-na",
        None,
        None,
        "turn grounding",
        &[],
        1,
        8,
        None,
        Some("na"),
        None,
        Some("sha256:abc"),
    );
    assert!(result.is_err());
}

fn cited(item: &str, iri: &str) -> crate::memory_recall::CitedItem {
    crate::memory_recall::CitedItem {
        item: item.into(),
        iri: iri.into(),
        kind: "Decision".into(),
        natural_key: format!("{iri}-key"),
        line: format!("[{item}] Decision: x"),
    }
}

fn rate(store: &FeedbackStore, inj: &str, rating: &str, item: Option<&str>) -> Result<i64> {
    store.store_feedback(&FeedbackInput {
        injection_id: inj.into(),
        agent: "test".into(),
        rating: rating.into(),
        reason: String::new(),
        item: item.map(String::from),
    })
}

#[test]
fn item_feedback_resolves_to_the_cited_entity() {
    let (store, _f) = temp_store();
    store
        .store_injection_items(
            "inj-m",
            &[cited("m1", "urn:mem/a"), cited("m2", "urn:mem/b")],
        )
        .unwrap();
    assert_eq!(store.injection_items("inj-m").unwrap().len(), 2);

    rate(&store, "inj-m", "used", Some("m1")).unwrap();
    rate(&store, "inj-m", "noise", Some("m2")).unwrap();
    rate(&store, "inj-m", "useful", None).unwrap();
    // An item the injection never logged still records, with no IRI.
    rate(&store, "inj-m", "used", Some("m5")).unwrap();

    let records = store.list_feedback(&FeedbackQuery::default()).unwrap();
    let m1 = records
        .iter()
        .find(|r| r.item.as_deref() == Some("m1"))
        .unwrap();
    assert_eq!(m1.iri.as_deref(), Some("urn:mem/a"));
    assert_eq!(m1.rating, "used");
    let m5 = records
        .iter()
        .find(|r| r.item.as_deref() == Some("m5"))
        .unwrap();
    assert!(m5.iri.is_none());

    let stats = store.stats().unwrap();
    assert_eq!(stats.used, 2);
    assert_eq!(stats.item_feedback, 3);
    let a = stats
        .memory_entities
        .iter()
        .find(|e| e.iri == "urn:mem/a")
        .unwrap();
    assert_eq!((a.injected, a.used, a.noise), (1, 1, 0));
    assert!(a.last_used.is_some());
    let b = stats
        .memory_entities
        .iter()
        .find(|e| e.iri == "urn:mem/b")
        .unwrap();
    assert_eq!((b.injected, b.used, b.noise), (1, 0, 1));
}

#[test]
fn used_needs_an_item_and_items_are_validated() {
    let (store, _f) = temp_store();
    let err = rate(&store, "inj-x", "used", None).unwrap_err();
    assert!(err.to_string().contains("Invalid rating 'used'"));
    let err = rate(&store, "inj-x", "useful", Some("item1")).unwrap_err();
    assert!(err.to_string().contains("Invalid item"));
    assert!(is_valid_item("m1") && is_valid_item("m12"));
    assert!(
        !is_valid_item("m0")
            && !is_valid_item("m")
            && !is_valid_item("m123")
            && !is_valid_item("x1")
    );
}

#[test]
fn older_feedback_db_gains_item_columns() {
    let f = NamedTempFile::new().unwrap();
    {
        let conn = Connection::open(f.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE feedback (id INTEGER PRIMARY KEY AUTOINCREMENT, injection_id TEXT NOT NULL, timestamp TEXT NOT NULL, agent TEXT NOT NULL, rating TEXT NOT NULL, reason TEXT DEFAULT '');
             INSERT INTO feedback (injection_id, timestamp, agent, rating) VALUES ('inj-old', '2026-01-01T00:00:00Z', 'a', 'useful');",
        )
        .unwrap();
    }
    let store = FeedbackStore::open(f.path()).unwrap();
    let records = store.list_feedback(&FeedbackQuery::default()).unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].item.is_none());
}
