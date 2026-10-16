# Capped L0 evaluation artifact

Base `d619d2212f8dd9784d9936396b36fe9a92f9ae5c` has the preregistration but no report of this run. Artifact commit `4eb744d` retains the capped cohort: 330 unique cells across 10 of 40 planned tasks, including every error and skip. Watch the committed row counts and scoring positive control, then a one-byte corruption fail the pinned digest.

Run `python3 docs/demos/l0-partial-20261016/reproduce.py` from the repository root. [Actual transcript](output.log) and [timing](timing.log) follow the existing small-demo retention convention. Download both and use `scriptreplay --log-out output.log --log-timing timing.log`, or read the transcript.

This is an audit of committed data, not another study run, a deployment or an efficacy demonstration. It executes no index build, retrieval, live service or model call. The full preregistered matrix remains incomplete.
