---
title: Hooks
description: Setting up Claude Code hooks for automatic context injection
tags: [hooks, claude-code, guide]
status: draft
category: guide
related: [cli/hook.md, config/hooks.md, getting-started/agent-setup.md]
commands: [hook]
---

# Hooks

Bobbin's hook system integrates with Claude Code to automatically inject relevant code context into every conversation. When you type a prompt, bobbin searches your index for code related to what you're asking about and injects it before Claude sees the message. The result: Claude understands your codebase without you having to copy-paste code.

## How hooks work

Bobbin installs four Claude Code hooks:

1. **UserPromptSubmit** — fires on every prompt you send. Bobbin runs a semantic search against your query, and if the results are relevant enough, injects them as context that Claude can see.
2. **SessionStart (compact)** — fires after Claude Code compacts its context window. Bobbin re-injects key context so Claude doesn't lose codebase awareness after compaction.
3. **PostToolUse** (`Write|Edit|Bash|Grep|Glob|Read`) — fires after a tool runs. Bobbin reactively injects files related to the code you just touched.
4. **PostToolUseFailure** — fires when a tool call fails. Bobbin surfaces related context to help diagnose the failure.

All four hooks are non-blocking and add minimal latency to your interaction.

## Setting up hooks

### Prerequisites

You need a bobbin index first:

```bash
bobbin init
bobbin index
```

### Install hooks

```bash
bobbin hook install
```

This adds hook entries to your project's `.claude/settings.json`. From now on, every prompt in this project directory triggers automatic context injection.

For global installation (all projects):

```bash
bobbin hook install --global
```

Global hooks write to `~/.claude/settings.json` and fire in every Claude Code session, regardless of project.

### Verify installation

```bash
bobbin hook status
```

Output shows whether hooks are installed and the current configuration:

```text
Claude Code hooks: installed
Git hook: not installed

Configuration:
  threshold:        0.5
  budget:           150 lines
  content_mode:     preview
  min_prompt_length: 10
  gate_threshold:   0.75
  dedup:            enabled
```

## Tuning injection behavior

The defaults work for most projects, but you can tune how aggressively bobbin injects context.

### Threshold

The **threshold** controls the minimum relevance score for a search result to be included in the injected context:

```bash
bobbin hook install --threshold 0.3    # More results, lower relevance bar
bobbin hook install --threshold 0.7    # Fewer results, higher quality
```

Lower values mean more code gets injected (broader context). Higher values mean only highly relevant code is injected (tighter focus).

You can also set this in `.bobbin/config.toml`:

```toml
[hooks]
threshold = 0.5
```

### Budget

The **budget** limits the total lines of injected context:

```bash
bobbin hook install --budget 200    # More context
bobbin hook install --budget 50     # Minimal context
```

A larger budget gives Claude more code to work with. A smaller budget keeps injections concise and avoids overwhelming the context window.

```toml
[hooks]
budget = 150
```

### Gate threshold

The **gate threshold** is a separate check that decides whether injection happens *at all*. Before injecting, bobbin checks the top semantic search result's raw similarity score. If it falls below the gate threshold, the entire injection is skipped — your prompt isn't relevant to the indexed code.

```toml
[hooks]
gate_threshold = 0.75    # Default: skip injection if top result < 0.75 similarity
```

Lower values make injection more eager (fires on loosely related prompts). Higher values make it more conservative (only fires when the prompt clearly relates to indexed code).

### Minimum prompt length

Very short prompts ("yes", "ok", "continue") don't benefit from code injection. The `min_prompt_length` setting skips injection for prompts below a character threshold:

```toml
[hooks]
min_prompt_length = 10    # Default
```

### Content mode

Controls how much code is included per result:

```toml
[hooks]
content_mode = "preview"    # 3-line excerpts (default)
# content_mode = "full"     # Full source code
# content_mode = "none"     # Paths and metadata only
```

**`preview`** is the default and works well for most cases — it gives Claude enough to understand each result without consuming too much of the context window.

**`full`** is useful when you want Claude to have complete function implementations in context, not just previews.

### Deduplication

By default, bobbin tracks what it injected in the previous prompt and skips re-injection if the results haven't changed. This avoids flooding Claude's context with the same code on follow-up messages about the same topic.

```toml
[hooks]
dedup_enabled = true    # Default
```

Set to `false` if you want every prompt to get fresh injection regardless.

## Progressive reducing

