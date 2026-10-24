---
title: Embedding Settings
description: Configuring the embedding model, batch size, and ONNX runtime
tags: [config, embedding, onnx]
status: draft
category: config
related: [architecture/embedding.md, config/reference.md, cli/benchmark.md]
---

# Embedding Settings

The `[embedding]` and `[embedding.context]` sections control embedding model and batch processing.

## Configuration

```toml
[embedding]
model = "all-MiniLM-L6-v2"
batch_size = 32

[embedding.context]
context_lines = 5
enabled_languages = ["markdown"]
```

## `[embedding]` Options

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `model` | string | `"all-MiniLM-L6-v2"` | ONNX embedding model name. Downloaded to `~/.cache/bobbin/models/` on first use. |
| `batch_size` | int | `32` | Number of chunks to embed per batch |

## `[embedding.context]` Options

Controls contextual embedding, where chunks are embedded with surrounding source lines for better retrieval.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `context_lines` | int | `5` | Lines of context before and after each chunk |
| `enabled_languages` | string[] | `["markdown"]` | Languages where contextual embedding is active |

## Notes

- The embedding model is downloaded automatically on first run and cached in `~/.cache/bobbin/models/`.
- Contextual embedding enriches each chunk with surrounding lines before computing its vector, improving search relevance at the cost of slightly longer indexing time.
- Increasing `batch_size` may improve indexing throughput but uses more memory.

## Native runtime validation

Before loading a local ONNX model, Bobbin validates session creation in a
bounded child process. This adds one model-load check per model/runtime pair
in a process. The check does not download libraries or activate CUDA.

Some system builds of ONNX Runtime link an ONNX library with static schema
registration enabled. Creating a session then registers the schemas again,
producing hundreds of native `Schema error` messages even when session creation
returns success. This packaging interaction is also tracked in
[the ONNX Runtime ecosystem](https://github.com/microsoft/vcpkg/issues/49060).
Bobbin rejects that runtime with one diagnostic rather than claiming healthy
semantic retrieval. Keyword search does not require an embedding runtime.
Use a compatible release-bundled runtime, or set `ORT_DYLIB_PATH` to one;
Bobbin does not replace a system library or silently turn semantic search into
keyword search.

Normal CLI logging defaults to warnings. `RUST_LOG` remains an explicit logging
override; dependency INFO diagnostics, which can contain absolute index paths,
are no longer printed by default.
