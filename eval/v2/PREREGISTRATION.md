# Bobbin eval v2 — design and preregistration

Status: **preregistered, first pass approved.** This file is committed before any v2 run.
Anything below that changes after the first run is recorded in the amendment log at the end,
with the date and the reason. An analysis not listed here is reported as exploratory.

## Why v2 exists

The v1 study (`docs/paper-context-injection.md`, `docs/book/src/eval/summary.md`) cannot carry
a public claim, and the reasons are design problems, not sample-size problems:

1. **Underpowered arms.** 3-4 runs per arm against a per-run SD of 0.347 F1. No ablation survives
   Holm-Bonferroni.
2. **Baseline not model-matched.** One baseline run used a stronger model. Model-matched, three of
   the six ablation effects change sign.
3. **Tool/injection confound.** Most of the "with bobbin" gain (+0.287 F1) appears once the agent
   has a search tool; automatic injection adds about +0.025. v1 never separated the two.
4. **Proxy primary metric.** File-level F1 against the gold patch measures "touched the right
   files", not "solved the task". The published test pass rate went *down* with bobbin (66.7% to
   58.3%) and was not explained.
5. **One ablation arm was mislabelled.** `gate_threshold=1.0` skips injection entirely; it is a
   third baseline, not a gating ablation.

v2 fixes these by construction and does the cheap, deterministic measurement first.

## Layers

| layer | what | agent? | cost | status |
|---|---|---|---|---|
| **L0** | offline retrieval: run the production injection path on the task prompt alone, score what it would inject | no | seconds per task | approved (full task set, all arms) |
| **L1** | agent study, paired 2x2 {search tool} x {auto injection} | yes | ~$1.4 per run (v1 mean, to be re-measured) | **pilot only** approved; full study needs a separate OK after the pilot |
| **L2** | widen the task set with a SWE-bench Verified subset | — | — | later |

## Task set

The 40 tasks in `eval/tasks/*.yaml` (5 each from cargo, django, go, nushell, pandas, polars,
ruff, typst). `eval/tasks/_quarantined/` is excluded. Each task pins a fixing commit; the gold
patch is `git diff <commit>^ <commit>`.

**Gold definitions (fixed now):**

- **gold files**: files the fixing commit modifies that exist at `<commit>^` (a file the commit
  *creates* is not in the tree the agent or the retriever sees, so it cannot be retrieved and is
  not gold), excluding test files (`test`/`tests` path
  components or `_test`/`test_` file-name affixes), lockfiles and changelogs. Test files are
  excluded because the hidden tests are the grader, not context an agent should be shown.
- **gold hunks**: for each gold file, the *pre-image* line ranges of the fixing commit's hunks
  (the lines as they exist at `<commit>^`, which is the tree the agent sees). A pure insertion
  contributes the one pre-image line it follows.
- **gold lines**: the union of the gold hunks' line ranges.

A task whose gold file set is empty after exclusion is dropped from L0 and reported by id.

## Contamination

Every task is a public fix in a public repository, so a model may have seen the fix, or text about it, in training. This is recorded rather than assumed away:

- **Fix dates are fixed now.** `eval/v2/task-fix-dates.tsv` holds each task's fixing-commit
  committer date, from the GitHub API. Range: 2025-06-03 to 2026-02-15, 22 of 40 in 2026.
- **The model's training cutoff comes from the model provider's published documentation**, is
  written into each L1 run manifest, and is never inferred from model behaviour. A task whose fix
  predates the cutoff is flagged `possibly-seen`.
- **Primary L1 analyses use all tasks, as registered.** In addition, if at least 10 tasks postdate
  the cutoff, the primary L1 outcome is repeated on that subset as a sensitivity analysis. If fewer
  than 10 do, the report states that contamination cannot be excluded for this task set, and no
  claim is framed as out-of-distribution.
- **L0 cannot be contaminated.** It involves no model; retrieval is deterministic over the
  checked-out tree.
- **Our own public repositories** (bobbin, quipu and the rest of the stack) are **excluded from
  every confirmatory task set.** Their fixes, docs and discussions are public and written with the
  same tools under test, so a task drawn from them fails both contamination and independence. None
  of the 40 current tasks uses them; L2 must keep it that way.
