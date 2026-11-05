//! Push the chunk graph to Quipu as durable `bobbin:Chunk` facts (W2.P4).
//!
//! Chunk IRIs derive from STABLE COORDINATES — repo + path + `C{start_line}`
//! ordinal, matching the live ingest lane's `S{line}` section idiom — never
//! from bobbin's internal chunk ids, which are content-coordinate hashes
//! that re-mint on any line shift. The graph is the index, never the
//! warehouse: no chunk content crosses over, only identity, membership
//! (`bobbin:inDocument`), order (`bobbin:chunkOrder`), adjacency
//! (`bobbin:nextChunk`), and weak symbol-mention literals
//! (`bobbin:mentions "Name"`, W2.P5 — resolved to Ref edges by the
//! idempotent pass in `knowledge::mentions`). Containment (`part_of`)
//! stays in bobbin's own
//! edges table for now — the graph's `inDocument` covers the
//! chunk→document half the competency questions need.
//!
//! Writes go through `/knot` with `replace_snapshot` under a per-repo
//! producer key, so a reindex is a diffed replace: unchanged facts stay
//! live, vanished chunks retract. The embedded quipu revision is PROBED
//! for snapshot support before any fact is written — a store that predates
//! `replace_snapshot` would silently ignore the key and accumulate a copy
//! of the graph per index run, which is exactly the class of silent misses
//! this integration keeps finding (bobbin-jdlkh). Since the pin moved to
//! quipu 0.3.23 (rev 37bfc06a) the probe passes against the embedded store;
//! it stays because it guards correctness of whatever store is opened, not
//! a particular pin.

use std::path::Path;

use anyhow::{Context, Result};

use super::turtle::escape_literal;
use crate::iri::ONTOLOGY_NS;
use crate::types::{Chunk, ChunkEdge, ChunkEdgeType, ChunkType};

#[cfg(test)]
mod collision_tests;
mod graph_routing;
#[cfg(test)]
mod graph_tests;
mod remote;
use graph_routing::require_graph_routing;
pub use remote::push_chunks_to_remote_quipu;

/// Build the durable IRI for a chunk from stable coordinates.
pub(crate) use crate::iri::chunk_iri;

