//! Tests for the chunk→entity reconcile mapping (W2.P5, bobbin-c79).
//!
//! All tests run on `quipu::Store::open_in_memory()` with no embedding
//! provider attached — the reconcile pass is pure fact-plumbing, so
//! nothing here needs ONNX or the embedding model.

use super::*;
use crate::iri::{chunk_iri, symbol_iri};
use crate::types::{Chunk, ChunkEdge, ChunkEdgeType, ChunkType};

fn chunk(id: &str, file: &str, t: ChunkType, name: Option<&str>, start: u32) -> Chunk {
    Chunk {
        id: id.to_string(),
        file_path: file.to_string(),
        chunk_type: t,
        name: name.map(str::to_string),
        start_line: start,
        end_line: start + 5,
        content: "body".to_string(),
        language: "rust".to_string(),
        tags: String::new(),
    }
}

fn edge(source: &str, target_name: &str, t: ChunkEdgeType) -> ChunkEdge {
    ChunkEdge {
        source_chunk: source.to_string(),
        target_chunk: format!("{target_name}-target"),
        source_name: source.to_string(),
        target_name: target_name.to_string(),
        edge_type: t,
        file_path: "src/lib.rs".to_string(),
    }
}

// ── Emitter ───────────────────────────────────────────────────

#[test]
fn named_code_chunk_mentions_its_own_symbol() {
    let c = chunk("c1", "src/lib.rs", ChunkType::Function, Some("parse"), 10);
    let names = chunk_mention_names(&c, &HashMap::new());
    assert_eq!(names, vec!["parse"]);
}

#[test]
fn sections_and_unnamed_chunks_emit_no_self_mention() {
    let section = chunk("c1", "docs/a.md", ChunkType::Section, Some("Guide"), 3);
    assert!(chunk_mention_names(&section, &HashMap::new()).is_empty());
    let unnamed = chunk("c2", "src/lib.rs", ChunkType::Function, None, 9);
    assert!(chunk_mention_names(&unnamed, &HashMap::new()).is_empty());
}

#[test]
fn edge_targets_become_mentions_deduped_and_sorted() {
    let c = chunk("impl1", "src/lib.rs", ChunkType::Impl, Some("Foo"), 20);
    let edges = vec![
        edge("impl1", "Display", ChunkEdgeType::Implements),
        edge("impl1", "Foo", ChunkEdgeType::ImplFor), // dup of self-mention
        edge("impl1", "Bar", ChunkEdgeType::Extends),
        edge("other", "Baz", ChunkEdgeType::Tests), // different source chunk
        edge("impl1", "Next", ChunkEdgeType::NextChunk), // structural, not a symbol
    ];
    let map = edge_mention_map(&edges);
    let names = chunk_mention_names(&c, &map);
    assert_eq!(names, vec!["Bar", "Display", "Foo"]);
}

#[test]
fn turtle_carries_mention_literals_for_fixture_chunk() {
    let chunks = vec![chunk(
        "c1",
        "src/lib.rs",
        ChunkType::Function,
        Some("parse"),
        10,
    )];
    let edges = vec![edge("c1", "Config", ChunkEdgeType::Tests)];
    let turtle = super::super::chunks::generate_chunk_turtle(&chunks, &edges, "myrepo");
    assert!(turtle.contains("bobbin:mentions \"Config\""));
    assert!(turtle.contains("bobbin:mentions \"parse\""));
}

// ── Reconcile pass ────────────────────────────────────────────

/// Seed a store with turtle via tool_knot (no embedder — plain facts).
fn seed(store: &mut quipu::Store, turtle: &str) {
    quipu::tool_knot(
        store,
        &serde_json::json!({
            "turtle": turtle,
            "timestamp": "2026-01-01T00:00:00Z",
            "actor": "test",
            "source": "test-seed",
        }),
    )
    .expect("seed write");
}

/// One chunk mentioning three names against an entity graph where
/// `Unique` has one entity, `Ghost` has none, and `Twin` has two
/// (neither in the chunk's module).
fn seeded_store() -> (quipu::Store, String) {
    let mut store = quipu::Store::open_in_memory().expect("in-memory store");
    let c_iri = chunk_iri("repo", "src/main.rs", 5);
    let unique = symbol_iri("repo", "src/util.rs", "Unique");
    let twin_a = symbol_iri("repo", "src/a.rs", "Twin");
    let twin_b = symbol_iri("repo", "src/b.rs", "Twin");
    let turtle = format!(
        "@prefix bobbin: <{ns}> .\n\
         <{c_iri}> a bobbin:Chunk ;\n\
             bobbin:mentions \"Unique\" ;\n\
             bobbin:mentions \"Ghost\" ;\n\
             bobbin:mentions \"Twin\" .\n\
         <{unique}> bobbin:name \"Unique\" .\n\
         <{twin_a}> bobbin:name \"Twin\" .\n\
         <{twin_b}> bobbin:name \"Twin\" .\n",
        ns = crate::iri::ONTOLOGY_NS,
    );
    seed(&mut store, &turtle);
    (store, unique)
}

