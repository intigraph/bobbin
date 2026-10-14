# Bobbin: Tool-Result Context + Breadcrumb System

> **Implementation status (2026-09-05):** 🟨 **Feature 1 and the breadcrumb core have landed; triggers remain planned.**
> `src/breadcrumb.rs` provides the typed store and corruption-safe JSON persistence; `src/cli/breadcrumb.rs` provides `bc create/list/recall/delete`, with top-level `mark` and `recall` aliases. Recall currently returns the stored query and pinned files and records usage.
>
> **Feature 1 (tool-response-driven context) is done**, in `src/cli/hook_tool_response.rs`: `PostToolUseInput` declares `tool_response`, per-tool extractors turn it into repo-relative paths, and a `DiscoveredFiles` dispatch looks up the coupling of the files a search actually FOUND under a 60/40 budget split. Verified end to end against an indexed fixture repo: with `tool_response` the injection gains a `## Related to What You Found` section naming a co-changing file the semantic search did not surface; with the field absent the output is byte-identical to before.
>
> **Phase 1 step 2 below is obsolete, not skipped.** It says to log response shapes to metrics first because the JSON Claude Code sends was unconfirmed. It was already confirmable: desire-path captures every PostToolUse payload, `tool_response` included, so the extractors were written against measured shapes. The measurement immediately contradicted the obvious implementation — Grep in `content` mode returns `filenames: []` and hides every path inside `content` as `path:line:text`, so an extractor reading only `filenames` finds nothing in the mode Grep is most often called in, and finds it silently.
>
> Context assembly on recall, keyword triggers, session discovery, pruning, and Desire Path error capture remain later slices of GitHub issue #51.

## Context

Agents waste significant time rediscovering context across sessions. Two gaps:

1. **PostToolUse ignores tool results** — bobbin sees what a tool was *asked* to do (`tool_input`) but not what it *found* (`tool_response`). Claude Code already sends `tool_response` in the stdin JSON; bobbin's `PostToolUseInput` struct just doesn't declare the field (serde silently drops it).

2. **No breadcrumb/bookmark mechanism** — when an agent discovers something useful after deep searching, there's no way to leave a named shortcut so future sessions can instantly recall that context. Agents rebuild understanding from scratch every handoff.

## Feature 1: Tool-Response-Driven Context

### What Changes

**File: `src/cli/hook.rs`** (line 844)

Add `tool_response` field to `PostToolUseInput`:

```rust
struct PostToolUseInput {
    // ... existing fields ...
    #[serde(default)]
    tool_response: serde_json::Value,  // NEW
}
```

This is backward-compatible (`#[serde(default)]` → `Value::Null` if absent).

### Enhanced PostToolUse Dispatch (line 1754)

Add a new dispatch mode:

```rust
enum DispatchMode {
    EditRelated { file_path: String },
    SearchQuery { query: String, original_cmd: String },
    RefsOnly { file_path: String },
    DiscoveredFiles { files: Vec<String>, query: String },  // NEW
}
```

For **Grep/Glob/Bash** tools: after extracting the search query from `tool_input` (existing behavior), ALSO extract file paths from `tool_response`. Look up coupling relationships for those discovered files. This gives the agent "files related to what your search actually found."

### Extraction Logic

Add functions to parse `tool_response` by tool type:
- `extract_files_from_tool_response(tool_name, tool_response) -> Vec<String>`
- Grep: parse matched file paths from response
- Glob: parse matched file list
- Bash: parse stdout for file path patterns

**Phase 1 approach**: Log `tool_response` shapes to metrics first (we need to confirm exact JSON structure from Claude Code), then build extractors.

### Budget Split

When both query-based and discovered-file-based results exist:
- 60% budget → semantic search results (from query)
- 40% budget → coupling/related files (from discovered paths)

## Feature 2: Breadcrumb System

### Concept

An agent creates a named shortcut that maps to a specific bobbin query + optional pinned files. Future agents recall it by name or have it auto-triggered by keyword matching.

### Design Principles (Agent-Friendly)

