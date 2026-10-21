---
title: "Storage & Data Flow"
description: How bobbin stores indexes, embeddings, and coupling data in .bobbin/
tags: [architecture, storage, lancedb]
status: draft
category: architecture
related: [architecture/overview.md, config/index.md]
---

# Storage & Data Flow

Bobbin uses a dual-storage architecture: LanceDB as the primary store for chunks, embeddings, and full-text search, with SQLite handling temporal coupling data and global metadata.

## LanceDB (Primary Storage)

All chunk data, embeddings, and full-text search live in LanceDB:

```text
chunks table:
  - id: string            # SHA256-based unique chunk ID
  - vector: float[384]    # MiniLM embedding
  - repo: string          # Repository name (for multi-repo support)
  - file_path: string     # Relative file path
  - file_hash: string     # Content hash for incremental indexing
  - language: string      # Programming language
  - chunk_type: string    # function, method, class, section, etc.
  - chunk_name: string?   # Function/class/section name (nullable)
  - start_line: uint32    # Starting line number
  - end_line: uint32      # Ending line number
  - content: string       # Original chunk content
  - full_context: string? # Context-enriched text used for embedding (nullable)
  - indexed_at: string    # Timestamp
```

LanceDB also maintains an FTS index on the `content` field for keyword search.

Reparsed files replace their chunk-level relationship edges even when parsing
emits no new edges. The indexer clears selected paths in batches of at most 100,
scoped to the repository, so other repositories' edges remain intact. Each batch
uses the existing conflict retries; a persistent error stops indexing before
replacement edges are inserted. Deletion across batches is not atomic. The
existing legacy absolute-path edge cleanup runs before these scoped batches.

## SQLite (Auxiliary)

SQLite only stores temporal coupling data and global metadata:

```sql
-- Temporal coupling (git co-change relationships)
CREATE TABLE coupling (
    file_a TEXT NOT NULL,
    file_b TEXT NOT NULL,
    score REAL,
    co_changes INTEGER,
    last_co_change INTEGER,
    PRIMARY KEY (file_a, file_b)
);

-- Global metadata
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT
);
```

## Key Types

### Chunk

A semantic unit extracted from source code:

```rust
struct Chunk {
    id: String,           // SHA256-based unique ID
    file_path: String,    // Source file path
    chunk_type: ChunkType,// function, class, struct, etc.
    name: Option<String>, // Function/class name
    start_line: u32,      // Starting line number
    end_line: u32,        // Ending line number
    content: String,      // Actual code content
    language: String,     // Programming language
}
```

### SearchResult

```rust
struct SearchResult {
    chunk: Chunk,              // The matched chunk
    score: f32,                // Relevance score
    match_type: Option<MatchType>, // How it was matched
}
```

## Hybrid Search (RRF)

The hybrid search combines semantic (vector) and keyword (FTS) results using Reciprocal Rank Fusion:

```text
RRF_score = semantic_weight / (k + semantic_rank) + keyword_weight / (k + keyword_rank)
```

Where:

- `k = 60` (standard RRF constant)
- `semantic_weight` from config (default 0.7)
- `keyword_weight = 1 - semantic_weight`

Results that appear in both searches get boosted scores and are marked as `[hybrid]` matches.
