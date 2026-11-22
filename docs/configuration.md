# Configuration

Bobbin stores its configuration in `.bobbin/config.toml`, created by `bobbin init`.

## Full Default Configuration

```toml
# Quipu knowledge-graph endpoint (e.g. "http://quipu.example"). When set, search
# results are annotated with entity spotlight data. Unset by default.
# Top-level key — must appear before the first [section] header.
# quipu_endpoint = "http://quipu.example"

# Publish each index run's governed code-entity graph as a replaceable
# snapshot. Uses quipu_endpoint when configured, otherwise the embedded store.
quipu_push_chunks = false

[server]
# All three keys are unset by default (thin-client / serve options).

# Remote bobbin HTTP server URL for thin-client mode. When set, this machine
# queries the remote server instead of using a local index.
# url = "http://search.example"

# Bind address for `bobbin serve --http`. Runtime fallback when unset: "0.0.0.0".
# bind_address = "127.0.0.1"

# Filesystem prefix for indexed repos on the server, used to normalize absolute
# result paths back to repo-relative. Returned as-is when unset.
# repo_path_prefix = "/var/lib/bobbin/repos/"

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
    "**/CONTRIBUTING.md",
    "**/contributing.md",
    "**/searchindex*.js",
    "**/*.min.js",
    "**/*.min.css",
    "**/.scratch/**",
    "**/vendor/**",
    "**/.venv/**",
    "**/book/book/**",
]

# Whether to respect .gitignore files
use_gitignore = true

# Line-based chunker (unknown languages): lines per chunk and overlap.
# Chunks are also clamped to the embedding model's token window so dense
# chunks aren't silently truncated at embed time.
chunk_size = 50
chunk_overlap = 10

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

[hooks]
# Per-result filter on normalized RRF scores
threshold = 0.5

# Max lines of injected context
budget = 300

# Content display mode: full | preview | none
content_mode = "full"

# Skip injection for prompts shorter than this
min_prompt_length = 20

# Min raw semantic similarity to inject at all.
# The top semantic result's cosine similarity (before RRF normalization)
# must exceed this value, or the entire injection is skipped.
gate_threshold = 0.45

# Skip injection when search results haven't changed since last prompt.
# Uses a session ID derived from the top-10 chunk keys.
dedup_enabled = true

[git]
# Enable temporal coupling analysis (tracks which files
# frequently change together in git history)
coupling_enabled = true

# Number of commits to analyze for coupling relationships
coupling_depth = 5000

# Minimum number of co-changes required to establish a coupling link
coupling_threshold = 3

[context]
# Score multiplier for bridge-boosted files: final_score *= (1.0 + factor).
# Only used in boost / boost_inject bridge modes.
bridge_boost_factor = 0.3

# Max bridged files to fetch chunks for (prevents doc->source bridge explosion)
max_bridged_files = 2

# Max chunks kept per bridged file (first N by start line)
max_bridged_chunks_per_file = 1

# Minimum temporal-coupling SCORE (float 0.0-1.0) for a coupled file to enter
# context. Distinct from [git].coupling_threshold (an integer co-change COUNT).
coupling_threshold = 0.1

# Percent of the line budget reserved for knowledge-graph expansion
# (requires the `knowledge` feature build)
knowledge_budget_pct = 15.0
neighbor_budget_pct = 10.0

# Max graph-traversal hops for knowledge expansion
knowledge_max_hops = 2

# Unit the context budget is enforced in: "line" (count source lines) or
# "token" (estimate tokens per chunk, ~chars/4). Token mode makes injection
# size predictable against the model window. The budget value (e.g. the hook
# `budget`) is then interpreted in this unit.
budget_unit = "line"

[feedback]
# Maximum feedback boost multiplier. Actual boost = min(score * boost_weight, boost_max).
boost_max = 0.3

# Weight multiplier applied to raw cross-agent feedback scores
boost_weight = 0.2

[dependencies]
# Enable dependency extraction and storage
enabled = true

# Enable import path resolution
resolve_imports = true

[beads]
# Index beads (issue tracker) content
enabled = false

# Which store to read beads from: "dolt" (MySQL protocol) or "jsonl" (a br
# store's exported .beads/issues.jsonl). Defaults to "dolt".
source = "dolt"

# --- source = "dolt" ---
# Dolt server hostname
host = "dolt.example"

# Dolt server port
port = 3306

# Dolt user
user = "root"

# Database names to index (e.g. ["beads_aegis", "beads_gastown"]). None by default.
# This is the RIG IDENTITY for both sources, not a Dolt-only field: chunk keys
# are `beads:<rig>:<id>` with the rig derived from these names, so the list must
# stay the same across a source switch or the whole corpus re-keys.
databases = []

# --- source = "jsonl" ---
# One path per configured database. Every entry in `databases` needs one;
# a configured database with no path is an ERROR, not an empty rig.
# [beads.jsonl_paths]
# beads_aegis = "/var/lib/bobbin/repos/aegis/.beads/issues.jsonl"

# Include bead comments in indexed content
include_comments = true

# Include closed beads in the index
include_closed = false

# Skip beads older than this many days (0 = no limit)
max_age_days = 90

# Exclude beads carrying any of these labels (case-insensitive) — keeps sensitive
# beads out of the index entirely (e.g. ["security", "escalation"])
exclude_labels = []

[sql]
# Index rows from MySQL-protocol databases as searchable chunks
enabled = false

# SQL sources to index. None by default; uncomment to add:
# [[sql.sources]]
# # Label — rows index under repo key "sql-{name}" and paths "sql:{name}:{id}"
# name = "tickets"
# # Environment variable holding the mysql://user:pass@host:port/db URL.
# # Credentials never live in this file.
# url_env = "BOBBIN_SQL_TICKETS_URL"
# # Query producing the rows to index; must include id_column
# query = "SELECT id, title, body, status FROM tickets"
# id_column = "id"
# # Columns embedded as content (empty = all) and rendered as tags
# text_columns = ["title", "body"]
# tag_columns = ["status"]

[archive]
# Index archive sources (directories of markdown files with YAML frontmatter)
enabled = false

# HMAC-SHA256 secret for push notifications. Empty = reject all webhooks.
webhook_secret = ""

# Archive sources to index. None by default; uncomment to add:
# [[archive.sources]]
# # Label — used as the chunk language tag, path prefix, and search `source` filter
# name = "hla"
# # Filesystem path to the records directory
# path = "/mnt/hla/records"
# # String matched in YAML frontmatter to identify records
# # (e.g. "human-intent" matches `schema: human-intent/v2`)
# schema = "human-intent"
# # Frontmatter field used as the chunk-name prefix (empty = just the record id)
# name_field = "channel"

[access]
# When true, repos not named in any role rule are visible to ALL roles.
# When false, repos must be explicitly granted by a role's allow list.
default_allow = true

# Role-based access rules. None by default; deny beats allow. Uncomment to restrict:
# [[access.roles]]
# # Role name or glob (e.g. "human", "aegis/*", "bobbin/polecats/*")
# name = "bobbin/polecats/*"
# # Repos this role CAN see (globs; ["*"] = all)
# allow = ["bobbin", "aegis"]
# # Repos this role CANNOT see (globs; deny beats allow)
# deny = ["secrets"]
# # File-path patterns denied within allowed repos (globs)
# deny_paths = ["**/*.env", "**/secrets/**"]

[sources]
# Template applied to ALL auto-detected git remotes. Empty (default) =
# auto-detect forge type (GitHub/GitLab/Forgejo/Bitbucket). Placeholders:
# {remote_base}, {path}, {line}
remote_template = ""

# Fallback URL template for repos with no git remote and no per-repo override.
# Placeholders: {repo}, {path}, {line}
default_url = ""

# Per-repo URL overrides (highest priority). Empty by default.
# [sources.repos]
# beads = "https://github.com/scbrown/beads/blob/main/{path}#L{line}"

# Override forge detection per host. Empty by default.
# [sources.forge_overrides]
# "git.internal.example.com" = "gitlab"

# Named repo groups for scoped search (--group CLI flag / ?group= HTTP param).
# None by default; groups compose with role-based access filtering.
# [[groups]]
# name = "infra"
# repos = ["goldblum", "homelab-mcp", "aegis"]

# Custom file-type classification rules. None by default; evaluated in order,
# first match wins. Unmatched files fall back to the built-in classifier.
# Built-in categories: "source", "test", "documentation", "config"; custom
# names (e.g. "generated") are stored/displayed as-is.
# [[file_types]]
# name = "generated"
# patterns = ["*.pb.go", "*.generated.ts", "migrations/*.sql"]
```