/// Generate Turtle for the chunk graph of one repo. Pure, unit-testable.
///
/// Only file-backed chunks participate (line 0 marks synthetic sources —
/// commits, beads, SQL rows — which have their own identity schemes).
pub(crate) fn generate_chunk_turtle(chunks: &[Chunk], edges: &[ChunkEdge], repo: &str) -> String {
    let mut turtle = String::with_capacity(chunks.len() * 256);
    turtle.push_str(&format!("@prefix bobbin: <{ONTOLOGY_NS}> .\n"));
    turtle.push_str("@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n");
    turtle.push_str("@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\n");

    // Map bobbin's volatile chunk ids to durable IRIs for edge emission.
    let mut iri_of: std::collections::HashMap<&str, String> = std::collections::HashMap::new();

    // Weak symbol-mention literals (W2.P5): source-chunk id → mentioned
    // symbol names, from the parser's symbol-bearing edges. The reconcile
    // pass (`knowledge::mentions`) later resolves these to Ref edges.
    let edge_mentions = super::mentions::edge_mention_map(edges);

    let mut file_chunks: Vec<&Chunk> = chunks.iter().filter(|c| c.start_line > 0).collect();
    file_chunks.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.start_line.cmp(&b.start_line))
            .then(b.end_line.cmp(&a.end_line))
            .then(a.id.cmp(&b.id))
    });

    // The remote ontology enforces the code-entities SHACL vocabulary. Assert
    // each file entity in the same snapshot as its chunks: CodeSymbol.definedIn
    // uses sh:class, and Quipu validates the submitted graph rather than looking
    // up the target in pre-existing store state.
    let mut seen_files = std::collections::HashSet::new();
    for chunk in &file_chunks {
        if !seen_files.insert(chunk.file_path.as_str()) {
            continue;
        }
        let module = file_entity_iri(repo, chunk);
        if matches!(chunk.language.as_str(), "markdown" | "pdf") {
            turtle.push_str(&format!("<{module}> a bobbin:Document ;\n"));
            turtle.push_str(&format!(
                "    rdfs:label \"{}\" ;\n    bobbin:filePath \"{}\" .\n\n",
                escape_literal(&chunk.file_path),
                escape_literal(&chunk.file_path)
            ));
        } else {
            turtle.push_str(&format!("<{module}> a bobbin:CodeModule ;\n"));
            turtle.push_str(&format!(
                "    rdfs:label \"{}\" ;\n    bobbin:filePath \"{}\" ;\n    bobbin:repo \"{}\" ;\n    bobbin:language \"{}\" .\n\n",
                escape_literal(&chunk.file_path),
                escape_literal(&chunk.file_path),
                escape_literal(repo),
                escape_literal(&chunk.language)
            ));
        }
    }

    let mut order_in_file = 0u32;
    let mut prev_file: Option<&str> = None;
    let mut claimed_iris: std::collections::HashSet<String> = std::collections::HashSet::new();
    for chunk in &file_chunks {
        if prev_file != Some(chunk.file_path.as_str()) {
            order_in_file = 0;
            prev_file = Some(chunk.file_path.as_str());
        }
        let module = file_entity_iri(repo, chunk);
        let label = chunk
            .name
            .clone()
            .unwrap_or_else(|| format!("{}:{}", chunk.file_path, chunk.start_line));

        let mut governed_type = match chunk.chunk_type {
            ChunkType::Section if chunk.name.is_some() => Some("Section"),
            t if t.is_code_symbol() && chunk.name.is_some() => Some("CodeSymbol"),
            _ => None,
        };

        // A dual-typed chunk takes the identity of the entity it IS, so it
        // merges with yupana's node for the same symbol/section rather than
        // forking beside it (aegis-6noan). That makes yupana's collision
        // problem ours: bobbin has no scope chain, so two same-named symbols
        // in one file mint one IRI, and a second block would assert two
        // values for the maxCount-1 `name`/`symbolKind`/`chunkOrder`
        // properties. yupana keeps the first and drops the rest
        // (`export.rs::dedupe_by_iri`); we keep the first and DEMOTE the rest
        // to the chunk lane, which loses no span — bobbin's product is the
        // span — and never asserts a conflicting value.
        //
        // The demotion is not silent, for yupana's reason: a quiet collapse
        // hides a genuine IRI collision exactly as well as it hides a benign
        // one, and the note travels with the payload that gets promoted AND
        // with the one that gets refused.
        let mut iri = match (governed_type, chunk.name.as_deref()) {
            (Some("CodeSymbol"), Some(name)) => {
                crate::iri::symbol_iri(repo, &chunk.file_path, name)
            }
            (Some("Section"), Some(heading)) => {
                crate::iri::section_iri(repo, &chunk.file_path, heading)
            }
            _ => chunk_iri(repo, &chunk.file_path, chunk.start_line),
        };
        if governed_type.is_some() && !claimed_iris.insert(iri.clone()) {
            turtle.push_str(&format!(
                "# bobbin: <{iri}> was already claimed by an earlier chunk in this file \
                 (bobbin has no scope chain, so same-named symbols in one file collide). \
                 Demoting {}:{} to the chunk lane so no maxCount-1 property is asserted \
                 twice. See aegis-6noan.\n",
                chunk.file_path, chunk.start_line
            ));
            governed_type = None;
            iri = chunk_iri(repo, &chunk.file_path, chunk.start_line);
        }

        // Anonymous callbacks and demoted symbols can share a start line.
        // Keep the original line IRI for the first span, then claim distinct
        // suffixes; each span must own exactly one chunkOrder and edge endpoint.
        if governed_type.is_none() {
            let base = iri.clone();
            let mut occurrence = 1;
            while !claimed_iris.insert(iri.clone()) {
                occurrence += 1;
                iri = format!("{base}-{occurrence}");
            }
        }

        match governed_type {
            Some(kind) => turtle.push_str(&format!("<{iri}> a bobbin:Chunk, bobbin:{kind} ;\n")),
            None => turtle.push_str(&format!("<{iri}> a bobbin:Chunk ;\n")),
        }
        turtle.push_str(&format!("    bobbin:inDocument <{module}> ;\n"));
        turtle.push_str(&format!(
            "    bobbin:chunkOrder \"{order_in_file}\"^^xsd:integer ;\n"
        ));
        turtle.push_str(&format!(
            "    bobbin:filePath \"{}\" ;\n",
            escape_literal(&chunk.file_path)
        ));
        for name in super::mentions::chunk_mention_names(chunk, &edge_mentions) {
            turtle.push_str(&format!(
                "    bobbin:mentions \"{}\" ;\n",
                escape_literal(name)
            ));
        }
        if let (Some("CodeSymbol"), Some(name)) = (governed_type, chunk.name.as_deref()) {
            turtle.push_str(&format!(
                "    bobbin:name \"{}\" ;\n    bobbin:definedIn <{module}> ;\n",
                escape_literal(name)
            ));
            if let Some(kind) = governed_symbol_kind(chunk.chunk_type) {
                turtle.push_str(&format!("    bobbin:symbolKind \"{kind}\" ;\n"));
            }
        }
        if let (Some("Section"), Some(heading)) = (governed_type, chunk.name.as_deref()) {
            let depth = heading.split(" > ").count();
            turtle.push_str(&format!(
                "    bobbin:heading \"{}\" ;\n    bobbin:headingDepth \"{depth}\"^^xsd:integer ;\n",
                escape_literal(heading)
            ));
        }
        turtle.push_str(&format!(
            "    rdfs:label \"{}\" .\n\n",
            escape_literal(&label)
        ));

        iri_of.insert(chunk.id.as_str(), iri);
        order_in_file += 1;
    }

    for edge in edges {
        if edge.edge_type != ChunkEdgeType::NextChunk {
            continue;
        }
        let (Some(src), Some(tgt)) = (
            iri_of.get(edge.source_chunk.as_str()),
            iri_of.get(edge.target_chunk.as_str()),
        ) else {
            continue;
        };
        turtle.push_str(&format!("<{src}> bobbin:nextChunk <{tgt}> .\n"));
    }

    turtle
}

