# Bobbin 0.27.3: context and native hook (60 seconds)

[Watch the video](demo.mp4) · [Replay the terminal recording](demo.cast)

**Released Bobbin 0.27.3** indexes the public
[Desire Path repository](https://github.com/scbrown/desire-path). First,
`bobbin context` shows direct matches and files added through git co-change.
Then an actual Claude Code 2.1.287 turn receives a Bobbin `PostToolUse` hook
attachment. The next assistant turn names the injected files and coupling scores.
The local stdio MCP server is connected in that native session.

This is an **edited replay of native output**, paced to 60 seconds; the
underlying turn took 22.77 seconds. Source-file dumps and unrelated session-start
output are omitted. The hook scene is a read-back of the actual native Claude
transcript attachment, not a manually dispatched callback. Its timestamp
precedes the next assistant turn in [native-hook.json](native-hook.json).
No agent-performance improvement is claimed.

The capture uses a fresh Ubuntu 24.04.5 VM, a local index, and no remote Bobbin
service or homelab configuration. To reproduce, install the checksummed
[0.27.3 release](https://github.com/scbrown/bobbin/releases/tag/v0.27.3), clone
the public repository, and run these commands there:

```bash
bobbin init
bobbin index --no-quipu-publish
bobbin context 'Explain how repository code records failed tool calls and suggests shortcuts. Read only. Name coupled files from the injected context.'
bobbin hook install --global
claude mcp add --scope user bobbin -- /absolute/path/to/bobbin serve --no-remote /absolute/path/to/desire-path
```

Authenticate Claude Code with your supported account and ask that same
read-only question. `--global` and the user-scoped MCP registration were applied
only to the disposable guest, not the host. Git co-change expands context;
it does not prove that a file must change with another file.

Exact tool and repository versions are in [recording.json](recording.json).
