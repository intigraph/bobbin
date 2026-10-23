# L0 outcome reconciliation and the next-run gate

This is source/instrument work for [#95](https://github.com/intigraph/bobbin/issues/95),
not a fresh evaluation or permission to start one. Original rows, their digests,
legacy scoring and the model/chunker gates remain unchanged.

## Retained evidence and dispositions

The frozen CSV still has 330/1,320 cells from ten of forty tasks: 213 injected,
83 skipped and 34 errors. Its SHA256 is
`54b0f5d8da2e4a583c5ffee2c94c2e0b51171b59b7c63d2994c43b97c44f7c85`.
The manifest binds the original stream separately. A public scalar projection
is not the hook's stdout/stderr archive.

| Family | Count | Disposition |
|---|---:|---|
| Parsed injection | 213 | Retain exact old measurements and literal path convention; not automatically comparable to qualified scoring. |
| No parseable headers | 34 | Invalid measurements. Preserve errors, never replace with zero recall. Exact output/cause cannot be recovered from the scalar rows. |
| Previously injected | 23 | Invalid evidence for an independent no-dedup cell; retain old skip rows, refuse this condition in the new opt-in contract. |
| Operational intent | 42 | Explicit production non-injection; retain as a declared skip rather than retrieval failure. |
| Semantic gate | 18 | Explicit production non-injection; retain as a declared skip with the actual printed reason. |

All 34 old error rows also contain numeric `0.0` file/hunk recall from the
legacy driver. Those bytes stay unchanged: the error status excludes them from
valid evidence. The new independent driver leaves these metrics null, making
that distinction explicit rather than silently treating an invalid measurement
as an observed miss.

The 34 header-error rows occur in Django tasks. That association does not prove
which output form caused them. In particular, a synthetic headerless or JSON
complementary-files fixture is not a recovered historical output. Do not infer
that decoding arbitrary JSON repairs all 34 cells or that a headerless response
contains zero relevant code.

The frozen native hook has two mechanisms: `--no-dedup` disables the fallback
session hash comparison, while the reducing ledger is separately enabled by
configuration and a nonempty session ID. It can filter chunks and emit the
observed "previously injected" reason even with that flag. This source path
explains the condition; exact historical ledger contents are not reconstructed.
The already-delivered sealed cell isolation archives/restores mutable state
before the next cell and rejects a baseline containing history. Reuse that
mechanism; do not add a second ledger reset or alter shared runtime behavior.

## Versioned independent-cell contract

The sealed opt-in driver records `hook_outcomes=independent-hook-outcomes/v1`
and passes the strict contract through to the real hook-result classifier.
The legacy driver still records `legacy` and retains its previous behavior.
`scoring_paths=workspace-qualified-or-literal/v1` is a separate existing
convention; changing outcome classification does not conceal a scoring change.

Under the new contract:

- A successful output containing supported standard chunk headers is injected.
- Exactly one explicit operational-intent or consistent semantic-gate reason
  with no context is a skip. Printed scores are rounded: equal displayed
  score/gate values may represent a valid strict comparison before rounding.
- A dedup skip, partial reducing message, unknown/multiple skip reasons,
  contradictory context plus skip, unexplained silence, unsupported output,
  nonzero exit or reported hook error is an error.
- Error cells carry no numeric file/hunk recall or density. The existing
  isolation layer quarantines an error and stops subsequent cells; it does not
  silently reset uncertain state and continue the matrix.

This changes only the opt-in measurement path, not Rust retrieval, the CLI's
normal session behavior or historical outcomes. The preregistration amendment
records the interpretation before another campaign. Paired fixtures cover real
header syntax, headerless/complementary JSON refusal, explicit ordinary skips,
contamination and the actual task-driver propagation to null error metrics.
They are instrument controls, not retrieval-quality observations.

## What a further run waits on

The source work and the separately bounded execution envelope must be accepted
before another campaign. Do not silently resume the old output or repeat it to
fill missing cells. The following are concrete gates, not elapsed-time waits:

1. Land reviewed, green outcome-contract source and freeze its harness,
   preregistration, forty task YAMLs, eleven arms, budgets, config and scoring
   hashes in a new output manifest. Preserve original data and pin the actual
   executable independently of the harness revision.
2. Enroll the native executable intended for the current-source study. Existing
   sealed reuse admits only its exact 0.28.0 runtime/model bytes. A released
   0.30.0 version string is not enrollment. Cold/derived-index equivalence,
   repeated independent-cell positive controls and mutation refusals must bind
   the proposed native binary and complete CPU model/tokenizer hashes before
   it can be used. The old 0.28 runtime alone cannot establish today's-main
   retrieval quality.
3. Accept a measured whole-run storage plan. The old 21 built/partial indexes
   alone occupied 6,474,663,440 bytes, largest 450,579,646; they exclude full
   clones, mirrors, models, journals and all missing tasks. The existing
   512-MiB per-index cap is not a total-run quota or a feasibility estimate.
4. Admit one fresh run under the unchanged CPU400%, memory6GiB, swap0,
   no-GPU/no-agent/no-paid-model and 21,600-second whole-invocation limits.
   This document does not raise a limit or enroll another execution lane.

### Proposed bounded storage/execution plan (not yet accepted)

Use one fresh private run root on the mounted fleet filesystem, a fresh score
stream and the delivered sealed full/derived indexes. Never use production
indexes or another run's mutable cache. Inventory all task/source/model inputs
read-only before admitting indexing. Reuse read-only Git objects/mirrors only
when the task's exact pre-image and checkout isolation are verified; do not
share mutable working trees or reuse an index across distinct commits.

Propose a **32-GiB aggregate allocated-storage ceiling**, subject to acceptance
and a measured admission inventory. This is a proposed bound, not evidence that
all tasks fit or that the present host has room. It must cover clones/mirrors,
models, both index arms, results, journals and temporary files. External storage
must not be used to evade it; reflink apparent size alone is not allocated usage.
The existing per-index bound remains additive. If a reliable aggregate quota
and free-space floor cannot be enforced through the normal sanctioned lane,
the run remains blocked; a periodic `du` sample is not a hard quota.

For admission, provide expected plus worst-case allocated storage, mount/quota
identity, source/model input sizes and a floor that protects other fleet writers.
Keep one task's mutable full/derived pair at a time only if the accepted
retention contract can archive its forensic outputs before owner-authorized
reclamation. This is a proposal; it does not authorize deleting the old campaign
or other owners' data. Measure cold-index, copy/seal, hook and journal costs
separately. Tiny-fixture native timings cannot predict full-corpus wall time.

The actual supervisor must enforce the original wall/cgroup limits and stop on
quota/floor, seal, output-format, timeout or restoration failure. Retain raw
stdout/stderr for the failing cell privately under the accepted retention/byte
budget so another header failure is diagnosable; do not publish retrieved source
bodies. Preserve the terminal reason, immutable completed scores, quarantine and
explicit missing-cell coverage. Do not classify a cap hit as completion, auto-
retry a failed cell, increase the envelope or start a second campaign on this GO.

A complete registered matrix is a prerequisite for the preregistered aggregate
analysis; successful source tests and the earlier 330 cells do not satisfy it.
The six-hour cap can still produce an honestly incomplete result. #96/#97
remain gated on usable evidence and their own decisions.

## Reproduce the bounded instrument checks

`python3 -m pytest tests/test_l0_outcome_contract.py tests/test_l0.py tests/test_index_reuse.py tests/test_score_paths.py`
from `eval/` uses private fixtures and committed rows. It runs no corpus campaign,
model, GPU, shared index or provider. The normal full Python eval suite also
covers the surrounding driver. An inverse removing the strict driver forwarding
must fail the contamination/silence controls; removing the null-metric refusal
must fail the driver error controls.

Rollback: revert this instrument change, retain both old and separately named
new outputs, and never reinterpret one contract's results as the other's.
