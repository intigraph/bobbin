# Snapshot heading-depth regression

A stored README section beginning `## Before you start` has a one-component
breadcrumb, but its heading level is 2. An indented `#### Deep` section has
level 4 despite its two-component breadcrumb. The regression inserts both into
an actual temporary VectorStore, reads the publication snapshot, and serializes
its Turtle. It also checks that the snapshot retains at most nine syntax bytes
per section and neither private body appears in Turtle.

[Captured test transcript](transcript.txt) shows the same regression failing
with the old `chunk.content.clear()` snapshot and passing with the candidate.
Base source: `585e2d5d29f5913bdc7f7123fd86c62c2ab28a19`; candidate is this commit.
This is actual captured stdout, not a timed recording or an installed-service
probe. The original qualification was not terminal-recorded.

Reproduce the candidate with:

```sh
cargo test --locked --features knowledge --bin bobbin snapshot_preserves_heading_levels_without_retaining_document_text -- --nocapture
```

For the negative control, replace only the bounded section-content projection
inside `snapshot` with `chunk.content.clear()` and run the same test; restore
the candidate afterward. The existing heading parser and all other behavior
remain in place in that control. No network store or production graph is used.
