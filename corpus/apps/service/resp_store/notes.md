# service/resp_store

A Redis-compatible key-value server speaking a RESP2 subset over a real TCP
socket, with an append-only file. Strings, lists (`VecDeque`), counters with
overflow checks, expiry on a logical clock (one tick per command), glob
`KEYS`, `TYPE`, `SET .. NX`, inline commands as a telnet user types them,
binary-safe bulk values holding CRLF, and an input that ends inside a frame.
Every successful write is appended to `store.aof` in RESP, with `EXPIRE`
rewritten as an absolute `EXPIREAT`, as Redis does; after the sessions a
fresh store replays the file, restores the clock and answers the same reads.
The client prints replies the way redis-cli does. The mirror
`resp_store.py` wrote `expected.out`.

Written 2026-10-14 by the real-world apps thread; karac at e8eb667ace.

## Edits from source.orig.kara to source.kara

Author errors (the diagnostics were right):
- `|want: i64, exact: bool| -> bool { .. }`: closures do not declare a
  return type; the parser said so and how to fix it. The helper was dropped
  for plain `argc == n` / `argc < n` tests.
- `struct Parser` shadows the prelude's `Parser` (a warning); renamed to
  `RespReader` in both files.
- In a workaround, `let (mut q, exp) = ..`: syntax.md puts `mut` before the
  whole pattern (`let mut (q, exp)`). The message did not say so
  (B-2026-10-14-57).

Compiler gaps, worked around and listed in the source header:
- B-2026-10-14-56: `match self.items.get(k).unwrap().value { .. }`, a match
  on a field reached through a call that returns a `ref`, is refused by the
  MIR builder with "cannot move a non-`Copy` value out of a borrowed place",
  even with patterns that bind nothing. Check accepts it; binding the entry
  first (`let it = ..unwrap(); match it.value`) works.
- B-2026-10-14-43: string literal patterns cannot match a `Str`, so
  `match name.as_str()` became `match name` on the `String`.
- B-2026-10-12-3: no `SortedMap.get_mut`; entries are removed, changed and
  inserted back.
- B-2026-10-12-6: `i64.parse` returns `Option`.
- B-2026-10-14-44: `String` paths and addresses, `usize` write counts.
- B-2026-10-14-51: check refuses binding `self.text[a..b]` in a method.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.