## Section Reference

### `[index]`

Controls which files are indexed.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `include` | string[] | See above | Glob patterns for files to include |
| `exclude` | string[] | See above | Additional exclusion patterns (on top of `.gitignore`) |
| `use_gitignore` | bool | `true` | Whether to respect `.gitignore` files |
| `chunk_size` | int | `50` | Lines per chunk for the line-based (unknown-language) chunker. |
| `entities` | bool | `false` | Derive semantic entities (CodeModule/CodeSymbol/Document/Section, live-lane IRIs) from parsed chunks into the local entities table on each index run. Adds a second, small embed pass. |
| `chunk_overlap` | int | `10` | Overlapping lines between consecutive line-based chunks. Capped below `chunk_size`. |

Line-based chunks are additionally clamped to the embedding model's token window (`max_seq`) so a dense chunk never silently overflows and gets truncated at embed time — chunks are split to fit. The default model (`all-MiniLM-L6-v2`) has a 256-token window; `bge-small-en-v1.5` and `gte-small` allow 512.

### `[embedding]`

Controls embedding model and batch processing.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `model` | string | `"all-MiniLM-L6-v2"` | ONNX embedding model name. Downloaded to `~/.cache/bobbin/models/` on first use. |
| `batch_size` | int | `32` | Number of chunks to embed per batch |