Over a long conversation, bobbin avoids re-injecting code you've already seen. The **reducing** system keeps a per-session ledger of every chunk injected and filters duplicates on subsequent turns.

```toml
[hooks]
reducing_enabled = true    # Default
```

How it works:

1. Each injection records which chunks were sent, stored in `.bobbin/session/<session_id>/ledger.jsonl`
2. On the next prompt, bobbin filters out chunks already in the ledger
3. Only new or changed chunks are injected
4. The ledger resets on context compaction (SessionStart hook)

This means early prompts get full context, and follow-up prompts get only *new* relevant code — keeping the context window efficient.

When reducing fires, you'll see output like:

```text
bobbin: injecting 3 new chunks (7 previously injected, turn 4)
```

Set `reducing_enabled = false` to inject all matching chunks every time (useful for debugging).

## Keyword-triggered repo scoping

In multi-repo setups, certain queries clearly target specific repos. Configure keyword rules to auto-scope search:

```toml
[[hooks.keyword_repos]]
keywords = ["ansible", "playbook", "deploy"]
repos = ["goldblum", "homelab-mcp"]

[[hooks.keyword_repos]]
keywords = ["bobbin", "search", "embedding"]
repos = ["bobbin"]

[[hooks.keyword_repos]]
keywords = ["beads", "bd ", "issue"]
repos = ["beads"]
```

When any keyword matches (case-insensitive substring), search is scoped to only those repos. If no keywords match, all repos are searched.

Multiple rules can match simultaneously — their repo lists are merged and deduplicated.

## Repo affinity boost

Files from the agent's current working directory repo get a score multiplier:

```toml
[hooks]
repo_affinity_boost = 2.0    # Default: 2x score for local repo
```

This is a soft bias — cross-repo results still appear, but local files rank higher. Set to `1.0` to disable.

## Feedback prompts

Bobbin periodically prompts agents to rate injected context quality:

```toml
[hooks]
feedback_prompt_interval = 5    # Every 5 injections (0 = disabled)
```

Every N injections, bobbin shows unrated injection IDs and prompts:

```text
bobbin: 3 unrated injections this session. Rate with:
  bobbin feedback submit --injection <id> --rating <useful|noise|harmful>
```

Ratings feed into the [Feedback System](feedback.md) — over time, frequently useful code gets boosted and noisy code gets demoted.

## Format modes

Control the output format of injected context:

```toml
[hooks]
format_mode = "standard"    # Default
```

| Mode | Description |
|------|-------------|
| `standard` | File paths, line ranges, and code content |
| `minimal` | Compact output, fewer decorators |
| `verbose` | Extended metadata (scores, tags, chunk types) |
| `xml` | XML-tagged output for structured parsing |

The `show_docs` setting controls whether documentation files appear in injection output:

```toml
[hooks]
show_docs = true    # Default. Set false to inject code-only
```

