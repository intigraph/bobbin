---
title: Hooks Configuration
description: Configuring automatic context injection hooks
tags: [config, hooks]
status: published
category: config
related: [cli/hook.md, guides/hooks.md, config/reference.md]
---

# Hooks Configuration

All hook settings live under `[hooks]` in `.bobbin/config.toml` (local) or
`~/.config/bobbin/config.toml` (global).

## Settings Reference

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `threshold` | float | `0.5` | Minimum relevance score to include a result in injected context |
| `budget` | int | `300` | Maximum lines of injected context per prompt |
| `content_mode` | string | `"full"` | Display mode: `"full"`, `"preview"`, or `"none"` |
| `min_prompt_length` | int | `20` | Skip injection for prompts shorter than this (chars) |
| `gate_threshold` | float | `0.65` | Minimum top-result semantic similarity to inject at all |
| `dedup_enabled` | bool | `true` | Skip re-injection when results match previous turn |
| `show_docs` | bool | `true` | Include documentation files in output (false = code only, docs still used for bridging) |
| `format_mode` | string | `"standard"` | Output format: `"standard"`, `"minimal"`, `"verbose"`, or `"xml"` |
| `reducing_enabled` | bool | `true` | Track injected chunks across turns; only inject new/changed chunks |
| `repo_affinity_boost` | float | `2.0` | Score multiplier for files from the agent's current repo. Set `1.0` to disable |
| `feedback_prompt_interval` | int | `5` | Prompt agents to rate injections every N injections. `0` = disabled |
| `skip_prefixes` | list | `[]` | Prompt prefixes that skip injection entirely (case-insensitive) |
| `keyword_repos` | list | `[]` | Keyword-triggered repo scoping rules (see below) |

## Gate Threshold vs Threshold

These are two different filters:

- **`gate_threshold`** — Decides whether injection happens *at all*. Checks the top semantic search result's raw similarity. Below this → skip the entire injection. This prevents bobbin from injecting irrelevant context when your prompt has nothing to do with the indexed code.

- **`threshold`** — Once injection is triggered, this controls which *individual results* are included. Results scoring below this threshold are dropped from the injection.

Typical tuning: raise `gate_threshold` (e.g., `0.75`) if you get too many injections on unrelated prompts. Lower `threshold` (e.g., `0.3`) if you want more results per injection.

## Skip Prefixes

Skip injection entirely for prompts that start with operational commands:

```toml
[hooks]
skip_prefixes = [
    "git ", "git push", "git pull", "git status",
    "sd ready", "sd list", "sd show", "sd close",
    "gt hook", "gt mail", "gt handoff", "gt prime",
    "cargo check", "cargo build", "cargo test",
    "go test", "go build", "go vet",
    "y", "n", "yes", "no", "ok", "done",
]
```

Matching is case-insensitive and prefix-based (the prompt is trimmed before
matching). This prevents wasting tokens on context injection for commands
where the agent just needs to run a tool, not understand code.

## Keyword Repos

Route queries to specific repositories based on keywords. Without this,
every query searches all indexed repos, which can return noisy cross-repo
results.

```toml
[[hooks.keyword_repos]]
keywords = ["bobbin", "search index", "embedding", "context injection"]
repos = ["bobbin"]

[[hooks.keyword_repos]]
keywords = ["seeds", "sd ", "issue tracking"]
repos = ["seeds"]

[[hooks.keyword_repos]]
keywords = ["traefik", "reverse proxy", "TLS certificate"]
repos = ["aegis", "goldblum"]

[[hooks.keyword_repos]]
keywords = ["shanty", "tmux", "terminal"]
repos = ["shanty"]
```

When a prompt matches any keyword (case-insensitive substring), the search
is scoped to the matched repos instead of searching all repos. Multiple
rules can match — repos are deduplicated.

If no keywords match, the search falls back to all repos (default behavior).

## Format Modes

| Mode | Description |
|------|-------------|
| `standard` | File paths, chunk names, line ranges, and content. Default. |
| `minimal` | File paths and chunk names only. Lowest token cost. |
| `verbose` | Full metadata including tags, scores, and match types. |
| `xml` | XML-structured output for agents that parse structured context. |

## Reducing (Delta Injection)

When `reducing_enabled = true` (default), bobbin tracks which chunks were
injected in previous turns. On subsequent prompts, only new or changed
chunks are injected — chunks already in the agent's context are skipped.

This reduces token waste on multi-turn conversations about the same topic.
Disable it (`false`) if you need every turn to get the full context
(e.g., testing injection quality).

## Repo Affinity

`repo_affinity_boost` gives a score multiplier to files from the agent's
current repository (detected from the working directory). Default `2.0`
means results from the current repo score 2x higher than cross-repo results.

Set to `1.0` to treat all repos equally. Useful when you frequently need
context from other projects.

## Example: Multi-Agent Environment

For environments with many agents working across multiple repos:

```toml
[hooks]
threshold = 0.5
budget = 300
gate_threshold = 0.65
min_prompt_length = 20
dedup_enabled = true
reducing_enabled = true
repo_affinity_boost = 2.0
feedback_prompt_interval = 5

# Skip operational commands
skip_prefixes = [
    "git ", "sd ", "gt ",
    "cargo ", "go ", "npm ",
    "y", "n", "ok", "done",
]

# Route queries to relevant repos
[[hooks.keyword_repos]]
keywords = ["bobbin", "search", "embedding"]
repos = ["bobbin"]

[[hooks.keyword_repos]]
keywords = ["beads", "issue", "bead"]
repos = ["beads"]

[[hooks.keyword_repos]]
keywords = ["gas town", "gastown", "molecule", "polecat"]
repos = ["gastown"]
```
