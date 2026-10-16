# Breadcrumb keyword triggers

This is a replayable terminal recording of two actual CLI builds against
disposable local repositories. No index, embedding model, server, installed
hook configuration or real breadcrumb store is changed.

Watch the same `refresh_token` keyword on prompt and tool-input hooks:
the baseline is quiet; the candidate emits the saved breadcrumb and pinned
file reference. The same session does not receive a duplicate. A fresh session
receives one tool-hook block. Each block uses at most six of twenty lines.
The reproducer asserts that the breadcrumb store stays byte-identical.
SessionStart discovery is already present in both builds.

- [Recording transcript (gzip)](output.log.gz)
- [Replay timing](timing.log)
- [Reproducer](reproduce.py)

Replay after downloading both recording files:

```sh
gzip -dc output.log.gz > output.log
scriptreplay --log-out output.log --log-timing timing.log
```

Reproduce with two supplied CLI builds:

```sh
BOBBIN_DEMO_BEFORE=/path/to/before/bobbin \
BOBBIN_DEMO_AFTER=/path/to/after/bobbin \
python3 docs/demos/breadcrumb-triggers/reproduce.py
```

The baseline binary is revision `843dc42bae0cfd39718d29eaecb3b12fe6e8dbfe`.
Its source differs from the branch baseline
`e48be5bc7eafa9e66fa284ac891ffa5e67c2e234` only in README and evaluation
documentation; executable source is identical. Candidate executable source:
`40fd06fe143ead53e3326c6d6162f296c31959a5`; the subsequent base merge adds only
evaluation documentation. The recording prints both binary SHA256 values. The candidate
binary is `4178cb9d20f460e058acae76b04ce65eef98527ca86dd8278b0f5c417052f0e2`.
This small recording is retained in Git alongside the reproducer. Playback
requires a terminal replay tool; no inline video playback is claimed.

Limits: this is candidate CLI behavior on a synthetic fixture, not installed
hook acceptance or a measurement of retrieval quality. Separate CLI tests cover
selected-server query execution, access controls, invalid stores and bounded
nonblocking reads of special files.
