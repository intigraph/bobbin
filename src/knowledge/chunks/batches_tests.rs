use super::*;

fn scope() -> Scope {
    Scope {
        repo: "fixture".into(),
        graph: None,
        generation: "generation-a".into(),
        expected_legacy_head: "legacy-a".into(),
    }
}
fn limits() -> Limits {
    Limits {
        bytes: 64 * 1024,
        triples: 1000,
        input_bytes: 1024 * 1024,
    }
}
fn chunk() -> Chunk {
    Chunk {
        id: "c".into(),
        file_path: "README.md".into(),
        chunk_type: ChunkType::Section,
        name: Some("Setup".into()),
        start_line: 1,
        end_line: 4,
        content: "## Setup\nPRIVATE BODY MUST NOT TRAVEL".into(),
        language: "markdown".into(),
        tags: String::new(),
    }
}
fn batch() -> Batch {
    project(
        &scope(),
        "README.md",
        0,
        vec![chunk()],
        vec![],
        limits(),
        false,
    )
    .unwrap()
}
fn receipt(b: &Batch) -> Receipt {
    Receipt {
        contract: TRANSFER_CONTRACT.into(),
        scope: b.scope.clone(),
        document: b.document.clone(),
        sequence: b.sequence,
        snapshot: b.snapshot.clone(),
        content_hash: b.content_hash.clone(),
        delete: b.delete,
        tx_id: 0,
        ownership_moved: true,
        writer_released: true,
    }
}

#[test]
fn keeps_observed_depth_and_document_identity_without_text() {
    let b = batch();
    assert!(b.turtle.contains("README.md#setup>"));
    assert!(b.turtle.contains("headingDepth \"2\""));
    assert!(!b.turtle.contains("PRIVATE BODY"));
    assert_eq!(b.triples, 11);
    assert_eq!(
        b,
        project(
            &scope(),
            "README.md",
            0,
            vec![chunk()],
            vec![],
            limits(),
            false
        )
        .unwrap()
    );
}

#[test]
fn predicate_like_literal_text_cannot_change_the_triple_count() {
    let mut c = chunk();
    c.name = Some("tricky > a bobbin:X, bobbin:Y".into());
    let b = project(&scope(), "README.md", 0, vec![c], vec![], limits(), false).unwrap();
    assert_eq!(b.triples, 11);
}

#[test]
fn refuses_bytes_triples_input_rows_and_foreign_membership() {
    let mut c = chunk();
    c.name = Some("界".repeat(24000));
    assert!(
        project(&scope(), "README.md", 0, vec![c], vec![], limits(), false)
            .unwrap_err()
            .contains("byte/triple")
    );
    let mut l = limits();
    l.triples = 10;
    assert!(project(&scope(), "README.md", 0, vec![chunk()], vec![], l, false).is_err());
    l = limits();
    l.input_bytes = 1;
    assert!(
        project(&scope(), "README.md", 0, vec![chunk()], vec![], l, false)
            .unwrap_err()
            .contains("metadata")
    );
    let mut c = chunk();
    c.file_path = "other.md".into();
    assert!(project(&scope(), "README.md", 0, vec![c], vec![], limits(), false).is_err());
    let mut c = chunk();
    c.start_line = 0;
    assert!(project(&scope(), "README.md", 0, vec![c], vec![], limits(), false).is_err());
    let e = ChunkEdge {
        source_chunk: "c".into(),
        target_chunk: "foreign".into(),
        source_name: String::new(),
        target_name: String::new(),
        edge_type: ChunkEdgeType::NextChunk,
        file_path: "README.md".into(),
    };
    assert!(project(
        &scope(),
        "README.md",
        0,
        vec![chunk()],
        vec![e],
        limits(),
        false
    )
    .is_err());
}

#[test]
fn key_is_stable_across_generations_but_scoped_to_graph_and_document() {
    let a = batch();
    let mut s = scope();
    s.generation = "generation-b".into();
    let b = project(&s, "README.md", 0, vec![chunk()], vec![], limits(), false).unwrap();
    assert_eq!(a.snapshot, b.snapshot);
    s.graph = Some("urn:other".into());
    let c = project(&s, "README.md", 0, vec![chunk()], vec![], limits(), false).unwrap();
    assert_ne!(a.snapshot, c.snapshot);
    for path in [
        "../README.md",
        "/README.md",
        "a/../README.md",
        "a//README.md",
        "a\n.md",
    ] {
        assert!(project(&scope(), path, 0, vec![], vec![], limits(), true).is_err());
    }
}

#[test]
fn lost_or_wrong_receipt_stop_and_stale_generation_cannot_advance() {
    let b = batch();
    let initial = Checkpoint::default();
    let mut state = initial.clone();
    assert!(state.admit(&b, true).is_err());
    for variant in 0..8 {
        let mut r = receipt(&b);
        match variant {
            0 => r.contract = "legacy-promote".into(),
            1 => r.writer_released = false,
            2 => r.ownership_moved = false,
            3 => r.content_hash = "wrong".into(),
            4 => r.scope.graph = Some("urn:foreign".into()),
            5 => r.scope.expected_legacy_head = "stale".into(),
            6 => r.sequence += 1,
            _ => r.delete = true,
        }
        assert!(state.accept(&b, &r).is_err());
        assert_eq!(state, initial);
    }
    state.accept(&b, &receipt(&b)).unwrap();
    assert_eq!(state.next_sequence, 1);
    let committed = state.clone();
    assert!(state.accept(&b, &receipt(&b)).is_err());
    assert_eq!(state, committed);
    let mut next = b.clone();
    next.sequence = 1;
    next.scope.generation = "changed".into();
    assert!(state.admit(&next, false).is_err());
    assert_eq!(state, committed);
}

#[test]
fn deletion_is_explicit_and_bound_to_its_exact_receipt() {
    assert!(project(&scope(), "README.md", 0, vec![], vec![], limits(), false).is_err());
    assert!(project(
        &scope(),
        "README.md",
        0,
        vec![chunk()],
        vec![],
        limits(),
        true
    )
    .is_err());
    let b = project(&scope(), "README.md", 0, vec![], vec![], limits(), true).unwrap();
    assert!(b.turtle.is_empty());
    assert_eq!(b.triples, 0);
    Checkpoint::default().accept(&b, &receipt(&b)).unwrap();
}
