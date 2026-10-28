---
title: prime
description: Show LLM-friendly project overview with live stats
tags: [cli, prime, overview]
status: draft
category: cli-reference
related: [cli/status.md, mcp/tools.md]
commands: [prime]
feature: prime
source_files: [src/cli/prime.rs]
---

# prime

Show LLM-friendly project overview with live stats.

## Synopsis

```bash
bobbin prime [OPTIONS] [PATH]
```

## Description

The `prime` command prints the same single-source primer used by the MCP and
HTTP prime tools, followed by a bounded read-only summary of the local index.
It opens an existing `chunks.lance` table directly: no schema migration,
compaction, model setup, config writes or command metrics. Missing or unreadable
indexes still print the primer and exit 0. Prime is local: it does not use
`--server`, `BOBBIN_SERVER`, or a configured remote server.

`--brief` prints five to ten lines; `--static` skips index reads entirely.
`--section` selects a heading by case-insensitive prefix. Unknown sections and
bad flags exit 2. The older `commands`, `key commands` and `what bobbin does`
section names remain aliases.

The live read has a 550 ms deadline and scans at most 2048 rows for file counts,
languages and indexed timestamps. The total chunk count comes from the existing
Lance table metadata. A capped scan or deadline reports `partial`; global file
counts and last-indexed time remain null when the scan is incomplete. Sampled
values are labeled separately. Index age is not a comparison against git HEAD
or proof that working-tree changes have been indexed. No network server is
contacted by this local command.

`--json` uses `stack-prime/1`, with `tool`, `version`, `primer.sections`,
`primer.text`, `live.{status,elapsed_ms,summary}` and overall `status`.
`--brief --json` omits `primer.text`. `--static` reports live `skipped` and an
empty summary. Overall status is `ok` for live `ok` or `skipped`, `degraded`
otherwise. This replaces the older CLI `{primer,stats,initialized}` JSON;
the MCP and HTTP response envelopes retain their existing shape.

## Static primer

{{#include ../../../../docs/primer.md}}

## Arguments

| Argument | Description |
|----------|-------------|
| `PATH` | Directory to check (default: `.`) |

## Options

| Option | Default | Description |
|--------|---------|-------------|
| `--brief` | | Show brief (compact) overview only |
| `--section <NAME>` | | Select a section by heading prefix |
| `--static` | | Skip all index reads |
| `--json` | | Print the shared stack-prime/1 envelope |

## Examples

Show full project overview:

```bash
bobbin prime
```

Brief summary:

```bash
bobbin prime --brief
```

Specific section:

```bash
bobbin prime --section "mcp tools"
```

JSON output:

```bash
bobbin prime --json
```

## Prerequisites

Works best with a bobbin index for live stats, but can run without one.

## See Also

- [status](status.md) — index statistics
