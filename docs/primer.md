# Bobbin agent primer

## What Bobbin is

Bobbin is a local-first code context engine for agents.
It combines semantic search, keyword search, git coupling and context assembly.
Indexes live in LanceDB and SQLite alongside the repository.
Search and context work locally without API keys.
Quipu supplies shared knowledge; Yupana supplies structural change analysis.

## Agent workflow

1. Search before scanning files: `bobbin search "<intent>"`.
2. Read a returned chunk or assemble context with `bobbin context "<task>"`.
3. Ask `bobbin impact <file>` for related files; use Yupana for structural callers.
4. Check `bobbin status` before trusting an empty result; reindex with `bobbin index`.
5. Treat an old index or a failed read as unknown, not as proof of absence.

## Core commands

| Command | What it does | Docs |
|---------|-------------|------|
| `bobbin search <query>` | Hybrid semantic + keyword search | `docs/book/src/guides/searching.md` |
| `bobbin context <query>` | Budget-controlled context bundles | `docs/book/src/cli/context.md` |
| `bobbin grep <pattern>` | Regex/keyword search | `docs/book/src/cli/grep.md` |
| `bobbin index` | Build/update search index | `docs/book/src/cli/index.md` |
| `bobbin calibrate` | Auto-tune search params via git history | `docs/book/src/cli/calibrate.md` |
| `bobbin related <file>` | Files that change together (git coupling) | `docs/book/src/guides/git-coupling.md` |
| `bobbin refs <symbol>` | Symbol definitions and usages | `docs/book/src/cli/refs.md` |
| `bobbin hotspots` | High-churn, high-complexity files | `docs/book/src/cli/hotspots.md` |
| `bobbin impact <file>` | Predict affected files from a change | `docs/book/src/cli/impact.md` |
| `bobbin review <range>` | Diff-aware context for code review | `docs/book/src/cli/review.md` |
| `bobbin history <file>` | Commit history and churn stats | `docs/book/src/cli/history.md` |
| `bobbin serve` | HTTP API / MCP server | `docs/book/src/mcp/overview.md` |
| `bobbin hook` | Claude Code hook management | `docs/book/src/guides/hooks.md` |
| `bobbin tag` | Semantic tag management | `docs/book/src/guides/tags.md` |
| `bobbin connect <url>` | Connect to a remote server + install hooks | — |
| `bobbin status` | Index stats and calibration state | `docs/book/src/cli/status.md` |
| `bobbin bundle list` | List all context bundles (tree view) | `docs/designs/context-bundles.md` |
| `bobbin bundle show <name>` | Show bundle contents (L1 outline, L2 with --deep) | `docs/designs/context-bundles.md` |
| `bobbin bundle create <name>` | Create a new bundle | `docs/designs/context-bundles.md` |
| `bobbin bundle add <name>` | Add files/refs/keywords to a bundle | `docs/designs/context-bundles.md` |
| `bobbin bundle remove <name>` | Remove members or delete a bundle | `docs/designs/context-bundles.md` |

All commands support `--json`, `--quiet`, `--verbose`, and `--help`.

## Configuration

Layered: compiled defaults → global (`~/.config/bobbin/config.toml`) → per-repo (`.bobbin/config.toml`) → calibration (`.bobbin/calibration.json`) → CLI flags. Tables merge recursively; arrays and scalars replace.

→ Full reference: `docs/book/src/config/reference.md`

Key sections: `[index]` file patterns, `[search]` hybrid weights, `[embedding]` model settings, `[git]` coupling, `[hooks]` injection tuning, `[sources]` remote browse URLs, `[access]` role-based filtering.

## Search Modes

- **hybrid** (default): Combines semantic + keyword via RRF. Weight controlled by `search.semantic_weight` (0.0–1.0).
- **semantic**: Vector similarity only.
- **keyword**: Full-text search only.
- **regex**: Pattern matching (`bobbin grep`).

## Key Features

| Feature | Summary | Docs |
|---------|---------|------|
| Calibration | Grid sweep of search params against git history | `docs/book/src/cli/calibrate.md` |
| Tags & effects | Pattern-based chunk tagging with score boosts/demotions | `docs/book/src/guides/tags.md`, `docs/book/src/guides/tags-playbook.md` |
| Hooks | Auto-inject context into Claude Code prompts | `docs/book/src/guides/hooks.md`, `docs/book/src/config/hooks.md` |
| RBAC | Role-based repo/path access control | `docs/book/src/guides/access-control.md` |
| Multi-repo | Index multiple repos into one store | `docs/book/src/guides/multi-repo.md` |
| Reactions | Pattern-triggered hook actions | `.bobbin/reactions.toml` |
| Feedback | Agent ratings improve search quality | `docs/book/src/guides/feedback.md` |
| Archive | Index structured markdown records | `docs/book/src/guides/archive.md` |
| MCP server | Expose tools via Model Context Protocol | `docs/book/src/mcp/overview.md` |
| Forge URLs | Auto-detect GitHub/GitLab/Forgejo/Bitbucket for deep links | `[sources]` config |
| Bundles | Named knowledge anchors — curated file/symbol groups with progressive disclosure (L0→L1→L2) | `docs/designs/context-bundles.md` |

## Architecture

```
Repository → Tree-sitter / pulldown-cmark → ONNX Embedder → LanceDB + SQLite
             (structural parsing)           (384-dim)        (vectors + FTS + coupling)
```

Languages: Rust, TypeScript, Python, Go, Java, C++, Markdown. Others use line-based chunking.

## MCP Tools

When running as MCP server (`bobbin serve`):

| Tool | Use |
| --- | --- |
| `search` / `grep` | Semantic, keyword and regex search |
| `context` | Assemble a bounded task context |
| `related` / `impact` | Git coupling and predicted affected files |
| `test_coverage` | Find related tests |
| `find_refs` / `list_symbols` | Definitions, usages and symbols |
| `read_chunk` | Read one indexed chunk |
| `hotspots` / `review` | Churn and diff-aware review context |
| `similar` | Find similar code |
| `dependencies` / `file_history` | Imports and file history |
| `status` | Index state |
| `search_beads` / `commit_search` | Search indexed work and commits |
| `prime` | This shared agent primer and index statistics |

→ Full tool reference: `docs/book/src/mcp/tools.md`

## Quick Start

**Server mode** (connect to a running bobbin server):
```bash
bobbin connect http://search.example --global   # saves URL + installs hooks
bobbin search "error handling"
```

**Local mode** (standalone, no server):
```bash
bobbin init && bobbin index
bobbin search "error handling"
bobbin context "fix the login bug"
```

## Documentation Index

All docs live in `docs/book/`:

| Section | Path |
|---------|------|
| Guides | `docs/book/src/guides/` — searching, context, git coupling, multi-repo, tags, hooks |
| CLI reference | `docs/book/src/cli/` — all commands with options and examples |
| Configuration | `docs/book/src/config/` — full config.toml reference, section-by-section |
| Architecture | `docs/book/src/architecture/` — storage, embedding, language support |
| MCP | `docs/book/src/mcp/` — tools reference, client config, HTTP mode |

## Next steps

- Book: <https://intigraph.github.io/bobbin/>
- Skill: `skills/bobbin/SKILL.md`
- CLI help: `bobbin --help` and `bobbin help <command>`
