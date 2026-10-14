---
title: Tools Reference
description: Complete reference for all bobbin MCP tools
tags: [mcp, tools, reference]
status: draft
category: mcp
related: [mcp/overview.md, cli/search.md, cli/context.md]
---

# Tools Reference

All tools are available when bobbin runs as an MCP server (`bobbin serve`). Each tool accepts JSON parameters and returns JSON results.

## search

Search for code using natural language. Finds functions, classes, and other code elements that match the semantic meaning of your query.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `query` | string | yes | — | Natural language search query |
| `type` | string | no | all | Filter by chunk type: `function`, `method`, `class`, `struct`, `enum`, `interface`, `module`, `impl`, `trait` |
| `limit` | integer | no | 10 | Maximum number of results |
| `mode` | string | no | `hybrid` | Search mode: `hybrid`, `semantic`, or `keyword` |
| `repo` | string | no | all | Filter to a specific repository |

**Response fields:** `query`, `mode`, `count`, `results[]` (each with `id`, `file_path`, `name`, `chunk_type`, `start_line`, `end_line`, `score`, `match_type`, `language`, `content_preview`)

The `id` field is the chunk's stable identifier — pass it to `chunk_neighbors` to follow relationship edges from a result.

## grep

Search for code using exact keywords or regex patterns.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `pattern` | string | yes | — | Pattern to search for |
| `ignore_case` | boolean | no | false | Case-insensitive search |
| `regex` | boolean | no | false | Enable regex matching (post-filters FTS results) |
| `type` | string | no | all | Filter by chunk type |
| `limit` | integer | no | 10 | Maximum number of results |
| `repo` | string | no | all | Filter to a specific repository |

**Response fields:** `pattern`, `count`, `results[]` (each with `file_path`, `name`, `chunk_type`, `start_line`, `end_line`, `score`, `language`, `content_preview`, `matching_lines[]`)

## context

Assemble a comprehensive context bundle for a task. Combines semantic search results with temporally coupled files from git history.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `query` | string | yes | — | Natural language task description |
| `budget` | integer | no | 500 | Maximum lines of content |
| `depth` | integer | no | 1 | Coupling expansion depth (0 = no coupling) |
| `max_coupled` | integer | no | 3 | Max coupled files per seed file |
| `limit` | integer | no | 20 | Max initial search results |
| `coupling_threshold` | float | no | 0.1 | Minimum coupling score |
| `repo` | string | no | all | Filter to a specific repository |

**Response fields:** `query`, `budget` (`max_lines`, `used_lines`), `files[]` (each with `path`, `language`, `relevance`, `score`, `coupled_to[]`, `chunks[]`), `summary` (`total_files`, `total_chunks`, `direct_hits`, `coupled_additions`)

## related

Find files related to a given file based on git commit history (temporal coupling).

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | — | File path relative to repo root |
| `limit` | integer | no | 10 | Maximum number of results |
| `threshold` | float | no | 0.0 | Minimum coupling score (0.0–1.0) |

**Response fields:** `file`, `related[]` (each with `path`, `score`, `co_changes`)

## test_coverage

Map test↔source coverage inferred from git co-change history. Given a source
file, returns the test files that change with it (the tests that likely cover
it); given a test file, returns the source files it covers. Test files are
detected by path conventions (`test_*`, `*_test.*`, `*_spec.*`, `tests/`, etc.).

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | — | File path relative to repo root |
| `limit` | integer | no | 10 | Maximum number of results |
| `threshold` | float | no | 0.0 | Minimum coupling score (0.0–1.0) |

**Response fields:** `file`, `link_kind` (`"test"` when `file` is source,
`"source"` when `file` is a test), `links[]` (each with `path`, `score`,
`co_changes`)

## find_refs

Find the definition and all usages of a symbol by name.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `symbol` | string | yes | — | Exact symbol name (e.g., `parse_config`) |
| `type` | string | no | all | Filter by symbol type |
| `limit` | integer | no | 20 | Maximum number of usage results |
| `repo` | string | no | all | Filter to a specific repository |

**Response fields:** `symbol`, `definition` (`name`, `chunk_type`, `file_path`, `start_line`, `end_line`, `signature`), `usage_count`, `usages[]` (each with `file_path`, `line`, `context`)

## list_symbols

List all symbols (functions, structs, traits, etc.) defined in a file.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | — | File path relative to repo root |
| `repo` | string | no | all | Filter to a specific repository |

**Response fields:** `file`, `count`, `symbols[]` (each with `name`, `chunk_type`, `start_line`, `end_line`, `signature`)

## chunk_neighbors

