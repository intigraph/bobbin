//! Streaming lab planner. No network, credentials, stores or production caller.
#![allow(dead_code, unused_imports)] // Shared native types contain unrelated application types.
mod config {
    pub struct FileTypeRule {
        pub name: String,
        pub patterns: Vec<String>,
    }
}
#[path = "../../../src/iri/mod.rs"]
mod iri;
mod knowledge;
#[path = "../../../src/types.rs"]
mod types;
use knowledge::chunks::batches::{project, Limits, Scope};
use serde::Deserialize;
use std::io::{BufRead, Read, Write};
#[derive(Deserialize)]
struct Input {
    document: String,
    chunks: Vec<types::Chunk>,
    edges: Vec<types::ChunkEdge>,
    #[serde(default)]
    delete: bool,
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 6 {
        return Err(
            "usage: planner MAX_BYTES MAX_TRIPLES REPO GENERATION LEGACY_HEAD (NDJSON stdin)"
                .into(),
        );
    }
    let limits = Limits {
        bytes: args[1].parse()?,
        triples: args[2].parse()?,
        input_bytes: 8 * 1024 * 1024,
    }
    .validate()?;
    let scope = Scope {
        repo: args[3].clone(),
        graph: None,
        generation: args[4].clone(),
        expected_legacy_head: args[5].clone(),
    };
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    for sequence in 0..100_000u64 {
        let mut line = Vec::new();
        let n = input
            .by_ref()
            .take(8 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(());
        }
        if n > 8 * 1024 * 1024 || !line.ends_with(b"\n") {
            return Err("input record exceeds bound or is incomplete".into());
        }
        let row: Input = serde_json::from_slice(&line)?;
        let batch = project(
            &scope,
            &row.document,
            sequence,
            row.chunks,
            row.edges,
            limits,
            row.delete,
        )?;
        serde_json::to_writer(&mut output, &batch)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Err("document count bound exceeded".into())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("REFUSED: {e}");
        std::process::exit(2);
    }
}
