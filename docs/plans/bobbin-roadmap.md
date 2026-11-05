# Bobbin Roadmap — strategic direction

**Status**: living document. Created 2026-08-21 (strider, from Stiwi's direction).
**Relationship to other docs**: `docs/roadmap.md` is the historical feature
checklist and stays what it is; this file is the strategic half that
`docs/plans/bobbin-debt.md` notes as absent. Debt lives in bobbin-debt.md;
this file only references it. Existing designs are linked, not duplicated:
`docs/plans/quipu-integration.md`, `docs/design/knowledge-aware-bundles.md`,
`docs/plans/backlog.md`, PRD.md Appendix B (relationship vocabulary).

**Direction (Stiwi, 2026-08-21)**: evolve bobbin from a flat chunk index into
a system that (1) relates chunks to each other deterministically, (2) extracts
entities and relationships from documents into the quipu knowledge graph,
(3) ingests sources beyond code and docs — SQL databases, logs, metrics — and
(4) maintains durable mappings from bobbin chunks to quipu graph nodes.

## Execution status (2026-08-21, Stiwi-directed sprint)

One pass executed most of the near-term graph: **W1 complete** (P1 edges +
neighbors tool; P2 edge-aware context leg with `neighbor_budget_pct`; P3
repo-scoped edges; P4 Tests edge). **W2 P1–P4 shipped** (with two P2 corrections dated 2026-08-25 below —
`expand_knowledge` and the orphaned mapping file): camayoc competency
slice `document-structure-and-chunks`; namespace consolidated to the live
aegis scheme on both sides (quipu's dead bobbin.dev constructors deleted;
coupling/PPR/expand_knowledge join the live entity graph); advisory
`bobbin:Chunk` vocabulary in camayoc shapes; `/knot` chunk emitter opt-in
(`quipu_push_chunks`) with a snapshot-support probe — unblocked 2026-08-22:
the quipu pin moved to 0.3.23 rev 37bfc06a, so `replace_snapshot` is real
on the embedded store and the probe passes. W2.P5 → bead bobbin-c79. **W3.A shipped**
(`[index] entities` producer, live-lane IRIs). **W3.B quarantined-track
discipline shipped** (bobbin-15f closed): `InferredExtractor` seam +
deterministic `backtick-coderef/v1` baseline (`knowledge/inferred.rs`),
stamped landing in the camayoc `crew:inferred` plane (rank 0) with
`quipu:derivedBy` + `aegis:sourceKind=inferred`, masquerade guard, envelope
rule on the `knowledge_inferred_extract` MCP surface, graph-routing probe
that refuses stores predating quipu 22b3569; the quipu pin bump
(0.3.23, 2026-08-22) makes live pushes real — the routing probe passes
against the embedded store — and a model-backed extractor is one more impl
of the seam.
**W4 P1–P2 shipped** (ChunkSource seam; beads retrofitted; SQL source);
commits/archives retrofit shipped (bobbin-d5e closed: commits behind a
watermark pipeline on the seam, archives upgraded from full-replace to
content-hash incremental with a removal sweep); P3 (logs/metrics +
`occurred_at` migration batch) remains open.

## Sequencing at a glance

```text
W1.P1 chunk edges (DONE) ──► W1.P2 edge-aware context ──► W1.P3 edge schema batch
                                                              │
W2.P1 camayoc competency file ──► W2.P3 chunk vocabulary ──► W2.P4 /knot emitter ──► W2.P5 reconcile mapping
        W2.P2 namespace consolidation ──┘
W3.A deterministic extraction ──► (needs W2.P3 vocabulary)
W3.B model extraction ──► seam+quarantine DONE; live push unblocked by quipu pin bump (0.3.23, 2026-08-22)
W4.P1 ChunkSource trait ──► W4.P2 SQL source ──► W4.P3 logs+metrics (occurred_at migration batch)
```

Cross-repo gates: W2.P1 (camayoc) gates any new ontology term; quipu plane
routing gates W3 track B; the `occurred_at` chunks-table migration gates
W4.P3 and should absorb every other pending chunks-table change.

## W1 — Chunk graph (deterministic relationships)

**P1 — done (2026-08-21).** `next_chunk` and `part_of` edges emitted for every
indexed file by `src/index/structural.rs` (language-agnostic post-pass:
adjacency from pre-order document order; containment from line-range nesting
for code and markdown blocks, from breadcrumb names for markdown sections,
which tile rather than nest). Read path `get_edges_for_chunk` + MCP tool
`chunk_neighbors`; `id` now on search results; edge counts in `status --json`.
Fixed en route: edges were stored under absolute paths while chunks used
rel paths (no read could ever join — legacy rows swept on index), and stale
edges survived file deletion and zero-edge reparses.

**P2 — edge-aware context assembly.** `ContextAssembler` should pull the
parent section and adjacent chunks for documentation results instead of the
raw-line `full_context` window (neighbor information as edges, not as text).
Measure with the existing eval framework before/after. Acceptance: context
for a doc-heavy query includes parent/adjacent sections; eval scores do not
regress.

**P3 — chunk_edges schema batch + durable chunk identity.** The edges table
has no `repo` column, so identical rel paths in two indexed repos can
cross-contaminate neighbor lookups; adding it is a chunk_edges-only table
drop (cheap: edges rebuild on force reindex, no re-embed). Chunk IDs are
`sha256(path:start:end)` — any line shift re-mints them; the `dangling` field
on `chunk_neighbors` responses is the visible metric. When dangling rates
annoy, add stable ordinal-based identity in the same batch. Acceptance:
multi-repo stores resolve neighbors per repo; dangling rate measured.

**P4 — emit the `Tests` edge.** Declared in `ChunkEdgeType` since its
introduction, never emitted by any collector. Small, self-contained.

## W2 — Durable quipu mapping

**Decision (Stiwi, 2026-08-21): `bobbin:` ≡ `aegis:`** — the live-lane IRI
namespace (`http://aegis.gastown.local/ontology/`) wins. Quipu's
`https://bobbin.dev/ontology#` constants and bobbin's `https://bobbin.dev/`
in `src/knowledge/coupling.rs` are to be retired/ported.

**P1 — camayoc competency file (cross-repo, gates P3).** Camayoc's discipline:
no ontology term without a competency question that needs it. A new
`competency/` file for document structure and chunk retrieval (what document
is this chunk part of? what follows it? which sections cite this symbol?) is
the prerequisite for any chunk vocabulary.

**P2 — namespace consolidation.** Three incompatible spellings exist today:
`coupling.rs` writes `https://bobbin.dev/code/...`; `context.rs`
`expand_knowledge` strips `bobbin:code/` CURIEs (never matches, so the
knowledge-expansion leg very likely returns nothing in production — same
silent-failure class as the PPR path bug, bobbin-jdlkh); quipu
`src/namespace.rs` defines `https://bobbin.dev/ontology#` with full IRI
constructors used only by its reconcile module. Port quipu's constructors
(`code_module_iri`, `code_symbol_iri`, `document_iri`, `section_iri`,
`bundle_iri`, `parse_bobbin_iri`) to the aegis base and fix both bobbin ends.
Also: `bobbin-quipu-mapping.toml` at repo root is orphaned (no loader was
ever written) — implement or delete it. **Deleted 2026-08-25**, see the note
below. Acceptance: one IRI spelling
round-trips bobbin→quipu→bobbin; `expand_knowledge` verifiably returns
entities on a live store.

> **`bobbin-quipu-mapping.toml`, resolved by deletion (2026-08-25).** It had 0
> references in `src/`, `tests/`, the justfile or `build.rs` — no loader was
> ever written — while its own header claimed "Changes here only require a
> config reload, not code changes". That sentence was false in the most
> expensive direction: it invites an operator to retune `show_predicates`,
> `max_depth` or `spotlight_confidence` and conclude from the unchanged output
> that the setting did nothing, rather than that the file is read by nobody.
> The behavior it purports to configure is real but lives elsewhere:
> `config.quipu_endpoint` (`src/config.rs`) is what gates the spotlight call in
> `src/http/handlers/search.rs`, and entity identity comes from `src/iri.rs`,
> not from `[mappings.*]`. A config file that silently does nothing is worse
> than no config file, so it is deleted rather than kept as a placeholder. The
> declarative-mapping design it sketched still stands in
> `docs/design/micro-ui.md`, which is now marked as unimplemented.

> **Correction, 2026-08-25.** The row in the register below was marked ✅ for
> this item while HALF of it was still broken, and the half that was broken is
> the half the acceptance criterion names. `coupling.rs` did move to the aegis
> base at 242b10e; `expand_knowledge` did not. Its parser was ported from the
> `bobbin:code/` CURIE onto `http://aegis.gastown.local/code/` — the
> **superseded** ingest-repos.py lane, measured at 0 live instances, minted by
> nothing in this repo — so the leg went on returning nothing, and its three
> unit tests pinned that dead lane and stayed green over it. Fixed by parsing
> `iri::CODE_BASE`/`DOC_BASE`/`CHUNK_BASE` with yupana's real `::symbol` and
> `#slug` suffixes, with the parser moved next to the minters in `src/iri.rs`
> and a round-trip test (`parses_what_the_minters_mint`) composing the actual
> constructors, so a fourth lane cannot pass. The lesson for the register: a
> ✅ whose acceptance criterion says "verifiably ... on a live store" needs
> that verification, and a unit test written against the same wrong constant
> as the code is not it.

**P3 — chunk vocabulary (after P1).** Mint `bobbin:Chunk`, `nextChunk`, and
an `inDocument`-style part-of predicate. Constraints from camayoc: never
reuse `aegis:contains` (shape-bound to `aegis:Bead` via `sh:targetSubjectsOf`
— it fires on any subject); `aegis:stepOrder` is the ordering precedent;
shapes ship after the emitter, value-constrained first, `minCount` only
after measuring live emitter output (advise-before-enforce).

**P4 — /knot snapshot emitter.** Emit chunk facts as Turtle via quipu
`POST /knot` with `replace_snapshot: true` and a per-repo `snapshot` key —
a reindex becomes a diffed replace where unchanged facts stay live.
`/episode` is unusable for this (rewrites node IRIs into `aegis:` local
names and refuses undeclared prefixes). Chunk IRIs derive from stable
coordinates (repo + path + slug/ordinal), never content hashes or line
numbers. The graph is the index, never the warehouse: chunk bytes stay in
bobbin; the graph holds the reference (`quipu:contentRef` pattern).

**P5 — chunk→entity mapping via the reconcile pattern.** Copy quipu
`src/reconcile/`: write the weak literal form at ingest, resolve to real
`Ref` edges in an idempotent second pass, report Resolved | Dangling |
Ambiguous honestly rather than guessing.

## W3 — Entity extraction (two tracks, in parallel)

Bobbin's `entities` LanceDB table, `Entity` type, and its full
upsert/search/delete API already exist with no producer and no consumer —
the storage layer is waiting for an extractor. `bobbin ontology infer`
(coupling-graph clustering) is prior art for extraction from deterministic
signals.

**Track A — deterministic (observed → canonical).** Symbols, headings, link
targets, bead/commit references extracted by parsers. These are `observed`
facts in camayoc's terms and land canonically (`crew:records` plane,
trust rank 20). Phases: extractor → vocabulary (behind W2.P3) → emitter
(reuses W2.P4).

