# L0 bounded index reuse

The original six-hour run is still partial: 330 of 1,320 cells, ten of forty
registered tasks, and exit 124. Its rows and runtime manifest are unchanged.
This opt-in source change does not start or resume that campaign and does not
establish full-cohort quality or six-hour feasibility.

## What changes

The normal L0 path remains available. `--reuse-model-dir` enables a stricter,
run-local path. Each task builds one full index, seals its inputs and artifacts,
and derives an independent no-coupling copy before any hook runs. This removes
the second native index/embedding pass; it does not share a mutable index across
arms or reuse one commit's index for a different commit. All other arms keep the
registered calibration and line budgets.

The copy clears `coupling`, `cross_repo_coupling`, and their coupling watermarks.
It rebinds only the verified `repo_source:<name>` entry to the new private source
path and disables `git.coupling_enabled`. Vector bytes and unrelated payloads
remain identical. The two directories have the **same basename**: both native
index labels and hook affinity depend on it. Copies use independent files, never
hard links. The source must remain sealed and unchanged through the copy.

The adapter currently supports only the frozen Linux native runtime:

- Bobbin 0.28.0, source `843dc42bae0cfd39718d29eaecb3b12fe6e8dbfe`;
- executable SHA256 `9bebd2fad732dc20ae614d6d2df6829d480e5306b0cc6a7b28653c7b0ec69721`;
- CPU ONNX `all-MiniLM-L6-v2`, with the original model/tokenizer hashes in
  `runner/index_reuse.py`.

A version label, wrapper script or another native build is insufficient. Add
another runtime only with cold/derived native equivalence and refusal controls.
The installed 0.29.0 is **not** silently enrolled. No model provider is called;
the exact complete model directory must already occupy the isolated native
`XDG_CACHE_HOME/bobbin/models/<name>` cache.

## Refusal and accounting

A seal binds the checked-out commit, tracked and untracked source bytes/modes,
configuration, executable, all model files and every index payload file. Missing,
stale (over six hours), future, corrupt, linked, changed or unknown artifacts
refuse without a fallback rebuild. Model/runtime admission runs **before** the
native index, and the identities must still match afterward. A prior successful
search can add `metrics.jsonl`; that mutation invalidates the seal too. Derive
before querying, rather than editing or ignoring the seal.

Each opt-in cell archives its mutable hook/session state and restores the exact
sealed baseline before another cell. The frozen hook can reuse a reducing ledger
even with `--no-dedup`; a private repeated-call falsifier exposed that contamination.
Successful restoration is verified against the complete seal. An error, unexpected
immutable mutation or failed restoration quarantines the workspace and stops the
run; its state remains available, and following cells remain missing. Journals
retain each cell's feedback, ledger, calibration and metrics rather than discarding
them. Legacy mode retains its existing behavior.

The frozen indexer also writes coupling edges into its embedded Quipu database,
independently of the HTTP push flags. Derivation removes that database only from
the new private copy, under an exact supported layout; the source remains intact.
Unknown layouts and entity/document/multimodal indexing profiles refuse. Clearing
only the SQLite coupling table would leave this additional graph channel behind.

The opt-in command requires an empty new workspace and new score output. It
never overwrites the original partial result. Its manifest enumerates every
planned `(task, arm, budget)` key. `matrix-coverage.json` separately records all
missing cells and the explicit injected/skipped/error totals; it does not turn
errors into zero scores. Duplicate/unplanned cells refuse. `index-costs.jsonl`
keeps cold native-index and derived-copy work separate.

Retain the original enclosing CPU 400%, memory 6 GiB, swap zero and 21,600-second
wall supervisor. The opt-in command checks the cgroup-v2 CPU/memory/swap values;
it does not create a supervisor or relax the wall limit. The index subprocess
now drains both output pipes under its timeout and kills/reaps its own POSIX
process group on expiry. No completion seal is published on failure. This
process-group guarantee is not claimed on non-POSIX systems.

