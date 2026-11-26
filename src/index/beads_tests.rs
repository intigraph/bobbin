//! Tests for `beads`.
//!
//! Split out of `beads.rs` so that file clears the 500-line error limit
//! (bobbin-aoz). `scripts/check-file-size.sh` exempts `*tests.rs` by
//! design, and the alternative — an allowlist entry — is the exit that
//! makes the ratchet meaningless.

use super::*;

#[test]
fn test_chunk_id_deterministic() {
    let input = "beads:aegis:aegis-0a9";
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let id1 = hex::encode(hasher.finalize());

    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let id2 = hex::encode(hasher.finalize());

    assert_eq!(id1, id2);
    assert_eq!(id1.len(), 64);
}

fn sample_issue() -> BeadRow {
    BeadRow {
        id: "aegis-0a9".to_string(),
        title: "Fix the widget".to_string(),
        description: "The widget is broken.".to_string(),
        status: "open".to_string(),
        priority: 1,
        assignee: Some("strider".to_string()),
        notes: "investigated already".to_string(),
        metadata: String::new(),
    }
}

#[test]
fn test_metadata_is_meaningful() {
    assert!(!metadata_is_meaningful(""));
    assert!(!metadata_is_meaningful("  {}  "));
    assert!(!metadata_is_meaningful("null"));
    assert!(metadata_is_meaningful(r#"{"source":"T1"}"#));
}

#[test]
fn test_build_bead_content_includes_metadata() {
    let mut issue = sample_issue();
    issue.metadata = r#"{"source":"guidance-T1","ref":"doc#42"}"#.to_string();
    let content = build_bead_content(&issue, &[], &[]);
    assert!(content.contains("Metadata:"));
    assert!(content.contains("guidance-T1"));
}

#[test]
fn test_build_bead_content_skips_empty_metadata() {
    let issue = sample_issue(); // metadata = ""
    let content = build_bead_content(&issue, &[], &[]);
    assert!(!content.contains("Metadata:"));
}

#[test]
fn test_bead_excluded_by_label() {
    let exclude = vec!["security".to_string(), "escalation".to_string()];
    assert!(bead_excluded(&["security".to_string()], &exclude));
    assert!(bead_excluded(&["Escalation".to_string()], &exclude)); // case-insensitive
    assert!(bead_excluded(
        &["pitch".to_string(), "security".to_string()],
        &exclude
    ));
    assert!(!bead_excluded(&["pitch".to_string()], &exclude));
    assert!(!bead_excluded(&["security".to_string()], &[])); // empty = no exclusion
}

#[test]
fn test_content_hash_stable_and_sensitive() {
    let a = content_hash("hello");
    assert_eq!(a, content_hash("hello"));
    assert_ne!(a, content_hash("hello!"));
    assert_eq!(a.len(), 64);
}

#[test]
fn test_build_bead_content_includes_labels() {
    let issue = sample_issue();
    let labels = vec!["pitch".to_string(), "b:search".to_string()];
    let content = build_bead_content(&issue, &[], &labels);
    assert!(content.contains("Fix the widget"));
    assert!(content.contains("The widget is broken."));
    assert!(content.contains("Notes:\ninvestigated already"));
    assert!(content.contains("Labels: pitch, b:search"));
    assert!(content.contains("Status: open | Priority: P1 | Assignee: strider"));
}

#[test]
fn test_build_bead_content_no_labels_no_comments() {
    let issue = sample_issue();
    let content = build_bead_content(&issue, &[], &[]);
    assert!(!content.contains("Labels:"));
    assert!(!content.contains("Comments:"));
    assert!(content.contains("Assignee: strider"));
}

#[test]
fn test_build_bead_content_includes_comments() {
    let issue = sample_issue();
    let c = CommentRow {
        issue_id: "aegis-0a9".to_string(),
        author: "ian".to_string(),
        text: "looks good".to_string(),
    };
    let content = build_bead_content(&issue, &[&c], &[]);
    assert!(content.contains("Comments:"));
    assert!(content.contains("--- ian ---\nlooks good"));
}

#[test]
fn test_disabled_config_returns_empty() {
    let config = BeadsConfig::default();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let chunks = rt.block_on(fetch_beads(&config)).unwrap();
    assert!(chunks.is_empty());
}

#[test]
fn test_empty_databases_returns_empty() {
    let config = BeadsConfig {
        enabled: true,
        databases: vec![],
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let chunks = rt.block_on(fetch_beads(&config)).unwrap();
    assert!(chunks.is_empty());
}

#[test]
fn live_metadata_enrichment_fails_loudly_when_source_is_unreachable() {
    let config = BeadsConfig {
        enabled: true,
        host: "127.0.0.1".to_string(),
        // Port 1 is deliberately closed. Enrichment is an explicitly live
        // contract; callers that want indexed-only behavior set enrich=false.
        port: 1,
        databases: vec!["beads_aegis".to_string()],
        ..Default::default()
    };
    let ids = vec![("aegis".to_string(), "aegis-probe".to_string())];
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt
        .block_on(fetch_bead_metadata(&config, &ids))
        .expect_err("unreachable live metadata source must not become an empty map");

    assert!(
        err.to_string()
            .contains("connect to beads_aegis for live bead enrichment"),
        "unexpected error: {err:#}"
    );
}

// ---------------------------------------------------------------------------
// Single-bead path (GH#52 Phase 4)
// ---------------------------------------------------------------------------

fn open_config() -> BeadsConfig {
    BeadsConfig {
        enabled: true,
        databases: vec!["beads_aegis".to_string()],
        max_age_days: 90,
        ..Default::default()
    }
}

/// THE INVARIANT. Narrowing to one bead must add a predicate and remove none.
///
/// If `index-bead` relaxed the visibility rules, asking for a bead by name
/// would index rows `--include-beads` deliberately filters out — closed beads,
/// aged-out beads, deleted beads — and the two paths would disagree about what
/// the corpus contains, with the batch sweep silently deleting whatever the
/// single-bead path admitted on its next run.
#[test]
fn single_bead_narrows_without_relaxing() {
    let config = open_config();
    let batch = issues_where_clause(&config, None);
    let single = issues_where_clause(&config, Some("aegis-0a9"));

    // Every batch condition survives verbatim.
    assert!(
        single.starts_with(batch.as_str()),
        "batch={batch}\nsingle={single}"
    );
    // And exactly one condition is added.
    assert_eq!(
        single.matches(" AND ").count(),
        batch.matches(" AND ").count() + 1
    );
    assert!(single.ends_with("id = 'aegis-0a9'"));
}

#[test]
fn single_bead_clause_honours_include_closed() {
    let mut config = open_config();
    config.include_closed = true;
    let single = issues_where_clause(&config, Some("x-1"));
    assert!(single.contains("status != 'deleted'"));
    // Deleted rows stay excluded even when closed ones are wanted.
    assert!(!single.contains("status NOT IN ('closed', 'deleted') AND"));
}

#[test]
fn single_bead_clause_escapes_quotes() {
    let config = open_config();
    let single = issues_where_clause(&config, Some("o'brien-1"));
    assert!(single.ends_with("id = 'o''brien-1'"), "{single}");
}

/// The chunk id is derived from the chunk key, so `index_hashed_item` can
/// address the row the batch path wrote without re-querying it.
#[test]
fn chunk_id_is_the_hash_of_the_chunk_key() {
    let key = bead_file_path("aegis", "aegis-0a9");
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    assert_eq!(hex::encode(hasher.finalize()).len(), 64);
    assert_eq!(key, "beads:aegis:aegis-0a9");
}

#[test]
fn fetch_bead_is_a_no_op_when_beads_are_unconfigured() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Disabled.
    let chunks = rt
        .block_on(fetch_bead(&BeadsConfig::default(), "bo-1", None))
        .unwrap();
    assert!(chunks.is_empty());
    // Enabled but with no databases: no connection is attempted, so this must
    // return empty rather than erroring on a refused MySQL connect.
    let config = BeadsConfig {
        enabled: true,
        databases: vec![],
        ..Default::default()
    };
    let chunks = rt.block_on(fetch_bead(&config, "bo-1", None)).unwrap();
    assert!(chunks.is_empty());
}

/// A rig filter that matches nothing must skip every database rather than
/// falling through to "all of them".
#[test]
fn fetch_bead_with_an_unmatched_rig_touches_nothing() {
    let config = BeadsConfig {
        enabled: true,
        // Deliberately unroutable: if the filter leaked, this would try to
        // connect and the test would fail with a connection error instead.
        host: "192.0.2.1".to_string(),
        databases: vec!["beads_aegis".to_string()],
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let chunks = rt
        .block_on(fetch_bead(&config, "bo-1", Some("gastown")))
        .unwrap();
    assert!(chunks.is_empty());
}

/// One table of cases through BOTH visibility rule sets.
///
/// The Dolt backend pushes visibility down as a `WHERE` clause; the JSONL
/// backend has nothing to push into and applies `bead_visible`. Two rule sets
/// over one rig is how two backends quietly build different corpora, so a
/// change to either side has to break this.
#[test]
fn visibility_rules_agree_with_the_sql_clause() {
    use super::keys::bead_visible;

    let recent = chrono::Utc::now().to_rfc3339();
    let ancient = "2020-01-01T00:00:00Z";

    // (include_closed, max_age_days) -> expectations per (status, created_at)
    let cases: &[(bool, u32, &str, &str, bool)] = &[
        // Deleted is never visible, under any configuration.
        (false, 0, "deleted", ancient, false),
        (true, 0, "deleted", ancient, false),
        (true, 90, "deleted", &recent, false),
        // Open is always visible, regardless of age.
        (false, 0, "open", ancient, true),
        (false, 90, "open", ancient, true),
        (true, 90, "in_progress", ancient, true),
        // Closed depends on include_closed...
        (false, 0, "closed", &recent, false),
        (true, 0, "closed", ancient, true),
        // ...and then on the age bound.
        (true, 90, "closed", &recent, true),
        (true, 90, "closed", ancient, false),
    ];

    for (include_closed, max_age_days, status, created_at, expected) in cases {
        let config = BeadsConfig {
            enabled: true,
            databases: vec!["beads_aegis".into()],
            include_closed: *include_closed,
            max_age_days: *max_age_days,
            ..Default::default()
        };

        assert_eq!(
            bead_visible(&config, status, Some(created_at)),
            *expected,
            "predicate: include_closed={include_closed} max_age_days={max_age_days} \
             status={status} created_at={created_at}"
        );

        // The clause side, structurally: the same three rules, in SQL.
        let clause = issues_where_clause(&config, None);
        assert!(
            clause.contains("deleted"),
            "every clause must exclude deleted rows: {clause}"
        );
        assert_eq!(
            clause.contains("status NOT IN ('closed', 'deleted')")
                && !clause.contains("status != 'deleted'"),
            !*include_closed,
            "closed-bead admission must track include_closed: {clause}"
        );
        assert_eq!(
            clause.contains("created_at >= DATE_SUB"),
            *max_age_days > 0,
            "the age bound must appear exactly when max_age_days > 0: {clause}"
        );
        if *max_age_days > 0 {
            // The age bound is OR'd with "not closed" — it bounds closed beads
            // only, which is the asymmetry `bead_visible` also encodes.
            assert!(
                clause.contains("(status NOT IN ('closed', 'deleted') OR created_at >="),
                "the age bound must apply to CLOSED beads only: {clause}"
            );
        }
    }
}

/// An unreadable `created_at` must not silently drop a bead.
#[test]
fn an_unparseable_created_at_is_treated_as_unbounded() {
    use super::keys::bead_visible;

    let config = BeadsConfig {
        enabled: true,
        include_closed: true,
        max_age_days: 1,
        ..Default::default()
    };
    // The age bound exists to trim old CLOSED work. A row whose age cannot be
    // read is a row we know nothing about, and dropping it would be a silent
    // deletion from the corpus — the failure this whole change exists to fix.
    assert!(bead_visible(&config, "closed", None));
    assert!(bead_visible(&config, "closed", Some("not-a-timestamp")));
}

#[test]
fn test_is_inbox_pointer_prefix_only() {
    assert!(is_inbox_pointer("inbox: hello"));
    assert!(is_inbox_pointer("  Inbox:hello"));
    assert!(is_inbox_pointer("INBOX:"));
    assert!(!is_inbox_pointer("Drain the inbox: stale mail"));
    assert!(!is_inbox_pointer("inbox cleanup"));
    assert!(!is_inbox_pointer("inbo"));
    assert!(!is_inbox_pointer(""));
    // Non-ASCII before byte 6 must not panic on a char boundary.
    assert!(!is_inbox_pointer("ínbox: x"));
}

fn indexed_result(id: &str, title: &str) -> SearchResult {
    SearchResult {
        chunk: Chunk {
            id: id.to_string(),
            file_path: bead_file_path("aegis", id),
            chunk_type: ChunkType::Issue,
            name: Some(title.to_string()),
            start_line: 0,
            end_line: 0,
            content: title.to_string(),
            language: "beads".to_string(),
            tags: String::new(),
        },
        score: 0.9,
        match_type: None,
        indexed_at: None,
        repo: None,
    }
}

/// aegis-8dda.4 query-time half: an index built BEFORE the index-time filter
/// still holds pointer chunks; search must drop them without a reindex.
#[test]
fn test_retain_work_items_filters_stale_index() {
    let mut results = vec![
        indexed_result("aegis-ptr", "inbox: msg from sattler"),
        indexed_result("aegis-work", "Fix search ranking"),
        indexed_result("aegis-mid", "Clean up the inbox: drain stale mail"),
        // Indexed under a normal title, but the live store says it is a pointer.
        indexed_result("aegis-renamed", "old title"),
    ];
    let mut live = HashMap::new();
    live.insert(
        "aegis-renamed".to_string(),
        LiveBeadMetadata {
            status: "open".into(),
            priority: 2,
            assignee: None,
            title: "Inbox: retired pointer".into(),
            issue_type: "task".into(),
            owner: String::new(),
            labels: vec![],
            created_at: None,
        },
    );
    retain_work_items(&mut results, &live);
    let ids: Vec<&str> = results.iter().map(|r| r.chunk.id.as_str()).collect();
    assert_eq!(ids, vec!["aegis-work", "aegis-mid"]);
}
