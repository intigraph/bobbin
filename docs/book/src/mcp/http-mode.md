---
title: HTTP Mode
description: Running bobbin as an HTTP REST API server for centralized deployments
tags: [mcp, http, server, thin-client]
status: draft
category: mcp
related: [mcp/overview.md, cli/serve.md, config/reference.md]
---

# HTTP Mode

Bobbin can run as an HTTP REST API server for centralized deployments, shared team use, or webhook-driven indexing.

## Starting the HTTP Server

```bash
bobbin serve --http                  # HTTP on port 3030 (default)
bobbin serve --http --port 8080      # Custom port
bobbin serve --http --mcp            # HTTP + MCP stdio simultaneously
```

The server binds to `0.0.0.0` on the specified port and includes CORS headers for browser-based clients.

## REST API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/search` | Semantic/hybrid/keyword search |
| `GET` | `/grep` | Keyword/regex pattern search |
| `GET` | `/context` | Task-aware context assembly |
| `GET` | `/chunk/{id}` | Read a specific chunk by ID |
| `GET` | `/read` | Read file lines by path and range |
| `GET` | `/related` | Find temporally coupled files |
| `GET` | `/refs` | Find symbol definitions and usages |
| `GET` | `/symbols` | List symbols in a file |
| `GET` | `/deps` | Import dependencies for a file |
| `GET` | `/history` | Git commit history for a file |
| `GET` | `/hotspots` | Identify high-churn complex files |
| `GET` | `/impact` | Predict change impact |
| `GET` | `/review` | Diff-aware review context |
| `GET` | `/similar` | Find similar code or duplicate clusters |
| `GET` | `/prime` | Project overview with live stats |
| `GET` | `/beads` | Search indexed beads/issues |
| `GET` | `/status` | Index statistics |
| `GET` | `/metrics` | Prometheus metrics |
| `POST` | `/webhook/push` | Trigger re-indexing (for CI/CD) |

### GET /search

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `q` | string | required | Search query |
| `mode` | string | `hybrid` | Search mode: `hybrid`, `semantic`, `keyword` |
| `type` | string | all | Filter by chunk type |
| `limit` | integer | 10 | Maximum results |
| `repo` | string | all | Filter by repository |
| `spotlight` | bool | `true` | Include optional Quipu entity annotations. Set `false` for a search-only health probe; retrieval and hit validation still run. |

```bash
curl "http://localhost:3030/search?q=error+handling&limit=5"
```

### GET /grep

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `pattern` | string | required | Search pattern |
| `ignore_case` | bool | false | Case-insensitive search |
| `regex` | bool | false | Enable regex matching |
| `type` | string | all | Filter by chunk type |
| `limit` | integer | 10 | Maximum results |
| `repo` | string | all | Filter by repository |

```bash
curl "http://localhost:3030/grep?pattern=handleAuth&limit=5"
```

### GET /context

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `q` | string | required | Task description |
| `budget` | integer | 500 | Max lines of content |
| `depth` | integer | 1 | Coupling expansion depth |
| `max_coupled` | integer | 3 | Max coupled files per seed |
| `limit` | integer | 20 | Max initial search results |
| `coupling_threshold` | float | 0.1 | Min coupling score |
| `repo` | string | all | Filter by repository |

```bash
curl "http://localhost:3030/context?q=refactor+auth+flow&budget=300"
```

### GET /read

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `file` | string | required | File path (relative to repo root) |
| `start_line` | integer | required | Start line number |
| `end_line` | integer | required | End line number |
| `context` | integer | 0 | Extra context lines before/after |

```bash
curl "http://localhost:3030/read?file=src/main.rs&start_line=1&end_line=20"
```

### GET /related

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `file` | string | required | File path to find related files for |
| `limit` | integer | 10 | Maximum results |
| `threshold` | float | 0.0 | Min coupling score |

```bash
curl "http://localhost:3030/related?file=src/auth.rs&limit=5"
```

### GET /refs

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `symbol` | string | required | Symbol name to find |
| `type` | string | all | Filter by symbol type |
| `limit` | integer | 20 | Max usage results |
| `repo` | string | all | Filter by repository |

```bash
curl "http://localhost:3030/refs?symbol=parse_config"
```

### GET /symbols

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `file` | string | required | File path |
| `repo` | string | all | Filter by repository |

```bash
curl "http://localhost:3030/symbols?file=src/config.rs"
```

### GET /deps

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `file` | string | required | File path |
| `reverse` | boolean | `false` | Show what imports this file instead |
| `both` | boolean | `false` | Show both directions |
| `role` | string | none | Role for access filtering |

`imports` and `imported_by` are **omitted, not empty**, when a direction was
not asked for — so "not asked" stays distinguishable from "none found".

```bash
curl "http://localhost:3030/deps?file=src/config.rs&both=true"
```