Follow relationship edges from a chunk. Deterministic structural edges (`next_chunk`, `part_of`) are emitted for every indexed file; AST edges (`implements`, `impl_for`, `extends`) come from tree-sitter; `similar_to` edges are near-duplicate pairs persisted opt-in by `bobbin similar --scan --persist`.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `id` | string | no* | — | Chunk ID from search results (preferred anchor) |
| `file` | string | no* | — | File path relative to repo root, used with `line` |
| `line` | integer | no* | — | Line within `file`; smallest containing chunk becomes the anchor |
| `repo` | string | no | all | Disambiguates `file` in multi-repo stores |
| `edge_type` | string | no | all | Filter: `next_chunk`, `part_of`, `implements`, `impl_for`, `extends`, `tests`, `similar_to` |
| `direction` | string | no | `both` | `out` (anchor is edge source), `in` (anchor is target) |
| `limit` | integer | no | 20 | Maximum neighbors returned |

*Provide either `id`, or both `file` and `line`.

Direction semantics: for `next_chunk`, `out` is the following chunk and `in` the preceding one; for `part_of`, `out` is the containing parent and `in` lists children.

**Response fields:** `chunk` (resolved anchor: `id`, `file_path`, `name`, `chunk_type`, `start_line`, `end_line`), `count`, `neighbors[]` (each with `edge_type`, `direction`, the same chunk fields, `content_preview`), `dangling[]` (edge endpoints whose chunk ID no longer resolves — present only after edits shifted line ranges; re-index the file to refresh)

## read_chunk

Read a specific section of code from a file by line range.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | — | File path relative to repo root |
| `start_line` | integer | yes | — | Starting line number |
| `end_line` | integer | yes | — | Ending line number |
| `context` | integer | no | 0 | Context lines to include before and after |

**Response fields:** `file`, `start_line`, `end_line`, `actual_start_line`, `actual_end_line`, `content`, `language`

## hotspots

Identify code hotspots — files with both high churn and high complexity.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `since` | string | no | `1 year ago` | Time window (e.g., `6 months ago`, `3 months ago`) |
| `limit` | integer | no | 20 | Maximum number of hotspots |
| `threshold` | float | no | 0.0 | Minimum hotspot score (0.0–1.0) |

**Response fields:** `count`, `since`, `hotspots[]` (each with `file`, `score`, `churn`, `complexity`, `language`)

## prime

Get an LLM-friendly overview of the bobbin project with live index statistics.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `section` | string | no | all | Specific section: `what bobbin does`, `architecture`, `supported languages`, `key commands`, `mcp tools`, `quick start`, `configuration` |
| `brief` | boolean | no | false | Compact overview (title and first section only) |

**Response fields:** `primer` (markdown text), `section`, `initialized`, `stats` (`total_files`, `total_chunks`, `total_embeddings`, `languages[]`, `last_indexed`)

## impact

Predict which files are affected by a change to a target file or function. Combines git co-change coupling and semantic similarity signals.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `target` | string | yes | — | File path or `file:symbol` reference |
| `depth` | integer | no | 1 | Transitive expansion depth (0–3) |
| `mode` | string | no | `combined` | Signal mode: `combined`, `coupling`, `semantic`, `deps` |
| `threshold` | float | no | 0.1 | Minimum impact score |
| `limit` | integer | no | 20 | Maximum number of results |

## review

Assemble review context from a git diff. Finds indexed chunks overlapping changed lines and expands via temporal coupling.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `diff` | string | no | unstaged | Diff spec: `unstaged`, `staged`, `branch:<name>`, `commit:<range>` |
| `budget` | integer | no | 500 | Maximum lines of context |
| `depth` | integer | no | 1 | Coupling expansion depth |

## similar

Find code chunks semantically similar to a target, or scan for duplicate clusters.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `target` | string | no | — | Chunk reference (`file.rs:function_name`) or free text |
| `scan` | boolean | no | false | Scan entire codebase for near-duplicate clusters |
| `threshold` | float | no | 0.85 | Minimum similarity score |
| `limit` | integer | no | 10 | Maximum results |
| `cross_repo` | boolean | no | false | Include cross-repo matches |

## search_beads

Search for beads (issues/tasks) using natural language. Requires beads to be indexed via `bobbin index --include-beads`.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `query` | string | yes | — | Natural language query |
| `priority` | integer | no | all | Filter by priority (1–4) |
| `status` | string | no | all | Filter by status |
| `assignee` | string | no | all | Filter by assignee |
| `limit` | integer | no | 10 | Maximum results |
| `enrich` | boolean | no | true | Reread configured-source metadata; JSONL is an exported snapshot, not live tracker state |

