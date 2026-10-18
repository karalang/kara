# pipeline/event_store

An event-sourced account ledger. Commands (`open`, `deposit`, `withdraw`,
`transfer`, `close`) are validated against the current state and turned
into events; the events, one pipe-separated escaped line each, are appended
to `events.log` and are the only record. An `Aggregate` trait
(`fn empty() -> Self`, `fn apply(mut ref self, e: ref Event)`) is folded by
one generic `replay[A: Aggregate]`, which builds both the balance view and
a per-account statement. Every five events a snapshot records the state and
the log's byte offset. A simulated crash leaves a torn last record;
recovery truncates it, loads the snapshot, seeks the log to the offset,
replays the tail, and checks the result against a full replay. Command ids
make retries idempotent across the crash. Money is integer cents with a
strict `N` / `N.NN` parser.

The mirror `event_store.py` is an independent Python implementation and
wrote `expected.out`.

Written 2026-10-14 by the real-world apps thread; karac at 0b14473a2e.

## Edits from source.orig.kara to source.kara

Author errors (the diagnostics were right, or the spec says so):
- closures written `|n: i64| -> T { .. }`: closures cannot declare a
  return type; the bindings carry a full `Fn(..)` annotation instead
  (B-2026-10-14-59 is why the annotation is needed at all).
- `let (cmd, verb, acct) = (w[0], w[1], w[2]);` moves out of an index
  (D5); written as three `ref w[i]` bindings, and `match verb.clone()`.
- `text.rfind("\n")`: docs/library/strings.md has `find` but no `rfind`,
  so the last newline is found with a byte loop. A spec gap rather than a
  compiler bug, noted here and not filed.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-14-64: `if self.account(acct).exists { return Err(..) }`, a
  field of an owned call result with a `String` field as an `if`
  condition, fails the MIR move check ("use of _N.0, which may have been
  moved or never initialized"); the field is bound first.
- B-2026-10-14-65: the amount parser as an annotated closure with an early
  `return Err(..)` builds invalid MIR ("assigning Result[(), String] to _0
  of type Result[i64, String]"); it is a top-level fn taking the words.
- B-2026-10-14-66: `{c:<34}` on a `Str` is refused; `{c.to_string():<34}`.
- B-2026-10-14-67: `join` needs `String` elements (spec: `T: Display`).
  The word list is a `Vec[String]` here anyway, because of
  B-2026-10-12-7. B-2026-10-14-68 (a literal passed to `Vec[Str].push` is
  typed `String`) was found probing it and does not touch the app.
- B-2026-10-12-6, B-2026-10-12-7 and B-2026-10-14-44 as in the other apps.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-14-64 and
B-2026-10-14-44 are fixed, so `if self.account(acct).exists` reads the
field directly and the `fs` and `File` calls take the `Str` consts and
count in `usize`. At 3385ce51b1 the rest of B-2026-10-14-66 is fixed
too, so `{c:<34}` formats the `ref Str` loop variable itself. Each of
these is the original line again, and the app still matches on all four
MIR surfaces.

## Back to the original, 2026-10-18

The rows behind the edits above are fixed, so `event_store.kara` is now exactly what `karac fix` makes of `event_store.orig.kara`. It matches expected.out on all four MIR backends (karac at 4ea06e27b9).