1. **Minimal syntax** — positional args for the common case, flags only for extras
2. **Keywords not regex** — agents shouldn't write regex. Simple word matching.
3. **Discoverable** — breadcrumb names injected at session start so agents know they exist
4. **One command** — create with triggers inline, not as a separate step
5. **Description required** — future agents need to understand what a breadcrumb captures
6. **Aliasable commands** — all subcommands support aliases via `bobbin run` / `commands.toml`. If an agent keeps typing `bobbin mark` instead of `bobbin bc create`, that's a desire path — alias it.
7. **Desire-path integration (`dp`)** — `dp` (desire_path rig) is a Go CLI that captures failed tool calls and surfaces patterns. Bobbin should integrate as a first-class `dp` consumer:
   - Unrecognized bobbin subcommands/flags pipe to `dp record --source bobbin` (not a custom metrics file)
   - `dp paths --source bobbin` shows what agents keep trying that doesn't exist
   - `dp alias` maps agent-attempted commands to real bobbin subcommands
   - This means bobbin's CLI error handler should call `dp record` before printing "unknown command". The breadcrumb CLI ships with this from day one.
   - Ship default aliases (`mark` → `bc create`, `recall` → `bc recall`) based on what we expect agents to try, then let `dp` validate whether those were the right guesses.

### Storage

**File: `.bobbin/breadcrumbs.json`** — JSON, agent-friendly, debuggable.

Follows the pattern of existing `commands.rs` (`CommandDef` / `CommandsMap` / `load/save`).

```rust
pub struct Breadcrumb {
    name: String,              // "auth-refresh", "db-migration-flow"
    description: String,       // REQUIRED: what this captures (for future agents)
    query: String,             // Semantic search query to run on recall
    pinned_files: Vec<String>, // Always-include files
    tags: Vec<String>,         // Categorization
    keywords: Vec<String>,     // Trigger words (simple matching, NOT regex)
    created_by: String,        // Agent identity or session_id
    created_at: String,        // RFC3339
    last_recalled: Option<String>,
    recall_count: u64,
    ttl_days: u32,             // 0 = never expires
}
```

**Key simplification**: No `TriggerRule` struct with regex patterns. Just a flat
`keywords` list. If any keyword appears in a user prompt or tool input, the
breadcrumb fires. Bobbin converts keywords to case-insensitive substring matches
internally. Agents think in words, not patterns.

### CLI Commands

```bash
# CREATE — positional: name, query. Flags for extras.
bobbin bc create <name> "<query>" "<description>" [--pin <files>] [--tag <tags>] [--on <keywords>]

# RECALL — get the context back
bobbin bc recall <name>

# LIST — see what exists
bobbin bc list

# DELETE
bobbin bc delete <name>

# PRUNE — clean up stale breadcrumbs
bobbin bc prune [--days <n>]
```

`bobbin bc` is the primary interface (not `bobbin breadcrumb` — too long).

**Built-in aliases** (registered in `commands.toml` on install):
```toml
[mark]
description = "Alias for bc create"
command = "bc"
args = ["create"]

[recall]
description = "Alias for bc recall"
command = "bc"
args = ["recall"]
```

So agents can use `bobbin mark`, `bobbin recall`, or `bobbin bc create` — whichever
sticks. Desire-path metrics will show which forms agents actually reach for.

**Create is one command with everything inline:**
```bash
bobbin bc create auth-refresh \
  "token refresh authentication flow" \
  "Auth token refresh spans 5 files across auth module and middleware" \
  --pin src/auth/refresh.rs,src/middleware/token.rs \
  --tag auth,security \
  --on refresh_token,token_expiry,auth_refresh
```

Commas for multi-value flags (not repeated `--pin` flags).

### Session Discovery

**SessionStart hook** (compaction recovery): Inject breadcrumb names + descriptions
into the context so agents know what's available. Format:

```
=== Breadcrumbs ===
- auth-refresh: Auth token refresh spans 5 files across auth module and middleware
- db-migration: Database migration pipeline from schema to rollback
```

This is cheap (just names + descriptions, no search). Agents see breadcrumbs
exist without having to run `bobbin bc list`.

Implemented for both `hook prime-context` (startup) and `hook session-context`
(compaction). Discovery reads the repository-local store, skips expired entries,
and never recalls or rewrites them. It lists at most 29 entries; compaction
reserves at most 30% of its line budget for the list. Missing, corrupt, or empty
stores leave the ordinary primer/recovery output intact. Keyword triggers and
context assembly on recall remain separate work below.

### Hook Integration (Keyword Triggers)

**UserPromptSubmit** (line 545): After normal search, scan breadcrumb `keywords`
against the user prompt. If any keyword is a substring match (case-insensitive),
merge that breadcrumb's context into the injection.

**PostToolUse** (line 1743): After normal dispatch, scan breadcrumb `keywords`
against the tool_input (serialized to string). If matched, inject breadcrumb
context alongside normal results.

