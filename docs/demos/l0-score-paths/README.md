# Exact L0 scoring paths

A synthetic header in the measured frozen native format names
`repos/fixture/app.py`; Git gold names `app.py`. The same two-line source scores
file recall 0 under legacy literal comparison and 1 through the opt-in exact
workspace mapping. A foreign-only path refuses, and original literal scoring
remains unchanged. This demonstrates the instrument, not retrieval quality.

Run `python3 docs/demos/l0-score-paths/probe.py` from the repository root.
The source base is `9624689e5eca15f198910201fd89ebf3afd1af9e`; the candidate
adds the mapper and opt-in driver wiring. Exact candidate identity is in the PR.
No native executable, model, provider, graph or campaign is invoked.

[Real terminal capture](output.log) and [timing](timing.log) can be downloaded
and replayed with:

```sh
scriptreplay --log-timing timing.log --log-out output.log --maxdelay 0.1
```

The private paired control also applies this mapper to retained actual native
hook chunks. Its outcome does not change or reinterpret the earlier partial run.
