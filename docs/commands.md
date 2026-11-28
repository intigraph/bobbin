# CLI Reference

> **Note:** This file is kept for reference. The authoritative CLI documentation is in [The Bobbin Book](https://intigraph.github.io/bobbin/cli/overview.html).

All commands support these global flags:

| Flag | Description |
|------|-------------|
| `--json` | Output in JSON format |
| `--quiet` | Suppress non-essential output |
| `--verbose` | Show detailed progress |

## `bobbin init [PATH]`

Initialize Bobbin in a repository. Creates a `.bobbin/` directory with configuration, SQLite database, and LanceDB vector store.

```bash
bobbin init              # Initialize in current directory
bobbin init /path/to/repo
bobbin init --force      # Overwrite existing configuration
```

| Flag | Description |
|------|-------------|
| `--force` | Overwrite existing configuration |

## `bobbin index [PATH]`

Build or update the search index. Walks repository files, parses them with Tree-sitter (or pulldown-cmark for Markdown), generates embeddings, and stores everything in LanceDB.

```bash
bobbin index                           # Full index of current directory
bobbin index --incremental             # Only update changed files
bobbin index --force                   # Force reindex all files
bobbin index --repo myproject          # Tag chunks with a repository name
bobbin index --source /other/repo --repo other  # Index a different directory
```

| Flag | Short | Description |
|------|-------|-------------|
| `--incremental` | | Only update changed files |
| `--force` | | Force reindex all files |
| `--repo <NAME>` | | Repository name for multi-repo indexing (default: "default") |
| `--source <PATH>` | | Source directory to index files from (defaults to path) |

## `bobbin search <QUERY>`

Search across your codebase. By default uses hybrid search combining semantic (vector similarity) and keyword (FTS) results using Reciprocal Rank Fusion (RRF).

```bash
bobbin search "error handling"                    # Hybrid search (default)
bobbin search "database connection" --limit 20    # More results
bobbin search "auth" --type function              # Filter by chunk type
bobbin search "auth" --mode semantic              # Semantic-only search
bobbin search "handleAuth" --mode keyword         # Keyword-only search
bobbin search "auth" --repo myproject             # Search within a specific repo
```

| Flag | Short | Description |
|------|-------|-------------|
| `--type <TYPE>` | `-t` | Filter by chunk type (function, method, class, struct, enum, interface, module, impl, trait, doc, section, table, code_block) |
| `--limit <N>` | `-n` | Maximum results (default: 10) |
| `--mode <MODE>` | `-m` | Search mode: `hybrid` (default), `semantic`, or `keyword` |
| `--repo <NAME>` | `-r` | Filter to a specific repository |

**Search modes:**

| Mode | Description |
|------|-------------|
| `hybrid` | Combines semantic + keyword using RRF (default) |
| `semantic` | Vector similarity search only |
| `keyword` | Full-text keyword search only |

## `bobbin context <QUERY>`

Assemble task-relevant context from search results and git history. Searches for code matching your query, then expands results with temporally coupled files (files that change together in git history). Outputs a context bundle optimized for feeding to AI agents or for understanding a task's scope.

```bash
bobbin context "fix the login bug"                   # Default: 500 line budget
bobbin context "refactor auth" --budget 1000          # Larger context budget
bobbin context "add tests" --content full             # Include full code content
bobbin context "auth" --content none                  # Paths/metadata only
bobbin context "auth" --depth 0                       # No coupling expansion
bobbin context "auth" --repo myproject --json         # JSON output for a specific repo
```

| Flag | Short | Description |
|------|-------|-------------|
| `--budget <LINES>` | `-b` | Maximum lines of content to include (default: 500) |
| `--content <MODE>` | `-c` | Content mode: `full`, `preview` (default for terminal), `none` |
| `--depth <N>` | `-d` | Coupling expansion depth, 0 = no coupling (default: 1) |
| `--max-coupled <N>` | | Max coupled files per seed file (default: 3) |
| `--limit <N>` | `-n` | Max initial search results (default: 20) |
| `--coupling-threshold <F>` | | Min coupling score threshold (default: 0.1) |
| `--repo <NAME>` | `-r` | Filter to a specific repository |

The context bundle includes:
- **Direct matches**: Code chunks matching your query, ranked by relevance
- **Coupled files**: Files with shared commit history to the direct matches
- **Budget tracking**: How many lines were used out of the budget
- **File metadata**: Paths, chunk types, line ranges, relevance scores

## `bobbin grep <PATTERN>`

Keyword and regex search using LanceDB full-text search.

```bash
bobbin grep "TODO"
bobbin grep "handleRequest" --ignore-case
bobbin grep "fn.*test" --regex                   # Regex post-filter
bobbin grep "TODO" --type function --context 2   # With context lines
bobbin grep "auth" --repo myproject
```

| Flag | Short | Description |
|------|-------|-------------|
| `--ignore-case` | `-i` | Case insensitive search |
| `--regex` | `-E` | Use extended regex matching (post-filters FTS results) |
| `--type <TYPE>` | `-t` | Filter by chunk type |
| `--limit <N>` | `-n` | Maximum results (default: 10) |
| `--context <N>` | `-C` | Number of context lines around matches (default: 0) |
| `--repo <NAME>` | `-r` | Filter to a specific repository |

## `bobbin related <FILE>`

Find files that are temporally coupled to a given file -- files that frequently change together in git history.

```bash
bobbin related src/auth.rs
bobbin related src/auth.rs --limit 20
bobbin related src/auth.rs --threshold 0.5   # Only strong coupling
```

| Flag | Short | Description |
|------|-------|-------------|
| `--limit <N>` | `-n` | Maximum results (default: 10) |
| `--threshold <F>` | | Minimum coupling score (default: 0.0) |

## `bobbin history <FILE>`

Show commit history and churn statistics for a file.

```bash
bobbin history src/main.rs
bobbin history src/main.rs --limit 50
bobbin history src/main.rs --json        # JSON output with stats
```

| Flag | Short | Description |
|------|-------|-------------|
| `--limit <N>` | `-n` | Maximum entries to show (default: 20) |

Output includes:
- Commit date, author, and message for each entry
- Referenced issue IDs (if present in commit messages)
- Statistics: total commits, churn rate (commits/month), author breakdown

## `bobbin status`

Show index statistics.

```bash
bobbin status
bobbin status --detailed              # Per-language breakdown
bobbin status --repo myproject        # Stats for a specific repo
```

| Flag | Short | Description |
|------|-------|-------------|
| `--detailed` | | Show per-language breakdown |
| `--repo <NAME>` | `-r` | Stats for a specific repository only |

## `bobbin serve [PATH]`

Start an MCP (Model Context Protocol) server, exposing Bobbin's search and analysis capabilities to AI agents like Claude and Cursor.

```bash
bobbin serve                # Serve current directory
bobbin serve /path/to/repo  # Serve a specific repository
```

**MCP tools exposed:**

| Tool | Description |
|------|-------------|
| `search` | Semantic/hybrid/keyword code search |
| `grep` | Pattern matching with regex support |
| `context` | Task-aware context assembly |
| `related` | Find temporally coupled files |
| `read_chunk` | Read a specific code chunk with context lines |

See [AI Agent Integration](../README.md#ai-agent-integration) for MCP configuration examples.

## `bobbin feedback <SUBCOMMAND>`

Submit and review quality feedback on bobbin context injections. Every injection includes an `injection_id` (e.g. `inj-01KJ...`) — use this to reference which injection you're rating.

```bash
bobbin feedback submit --injection inj-abc123 --rating useful --reason "foo.rs was the exact file"
bobbin feedback list --limit 20 --rating noise
bobbin feedback show inj-abc123
bobbin feedback stats
```

### feedback submit

Submit a quality rating for an injection.

| Flag | Description |
|------|-------------|
| `--injection <ID>` | Injection ID to rate (required) |
| `--rating <RATING>` | Rating: `useful`, `noise`, or `harmful` (required) |
| `--reason <TEXT>` | Free-text explanation |
| `--chunks <IDS>` | Comma-separated chunk IDs referenced |

### feedback list

List recent feedback records.

| Flag | Description |
|------|-------------|
| `--limit <N>` | Maximum results (default: 20) |
| `--rating <RATING>` | Filter by rating |
| `--agent <NAME>` | Filter by agent |

### feedback show

Show feedback and injection details for a specific injection ID.

```bash
bobbin feedback show inj-abc123
```

### feedback stats

Show aggregated feedback statistics: injection count, feedback coverage, rating distribution.

```bash
bobbin feedback stats
```

## Supported Languages

Bobbin uses Tree-sitter for structure-aware parsing, and pulldown-cmark for Markdown:

| Language | Extensions | Extracted Units |
|----------|------------|-----------------|
| Rust | `.rs` | functions, impl blocks, structs, enums, traits, modules |
| TypeScript | `.ts`, `.tsx` | functions, methods, classes, interfaces |
| Python | `.py` | functions, classes |
| Go | `.go` | functions, methods, type declarations |
| Java | `.java` | methods, constructors, classes, interfaces, enums |
| C++ | `.cpp`, `.cc`, `.hpp` | functions, classes, structs, enums |
| Markdown | `.md` | sections, tables, code blocks, YAML frontmatter |

Other file types fall back to line-based chunking (50 lines per chunk with 10-line overlap).