**Matching is simple**: `keywords: ["refresh_token", "token_expiry"]` means
"if the string `refresh_token` OR `token_expiry` appears anywhere in the prompt
or tool input, trigger this breadcrumb." No regex, no JSON path traversal.

Budget sharing: normal results get 70%, triggered breadcrumbs get 30%.

Triggered breadcrumbs participate in dedup (included in session ID hash).

### Example Workflow

```bash
# Agent discovers auth token refresh spans 5 files after deep searching
bobbin bc create auth-refresh \
  "token refresh authentication flow" \
  "Token refresh logic spanning auth module and middleware — 5 key files" \
  --pin src/auth/refresh.rs,src/middleware/token.rs \
  --on refresh_token,token_expiry

# Next session: agent sees "auth-refresh" in session start context
# Agent can explicitly recall it:
bobbin bc recall auth-refresh

# Or it fires automatically when the agent greps for "refresh_token"
# (keyword match triggers injection alongside normal PostToolUse results)
```

## New Files

| File | Purpose |
|------|---------|
| `src/breadcrumb.rs` | Types, load/save, keyword matching (~300 lines) |
| `src/cli/breadcrumb.rs` | CLI subcommand handlers (~250 lines) |

## Modified Files

| File | Change |
|------|--------|
| `src/cli/hook.rs:844` | Add `tool_response` to `PostToolUseInput` |
| `src/cli/hook.rs:1754` | Add `DiscoveredFiles` dispatch mode |
| `src/cli/hook.rs:1743` | Extract files from `tool_response`, keyword-match breadcrumbs |
| `src/cli/hook.rs:545` | Add breadcrumb keyword matching to UserPromptSubmit |
| `src/cli/hook.rs` (session-context) | Inject breadcrumb names at session start |
| `src/cli/mod.rs` | Add `Bc` to `Commands` enum |
| `src/config.rs` | Optional breadcrumb config fields in `HooksConfig` |

## Implementation Phases

### Phase 1: Tool Response Capture
1. Add `tool_response` field to `PostToolUseInput`
2. Log response shapes to metrics (discover actual JSON from Claude Code)
3. Build extraction functions per tool type
4. Add `DiscoveredFiles` dispatch + coupling lookups
5. Tests

### Phase 2: Breadcrumb Core
1. `breadcrumb.rs` — Breadcrumb struct, BreadcrumbStore, load/save JSON
2. `cli/breadcrumb.rs` — create (positional args), recall, list, delete (done); prune remains Phase 4
3. Wire `Bc` subcommand into `cli/mod.rs`
4. Recall: return stored query + pinned files and update metadata (done); ContextAssembler execution remains a later slice
5. Register built-in `mark` and `recall` aliases (done as direct, help-visible commands)
6. Desire-path integration: unrecognized subcommands call `dp record --source bobbin` before printing error. Requires `dp` binary on PATH (graceful no-op if absent).
7. Tests for storage roundtrip, recall output, name validation

### Phase 3: Keyword Triggers + Discovery
1. Keyword matching in `breadcrumb.rs` (case-insensitive substring)
2. Integrate into UserPromptSubmit hook (scan prompt against keywords)
3. Integrate into PostToolUse hook (scan tool_input against keywords)
4. Budget splitting for mixed normal + breadcrumb results
5. Add breadcrumb names to SessionStart hook output (discovery; done for startup and compaction)
6. Dedup integration
7. Tests

### Phase 4: Polish
1. `prune` subcommand (remove breadcrumbs not recalled in N days)
2. TTL checking during keyword matching (skip expired)
3. Metrics (bc_created, bc_recalled, bc_triggered counts)
4. Docs update

## Verification

1. **Tool response**: `cargo build`, pipe mock PostToolUse JSON with `tool_response` field → verify parsed
2. **Breadcrumb CRUD**: `bobbin bc create test "test query" "test description" && bobbin bc list && bobbin bc recall test && bobbin bc delete test`
3. **Recall output**: Create breadcrumb with `--pin`, recall → verify pinned files appear in context
4. **Keyword trigger**: Create breadcrumb with `--on keyword`, pipe mock PostToolUse JSON containing "keyword" → verify breadcrumb context appears
5. **Session discovery**: Run `bobbin hook session-context` with breadcrumbs present → verify names listed
6. **E2E**: Create breadcrumb in session, handoff, recall in new session — verify context restored
