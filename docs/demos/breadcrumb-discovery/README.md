# SessionStart breadcrumb discovery

Create `auth-refresh` through the real `mark` CLI, then invoke the real startup
hook in an isolated initialized directory. Watch the saved name and description
appear while `ordinary_primer=true` remains unchanged.

The [before recording](before.log.gz) and [timing](before.time) use installed
v0.27.3, commit `dcbc9f67c4c87b988352bd162bba146e9273e840`. The
[after recording](after.log.gz) and [timing](after.time) use the candidate built
from `94bfc9353d9d5f7a256e673c489aa691c3155b6a` plus this PR's discovery patch.
The candidate's version string reports its base commit because the recording
preceded the source commit. Both use the same [reproducer](run.sh) and fixture.
This is candidate behavior, not evidence of deployment or completed keyword
triggers. Discovery lists names and descriptions; it does not execute the saved
query or inject pinned file contents.

These are output-only terminal recordings with readable transcripts. Download
the compressed log and timing files to replay them with util-linux `scriptreplay`:

```sh
gzip -dc after.log.gz > after.log
scriptreplay --log-out after.log --log-timing after.time
```

The compressed logs preserve the exact recorded CRLF bytes and timing offsets.
Playback was checked locally after decompression. No inline player is assumed. Reproduce against a
built candidate with `BOBBIN_DEMO_BIN=/path/to/bobbin bash run.sh`. These small
fixtures and recordings are retained in Git with the source change; they contain
no private paths or live service requests.
