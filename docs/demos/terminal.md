# Find code by meaning, then read its context

This recording runs native Bobbin 0.36.1 at commit
`667a680d393c0d9e5347af7fc9d71472c00a494e` against a local public Python fixture.
The manifest pins the executable and every input. Semantic search returns
`calculate_total`; context displays its three source lines. The optional Quipu
store is unavailable, so its single warning remains visible. This does not
claim graph ranking occurred.

The two reading holds total 20 seconds. Actual command execution durations
are recorded separately. The static transcript is an accessible fallback;
the last SVG frame remains visible when animation is disabled.

The vendored renderer is byte-identical to Tambo's shared recorder at
`b0765b5d8f16a6a26acb3595552f17826d965b44`,
SHA-256 `bebbe000565314f18a58731f2a1a4690577db127b3673fa10a45ce74eef0f23c`.
It refuses possible private paths and credentials rather than filtering output.

## Reproduce

Use Python 3, the recorded Bobbin version and a compatible ONNX runtime.
Prepare a new, absent workspace with the helper. It creates an isolated HOME
and index, and indexing may download the embedding model. It does not install
hooks or connect to a graph.

```sh
python3 docs/demos/terminal/prepare.py /tmp/bobbin-terminal-demo --tool "$(command -v bobbin)"
python3 docs/demos/terminal/render.py docs/demos/terminal/demo.tape.json   --workspace /tmp/bobbin-terminal-demo --tool bobbin="$(command -v bobbin)"   --output /tmp/bobbin-terminal-render
```

The fixture and model cache stay inside that disposable workspace. No existing
index or user configuration is read. Use a fresh directory for each capture.
