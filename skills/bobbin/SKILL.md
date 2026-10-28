---
name: bobbin
description: >
  Semantic code search and codebase intelligence. Use this skill when asked to
  "search code", "find where X is defined", "what files are related to Y",
  "show me the impact of changing Z", "find duplicates", "search commits",
  "search beads/issues", "/bobbin <query>", or any codebase exploration that
  goes beyond simple grep. Routes queries to the right bobbin MCP tool.
---

# Bobbin Skill — Semantic Code Search

You are performing a codebase search using bobbin's semantic search engine.
Bobbin indexes code across all repos in the workspace and provides tools far
more powerful than grep: semantic search, symbol references, impact analysis,
commit history, coupling graphs, and more.

## Input

The user provided a query: `{{args}}`

## Routing Decision

Analyze the query and pick the RIGHT tool. Don't default to `search` for
everything — use the most specific tool available.

### Route 1: Symbol Lookup
**Trigger**: Query names a specific function, struct, class, or variable.
Examples: "find parse_config", "where is Config defined", "who calls handle_request"

**Action**: Use `bobbin find_refs` with the symbol name.
- Returns: definition location + all usage sites
- Follow up with `bobbin read_chunk` to show the actual code

### Route 2: Semantic Code Search
**Trigger**: Query describes code by what it does, not by exact name.
Examples: "authentication logic", "error handling with retries", "database connection pooling"

**Action**: Use `bobbin search` with the natural language query.
- Use `mode: "hybrid"` (default) for best results
- Use `type` filter if the user wants a specific kind (function, struct, etc.)
- Use `repo` filter if the user specifies a repo
- Use `bundle` filter if the user specifies a bundle scope

### Route 3: Exact Pattern Search
**Trigger**: Query is a literal string, regex, or exact identifier.
Examples: "TODO FIXME", "fn.*async", grep-style patterns

**Action**: Use `bobbin grep` with the pattern.
- Set `regex: true` if the pattern uses regex syntax
- Set `ignore_case: true` for case-insensitive searches

### Route 4: File Relationships
**Trigger**: Query asks what files are related, coupled, or co-changed.
Examples: "what changes with config.rs", "files related to auth"

**Action**: Use `bobbin related` with the file path.
- Follow up with `bobbin read_chunk` on interesting results

### Route 5: Impact Analysis
**Trigger**: Query asks what would break or be affected by a change.
Examples: "impact of changing auth.rs", "what breaks if I modify the parser"

**Action**: Use `bobbin impact` with the target file or file:function.
- Use `depth: 2` for transitive impact

### Route 6: Commit Search
**Trigger**: Query asks about git history semantically.
Examples: "when was auth added", "commits that changed error handling", "who refactored X"

**Action**: Use `bobbin commit_search` with the query.
- Use `author` filter if specified
- Use `file` filter if specified

### Route 7: Bead/Issue Search
**Trigger**: Query asks about issues, bugs, tasks, or work items.
Examples: "open auth bugs", "P1 issues", "beads about monitoring"

**Action**: Use `bobbin search_beads` with the query.
- Use priority/status/assignee/label filters as appropriate

### Route 8: Context Assembly
**Trigger**: Query asks for comprehensive context for a task or change.
Examples: "context for refactoring auth", "everything related to the deploy pipeline"

**Action**: Use `bobbin context` with the task description.
- Adjust `budget` based on scope (default 500 lines)

### Route 9: File Symbols
**Trigger**: Query asks what's defined in a specific file.
Examples: "symbols in main.rs", "what functions are in config.py", "API of auth module"

**Action**: Use `bobbin list_symbols` with the file path.

### Route 10: Duplicate Detection
**Trigger**: Query asks about duplicate or similar code.
Examples: "find code similar to X", "detect duplicates", "near-duplicate functions"

**Action**: Use `bobbin similar`.
- Single target: provide `target` as `file:function` or free text
- Scan mode: set `scan: true` to find clusters across the codebase

### Route 11: Dependency Analysis
**Trigger**: Query asks about imports, dependencies, or what depends on a file.
Examples: "what imports config.rs", "dependencies of auth module", "reverse deps"

**Action**: Use `bobbin dependencies` with the file path.
- Use `reverse: true` for "what depends on this"
- Use `both: true` for full picture

