# Stats defer maintenance while an index writer holds the lock

Source: `1275847836155a3fff23f852d184913f3f1021a8`, based on
`941d3b718a661716a7cd45b7f921f5bf3e8a3d6e`.

This is a real test transcript from an isolated one-chunk Lance store and an
exclusive file lock. It compares the candidate with its guard unset (the
existing behavior) and configured. It does not run an installed server or the
base executable. With the writer held, both reads return one chunk; only the
unconfigured read records maintenance. A maintenance timestamp records an
attempt, not a successful compaction. The other controls exercise a free lock,
release, unsafe paths, and explicit scheduled maintenance while its wrapper
owns the outer lock.

Reproduce from this source with Rust and the repository build dependencies:

```sh
cargo test --bin bobbin read_path_writer_guard -j 2 -- --nocapture --test-threads=1
```

[Actual stdout](output.txt) is a transcript fallback; asciinema and termtosvg
were unavailable. Build diagnostics remain outside the published transcript.
The small sanitized transcript is retained here with the reproducer, following
the repository's existing `docs/demos` convention. No production data, secrets,
service mutation, or installed acceptance is represented.