### `[embedding.context]`

Controls contextual embedding, where chunks are embedded with surrounding source lines for better retrieval.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `context_lines` | int | `5` | Lines of context before and after each chunk |
| `enabled_languages` | string[] | `["markdown"]` | Languages where contextual embedding is active |

### `[search]`

Controls search behavior defaults.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `default_limit` | int | `10` | Default number of results returned |
| `semantic_weight` | float | `0.9` | Balance between semantic (1.0) and keyword (0.0) in hybrid mode |

### `[hooks]`

Controls Claude Code hook behavior for automatic context injection.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `threshold` | float | `0.5` | Per-result filter on normalized RRF scores |
| `budget` | int | `300` | Maximum lines of injected context |
| `content_mode` | string | `"full"` | Content display mode: `full`, `preview`, or `none` |
| `min_prompt_length` | int | `20` | Skip injection for prompts shorter than this |
| `gate_threshold` | float | `0.45` | Minimum raw semantic similarity (cosine, before RRF) to inject at all. If the top result's score is below this, the entire injection is skipped. |
| `dedup_enabled` | bool | `true` | Skip injection when search results match the previous prompt's session ID (same top-10 chunks = same session) |

### `[git]`

Controls temporal coupling analysis from git history.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `coupling_enabled` | bool | `true` | Enable temporal coupling analysis |
| `coupling_depth` | int | `5000` | How many commits back to analyze |
| `coupling_threshold` | int | `3` | Minimum co-changes to establish a coupling relationship |

### `[context]`