### Route 12: File History
**Trigger**: Query asks about the history of a specific file.
Examples: "history of config.rs", "who changed auth.py recently", "churn on main.rs"

**Action**: Use `bobbin file_history` with the file path.

### Route 13: Code Hotspots
**Trigger**: Query asks about risky, complex, or high-churn code.
Examples: "hotspots", "what needs refactoring", "riskiest files"

**Action**: Use `bobbin hotspots`.
- Adjust `since` for time window
- Adjust `threshold` to filter noise

### Route 14: Archive Search
**Trigger**: Query asks about chat logs, IRC discussions, or agent memory.
Examples: "what did we discuss about auth", "recent deploy discussions", "telegram messages"

**Action**: Use `bobbin archive_search` with the query.
- Use `source: "hla"` for chat logs, `source: "pensieve"` for agent memory
- Use date filters (`after`, `before`) as appropriate

### Route 15: Review Context
**Trigger**: Query asks for review context on current changes or a diff.
Examples: "review context for my changes", "what do I need to review this PR"

**Action**: Use `bobbin review`.
- Use `diff: "staged"` or `diff: "unstaged"` for working tree
- Use `diff: "branch:feature-x"` for branch comparison

## Output Guidelines

1. **Show the most relevant results first** — don't dump everything
2. **Include file paths and line numbers** so the user can navigate
3. **Read and show actual code** for the top 2-3 results using `read_chunk`
4. **Suggest follow-up queries** if the results are ambiguous or partial
5. **Chain tools** when appropriate — e.g., `search` -> `find_refs` -> `read_chunk`
6. **Be concise** — summarize what you found, don't just paste raw output

## Agent primer

<!-- BEGIN bobbin prime -->
### Bobbin agent primer

#### What Bobbin is

Bobbin is a local-first code context engine for agents.
It combines semantic search, keyword search, git coupling and context assembly.
Indexes live in LanceDB and SQLite alongside the repository.
Search and context work locally without API keys.
Quipu supplies shared knowledge; Yupana supplies structural change analysis.

#### Agent workflow

1. Search before scanning files: `bobbin search "<intent>"`.
2. Read a returned chunk or assemble context with `bobbin context "<task>"`.
3. Ask `bobbin impact <file>` for related files; use Yupana for structural callers.
4. Check `bobbin status` before trusting an empty result; reindex with `bobbin index`.
5. Treat an old index or a failed read as unknown, not as proof of absence.

#### Core commands

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

#### Configuration

Layered: compiled defaults → global (`~/.config/bobbin/config.toml`) → per-repo (`.bobbin/config.toml`) → calibration (`.bobbin/calibration.json`) → CLI flags. Tables merge recursively; arrays and scalars replace.

→ Full reference: `docs/book/src/config/reference.md`

Key sections: `[index]` file patterns, `[search]` hybrid weights, `[embedding]` model settings, `[git]` coupling, `[hooks]` injection tuning, `[sources]` remote browse URLs, `[access]` role-based filtering.

#### Search Modes

- **hybrid** (default): Combines semantic + keyword via RRF. Weight controlled by `search.semantic_weight` (0.0–1.0).
- **semantic**: Vector similarity only.
- **keyword**: Full-text search only.
- **regex**: Pattern matching (`bobbin grep`).

#### Key Features

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

#### Architecture

```
Repository → Tree-sitter / pulldown-cmark → ONNX Embedder → LanceDB + SQLite
             (structural parsing)           (384-dim)        (vectors + FTS + coupling)
```

Languages: Rust, TypeScript, Python, Go, Java, C++, Markdown. Others use line-based chunking.

#### MCP Tools

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

#### Quick Start

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

#### Documentation Index

All docs live in `docs/book/`:

| Section | Path |
|---------|------|
| Guides | `docs/book/src/guides/` — searching, context, git coupling, multi-repo, tags, hooks |
| CLI reference | `docs/book/src/cli/` — all commands with options and examples |
| Configuration | `docs/book/src/config/` — full config.toml reference, section-by-section |
| Architecture | `docs/book/src/architecture/` — storage, embedding, language support |
| MCP | `docs/book/src/mcp/` — tools reference, client config, HTTP mode |

#### Next steps

- Book: <https://intigraph.github.io/bobbin/>
- Skill: `skills/bobbin/SKILL.md`
- CLI help: `bobbin --help` and `bobbin help <command>`
<!-- END bobbin prime -->
