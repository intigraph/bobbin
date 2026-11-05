//! Deterministic entity extraction from indexed chunks (roadmap W3.A).
//!
//! The `entities` LanceDB table shipped with a full read/write API and no
//! producer. This is the producer, and it is strictly the deterministic
//! track: entities are derived from parser output (symbols, sections,
//! files) — observed facts, never model inference. The model/NLP track
//! lands quarantined in the knowledge graph per the camayoc ingress
//! discipline and is specified separately.
//!
//! Entity IRIs use the live-lane scheme (`crate::iri`), so a semantic hit
//! on a local entity is the same identity the quipu knowledge graph knows.

use crate::types::{Chunk, ChunkType, Entity};

/// Derive entity rows from one repo's freshly parsed chunks. Pure.
///
/// - One `CodeModule`/`Document` entity per file (by language).
/// - One `CodeSymbol` entity per named code chunk.
/// - One `Section` entity per named markdown section.
///
/// Synthetic sources (commits, beads, SQL rows — line 0) don't participate;
/// they have their own identity schemes and search surfaces.
pub fn build_entities(chunks: &[Chunk], repo: &str) -> Vec<Entity> {
    let mut entities = Vec::new();
    let mut seen_files: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for chunk in chunks.iter().filter(|c| c.start_line > 0) {
        if seen_files.insert(chunk.file_path.as_str()) {
            let is_doc = chunk.language == "markdown" || chunk.language == "pdf";
            // Documents live on their own `doc/` lane in the live yupana scheme,
            // not on `code/` with a different rdf:type (aegis-6noan).
            entities.push(Entity {
                entity_iri: if is_doc {
                    crate::iri::document_iri(repo, &chunk.file_path)
                } else {
                    crate::iri::code_module_iri(repo, &chunk.file_path)
                },
                text: chunk.file_path.clone(),
                entity_type: if is_doc { "Document" } else { "CodeModule" }.to_string(),
                repo: Some(repo.to_string()),
            });
        }

        let Some(name) = chunk.name.as_deref() else {
            continue;
        };
        match chunk.chunk_type {
            t if t.is_code_symbol() => {
                entities.push(Entity {
                    entity_iri: crate::iri::symbol_iri(repo, &chunk.file_path, name),
                    text: format!("{} ({} in {})", name, chunk.chunk_type, chunk.file_path),
                    entity_type: "CodeSymbol".to_string(),
                    repo: Some(repo.to_string()),
                });
            }
            ChunkType::Section => {
                entities.push(Entity {
                    entity_iri: crate::iri::section_iri(repo, &chunk.file_path, name),
                    text: format!("{} ({})", name, chunk.file_path),
                    entity_type: "Section".to_string(),
                    repo: Some(repo.to_string()),
                });
            }
            _ => {}
        }
    }

    entities
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(file: &str, t: ChunkType, name: Option<&str>, start: u32, lang: &str) -> Chunk {
        Chunk {
            id: format!("{file}:{start}"),
            file_path: file.to_string(),
            chunk_type: t,
            name: name.map(str::to_string),
            start_line: start,
            end_line: start + 3,
            content: "body".to_string(),
            language: lang.to_string(),
            tags: String::new(),
        }
    }

    #[test]
    fn derives_modules_symbols_and_sections() {
        let chunks = vec![
            chunk("src/lib.rs", ChunkType::Function, Some("parse"), 10, "rust"),
            chunk("src/lib.rs", ChunkType::Struct, Some("Config"), 30, "rust"),
            chunk(
                "docs/a.md",
                ChunkType::Section,
                Some("Guide > Setup"),
                5,
                "markdown",
            ),
        ];
        let entities = build_entities(&chunks, "myrepo");

        let types: Vec<&str> = entities.iter().map(|e| e.entity_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "CodeModule",
                "CodeSymbol",
                "CodeSymbol",
                "Document",
                "Section"
            ]
        );
        // The LIVE lane (aegis-6noan): symbols are `{module}::{name}` on the
        // `code/` base, sections are `{document}#{leaf-slug}` on `doc/`. The
        // local entities table and the quipu emitters share `crate::iri`, so
        // a semantic hit here is the same identity yupana's graph knows.
        assert_eq!(
            entities[1].entity_iri,
            "http://aegis.gastown.local/ontology/code/myrepo/src%2Flib.rs::parse"
        );
        assert_eq!(
            entities[4].entity_iri,
            "http://aegis.gastown.local/ontology/doc/myrepo/docs%2Fa.md#setup"
        );
        // No entity may regress to the superseded ingest-repos.py lane.
        for e in &entities {
            assert!(
                !e.entity_iri.starts_with("http://aegis.gastown.local/code/"),
                "regressed to the superseded lane: {}",
                e.entity_iri
            );
        }
    }

    #[test]
    fn synthetic_and_unnamed_chunks_are_skipped() {
        let chunks = vec![
            chunk("beads:rig:1", ChunkType::Issue, Some("x"), 0, "beads"),
            chunk("src/a.rs", ChunkType::Other, None, 1, "rust"),
        ];
        let entities = build_entities(&chunks, "r");
        // Only the per-file module entity for a.rs; nothing for the bead.
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, "CodeModule");
    }
}
