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
| false-positive | 17 |
| diagnostics | 16 |
| missing-feature | 16 |
| perf | 10 |
| miscompile | 5 |
| soundness | 4 |
| run-vs-build | 4 |
| other | 2 |
| codegen-gap | 2 |

### By surface

| surface | total |
|---|---|
| typecheck | 35 |
| codegen | 22 |
| interp | 15 |
| mir | 11 |
| runtime | 8 |
| cli | 8 |
| ownership | 4 |
| mir-llvm | 3 |
| other | 2 |
| parser | 2 |
| resolver | 1 |
## Current state

_Generated from `bugs-v2.jsonl` by `scripts/bug-curve.py` (2026-09-26 → 2026-10-24). Do not edit this block by hand; edit the ledger and regenerate._

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
| B-2026-10-20-34 | 2026-10-20 | typecheck+interp | medium | A closure returned inside its enclosing closure's result captures that closure's parameter or local, `Copy` or not, by `ref`, so `(0..n).flat_map(|i| (i + 1..n).map(|j| i * 10 + j))` is refused (E0509 'the result borrows `i`', or E0516 for a local) by `karac check` and by MIR borrowck; core-semantics.md §9.1 ('a place that dies first is moved in') and §9.3 (a returned closure escapes and captures by move) both make the capture a copy, and Kāra has no `move` to ask for one | — |
| B-2026-10-21-22 | 2026-10-21 | typecheck | medium | `AnyError`, which docs/library/README.md lists in the prelude and design.md §10 uses for `main` and `?`, is not defined: `fn main() -> Result[(), AnyError]` is "undefined type 'AnyError'" | — |
| B-2026-10-21-29 | 2026-10-21 | ownership+typecheck | medium | `karac check` disagrees with collections.md on borrows in four places. It refuses: `a.union(b)` then `a.intersection(b)` ("value 'b' moved here"; Set and SortedSet `union`/`intersection`/`difference` take `other` bare, which borrows under D5), and `a[0] = 10` through a half of `v.split_at_mut(2)` ("declared without `mut`", while patterns cannot carry `mut`). It accepts what the page rejects: `v.len()` while both `split_at_mut` halves are live, and `s.iter_mut()` on a `Set`/`SortedSet` ("have no `iter_mut`"); the MIR backends then exit 3 on the latter. It also types a `VecDeque` with `Vec`'s whole method surface, so `d.sort()`, `d.truncate(n)` and `d.contains(x)` check though the page's VecDeque table has none of them. Later. | — |
| B-2026-10-21-44 | 2026-10-21 | runtime+codegen+interp | medium | Connection pool `acquire` never waits and has no `Busy`: at capacity it returns `Timeout` at once whatever the timeout, and `max_waiters` limits nothing, where docs/library/pool.md specifies waiting up to the timeout for a value to come back and `PoolError.Busy` at once when `max_waiters` callers are already waiting | — |
| B-2026-10-22-16 | 2026-10-22 | other | low | The drop model refuses 10 more programs that check accepts: 4 count a Copy value (`Str`, a `T: Copy` parameter) as moved, 1 is an unparsed construct reported as a rejection, 5 not yet traced | — |
| B-2026-10-22-36 | 2026-10-22 | interp+codegen | medium | MIR builder gaps from the legacy test programs: raw pointer methods and `ptr.null`/`dangling`/`is_null` (M3, needed for FFI), associated functions and comparison methods on primitives (`i64.zero()`, `impl i64 { fn two }`, `a.lt(b)` on u8/bool), collection literals typed `VecDeque` or nested, indexing by a let-bound range, an uppercase local binding (`let F = Foo {..}; F.value`), literal patterns (negative, float, string inside `Option`), and a tail of single constructs | — |
| B-2026-10-22-43 | 2026-10-22 | typecheck | low | E0517 prints a MIR temporary instead of the source place: `let g = v[0];` over a `Vec[Fn(i64) -> i64]` reports "move of (*_7) through a shared reference"; a user-facing message must name the source place (`v[0]`) | — |
| B-2026-10-23-4 | 2026-10-23 | interp+codegen | low | Float Display breaks an exact tie between two equally short decimals away from zero, where Python's repr keeps the even last digit: -75455689800034.625 prints -75455689800034.63 on every v2 backend and in `--interp`, Python prints -75455689800034.62; library/core-types.md § Printing says only "the shortest digits that read back", so which of the two is unspecified | — |
| B-2026-10-23-8 | 2026-10-23 | typecheck+interp+codegen | low | `karac check` accepts `*x` on a binding from a pattern over a bare `for` element (a destructuring `for (o, n) in v` over a `Vec[(String, i64)]`, `if let Some(f) = g` inside `for g in v` over a `Vec[Option[String]]`, or a `match` arm `Seg.Code(v)` inside `for s in p.segs` over a field of a borrowed struct, where `v` is an `i64`), typing it as a borrow, while every MIR front end (`__mir-run`, `__mir-llvm-run`, `build --mir`) types it as the value type and refuses the `*` | — |
| B-2026-10-23-14 | 2026-10-23 | cli | medium | v2 project mode (`karac build`/`run`/`test` with no file) refuses a whole-module `import` and a renaming `import ... as`, does not combine two modules' same-named items, and does not print the dependency summary or the legacy project-build reports; 19 cli tests ignored at the M3 deletion | — |
| B-2026-10-23-15 | 2026-10-23 | cli | medium | `karac test` on v2 does not run the provider fixtures (`#[with_provider]` variants) of 8 cli tests and does not report what 6 more pin (legacy test-runner output); ignored at the M3 deletion | — |
| B-2026-10-23-16 | 2026-10-23 | cli | low | The v2 build ignores `--monomorphization-budget`, `--enable-hot-swap` and a cross `--target`, and its `--output=json` carries no typecheck warnings; 10 cli tests ignored at the M3 deletion | — |
| B-2026-10-23-17 | 2026-10-23 | mir-llvm | medium | The v2 wasm build prints a whole float with a trailing `.` (`3.` where native prints `3`) and does not lower the `report` native; 8 wasm tests ignored at the M3 deletion | — |
| B-2026-10-23-18 | 2026-10-23 | mir | low | v2 renders runtime faults differently from the pinned output: a `main` error goes through Debug instead of its `Display` impl, a panic has no `panicked` line, a contract violation loses its message, the error return trace is missing on some paths, and a tail `Err` runs its errdefers and defers in a different order; 14 tests ignored at the M3 deletion | — |
| B-2026-10-23-19 | 2026-10-23 | mir | medium | `karac run --interp` (the MIR interpreter) iterates a `Map`/`Set` in the same order on every run and ignores `KARAC_HASH_SEED`, unlike the compiled backend and design.md § Hash (per-process random key); 3 tests ignored at the M3 deletion | — |
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
| B-2026-10-24-19 | 2026-10-24 | typecheck | low | A bare parameter, or a binding out of one, handed to `Vec.push` gets the generic borrow mismatch (no fix for a non-Clone type, `.clone()` for a Clone one) instead of the "cannot give away parameter" message and `own` fix that `return e` gets in the same function | — |
| B-2026-10-24-22 | 2026-10-24 | resolver | low | A primitive type name bound as a value (`let i64 = 3;`, `let u8 = ...`, a parameter `u8: i64`, `for char in ...`, `let bool = ...`) passes `karac check` and runs, though design.md §3 says primitive type names "cannot be declared or bound as names: `let i64 = 3;` is an error". The one place it fails is a method call on the binding, `u8.len()`, which reports `no associated function 'len' on type 'u8'` at the use and never names the binding | — |
| B-2026-10-24-23 | 2026-10-24 | typecheck | medium | `contains`, `get`, `first` and `last` on an `Array[T, N]` whose element is not a scalar (`Str`, `String`) fail with `no method 'contains' on type 'Array'`, though library/collections.md says an `Array[T, N]` has every `Slice[T]` method. The same calls on `Array[i64, N]` work, and so does `.as_slice().contains(..)` on the Str array | src/typechecker/method_sequence_mutation.rs (fixed-array read-only arm) |
| B-2026-10-24-24 | 2026-10-24 | typecheck | low | `s + t` with `s: String` and `t: Str` (a `Str` constant, an element of a `const` `Array[Str, N]`, a `Str` binding) reports `'+' on 'String' requires a 'String' right operand, found 'Str'` with no fix, while the matching mismatch at a call (`Str` passed to `ref String`) carries a machine-applicable `.to_string()` that `karac fix` applies. The error's class is also `INVALID_UNARY_OP` for a binary operator | — |
| B-2026-10-24-25 | 2026-10-24 | typecheck | medium | Inside a `mut ref self` method, a bare `for x in CONST_ARRAY` over a global `const` is taken to borrow `self`, so any write to a field of `self` (or a call of a method that writes `self`) in the loop body is refused with E0516 "while the `for` loop over `CONST` borrows it". The same loop over `CONST.iter()`, or in a free function over a `mut ref` parameter, passes | — |
| B-2026-10-24-26 | 2026-10-24 | typecheck | low | `.copied()` on an `Option[ref V]` (from `Map.get`, `SortedMap.get` or `Vec.get`) is `no method 'copied' on type 'Option'` with no hint and no fix, though the library has `.cloned()` for exactly this and documents `m.get(k) ?? d` as the idiom for a `Copy` value; `.copied()` is the Rust spelling an author coming from Rust writes first | — |

