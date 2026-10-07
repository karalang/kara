lines: 264

build: ok
interp: same
mirror: agrees

stmt-par: L238-L242 five independent fs.read_to_string calls (wf_harbor/garden/server/library/kitchen.txt)

shared types: none

workarounds:
- L98-L112 `FreqTable.top`: the natural form was a user `impl PartialEq/Eq/PartialOrd/Ord for WordCount` (count descending, then word ascending) followed by `ranked.sort()`. It compiled, but `Vec.sort()` ignored the user `Ord` impl on BOTH backends (build and --interp) and sorted by the struct's fields in declaration order (word first), so output was purely alphabetical. Minimal probe: two-element Vec of a struct whose `cmp` orders by count desc; `v.sort()` left it in ascending/field order while `a.cmp(b)` returned the right Ordering. Replaced with a `Vec[(i64, String)]` of `(-count, word)` sort keys and tuple `sort()`, then rebuilt `WordCount`s.
- L146-L147 `match fs.write(...) { Ok(()) => {} ... }`: rejected with `error[typecheck]: non-exhaustive match: pattern 'Ok(_)' not covered` — the unit pattern `()` is not treated as exhaustive for `()`. Changed to `Ok(_) => {}`.
- L78 and L86/L101 `Map.get` / map `for` values: spec says `get` returns `Option[ref V]` and bare `for (k, v) in map` yields `ref V`, so I wrote `Some(c) => *c` and `*count`; compiler rejected with `error[typecheck]: unary '*' requires 'ref T', 'mut ref T', or a raw pointer ... found 'i64'`. Dropped the `*`.

ignored diagnostics: 2 (both `error[borrow_projection_copy]` from `karac check`: L53 `words.insert(w)` for a bare-`for` element, and L207 `table.add(word, 1)` after `stop.contains(word)`); build succeeded regardless.
