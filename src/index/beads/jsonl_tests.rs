//! Tests for the br JSONL bead backend.
//!
//! The load-bearing assertion here is not "it parses JSON". It is that this
//! backend and the Dolt backend agree: same chunk keys, same visibility, same
//! assembled content. aegis-205llh was two stores answering as one; a silent
//! disagreement between two backends is the same failure with a shorter fuse.

use super::*;
use crate::config::BeadsSource;
use std::io::Write;

fn write_jsonl(lines: &[&str]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().expect("temp file");
    for l in lines {
        writeln!(f, "{}", l).expect("write");
    }
    f.flush().expect("flush");
    f
}

fn config_for(path: &str) -> BeadsConfig {
    let mut c = BeadsConfig {
        enabled: true,
        source: BeadsSource::Jsonl,
        databases: vec!["beads_aegis".to_string()],
        include_closed: false,
        max_age_days: 0,
        ..Default::default()
    };
    c.jsonl_paths
        .insert("beads_aegis".to_string(), path.to_string());
    c
}

#[test]
fn quick_id_reads_the_head_and_declines_other_shapes() {
    assert_eq!(
        quick_id(r#"{"id":"aegis-205llh","title":"x"}"#),
        Some("aegis-205llh")
    );
    // Any other layout must decline rather than guess — callers full-parse on None.
    assert_eq!(quick_id(r#"{"title":"x","id":"aegis-1"}"#), None);
    assert_eq!(quick_id("not json"), None);
}

#[test]
fn chunk_key_matches_the_shared_constructor() {
    let f =
        write_jsonl(&[r#"{"id":"aegis-skg66e","title":"T","description":"D","status":"open"}"#]);
    let cfg = config_for(f.path().to_str().unwrap());
    let chunks = fetch_from_file(&cfg, f.path().to_str().unwrap(), "beads_aegis", None).unwrap();
    assert_eq!(chunks.len(), 1);
    // The identity the incremental machinery hashes against. If this ever
    // differs from the Dolt path's key, the two build separate corpora.
    assert_eq!(
        chunks[0].file_path,
        crate::index::beads::bead_file_path("aegis", "aegis-skg66e")
    );
}

#[test]
fn content_is_byte_identical_to_the_dolt_path_for_the_same_fields() {
    let f = write_jsonl(&[
        r#"{"id":"a-1","title":"T","description":"D","status":"open","priority":1,"assignee":"gennaro","notes":"N","labels":["beads","search"],"comments":[{"author":"wu","text":"C"}]}"#,
    ]);
    let cfg = config_for(f.path().to_str().unwrap());
    let chunks = fetch_from_file(&cfg, f.path().to_str().unwrap(), "beads_aegis", None).unwrap();

    // Assemble the same bead through the shared constructor the Dolt backend
    // uses, from rows as that backend would have built them.
    let issues = vec![BeadRow {
        id: "a-1".into(),
        title: "T".into(),
        description: "D".into(),
        status: "open".into(),
        priority: 1,
        assignee: Some("gennaro".into()),
        notes: "N".into(),
        metadata: String::new(),
    }];
    let comments = vec![CommentRow {
        issue_id: "a-1".into(),
        author: "wu".into(),
        text: "C".into(),
    }];
    let mut labels = HashMap::new();
    labels.insert(
        "a-1".to_string(),
        vec!["beads".to_string(), "search".to_string()],
    );
    let expected = rows_to_chunks("aegis", issues, &comments, &labels, &[]);

    assert_eq!(chunks[0].content, expected[0].content);
    assert_eq!(chunks[0].id, expected[0].id);
    assert_eq!(chunks[0].tags, expected[0].tags);
}

#[test]
fn closed_and_deleted_beads_obey_the_same_rules_as_the_sql_clause() {
    let f = write_jsonl(&[
        r#"{"id":"a-open","title":"o","status":"open"}"#,
        r#"{"id":"a-closed","title":"c","status":"closed"}"#,
        r#"{"id":"a-deleted","title":"d","status":"deleted"}"#,
    ]);
    let path = f.path().to_str().unwrap();
    let mut cfg = config_for(path);

    let got: Vec<String> = fetch_from_file(&cfg, path, "beads_aegis", None)
        .unwrap()
        .into_iter()
        .map(|c| c.file_path)
        .collect();
    assert_eq!(got, vec!["beads:aegis:a-open".to_string()]);

    // include_closed admits closed but NEVER deleted — same as the SQL clause.
    cfg.include_closed = true;
    let mut got: Vec<String> = fetch_from_file(&cfg, path, "beads_aegis", None)
        .unwrap()
        .into_iter()
        .map(|c| c.file_path)
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            "beads:aegis:a-closed".to_string(),
            "beads:aegis:a-open".to_string()
        ]
    );
}

#[test]
fn age_bounds_closed_beads_only() {
    let f = write_jsonl(&[
        r#"{"id":"a-old-open","title":"o","status":"open","created_at":"2020-01-01T00:00:00Z"}"#,
        r#"{"id":"a-old-closed","title":"c","status":"closed","created_at":"2020-01-01T00:00:00Z"}"#,
    ]);
    let path = f.path().to_str().unwrap();
    let mut cfg = config_for(path);
    cfg.include_closed = true;
    cfg.max_age_days = 90;

    let got: Vec<String> = fetch_from_file(&cfg, path, "beads_aegis", None)
        .unwrap()
        .into_iter()
        .map(|c| c.file_path)
        .collect();
    // The OPEN bead is ancient and still indexed; the closed one aged out.
    assert_eq!(got, vec!["beads:aegis:a-old-open".to_string()]);
}

#[test]
fn only_id_narrows_without_relaxing_visibility() {
    let f = write_jsonl(&[
        r#"{"id":"a-1","title":"x","status":"open"}"#,
        r#"{"id":"a-2","title":"y","status":"closed"}"#,
    ]);
    let path = f.path().to_str().unwrap();
    let cfg = config_for(path);

    assert_eq!(
        fetch_from_file(&cfg, path, "beads_aegis", Some("a-1"))
            .unwrap()
            .len(),
        1
    );
    // Asking for a filtered bead BY NAME must not re-admit it, or index-bead
    // would quietly restore exactly the rows the batch sweep drops.
    assert!(fetch_from_file(&cfg, path, "beads_aegis", Some("a-2"))
        .unwrap()
        .is_empty());
}

#[test]
fn excluded_labels_keep_the_bead_out_entirely() {
    let f = write_jsonl(&[
        r#"{"id":"a-sec","title":"s","status":"open","labels":["security"],"comments":[{"author":"x","text":"secret"}]}"#,
    ]);
    let path = f.path().to_str().unwrap();
    let mut cfg = config_for(path);
    cfg.exclude_labels = vec!["SECURITY".to_string()]; // case-insensitive

    assert!(fetch_from_file(&cfg, path, "beads_aegis", None)
        .unwrap()
        .is_empty());
}

#[test]
fn a_malformed_line_does_not_take_the_corpus_down() {
    let f = write_jsonl(&[
        r#"{"id":"a-1","title":"x","status":"open"}"#,
        r#"{"id":"a-broken","title":"#,
        r#"{"id":"a-2","title":"y","status":"open"}"#,
    ]);
    let path = f.path().to_str().unwrap();
    let cfg = config_for(path);
    assert_eq!(
        fetch_from_file(&cfg, path, "beads_aegis", None)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn a_repeated_id_takes_the_last_record() {
    let f = write_jsonl(&[
        r#"{"id":"a-1","title":"first","status":"open"}"#,
        r#"{"id":"a-1","title":"second","status":"open"}"#,
    ]);
    let path = f.path().to_str().unwrap();
    let cfg = config_for(path);
    let chunks = fetch_from_file(&cfg, path, "beads_aegis", None).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].name.as_deref(), Some("second"));
}

#[test]
fn live_metadata_reads_the_same_file() {
    let f = write_jsonl(&[
        r#"{"id":"a-1","title":"T","status":"in_progress","priority":1,"assignee":"gennaro","owner":"braino","issue_type":"bug","created_at":"2026-09-16T04:00:00Z","labels":["search"]}"#,
        r#"{"id":"a-2","title":"other","status":"open"}"#,
    ]);
    let cfg = config_for(f.path().to_str().unwrap());
    let got = fetch_bead_metadata(&cfg, &[("aegis".into(), "a-1".into())]).unwrap();

    assert_eq!(got.len(), 1, "only the requested bead comes back");
    let m = &got["a-1"];
    assert_eq!(m.status, "in_progress");
    assert_eq!(m.priority, 1);
    assert_eq!(m.assignee.as_deref(), Some("gennaro"));
    assert_eq!(m.owner, "braino");
    assert_eq!(m.issue_type, "bug");
    // The Dolt path formats created_at as %Y-%m-%d; this must match.
    assert_eq!(m.created_at.as_deref(), Some("2026-09-16"));
    assert_eq!(m.labels, vec!["search".to_string()]);
}

#[test]
fn a_missing_jsonl_path_is_an_error_not_an_empty_rig() {
    // "indexed zero beads" and "this rig is not configured" are
    // indistinguishable after the fact, and the first is how search goes blind.
    let cfg = BeadsConfig {
        enabled: true,
        source: BeadsSource::Jsonl,
        databases: vec!["beads_aegis".to_string()],
        ..Default::default()
    };
    let err = super::super::jsonl_path_for(&cfg, "beads_aegis")
        .expect_err("a database with no path must refuse");
    assert!(err.to_string().contains("jsonl_paths"), "{err}");
}

#[test]
fn an_unreadable_file_fails_loudly() {
    let cfg = config_for("/nonexistent/path/issues.jsonl");
    let err = fetch_from_file(&cfg, "/nonexistent/path/issues.jsonl", "beads_aegis", None)
        .expect_err("a missing export must not read as an empty corpus");
    assert!(err.to_string().contains("issues.jsonl"), "{err}");
}

#[test]
fn the_source_label_names_the_path_and_its_age() {
    let f = write_jsonl(&[r#"{"id":"a-1","title":"x","status":"open"}"#]);
    let path = f.path().to_str().unwrap().to_string();
    let cfg = config_for(&path);

    let label = crate::index::beads::source_label(&cfg);
    assert!(label.contains(&path), "{label}");
    // Freshly written: minutes, not days. The age is the point — a producer
    // that stopped is as quiet as a dead store.
    assert!(label.contains("0m old"), "{label}");

    // A path that is not there must SAY so rather than read as fresh.
    let mut missing = cfg.clone();
    missing
        .jsonl_paths
        .insert("beads_aegis".into(), "/nonexistent/issues.jsonl".into());
    assert!(
        crate::index::beads::source_label(&missing).contains("UNREADABLE"),
        "{}",
        crate::index::beads::source_label(&missing)
    );
}

/// aegis-8dda.4: retired-store `inbox:` pointers are messages, not work. An
/// index built from a store holding one pointer and one real item must hold
/// only the real item — and a title that merely mentions "inbox" stays.
#[test]
fn inbox_pointers_are_never_indexed_as_work() {
    let f = write_jsonl(&[
        r#"{"id":"aegis-ptr1","title":"inbox: msg from sattler","description":"pointer","status":"open"}"#,
        r#"{"id":"aegis-ptr2","title":"  INBOX: shouted pointer","description":"pointer","status":"open"}"#,
        r#"{"id":"aegis-work","title":"Fix search ranking","description":"real","status":"open"}"#,
        r#"{"id":"aegis-mid","title":"Clean up the inbox: drain stale mail","description":"real","status":"open"}"#,
    ]);
    let cfg = config_for(f.path().to_str().unwrap());
    let chunks = fetch_from_file(&cfg, f.path().to_str().unwrap(), "beads_aegis", None).unwrap();
    let mut ids: Vec<&str> = chunks
        .iter()
        .map(|c| c.file_path.rsplit(':').next().unwrap())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["aegis-mid", "aegis-work"]);

    // The single-bead path returns empty, which callers treat as "remove".
    let one = fetch_from_file(
        &cfg,
        f.path().to_str().unwrap(),
        "beads_aegis",
        Some("aegis-ptr1"),
    )
    .unwrap();
    assert!(one.is_empty());
}