Tunes context assembly — the bridging + knowledge-expansion pipeline that is Bobbin's core differentiator. These knobs were previously hardcoded (and inconsistent) at each call site; surfacing them here lets you tune behavior without recompiling. Defaults match the tuned production values used by the hook and HTTP injection paths. CLI/HTTP request parameters still override these per-call.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `bridge_boost_factor` | float | `0.3` | Score multiplier for bridge-boosted files: `final_score *= (1.0 + factor)`. Only used in `boost`/`boost_inject` bridge modes. |
| `max_bridged_files` | int | `2` | Max bridged files to fetch chunks for. Prevents bridge explosion from doc→source blame chains. |
| `max_bridged_chunks_per_file` | int | `1` | Max chunks kept per bridged file (first N by start line). |
| `coupling_threshold` | float | `0.1` | Minimum coupling **score** (0.0–1.0) for a coupled file to enter context. Distinct from `[git].coupling_threshold`, which is an integer co-change **count** applied during indexing. |
| `knowledge_budget_pct` | float | `15.0` | Percent of the line budget reserved for knowledge-graph expansion (requires the `knowledge` feature). |
| `neighbor_budget_pct` | float | `10.0` | Percent of the line budget reserved for structural neighbors of documentation hits (parent section + adjacent chunks via chunk edges). `0` disables the leg. |
| `knowledge_max_hops` | int | `2` | Max graph-traversal hops for knowledge expansion. |
| `budget_unit` | string | `"line"` | Unit the context budget is enforced in: `"line"` (count source lines) or `"token"` (estimate tokens per chunk, ~chars/4). Token mode makes injection size predictable against the model window; the budget value is then interpreted in tokens. |

### `[feedback]`

Tunes cross-agent feedback boosting. Files rated "useful" by other agents for similar queries get a bounded score boost during context assembly.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `boost_max` | float | `0.3` | Maximum feedback boost multiplier. Actual boost is `min(score * boost_weight, boost_max)`. |
| `boost_weight` | float | `0.2` | Weight multiplier applied to raw feedback scores. |

### `[server]`

Thin-client and HTTP-server options. All keys are unset by default.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `url` | string | _(unset)_ | Remote bobbin server URL. When set, this machine queries the remote server instead of a local index. |
| `bind_address` | string | _(unset → `"0.0.0.0"`)_ | Bind address for `bobbin serve --http`. Runtime fallback is `0.0.0.0` when unset. |
| `repo_path_prefix` | string | _(unset)_ | Filesystem prefix used to normalize absolute result paths back to repo-relative on the server. |

For a local server sharing its index with a scheduled indexer, set
`BOBBIN_INDEX_WRITER_LOCK` to the wrapper's existing regular lock file. Stats
reads then try that exclusive advisory lock before opportunistic compaction.
An active writer, missing file, or unsafe path defers compaction while stats
remain available. The reader never creates or truncates the lock. Every writer
must use the same file; this setting cannot serialize writers that ignore it.

Leave the variable unset for the existing standalone behavior. Set it only on
readers: an index wrapper that already holds the lock must keep using explicit
scheduled maintenance, which does not acquire the outer lock again. Deploy the
binary supporting this variable before enabling it in service configuration.

### `[dependencies]`

Controls dependency extraction and import resolution.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Enable dependency extraction and storage |
| `resolve_imports` | bool | `true` | Enable import-path resolution |

### `[beads]`

Indexes beads (the issue tracker) as searchable content, from either a Dolt
server or a br store's JSONL export.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Index beads content |
| `source` | string | `"dolt"` | Which store to read: `"dolt"` or `"jsonl"` |
| `host` | string | `"dolt.example"` | Dolt server hostname (`source = "dolt"`) |
| `port` | int | `3306` | Dolt server port (`source = "dolt"`) |
| `user` | string | `"root"` | Dolt user (`source = "dolt"`) |
| `databases` | string[] | `[]` | Database names to index (e.g. `["beads_aegis"]`). Rig identity for BOTH sources |
| `jsonl_paths` | table | `{}` | `source = "jsonl"`: database name → path of that rig's `issues.jsonl`. Required for every entry in `databases` |
| `include_comments` | bool | `true` | Include bead comments in indexed content |
| `include_closed` | bool | `false` | Include closed beads |
| `max_age_days` | int | `90` | Skip beads older than this many days (`0` = no limit) |
| `exclude_labels` | string[] | `[]` | Exclude beads carrying any of these labels (case-insensitive) — keeps sensitive beads out of the index |

### `[archive]`

