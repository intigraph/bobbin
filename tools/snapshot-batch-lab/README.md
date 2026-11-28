# Disposable document batch lab

This unpublished tool exercises the native Bobbin graph serializer one document
at a time. It has no production publisher, network client, credentials or store.
The planner refuses oversized records and byte/triple budgets, foreign document
membership, implicit deletion and incompatible receipts. Its checkpoint contract
is a proposal for a future bounded ownership transfer API; current Quipu does not
implement it. A legacy promotion receipt cannot advance that checkpoint.

Run `just test-snapshot-batches` and `just build-snapshot-batch-lab`. The planner
takes `MAX_BYTES MAX_TRIPLES REPO GENERATION LEGACY_HEAD` and reads newline
delimited JSON documents containing `document`, `chunks`, `edges` and optional
`delete`. It emits a complete Turtle batch and exact scope/hash metadata per
record. It does not load a repository or persist a checkpoint. Callers must also
bound their document loader before constructing a record.

`scripts/snapshot-batch-lab.py` starts a new loopback Quipu with a temporary store,
private generated bearer and embeddings disabled. It accepts a server binary,
planner binary, code-entities shapes file and new report path, never an endpoint
or existing database. It requires a finite memory scope with `MemoryHigh=2560M`,
`MemoryMax=3G`, `MemorySwapMax=0`; the `snapshot-batch-lab` recipe supplies it.
Use the crew's HOME escape guard when running runtime checks in a crew workspace.

The harness records actual completed writer counter deltas, native terminal
receipts, 5 ms process/cgroup samples and conservative process highwater. Reports
are exclusive-created with mode 0600. A sampling/resource/receipt failure stops
the run. The child process group is terminated on every exit path. Process
highwater is lifetime scoped, so later batch deltas can overestimate their peaks.
Sampling can still miss a peak; this is not server enforcement.

`--path update` uses the same native Turtle as the body of `INSERT DATA` and checks
the asserted-triple receipt. Use one size per fresh-store run when comparing
paths. Update timing uses its native writer-held store counter; promotion timing
uses completed explicit writer acquisition counters. Update phase observations
are copied from the isolated server log. `--validation off` disables configured
validation and loads no shapes: vocabulary bootstrap is also inactive. This is
only a private-store diagnostic control. The default loads code-entities shapes
and enables validation. No production setting is changed.

`--existing-snapshot` first seeds a producer to a committed native receipt, then
changes only label case and promotes the changed hash under that same key. Its
payload byte/triple counts and entity identities stay constant, so the second
promotion exercises existing ownership and term lookup rather than an upload
receipt cache. The memory sampler includes both seed and replacement. It measures
only that document's old source; it does not emulate an entire legacy repository
source vector.

The harness always reports `qualified: false` until bounded CAS ownership
transfer, incremental reader semantics, representative/adversarial workloads,
concurrent probe coverage and server enforced bounds exist. Its default exit is
3 for that unqualified result. `--characterize-existing` permits exit 0 solely for
collecting an explicitly unqualified baseline; it does not waive acceptance.

Fresh-store measurements cannot establish the cost of replacing an existing
repository source or migrating legacy ownership. No append-then-bulk-cleanup
fallback is permitted. Repository readers will need an explicit mixed generation
and incomplete progress contract; consumers requiring one coherent repository
cut need a different generation design. Publishing and deployment admission are
separate from this lab.

The future prepare/commit path must preserve symbolic RDF term kinds and
document scoped blank nodes outside the writer. Read snapshots end before taking
the application writer and a real SQLite write reservation. Commit rechecks head,
ownership, schema, policy and dictionary context, then resolves canonical term
IDs using the existing term space allocator. Dictionary-only insertions may not
advance the fact head; a prepared miss is never a stable ID. All new terms,
ownership, poststate gates, facts and receipt need the same rollback scope.
Necessary dictionary insertions remain inside the writer. Moving parsing or
validation alone does not establish the numerical hold bound.