/// The file entity a chunk belongs to: documents live on the live lane's
/// `doc/` base, source files on `code/` (aegis-6noan).
fn file_entity_iri(repo: &str, chunk: &Chunk) -> String {
    if matches!(chunk.language.as_str(), "markdown" | "pdf") {
        crate::iri::document_iri(repo, &chunk.file_path)
    } else {
        crate::iri::code_module_iri(repo, &chunk.file_path)
    }
}

fn governed_symbol_kind(chunk_type: ChunkType) -> Option<&'static str> {
    match chunk_type {
        ChunkType::Function => Some("function"),
        ChunkType::Method => Some("method"),
        ChunkType::Class => Some("class"),
        ChunkType::Struct => Some("struct"),
        ChunkType::Enum => Some("enum"),
        ChunkType::Interface => Some("interface"),
        // The loaded shape has no `trait` or `impl` member. Both remain
        // CodeSymbol facts, but omit the optional symbolKind rather than lie.
        ChunkType::Trait | ChunkType::Impl => None,
        _ => None,
    }
}

/// Push the chunk graph as a diffed snapshot replacement.
///
/// Returns `(tx_id, fact_count)`. Errors — without writing anything — if
/// the embedded quipu store predates `replace_snapshot`.
pub fn push_chunks_to_quipu(
    chunks: &[Chunk],
    edges: &[ChunkEdge],
    repo_name: &str,
    repo_root: &Path,
    target_graph: Option<&str>,
) -> Result<(i64, usize)> {
    let quipu_config = quipu::QuipuConfig::load(repo_root);
    let db_path = if quipu_config.store_path.is_relative() {
        repo_root.join(&quipu_config.store_path)
    } else {
        quipu_config.store_path.clone()
    };
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create quipu store directory")?;
    }
    let mut store = quipu::Store::open(db_path.to_string_lossy().as_ref())
        .map_err(|e| anyhow::anyhow!("Failed to open quipu store: {e}"))?;

    let snapshot_key = format!("bobbin-chunks:{repo_name}");

    // A named target must be PROVEN honoured before any write, or chunks could land in ROOT.
    if let Some(graph) = target_graph {
        require_graph_routing(&mut store, graph)?;
    }

    // Probe snapshot support with an EMPTY payload before writing facts: a
    // quipu that predates replace_snapshot ignores the key (no "replaced"
    // field in the response) and would accumulate one copy of the graph per
    // index run. The probe itself replaces nothing on a supporting store
    // (empty diff) and writes nothing on an old one.
    let probe = quipu::tool_knot(
        &mut store,
        &serde_json::json!({
            "turtle": "",
            "actor": "bobbin",
            "source": "chunk-index-probe",
            "replace_snapshot": true,
            "snapshot": snapshot_key,
            "graph": target_graph,
        }),
    )
    .map_err(|e| anyhow::anyhow!("Quipu snapshot probe failed: {e}"))?;
    if probe.get("replaced").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!(
            "embedded quipu predates snapshot replacement (no 'replaced' in /knot response); \
             refusing to push chunks — they would accumulate per index run. \
             The pinned quipu (0.3.23, rev 37bfc06a) supports it, so this store \
             was opened by something older."
        );
    }

    let turtle = generate_chunk_turtle(chunks, edges, repo_name);
    let result = quipu::tool_knot(
        &mut store,
        &serde_json::json!({
            "turtle": turtle,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "actor": "bobbin",
            "source": "chunk-index",
            "replace_snapshot": true,
            "snapshot": snapshot_key,
            "graph": target_graph,
        }),
    )
    .map_err(|e| anyhow::anyhow!("Failed to push chunks to quipu: {e}"))?;

    // With quipu's shacl feature compiled in (this build), a validation
    // refusal comes back as Ok with `conforms: false` — never mistake it
    // for a write.
    if result.get("conforms").and_then(|v| v.as_bool()) == Some(false) {
        anyhow::bail!("chunk push refused by SHACL validation: {result}");
    }

    Ok((
        result["tx_id"].as_i64().unwrap_or(-1),
        result["count"].as_u64().unwrap_or(0) as usize,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkType;

    pub(super) fn chunk(id: &str, file: &str, start: u32, name: Option<&str>) -> Chunk {
        Chunk {
            id: id.to_string(),
            file_path: file.to_string(),
            chunk_type: ChunkType::Section,
            name: name.map(str::to_string),
            start_line: start,
            end_line: start + 5,
            content: "body".to_string(),
            language: "markdown".to_string(),
            tags: String::new(),
        }
    }

    #[test]
    fn turtle_uses_stable_coordinates_not_chunk_ids() {
        let chunks = vec![
            chunk("hash1", "docs/guide.md", 1, Some("Intro")),
            chunk("hash2", "docs/guide.md", 7, Some("Intro > Setup")),
        ];
        let edges = vec![ChunkEdge {
            source_chunk: "hash1".to_string(),
            target_chunk: "hash2".to_string(),
            source_name: "Intro".to_string(),
            target_name: "Intro > Setup".to_string(),
            edge_type: ChunkEdgeType::NextChunk,
            file_path: "docs/guide.md".to_string(),
        }];
        let turtle = generate_chunk_turtle(&chunks, &edges, "myrepo");

        // Durable IRIs on the LIVE lane (aegis-6noan): a NAMED section takes
        // the identity of the section it is — `doc/{repo}/{path}#{slug}` — so
        // it merges with yupana's node instead of forking beside it. The slug is
        // of the LEAF heading, matching yupana; `Intro > Setup` is a breadcrumb.
        assert!(turtle.contains(
            "<http://aegis.gastown.local/ontology/doc/myrepo/docs%2Fguide.md#intro> \
             a bobbin:Chunk, bobbin:Section"
        ));
        assert!(turtle.contains(
            "<http://aegis.gastown.local/ontology/doc/myrepo/docs%2Fguide.md#intro> \
             bobbin:nextChunk \
             <http://aegis.gastown.local/ontology/doc/myrepo/docs%2Fguide.md#setup> ."
        ));
        // Membership points at the document entity on the doc lane, so the
        // graphs join.
        assert!(turtle.contains(
            "bobbin:inDocument <http://aegis.gastown.local/ontology/doc/myrepo/docs%2Fguide.md>"
        ));
        // The superseded lane must not reappear.
        assert!(!turtle.contains("http://aegis.gastown.local/code/"));
        assert!(!turtle.contains("/C1>"));
        // The volatile hash ids never reach the graph.
        assert!(!turtle.contains("hash1"));
    }

    #[test]
    fn turtle_maps_chunks_into_loaded_code_entity_vocabulary() {
        let code = Chunk {
            id: "fn".into(),
            file_path: "src/lib.rs".into(),
            chunk_type: ChunkType::Function,
            name: Some("parse".into()),
            start_line: 10,
            end_line: 20,
            content: "fn parse() {}".into(),
            language: "rust".into(),
            tags: String::new(),
        };
        let section = chunk("section", "docs/guide.md", 3, Some("Guide > Setup"));
        let turtle = generate_chunk_turtle(&[code, section], &[], "repo");

        assert!(turtle.contains(
            "<http://aegis.gastown.local/ontology/code/repo/src%2Flib.rs> a bobbin:CodeModule"
        ));
        assert!(turtle.contains("bobbin:repo \"repo\""));
        assert!(turtle.contains("bobbin:language \"rust\""));
        // A dual-typed code chunk IS yupana's CodeSymbol node: `{module}::{name}`.
        assert!(turtle.contains(
            "<http://aegis.gastown.local/ontology/code/repo/src%2Flib.rs::parse> \
             a bobbin:Chunk, bobbin:CodeSymbol"
        ));
        assert!(turtle.contains("bobbin:name \"parse\""));
        assert!(turtle.contains("bobbin:symbolKind \"function\""));
        assert!(turtle.contains(
            "<http://aegis.gastown.local/ontology/doc/repo/docs%2Fguide.md> a bobbin:Document"
        ));
        assert!(turtle.contains(
            "<http://aegis.gastown.local/ontology/doc/repo/docs%2Fguide.md#setup> \
             a bobbin:Chunk, bobbin:Section"
        ));
        assert!(turtle.contains("bobbin:heading \"Guide > Setup\""));
        assert!(turtle.contains("bobbin:headingDepth \"2\"^^xsd:integer"));
    }

    #[test]
    fn synthetic_line_zero_chunks_are_skipped() {
        let chunks = vec![chunk("c", "beads:rig:x", 0, None)];
        let turtle = generate_chunk_turtle(&chunks, &[], "r");
        assert!(!turtle.contains("beads"));
    }

    // Proof the pinned quipu (0.3.23) handles `replace_snapshot` server-side:
    // the probe passes and a re-push DIFFS instead of accumulating — the
    // failure mode the probe exists to refuse on older stores.
    #[test]
    fn snapshot_push_replaces_instead_of_accumulating() {
        let dir = tempfile::tempdir().unwrap();
        let two = vec![
            chunk("h1", "docs/guide.md", 1, Some("Intro")),
            chunk("h2", "docs/guide.md", 7, Some("Setup")),
        ];
        let (tx1, n1) = push_chunks_to_quipu(&two, &[], "r", dir.path(), None).expect("first push");
        assert!(tx1 > 0);
        assert!(n1 > 0);

        // Push again with one chunk vanished: its facts must retract.
        let one = vec![chunk("h1", "docs/guide.md", 1, Some("Intro"))];
        push_chunks_to_quipu(&one, &[], "r", dir.path(), None).expect("second push");

        let store = quipu::Store::open(dir.path().join(".bobbin/quipu/quipu.db").to_str().unwrap())
            .unwrap();
        // Named sections take the section identity, not the chunk-span one.
        let gone = crate::iri::section_iri("r", "docs/guide.md", "Setup");
        let facts = store.current_facts().unwrap();
        if let Some(id) = store.lookup(&gone).unwrap() {
            assert!(
                !facts.iter().any(|f| f.entity == id),
                "vanished chunk C7 must have no live facts after the replace"
            );
        }
        // And the survivor is still live.
        let kept = store
            .lookup(&crate::iri::section_iri("r", "docs/guide.md", "Intro"))
            .unwrap();
        let kept = kept.expect("surviving chunk stays interned");
        assert!(facts.iter().any(|f| f.entity == kept));
    }

    #[test]
    fn order_restarts_per_file() {
        let chunks = vec![
            chunk("a", "a.md", 1, None),
            chunk("b", "a.md", 9, None),
            chunk("c", "b.md", 3, None),
        ];
        let turtle = generate_chunk_turtle(&chunks, &[], "r");
        // Unnamed chunks have no governed entity to be, so they stay on the
        // chunk lane: `chunk/{repo}/{path}#C{line}`.
        let c_block = turtle.split("b.md#C3").nth(1).unwrap();
        assert!(c_block.contains("chunkOrder \"0\""));
    }
}
