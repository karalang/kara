# mini_priority

The HTTP/2 priority tree of the `priority` library 2.0.0 — the third piece of
the h2 stack, alongside `mini_hpack` (header compression) and
`mini_hyperframe` (framing). A server uses it to decide which of many open
streams to send bytes for next: clients arrange their streams into a
dependency tree with weights (RFC 7540 §5.3), and the tree hands back stream
ids in a deficit-weighted round-robin so a weight-32 stream gets roughly twice
the turns of a weight-16 sibling.

The port follows the library's own `priority.py`. It keeps each node's
children in a binary min-heap keyed on `(level, stream_id)` — the same key
Python's `heapq` uses, so the pull order is identical by construction — and
reproduces the scheduler's deficit arithmetic exactly
(`level += (256 + deficit) // weight`, carrying the remainder), the recursive
descent through blocked parents to their active children, exclusive insertion
(which re-parents a node's existing children under the newcomer), removal with
child-stripping, reprioritization with its dependency-cycle reversal, and
blocking/unblocking. Every error the library raises is reproduced with its
exact message: `DuplicateStreamError`, `MissingStreamError` (with the
`KeyError` quoting Python gives it), `PseudoStreamError` on stream 0,
`BadWeightError`, `PriorityLoop` on a self-dependency, `TooManyStreamsError`,
and `DeadlockError` when every stream is blocked.

**Jobs.** `data/jobs.tsv` holds 73 jobs, one per line, tab-separated. State
persists until the next `tree` line resets it:

- `tree <max_streams>` — start a fresh tree;
- `insert <id> <depends_on|-> <weight> <excl 0|1>`;
- `reprio <id> <depends_on|-> <weight> <excl 0|1>`;
- `remove <id>`, `block <id>`, `unblock <id>`;
- `sched <count>` — pull `count` stream ids.

A mutation prints `ok` or `err=<message>`; a `sched` prints the list of ids
pulled, with the `DeadlockError` message on its own line if the tree jams.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `priority` in `vendor/priority/` (MIT, in
`LICENSE-priority.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py` embeds
it into the Kāra source as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with
no new compiler gaps. karac at 502e55cb69.