Every response includes `warnings`, including compact and empty responses.
Every result includes `metadata_provenance`: `source`, `as_of`, `observed_at`,
`export_file_modified_at`, `current_board_verified` and `warning`.
`jsonl_snapshot` means an exported file was reread; it can still be retained
from a retired tracker. `indexed_content` means no matching source metadata was
available or enrichment was disabled. `configured_database` means that database
was queried, which does not establish that it remains the active board.
Older remote servers are marked `unreported` instead of silently claiming live
enrichment.

`as_of` is currently null: the sources do not provide a verified snapshot
timestamp. `observed_at` records Bobbin's observation, while file modification
time can change during a checkout without the underlying board advancing.
Neither proves freshness. Verify status, priority and assignee with the active
tracker before routing work. Discovery still uses the last indexed corpus;
new beads can be absent and filters can exclude changed beads, so zero results
are not proof of absence from the active board. This reports provenance; it
does not migrate a tracker or repair an out-of-date index.

## dependencies

Show import dependencies for a file. Returns forward and/or reverse dependencies.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | — | File path relative to repo root |
| `reverse` | boolean | no | false | Show reverse dependencies (what imports this file) |
| `both` | boolean | no | false | Show both forward and reverse |
| `repo` | string | no | all | Filter to a specific repository |

## file_history

Show git commit history for a specific file, with author breakdown and churn rate.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | — | File path relative to repo root |
| `limit` | integer | no | 20 | Maximum commits to return |

## status

Show current index status and statistics.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `languages` | boolean | no | false | Include per-language breakdown |

## commit_search

Search git commit history using natural language.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `query` | string | yes | — | Natural language query |
| `author` | string | no | all | Filter by author |
| `file` | string | no | all | Filter by file path |
| `limit` | integer | no | 10 | Maximum results |

## feedback_submit

Submit feedback on a bobbin context injection. Rate injections as useful, noise, or harmful.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `injection_id` | string | yes | — | Injection ID from `[injection_id: inj-xxx]` |
| `rating` | string | yes | — | `useful`, `noise`, or `harmful` |
| `agent` | string | no | auto | Agent identity (auto-detected from env) |
| `reason` | string | no | — | Explanation (max 1000 chars) |

## feedback_list

List recent feedback records with optional filters.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `rating` | string | no | all | Filter by rating |
| `agent` | string | no | all | Filter by agent |
| `limit` | integer | no | 20 | Maximum results (max 50) |

## feedback_stats

Get aggregated feedback statistics — total injections, coverage rate, rating breakdown, and lineage counts.

**Parameters:** None.

## feedback_lineage_store

Record a lineage action that ties feedback to a concrete fix. Links feedback records to commits, beads, or config changes.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `feedback_ids` | integer[] | yes | — | Feedback record IDs to link |
| `action_type` | string | yes | — | `code_fix`, `config_change`, `tag_effect`, `access_rule`, or `exclusion_rule` |
| `bead` | string | no | — | Associated bead ID |
| `commit_hash` | string | no | — | Git commit hash |
| `description` | string | yes | — | What was done |
| `agent` | string | no | auto | Agent identity |

## feedback_lineage_list

List lineage records showing how feedback was acted on.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `feedback_id` | integer | no | all | Filter by feedback ID |
| `bead` | string | no | all | Filter by bead ID |
| `commit_hash` | string | no | all | Filter by commit hash |
| `limit` | integer | no | 20 | Maximum results (max 50) |

## archive_search

Search archive records (HLA chat logs, Pensieve agent memory) using natural language.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `query` | string | yes | — | Natural language query |
| `source` | string | no | all | Filter: `hla` or `pensieve` |
| `filter` | string | no | all | Filter by name/channel |
| `after` | string | no | — | Only records after date (YYYY-MM-DD) |
| `before` | string | no | — | Only records before date (YYYY-MM-DD) |
| `limit` | integer | no | 10 | Maximum results |
| `mode` | string | no | `hybrid` | `hybrid`, `semantic`, or `keyword` |

## archive_recent

List recent archive records by date.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `after` | string | yes | — | Only records after date (YYYY-MM-DD) |
| `source` | string | no | all | Filter: `hla` or `pensieve` |
| `limit` | integer | no | 20 | Maximum results |

## knowledge_context