**Track B — model/NLP (inferred → quarantined).** LLM/NLP-extracted entities
and relationships from doc prose. Non-negotiables from the camayoc/quipu
discipline: own named graph, declared trust chain, `quipu:derivedBy`
recording the extractor and parameters, landing in `crew:inferred`
(trust rank 0), leaving quarantine only through the existing 4-gate
promotion flow. Never written to the root graph, never masquerading as
observed.

**Former blocker for track B (resolved 2026-08-22):** quipu `/knot` used to
hardcode the ROOT graph and silently drop the `graph` key (quipu `src/rdf.rs`).
Strict registered-committed-graph routing landed upstream (quipu 22b3569) and
is in the pinned rev (0.3.23); bobbin's quarantine push probes it with an
unregistered sentinel and the probe passes in-process
(`src/knowledge/quarantine.rs`).

**Envelope rule (both tracks):** bobbin's knowledge-facing responses must
carry plane + trust on the response envelope (camayoc bead camayoc-j4s) —
quarantined material must be visibly quarantined even in a summary line.

## W4 — New sources (SQL, logs, metrics)

**P1 — extract a `ChunkSource` trait.** Five source integrations live as
hardcoded branches in `src/cli/index.rs` (files, PDFs, git, beads, archives),
each re-implementing embed/dedupe/store. `src/index/beads.rs` — already a
non-filesystem (MySQL) source with stable IDs and incremental hashing — is
the template. Extraction is also debt payoff (`index.rs` is allowlisted over
the size gate). Acceptance: existing five sources behind one trait, no
behavior change.