Artifact/model inventory is capped at 512 MiB and 100,000 files per inventory.
The retained partial campaign's 21 built or partial indexes totalled
6,474,663,440 bytes; the largest was 450,579,646 bytes. Thus the conservative
512-MiB per-index bound accommodates those measured artifacts, not every
unmeasured repository. Copy admission also requires payload size plus 16 MiB
free disk. This is **not** an aggregate campaign disk quota. Source clones,
repository mirrors, models, both arm copies and retained results need a separate
measured whole-run disk budget before a new campaign is authorized.

For an already authorized fresh campaign, use the existing L0 invocation inside
that original supervisor and add `--reuse-model-dir` pointing to the pinned
private model directory. Pin the real native executable, not an indirect shell
wrapper. No example here launches the forty-task campaign automatically.

## Measured private controls

A two-file, three-commit fixture used the original resource limits and frozen
runtime. Full indexing took 2.169 seconds; cold no-coupling indexing took
2.147 seconds. One isolated derivation took 0.270 seconds, with zero native
index calls and 333,160 payload bytes. Initial full sealing and other campaign
costs are not included in those native timings; do not extrapolate total run
speed from them. Combined memory peak for the initial paired probe was
873,955,328 bytes.

The full index had one coupling row (positive control); derived and cold copies
had none. All five semantic search results matched. The actual injection hook,
with its gate explicitly disabled as a positive control, injected the same two
source chunks from cold and derived copies. The full source stayed sealed.
A fresh paired control then repeated the same hook twice on both arms under
cell isolation: both repetitions injected identical chunks, both no-coupling
indexes lacked the embedded coupling database, and the full source stayed sealed.
That derivation took 0.277 seconds and cold indexing 1.571 seconds; these are
separate probes, not an aggregate speedup. These are synthetic private controls,
not efficacy or full-corpus measurements.

The old completion-marker callback accepted a corrupted index without rebuilding
or refusing. The new seal rejects that same kind of mutation. The old index
reader also returned success after a one-second fake process despite a
0.1-second timeout; a second control found a surviving grandchild. The new
controls prove prompt refusal and no late child write. No model or network is
used in these timeout tests.

The frozen forty task YAML digests are unchanged. Unit controls enumerate all
1,320 unique planned keys and exercise complete/partial/error/skip/duplicate
accounting synthetically. The actual new full campaign, measured whole-run
resources, outcome publication and issue 95 acceptance remain open milestones.

Rollback: revert the source commit, retain all immutable prior/new results, and
do not reinterpret a partial run as complete. No production indexing, release,
resource increase, timer or service change is part of this source delivery.

A paired scoring probe found that the retained native hook emits repository-
qualified paths such as `repos/fixture/app.py`, while the existing scorer matches
paths literally against Git-relative gold paths. Legacy scoring and original
scores stay unchanged; the private hook-equivalence controls compare native
chunks directly, not claimed recall improvements. The opt-in correction below
is paired against those same chunks before any full-cohort interpretation.

The opt-in scorer now resolves that path caveat with exact workspace ownership:
only a literal existing file or the exact current `repos/<workspace-name>/`
prefix can match. Exactly one nonlinked candidate is required. Ambiguity,
unknown/foreign-only paths and traversal refuse as an error. No gold-dependent
matching is used; normalization is applied before baseline character counting,
while actual hook character accounting is preserved. The manifest records
`workspace-qualified-or-literal/v1`; legacy scoring and old rows stay unchanged.
The paired private instrument probe checks the same retained actual native chunks,
and driver controls verify recall 0 before versus 1 with the opt-in mapping.
This repairs interpretation of paths, not a measured improvement in retrieval.

Path qualification uses the workspace that produced the current arm’s chunks.
The no-coupling copy is checked independently: an original-workspace file cannot
satisfy a missing derived-workspace file, even when both directories share a name.

An unreadable candidate is an explicit error, not evidence of absence. Strict
metadata reads refuse instead of treating an inaccessible alternative as a
unique path match.
