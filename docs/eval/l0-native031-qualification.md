# Native 0.31.0 sealed-reuse qualification

This is a two-file synthetic instrument check, not a calibrated retrieval study.
The original 330/1,320 partial cells and the #95/#96/#97 evidence gates remain
unchanged. No full campaign is authorized by this enrollment.

## Exact inputs

- Harness base: `55d3b70f207e22f844eb92f5856e0c29313b423c`.
- Native Linux Bobbin 0.31.0 source: `c204212178a56b2e8b7f161d1c1e68440a0f8dcf`.
- Binary SHA256: `acc29d7a2371d9dfe7c7c47564ef4bf9ced5270882430dda16b9030cf9d25c3b`.
- CPU ONNX Runtime 1.24.1 from the official Microsoft release:
  [onnxruntime-linux-x64-1.24.1.tgz](https://github.com/microsoft/onnxruntime/releases/download/v1.24.1/onnxruntime-linux-x64-1.24.1.tgz).
- Archive SHA256: `9142552248b735920f9390027e4512a2cacf8946a1ffcbe9071a5c210531026f`.
- Extracted library SHA256: `e5a7e3646718d8f1f8f52c8fcb770fe229ab44305caf3ea702d558e6e426c9aa`.
- Model: the existing pinned CPU `all-MiniLM-L6-v2` ONNX and tokenizer hashes
  in `runner/index_reuse.py`; no download or provider call.

The first, separately bounded attempt without the explicit library failed before
embedding: duplicate schema registration. That failure is retained. Read-only
`ldd` and `readelf` showed no directly linked ORT; the release's `knowledge`
build and resolved Quipu features selected `shacl`, not `onnx`. These checks do
not prove the failing system library's internal cause. The compatible explicit
library passed; a second Quipu ORT release defect was not established.

## Observations, 2026-11-02

A fresh finite grant allowed exactly two synthetic index builds. All fixture,
model, binary and output bytes lived on the separate `/srv/fleet` filesystem;
no production store or index was opened. The supervisor limited CPU to four,
memory to 6 GiB, swap to zero and wall time to 300 seconds, with nice 19 and idle
I/O scheduling. The actual run took 8.267 seconds and peaked at 704.7 MiB memory.

The two builds were a full coupled index and an independent cold no-coupling
index. A derived copy required no additional native indexing. The full fixture
had one coupling row (positive control); both no-coupling copies had zero rows
and no embedded coupling database. The full source remained sealed throughout.

Two repetitions of the production injection hook on both cold and derived
copies returned identical nonempty injected chunks. Each cell archived mutable
state and restored its sealed baseline. The unqualified binary was first
refused by the unchanged production admission table; a candidate entry existed
only inside the private probe process for the equivalence test.

Read-only `/proc/<owned-child>/maps` sampling observed exactly one library
identity (device/inode/path, rather than counting mapping segments) for each
of the two index processes and all four hook processes, at the pinned explicit
library path. No map-read errors were observed. This is sampled evidence, not
an assertion about every instant of execution.

Peak task scratch was 251,854,848 allocated bytes / 250,510,699 logical bytes,
248 files, below the admitted 2 GiB ceiling. Scratch was removed afterward.
The normal HOME guard verified 158 watched paths and five UID-keyed resources
unchanged, with child exit zero. Before/after `df` and the failed first attempt
are retained with task `aegis-74d5`; filesystem variation is not a capacity
recovery claim. This small fixture does not establish whole-matrix storage or
six-hour feasibility.

## Admission and rollback

Only the exact new binary plus the exact explicit dynamic library are admitted.
`ORT_DYLIB_PATH` must select an absolute regular, non-symlink file with the pinned
hash. Missing, relative, changed or indirect library inputs refuse; the library
hash participates in new-runtime seals. The old 0.28.0 enrollment and seals
retain their existing format. Unlisted binaries remain refused.

Rollback is the normal source inverse, retaining every historical and new
result. It neither installs a runtime nor changes loader/system configuration.
A new campaign still needs its separate aggregate-storage and frozen execution
packet acceptance. This source change supplies no campaign, GPU or shared-index
authority.

## Before/after demonstration

This is a real readable transcript, not a video (`asciinema` is unavailable).
The [synthetic reproducer](demo-031-ort-binding.py) makes only inert fixture
files and calls the library identity checker; no native executable or model runs.
Run from this source checkout:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 docs/eval/demo-031-ort-binding.py --base 55d3b70f207e22f844eb92f5856e0c29313b423c
```

```text
Target: synthetic files; no executable, model or provider call.
base: pinned-library positive control ACCEPTED
candidate: pinned-library positive control ACCEPTED
base: changed library ACCEPTED
candidate: changed library REFUSED: qualified ONNX runtime bytes changed
```

The base is the exact revision above; candidate is this proposal's ORT-binding
implementation. It shows a synthetic dynamic-library identity change, not an
installed service or a retrieval-quality improvement. All 628 eval tests pass;
four new library-binding controls fail against base, then pass against candidate.
Both the demonstration and test suite completed with clean HOME guards.