- L2's SWE-bench Verified subset carries the same date rule; its public prominence makes
  exposure more likely, and that is stated wherever L2 results appear.

## L0 — offline retrieval

### Procedure

For each task: clone the repo, check out `<commit>^`, `bobbin init` + `bobbin index` (identical
settings for every arm), then for each arm run **`bobbin hook inject-context --no-dedup`** with
the task description as the prompt. This is the exact code path the UserPromptSubmit hook runs
for an agent, not the looser `bobbin context` command, so L0 scores what an agent would actually
be shown. The injected text is parsed into `(path, start_line, end_line)` chunks from its
per-chunk headers.

**How the arms are applied, and why.** The hook resolves its tuning in the order
`.bobbin/calibration.json` > `.bobbin/config.toml` > intent adjustments, and blame bridging
(`bridge_mode`) can *only* be set through the calibration file. So every arm, `full` included,
writes a complete `calibration.json` whose values are the shipped defaults (`semantic_weight`
0.9, `doc_demotion` 0.3, `rrf_k` 60, `recency_weight` 0.3, `bridge_mode` inject) with only the
ablated knob changed. All arms therefore take the same precedence path, and none can inherit a
stale calibration from the index step. Two knobs cannot go through that file:

- the quality gate is a hook flag, and the hook adds an intent-dependent boost to it, so a base of
  0.0 can still gate. The ablation passes `--gate-threshold=-1`, which no similarity can fail;
- temporal coupling is computed when indexing and is not read at query time, so `-coupling` uses
  a second index of the same checkout built with `git.coupling_enabled = false`.

Every arm is held to the **same line budget, 300** (the shipped default). Budget sensitivity is
reported at 100 / 300 / 600 as secondary.

### Arms

Full pipeline (defaults) is the reference arm. Every comparison is paired by task.

