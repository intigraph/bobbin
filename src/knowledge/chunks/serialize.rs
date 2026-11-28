//! Pure chunk graph serialization shared with the isolated batch lab.
use super::heading_depth::section_heading_depth;
use crate::iri::{chunk_iri, ONTOLOGY_NS};
use crate::knowledge::turtle::escape_literal;
use crate::types::{Chunk, ChunkEdge, ChunkEdgeType, ChunkType};

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
    let edge_mentions = crate::knowledge::mentions::edge_mention_map(edges);

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
        for name in crate::knowledge::mentions::chunk_mention_names(chunk, &edge_mentions) {
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
            let depth = section_heading_depth(&chunk.content, heading);
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