Indexes archive sources — directories of markdown files with YAML frontmatter.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Index archive sources |
| `webhook_secret` | string | `""` | HMAC-SHA256 secret for push notifications (empty = reject all webhooks) |
| `sources` | table[] | `[]` | Archive sources — each `[[archive.sources]]` has `name`, `path`, `schema` (required) and optional `name_field` |

### `[access]`

Role-based access filtering — restricts which repos a calling role can see.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `default_allow` | bool | `true` | When true, repos not named in any rule are visible to all roles; when false, repos must be explicitly granted |
| `roles` | table[] | `[]` | Role rules — each `[[access.roles]]` has `name` (required) and optional `allow`, `deny`, `deny_paths` globs. `deny` beats `allow`. |

### `[sources]`

Source-link generation — maps indexed files to clickable forge URLs.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `remote_template` | string | `""` | Template for all auto-detected remotes; empty = auto-detect forge type. Placeholders `{remote_base}`, `{path}`, `{line}` |
| `default_url` | string | `""` | Fallback template for repos with no remote/override. Placeholders `{repo}`, `{path}`, `{line}` |
| `repos` | table | `{}` | Per-repo URL overrides under `[sources.repos]` (highest priority) |
| `forge_overrides` | table | `{}` | Per-host forge-type overrides under `[sources.forge_overrides]` |

### `groups`

Named repo groups for scoped search (`--group` CLI flag / `?group=` HTTP param). None by default; groups compose with `[access]` filtering. Each entry is an `[[groups]]` table with `name` and `repos` (both required).

### `file_types`

Custom file-type classification rules. None by default; evaluated in order, first match wins, with unmatched files falling back to the built-in classifier. Each entry is a `[[file_types]]` table with `name` and `patterns` (both required). Built-in category names are `source`, `test`, `documentation`, `config`; custom names are stored and displayed as-is.

### `quipu_endpoint`

Top-level key (not a table). Quipu knowledge-graph endpoint (e.g. `"http://quipu.example"`); unset by default. When set, search results are annotated with entity spotlight data. Must appear before the first `[section]` header in the TOML.

### `quipu_push_chunks`

Top-level key (not a table). Default `false`. When `true` (and bobbin is built with the `knowledge` feature), each index run publishes a governed code-entity graph as a diffed snapshot replacement under the producer key `bobbin-chunks:{repo}` — `CodeModule`, `CodeSymbol`, `Document`, and `Section` facts plus chunk identity, membership, order, and adjacency, never chunk content. When `quipu_endpoint` is configured, Bobbin sends the snapshot to its authenticated `/knot` endpoint using `QUIPU_AUTH_TOKEN`, `QUIPU_AUTH_TOKEN_FILE`, or `~/.config/aegis/quipu_token` (in that order); without a remote endpoint, Bobbin uses its embedded Quipu store. The target must support `replace_snapshot`; Bobbin refuses a response that does not confirm replacement rather than letting facts accumulate per run. A failed publication reports `dropped_pushes=1` and makes the index command exit nonzero, without retrying an abandoned request. Remote requests carry `X-Quipu-Client: ingest-cron`.

#### Choosing a source

`source = "dolt"` talks to a Dolt server over the MySQL protocol.
`source = "jsonl"` reads a br store's `.beads/issues.jsonl` — a plain file, so
it needs no credential, no network, and no server: a host that already clones
the rig's repo for code indexing has the bead corpus on disk already.

Both produce the same chunks. `databases` still names the rigs (chunk keys are
`beads:<rig>:<id>`), so switching sources does **not** re-key the corpus, and
`include_closed`, `max_age_days` and `exclude_labels` apply identically —
the JSONL reader applies them as a predicate where the Dolt reader pushes them
down as a `WHERE` clause, and one test walks both rule sets through the same
table of cases.

A JSONL export only moves when its producer writes it, and a producer that has
stopped is as quiet as a dead server. Every indexing run therefore prints the
path **and its age**:

```text
  Indexing beads from br JSONL (/var/lib/bobbin/repos/aegis/.beads/issues.jsonl [12m old])...
```

Watch that number. bobbin once answered bead search from a store that had
stopped receiving writes and reported nothing unusual for 18 days.
