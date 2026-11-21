# grade_histogram — mistakes and findings

Authored knowingly (dogfooding + gap-finding, NOT a blind machine-fix-rate
sample, per the honesty rule in CLAUDE.md).

## Machine/loop-resolved mistakes

- **f-string escaped quote** — `f"... {gb.score_of(\"cy\")}"`. Parser emits a
  precise hint: string literals inside an interpolation use plain quotes,
  `{f("x")}` not escaped. Descriptive fix (re-reason from prose), not a
  `replacement`.
- **giving away a bare parameter, then use-after-move** — `add(mut ref self,
  name: String, ..)` stores `name` twice, but a bare parameter borrows
  (core-semantics.md §4.1), so each store is `E0200` ("cannot give away
  parameter `name`") with `name: own String` as its `replacement`. Once `name`
  is owned, `self.index.insert(name, score)` moves it and `self.names.push(name)`
  reuses it, `E0500`, whose `replacement` clones the key
  (`self.index.insert(name.clone(), score)`); the Vec takes the original. One
  `karac fix` applies both in turn, along with the `redundant_suffix`
  fixes for `0i64` (`i64` is the default).

## Compiler finding (filed in the bug ledger)

Writing `let mut i = 0u64; while i < self.scores.len()` tripped E0200
("cannot mix integer types 'u64' and 'i64'") because `len()` returns `i64`.
Idiomatic resolution: use `i64` counters. But probing the mismatch revealed a
consistency gap — the typechecker STRICTLY rejects mixed-integer *arithmetic*
yet SILENTLY accepts implicit integer conversions (any signedness/width,
including lossy narrowing and sign-flip) at `let`-annotation, function-argument,
and return boundaries, with no `as` and no diagnostic. See the ledger entry.

## Oracle

`expected.txt` — build + run, diff stdout. Also verified interp == compiled.