Find entities and facts relevant to a topic across both knowledge graphs a deployment has, using text search with link expansion. The response has two clearly separated sections: `ontology` (the organization knowledge graph on a remote [Quipu](https://github.com/scbrown/quipu) — infrastructure, ownership, and operational facts) and `local_code_graph` (bobbin's own embedded graph of code entities and file-coupling from git history). Best for questions like "what services run on node-4?" or "which files change together with X?".

Always read the `store` field in the result: `ontology.consulted = false` means the remote was not asked (no remote configured), and an `error` there is a transport failure — neither is evidence a fact is absent.

> Requires bobbin built with the `knowledge` feature (`cargo build --features knowledge`). Without it the tool is registered but returns an error directing you to rebuild.

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `query` | string | yes | — | Natural language query describing the entities you need |
| `max_entities` | integer | no | 20 | Maximum entities to return |
| `expand_links` | boolean | no | true | Expand results by following graph links from direct hits |

## knowledge_query

Execute a SPARQL SELECT query against both knowledge graphs, with optional temporal filtering. The same query runs against the remote ontology Quipu and bobbin's embedded code graph, and each returns its rows in a separate section — an IRI that exists in only one graph returns rows in only that section. Best for precise structured queries when you know the entity IRIs or predicates.

As with `knowledge_context`, read the `store` field: an empty section is never by itself evidence the fact does not exist.

> Requires the `knowledge` build feature (see `knowledge_context`).

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `query` | string | yes | — | SPARQL SELECT query |
| `valid_at` | string | no | — | ISO-8601 timestamp for a temporal query (what was true then?) |
| `tx` | integer | no | — | Transaction ID for a point-in-time (as-of) query |

## knowledge_knot

Write facts into bobbin's local embedded knowledge graph as RDF Turtle. This writes only to the local graph — never to the remote ontology Quipu, which is read-only from here.

Always read the `shacl_validated` field in the result. When `true`, the write was checked against the configured SHACL shapes before being committed, and a violating write would have been refused (a SHACL refusal surfaces as a tool error, not as a success with a flag). When `false`, validation was not compiled in and the facts were stored unchecked — a success then means only that the write was accepted, not that it is conformant.

> Requires the `knowledge` build feature (see `knowledge_context`).

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `turtle` | string | yes | — | Facts to write, as RDF Turtle |
| `actor` | string | no | — | Actor recorded as the author of this write (provenance). Pass it whenever you have one |
| `source` | string | no | — | Where the facts came from (e.g. a file path or tool name) |
| `shapes` | string | no | — | SHACL shapes (Turtle) to validate against instead of the store's configured shapes |

**Response fields:** `written` (the store's write result), `shacl_validated`, `store`

## knowledge_reconcile_mentions

Run the idempotent chunk→entity mention reconcile pass over bobbin's local knowledge graph. It resolves weak `bobbin:mentions "SymbolName"` literals on chunk facts into typed reference edges against the live entity graph, and reports every mention as one of three outcomes: **resolved** (exactly one match — edge written), **dangling** (no match — literal left in place for a later run), or **ambiguous** (multiple matches — left unresolved, never guessed).

Safe to re-run: an unchanged store yields the identical classification with `edges_written = 0`. Run it after new entities land in the graph to pick up previously dangling mentions. The same pass runs automatically after each chunk-snapshot push during `bobbin index` (see [Quipu Integration](../guides/quipu-integration.md)).

> Requires the `knowledge` build feature (see `knowledge_context`).

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `max_details` | integer | no | 50 | Maximum per-mention detail entries to return. The three counts are always complete |

**Response fields:** `resolved`, `dangling`, `ambiguous`, `edges_written`, `details[]`, `details_truncated`, `store`

## knowledge_inferred_extract

Extract candidate entities and relationships from markdown prose via bobbin's inferred-track extractor seam. The one extractor today is the deterministic backtick-coderef heuristic — not a language model, and honestly labeled as such in every fact it produces.

Everything returned is a claim at quarantined standing, never an observation. The response envelope carries the quarantine plane, trust rank 0, and `sourceKind = inferred` — inferred facts are never served bare. With `push = true` the stamped facts also land in the quarantined plane via a graph-routed `/knot` write, each fact carrying its extractor and parameters as the derivation method; the write refuses if the embedded store cannot enforce graph routing (the facts would masquerade in the default graph at observed standing) or the plane is unregistered. Promotion out of quarantine is the governing ontology's authority-gated flow, never this tool's.

> Requires the `knowledge` build feature (see `knowledge_context`).

**Parameters:**

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `text` | string | yes | — | Markdown prose to run the extractor over |
| `repo` | string | no | `adhoc` | Repository name the prose belongs to, for IRI minting |
| `file_path` | string | no | `adhoc.md` | File path the prose came from, for the source-chunk IRI |
| `push` | boolean | no | false | Also land the stamped facts in the quarantined plane (default is preview only) |

**Response fields:** `extractor` (`id`, `params`), `entities`, `relations`, `quarantine` (`graph`, `snapshot`, `turtle`), `pushed` (`tx_id`, `count` — only when `push = true`), all wrapped in the quarantine envelope
