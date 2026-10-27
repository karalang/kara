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
| false-positive | 47 |
| diagnostics | 38 |
| missing-feature | 33 |
| soundness | 14 |
| perf | 13 |
| miscompile | 10 |
| run-vs-build | 9 |
| other | 4 |
| codegen-gap | 3 |
| crash | 1 |

### By surface

| surface | total |
|---|---|
| typecheck | 71 |
| mir | 36 |
| codegen | 22 |
| interp | 20 |
| cli | 18 |
| ownership | 17 |
| mir-llvm | 14 |
| runtime | 10 |
| parser | 6 |
| other | 2 |
| resolver | 2 |
| effect | 2 |
| lexer | 1 |
## Current state

_Generated from `bugs-v2.jsonl` by `scripts/bug-curve.py` (2026-09-26 → 2026-10-27). Do not edit this block by hand; edit the ledger and regenerate._

### Open

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-09-28-33 | 2026-09-28 | typecheck+interp+codegen | medium | `partial_move_of_drop_enum` MISSES A PAYLOAD MOVED INTO A PRELUDE CONSTRUCTOR -- `match e { E.A(r) => { return Some(r); } ... }` and `let x = Some(r); return x;` over an `enum E` with its own `impl Drop` compile without the error that `E.A(r) => { return r; }` gets, and the backends then disagree about the enum's body: `--interp` prints no `dE` and jit / `-O0` / `-O2` print it once per call. The lint asks `binding_only_borrowed_with`, which reads the bare `Some(..)` callee as an entry-copying free function, so `r` looks borrowed. | — |
| B-2026-10-02-39 | 2026-10-02 | runtime+codegen | low | A growth-heavy `Map[i64, i64]` workload runs 1.63x Rust's `HashMap` at equal hashing (SipHash-1-3 both), and ties a plain C linear-probing table: kata:325's bench, 857.5 ms against 520.6 for rust_ovf | — |
| B-2026-10-15-10 | 2026-10-15 | runtime | medium | NARROWED AGAIN 2026-10-19 (Thread A, d3124030ef): `std.sync.MutexGuard` has landed (`m.lock()` returns one, and a `lock` block lowers onto it; B-2026-10-19-23). Still specified-only: `std.iter.Extend` and all of `std.log`. PREVIOUSLY: NARROWED AGAIN 2026-10-19 (Thread A, 7794d0fad9): `std.process.exit` (a gated Kāra module over the native; `-> Never` now lowers) and `std.iter.DoubleEndedIterator` (already the library's trait, only its import was missing) have landed. Still specified-only: `std.sync.MutexGuard`, `std.iter.Extend` and all of `std.log`. MEASURED at 7794d0fad9: `Mutex` has no `lock()` method at all (`no method 'lock' on type 'Mutex'`), only the `lock m x { }` block (which does exclude: 8 tasks of 200000 increments each reach 1600000 on `__mir-llvm-run`, three runs of three); `Extend` has no trait signature anywhere in docs/library (README names it; collections.md gives only `Vec.extend[I: IntoIterator[Item = T]]`), and the v2 library has no `IntoIterator` trait; `std.log` needs `key: value` call fields. PREVIOUSLY: NARROWED 2026-10-19 (Thread A, read off prelude::LIBRARY_GLOBAL_MODULES at bdea5d5967): std.time (Instant partly, B-2026-10-15-112; SystemTime, Duration, sleep), std.random.next_u64 and std.uuid.v4 have landed (aff0e47fee, 3c1fb1d390, d0d9258c19); still specified-only are `std.sync.MutexGuard`, `std.iter.{DoubleEndedIterator, Extend}`, `std.process.exit` and all of `std.log`. WAS: Library items that docs/library/README.md's module table specifies are not implemented: `std.time.{Instant, SystemTime, Duration, sleep}`, `std.random.next_u64`, `std.uuid.v4`, `std.iter.{DoubleEndedIterator, Extend}`, `std.process.exit`, `std.sync.MutexGuard` and all of `std.log`; importing one now says "is in the library spec (docs/library) but not implemented yet" | — |
| B-2026-10-22-43 | 2026-10-22 | typecheck | low | E0517 prints a MIR temporary instead of the source place: `let g = v[0];` over a `Vec[Fn(i64) -> i64]` reports "move of (*_7) through a shared reference"; a user-facing message must name the source place (`v[0]`) | — |
| B-2026-10-24-2 | 2026-10-24 | codegen | low | A long `sort_by` of shuffled pairs on the MIR compiled backend runs 1.5x legacy and about 1.9x Rust after B-2026-10-13-18's partition: kata 252's bench is 0.28-0.30 s against legacy's 0.19 s, and 1,000,000 indices sorted by key 10 times take 0.93-1.05 s against rustc -O's 0.51-0.56 s | — |
| B-2026-10-24-6 | 2026-10-24 | mir-llvm+runtime | low | After the in-line Map probe (9abd7da4c8), kata 340's sliding window still pays about 200 instructions per `remove` in karac_mir_map_remove_entry (Fenwick upkeep on a holed table, a rebuild on the first hole, and compactions) and 79 per word hash (93 before db4018cfb6), against Rust's ~74, so v2 sits at legacy's 3.6-3.9G instructions where Rust -O ran 1.98G | — |
| B-2026-10-25-31 | 2026-10-25 | mir+mir-llvm | low | A table's hasher is not part of its type on v2, where collections.md makes `H` the last type parameter of `Map[K, V, H]` / `Set[T, H]`: the parser strips it, so a table built without the annotation in sight walks in the seeded order, and no BuildHasher's own code runs (a user hasher's build/write/finish never run, and an Fx table hashes with SipHash). RULED 10-27 by the review thread: `H` becomes a real type parameter. Stage 1 is the type, with the default filled in where `H` is omitted; stage 2 runs the builder's code. Already fixed: annotated lets, plain struct fields, returns, owned params, shared fields, and qualified constructors (5ed688429b) | — |
| B-2026-10-26-17 | 2026-10-26 | mir+mir-llvm | medium | After B-2026-10-26-14's fixes, at 9030678cd0 `KARAC_AUTO_PAR=1` builds still run under the M4b floor of 0.9x their own seq build: on one worker, the default, kata 28 brute_force 0.26x, 191 popcount 0.36x, 4 binary_search_partition 0.56x, 9 palindrome 0.70x and 8 atoi 0.75x (11 programs under 0.9x in all) (CORRECTED: hotel_audit, under on 4 workers in the first three timings, ran 0.97 / 0.98 / 0.92 in three more 21-run timings, inside the swing of a no-site control; no 4-worker app is reliably under at this box's resolution) (Thread B: hotel_audit's two small tasked loops are not the cause either, re-timed in three 31-run passes; the one-worker katas pay for the task form itself, see detail) | — |
| B-2026-10-27-1 | 2026-10-27 | ownership | low | `karac check` refuses returning a `Vec[(ref Node, i64)]` whose pushed tuples hold a borrow of a parameter ("returned borrowed collection holds an element that borrows function-local storage"), because the pinning check reads the tuple literal `(c, 1)` as a local source; the same push of a bare `c` into a `Vec[ref Node]` passes, and the MIR borrow checker accepts both | — |
| B-2026-10-27-4 | 2026-10-27 | cli | low | The v2 build refuses `--enable-hot-swap` on a default-profile project and a cross `--target` / `[build].target` by name, where legacy honoured both; 4 cli tests stay ignored | — |
| B-2026-10-27-7 | 2026-10-27 | typecheck | low | A `let p: P = Json.deserialize(s).map_err(f)?` (or `.context(msg)?`) is refused with "cannot infer type parameter 'T' of `Result[T, DeserializeError]`": the annotation on the `let` does not reach the receiver of a method whose `Ok` type is the receiver's, through the `?` | — |
| B-2026-10-27-8 | 2026-10-27 | mir | low | A `par` branch that calls a captured, annotated `let` (`let f: Fn(i64) -> i64 with reads(Clock) = ..; par { f(3), f(4) }`) is still refused as `unknown (a call through a value)`: the declared clause reaches a call through a field and through a let in its own body, not through a capture | — |
| B-2026-10-27-9 | 2026-10-27 | typecheck | high | An impl method's signature is not checked against its trait's: `impl Mk2 for S { type A = bool; fn mk2(self) -> Option[i64] { Some(7) } }` under `trait Mk2 { type A; fn mk2(self) -> Option[Self.A]; }` passes check, and `fn get[T: Mk2[A = bool]](t: T) -> bool { t.mk2().unwrap() }` prints `7` for a `bool`; an impl binding an associated type its trait does not declare (`type B = bool`) is accepted too | — |
| B-2026-10-27-10 | 2026-10-27 | mir | medium | A trait impl's method named like an inherent method of the same type shares its MIR instance name (`Bag.into_iter`), so a call through the bound (`c.into_iter()` under `C: IntoIterator`) runs the inherent body: prints 7 where the trait impl's body gives 100 | — |
| B-2026-10-27-11 | 2026-10-27 | typecheck+mir | medium | `Iterable` (design.md § `Iterator` and `Iterable`, a lang-item trait: `type Item; fn iter(self) -> impl Iterator[Item = Self.Item]`) does not exist, so `for x in c` over a user collection cannot borrow it: `impl Iterable for Bag` is 'undefined type Iterable' | — |

### Relocated

<details><summary>Relocated — real and still scheduled, but with no action item today and a concrete external trigger, so the work now lives on a canonical tracker. Not wontfix (that is 'measured to a standstill'); follow the tracker column.</summary>

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-10-27-6 | 2026-10-27 | mir-llvm | low | The v2 wasm build always emits a core module: `--bindings=component` (the wasm_wasi project default) does not embed the WIT world or componentize, `[toolchain] wasm-tools` is never consulted, and a `host fn` imports from `kara_host` instead of the canonical-ABI `kara:<pkg>/host`; 2 cli tests ignored | docs/deferred.md § Web: Bindings, component model, entry points |

</details>

### Fixed

<details><summary>Fixed — compact index (one-line titles; full write-up + cross-refs live in `bugs-v2.jsonl`, grep by id). The regression test is the durable artifact.</summary>

| id | surface | sev | title | fix |
|---|---|---|---|---|
| B-2026-09-26-56 | typecheck | low | W0299 `borrow_projection_copy` IS SILENT WHEN A BORROW PROJECTION IS PASSED TO A USER CALLEE THAT KEEPS IT -- `keep(w.r, mut v)`, a storing associate… | 52ee6bb0ae |
| B-2026-09-27-76 | ownership+interp+codegen | medium | `for g in v.into_iter()` DOES NOT CONSUME `v` OR MOVE ITS ELEMENTS, ON ALL FOUR SURFACES -- the element's `Drop` body runs after the loop instead of… | d3473e873d |
| B-2026-09-27-80 | typecheck | low | THE `borrow_projection_copy` LINT (W0299) IS STILL SILENT WHEN A BORROWED VALUE IS HANDED TO A KEEPING FREE-FUNCTION CALLEE -- `for g in v { let o =… | 52ee6bb0ae |
| B-2026-10-03-10 | codegen+runtime | medium | (LEGACY PREMISE: on v2 the tokens were already views; the cost was two runtime calls a token, a StrSearcher build and a lossy re-borrow) `for tok in… | 83cbad997f |
| B-2026-10-04-79 | codegen+runtime | medium | AFTER B-2026-10-04-56 KATA 340'S `Map[char, i64]` BENCH STILL RUNS 1.85x RUST AT EQUAL HASHING (399 ms vs 216): each hash costs 92 instructions again… | 9abd7da4c8 |
| B-2026-10-14-29 | interp+codegen | medium | The MIR borrow checker keeps a `Vec[(ref Node, i64)]` borrowed for as long as a `ref Node` copied out of one of its elements is live: `let (a, n) = v… | 2c6b4da665 |
| B-2026-10-14-39 | codegen+runtime | low | A one-integer f-string `f"{n}"` still runs about 3x legacy on the v2 compiled backend after B-2026-10-13-20's fix: 44M `f"{i % 7}"` take 0.86 s on `k… | bc3451e2bc |
| B-2026-10-18-52 | codegen | medium | A bare list literal used only to be walked or read, `for step in [1, 9] { . | 2dfab26e09 |
| B-2026-10-19-32 | typecheck | low | A list literal passed to a method of a generic receiver does not take the parameter's type as its expected type: `best.push([0, 1])` on a `Vec[Array[… | a10f2705e6 |
| B-2026-10-19-43 | ownership+typecheck | medium | §5.8's instance check is absent: a generic instance whose body stores a view-typed value where a view may not go (pin 5.8-generic-view-instance, `put… | d5df59923c |
| B-2026-10-19-66 | other | low | PINS: two existing core pins no longer test their rule as written: corpus/core/err_par_conflict has no comma between its par branches, so under Amend… | 906f96ac99 |
| B-2026-10-20-32 | typecheck+interp | medium | `IntoIterator` is not the spec's trait: the prelude's (runtime/stdlib/into_iterator.kara, CR-202) has `type Item; type IntoIter;` and no `into_iter`… | dc28b584ce |
| B-2026-10-20-34 | typecheck+interp | medium | A closure returned inside its enclosing closure's result captures that closure's parameter or local, `Copy` or not, by `ref`, so `(0..n).flat_map(\|i\|… | 643e98ab75 |
| B-2026-10-20-36 | cli | low | karac fix skips the `.clone()` for a match arm whose body is a block ("applying it would not parse; please report it") while the same run also fixes… | 7deb035b43 |
| B-2026-10-21-3 | typecheck | low | An unannotated `Map.new()` or `Set.new()` whose first use is a lookup by a `Str` is fixed at a `Str` key, so the `String` insert after it is refused… | 7deb035b43 |
| B-2026-10-21-22 | typecheck | medium | `AnyError`, which docs/library/README.md lists in the prelude and design.md §10 uses for `main` and `?`, is not defined: `fn main() -> Result[(), Any… | edadb9212d |
| B-2026-10-21-29 | ownership+typecheck | medium | `karac check` disagrees with collections.md on borrows in four places | 4344167c8b |
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
| B-2026-10-22-16 | other | low | The drop model refuses 10 more programs that check accepts: 4 count a Copy value (`Str`, a `T: Copy` parameter) as moved, 1 is an unparsed construct… | f4f7c2283c |
| B-2026-10-22-36 | interp+codegen | medium | MIR builder gaps from the legacy test programs: raw pointer methods and `ptr.null`/`dangling`/`is_null` (M3, needed for FFI), associated functions an… | a13217b167 |
| B-2026-10-22-44 | cli+codegen | medium | A v2 `karac test` failure from `assert_eq`/`assert_ne` carries no `left`/`right`: the MIR assertion aborts with "assertion failed: left != right" and… | 31c59a1eb5 |
| B-2026-10-23-4 | interp+codegen | low | Float Display breaks an exact tie between two equally short decimals away from zero, where Python's repr keeps the even last digit: -75455689800034.6… | cbcd20a557 |
| B-2026-10-23-7 | typecheck | low | A `vec![..]` of integer literals inside a tuple literal is typed `Vec[i64]` even where the tuple's expected type says `Vec[u8]`: `let b: (String, Vec… | ef1c77dd26 |
| B-2026-10-23-8 | typecheck+interp+codegen | low | `karac check` accepts `*x` on a binding from a pattern over a bare `for` element (a destructuring `for (o, n) in v` over a `Vec[(String, i64)]`, `if… | d5513133a7 |
| B-2026-10-23-10 | typecheck | low | A top-level `fn todo(..)` (likewise `unreachable`, `panic`, and `ref_eq`, which is checked against the prelude intrinsic) is accepted but never calle… | a10f2705e6 |
| B-2026-10-23-12 | typecheck | medium | `karac check` accepts a `Map` whose value type defaulted to `i64` (`let mut m = Map.new(); m.insert(5, 9)`) where `Map[i64, u64]` is wanted as a stru… | a10f2705e6 |
| B-2026-10-23-13 | codegen | low | (NOT SMALL, kata thread 2026-10-24: 3.4x on kata 951's tuple match of two Option[shared] params, 1.31 s vs 0.39 s for nested matches) A `match` on a… | a97412f163 |
| B-2026-10-23-14 | cli | medium | v2 project mode (`karac build`/`run`/`test` with no file) refuses a whole-module `import` and a renaming `import .. | 45b9315c99 |
| B-2026-10-23-15 | cli | medium | `karac test` on v2 does not run the provider fixtures (`#[with_provider]` variants) of 8 cli tests and does not report what 6 more pin (legacy test-r… | 2c0ab534bb |
| B-2026-10-23-16 | cli | low | The v2 build ignored `--monomorphization-budget`, `--enable-hot-swap` and a cross `--target`, and its `--output=json` carried no typecheck warnings;… | 392c90770a |
| B-2026-10-23-17 | mir-llvm | medium | The v2 wasm build did not lower `host fn` calls (`native report is not lowered yet`), and five wasm float tests pinned legacy rendering (`3` where th… | 808e37ce9b |
| B-2026-10-23-18 | mir | low | v2 renders runtime faults differently from the pinned output: a `main` error goes through Debug instead of its `Display` impl, a panic has no `panick… | 0bb0c20d38 |
| B-2026-10-23-19 | mir | medium | `karac run --interp` (the MIR interpreter) iterates a `Map`/`Set` in the same order on every run and ignores `KARAC_HASH_SEED`, unlike the compiled b… | e6d5b77c68 |
| B-2026-10-23-20 | mir | medium | The v2 build refuses selfhost/ (a move out of a collection element, plus legacy-dialect code), so the 8 self-host oracle tests are ignored at the M3… | ccd103ff30 |
| B-2026-10-23-21 | cli | low | `karac_jit_runner`'s `--repl-mode` and `--test-batch` paths are dead after the M3 deletion (first half, project `karac check` not refusing the archiv… | 0ad07ee467 |
| B-2026-10-24-1 | cli+interp | low | The v2 REPL's remaining legacy-only pieces: `%show` and `--auto-clone` / `%set auto-clone` refuse on v2, `:provide` does not check that the provider… | dd769270e7 |
| B-2026-10-24-3 | interp+codegen | high | `v.push(m.get(k).unwrap())` into a `Vec[i64]` stores the ADDRESS of the map's value, not the value: on mir-llvm and `karac build --mir` the element p… | bd015b5c7c |
| B-2026-10-24-4 | mir | medium | The MIR builder refuses an adaptor called on a value of generic type `I: Iterator[Item = i64]`: `it.map(\|x\| x + 1).count()` checks, and `__mir-run` a… | bc575797b1 |
| B-2026-10-24-5 | mir | high | MIR borrowck lets the borrowed place change while a ref read through a borrowed struct's `mut ref` field is still live, a use-after-free that `karac… | 930b560ed5 |
| B-2026-10-24-7 | typecheck | low | `c as i128` and `c as u128` on a `char` are refused with a bare "cannot cast 'char' to 'i128'", though `c as i64` and `c as u64` are accepted: intege… | a10f2705e6 |
| B-2026-10-24-8 | mir | low | The MIR builder refuses `.into()` called straight on a unit-variant literal: `let b: B = A.Y.into();` checks (since eb790336d3), and `__mir-run` and… | da0b9e8019 |
| B-2026-10-24-9 | mir | medium | A closure returned from a closure is tied to the callee: `let make = \|n: i64\| \|x: i64\| x + n; fs.push(make(i));` is refused by check and both MIR col… | eede6bfd40 |
| B-2026-10-24-10 | mir | low | A borrow-check refusal names a MIR temporary: `let f = make("abc".to_string());` with `fn make(p: String) -> Fn(i64) -> i64 { \|n\| p.len() + n }` repo… | 94e7ecf4ae |
| B-2026-10-24-11 | mir | low | A `match` on a tuple of handle LOCALS that outlive it (`match (top_left, top_right, bottom_left, bottom_right) { (Quad.Leaf(p), ..) => . | ea9c109f6e |
| B-2026-10-24-12 | parser | low | `karac fix` has no fix for `format!("..{}..", a, b)`: the error names the f-string to write but offers no edit, though a literal whose holes are all… | 80fb30bcf3 |
| B-2026-10-24-13 | parser | low | The turbofish fix deletes `::<T>` / `::[T]` from `let x = it.collect::[Vec[char]]();` and leaves `let x = it.collect();`, which then fails E0238 "can… | 51f56fc2ed |
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
| B-2026-10-24-28 | ownership | medium | `f(None, None)` with a `ref Option[T]` first parameter and an owned second one is refused by `karac check`, `karac build` and `karac run`: "cannot mo… | f7bccfcbbb |
| B-2026-10-24-29 | mir | medium | `Map[K, V, H]` / `Set[T, H]` lose their hasher in MIR (IntrinsicTy::Map(K, V)), so `FxBuildHasher` and a user `BuildHasher` are ignored on both v2 ba… | 4fe63393b1 |
| B-2026-10-24-30 | typecheck | low | A bare parameter inside a tuple literal handed to an owned slot (`v.push((t, value))`), or given as `Map.entry(key)`'s owned key, gets the generic `e… | 5059227eeb |
| B-2026-10-24-31 | parser | low | A reserved keyword (`and`) bound by a `for` loop, or as a match-arm pattern, gets the right "reserved keyword" error and then cascade errors: `Expect… | 3a38fff707 |
| B-2026-10-24-32 | ownership | low | E0500 FALSE POSITIVE ON A SECOND `unwrap_or` OF A COPY `Option[i64]` FIELD -- `p.a.unwrap_or(0)` twice is refused as a move when `p` is an owned loca… | ea1fd651e8 |
| B-2026-10-24-33 | typecheck | low | A string literal bound with no type (`let mut how = "";`) is a `String`, as design.md §3 says, but using the binding where a `Str` is expected (a `St… | a3d8d378ae |
| B-2026-10-24-34 | mir | medium | Auto-par statement verdicts (`KARAC_AUTO_PAR=1`) drop every use in an arm that ends in `return`, not only its drops and its write of the result, so `… | b068b8f173 |
| B-2026-10-24-35 | mir | medium | Auto-par statement verdicts (`KARAC_AUTO_PAR=1`) let a statement that may panic run beside an earlier one that may return, so `if grow(n) < 10 { retu… | b068b8f173 |
| B-2026-10-24-36 | typecheck+mir | low | A `Copy` binding destructured from a borrowed value takes `*` inconsistently: `karac check` refuses `*w` after `let (_, w) = q.front().unwrap()` or `… | 76285f0a59 |
| B-2026-10-24-37 | typecheck | low | E0517's fix-it for a write through a BARE parameter (`fn f(items: Vec[String]) { items.sort() }`, or through a local bound from it, `let mut s = item… | e99070a60e |
| B-2026-10-24-38 | cli | low | `karac fix` still answers E_INDEX_MOVE_NON_COPY on a DESTRUCTURING `let (a, b) = cases[i];` (both parts only read) with `.clone()`, a deep copy of th… | 02feb6389d |
| B-2026-10-24-39 | cli | medium | `karac check` on a project (no file) checks against the checker's built-in legacy library signatures, not the v2 library the build appends, so a pack… | 0a647b39e0 |
| B-2026-10-24-40 | typecheck | low | `karac check` says `CODES.find(s)` MOVES its String argument when the receiver is a `const` Str (`const CODES: Str = "LRI";`), so a later use of `s`… | be69aae3e9 |
| B-2026-10-24-41 | effect | medium | `karac check` accepts a public function that reads a socket under `with sends(Network) receives(Network)`: the library's socket, TLS, UDP, WebSocket… | 86104518db |
| B-2026-10-24-42 | typecheck | low | `s.as_bytes()` on a `String` is a bare E0236 `no method 'as_bytes' on type 'String'` with no note and no fix-it, though `String.bytes()` is the metho… | 4f6892cea5 |
| B-2026-10-24-43 | cli | medium | A package with more than one module cannot name a v2-only library item (`WsMessage`, `TlsFailure`): its per-module package check resolves against the… | 3c10118a4d |
| B-2026-10-25-1 | interp+mir-llvm | low | Indexing a `Map` with a missing key (`m[k]`, `m[k] -= 1`) stops with `runtime error: explicit panic` under `__mir-run` and `__mir-llvm-run`, a messag… | 216cc03ccc |
| B-2026-10-25-2 | mir-llvm | medium | A built program dies with exit 141 (SIGPIPE) when it writes to a child's stdin after the child exits; the MIR interpreter returns an error from the s… | e09926a988 |
| B-2026-10-25-3 | mir | low | v2 prints no error return trace for a `?` chain: `main` returning a propagated `Err` prints `Error: e` alone on both backends, and a panic's trace is… | f2ff086fc1 |
| B-2026-10-25-4 | cli | medium | A package with more than one module is checked without D5: its per-module passes and late phases read a bare parameter as owned, so the check in fron… | f36a5e4595 |
| B-2026-10-25-5 | cli | medium | `karac check` on a package with more than one module runs only the module-aware package check, not the v2 check of the combined source that `karac ru… | 3baf757f4d |
| B-2026-10-25-6 | typecheck | medium | An array or `Vec` literal whose elements carry unsolved inference variables is refused by `karac check`: `[Vec.new(), Vec.new()]` as `expected 'Vec[?… | fd8b172278 |
| B-2026-10-25-7 | typecheck | low | E0516 on `self.m(self.f)`, where `m` takes `mut ref self` and `f` is passed to a `ref` parameter, names the method call as the place of the shared bo… | d946efabc8 |
| B-2026-10-25-8 | parser | low | `Ok(mut b) => b.final()` in a match arm gets only prose ("rebind it where it is used: `let mut b = b;`") and no machine-applicable fix, so `karac fix… | 71842bced5 |
| B-2026-10-25-9 | typecheck | low | `expected 'Vec[u8]', found 'Slice[u8]'` (or `'ref Vec[u8]'`) on `s.bytes()` carries no fix, though `.to_vec()` (Slice's own way back, B-2026-08-14-20… | b177e376c7 |
| B-2026-10-25-10 | typecheck | low | `cannot mix integer types 'i64' and 'usize'` on `n * DAYS as usize` (n a `len()`) tells the writer to add an `as` cast, though the cast is the error:… | 1484ebcbba |
| B-2026-10-25-11 | typecheck | low | `Vec.filled(n, v)` and `Vec.with_capacity(n)` accept a `usize` length with no error, though passing the same `usize` to a user function's `i64` param… | 1484ebcbba |
| B-2026-10-25-12 | typecheck | medium | A `ref` to a `shared enum` cannot be used as a handle: returning it, wrapping it in `Some`, or passing it to a bare parameter is refused ('expected E… | 92a0f23874 |
| B-2026-10-25-13 | typecheck | medium | The closure-kind walker calls three uses that move nothing a consume, so the closure is `OnceFn` and refused for an `Fn` slot: a capture passed to a… | 2928490eee |
| B-2026-10-25-14 | ownership | low | `karac check` reports a capture of an ESCAPING closure as moved by its first use inside the body: two `size(prog)` calls (`p: ref Prog`) in one escap… | 1b9edf0601 |
| B-2026-10-25-15 | ownership | low | The E0500 fix-it on a struct whose `Vec` element type is all-`Copy` inserts `#[derive(Copy)]` on the ELEMENT type ('add #[derive(Copy)] to Op .. | fda9e3a216 |
| B-2026-10-25-16 | mir | medium | The MIR move check treats a `shared` handle captured by an ESCAPING closure as moved, so a second closure over it, or any later use, is 'use of _1, w… | f6b4ec231b |
| B-2026-10-25-17 | mir+mir-llvm | high | A closure passed to a user function's `escaping` parameter that captures a BARE (borrowed) parameter of non-Copy type, or a field of one, captures it… | f6b4ec231b |
| B-2026-10-25-18 | mir | low | v2 project build (`karac build` with no file) does not print the legacy project-build reports (discovered modules, JSON output with resolve diagnosti… | 1ed4f406c2 |
| B-2026-10-25-19 | ownership | low | `karac fix` applies both the `: Str` fix-it on a string-literal `let` (B-2026-10-24-33) and the ownership pass's `.clone()` fix-it for a 'moved' erro… | e1228ca6d3 |
| B-2026-10-25-20 | ownership | high | `karac check` accepts an ESCAPING closure that uses a bare (borrowed) non-Copy, non-handle parameter as a whole (`new_op(\|x\| x + size(p))`, `p: P` wi… | 5f1a515ae0 |
| B-2026-10-25-21 | interp | medium | On the MIR interpreter, a group's deadline never tears down the task that ran last before it fired: that task is marked cancelled but stays parked, a… | c2f859dacc |
| B-2026-10-25-22 | mir | low | `IsolatingGroup.spawn` of a named function (`g.spawn(work)`) is refused at build with `an isolating group's spawn of other than a closure written the… | 6fcffec0b3 |
| B-2026-10-25-23 | mir | low | v2 does not mark a panic raised while a `requires`/`ensures` predicate is evaluated (inline or in a callee) as `contract predicate panicked: ..`, so… | f82c28e5e9 |
| B-2026-10-25-24 | typecheck | medium | `karac check` accepts `with_provider[Clock](FakeClock {..}, ..)` and `Clock.now()` when the program redeclares `effect resource Clock;` and `FakeCloc… | 16eec2e53b |
| B-2026-10-25-25 | typecheck+ownership | medium | WIDER THAN FILED: `karac check` accepts a method taking `own self` on ANY borrowed receiver (a `ref` parameter, a bare one, a bare `for` element of a… | 5e1f7f9923 |
| B-2026-10-25-26 | cli | medium | `karac fix` on a file of a package fixes it as a single file: it cannot see the package's other modules, so a resolver 'did you mean' rewrites a corr… | 66683e8bf5 |
| B-2026-10-25-27 | parser | medium | A function type's effect clause with commas between its effects (`Fn(A) -> R with writes(Stdout), receives(Network), blocks`) ends the struct field,… | 7eba43e706 |
| B-2026-10-25-28 | typecheck | low | A spawned closure that moves one half of a destructured tuple into an array literal (`group.spawn(\|\| count([rx]))` after `let (tx, rx) = channel(1)`)… | e121bf2159 |
| B-2026-10-25-29 | mir | low | The error return trace's JSON frames carry `file`, `line` and `column` but not design.md's `expr` (the `?` expression's text) or `context` (a `.conte… | 42614fb9d9 |
| B-2026-10-25-30 | mir | low | A `?` chain that crosses a task boundary prints the frames left on the exiting thread instead of design.md's `Error return trace: unavailable (the ?… | e790115219 |
| B-2026-10-25-32 | typecheck+mir | medium | A method call on a value of an associated type of a generic bound (`let h = b.build(); h.finish()` with `B: BuildHasher`) passes `karac check` but th… | 2fb98ebc8d |
| B-2026-10-25-33 | typecheck | medium | `karac check` refuses a bare use of a program's own variant named like a `std.regex` / `std.json` item (a pattern `Match(n)` on `enum E { Match(i64)… | 66683e8bf5 |
| B-2026-10-25-34 | typecheck | high | In a package, a function a module imports from a sibling (`import ast.{take}`) is typed as the library's generic `take(mut ref T) -> T` in the import… | 01e8f145bb |
| B-2026-10-25-35 | cli | medium | A package's v2 check reports places in the combined source inside its diagnostics: a move-after-use message names the combined line (`moved at line 1… | e74039abaf |
| B-2026-10-25-36 | mir | medium | A match-arm binding moved into a `TaskGroup.spawn` closure that passes it to a function value (`Some(s) => { g.spawn(\|\| f(s)); }`, `f` an `own Fn(own… | a324abb882 |
| B-2026-10-25-37 | typecheck | medium | `tx.send(v)` accepts a plain `shared` value (a `channel(4)` of a `shared struct`), so a `shared` handle can reach another task through a channel; and… | 34730a91fa |
| B-2026-10-25-38 | cli | medium | In a package, a module's own function named like a library function (`pub fn take(b: own Box2)` in ast.kara) captures that name in every other module… | c164118d19 |
| B-2026-10-25-39 | interp | low | A panic raised in a library body inside an isolating task (`i64.parse(s).unwrap()` on `Err`, `none.unwrap()`) records a line of the appended library… | 37712078bc |
| B-2026-10-25-40 | resolver+effect | low | A channel operation's effect cannot be declared: the effect checker charges a stored callback that receives on a channel with `receives(Channel)` and… | 7ca1a3ebf5 |
| B-2026-10-25-41 | runtime+interp | medium | Server.serve/serve_tls/serve_ws/serve_ws_tls serve connections one at a time; a stalled handshake or request blocks the accept loop (fixed with keep-… | 581bf59ec2 |
| B-2026-10-25-42 | typecheck | medium | A `sync struct` field of a plain `shared` type is accepted, so a `shared` handle reaches another task inside a `sync` value | fc4f4e7adb |
| B-2026-10-25-43 | ownership | medium | A self-referential `sync enum` (`sync enum Tree { Leaf(i64), Pair(Tree, Tree) }`) is refused as an ownership cycle, where the `shared enum` twin is a… | fc4f4e7adb |
| B-2026-10-25-44 | mir | low | An error return trace frame's `context` is always null: design.md § Errors fills it from a `.context(msg)` site's message, but the v2 library has no… | 0bd9d73105 |
| B-2026-10-25-45 | mir | low | A `weak T` local (`let w: weak N = a;`) passes `karac check` but every MIR backend refuses it: "a type that failed to check is not lowered yet" at it… | e0e2a6f136 |
| B-2026-10-25-46 | typecheck | medium | In a package of several modules, the package check's per-module typecheck refuses what the v2 check of the combined source accepts, and its refusal s… | a654823884 |
| B-2026-10-25-47 | typecheck | low | A `mut ref Vec[T]` binding passed to a `Slice[T]` parameter is refused (`expected 'Slice[u8]', found 'mut ref Vec[u8]'`), though design.md's call-bou… | 2e6bbc091e |
| B-2026-10-25-48 | mir | medium | A capturing closure passed as `Server.serve`'s handler is called with its `Request` moved, not borrowed, so library code that reads the request after… | a8cc981ce6 |
| B-2026-10-25-49 | typecheck | medium | The §9.6 kind check is skipped for an associated-function call: a MutFn closure passed where `own Fn` is declared (`Runner.apply(\|k\| note(mut t, k))`… | 0b16b77585 |
| B-2026-10-25-50 | typecheck | low | The withdrawn RAII-across-yield pass is dead code: src/raii_check.rs, lib.rs's `raii_across_yield_check`, the `raii_errors` consumers in src/cli.rs /… | 1e0266d377 |
| B-2026-10-26-1 | typecheck | medium | A private field of another module's struct is readable through a call's result when the struct is not imported (`mk().secret`), while the same read t… | fed40c4735 |
| B-2026-10-26-2 | typecheck | low | `Vec.swap` (and other `Vec` method index arguments) accept a `usize` with no error, though a `usize` length of `Vec.filled` is now refused as a sign-… | 1bad46461f |
| B-2026-10-26-3 | typecheck | medium | A `ref` to a `shared` handle placed in a tuple built by a closure stays `ref S`, so `items.iter().map(\|(n, v)\| (n.to_string(), v)).collect()` into a… | 99e5f3ff2f |
| B-2026-10-26-4 | typecheck | low | `.upgrade()` on a `weak` slot has no working spelling: `a.me.upgrade()` on a `weak` field is "no method 'upgrade' on type 'Option'", though design.md… | 99e5f3ff2f |
| B-2026-10-26-5 | ownership | low | Passing a `weak` field straight to a `weak` parameter (`id_of(a.me)` with `fn id_of(p: weak N)`) is refused as "cannot move a non-`Copy` value out of… | 99e5f3ff2f |
| B-2026-10-26-6 | mir-llvm | medium | `KARAC_DEBUG_INFO=1 karac build` of any program that spawns a task fails in the LLVM verifier: the call to a coroutine's `#resume` carries no `!dbg`… | f73d059f33 |
| B-2026-10-26-7 | mir-llvm | high | Auto-par loops that call into the library run slower than seq on the pool: the compiled program records each library call's place in ONE process-wide… | da5bdc59fc |
| B-2026-10-26-8 | mir | medium | A closure returned from a closure that captures one of the outer closure's own non-`Copy` captures passes `karac check` but the MIR build refuses it:… | a5ec3ea61c |
| B-2026-10-26-10 | typecheck | low | Writing a `mut` field of a captured `shared` handle makes a closure `MutFn` (E0518 at an `Fn` slot), though the capture itself is unchanged and a bor… | 0c3f03df1f |
| B-2026-10-26-9 | ownership | medium | `karac check` treats a closure that returns a closure as an `Fn` that borrows its captures, when the returned closure escapes and moves a non-`Copy`… | 016b5608ec |
| B-2026-10-26-11 | ownership | medium | `karac check` accepts an escaping closure created inside a loop that captures an outer non-`Copy` binding, which moves it on every iteration; `karac… | 647464dbbb |
| B-2026-10-26-12 | ownership | medium | `karac check` counts a capture of an ESCAPING closure as moved when a nested closure in its body reads it (`is.iter().map(\|i\| look(p, *i)).sum()`, wi… | 6f363a1aa2 |
| B-2026-10-26-13 | typecheck+mir-llvm | high | A `shared` handle crosses a task boundary inside a function value: a closure capturing a `shared` struct, erased to `Fn` and captured by a `TaskGroup… | 6cfb8ff699 |
| B-2026-10-26-14 | mir | medium | `KARAC_AUTO_PAR=1` builds ran M4b programs under 0.9x their own seq build: at 905c25d3e2, 22 of 129 on 4 workers (worst apps/pipeline/thumb_render 0.… | 66ebdb23b7 |
| B-2026-10-26-15 | mir-llvm | high | Two `par` branches that walk one `frozen` graph of a `shared struct` race on its reference counts on the pool (KARAC_THREADS=4): ThreadSanitizer repo… | dfce639b14 |
| B-2026-10-26-16 | typecheck | low | E_CHAR_AS_NARROW_INT on an ASCII char LITERAL (`'#' as u8`) carries no edit and its help does not name the byte literal `b'#'`, so `karac fix` leaves… | a418ad6c44 |
| B-2026-10-26-18 | typecheck | low | `karac check` types a `VecDeque` with `Vec`'s whole method surface, so `d.sort()`, `d.dedup()`, `d.binary_search(x)`, `d.as_slice()` check though col… | 687ad99102 |
| B-2026-10-27-2 | mir | low | A `par` over calls through a stored callback ignores the callback type's effect clause: `struct Job { run: Fn(i64) -> i64 with reads(Clock) }` and `p… | ee104fa4a0 |
| B-2026-10-27-3 | cli+interp | medium | The v2 REPL garbles an item's non-ASCII text: `fn greet() -> Str { "héllo" }` then `println(greet());` prints `hÃ©llo`, because `strip_main` (src/rep… | 0f5289b733 |
| B-2026-10-27-5 | mir-llvm+runtime | medium | On the pool (threads on by default since 72ce9e026c), sleepers wake out of deadline order: the first park opens the reactor's epoll set under the exe… | 808e37ce9b |

</details>

<!-- BUG-LEDGER:GENERATED:END -->
