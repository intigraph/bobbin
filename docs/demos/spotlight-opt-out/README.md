# Optional spotlight annotations

Before this change, a configured Quipu endpoint is called for every HTTP search.
The candidate keeps that default and accepts `spotlight=false` to omit only the
optional annotation request. Search retrieval and result validation still run.

The isolated Rust reproducer parses the actual query parameters, starts a local
HTTP fixture, and checks that the opted-out request makes zero annotation calls.
The default request makes one call, retains the annotation and supplies the
`X-Quipu-Client` header. It also checks invalid boolean input and an absent endpoint.

```sh
cargo test --locked -j2 --features knowledge --bin bobbin \
  http::handlers::search::spotlight_tests
```

Observed candidate output is in [output.txt](output.txt). It is a readable test
transcript, not a video, installed-service comparison or live monitoring proof.
The transcript records the source candidate on base `f8786331`; the PR identifies
its final committed head. The tests use synthetic data and a loopback fixture.
No model acquisition, production search, release, installation or monitoring
configuration change is demonstrated. Full suite and Clippy remain CI gates.
