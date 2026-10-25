# Bug ledger (v2)

`docs/bugs-v2.jsonl` is the live, machine-countable record of every bug found in
`karac` since M3, when legacy was deleted. One JSON object per line, keyed by a
`B-YYYY-MM-DD-N` id (the day it was found, `N` that day's sequence). Ids continue
the pre-M3 sequence, so an id names exactly one row across this file and the
archive.

The pre-M3 ledger is archived read-only at
[`docs/legacy/bug-ledger.jsonl`](legacy/bug-ledger.jsonl), with its rollup at
[`docs/legacy/bug-ledger.md`](legacy/bug-ledger.md). The rows still open at the
split were carried here verbatim; their archive copies are `relocated` stubs
pointing at this file. A legacy bug that turns out to be live on v2 gets a FRESH
row here citing the archived one.

## The rule

1. Every bug found gets one appended line. Allocate the id as late as possible,
   off a fresh `git fetch`; on a collision, keep both rows and renumber yours.
2. A row carries the countable fields plus `detail`, which holds the
   measurements. `detail` + `fix` stay under 6 KB per row (lint rule 11): put a
   long reproduction in a corpus program and cite it in `corpus`.
3. A kata that surfaces a bug cites its id in its README.
4. Close a row with `scripts/bug-close.py` (its `--expect` assertion is
   mandatory), after the final rebase.
5. `scripts/bug-lint.sh` enforces all of this; run it after the final rebase.

## Schema

| field | values |
|---|---|
| `id` | `B-YYYY-MM-DD-N` |
| `date` | `YYYY-MM-DD`, the day it was found |
| `source` | `family[:slug]`, family one of kata · kata-gap · kata-gap-audit · selfhost · dogfood · probe · spike · internal · followup · test-infra · example |
| `surface` | typecheck · resolver · parser · lexer · ownership · effect · mir · mir-llvm · interp (the MIR interpreter) · runtime · cli · autopar · other, or a `+`-joined compound |
| `class` | miscompile · double-free · use-after-free · leak · crash · codegen-gap · missing-feature · false-positive · soundness · run-vs-build · diagnostics · perf · other |
| `severity` | high · medium · low |
| `status` | open · fixed · wontfix · relocated · invalid · not-reproduced |
| `fix` | opens with `FIXED by <sha>.` when fixed |
| `title` | one line |
| `tracker` | required for `relocated`: where the work now lives |
| `detail` | measurements, under the 6 KB cap with `fix` |
| `corpus` | optional: the corpus program that reproduces it |

## Tooling

```bash
python3 scripts/bug-curve.py --inject docs/bugs-v2.md   # refresh the block below
KARA_KATAS_DIR=../kara-katas ./scripts/bug-lint.sh      # integrity gate
grep '"status": "open"' docs/bugs-v2.jsonl              # the work queue
grep 'B-2026-10-22-33' docs/bugs-v2.jsonl docs/legacy/bug-ledger.jsonl
```


<!-- BUG-LEDGER:GENERATED:BEGIN -->

### By class

| class | total |
|---|---|
| diagnostics | 34 |
| false-positive | 34 |
| missing-feature | 26 |
| perf | 10 |
| miscompile | 8 |
| soundness | 8 |
| run-vs-build | 6 |
| other | 2 |
| codegen-gap | 2 |
| crash | 1 |

### By surface

| surface | total |
|---|---|
| typecheck | 55 |
| mir | 26 |
| codegen | 22 |
| interp | 17 |
| cli | 16 |
| ownership | 11 |
| runtime | 8 |
| mir-llvm | 7 |
| parser | 6 |
| other | 2 |
| resolver | 1 |
| lexer | 1 |
| effect | 1 |
## Current state

_Generated from `bugs-v2.jsonl` by `scripts/bug-curve.py` (2026-09-26 → 2026-10-25). Do not edit this block by hand; edit the ledger and regenerate._

### Open

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-09-26-56 | 2026-09-26 | typecheck | low | W0299 `borrow_projection_copy` IS SILENT WHEN A BORROW PROJECTION IS PASSED TO A USER CALLEE THAT KEEPS IT -- `keep(w.r, mut v)`, a storing associated function and `k.put(w.r)` each copy the field and run its `Drop` body twice on every surface, and none of them warns; the lint fires only for the `Vec.push` builtin argument and a TRAIT associated-function argument. design.md now says which call copies (d618ccbcf), so the rule to implement is settled | — |
| B-2026-09-27-76 | 2026-09-27 | ownership+interp+codegen | medium | `for g in v.into_iter()` DOES NOT CONSUME `v` OR MOVE ITS ELEMENTS, ON ALL FOUR SURFACES -- the element's `Drop` body runs after the loop instead of at the end of each iteration, TWICE when the loop moves `g` on (`let h = g`, `w.push(g)`), BEFORE the loop body reads the element when the iterator is bound first (`let it = v.into_iter(); for g in it`), and `v.len()` after the loop still compiles and prints 2, where design.md says `into_iter()` consumes the collection | — |
| B-2026-09-27-80 | 2026-09-27 | typecheck | low | THE `borrow_projection_copy` LINT (W0299) IS STILL SILENT WHEN A BORROWED VALUE IS HANDED TO A KEEPING FREE-FUNCTION CALLEE -- `for g in v { let o = f(g); }` and `fn k(s: ref S) { let o = f(s.r); }` over `fn f(x: D) -> D { x }` copy the value and run its `Drop` body twice with no warning, where `let h = g` and `let o = s.r` now warn | — |
| B-2026-09-28-33 | 2026-09-28 | typecheck+interp+codegen | medium | `partial_move_of_drop_enum` MISSES A PAYLOAD MOVED INTO A PRELUDE CONSTRUCTOR -- `match e { E.A(r) => { return Some(r); } ... }` and `let x = Some(r); return x;` over an `enum E` with its own `impl Drop` compile without the error that `E.A(r) => { return r; }` gets, and the backends then disagree about the enum's body: `--interp` prints no `dE` and jit / `-O0` / `-O2` print it once per call. The lint asks `binding_only_borrowed_with`, which reads the bare `Some(..)` callee as an entry-copying free function, so `r` looks borrowed. | — |
| B-2026-10-02-39 | 2026-10-02 | runtime+codegen | low | A growth-heavy `Map[i64, i64]` workload runs 1.63x Rust's `HashMap` at equal hashing (SipHash-1-3 both), and ties a plain C linear-probing table: kata:325's bench, 857.5 ms against 520.6 for rust_ovf | — |
| B-2026-10-14-29 | 2026-10-14 | interp+codegen | medium | The MIR borrow checker keeps a `Vec[(ref Node, i64)]` borrowed for as long as a `ref Node` copied out of one of its elements is live: `let (a, n) = v[0]; v.pop(); a.val` is refused on both MIR backends ("mutable borrow of _2 while it is borrowed", E0516), though core-semantics §1.1 makes the tuple `Copy` and the copied reference keeps the origin of the node, not of the vector; the same read out of a `Vec[ref Node]` runs, and `karac check` passes both | — |
| B-2026-10-14-39 | 2026-10-14 | codegen+runtime | low | A one-integer f-string `f"{n}"` still runs about 3x legacy on the v2 compiled backend after B-2026-10-13-20's fix: 44M `f"{i % 7}"` take 0.86 s on `karac build --mir` against 0.28 to 0.31 s on legacy `karac build`, and kata 504's bench 1.97 to 2.25 s against 1.51 to 1.62 s | — |
| B-2026-10-15-10 | 2026-10-15 | runtime | medium | NARROWED AGAIN 2026-10-19 (Thread A, d3124030ef): `std.sync.MutexGuard` has landed (`m.lock()` returns one, and a `lock` block lowers onto it; B-2026-10-19-23). Still specified-only: `std.iter.Extend` and all of `std.log`. PREVIOUSLY: NARROWED AGAIN 2026-10-19 (Thread A, 7794d0fad9): `std.process.exit` (a gated Kāra module over the native; `-> Never` now lowers) and `std.iter.DoubleEndedIterator` (already the library's trait, only its import was missing) have landed. Still specified-only: `std.sync.MutexGuard`, `std.iter.Extend` and all of `std.log`. MEASURED at 7794d0fad9: `Mutex` has no `lock()` method at all (`no method 'lock' on type 'Mutex'`), only the `lock m x { }` block (which does exclude: 8 tasks of 200000 increments each reach 1600000 on `__mir-llvm-run`, three runs of three); `Extend` has no trait signature anywhere in docs/library (README names it; collections.md gives only `Vec.extend[I: IntoIterator[Item = T]]`), and the v2 library has no `IntoIterator` trait; `std.log` needs `key: value` call fields. PREVIOUSLY: NARROWED 2026-10-19 (Thread A, read off prelude::LIBRARY_GLOBAL_MODULES at bdea5d5967): std.time (Instant partly, B-2026-10-15-112; SystemTime, Duration, sleep), std.random.next_u64 and std.uuid.v4 have landed (aff0e47fee, 3c1fb1d390, d0d9258c19); still specified-only are `std.sync.MutexGuard`, `std.iter.{DoubleEndedIterator, Extend}`, `std.process.exit` and all of `std.log`. WAS: Library items that docs/library/README.md's module table specifies are not implemented: `std.time.{Instant, SystemTime, Duration, sleep}`, `std.random.next_u64`, `std.uuid.v4`, `std.iter.{DoubleEndedIterator, Extend}`, `std.process.exit`, `std.sync.MutexGuard` and all of `std.log`; importing one now says "is in the library spec (docs/library) but not implemented yet" | — |
| B-2026-10-19-43 | 2026-10-19 | ownership+typecheck | medium | §5.8's instance check is absent: a generic instance whose body stores a view-typed value where a view may not go (pin 5.8-generic-view-instance, `put[ref String]` into a `shared` field) is accepted by `karac check`, `karac build --mir` and both MIR runs. (Narrowed 2026-10-22: §5.7's four placement rules are now refused by check too, through its MIR borrow check, as well as both MIR backends.) | — |
| B-2026-10-19-66 | 2026-10-19 | other | low | PINS: two existing core pins no longer test their rule as written: corpus/core/err_par_conflict has no comma between its par branches, so under Amendment 6 it is one branch and the MIR backends correctly run it; review ok_closure_kinds declares `f: MutFn()` bare, which D5 + §9.6 make uncallable | — |
| B-2026-10-20-32 | 2026-10-20 | typecheck+interp | medium | `IntoIterator` is not the spec's trait: the prelude's (runtime/stdlib/into_iterator.kara, CR-202) has `type Item; type IntoIter;` and no `into_iter` method, where design.md § Iteration gives `type Item; fn into_iter(own self) -> impl Iterator[Item = Self.Item]`. A spec-shaped impl is refused, a `C: IntoIterator` bound cannot call `.into_iter()`, and `v.extend(x)` on a user `IntoIterator` type passes check but the MIR builder refuses it | — |
| B-2026-10-21-22 | 2026-10-21 | typecheck | medium | `AnyError`, which docs/library/README.md lists in the prelude and design.md §10 uses for `main` and `?`, is not defined: `fn main() -> Result[(), AnyError]` is "undefined type 'AnyError'" | — |
| B-2026-10-21-29 | 2026-10-21 | ownership+typecheck | medium | `karac check` disagrees with collections.md on borrows in four places. It refuses: `a.union(b)` then `a.intersection(b)` ("value 'b' moved here"; Set and SortedSet `union`/`intersection`/`difference` take `other` bare, which borrows under D5), and `a[0] = 10` through a half of `v.split_at_mut(2)` ("declared without `mut`", while patterns cannot carry `mut`). It accepts what the page rejects: `v.len()` while both `split_at_mut` halves are live, and `s.iter_mut()` on a `Set`/`SortedSet` ("have no `iter_mut`"); the MIR backends then exit 3 on the latter. It also types a `VecDeque` with `Vec`'s whole method surface, so `d.sort()`, `d.truncate(n)` and `d.contains(x)` check though the page's VecDeque table has none of them. Later. | — |
| B-2026-10-22-16 | 2026-10-22 | other | low | The drop model refuses 10 more programs that check accepts: 4 count a Copy value (`Str`, a `T: Copy` parameter) as moved, 1 is an unparsed construct reported as a rejection, 5 not yet traced | — |
| B-2026-10-22-36 | 2026-10-22 | interp+codegen | medium | MIR builder gaps from the legacy test programs: raw pointer methods and `ptr.null`/`dangling`/`is_null` (M3, needed for FFI), associated functions and comparison methods on primitives (`i64.zero()`, `impl i64 { fn two }`, `a.lt(b)` on u8/bool), collection literals typed `VecDeque` or nested, indexing by a let-bound range, an uppercase local binding (`let F = Foo {..}; F.value`), literal patterns (negative, float, string inside `Option`), and a tail of single constructs | — |
| B-2026-10-22-43 | 2026-10-22 | typecheck | low | E0517 prints a MIR temporary instead of the source place: `let g = v[0];` over a `Vec[Fn(i64) -> i64]` reports "move of (*_7) through a shared reference"; a user-facing message must name the source place (`v[0]`) | — |
| B-2026-10-23-4 | 2026-10-23 | interp+codegen | low | Float Display breaks an exact tie between two equally short decimals away from zero, where Python's repr keeps the even last digit: -75455689800034.625 prints -75455689800034.63 on every v2 backend and in `--interp`, Python prints -75455689800034.62; library/core-types.md § Printing says only "the shortest digits that read back", so which of the two is unspecified | — |
| B-2026-10-23-16 | 2026-10-23 | cli | low | The v2 build ignores `--monomorphization-budget`, `--enable-hot-swap` and a cross `--target`, and its `--output=json` carries no typecheck warnings; 10 cli tests ignored at the M3 deletion | — |
| B-2026-10-23-17 | 2026-10-23 | mir-llvm | medium | The v2 wasm build prints a whole float with a trailing `.` (`3.` where native prints `3`) and does not lower the `report` native; 8 wasm tests ignored at the M3 deletion | — |
| B-2026-10-23-20 | 2026-10-23 | mir | medium | The v2 build refuses selfhost/ (a move out of a collection element, plus legacy-dialect code), so the 8 self-host oracle tests are ignored at the M3 deletion and the self-host fixpoint has no v2 check | — |
| B-2026-10-23-21 | 2026-10-23 | cli | low | `karac check` on a project (no file) does not refuse the archived features (WASM exports, exported C ABI, GPU) or the M4a ones by name, as single-file `check` and `build` do; and `karac_jit_runner`'s `--repl-mode` and `--test-batch` paths are dead after the M3 deletion | — |
| B-2026-10-24-1 | 2026-10-24 | cli+interp | low | The v2 REPL's remaining legacy-only pieces: `%show` and `--auto-clone` / `%set auto-clone` refuse on v2, `:provide` does not check that the provider implements the resource's trait until the next cell, and the `:provide` construction program still replays every earlier cell | — |
| B-2026-10-24-2 | 2026-10-24 | codegen | low | A long `sort_by` of shuffled pairs on the MIR compiled backend runs 1.5x legacy and about 1.9x Rust after B-2026-10-13-18's partition: kata 252's bench is 0.28-0.30 s against legacy's 0.19 s, and 1,000,000 indices sorted by key 10 times take 0.93-1.05 s against rustc -O's 0.51-0.56 s | — |
| B-2026-10-24-6 | 2026-10-24 | mir-llvm+runtime | low | After the in-line Map probe (9abd7da4c8), kata 340's sliding window still pays about 200 instructions per `remove` in karac_mir_map_remove_entry (Fenwick upkeep on a holed table, a rebuild on the first hole, and compactions) and 93 per word hash, against Rust's ~74, so v2 sits at legacy's 3.6-3.9G instructions where Rust -O ran 1.98G | — |
| B-2026-10-24-9 | 2026-10-24 | mir | medium | A closure returned from a closure is tied to the callee: `let make = |n: i64| |x: i64| x + n; fs.push(make(i));` is refused by check and both MIR columns (E0509 "the result borrows `make`"), though §9.3 makes a returned closure own its captures and the same shape through `fn adder(n: i64) -> Fn(i64) -> i64` passes | — |
| B-2026-10-24-10 | 2026-10-24 | mir | low | A borrow-check refusal names a MIR temporary: `let f = make("abc".to_string());` with `fn make(p: String) -> Fn(i64) -> i64 { |n| p.len() + n }` reports "drop of _2 while it is borrowed (borrow at 3:13) [E0516]" on check, where the reader needs the temporary `"abc".to_string()` and the binding `f` that borrows it | — |
| B-2026-10-24-11 | 2026-10-24 | mir | low | A `match` on a tuple of handle LOCALS that outlive it (`match (top_left, top_right, bottom_left, bottom_right) { (Quad.Leaf(p), ..) => .. }`, kata 558's `intersect`) still counts each handle into the built tuple and releases the tuple after the match, although the arms only read discriminants and Copy payloads | — |
| B-2026-10-24-12 | 2026-10-24 | parser | low | `karac fix` has no fix for `format!("..{}..", a, b)`: the error names the f-string to write but offers no edit, though a literal whose holes are all `{}` or `{:spec}` with one argument each maps one-to-one onto `f"..{a}..{b:spec}.."` | — |
| B-2026-10-24-13 | 2026-10-24 | parser | low | The turbofish fix deletes `::<T>` / `::[T]` from `let x = it.collect::[Vec[char]]();` and leaves `let x = it.collect();`, which then fails E0238 "cannot infer type parameter 'C'": in a `let` with no annotation the fix should move the type onto the binding, as its own message tells the author to | — |
| B-2026-10-24-28 | 2026-10-24 | ownership | medium | `f(None, None)` with a `ref Option[T]` first parameter and an owned second one is refused by `karac check`, `karac build` and `karac run`: "cannot move `None` while a borrow into it (a returned reference still points at it) is still live". The two literal `None`s are taken for one place named `None`; `__mir-run` runs the program | — |
| B-2026-10-24-31 | 2026-10-24 | parser | low | A reserved keyword (`and`) bound by a `for` loop, or as a match-arm pattern, gets the right "reserved keyword" error and then cascade errors: `Expected expression, found RightBrace` and, for the loop, a false `file contains both top-level statements and an explicit fn main()` that points at a statement inside `main` | — |
| B-2026-10-24-38 | 2026-10-24 | cli | low | `karac fix` still answers E_INDEX_MOVE_NON_COPY on a DESTRUCTURING `let (a, b) = cases[i];` (both parts only read) with `.clone()`, a deep copy of the element, instead of the `ref cases[i]` borrow the message lists first; B-2026-10-24-18's borrow fix covers only a plain `let t = v[i];` | — |
| B-2026-10-25-1 | 2026-10-25 | interp+mir-llvm | low | Indexing a `Map` with a missing key (`m[k]`, `m[k] -= 1`) stops with `runtime error: explicit panic` under `__mir-run` and `__mir-llvm-run`, a message that names no key and reads as if the program had called `panic`, where an out-of-range `Vec` index says `index out of bounds` | — |
| B-2026-10-25-6 | 2026-10-25 | typecheck | medium | An array or `Vec` literal whose elements carry unsolved inference variables is refused by `karac check`: `[Vec.new(), Vec.new()]` as `expected 'Vec[?T0]', found 'Vec[?T1]'`, and `[None, Some(1)]` as `cannot infer type parameter 'T'`. Later elements are compared with the first without unifying, while `[Some(1), None]` and an `if`/`else` join of the same values pass | — |
| B-2026-10-25-7 | 2026-10-25 | typecheck | low | E0516 on `self.m(self.f)`, where `m` takes `mut ref self` and `f` is passed to a `ref` parameter, names the method call as the place of the shared borrow (`the borrow is taken at 16:9`, the same span the error points at) instead of the argument `self.f`, and offers no fix, though `.clone()` on the argument is the mechanical one when the field's type is Clone | — |
| B-2026-10-25-8 | 2026-10-25 | parser | low | `Ok(mut b) => b.final()` in a match arm gets only prose ("rebind it where it is used: `let mut b = b;`") and no machine-applicable fix, so `karac fix` leaves it, though the edit is mechanical | — |
| B-2026-10-25-9 | 2026-10-25 | typecheck | low | `expected 'Vec[u8]', found 'Slice[u8]'` (or `'ref Vec[u8]'`) on `s.bytes()` carries no fix, though `.to_vec()` (Slice's own way back, B-2026-08-14-20) is the mechanical one | — |
| B-2026-10-25-10 | 2026-10-25 | typecheck | low | `cannot mix integer types 'i64' and 'usize'` on `n * DAYS as usize` (n a `len()`) tells the writer to add an `as` cast, though the cast is the error: lengths are `i64` (design.md §5) and dropping `as usize` is the machine fix | — |
| B-2026-10-25-11 | 2026-10-25 | typecheck | low | `Vec.filled(n, v)` and `Vec.with_capacity(n)` accept a `usize` length with no error, though passing the same `usize` to a user function's `i64` parameter is refused as a coercion that may change sign | — |
| B-2026-10-25-12 | 2026-10-25 | typecheck | medium | A `ref` to a `shared enum` cannot be used as a handle: returning it, wrapping it in `Some`, or passing it to a bare parameter is refused ('expected E, found ref E'), though the same code over a `shared struct` checks and core-semantics §4.1/§6.1 allow it | — |
| B-2026-10-25-13 | 2026-10-25 | typecheck | medium | The closure-kind walker calls three uses that move nothing a consume, so the closure is `OnceFn` and refused for an `Fn` slot: a capture passed to a user METHOD's parameter (bare or `ref`), a captured `shared` handle used as a value, and a `match` on a captured place whose arms bind nothing by move | — |
| B-2026-10-25-14 | 2026-10-25 | ownership | low | `karac check` reports a capture of an ESCAPING closure as moved by its first use inside the body: two `size(prog)` calls (`p: ref Prog`) in one escaping closure are 'value prog moved here, used again here', while both MIR backends accept the program and print the right answer | — |
| B-2026-10-25-15 | 2026-10-25 | ownership | low | The E0500 fix-it on a struct whose `Vec` element type is all-`Copy` inserts `#[derive(Copy)]` on the ELEMENT type ('add #[derive(Copy)] to Op ... so prog is copied'), which leaves the struct move-only; `karac fix` applies it and the same error remains | — |
| B-2026-10-25-18 | 2026-10-25 | mir | low | v2 project build (`karac build` with no file) does not print the legacy project-build reports (discovered modules, JSON output with resolve diagnostics, entry/src-dir refusals, manifest build target, wasm platform module, panic=unwind refusal) or the dependency summary; 12 cli tests ignored | — |
| B-2026-10-25-19 | 2026-10-25 | ownership | low | `karac fix` applies both the `: Str` fix-it on a string-literal `let` (B-2026-10-24-33) and the ownership pass's `.clone()` fix-it for a 'moved' error on the same binding, and the combined result fails check: `Str.clone()` is a `String`, and the move only existed while the binding was a `String` | — |
| B-2026-10-25-27 | 2026-10-25 | parser | medium | A function type's effect clause holds one effect: `Fn(A) -> R with writes(Stdout), receives(Channel), blocks` does not parse in a struct field (the comma ends the field), in parentheses, or in a `type` alias, so a stored callback that does two kinds of effect cannot be declared | — |
| B-2026-10-25-28 | 2026-10-25 | typecheck | low | A spawned closure that moves one half of a destructured tuple into an array literal (`group.spawn(|| count([rx]))` after `let (tx, rx) = channel(1)`) makes the checker treat the other half as borrowed by the task: a later `let _ = tx;` is refused with `tx is moved while a task of group borrows it` | — |
| B-2026-10-25-29 | 2026-10-25 | mir | low | The error return trace's JSON frames carry `file`, `line` and `column` but not design.md's `expr` (the `?` expression's text) or `context` (a `.context()` message), on both v2 executors | — |
| B-2026-10-25-30 | 2026-10-25 | mir | low | A `?` chain that crosses a task boundary prints the frames left on the exiting thread instead of design.md's `Error return trace: unavailable (the ? chain was split across parallel tasks)`; waits on M4 item 10's pool | — |
| B-2026-10-25-31 | 2026-10-25 | mir+mir-llvm | low | A table's hasher reaches v2 only as a fixed walk key on an annotated `let`: a user `BuildHasher`'s build/write/finish never run, FxBuildHasher hashes with the default, and a hasher named on a struct field, parameter, return type or turbofish is dropped | — |
| B-2026-10-25-32 | 2026-10-25 | typecheck+mir | medium | A method call on a value of an associated type of a generic bound (`let h = b.build(); h.finish()` with `B: BuildHasher`) passes `karac check` but the MIR build fails: "a type that failed to check is not lowered yet" | — |
| B-2026-10-25-35 | 2026-10-25 | cli | medium | A package's v2 check reports places in the combined source inside its diagnostics: a move-after-use message names the combined line (`moved at line 10:17` for main.kara:4:17), the snippet's gutter is misaligned, and the `--output=json` `replacement` is an offset into the combined text (here a `#[derive(Clone)]` at offset 0, which is the first module's, under `file: src/main.kara`) | — |
| B-2026-10-25-36 | 2026-10-25 | mir | medium | A match-arm binding moved into a `TaskGroup.spawn` closure that passes it to a function value (`Some(s) => { g.spawn(|| f(s)); }`, `f` an `own Fn(own String)` parameter) checks clean, but the MIR build refuses it: `cannot move a non-Copy value out of a borrowed place` | — |
| B-2026-10-25-37 | 2026-10-25 | typecheck | medium | `tx.send(v)` accepts a plain `shared` value (a `channel(4)` of a `shared struct`), so a `shared` handle can reach another task through a channel; and the TaskGroup E_NOT_CROSS_TASK help still says "the `par` form" where Amendment 6 named it `sync` | — |
| B-2026-10-25-38 | 2026-10-25 | cli | medium | In a package, a module's own function named like a library function (`pub fn take(b: own Box2)` in ast.kara) captures that name in every other module through the combined source, so a module that never imports it and calls `std.mem`'s `take(mut x)` is refused with `expected 'Box2', found 'i64'` | — |

### Fixed

<details><summary>Fixed — compact index (one-line titles; full write-up + cross-refs live in `bugs-v2.jsonl`, grep by id). The regression test is the durable artifact.</summary>

| id | surface | sev | title | fix |
|---|---|---|---|---|
| B-2026-10-03-10 | codegen+runtime | medium | (LEGACY PREMISE: on v2 the tokens were already views; the cost was two runtime calls a token, a StrSearcher build and a lossy re-borrow) `for tok in… | 83cbad997f |
| B-2026-10-04-79 | codegen+runtime | medium | AFTER B-2026-10-04-56 KATA 340'S `Map[char, i64]` BENCH STILL RUNS 1.85x RUST AT EQUAL HASHING (399 ms vs 216): each hash costs 92 instructions again… | 9abd7da4c8 |
| B-2026-10-18-52 | codegen | medium | A bare list literal used only to be walked or read, `for step in [1, 9] { . | 2dfab26e09 |
| B-2026-10-19-32 | typecheck | low | A list literal passed to a method of a generic receiver does not take the parameter's type as its expected type: `best.push([0, 1])` on a `Vec[Array[… | a10f2705e6 |
| B-2026-10-20-34 | typecheck+interp | medium | A closure returned inside its enclosing closure's result captures that closure's parameter or local, `Copy` or not, by `ref`, so `(0..n).flat_map(\|i\|… | 643e98ab75 |
| B-2026-10-20-36 | cli | low | karac fix skips the `.clone()` for a match arm whose body is a block ("applying it would not parse; please report it") while the same run also fixes… | 7deb035b43 |
| B-2026-10-21-3 | typecheck | low | An unannotated `Map.new()` or `Set.new()` whose first use is a lookup by a `Str` is fixed at a `Str` key, so the `String` insert after it is refused… | 7deb035b43 |
| B-2026-10-21-41 | typecheck+interp+codegen | medium | Connection pool factory cannot fail: docs/library/pool.md specifies `Pool[T, E]` with `new(create_fn: own Fn() -> Result[T, E], max_connections, max_… | de17752282 |
| B-2026-10-21-42 | typecheck+interp+codegen | medium | Connection pool `acquire` takes `timeout: i64` milliseconds where docs/library/pool.md specifies `timeout: Duration` (`pool.acquire(Duration.ms(500))… | de17752282 |
| B-2026-10-21-43 | typecheck+interp+codegen | low | Connection pool `with_health_check` takes `ref self` and returns a second `Pool` value where docs/library/pool.md specifies a builder taking `own sel… | de17752282 |
| B-2026-10-21-44 | runtime+codegen+interp | medium | Connection pool `acquire` never waits and has no `Busy`: at capacity it returns `Timeout` at once whatever the timeout, and `max_waiters` limits noth… | ed9152f8a5 |
| B-2026-10-21-59 | ownership+codegen | medium | `for w in words.by_ref() { flags.push(w) }` then `words.collect()` (iterators.md's by_ref example) is refused E0516 by check and the MIR borrow check… | de3fb51f66 |
| B-2026-10-21-60 | typecheck+codegen | medium | Iterator methods on a value of generic type `I: Iterator[Item = i64]` fail: check refuses `it.fold(0, \|a, x\| a + x)` ("expected 'Fn(AccV, I.Item) ->… | e8b72105df |
| B-2026-10-21-71 | typecheck | medium | `TaskGroup.spawn` does not check that the task's RESULT is `CrossTask`: `g.spawn(\|\| Node { v: 1 })` with `shared struct Node` checks and runs, where… | 58a146e1e4 |
| B-2026-10-21-76 | typecheck | medium | `for x in s` over a value that is not iterable passes `karac check` and binds the value itself: `for msg in rx {}` over a `Receiver[i64]` types `msg`… | 1f45c953c4 |
| B-2026-10-21-77 | typecheck | low | `karac __mir-run` and `__mir-llvm-run` run a program whose forwarded `mut ref` argument has no call-site `mut` marker, which `karac check` and `karac… | 7469db271d |
| B-2026-10-21-79 | typecheck | low | A type mismatch prints the string view as `StringSlice` where the source and the pages write `Str`: `Command.new("echo").args(a)` with `a: Vec[String… | f73dcc99bb |
| B-2026-10-21-80 | typecheck+codegen | low | `.into()` called straight on a unit-variant literal is refused: `let b: B = A.Y.into();` with `impl From[A] for B` gives "no method 'into' on type 'A… | eb790336d3 |
| B-2026-10-22-6 | typecheck | low | A list literal whose FIRST element is a `ref T` of a `Copy` `T` is typed `Vec[ref T]` even where a `Vec[T]` is expected: `for p in v.iter() { let u:… | ef1c77dd26 |
| B-2026-10-22-44 | cli+codegen | medium | A v2 `karac test` failure from `assert_eq`/`assert_ne` carries no `left`/`right`: the MIR assertion aborts with "assertion failed: left != right" and… | 31c59a1eb5 |
| B-2026-10-23-7 | typecheck | low | A `vec![..]` of integer literals inside a tuple literal is typed `Vec[i64]` even where the tuple's expected type says `Vec[u8]`: `let b: (String, Vec… | ef1c77dd26 |
| B-2026-10-23-8 | typecheck+interp+codegen | low | `karac check` accepts `*x` on a binding from a pattern over a bare `for` element (a destructuring `for (o, n) in v` over a `Vec[(String, i64)]`, `if… | d5513133a7 |
| B-2026-10-23-10 | typecheck | low | A top-level `fn todo(..)` (likewise `unreachable`, `panic`, and `ref_eq`, which is checked against the prelude intrinsic) is accepted but never calle… | a10f2705e6 |
| B-2026-10-23-12 | typecheck | medium | `karac check` accepts a `Map` whose value type defaulted to `i64` (`let mut m = Map.new(); m.insert(5, 9)`) where `Map[i64, u64]` is wanted as a stru… | a10f2705e6 |
| B-2026-10-23-13 | codegen | low | (NOT SMALL, kata thread 2026-10-24: 3.4x on kata 951's tuple match of two Option[shared] params, 1.31 s vs 0.39 s for nested matches) A `match` on a… | a97412f163 |
| B-2026-10-23-14 | cli | medium | v2 project mode (`karac build`/`run`/`test` with no file) refuses a whole-module `import` and a renaming `import .. | 45b9315c99 |
| B-2026-10-23-15 | cli | medium | `karac test` on v2 does not run the provider fixtures (`#[with_provider]` variants) of 8 cli tests and does not report what 6 more pin (legacy test-r… | 2c0ab534bb |
| B-2026-10-23-18 | mir | low | v2 renders runtime faults differently from the pinned output: a `main` error goes through Debug instead of its `Display` impl, a panic has no `panick… | 0bb0c20d38 |
| B-2026-10-23-19 | mir | medium | `karac run --interp` (the MIR interpreter) iterates a `Map`/`Set` in the same order on every run and ignores `KARAC_HASH_SEED`, unlike the compiled b… | e6d5b77c68 |
| B-2026-10-24-3 | interp+codegen | high | `v.push(m.get(k).unwrap())` into a `Vec[i64]` stores the ADDRESS of the map's value, not the value: on mir-llvm and `karac build --mir` the element p… | bd015b5c7c |
| B-2026-10-24-4 | mir | medium | The MIR builder refuses an adaptor called on a value of generic type `I: Iterator[Item = i64]`: `it.map(\|x\| x + 1).count()` checks, and `__mir-run` a… | bc575797b1 |
| B-2026-10-24-5 | mir | high | MIR borrowck lets the borrowed place change while a ref read through a borrowed struct's `mut ref` field is still live, a use-after-free that `karac… | 930b560ed5 |
| B-2026-10-24-7 | typecheck | low | `c as i128` and `c as u128` on a `char` are refused with a bare "cannot cast 'char' to 'i128'", though `c as i64` and `c as u64` are accepted: intege… | a10f2705e6 |
| B-2026-10-24-8 | mir | low | The MIR builder refuses `.into()` called straight on a unit-variant literal: `let b: B = A.Y.into();` checks (since eb790336d3), and `__mir-run` and… | da0b9e8019 |
| B-2026-10-24-14 | runtime | medium | A growing `VecDeque` moves its elements into the new buffer one at a time on the compiled backends: RawDeque::grow (runtime/src/mir.rs) computes each… | d34614d017 |
| B-2026-10-24-15 | typecheck+mir | low | A binding of a non-Copy payload from a `match` on a FIELD of a `mut ref` parameter (`match c.s { S.B(buf) => add(mut buf, 7) }`, `c: mut ref C`) is a… | 1f187f6814 |
| B-2026-10-24-16 | typecheck | medium | `karac check` rejects `.collect()` after an item-preserving adaptor on a generic iterator: under `I: Iterator[Item = i64]`, `it.filter(\|x\| x % 2 == 0… | 311a15fb9b |
| B-2026-10-24-17 | typecheck+interp | medium | An integer literal in a tuple literal gets no range check against its slot: `let t: (i64, u8) = (1, 300);` passes `karac check` and the MIR run print… | 311a15fb9b |
| B-2026-10-24-18 | cli | low | `karac fix` answers E_INDEX_MOVE_NON_COPY on `let t = v[i];` (a non-Copy element, only read afterwards) by adding `#[derive(Clone)]` to the element t… | 21faffb7ac |
| B-2026-10-24-19 | typecheck | low | A bare parameter, or a binding out of one, handed to `Vec.push` gets the generic borrow mismatch (no fix for a non-Clone type, `.clone()` for a Clone… | b12c3d1318 |
| B-2026-10-24-20 | mir+mir-llvm | medium | Under `KARAC_AUTO_PAR=1` a signed `+` reduction (and a checked `*`) split into tasks overflows in a run where the sequential loop never does: `acc =… | fe8fa4918f |
| B-2026-10-24-21 | typecheck | medium | `karac check` accepts passing a binding out of a place reached through a shared `ref` as a `mut` argument: `fn put(c: ref C) { match c.s { S.B(buf) =… | afb5b6c585 |
| B-2026-10-24-22 | resolver | low | A primitive type name bound as a value (`let i64 = 3;`, `let u8 = ...`, a parameter `u8: i64`, `for char in ...`, `let bool = ...`) passes `karac che… | b63bc97193 |
| B-2026-10-24-23 | typecheck | medium | `contains`, `get`, `first` and `last` on an `Array[T, N]` whose element is not a scalar (`Str`, `String`) fail with `no method 'contains' on type 'Ar… | 6484f80350 |
| B-2026-10-24-24 | typecheck | low | `s + t` with `s: String` and `t: Str` (a `Str` constant, an element of a `const` `Array[Str, N]`, a `Str` binding) reports `'+' on 'String' requires… | cd5b74be01 |
| B-2026-10-24-25 | typecheck | medium | Inside a `mut ref self` method, a bare `for x in CONST_ARRAY` over a global `const` is taken to borrow `self`, so any write to a field of `self` (or… | f0ada4a3b1 |
| B-2026-10-24-26 | typecheck | low | `.copied()` on an `Option[ref V]` (from `Map.get`, `SortedMap.get` or `Vec.get`) is `no method 'copied' on type 'Option'` with no hint and no fix, th… | 8e4b5d5263 |
| B-2026-10-24-27 | lexer+parser | low | A lexer error reaches the user wrapped as the parser's `Expected expression, found Error("…")`, Debug-escaped (and escaped again in JSON), with no fi… | cb0cb3b734 |
| B-2026-10-24-29 | mir | medium | `Map[K, V, H]` / `Set[T, H]` lose their hasher in MIR (IntrinsicTy::Map(K, V)), so `FxBuildHasher` and a user `BuildHasher` are ignored on both v2 ba… | 4fe63393b1 |
| B-2026-10-24-30 | typecheck | low | A bare parameter inside a tuple literal handed to an owned slot (`v.push((t, value))`), or given as `Map.entry(key)`'s owned key, gets the generic `e… | 5059227eeb |
| B-2026-10-24-32 | ownership | low | E0500 FALSE POSITIVE ON A SECOND `unwrap_or` OF A COPY `Option[i64]` FIELD -- `p.a.unwrap_or(0)` twice is refused as a move when `p` is an owned loca… | ea1fd651e8 |
| B-2026-10-24-33 | typecheck | low | A string literal bound with no type (`let mut how = "";`) is a `String`, as design.md §3 says, but using the binding where a `Str` is expected (a `St… | a3d8d378ae |
| B-2026-10-24-34 | mir | medium | Auto-par statement verdicts (`KARAC_AUTO_PAR=1`) drop every use in an arm that ends in `return`, not only its drops and its write of the result, so `… | b068b8f173 |
| B-2026-10-24-35 | mir | medium | Auto-par statement verdicts (`KARAC_AUTO_PAR=1`) let a statement that may panic run beside an earlier one that may return, so `if grow(n) < 10 { retu… | b068b8f173 |
| B-2026-10-24-36 | typecheck+mir | low | A `Copy` binding destructured from a borrowed value takes `*` inconsistently: `karac check` refuses `*w` after `let (_, w) = q.front().unwrap()` or `… | 76285f0a59 |
| B-2026-10-24-37 | typecheck | low | E0517's fix-it for a write through a BARE parameter (`fn f(items: Vec[String]) { items.sort() }`, or through a local bound from it, `let mut s = item… | e99070a60e |
| B-2026-10-24-39 | cli | medium | `karac check` on a project (no file) checks against the checker's built-in legacy library signatures, not the v2 library the build appends, so a pack… | 0a647b39e0 |
| B-2026-10-24-40 | typecheck | low | `karac check` says `CODES.find(s)` MOVES its String argument when the receiver is a `const` Str (`const CODES: Str = "LRI";`), so a later use of `s`… | be69aae3e9 |
| B-2026-10-24-41 | effect | medium | `karac check` accepts a public function that reads a socket under `with sends(Network) receives(Network)`: the library's socket, TLS, UDP, WebSocket… | 86104518db |
| B-2026-10-24-42 | typecheck | low | `s.as_bytes()` on a `String` is a bare E0236 `no method 'as_bytes' on type 'String'` with no note and no fix-it, though `String.bytes()` is the metho… | 4f6892cea5 |
| B-2026-10-24-43 | cli | medium | A package with more than one module cannot name a v2-only library item (`WsMessage`, `TlsFailure`): its per-module package check resolves against the… | 3c10118a4d |
| B-2026-10-25-2 | mir-llvm | medium | A built program dies with exit 141 (SIGPIPE) when it writes to a child's stdin after the child exits; the MIR interpreter returns an error from the s… | e09926a988 |
| B-2026-10-25-3 | mir | low | v2 prints no error return trace for a `?` chain: `main` returning a propagated `Err` prints `Error: e` alone on both backends, and a panic's trace is… | f2ff086fc1 |
| B-2026-10-25-4 | cli | medium | A package with more than one module is checked without D5: its per-module passes and late phases read a bare parameter as owned, so the check in fron… | f36a5e4595 |
| B-2026-10-25-5 | cli | medium | `karac check` on a package with more than one module runs only the module-aware package check, not the v2 check of the combined source that `karac ru… | 3baf757f4d |
| B-2026-10-25-16 | mir | medium | The MIR move check treats a `shared` handle captured by an ESCAPING closure as moved, so a second closure over it, or any later use, is 'use of _1, w… | f6b4ec231b |
| B-2026-10-25-17 | mir+mir-llvm | high | A closure passed to a user function's `escaping` parameter that captures a BARE (borrowed) parameter of non-Copy type, or a field of one, captures it… | f6b4ec231b |
| B-2026-10-25-20 | ownership | high | `karac check` accepts an ESCAPING closure that uses a bare (borrowed) non-Copy, non-handle parameter as a whole (`new_op(\|x\| x + size(p))`, `p: P` wi… | 5f1a515ae0 |
| B-2026-10-25-21 | interp | medium | On the MIR interpreter, a group's deadline never tears down the task that ran last before it fired: that task is marked cancelled but stays parked, a… | c2f859dacc |
| B-2026-10-25-22 | mir | low | `IsolatingGroup.spawn` of a named function (`g.spawn(work)`) is refused at build with `an isolating group's spawn of other than a closure written the… | 6fcffec0b3 |
| B-2026-10-25-23 | mir | low | v2 does not mark a panic raised while a `requires`/`ensures` predicate is evaluated (inline or in a callee) as `contract predicate panicked: ..`, so… | f82c28e5e9 |
| B-2026-10-25-24 | typecheck | medium | `karac check` accepts `with_provider[Clock](FakeClock {..}, ..)` and `Clock.now()` when the program redeclares `effect resource Clock;` and `FakeCloc… | 16eec2e53b |
| B-2026-10-25-25 | typecheck+ownership | medium | WIDER THAN FILED: `karac check` accepts a method taking `own self` on ANY borrowed receiver (a `ref` parameter, a bare one, a bare `for` element of a… | 5e1f7f9923 |
| B-2026-10-25-26 | cli | medium | `karac fix` on a file of a package fixes it as a single file: it cannot see the package's other modules, so a resolver 'did you mean' rewrites a corr… | 66683e8bf5 |
| B-2026-10-25-33 | typecheck | medium | `karac check` refuses a bare use of a program's own variant named like a `std.regex` / `std.json` item (a pattern `Match(n)` on `enum E { Match(i64)… | 66683e8bf5 |
| B-2026-10-25-34 | typecheck | high | In a package, a function a module imports from a sibling (`import ast.{take}`) is typed as the library's generic `take(mut ref T) -> T` in the import… | 01e8f145bb |

</details>

<!-- BUG-LEDGER:GENERATED:END -->