### Fixed

<details><summary>Fixed — compact index (one-line titles; full write-up + cross-refs live in `bugs-v2.jsonl`, grep by id). The regression test is the durable artifact.</summary>

| id | surface | sev | title | fix |
|---|---|---|---|---|
| B-2026-10-03-10 | codegen+runtime | medium | (LEGACY PREMISE: on v2 the tokens were already views; the cost was two runtime calls a token, a StrSearcher build and a lossy re-borrow) `for tok in… | 83cbad997f |
| B-2026-10-04-79 | codegen+runtime | medium | AFTER B-2026-10-04-56 KATA 340'S `Map[char, i64]` BENCH STILL RUNS 1.85x RUST AT EQUAL HASHING (399 ms vs 216): each hash costs 92 instructions again… | 9abd7da4c8 |
| B-2026-10-18-52 | codegen | medium | A bare list literal used only to be walked or read, `for step in [1, 9] { . | 2dfab26e09 |
| B-2026-10-19-32 | typecheck | low | A list literal passed to a method of a generic receiver does not take the parameter's type as its expected type: `best.push([0, 1])` on a `Vec[Array[… | a10f2705e6 |
| B-2026-10-20-36 | cli | low | karac fix skips the `.clone()` for a match arm whose body is a block ("applying it would not parse; please report it") while the same run also fixes… | 7deb035b43 |
| B-2026-10-21-3 | typecheck | low | An unannotated `Map.new()` or `Set.new()` whose first use is a lookup by a `Str` is fixed at a `Str` key, so the `String` insert after it is refused… | 7deb035b43 |
| B-2026-10-21-41 | typecheck+interp+codegen | medium | Connection pool factory cannot fail: docs/library/pool.md specifies `Pool[T, E]` with `new(create_fn: own Fn() -> Result[T, E], max_connections, max_… | de17752282 |
| B-2026-10-21-42 | typecheck+interp+codegen | medium | Connection pool `acquire` takes `timeout: i64` milliseconds where docs/library/pool.md specifies `timeout: Duration` (`pool.acquire(Duration.ms(500))… | de17752282 |
| B-2026-10-21-43 | typecheck+interp+codegen | low | Connection pool `with_health_check` takes `ref self` and returns a second `Pool` value where docs/library/pool.md specifies a builder taking `own sel… | de17752282 |
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
| B-2026-10-23-10 | typecheck | low | A top-level `fn todo(..)` (likewise `unreachable`, `panic`, and `ref_eq`, which is checked against the prelude intrinsic) is accepted but never calle… | a10f2705e6 |
| B-2026-10-23-12 | typecheck | medium | `karac check` accepts a `Map` whose value type defaulted to `i64` (`let mut m = Map.new(); m.insert(5, 9)`) where `Map[i64, u64]` is wanted as a stru… | a10f2705e6 |
| B-2026-10-23-13 | codegen | low | (NOT SMALL, kata thread 2026-10-24: 3.4x on kata 951's tuple match of two Option[shared] params, 1.31 s vs 0.39 s for nested matches) A `match` on a… | a97412f163 |
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
| B-2026-10-24-20 | mir+mir-llvm | medium | Under `KARAC_AUTO_PAR=1` a signed `+` reduction (and a checked `*`) split into tasks overflows in a run where the sequential loop never does: `acc =… | fe8fa4918f |
| B-2026-10-24-21 | typecheck | medium | `karac check` accepts passing a binding out of a place reached through a shared `ref` as a `mut` argument: `fn put(c: ref C) { match c.s { S.B(buf) =… | afb5b6c585 |

</details>

<!-- BUG-LEDGER:GENERATED:END -->
