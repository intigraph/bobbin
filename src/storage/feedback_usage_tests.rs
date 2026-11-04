use super::*;
use crate::memory_recall::CitedItem;
use tempfile::NamedTempFile;

#[test]
fn idle_requires_ninety_days_of_endpoint_coverage_and_no_recent_use() {
    let file = NamedTempFile::new().unwrap();
    let store = FeedbackStore::open(file.path()).unwrap();
    let endpoint = "https://graph.example";
    store
        .observe_memory_entities(endpoint, ["urn:never-injected", "urn:used", "urn:invalid"])
        .unwrap();
    assert!(store
        .idle_memory_entities(endpoint, 100)
        .unwrap()
        .is_empty());
    store
        .conn
        .execute(
            "UPDATE memory_observations SET observed_since = '2020-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE memory_observations SET observed_since = 'invalid' WHERE iri = 'urn:invalid'",
            [],
        )
        .unwrap();
    store
        .store_injection_items("inj", &[cited("m1", "urn:used")])
        .unwrap();
    store.store_feedback(&used("inj", "m1")).unwrap();
    let candidates = store.idle_memory_entities(endpoint, 100).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].iri, "urn:never-injected");
    assert!(store
        .idle_memory_entities("https://other.example", 100)
        .unwrap()
        .is_empty());
    assert!(store.idle_memory_entities("", 100).unwrap().is_empty());
    assert!(store.idle_memory_entities(endpoint, 0).unwrap().is_empty());
}

fn cited(item: &str, iri: &str) -> CitedItem {
    CitedItem {
        item: item.into(),
        iri: iri.into(),
        kind: "Decision".into(),
        natural_key: iri.into(),
        line: "evidence".into(),
    }
}

fn used(injection: &str, item: &str) -> FeedbackInput {
    FeedbackInput {
        injection_id: injection.into(),
        agent: "test".into(),
        rating: "used".into(),
        reason: "acted on evidence".into(),
        item: Some(item.into()),
    }
}

#[test]
fn citation_replay_is_exact_and_a_conflicting_batch_rolls_back() {
    let file = NamedTempFile::new().unwrap();
    let store = FeedbackStore::open(file.path()).unwrap();
    store
        .store_injection_items("inj", &[cited("m1", "urn:a")])
        .unwrap();
    store
        .store_injection_items("inj", &[cited("m1", "urn:a")])
        .unwrap();
    assert!(store
        .store_injection_items(
            "inj",
            &[cited("m2", "urn:b"), cited("m1", "urn:retargeted"),]
        )
        .is_err());
    let items = store.injection_items("inj").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].iri, "urn:a");
    assert!(store
        .store_injection_items("invalid", &[cited("m1", "urn:a"), cited("bad", "urn:b"),])
        .is_err());
    assert!(store.injection_items("invalid").unwrap().is_empty());
}

#[test]
fn replay_preserves_use_receipt_and_time_but_new_injection_counts() {
    let file = NamedTempFile::new().unwrap();
    let store = FeedbackStore::open(file.path()).unwrap();
    store
        .store_injection_items("inj", &[cited("m1", "urn:a")])
        .unwrap();
    let id = store.store_feedback(&used("inj", "m1")).unwrap();
    store
        .conn
        .execute(
            "UPDATE feedback SET timestamp = '2020-01-01T00:00:00Z' WHERE id = ?1",
            [id],
        )
        .unwrap();
    assert_eq!(store.store_feedback(&used("inj", "m1")).unwrap(), id);
    let usage = store.entity_usage(10).unwrap();
    assert_eq!(usage[0].used, 1);
    assert_eq!(usage[0].last_used.as_deref(), Some("2020-01-01T00:00:00Z"));
    store
        .store_injection_items("new", &[cited("m1", "urn:a")])
        .unwrap();
    store.store_feedback(&used("new", "m1")).unwrap();
    assert_eq!(store.entity_usage(10).unwrap()[0].used, 2);
}

#[test]
fn unknown_citations_are_never_retroactively_assigned() {
    let file = NamedTempFile::new().unwrap();
    let store = FeedbackStore::open(file.path()).unwrap();
    let id = store.store_feedback(&used("inj", "m1")).unwrap();
    store
        .store_injection_items("inj", &[cited("m1", "urn:a")])
        .unwrap();
    assert_eq!(store.store_feedback(&used("inj", "m1")).unwrap(), id);
    assert_eq!(store.entity_usage(10).unwrap()[0].used, 0);
    assert!(store.list_feedback(&FeedbackQuery::default()).unwrap()[0]
        .iri
        .is_none());
}

#[test]
fn legacy_duplicate_receipts_count_once_but_keep_latest_recorded_use() {
    let file = NamedTempFile::new().unwrap();
    let store = FeedbackStore::open(file.path()).unwrap();
    store
        .store_injection_items("inj", &[cited("m1", "urn:a")])
        .unwrap();
    for date in ["2020-01-01T00:00:00Z", "2026-01-01T00:00:00Z"] {
        store.conn.execute("INSERT INTO feedback (injection_id, timestamp, agent, rating, reason, item, iri) VALUES ('inj', ?1, 'test', 'used', '', 'm1', 'urn:a')", [date]).unwrap();
    }
    let usage = store.entity_usage(10).unwrap();
    assert_eq!(usage[0].used, 1);
    assert_eq!(usage[0].last_used.as_deref(), Some("2026-01-01T00:00:00Z"));
    assert_eq!(
        store
            .list_feedback(&FeedbackQuery::default())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn legacy_retargeted_mapping_does_not_move_a_captured_use() {
    let file = NamedTempFile::new().unwrap();
    let store = FeedbackStore::open(file.path()).unwrap();
    store
        .store_injection_items("inj", &[cited("m1", "urn:original")])
        .unwrap();
    store.store_feedback(&used("inj", "m1")).unwrap();
    // Simulate an old database written before citation immutability.
    store
        .conn
        .execute("UPDATE injection_items SET iri = 'urn:replacement'", [])
        .unwrap();
    let usage = store.entity_usage(10).unwrap();
    let original = usage.iter().find(|e| e.iri == "urn:original").unwrap();
    assert_eq!((original.injected, original.used), (0, 1));
    let replacement = usage.iter().find(|e| e.iri == "urn:replacement").unwrap();
    assert_eq!((replacement.injected, replacement.used), (1, 0));
}

#[test]
fn old_citations_start_observation_now_and_reopen_does_not_reset_it() {
    let file = NamedTempFile::new().unwrap();
    let store = FeedbackStore::open(file.path()).unwrap();
    store
        .store_injection_items("old", &[cited("m1", "urn:a")])
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE injections SET timestamp = '2000-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    store
        .conn
        .execute_batch("DELETE FROM memory_observations; DROP TABLE memory_usage_migrations;")
        .unwrap();
    drop(store);
    let before = chrono::Utc::now();
    let store = FeedbackStore::open(file.path()).unwrap();
    let seen: String = store
        .conn
        .query_row(
            "SELECT observed_since FROM memory_observations WHERE iri = 'urn:a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(&seen).unwrap() >= before);
    drop(store);
    let store = FeedbackStore::open(file.path()).unwrap();
    let reopened: String = store
        .conn
        .query_row(
            "SELECT observed_since FROM memory_observations WHERE iri = 'urn:a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reopened, seen);
}
