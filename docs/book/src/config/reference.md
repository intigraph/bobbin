---
title: Configuration Reference
description: Complete reference for bobbin's config.toml settings
tags: [config, reference]
status: draft
category: config
related: [config/index.md, config/search.md, config/embedding.md, config/hooks.md]
---

# Configuration Reference

## Runtime thread budget

`BOBBIN_TOKIO_BLOCKING_THREADS` optionally limits Tokio's blocking I/O pool to
an integer from 1 through 512. For example, set it to `2` in a constrained job's
managed environment. Invalid or zero values refuse runtime construction.
Unset preserves Tokio's default blocking pool.

This is separate from `TOKIO_WORKER_THREADS`, which sizes async workers. Both
pools count toward a process or service thread limit. The blocking budget queues
excess tasks; it does not drop work or raise the service's thread limit. Other
library worker pools still need their own budgets. A bounded pool is not proof
that a full indexing workload fits a particular service limit; verify that
workload's peak and rejection counters after installation.

## Configuration Hierarchy

Bobbin uses a layered configuration system. Each layer overrides the one above:

| Priority | Location | Purpose |
|----------|----------|---------|
| 1 (lowest) | Compiled defaults | Sensible out-of-the-box values |
| 2 | `~/.config/bobbin/config.toml` | Machine-wide defaults (global config) |
| 3 | `.bobbin/config.toml` | Project-specific settings (per-repo config) |
| 4 | `.bobbin/calibration.json` | Auto-tuned search parameters |
| 5 (highest) | CLI flags | Per-invocation overrides |

### How merging works

When both global and per-repo configs exist, they are **deep-merged**:

- **Tables** merge recursively — a per-repo `[search]` section only needs to set the fields it wants to override. Unset fields inherit from global config.
- **Arrays** replace wholesale — a per-repo `index.include` replaces the entire global include list.
- **Scalars** replace — a per-repo `search.semantic_weight = 0.5` overrides the global value.

### Example

Global config (`~/.config/bobbin/config.toml`):

```toml
[server]
url = "http://search.example"

[hooks]
gate_threshold = 0.50
budget = 300
skip_prefixes = ["git ", "bd ", "gt "]
```

Per-repo config (`.bobbin/config.toml`):

```toml
[search]
semantic_weight = 0.8

[hooks]
budget = 500  # Override just this field; gate_threshold inherits 0.50
```

Result: server.url = `http://search.example`, hooks.gate_threshold = 0.50, hooks.budget = 500, search.semantic_weight = 0.8.

### Calibration overlay

`bobbin calibrate --apply` writes optimal search parameters to `.bobbin/calibration.json`. These override config.toml values for: `semantic_weight`, `doc_demotion`, `rrf_k`, and optionally `recency_half_life_days`, `recency_weight`, `coupling_depth`, `budget_lines`, `search_limit`, `bridge_mode`, `bridge_boost_factor`.

The `search`, `context`, and `hook` commands all load calibration.json and prefer its values over config.toml. CLI flags still override everything.

### Tags and reactions

Two additional config files have their own global/local merge behavior:

- **`tags.toml`** — tag pattern rules. Loaded from `.bobbin/tags.toml` or `~/.config/bobbin/tags.toml` (server loads at startup; requires restart to pick up changes).
- **`reactions.toml`** — hook reaction rules. Merged: global `~/.config/bobbin/reactions.toml` + local `.bobbin/reactions.toml`. Local rules override global rules by name.

## Full Default Configuration

```toml
[index]
# Glob patterns for files to include
include = [
    "**/*.rs",
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.py",
    "**/*.go",
    "**/*.java",
    "**/*.cpp",
    "**/*.cc",
    "**/*.hpp",
    "**/*.md",
]

# Glob patterns for files to exclude (in addition to .gitignore)
exclude = [
    "**/node_modules/**",
    "**/target/**",
    "**/dist/**",
    "**/.git/**",
    "**/build/**",
    "**/__pycache__/**",
]

# Whether to respect .gitignore files
use_gitignore = true

[embedding]
# Embedding model (downloaded automatically on first run)
model = "all-MiniLM-L6-v2"

# Batch size for embedding generation
batch_size = 32

[embedding.context]
# Number of context lines to include before and after a chunk
# when generating its embedding. More context improves retrieval
# quality at the cost of slightly longer indexing time.
context_lines = 5

# Languages where contextual embedding is enabled.
# Contextual embedding enriches each chunk with surrounding
# lines before computing its vector, improving search relevance.
enabled_languages = ["markdown"]

[search]
# Default number of search results
default_limit = 10

# Weight for semantic vs keyword search in hybrid mode.
# 0.0 = keyword only, 1.0 = semantic only, default 0.9.
semantic_weight = 0.9

[git]
# Enable temporal coupling analysis (tracks which files
# frequently change together in git history)
coupling_enabled = true

# Number of commits to analyze for coupling relationships
coupling_depth = 5000

# Minimum number of co-changes required to establish a coupling link
coupling_threshold = 3

# Enable semantic commit indexing (embed commit messages for search)
commits_enabled = true

# How many commits back to index for semantic search (0 = all)
commits_depth = 0
```

## Documentation-Heavy Projects

For projects that are primarily markdown documentation (doc sites, wikis, knowledge bases), consider these tuned settings:

```toml
[index]
include = [
    "**/*.md",
]

exclude = [
    "**/node_modules/**",
    "**/build/**",
    "**/dist/**",
    "**/.git/**",
    "**/site/**",            # MkDocs build output
    "**/book/**",            # mdBook build output
    "**/_build/**",          # Sphinx build output
]

[embedding.context]
context_lines = 5
enabled_languages = ["markdown"]

[search]
semantic_weight = 0.8        # Favor semantic search for natural-language queries
```

Key differences from the defaults:

- **Include** restricted to `**/*.md` to skip non-documentation files
- **Exclude** adds common doc-tool build output directories
- **Semantic weight** raised to 0.8 since documentation queries tend to be natural language

For projects that mix code and documentation, keep the default include patterns and add the documentation-specific exclude patterns.

See [Indexing Documentation](../guides/documentation.md) for a full walkthrough.

## Sections

| Section | Description | Details |
|---------|-------------|---------|
| `[index]` | File selection patterns and gitignore behavior | [Index Settings](index.md) |
| `[embedding]` | Embedding model and batch processing | [Embedding Settings](embedding.md) |
| `[embedding.context]` | Contextual embedding enrichment | [Embedding Settings](embedding.md) |
| `[search]` | Search defaults and hybrid weighting | [Search Settings](search.md) |
| `[git]` | Temporal coupling analysis from git history | See below |
| `[hooks]` | Claude Code hook integration and injection tuning | [Hooks Configuration](hooks.md) |

## `[git]` Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `coupling_enabled` | bool | `true` | Enable temporal coupling analysis |
| `coupling_depth` | int | `5000` | How many commits back to analyze for coupling |
| `coupling_threshold` | int | `3` | Minimum co-changes to establish a coupling relationship |
| `commits_enabled` | bool | `true` | Enable semantic commit indexing |
| `commits_depth` | int | `0` | How many commits back to index for semantic search (0 = all) |
