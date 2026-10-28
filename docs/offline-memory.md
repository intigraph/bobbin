# Camayoc offline backlog in memory injection

The existing memory lane in `bobbin hook inject-context` reports queued Camayoc
writes from `$XDG_STATE_HOME/camayoc/memory-queue.jsonl` and the sibling
`capture-spool.jsonl`, or
`~/.local/state/camayoc/memory-queue.jsonl` when XDG_STATE_HOME is unset. A positive
count adds `camayoc backlog: N queued (cy flush)` to the memory header. An
unreadable, malformed, nonregular or over-cap queue says UNKNOWN. A missing or
empty queue adds nothing.

This is a bounded local read (16 MiB), with no Quipu call, CLI subprocess or
queue mutation. Queued text never enters the injection. The backlog status also
appears when no recall items or Quipu endpoint are available. Existing prompt
eligibility and `BOBBIN_MEMORY_RECALL=0` still apply. It reports pending records,
not successful graph writes; Camayoc owns replay and its refusal policy.

Install the companion Camayoc buffer for ordered `cy flush` replay. Its
`cy flush --hook` mode always exits 0; the normal CLI returns 1 while entries
remain. The queue format and durability contract live in Camayoc's
`docs/offline-memory.md`.