**P2 — SQL databases.** Clone the beads pattern into a config-driven generic
SQL source (`[[sql.sources]]` with connection, query, id column, text
columns). Stable row IDs; no chunks-table schema change needed.

**P3 — logs + metrics (batched migration).** Specified as **`bobbin-5b0`**
(P2, 2026-08-25) — what must batch together, the operator-comms deliverables,
and acceptance criteria. Both need event time: `Chunk.occurred_at`. Any chunks-table column addition is a table drop in
`open_with_dim` — a silent full reindex + re-embed — so this lands as ONE
batched migration event (with any W1.P3 promotions) with explicit operator
comms, not as drive-by columns. Logs: windowed chunking by time, source
discriminator done properly (a `source` column, not more `file_path`
pseudo-namespaces). Metrics: follow camayoc's precedent — store the method,
never the values (catalogue/rules into the KG with content hashes; samples
stay in the TSDB).

## Cross-repo dependency register

| Item | Repo | Blocks | Status |
|---|---|---|---|
| Competency file: document structure & chunk retrieval | camayoc | W2.P3, W3.A vocabulary | ✅ done (competency/document-structure-and-chunks.md) |
| Plane-routed bulk ingress (`/knot` graph key honored) | quipu | W3.B | ✅ done — strict registered-committed-graph targets (quipu 22b3569) |
| Consolidate IRIs to the live aegis scheme | quipu | W2.P2 | ✅ done — dead bobbin.dev constructors deleted; reconcile fixed (quipu ee0c5a6) |
| Retire `https://bobbin.dev/` in coupling.rs / expand_knowledge | bobbin | W2.P2 | ✅ done — coupling.rs at 242b10e; `expand_knowledge` only from 2026-08-25 (see note below) |
| Chunk vocabulary shapes (advise-first) | camayoc | W2.P4 | ✅ done — advisory ChunkShape (camayoc 8cd622e) |
| Quipu dependency pin update (replace_snapshot + shacl) | bobbin | chunk push, W2.P5 | ✅ done 2026-08-22 — pin 0.3.23 rev 37bfc06a, shacl ON, onnx off; probes pass |
| SHACL named-graph type-context gap | quipu | W3.B chunked writes | ✅ done upstream — quipu-080 closed (d0c24b7), in the pinned rev |
| occurred_at chunks-table migration batch | bobbin | W4.P3 | open — **now specified as `bobbin-5b0`** (P2). Blocked on Stiwi's decision on timing, not on any technical dependency: executing it re-embeds every chunk in every deployed store. |

## Non-goals

- Storing chunk content bytes in the quipu graph (graph is the index, never
  the warehouse).
- Model-extracted facts outside quarantine, under any convenience argument.
- A fourth IRI namespace (camayoc bead camayoc-1pd).
- Reusing `aegis:contains` for chunk containment.