### GET /history

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `file` | string | required | File path |
| `limit` | integer | 20 | Maximum commits |
| `role` | string | none | Role for access filtering |

Returns the commits plus a `stats` block: total commits, per-author counts, and
`churn_rate` (commits per 30 days over the returned window). `churn_rate` is
`0.0` when fewer than two commits were returned — one commit gives no interval
to measure.

```bash
curl "http://localhost:3030/history?file=src/config.rs&limit=10"
```

### GET /hotspots

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `since` | string | `1 year ago` | Time window for churn analysis |
| `limit` | integer | 20 | Maximum results |
| `threshold` | float | 0.0 | Min hotspot score |

```bash
curl "http://localhost:3030/hotspots?since=6+months+ago&limit=10"
```

### GET /impact

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `target` | string | required | File or file:function target |
| `depth` | integer | 1 | Transitive depth (1-3) |
| `mode` | string | `combined` | Signal: `combined`, `coupling`, `semantic`, `deps` |
| `limit` | integer | 15 | Maximum results |
| `threshold` | float | 0.1 | Min impact score |
| `repo` | string | all | Filter by repository |

```bash
curl "http://localhost:3030/impact?target=src/auth.rs&depth=2"
```

### GET /review

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `diff` | string | `unstaged` | Diff spec: `unstaged`, `staged`, `branch:<name>`, or commit range |
| `budget` | integer | 500 | Max lines of content |
| `depth` | integer | 1 | Coupling expansion depth |
| `repo` | string | all | Filter by repository |

```bash
curl "http://localhost:3030/review?diff=staged&budget=300"
```

### GET /similar

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `target` | string | - | Chunk ref or text (required unless `scan=true`) |
| `scan` | bool | false | Scan for duplicate clusters |
| `threshold` | float | 0.85/0.90 | Min similarity threshold |
| `limit` | integer | 10 | Max results or clusters |
| `repo` | string | all | Filter by repository |
| `cross_repo` | bool | false | Cross-repo comparison in scan mode |

```bash
curl "http://localhost:3030/similar?target=src/auth.rs:login&limit=5"
curl "http://localhost:3030/similar?scan=true&threshold=0.9"
```

### GET /prime

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `section` | string | - | Specific section to show |
| `brief` | bool | false | Compact overview only |

```bash
curl "http://localhost:3030/prime?brief=true"
```

### GET /beads

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `q` | string | required | Search query |
| `priority` | integer | - | Filter by priority (1-4) |
| `status` | string | - | Filter by status |
| `assignee` | string | - | Filter by assignee |
| `rig` | string | - | Filter by rig name |
| `limit` | integer | 10 | Maximum results |
| `enrich` | bool | true | Enrich with live Dolt data |

```bash
curl "http://localhost:3030/beads?q=auth+bug&status=open"
```

### GET /status

Returns index statistics in JSON format.

```bash
curl http://localhost:3030/status
```

### GET /metrics

Returns Prometheus-compatible metrics.

```bash
curl http://localhost:3030/metrics
```

### POST /webhook/push

Triggers re-indexing. Useful for CI/CD pipelines or git webhook integrations.
The request must carry Forgejo/Gitea's `X-Gitea-Signature`: the lowercase hex
HMAC-SHA256 of the exact request body under `[archive].webhook_secret`.

```bash
body='{"ref":"refs/heads/main","repository":{"full_name":"owner/repo"}}'
signature=$(printf %s "$body" | openssl dgst -sha256 -hmac "$WEBHOOK_SECRET" -hex | awk '{print $2}')
curl -X POST http://localhost:3030/webhook/push \
  -H "X-Gitea-Signature: $signature" -H 'Content-Type: application/json' -d "$body"
```

## Thin-Client Mode

When a remote bobbin server is running, CLI commands can delegate to it instead of accessing local storage:

```bash
bobbin search "auth" --server http://localhost:3030
bobbin status --server http://localhost:3030
```

The `--server` flag is a global option available on all commands. When set, the CLI acts as a thin client that forwards requests to the HTTP server.

## Use Cases

### Team Server

Run bobbin on a shared machine with a large codebase indexed once:

```bash
# On the server
bobbin init
bobbin index
bobbin serve --http --port 3030
```

Team members connect via `--server` flag or configure their AI client to point at the server.

### CI/CD Integration

Add a webhook step to your CI pipeline to keep the index fresh:

```bash
# After deploy or merge
curl -X POST http://bobbin-server:3030/webhook/push
```

### Combined Mode

Run both HTTP and MCP simultaneously for maximum flexibility:

```bash
bobbin serve --http --mcp
```

This starts the HTTP server on the configured port and the MCP stdio server concurrently. The MCP server handles AI client connections while the HTTP server handles REST API requests and webhooks.
