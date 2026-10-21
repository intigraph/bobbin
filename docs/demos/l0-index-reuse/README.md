# Sealed L0 index reuse demo

Private synthetic controls demonstrate corrupted-payload refusal, independent
no-coupling copies, the actual CLI callback's one-build/zero-rebuild behavior,
cleanup of a timed-out index's child process, per-cell history restoration,
and removal of the copied embedded coupling graph. No corpus, provider or full
evaluation campaign runs. See the [measured native fixture and limitations](../../eval/l0-bounded-index-reuse.md).

Reproduce from the repository root with the normal evaluation test dependencies:

```sh
cd eval
python3 -m pytest -q -rA tests/test_index_reuse.py -k 'sealed_positive or no_coupling_copy or actual_cli_callback or timeout_reaps or isolated_cells or embedded_coupling'
```

Replay the captured real terminal output from this directory:

```sh
scriptreplay --log-timing timing.log --log-out output.log --maxdelay 0.1
```

[Terminal recording](output.log) · [timing](timing.log).