| arm | change from defaults | question |
|---|---|---|
| `full` | none | reference |
| `-semantic` | calibration `semantic_weight = 0.0` | does semantic retrieval carry weight? |
| `-coupling` | separate index, `git.coupling_enabled = false` | temporal coupling expansion |
| `-blame` | calibration `bridge_mode = off` | git blame bridging |
| `-recency` | calibration `recency_weight = 0.0` | recency boost |
| `-docdemote` | calibration `doc_demotion = 0.0` | doc demotion |
| `-gate` | `--gate-threshold=-1` | **the real gating ablation** (v1's 1.0 arm was not one) |
| `rg` | ripgrep over prompt keywords, files ranked by hit count, hit lines ±10 until the budget | keyword baseline |
| `bm25` | BM25 over 40-line windows of every indexed file, top windows until the budget | lexical baseline |
| `embed` | bobbin semantic search only, top chunks until the budget, no expansion or gating | embedding top-k baseline |
| `repomap` | per-file symbol outline (bobbin's parsed symbols), files ranked by prompt-term overlap, until the budget | repo-map-style baseline |

The `repomap` arm is a repo-map-*style* baseline built from bobbin's own parse, not Aider's
implementation (which ranks with PageRank over a reference graph). It is reported under that name
and must not be described as "Aider".

The Jev arms (J1 rerank, J2 knapsack + semantic dedup, J3 should-inject) are scored on this same
harness under their own bead once they exist. They are not part of this registration's
confirmatory family.

### Metrics (per task, per arm)

- **hunk recall@B** — fraction of gold hunks overlapped by at least one injected line.
- **file recall@B** — fraction of gold files with at least one injected chunk.
- **file precision** — fraction of injected files that are gold files.
- **density** — gold lines injected / total lines injected. An arm that injects nothing has
  undefined density; it is reported as a count and excluded from the density mean (not scored 0).
- **injected lines** and **injected tokens** (chars / 4).
- **injected?** — whether the arm injected anything at all.

### Confirmatory analysis (L0)

- **Primary L0 metric: hunk recall@300.** Co-primary: **density@300.**
- For each of the 6 ablation arms and 4 baselines: the mean paired difference from `full` over
  tasks, with a 95% bootstrap CI (10,000 resamples of tasks, fixed seed 20260924) and a paired
  Wilcoxon signed-rank p-value.
- **Holm correction across the 10 comparisons**, separately for each co-primary metric.
- A method "carries weight" when removing it lowers hunk recall with a Holm-adjusted p < 0.05.
- Everything else in the L0 tables (file metrics, budget sensitivity, per-repo splits) is
  secondary and reported without a significance claim.

**Gate for any change to the shipped hook** (including Jev J1–J3): a candidate must improve
density at 300 with a Holm-adjusted p < 0.05 **and** be non-inferior on hunk recall, with the
lower 95% bound of its recall difference above −0.02.

## L1 — agent study (paired 2x2)

### Design

Every task runs in all four cells; the analysis is paired by task, so task difficulty cancels.

| cell | bobbin search tool available | automatic injection hook |
|---|---|---|
| `none` | no | no |
| `tool` | yes | no |
| `inject` | no | yes |
| `both` | yes | yes |

- **One pinned model per study**, recorded per run, with mixed-model aggregation refused by the
  harness (it already does this since `e241a8c`). Pilot model: **`claude-sonnet-5`**.
- Identical prompts, turn limits, timeouts and repo checkouts across cells. Cell order is
  randomised per task with seed 20260924.

### Outcomes

- **Primary: task success** — the task's hidden `test_command` passes. A run in which zero tests
  execute is a failure, not a pass (`16300d3`).
- Secondary: file F1 against the gold files, input and output tokens, turns, dollar cost,
  turns until the first edit of a gold file.

### Pilot (approved)

- **30 tasks x 4 cells x 1 rep = 120 runs**, `claude-sonnet-5`. The 30 are chosen by seeded
  shuffle (seed 20260924) stratified to keep all 8 repos represented, and are fixed in
  `eval/v2/pilot-tasks.txt` (committed with this file). Procedure: order the 40 task ids, shuffle
  with `random.Random(20260924)`, then take tasks round-robin across the 8 repos in shuffled order
  until 30 are chosen — every repo contributes 3 or 4.
- Purpose: prove the harness end to end and **measure** the quantities that size the full study:
  per-cell pass rates, the discordant-pair rates for each contrast, per-run cost and wall time.
- **The pilot makes no confirmatory claim.** Its results are reported descriptively with CIs and
  are not pooled into the full study.

### Full study (not yet approved)

Registered now so it cannot be tuned to the pilot:

- **H1 (tool):** the search tool raises task success (`tool` + `both` vs `none` + `inject`).
- **H2 (injection):** automatic injection raises task success (`inject` + `both` vs `none` + `tool`).
- **H3 (interaction):** the injection effect differs with and without the tool.
- Model: mixed-effects logistic regression `success ~ tool * inject + (1 | task)`. H1 and H2 are
  the main-effect coefficients and H3 the interaction, with Holm correction across the three.
  A paired McNemar test on each collapsed contrast is reported alongside as a robustness check.
- **N:** tasks x reps chosen from the pilot's discordant-pair rates to give 80% power at
  alpha = 0.05 (Holm-adjusted) for an absolute success difference of 10 points on H2, capped at
  the approved budget. The initial plan is ~60 tasks x 4 cells x 2 reps (~480 runs), preferring
  more tasks over more reps. The computed N and cost are posted for approval before any
  full-study run.

## What is fixed, and what would count as a deviation

Fixed by this file: task set and exclusions, gold definitions, arm list and settings, budget,
metrics, primary and co-primary outcomes, tests and correction, seeds, pilot size and model.
Any change is a deviation, is dated in the log below with its reason, and results under the
changed rule are labelled as such.

## Amendment log

- 2026-09-30, after the resumed run stopped on expired credentials: the resume
  completed seven tasks (28 cells). Its next cell, nushell-001/tool, returned in 7 s with
  no serving model, zero cost, terminal reason `api_error` and "OAuth session expired".
  The agent never reached a model. The harness stopped the campaign as designed. A
  single, reviewed second resume may re-run that task once, and only that task, in a
  new output. Its prior receipt is kept, hashed and excluded from arm rates as an
  infrastructure failure, not an arm outcome. Every other spent task is skipped. The
  three never-attempted tasks run as scheduled. The bobbin executable, GPU wrapper,
  runtime verifier, runtime lock, client, model, cells, order and per-cell limits are
  byte-identical to the first resume. Credentials are refreshed and their expiry is
  recorded before launch. A receipt showing any model attribution, cost or usage is not
  eligible for a re-attempt.

- 2026-09-30, after the chained continuation stopped in index setup: the run
  completed ruff-001 (four cells), then `bobbin init` timed out at its fixed 30 s during
  go-003's index setup, before any go-003 cell existed. The pinned GPU wrapper verifies
  the runtime inside that budget. Measured under host load 22–37, verification took
  12–374 s. One attempt also failed on its 10 s package-query timeout. `init` now
  receives the task timeout, and an init timeout is a recorded setup error that still
  stops the campaign. The package query allows 120 s. **Deviation:** the resumed run's
  copied runtime verifier differs only by that timeout, so `runtime_verifier_sha256`
  changes. The bobbin executable, GPU wrapper, runtime lock, client, model, cells, order
  and per-cell limits are unchanged. A single resume skips every spent task and re-runs
  tasks with no cell directory (go-003 made zero model calls). The spent ruff-001 cells
  are kept and labelled with the chained cohort.

- 2026-09-30, after the bounded continuation and before any further paid cell: a
  second, chained continuation may complete the approved pilot's remaining slots. It
  skips every task attempted by the original run or the first continuation. Fixture
  repairs made since (bobbin #150, #152, #153: ten tasks had pinned the fixing commit's
  first parent, and grader/setup defects for Go, pytest, Ruff, pandas and polars) change
  task definitions only for tasks with no paid evidence; the executor refuses a changed
  definition of any paid task. Eligibility again requires fresh free parent/fix controls
  on the exact definitions that will run, with the discrimination rule unchanged. Model,
  cells, order, per-cell limits, runtime and client pins are the first continuation's.
  The three cohorts (original, first continuation, chained continuation) stay labelled
  in any descriptive pooling, and cohort-3 tasks ran on repaired definitions. The pilot
  remains descriptive; full-study approval is unchanged.

- 2026-09-27, after a partial pilot: conservative continuation must skip every
  previously attempted task, including fixture failures and incomplete pairs. The
  original run has two complete tasks (eight cells), one incomplete task (one of four
  cells, turn-capped), and eight fixture exclusions. The incomplete task remains
  explicitly incomplete. Original complete tasks may be reported alongside the
  continuation as descriptive pilot data only when their pins verify byte-for-byte;
  source-run labels remain visible. The 19 untouched tasks must all receive free
  parent/fix controls first; failed controls are fixture exclusions, never arm outcomes.
  A fresh runtime library lock must derive from the original verified archives, and
  an exclusive continuation claim must prevent duplicate spending. The
  [recovery commands](PILOT_RECOVERY.md) default to planning only; execution requires
  an explicit flag, reviewed input hashes and a new output directory. The original
  client executable was not pinned; the continuation records that limitation and a
  new client hash, without claiming original client identity. Implementation review,
  complete checks and the posted
  fixture-control report are required before any paid cell. Full-study approval is
  unchanged. The grader now recognizes explicit custom Typst result counts; prior
  recorded outcomes are preserved rather than silently rewritten.

- 2026-09-25: [L2 dataset preparation](SWE_BENCH_VERIFIED.md) freezes 23 external
  SWE-bench Verified instances from 12 repositories by a pre-outcome, fixed-seed
  selection. Gold/test records remain evaluator-only. This is exploratory dataset
  preparation, not a run or a change to the registered L1 pilot. Future L2 comparisons
  report the curated in-house tasks as a separate secondary set. Source task timestamps
  do not establish fix dates or absence of training exposure; the linked contamination
  note governs L2 claims.

- 2026-09-24: list markers normalized for Markdown lint only; no design or analysis change.
  First L0 manifest preserves the pre-formatting registration hash.
- 2026-09-24, before any L1 task run: implementing the pilot exposed two gaps in the
  existing v1 runner: it only distinguished with/without bobbin, and it never supplied
  the fixing commit's hidden tests. The new `python -m runner.pilot` entry point makes
  tool availability and automatic injection independent, with the registered seeded
  order and identical task prompts. Search tools are exposed through the local bobbin
  MCP server only in `tool`/`both`; agent-initiated bobbin CLI calls are denied in every
  cell. `inject`/`both` receive the production UserPromptSubmit hook at budget 300;
  v1's prime/post-edit hooks and its lowered quality gate are not used. Thus the
  injection factor measures the initial prompt hook, not v1's combined hook suite.
- 2026-09-24, same pre-L1 amendment: the grader installs changed test files from the
  fixing commit only after the agent exits. A task first needs a discriminating
  control: its hidden tests execute and fail on the parent, and execute and pass on
  the known fix. Zero executed tests or an unrecognized test summary cannot pass.
  A task that fails this control is recorded as a fixture error, never scored as an
  agent failure or silently replaced. Such errors must be resolved and documented
  before claiming a complete 120-run pilot. The cell checkout has one starting-tree
  commit, so the answer is unavailable through its git history.
- 2026-09-24, same pre-L1 amendment: each task uses an isolated home/config; each cell
  starts from an independent copy of the same parent/index. The executable is copied
  and hashed to prevent a host updater changing treatments mid-run. Default limits
  are $2 per agent run, 40 turns, and 900 seconds; these are identical across cells.
  The manifest records them, the registration hash, the harness commit and the model
  provider's [January 2026 training cutoff](https://platform.claude.com/docs/en/models/sonnet-5/overview).
  Serving-model mismatch or unavailability stops further spend. Two synthetic
  reply-OK model/authentication probes preceded this amendment; neither used a study
  task, and neither is an L1 result. No L1 task runs existed when these changes were
  made. The model, selected 30 tasks, four cells, repetition count, primary outcome
  and descriptive-only pilot analysis remain unchanged.
- 2026-09-24: the first L0 execution yielded 33 cargo-001 arm/budget scores without
  arm errors, then was stopped before the next task finished. These are preflight
  data, excluded from the full L0 report. The restart pins and hashes the executable
  and reuses an index only after a completion marker matches the source commit,
  executable hash and index overrides. An interrupted `.bobbin` directory is not
  evidence that indexing finished. No metric or arm definition changed.
- 2026-09-24, before the first paid L1 cell: the initial pilot was paused during
  CPU indexing with zero agent streams/results. At the study owner's direction,
  the replacement pilot uses CUDA on one fixed NVIDIA RTX 4070 Ti SUPER across
  all four cells. The device is part of the embedding configuration: ONNX Runtime
  GPU 1.23.2 (ort API 23), CUDA 12 runtime 12.8.90, cuBLAS 12.8.4.1, cuDNN
  9.8.0.87, cuFFT 11.3.3.83, cuRAND 10-series and nvJitLink 12.8.93. The exact
  cuRAND package version is pinned in the runtime manifest (its four-component
  version resembles a private IPv4 address to the public-repository scrub guard).
  The manifest
  records artifact SHA256s, device UUID/driver, library environment and wrapper hash.
  The original executable is retained and hashed. The incomplete CPU attempt is
  archived separately; none of its indexes or outcomes enter the replacement.
  Every task index must emit `ONNX session using CUDA GPU acceleration`; CPU
  fallback or missing proof fails the run. All cells inherit the same GPU wrapper.
  A configured gaming-hold probe gates launches and pauses active GPU processes;
  unknown state also pauses. Held wall time is not evidence of poor retrieval or
  task failure; any affected agent timeout is reported as an interrupted run.
  Every eval unit is bounded at four CPU cores and 6 GiB memory. Task sample,
  model, arms, metrics, per-cell budget and repetition count remain unchanged.
- 2026-09-24, after partial pilot execution: a correctly attributed model reached
  the configured 40-turn limit and the CLI returned `error_max_turns`, exit 1.
  The runner had conflated that bounded failure with model unavailability and
  stopped the campaign. Such a receipt remains an unsuccessful cell; only the
  campaign-stop decision changes. Continuation requires the pinned model plus
  both `error_max_turns` and terminal reason `max_turns`; timeouts, unattributed
  responses and other errors still stop further spend. Existing results are not
  rewritten or repeated by this correction, and no limits or sample sizes change.
- 2026-09-24, before full L1: new GPU runs require an extracted-library SHA256
  lock derived from the already pinned wheel archives. Run-start verification
  hashes every pinned library and probes the ORT/CUDA loader for unlisted
  dependencies before preparation/indexing. The wrapper and paid-cell boundary
  repeat verification and retain receipts. See [RUNTIME-LOCK.md](RUNTIME-LOCK.md).
  Historical campaigns remain unchanged; this adds no full-study authorization.

### A1 (2026-09-24): J4, per-chunk admission with a learned floor, plus abstention metrics

Added before any J4 scoring. It changes nothing above: the registered arms, metrics,
confirmatory family and gate are untouched, and the L0 run in progress is not re-scored or
re-labelled by this amendment. J1-J3 remain registered as above under their own bead; J4 is a
separate arm under its own bead (aegis-4hhqoe.3).

**Question.** The shipped quality gate is query-level: the hook compares the top raw cosine
score against `gate_threshold` (plus an intent boost) and injects everything or nothing. A top-k
retriever over a corpus with no answer still returns its nearest neighbours. J4 asks whether a
per-chunk relevance judgement can (a) drop the irrelevant chunks from an injection and (b)
abstain when no chunk is relevant, better than that single cosine threshold.

**Arm J4.**

- Candidates: the `-gate` arm's chunks at budget 600, in the hook's own order, first 20 at most.
  `-gate` is used so the cosine gate does not pre-filter what J4 judges.
- Judgement: one Jev `noul` per candidate, model **pinned to `jev-1.13.0`** by name (the
  API accepts a pinned version and refuses an unknown one; measured 2026-09-24). Every response's
  reported model is recorded, and **a run in which any response reports a different version is
  refused, not scored**. Calibration and evaluation must be judged by the same version. State, fixed now:
  `Task: <task description>` then a blank line, then `Passage (<path>:<start>-<end>):`
  and the passage text, cut at 120 lines. Question, fixed now: *"Does this passage contain code or
  text that a developer would need to read or change to complete the task?"*
- Admission: keep candidates with noul >= floor, in candidate order, until the 300-line budget.
- If none is kept, the outcome is **`abstained`**: a typed NO CONTEXT, distinct from `skipped`
  and `error`. An abstention has undefined density and chunk precision. It is never scored as 0
  and never as a perfect score. It is counted by the coverage metrics below and excluded from
  every mean that would otherwise be undefined.
- **Failures and retries, fixed now.** A failed judgement (no key, transport or HTTP error, an
  answer without a noul) is retried at most 2 more times, 5 s apart. If any candidate of a task
  still has no judgement, the whole task is `error` for J4. It is never partially judged and never
  falls back to admitting. Errors are counted and reported per set, and they are excluded from
  both the numerator and the denominator of every rate below; the counts appear next to each rate.
- **No confirmatory result from an incomplete evaluation set.** If any evaluation task, positive
  or negative, ends as `error`, J4's test below is reported as *not run (incomplete)* with the
  count, not as a pass or a fail.

**Negative set (wrong-repository stress set).** Each task's description is run against a
workspace from a *different* repository: the task with index i in repo R, against the task with
the same index in the next repo of the fixed cycle cargo -> django -> go -> nushell -> pandas ->
polars -> ruff -> typst -> cargo. The intended outcome is to abstain. **Caveat, reported
wherever this set is:** a generic fix can transfer across repositories, so a non-abstention on a
negative is not proof of an error. Admitted chunks on negatives are listed per task, so a
reader can judge them.

**Calibration split, frozen before any J4 scoring.** `eval/v2/j4-calibration-tasks.txt` is
committed with this amendment: 2 tasks per repo (16): one `random.Random(20260924)`, repos in sorted
order, each drawing `rng.sample(sorted(task ids of the repo), 2)`. (A fresh generator per repo
would pick the same positions in every repo, so it is one generator throughout.) The other 24 tasks are the evaluation split. A negative belongs to
the split of the task whose description it uses. The floor is learned on calibration only, and
every J4 number reported as a result comes from evaluation only.

**Learning the floor.** Grid 0.05, 0.10, ..., 0.95. On the calibration positives, choose the
floor that maximises mean density subject to mean hunk recall >= `full`'s mean hunk recall on
the same tasks minus 0.02. A tie goes to the lower floor. The whole grid curve is reported.

- **Abstentions and recall:** an abstained positive task counts in mean hunk recall with recall
  0 (its gold hunks exist and were not shown). Density may exclude abstentions; recall never
  does.
- **No admissible floor:** a grid point whose mean density is undefined (it abstains on every
  calibration positive) is not admissible. If no grid point is admissible and meets the recall
  constraint, the result is **no admissible floor, and J4 has no candidate to test**. The
  constraint is never relaxed to find one.

**Comparator: the shipped cosine gate.** Arm `G(t)`: the hook as shipped with
`--gate-threshold=t`, for t in 0.30, 0.35, ..., 0.70, on both sets. It abstains or injects
whole. J4 and G are compared as curves of abstention coverage against false abstention, on the
evaluation split.

**Added metrics (all arms, secondary unless stated).**

- **chunk P@k**, k = 5 and 10: of the first k chunks an arm injects (fewer if it injects fewer),
  the fraction overlapping a gold hunk. Undefined when nothing is injected.
- **abstention coverage**: on the negative set, the fraction of tasks with outcome `abstained`
  or `skipped`.
- **false abstention**: on the positive set, the fraction of tasks with outcome `abstained` or
  `skipped`.

**J4's test (evaluation split, paired with `full` on the same 24 tasks).** This uses the
existing hook-change gate: a density improvement with Wilcoxon p < 0.05 (Holm across all Jev
arms scored; the family is listed here when scoring starts) **and** a lower 95% bootstrap bound
on the hunk recall difference above -0.02. Abstentions are excluded from density pairs and
reported as a count. Passing the test changes no live behaviour by itself: shipping a floor is
a separate decision.

**Cost bound.** At most 20 Jev calls per task per set: 40 tasks x 2 sets x 20 = 1,600 calls,
measured at ~0.4 s and ~300 input tokens for a one-line passage (up to ~120 lines per passage
here).

## L0 path instrumentation correction (2026-10-21)

The bounded-index opt-in runner records `scoring_paths:
workspace-qualified-or-literal/v1`. The frozen native hook can emit
`repos/<repository-label>/<path>`, while Git gold uses repository-relative paths.
Literal comparison reports zero overlap for the same source file. Opt-in scoring
resolves a chunk only when exactly one existing, nonlinked workspace file matches
its literal path or the exact current workspace-label prefix removed. Unknown,
ambiguous, foreign-only or unsafe paths produce an explicit error, not a zero
score. It never searches by basename, gold membership, suffix or another repo.
The mapping applies equally to native baseline results and hook results; raw
hook character/token accounting is unchanged. The legacy runner and all retained
rows remain literal and untouched. Arms, budgets, tasks, gold exclusions, target
metrics and statistical tests are unchanged. Any new campaign must retain this
instrumentation identity and cannot silently pool its corrected rows with the
prior partial run.

### 2026-10-23: opt-in independent hook outcome contract

Before any further campaign, sealed reuse records
`hook_outcomes=independent-hook-outcomes/v1`. It retains per-cell state isolation
and qualified-path scoring, and refuses dedup/reducing contamination, unsupported
or contradictory output and unexplained silence as errors with null efficacy
metrics. Exactly stated operational-intent and semantic-gate non-injections stay
skips; rounded printed gate values are not treated as exact latent scores.
Legacy outcomes and the original partial rows are unchanged. This is an
instrument amendment, not permission to run, a new result, a resource increase
or a changed model/chunker decision. See
[`l0-outcome-contract.md`](../../docs/eval/l0-outcome-contract.md) for the outcome
family dispositions and the native-enrollment/aggregate-storage acceptance gates.
