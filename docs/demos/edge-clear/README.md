# Batched edge clearing

This recording runs the real native Lance parity tests against disposable,
identical synthetic stores. The serial arm uses the retained single-file API;
the batched arm uses the new API with at most 100 paths per delete. Both include
the same legacy absolute-path cleanup. The results report elapsed seconds and
Lance commit counts, and compare every retained edge field plus repository.

Run from the repository root:

```sh
python3 docs/demos/edge-clear/demo.py
```

Replay the captured terminal output with its original timing:

```sh
asciinema play docs/demos/edge-clear/demo.cast
```

The 17- and 205-path controls cover quoted paths and repository names,
repository isolation, unselected rows, and files with no replacement edges.
Additional controls exercise empty selections, an absent table and unscoped
selection. Timings vary with local storage; these fixtures do not measure a
complete indexing run or establish production recovery. Deletes across batches
are not atomic. No production index, embedding model or external service is used.

The recording was losslessly converted from util-linux script output and timing
into asciicast v2. The recording is retained here as a small reproducible PR demonstration. Its
source base is `9624689e5eca15f198910201fd89ebf3afd1af9e`; the accompanying
source changes are the candidate under test.
