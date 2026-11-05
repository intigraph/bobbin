---
title: Quipu Integration
description: Using Quipu's knowledge graph alongside Bobbin's code search for unified context
tags: [quipu, knowledge-graph, guide]
status: draft
category: guide
related: [mcp/tools.md, architecture/overview.md, guides/context-assembly.md]
---

# Quipu Integration

Bobbin optionally integrates with [Quipu](https://github.com/scbrown/quipu), a knowledge graph that stores structured facts as an EAVT (Entity-Attribute-Value-Time) log. When enabled, Bobbin's MCP server exposes Quipu's tools alongside its own, giving AI agents access to both code search and structured knowledge in a single session.

## Enabling the Integration

Quipu is gated behind the `knowledge` Cargo feature:

```bash
cargo build --features knowledge
```

This pulls in Quipu as a git dependency. Without the feature flag, Bobbin compiles and runs normally with no Quipu code included.

## Configuration

When the `knowledge` feature is enabled, add a `[knowledge]` section to `.bobbin/config.toml`:

```toml
[knowledge]
enabled = true
store_path = ".bobbin/knowledge.db"
schema_path = "schemas/"          # SHACL shapes for validation
auto_embed = true                 # embed entities using Bobbin's ONNX pipeline
```

## MCP Tools

With Quipu enabled, `bobbin serve` exposes these additional MCP tools:

| Tool | Description |
|------|-------------|
| `knowledge_context` | Semantic search over knowledge graph entities, across the remote ontology and the local code graph. |
| `knowledge_query` | Run SPARQL SELECT queries against both graphs, with optional temporal filtering. |
| `knowledge_export` | Export a canonical scoped RDF slice from the remote Quipu, optionally digest-only. |
| `knowledge_share` | Produce Quipu's canonical v1 manifest and exact share files. |
| `knowledge_import` | Verify and stage a canonical share; never promotes it. |
| `knowledge_import_promote` | Explicitly promote an eligible staged share into ROOT. |
| `knowledge_knot` | Write facts into the local embedded graph as RDF Turtle, with SHACL validation when compiled in. |
| `knowledge_reconcile_mentions` | Resolve chunk mention literals into typed edges against the live entity graph. |
| `knowledge_inferred_extract` | Run the quarantined-track extractor over markdown prose (see below). |

These appear alongside Bobbin's existing tools (search, grep, context, etc.) in a single MCP server. See the [Tools Reference](../mcp/tools.md) for parameters and response shapes.

### Example: knowledge_context

Ask for knowledge entities related to a topic:

```json
{
  "tool": "knowledge_context",
  "arguments": {
    "query": "authentication flow",
    "limit": 10
  }
}
```

### Example: knowledge_query

Run a SPARQL query against the graph:

```json
{
  "tool": "knowledge_query",
  "arguments": {
    "sparql": "SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 10"
  }
}
```

## Publishing Chunk Snapshots

With `quipu_push_chunks = true` (a top-level key in `.bobbin/config.toml`), every index run publishes the repository's governed code-entity graph as a **diffed snapshot replacement** under the producer key `bobbin-chunks:{repo}` — so a reindex diffs against the previous run rather than accumulating a copy per run. The snapshot carries `CodeModule`, `CodeSymbol`, `Document`, and `Section` facts plus chunk identity, membership, order, and adjacency — never chunk content.

Incremental publication uses the complete stored graph for the repository's current file walk,
including unchanged files and their edges. A changed-file delta is not a replacement snapshot:
it would retract unchanged files remotely. No-change runs with source files also publish, so a
previous failed publication can recover without forcing source re-embedding. Deleted or excluded
files and non-file sources are left out. This adds stored-graph reads and upload work to those
runs. Empty repositories retain the existing no-publication behavior; deleting the last file
requires a separate snapshot-retirement operation.

Where the snapshot goes depends on `quipu_endpoint`:

- **Set** — the snapshot is delivered to that remote Quipu's authenticated `/knot` endpoint. The bearer token is resolved from `QUIPU_AUTH_TOKEN`, then `QUIPU_AUTH_TOKEN_FILE`, then `~/.config/aegis/quipu_token`; with no token available the push fails rather than sending unauthenticated.
- **Unset** — the snapshot lands in the embedded local store.

Either way the target must support snapshot replacement: Bobbin probes for it first and refuses (with a message) rather than letting facts accumulate per run. After a successful push, `bobbin index` also runs the mention-reconcile pass over the local graph — the same pass the `knowledge_reconcile_mentions` tool runs on demand.

Chunk publication is part of a successful index run, not best-effort telemetry. If the single
bounded `/knot` attempt fails, the command reports `dropped_pushes=1` and exits nonzero; it does
not retry an abandoned request or let a scheduler record an incomplete graph as success. Remote
requests carry `X-Quipu-Client: ingest-cron` for server-side attribution.

## Quarantined Inferred Extraction

Facts that are *inferred* from prose are claims, not observations, and Bobbin keeps that distinction structural. The inferred track is a pluggable extractor seam; the one extractor today is a deterministic backtick-coderef heuristic over markdown — not a language model, and every fact it produces says so.

Candidates never mix with observed facts:

- Each fact carries its extractor and parameters as the derivation method, and `sourceKind = inferred`.
- Facts land only in a dedicated quarantined plane at trust rank 0, via a graph-routed `/knot` write. The push refuses when the store cannot enforce graph routing — inferred facts must never masquerade in the default graph at observed standing.
- The `knowledge_inferred_extract` MCP tool serves candidates only inside the quarantine envelope (plane, trust rank, source kind), never bare.
- Promotion out of quarantine is the governing ontology's authority-gated flow, deliberately not implemented in Bobbin.

Set `quipu_push_inferred = true` (top-level key, opt-in) to run the extractor over markdown prose during indexing and land the stamped candidates in the quarantined plane as a diffed snapshot. It requires the `knowledge` feature, a Quipu revision with strict `/knot` graph routing, and the quarantine plane registered and trust-labelled on the store.

## Architecture

```text
┌─────────────────────────────────────────────────────┐
│                    Agent / Claude Code               │
│                                                      │
│  MCP Tools:                                          │
│    search, context, grep, refs, ...    (Bobbin)      │
│    knowledge_context, knowledge_query  (Quipu)       │
└──────────────────────┬──────────────────────────────┘
                       │
        ┌──────────────┼──────────────┐
        │              │              │
   ┌────┴────┐    ┌────┴────┐   ┌────┴────┐
   │ Bobbin  │    │ Unified │   │  Quipu  │
   │  Code   │    │ Context │   │Knowledge│
   │ Search  │    │ Pipeline│   │  Graph  │
   └────┬────┘    └────┬────┘   └────┬────┘
        │              │              │
   ┌────┴────┐         │         ┌────┴────┐
   │ LanceDB │         │         │ SQLite  │
   │ vectors │         │         │  EAVT   │
   │ + FTS   │         │         │+ vectors│
   └─────────┘         │         └─────────┘
                       │
              ┌────────┴────────┐
              │  ONNX Embedder  │
              │ (shared session)│
              └─────────────────┘
```

Key design decisions:

- **Feature-gated**: Quipu is optional (`--features knowledge`). Bobbin works without it.
- **Async bridge**: Quipu is synchronous; Bobbin is async. Calls bridge via `tokio::task::spawn_blocking()`.
- **Shared embeddings**: Both systems use the same ONNX model session for vector generation.
- **Single MCP server**: One `bobbin serve` process exposes both Bobbin and Quipu tools.

## Share bundle contract

Bobbin's knowledge build includes typed consumer models for Quipu's versioned
share manifest and import request/response. A canonical v1 share contains
`manifest.json`, `export.nt`, and `shapes.ttl`; `export.ttl` is an optional
human-readable view and is not the graph identity. Consumers reject unsupported
manifest versions and non-canonical paths. Additive fields within v1 are
preserved during typed round trips so an older Bobbin does not erase extensions
from a newer compatible Quipu.

The checked-in fixture at `tests/fixtures/quipu-share-v1/` exercises graph and
shape hashes, the JCS-derived share identity, sorted duplicate-free N-Triples,
staged import resolution, quarantine, and promotion blockers. It is the contract
fixture for future `knowledge_*` adapters. Those adapters delegate canonical
serialization, identity resolution, quarantine, and RDF merge semantics to
Quipu; Bobbin does not define a parallel bundle format. `knowledge_share`
returns the producer's manifest and files unchanged. `knowledge_import` preserves
the producer's resolution, validation, quarantine, and promotion state, while
`knowledge_import_promote` keeps admission as a separate deliberate operation.

## Integration Roadmap

The integration is being built in phases. See [docs/plans/quipu-integration.md](https://github.com/scbrown/bobbin/blob/main/docs/plans/quipu-integration.md) for the full plan.

| Phase | Status | Description |
|-------|--------|-------------|
| 1. Crate dependency | Done | Quipu as git dep, feature-gated behind `knowledge` |
| 2. Shared embedding pipeline | Planned | Shared ONNX session via `EmbeddingProvider` trait |
| 3. MCP tool surface | Done | `knowledge_context` and `knowledge_query` tools wired in |
| 4. Unified search results | Planned | Merge code + knowledge results with normalized scores |
| 5. Knowledge-aware context | Planned | Context assembly expanded with knowledge graph facts |

## Governed Path Boundaries

Setting `quipu_endpoint` also turns on tripwire surfacing: governed path-boundary
policies spanning the files Bobbin is about to inject are named in the injected
context, so an agent sees a boundary before it crosses one. Unlike the MCP tools
above this needs no `knowledge` feature — it reads Quipu's `POST /query` over
HTTP. See [Governed Path Boundaries](governed-boundaries.md).

## See Also

- [Governed Path Boundaries](governed-boundaries.md)
- [Architecture Overview](../architecture/overview.md)
- [MCP Tools Reference](../mcp/tools.md)
- [Context Assembly](context-assembly.md)
- [Quipu Integration Plan](https://github.com/scbrown/bobbin/blob/main/docs/plans/quipu-integration.md)

The index stores a SHA-256 hash after successful publication, scoped to the repository
and destination. An unchanged snapshot sends no publication requests, including across
process restarts. Failed publishes do not advance this hash and retry on the next run.
`index --force-publish` bypasses only the publication hash for recovery after
remote data loss, without re-embedding unchanged files. Full `index --force` also
bypasses the hash and retains its existing re-embedding behavior.

## Remote client credentials

Remote chunk and MCP share writes use one resolver: nonempty
`QUIPU_AUTH_TOKEN`, then `QUIPU_AUTH_TOKEN_FILE`, then
`~/.config/quipu/token`. An explicit file never falls back when missing or empty.
The older token location is supported only through an explicit file override.

Missing or unreadable credentials and HTTP 401 print an actionable diagnostic
once per server and disable further writes to that server for the current
harness session, across new Bobbin processes. Without a stable harness session
ID or writable session-state directory, the diagnostic names its process-only
limit. Public reads remain available. Install the issued credential at the
canonical path, run `tambo doctor`, then start a new harness session (or restart
a process without a session ID) to re-enable writes. Bobbin never rotates credentials or relaxes server authorization.
