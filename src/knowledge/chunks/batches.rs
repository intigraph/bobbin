//! Lab-only document projection and receipt gate. No production caller exists.
//!
//! A transport part is not a transaction. This module emits one complete
//! document at a time and refuses to advance without the future server's
//! bounded ownership-transfer receipt. It never falls back to repo promotion.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use crate::types::{Chunk, ChunkEdge, ChunkEdgeType, ChunkType};

pub const TRANSFER_CONTRACT: &str = "quipu.document-transfer/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Scope {
    pub repo: String,
    pub graph: Option<String>,
    pub generation: String,
    pub expected_legacy_head: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub bytes: usize,
    pub triples: usize,
    pub input_bytes: usize,
}

impl Limits {
    pub fn validate(self) -> Result<Self, String> {
        if !(64 * 1024..=512 * 1024).contains(&self.bytes)
            || self.triples == 0
            || self.triples > 100_000
            || self.input_bytes == 0
            || self.input_bytes > 8 * 1024 * 1024
        {
            return Err("invalid lab input/byte/triple budget".into());
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Batch {
    pub scope: Scope,
    pub document: String,
    pub sequence: u64,
    pub snapshot: String,
    pub content_hash: String,
    pub delete: bool,
    pub triples: usize,
    pub turtle: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn valid_document(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

/// Exact count for this serializer's controlled Turtle grammar, not a generic
/// Turtle parser. Each emitted predicate has one object except the type list.
fn triple_count(turtle: &str) -> usize {
    turtle
        .lines()
        .filter(|s| !s.starts_with('#') && !s.starts_with('@'))
        .map(|s| {
            if s.starts_with('<') && s.contains("> a bobbin:") {
                1 + usize::from(s.contains(", bobbin:"))
            } else {
                usize::from(s.ends_with(" ;") || s.ends_with(" ."))
            }
        })
        .sum()
}

/// Project ONLY the supplied document. Callers must load one document at a
/// time. A repository-wide Vec is not a bounded source implementation.
pub fn project(
    scope: &Scope,
    document: &str,
    sequence: u64,
    mut chunks: Vec<Chunk>,
    mut edges: Vec<ChunkEdge>,
    limits: Limits,
    delete: bool,
) -> Result<Batch, String> {
    let limits = limits.validate()?;
    if !valid_document(document)
        || scope.repo.is_empty()
        || scope.repo.len() > 256
        || scope.generation.is_empty()
        || scope.generation.len() > 256
        || scope.expected_legacy_head.is_empty()
        || scope.expected_legacy_head.len() > 256
        || scope
            .graph
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 2048)
    {
        return Err("missing exact scope, generation, legacy head or document".into());
    }
    if chunks.len() > limits.triples || edges.len() > limits.triples {
        return Err("input row budget exceeded before serialization".into());
    }
    if delete && (!chunks.is_empty() || !edges.is_empty()) {
        return Err("deletion must not carry replacement facts".into());
    }
    if !delete && chunks.is_empty() {
        return Err("empty replacement is not an implicit deletion".into());
    }
    let mut input_bytes = 0usize;
    for chunk in &mut chunks {
        if chunk.file_path != document || chunk.start_line == 0 {
            return Err("foreign document or synthetic chunk in document batch".into());
        }
        chunk.content = if chunk.chunk_type == ChunkType::Section {
            super::heading_depth::section_heading_prefix(&chunk.content)
                .unwrap_or("")
                .to_owned()
        } else {
            String::new()
        };
        chunk.tags = String::new();
        for field in [
            chunk.id.as_str(),
            &chunk.file_path,
            chunk.name.as_deref().unwrap_or(""),
            &chunk.content,
            &chunk.language,
        ] {
            input_bytes = input_bytes
                .checked_add(field.len())
                .ok_or("input overflow")?;
        }
    }
    for edge in &edges {
        for field in [
            &edge.source_chunk,
            &edge.target_chunk,
            &edge.source_name,
            &edge.target_name,
            &edge.file_path,
        ] {
            input_bytes = input_bytes
                .checked_add(field.len())
                .ok_or("input overflow")?;
        }
    }
    if input_bytes > limits.input_bytes {
        return Err("input metadata budget exceeded before serialization".into());
    }
    let ids: HashSet<_> = chunks.iter().map(|c| c.id.as_str()).collect();
    for edge in &edges {
        if edge.file_path != document
            || !ids.contains(edge.source_chunk.as_str())
            || (edge.edge_type == ChunkEdgeType::NextChunk
                && !ids.contains(edge.target_chunk.as_str()))
        {
            return Err("foreign ownership or cross-document adjacency".into());
        }
    }
    edges.sort_by_cached_key(|e| serde_json::to_string(e).expect("serializable edge"));
    let turtle = if delete {
        String::new()
    } else {
        super::serialize::generate_chunk_turtle(&chunks, &edges, &scope.repo)
    };
    let triples = triple_count(&turtle);
    if turtle.len() > limits.bytes || triples > limits.triples {
        return Err("serialized byte/triple budget exceeded; no batch admitted".into());
    }
    let identity = serde_json::to_vec(&(&scope.repo, &scope.graph, document)).unwrap();
    Ok(Batch {
        scope: scope.clone(),
        document: document.into(),
        sequence,
        snapshot: format!("bobbin-document:v1:{}", hash(&identity)),
        content_hash: hash(turtle.as_bytes()),
        delete,
        triples,
        turtle,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub contract: String,
    pub scope: Scope,
    pub document: String,
    pub sequence: u64,
    pub snapshot: String,
    pub content_hash: String,
    pub delete: bool,
    pub tx_id: u64,
    pub ownership_moved: bool,
    pub writer_released: bool,
}

/// Durable callers persist this state only AFTER an exact committed receipt.
/// A timeout, a cancelled response, or a legacy promote reply cannot advance it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Checkpoint {
    pub scope: Option<Scope>,
    pub next_sequence: u64,
    pub last_receipt_hash: Option<String>,
}

impl Checkpoint {
    pub fn admit(&self, batch: &Batch, stopped: bool) -> Result<(), String> {
        if stopped {
            return Err("stop observed before next batch".into());
        }
        if batch.sequence != self.next_sequence
            || self.scope.as_ref().is_some_and(|s| s != &batch.scope)
        {
            return Err("stale generation, scope or sequence".into());
        }
        Ok(())
    }

    pub fn accept(&mut self, batch: &Batch, receipt: &Receipt) -> Result<(), String> {
        self.admit(batch, false)?;
        if receipt.contract != TRANSFER_CONTRACT
            || receipt.scope != batch.scope
            || receipt.document != batch.document
            || receipt.sequence != batch.sequence
            || receipt.snapshot != batch.snapshot
            || receipt.content_hash != batch.content_hash
            || receipt.delete != batch.delete
            || !receipt.ownership_moved
            || !receipt.writer_released
        {
            return Err(
                "exact bounded ownership-transfer receipt missing; preserve checkpoint".into(),
            );
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or("sequence overflow")?;
        self.scope = Some(batch.scope.clone());
        self.last_receipt_hash = Some(hash(&serde_json::to_vec(receipt).unwrap()));
        Ok(())
    }
}

#[cfg(test)]
#[path = "batches_tests.rs"]
mod tests;
