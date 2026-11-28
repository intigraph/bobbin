//! Pure observed symbol-mention projection.
use crate::types::{Chunk, ChunkEdge, ChunkEdgeType};
use std::collections::HashMap;

fn is_symbol_edge(t: ChunkEdgeType) -> bool {
    matches!(
        t,
        ChunkEdgeType::Implements
            | ChunkEdgeType::ImplFor
            | ChunkEdgeType::Extends
            | ChunkEdgeType::Tests
    )
}

/// Map source-chunk id → symbol names its edges target. Pure.
pub(crate) fn edge_mention_map(edges: &[ChunkEdge]) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        if is_symbol_edge(edge.edge_type) && !edge.target_name.is_empty() {
            map.entry(edge.source_chunk.as_str())
                .or_default()
                .push(edge.target_name.as_str());
        }
    }
    map
}

/// Symbol names one chunk mentions: its own name (for code-symbol chunk
/// types — the same set the entity extractor mints `CodeSymbol` for, so
/// every self-mention has a mintable counterpart) plus the names its
/// symbol-bearing edges target. Deduplicated and sorted for deterministic
/// turtle. Pure — derived from parser output only.
pub(crate) fn chunk_mention_names<'a>(
    chunk: &'a Chunk,
    edge_mentions: &HashMap<&str, Vec<&'a str>>,
) -> Vec<&'a str> {
    let mut names: Vec<&str> = Vec::new();
    if chunk.chunk_type.is_code_symbol() {
        if let Some(name) = chunk.name.as_deref() {
            names.push(name);
        }
    }
    if let Some(targets) = edge_mentions.get(chunk.id.as_str()) {
        names.extend(targets.iter().copied());
    }
    names.sort_unstable();
    names.dedup();
    names
}