#[test]
fn resolves_danglings_and_ambiguous_honestly() {
    let (mut store, unique_iri) = seeded_store();
    let report = reconcile_mentions(&mut store, "2026-01-02T00:00:00Z").expect("reconcile");

    assert_eq!(report.resolved, 1);
    assert_eq!(report.dangling, 1);
    assert_eq!(report.ambiguous, 1);
    assert_eq!(report.edges_written, 1);

    // The resolved Ref edge is in the store, pointing at the unique entity.
    let mentions_id = store.lookup(&mentions_iri()).unwrap().unwrap();
    let refs: Vec<String> = store
        .current_facts()
        .unwrap()
        .iter()
        .filter(|f| f.attribute == mentions_id)
        .filter_map(|f| match f.value {
            quipu::types::Value::Ref(t) => Some(store.resolve(t).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(refs, vec![unique_iri]);

    // Dangling and ambiguous literals were left alone (still 3 Str facts).
    let literals = store
        .current_facts()
        .unwrap()
        .iter()
        .filter(|f| f.attribute == mentions_id && matches!(f.value, quipu::types::Value::Str(_)))
        .count();
    assert_eq!(literals, 3);
}

#[test]
fn ambiguity_narrows_to_the_chunks_own_module() {
    let mut store = quipu::Store::open_in_memory().expect("in-memory store");
    let c_iri = chunk_iri("repo", "src/main.rs", 5);
    let local = symbol_iri("repo", "src/main.rs", "Twin");
    let remote = symbol_iri("repo", "src/other.rs", "Twin");
    let turtle = format!(
        "@prefix bobbin: <{ns}> .\n\
         <{c_iri}> a bobbin:Chunk ; bobbin:mentions \"Twin\" .\n\
         <{local}> bobbin:name \"Twin\" .\n\
         <{remote}> bobbin:name \"Twin\" .\n",
        ns = crate::iri::ONTOLOGY_NS,
    );
    seed(&mut store, &turtle);

    let report = reconcile_mentions(&mut store, "2026-01-02T00:00:00Z").expect("reconcile");
    assert_eq!(report.resolved, 1);
    assert_eq!(report.ambiguous, 0);
    match &report.details[0] {
        MentionResolution::Resolved { entity_iri, .. } => assert_eq!(entity_iri, &local),
        other => panic!("expected same-module resolve, got {other:?}"),
    }
}

#[test]
fn second_run_is_a_noop_with_identical_report() {
    let (mut store, _) = seeded_store();
    let first = reconcile_mentions(&mut store, "2026-01-02T00:00:00Z").expect("first run");
    let facts_after_first = store.current_facts().unwrap().len();

    let second = reconcile_mentions(&mut store, "2026-01-03T00:00:00Z").expect("second run");

    // Identical classification: same counts, same details.
    assert_eq!(second.resolved, first.resolved);
    assert_eq!(second.dangling, first.dangling);
    assert_eq!(second.ambiguous, first.ambiguous);
    assert_eq!(second.details, first.details);
    // And a true no-op: nothing written, store unchanged.
    assert_eq!(second.edges_written, 0);
    assert_eq!(store.current_facts().unwrap().len(), facts_after_first);
}

#[test]
fn dangling_resolves_once_the_entity_appears() {
    let (mut store, _) = seeded_store();
    reconcile_mentions(&mut store, "2026-01-02T00:00:00Z").expect("first run");

    // The entity graph learns about Ghost.
    let ghost = symbol_iri("repo", "src/ghost.rs", "Ghost");
    seed(
        &mut store,
        &format!(
            "@prefix bobbin: <{ns}> .\n<{ghost}> bobbin:name \"Ghost\" .\n",
            ns = crate::iri::ONTOLOGY_NS,
        ),
    );

    let report = reconcile_mentions(&mut store, "2026-01-04T00:00:00Z").expect("second run");
    assert_eq!(report.dangling, 0);
    assert_eq!(report.resolved, 2); // Unique (prior) + Ghost (new)
    assert_eq!(report.edges_written, 1); // only Ghost's edge is new
}

#[test]
fn empty_store_reports_all_zeroes() {
    let mut store = quipu::Store::open_in_memory().expect("in-memory store");
    let report = reconcile_mentions(&mut store, "2026-01-02T00:00:00Z").expect("reconcile");
    assert_eq!(
        (
            report.resolved,
            report.dangling,
            report.ambiguous,
            report.edges_written
        ),
        (0, 0, 0, 0)
    );
    assert!(report.details.is_empty());
}
