---
title: Feedback System
description: How agent ratings improve search quality over time
tags: [feedback, guide]
status: draft
category: guide
related: [guides/hooks.md, config/hooks.md]
---

# Feedback System

Bobbin tracks what context it injects and lets agents rate it. Over time, feedback data drives automatic quality improvements — files that are consistently useful get boosted, files that are consistently noise get demoted.

## How It Works

### 1. Injection Tracking

Every time bobbin injects context via hooks, it records:

- **injection_id** — Unique identifier for this injection
- **query** — The prompt that triggered the injection
- **files/chunks** — What was injected (paths, chunk IDs, content)
- **agent** — Which agent received the injection
- **session_id** — The Claude Code session

### 2. Feedback Collection

Agents rate injections as `useful`, `noise`, or `harmful`:

| Rating | Meaning |
|--------|---------|
| `useful` | The injected code was relevant and helpful |
| `noise` | The injection was irrelevant to the task |
| `harmful` | The injection was actively misleading |

Feedback is collected automatically via the `feedback_prompt_interval` config:

```toml
[hooks]
feedback_prompt_interval = 5  # Prompt every 5 injections (0 = disabled)
```

Every N injections, bobbin prompts the agent to rate up to 3 unrated injections from the current session.

### 3. Feedback Tags

Accumulated feedback generates tags on files:

| Tag | Meaning |
|-----|---------|
| `feedback:hot` | File is frequently rated as useful |
| `feedback:cold` | File is frequently rated as noise |

These tags are loaded during indexing and merged into chunk tags. Combined with tag effects:

```toml
[effects."feedback:hot"]
boost = 0.3    # Boost frequently-useful files
pin = true     # Always include if budget allows

[effects."feedback:cold"]
boost = -0.3   # Demote frequently-noisy files
```

## Memory citations and review dates

Memory recall uses the same `injection_id` as code context. Cite the printed
`m1`–`m5` item with the existing `feedback_submit` tool, HTTP endpoint, or CLI:

```sh
bobbin feedback submit --injection inj-example --item m1 --rating used --agent example
bobbin feedback stats
```

`used` means the agent acted on that evidence; injection alone is not use.
Replaying `used` for the same injection, item and agent returns the original
receipt without refreshing its time. A new injection can record another use.
Citation mappings are immutable, and unresolved feedback is never reassigned
when a citation is logged later. Per-entity statistics use the IRI captured in
the receipt. Historical duplicate receipts remain available but count once per
use in entity statistics. Their latest recorded use time is retained: old
duplicates cannot reliably distinguish a replay from a later genuine use.

With the `knowledge` feature, maintenance can bring an unused entity's
`reviewAfter` forward to now:

```sh
# arming: agent-invoked; review-due installs no schedule
bobbin feedback review-due --endpoint https://graph.example
bobbin feedback review-due --endpoint https://graph.example --apply --limit 20
```

Preview is the default. The command requires a fresh memory mirror for that
endpoint and the authoritative local feedback database; `--server` is refused.
It records when each mirror entity was first observed, including entities never
injected. Only entities observed locally for at least 90 days with no recorded
use in the last 90 days qualify. A new database or migration starts observation
now, regardless of the age of existing records. These are local usage counts,
not proof that nobody used the entity in another database.

Apply uses the normal Quipu credential and a conditional update in the entity's
existing source graph. It checks the exact version and previous review date,
never postpones an already-due review, and requires both a transaction receipt
and readback before reporting success. Refusals and uncertain outcomes stop the
command; verify the graph before retrying an uncertain write. Verified changes
update only the cached review date, preserving the mirror cursor and refresh
time. Expired records, unknown planes and invalid metadata are not repaired by
this command. No promotion, new memory version or review event is produced.

This maintenance is agent-invoked; it installs no schedule and makes no Quipu
calls from the per-prompt recall path. The graph's review consumer remains
responsible for turning due dates into reviews.

## HTTP API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/feedback` | POST | Submit a rating: `{injection_id, agent, rating, reason}` |
| `/feedback` | GET | List feedback with filters: `?rating=noise&agent=stryder&limit=50` |
| `/feedback/stats` | GET | Aggregated stats: total injections, ratings by type, coverage |
| `/injections/{id}` | GET | View injection detail: query, files, formatted output, feedback |
| `/feedback/lineage` | POST | Record an action that resolves feedback |
| `/feedback/lineage` | GET | List resolution actions |

## Lineage Tracking

When feedback leads to a fix (new tag rule, access control change, code fix), record it via lineage:

```json
{
  "action_type": "tag_effect",
  "description": "Added type:changelog demotion to reduce changelog noise",
  "commit_hash": "abc123",
  "bead": "aegis-xyz",
  "agent": "stryder"
}
```

Valid action types: `access_rule`, `tag_effect`, `config_change`, `code_fix`, `exclusion_rule`.

Lineage records link feedback to concrete improvements, creating an audit trail of search quality evolution.

## Web UI

The **Feedback** tab in the web UI shows:

- Injection and feedback counts with coverage percentage
- Rating breakdown (useful/noise/harmful)
- Filterable feedback list by rating and agent
- Click any feedback record to see the full injection detail: what was injected, the query, files included, and all ratings

## Best Practices

1. **Keep feedback_prompt_interval low** (3-5) during initial deployment to build up data quickly
2. **Review noise patterns** in the Feedback tab — repeated noise on the same files indicates missing tag rules or exclusions
3. **Use lineage** to track what you changed in response to feedback — this closes the loop
4. **Check feedback:hot/cold tags** after reindexing to verify the system is learning from ratings
