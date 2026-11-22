# Blocking pool budget

This is a real transcript from the source test at commit `1d240e64`, using the
same batch of 24 blocking jobs with two explicit budgets. The control permits
eight active jobs; the smaller budget permits two. Both complete all 24 jobs.
The reproducer also refuses invalid values and checks that an unset value
preserves Tokio defaults.

```sh
cargo test --bin bobbin runtime_config::tests -- --nocapture --test-threads=1
```

See [output.txt](output.txt). This is a source-level runtime fixture, not an
installed service or a full indexing workload. It demonstrates the pool setting;
it does not establish the cause of a particular thread-limit incident or prove
that every other worker pool fits a service cap. The output is retained here
with the source. Terminal recording tools `asciinema` and `termtosvg` were not
available, so this is a readable transcript rather than a video.
