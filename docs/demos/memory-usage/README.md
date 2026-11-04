# Replay a memory citation, then try to retarget it

This [terminal recording](output.log.gz), with its [timing file](timing.log), runs
the actual feedback-store source from base
`e3f91595a60c32901ba0096ce3f3cb02f6cca0e0` and the candidate in this change.
Each revision gets an isolated SQLite database. It logs `inj-fixture/m1`, submits
`used` twice as the same agent, then tries to point `m1` at another entity.

Watch these actual results:

| Result | Before | After |
| --- | --- | --- |
| Replay returns the original receipt | false | true |
| Entity uses after replay | 2 | 1 |
| Retargeting is refused | false | true |
| Entity receiving the use count | replacement | original |

The reproducer copies the real store module and citation type from each source
revision into a small temporary Rust program. It invokes their normal methods;
it does not simulate their SQL. The recording demonstrates the storage behavior,
not deployment or graph maintenance. Separate CLI tests cover observation age
and preview; the real Quipu fixture covers conditional metadata updates,
readback, replay refusal and a newer version in another graph.

Reproduce from a full checkout with cached Rust dependencies:

```sh
just memory-usage-demo e3f91595a60c32901ba0096ce3f3cb02f6cca0e0
```

After committing, pass the retained candidate revision as the second argument
to select it instead of the working tree. The captured candidate store SHA-256s
are `aed2942008e4afbd0df11ea1a90844bb9fb37c56979441ec24658d82f4f4ddbc`
for `src/storage/feedback.rs` and
`aab196a07a3b3505d301b875a77ccc7b19d8702af567ac911101624bfc46a10d`
for `src/storage/feedback_usage.rs`.

Download both recording files and replay them with util-linux:

```sh
gzip -dk output.log.gz
scriptreplay --maxdelay 0.1 --log-out output.log --log-timing timing.log
```

The recording retains the actual timing, including compilation; `--maxdelay`
only shortens pauses during playback. Playback was verified. These small files
are retained in the existing demo directory; no terminal player embed is claimed.
