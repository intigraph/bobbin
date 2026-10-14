# Bead metadata provenance

The same retained HTTP fixture returns an open issue assigned to a former owner.
The before MCP binary exposes those fields without qualifications. The candidate
MCP binary preserves the fields and adds an unreported source, unknown as-of time,
and warnings that the current tracker has not been verified, even in compact mode.

This is an actual MCP stdio recording against a loopback fixture, not a deployment
or proof that the snapshot matches an active board. HTTP, local MCP and typed-client
behavior are covered separately by the source tests.

Replay with `scriptreplay session.timing session.log`. Reproduce with:

```sh
uv run --script demo.py --before /path/to/before/bobbin --after /path/to/candidate/bobbin
```

The recording used the prior 0.27.3 binary and a candidate built with the knowledge
feature. No production tracker reads or writes occur in this demonstration.