When `false`, doc files are still used for [bridge discovery](searching.md#bridge-mode) but not shown to the agent.

## Full configuration reference

All settings in `.bobbin/config.toml` under `[hooks]`:

| Setting | Default | Description |
|---------|---------|-------------|
| `threshold` | `0.5` | Minimum relevance score to include a result |
| `budget` | `300` | Maximum lines of injected context |
| `content_mode` | `"full"` | Display mode: `full`, `preview`, or `none` |
| `min_prompt_length` | `20` | Skip injection for prompts shorter than this (chars) |
| `gate_threshold` | `0.45` | Minimum top-result similarity to inject at all |
| `dedup_enabled` | `true` | Skip injection when results match previous turn |
| `reducing_enabled` | `true` | Progressive reducing (delta injection across session) |
| `show_docs` | `true` | Include documentation files in output |
| `format_mode` | `"standard"` | Output format: `standard`, `minimal`, `verbose`, `xml` |
| `skip_prefixes` | `[]` | Prompt prefixes that skip injection entirely |
| `keyword_repos` | `[]` | Keyword-triggered repo scoping rules |
| `repo_affinity_boost` | `2.0` | Score multiplier for current repo files |
| `feedback_prompt_interval` | `5` | Prompt for feedback every N injections (0 = disabled) |

## Git hooks

Separately from Claude Code hooks, bobbin can install a git post-commit hook to keep the index fresh:

```bash
bobbin hook install-git-hook
```

This re-indexes files changed in each commit. See [Watch & Automation](watch-automation.md) for details on keeping your index current.

## Hot topics

After using hooks for a while, bobbin accumulates data on which code areas are most frequently injected. The `hot-topics` subcommand generates a summary:

```bash
bobbin hook hot-topics
```

This writes `hot-topics.md` with analytics about your most-referenced code. Use it to identify which parts of your codebase come up most often in AI conversations — these are the areas worth keeping well-documented and well-structured.

Use `--force` to regenerate even if the injection count hasn't reached the automatic threshold:

```bash
bobbin hook hot-topics --force
```

## Reactions (PostToolUse hooks)

Reactions are rule-based context injections that fire after specific tool calls. Unlike the prompt-level hook, reactions target tool usage patterns — when an agent edits a config file, runs a deploy command, or touches a specific codebase area, bobbin can inject relevant guidance and related files.

Reactions live in `.bobbin/reactions.toml`:

```toml
[[reactions]]
name = "service-restart-runbook"
tool = "Bash"
[reactions.match]
command = "systemctl (restart|stop|start)"
guidance = "Check the runbook for {args.command} before restarting services in production."
search_query = "service restart runbook {file_stem}"
max_context_lines = 30

[[reactions]]
name = "ansible-role-edit"
tool = "Edit"
[reactions.match]
file_path = ".*/ansible/roles/.*"
guidance = "This is an Ansible role. Test with: ansible-playbook --check -l test"
use_coupling = true
coupling_threshold = 0.15

[[reactions]]
name = "credential-guard"
tool = "Edit"
[reactions.match]
file_path = ".*\\.(env|credentials|secret).*"
guidance = "WARNING: This file may contain credentials. Never commit secrets to git."
```

### Reaction fields

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Rule name (used for dedup and metrics) |
| `tool` | string | Tool name glob (e.g., `"Edit"`, `"Bash"`, `"mcp__homelab__*"`) |
| `match` | table | Parameter match conditions — keys are param names, values are regex |
| `guidance` | string | Text shown to the agent. Supports `{args.X}` and `{file_stem}` templating |
| `search_query` | string | Search query template for injecting related files |
| `search_group` | string | Index group to search |
| `search_tags` | list | Tag filters for scoped search |
| `max_context_lines` | int | Max lines to inject (default: 50) |
| `use_coupling` | bool | Use git coupling instead of search |
| `coupling_threshold` | float | Min coupling score (default: 0.1) |
| `roles` | list | Role patterns this reaction applies to (glob, e.g., `"aegis/crew/*"`) |

### Role-scoped reactions

Restrict reactions to specific agent roles:

```toml
[[reactions]]
name = "deploy-check"
tool = "Bash"
[reactions.match]
command = "deploy|kubectl"
roles = ["*/crew/*"]      # Only crew members, not polecats
guidance = "Deploy detected. Verify staging tests passed first."
```

## Noise filtering

In multi-agent environments, many prompts are operational (patrol nudges, reactor alerts, handoff messages) rather than development queries. Bobbin automatically detects these and skips injection to avoid wasting tokens on irrelevant context.

Automated message detection recognizes patterns like:

- Patrol nudges (`"Auto-patrol: ..."`, `"PATROL LOOP ..."`)
- Reactor alerts (`"[reactor] ..."`)
- Handoff/startup protocols (`"HANDOFF COMPLETE"`, `"STARTUP PROTOCOL"`)
- Agent role announcements (`"aegis Crew ian, checking in."`)
- System reminder blocks (`"<system-reminder>..."`)

This is hardcoded in the hook logic and not configurable — it targets patterns that are definitively operational.

For user-configurable noise filtering, use `skip_prefixes`:

```toml
[hooks]
skip_prefixes = [
    "git ", "bd ", "gt ",
    "cargo build", "go test",
    "y", "n", "ok", "done",
]
```

See [Hooks Configuration](../config/hooks.md) for full skip_prefixes documentation.

## Directory navigation injection

When an agent encounters a file-not-found error or tries to read a directory, bobbin can inject the directory tree to help navigation:

- **EISDIR (directory read)**: Shows the directory tree (up to 20 lines)
- **File not found**: Shows the parent directory tree to help locate the right file

This fires via the `try_directory_navigation()` function in the hook and requires no configuration.

## Faction-scoped NA grounding

A trusted harness (Neural Amplifier) that consults its knowledge graph once at a turn boundary can pass that evidence into bobbin's hooks, so the graph's answer is injected without adding graph latency to the hook's hot path. The harness includes a `grounding` object in the hook's stdin JSON:

```json
{
  "prompt": "...",
  "grounding": {
    "scope": "na",
    "grounding_id": "sha256:<hex>",
    "faction_id": "gaians",
    "worldview_sha256": "<hex>"
  }
}
```

The reference is resolved **locally and verified before anything is injected**: `grounding_id` is a content address into a grounding cache on disk (`YUPANA_GROUNDING_CACHE_DIR`, `HANK_GROUNDING_CACHE_DIR`, or the `yupana/grounding` directory under the XDG state home), and the cached evidence must hash to exactly that digest and carry the same `faction_id` and `worldview_sha256` as the reference. Unknown scope, missing fields, a digest mismatch, or evidence from another faction or worldview all make the hook **abstain** — no unscoped fact reaches an injection.

Verified grounding is injected in two places:

- **inject-context** (thin-client mode, when a remote bobbin server is configured) — the grounding text is printed before ordinary code retrieval runs.
- **post-tool-use** — after `Edit` or `Write` tool calls, the same verified evidence is appended to the reactive context.

Each grounding injection is recorded in the feedback store as a scoped injection (`scope = "na"`, plus its `faction_id` and `grounding_id`), so it shows up in feedback and lineage queries like any other injection.

## Removing hooks

Remove Claude Code hooks:

```bash
bobbin hook uninstall

# Or for global hooks:
bobbin hook uninstall --global
```

Remove the git post-commit hook:

```bash
bobbin hook uninstall-git-hook
```

## Practical workflows

### Daily development setup

Install hooks once per project, then forget about them:

```bash
cd ~/projects/my-app
bobbin init && bobbin index
bobbin hook install
bobbin hook install-git-hook    # Keep index fresh on commits
```

Now every Claude Code conversation in this directory automatically gets relevant code context.

### Combining hooks with watch mode

For real-time index updates (not just on commits):

```bash
bobbin hook install         # Context injection
bobbin watch &              # Real-time index updates
```

This ensures the injected context reflects your latest uncommitted changes, not just what was last committed.

### Debugging injection

If Claude doesn't seem to be getting the right context, check what bobbin would inject for a given prompt:

```bash
echo '{"query": "how does auth work"}' | bobbin hook inject-context --no-dedup
```

Or check the hook status to verify configuration:

```bash
bobbin hook status --json
```

## Next steps

- [Governed Path Boundaries](governed-boundaries.md) — tripwire policies named
  in the injected context, on both the local and thin-client paths
- [Watch & Automation](watch-automation.md) — keep your index current
- [Searching](searching.md) — understand the search engine behind injection
- [`hook` CLI reference](../cli/hook.md) — full subcommand reference
- [Configuration Reference](../config/reference.md) — all `config.toml` settings

## Scope prompt retrieval to groups

Prompt retrieval can use named repository groups without removing repositories
from the index or changing explicit searches. Global configuration is merged
with the nearest project configuration. Project values override global values.

```toml
[[groups]]
name = "engineering"
repos = ["engine", "client"]

[[groups]]
name = "external"
repos = ["example"]

[hooks]
search_group = "engineering"

[hooks.roles]
"crew/*" = ["engineering"]
"crew/research" = ["external"]
```

The most specific matching role glob wins, using the same role identity as
repository access filtering. An absent role match uses `search_group`. With
neither setting, retrieval keeps its previous scope. A configured empty group
or empty intersection skips injection; an unknown group refuses retrieval.
Keyword repository hints intersect an explicit group and cannot widen it.
Repository access rules remain independent and still apply.

A harness may publish `bobbin_groups` on the current anchored plate:

```json
{"item":"work-1","at":1700000000,"bobbin_groups":["engineering"]}
```

A fresh, matching plate overrides the role group. Shantytown publishes these
names from `bobbin-group:<name>` item labels when it already has the item row;
ID-only dispatches omit unknown metadata until the next anchor refresh. Bobbin
never queries the tracker on prompt submission. Missing, stale, malformed or
mismatched task metadata falls back to the role group. Group names are bounded
and must refer to configured groups.

The scope applies to local assembly, remote context, and remote search fallback.
Results without repository provenance are omitted when a scope is explicit.
Unscoped bundle expansion and complementary expansion are disabled for scoped
injections so they cannot reintroduce excluded repositories. Explicit search
and bundle commands remain available.

Injection telemetry includes `retrieval_scope` and `file_provenance` with
repository, file and chunk keys for subsequent relevance audits. Historical
records without these fields cannot establish per-repository relevance or
usage. A missing rating is unknown, rather than a negative rating.
