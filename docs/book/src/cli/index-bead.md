---
title: "index-bead"
description: "Reindex a single bead by id, without fetching the whole bead corpus."
tags: [cli]
status: published
category: cli-reference
---

# index-bead

Reindex a single bead by id, without fetching the whole bead corpus.

```bash
bobbin index-bead <BEAD_ID> [PATH] [OPTIONS]
```

`bobbin index --include-beads` is the batch path: it pulls every visible bead
out of Dolt to work out which ones changed. That is fine on a schedule and far
too expensive to run after every tracker edit. `index-bead` is the fast path for a
post-write trigger — it fetches one bead, re-embeds it only if its assembled
content actually changed, and leaves the rest of the corpus alone.

Both paths write the same corpus, keyed the same way (`beads:{rig}:{bead_id}`),
so they are interchangeable: a bead updated incrementally looks exactly as a
batch run would have left it.

## Examples

```bash
bobbin index-bead bo-abc123              # reindex one bead, searching every configured rig
bobbin index-bead bo-abc123 --rig aegis  # restrict the lookup to one rig
bobbin index-bead bo-abc123 --force      # re-embed even if the content hash is unchanged
bobbin --json index-bead bo-abc123       # machine-readable result
```

## Options

| Flag | Description |
|------|-------------|
| `--rig <NAME>` | Restrict the lookup to one rig (default: every database in `[beads].databases`) |
| `--force` | Re-embed even when the bead's content hash is unchanged |

## Outcomes

`--json` reports one of four statuses, and the human output says the same thing
in words:

| Status | Meaning |
|--------|---------|
| `indexed` | The bead's content changed (or `--force`); it was re-embedded and replaced. |
| `unchanged` | The content hash matched; nothing was re-embedded. |
| `removed` | The bead is no longer visible in Dolt; its chunk was deleted from the index. |
| `absent` | The bead is in neither Dolt nor the index; nothing to do. |

`removed` is the case worth understanding. A bead that has just been **closed**
is exactly when a post-write trigger fires, and a closed bead does not pass the
default `[beads]` visibility rules. `index-bead` applies the same rules the
batch query does, so such a bead comes back as "not found" and is swept out of
the index — the same end state a full run would reach, arrived at one bead at a
time.

The corollary: `index-bead` never re-admits a bead the batch path filters out.
Asking for a bead by name does not relax `include_closed`, `max_age_days` or
`exclude_labels`.

## Prerequisites

`index-bead` refuses rather than guessing in three cases:

- **No `[beads].databases` configured.** Unlike `bobbin index`, there is no file
  half that could still do useful work, so this is an error and not a silent
  no-op — a hook wired to a misconfigured repo would otherwise report success
  forever while indexing nothing.
- **No index yet.** Run `bobbin index --include-beads` once first. (`bobbin
  init` creates an empty vector directory, so its existence is not evidence
  that an index was ever built.)
- **The embedding model changed.** A full index wipes and rebuilds on a model
  change; a single-bead run cannot, and inserting one row of incompatible
  vectors beside the rest would corrupt search silently. Run `bobbin index
  --force --include-beads` instead.

## Wiring it to a post-write trigger

The beads side of this — a `bd hooks` feature that fires the command after a
write — lives in the beads CLI, not here. Any post-write mechanism works; the
command is idempotent and cheap when nothing changed:

```bash
bobbin index-bead "$BEAD_ID"
```

See [Configuration Reference](../config/reference.md) for the `[beads]` section.
