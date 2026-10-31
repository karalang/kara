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
| diagnostics | 76 |
| false-positive | 76 |
| missing-feature | 60 |
| perf | 47 |
| soundness | 31 |
| miscompile | 22 |
| run-vs-build | 16 |
| other | 15 |
| codegen-gap | 13 |
| leak | 3 |
| crash | 2 |

### By surface

| surface | total |
|---|---|
| typecheck | 131 |
| mir | 78 |
| mir-llvm | 55 |
| cli | 43 |
| ownership | 37 |
| interp | 26 |
| codegen | 22 |
| parser | 22 |
| runtime | 21 |
| effect | 10 |
| other | 4 |
| resolver | 4 |
| autopar | 4 |
| lexer | 1 |
## Current state

_Generated from `bugs-v2.jsonl` by `scripts/bug-curve.py` (2026-09-26 → 2026-10-31). Do not edit this block by hand; edit the ledger and regenerate._

### Open

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-10-28-3 | 2026-10-28 | mir-llvm | low | A compiled program's run time moves 10-20% with where its hot function lands, and the runtime archive is one of the things that moves it: kata 125 runs about 10% slower whenever karac.main sits at 16 mod 64 (the full archive's larger headers shift user code by 0x3130 bytes), kata 28 KMP 0.41 vs 0.52 s between layouts; no alignment setting tried removes the swing without slowing another program 7-20% | — |
| B-2026-10-28-34 | 2026-10-28 | mir-llvm | medium | The MIR compiled backend runs kata 2's add-two-numbers seq bench 1.15x legacy's instructions and 1.17x its cycles, so kara drops from clang -O3 parity (1.01x) to 1.23x C; CORRECTED 2026-10-31: the cost is refcounting the `shared struct ListNode` on each traversal step, not Vec access | — |
| B-2026-10-30-37 | 2026-10-30 | runtime | low | tests/mir_panic_free_floor.rs fails 3 of 4 on macOS arm64: a print/Vec, an overflow-abort and a Map/Set program's binaries still contain std's panic-hook marker `RUST_BACKTRACE`, so the floor B-2026-10-28-36 / B-2026-10-29-41 measured on Linux does not hold on Apple | — |
| B-2026-10-31-8 | 2026-10-31 | mir-llvm | low | Every `karac` process starts at 15-20 ms and 46.6 MB peak RSS before doing anything (`karac --version`), and the link step of a small kata's `karac build` is its largest phase at 72-102 ms | — |
| B-2026-10-31-16 | 2026-10-31 | mir | low | `karac build` and `karac run` still do the front end's work twice in CPU: `check_v2` and the MIR builder's `frontend()` now run on two threads at once (6777e6ea51), but each parses, resolves, typechecks and ownership-checks the program and its library | — |
| B-2026-10-31-22 | 2026-10-31 | cli | medium | `karac build` refuses the project `karac new --lib` just made ("the v2 build makes an executable from src/main.kara … v2 does not build a library package yet"), and on a binary project it prints a slice-2 manifest dump (project/edition/root/target/entry/modules) before `Built:` | — |
| B-2026-10-31-25 | 2026-10-31 | typecheck | medium | `x.hash(mut h)` with a user `Hasher` is `no method 'hash'` on a type that derives `Hash` and on the primitives (`7.hash(mut h)`), though design.md § Hashing declares `trait Hash { fn hash[H: Hasher](self, hasher: mut ref H); }` and says the derive generates ordinary `self.a.hash(mut hasher)` code | — |
| B-2026-10-31-27 | 2026-10-31 | mir | medium | A provider through a generic trait (`trait Channel[T] { fn send(ref self, v: T) -> T; }`, `impl Channel[i64] for Echo`) passes `v` by value where the impl body reads it as a reference: `__mir-run` stops with `deref of non-reference Int(41)`, and the built program SEGVs in `Echo.send` (corpus fixtures/codegen/generics/e2e_generic_provider_trait_bound) | — |
| B-2026-10-31-28 | 2026-10-31 | mir-llvm | low | A rare LeakSanitizer report on corpus programs that spawn tasks or run `par` under load (fixtures/coro_e2e/SPAWN_NONBLOCK_SRC, SPAWN_MULTI_CAPTURE_SRC, interpreter/concurrency/test_dbg_json_in_par_emits_numeric_task_id, par_codegen/test_e2e_par_join_branch_float_method) | — |

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
| B-2026-09-28-33 | typecheck+interp+codegen | medium | `partial_move_of_drop_enum` MISSES A PAYLOAD MOVED INTO A PRELUDE CONSTRUCTOR -- `match e { E.A(r) => { return Some(r); } .. | 2209d30ceb |
| B-2026-10-02-39 | runtime+codegen | low | A growth-heavy `Map[i64, i64]` workload ran 1.63x Rust's `HashMap` at equal hashing (SipHash-1-3 both), and tied a plain C linear-probing table (kata… | 3332de7763 |
| B-2026-10-03-10 | codegen+runtime | medium | (LEGACY PREMISE: on v2 the tokens were already views; the cost was two runtime calls a token, a StrSearcher build and a lossy re-borrow) `for tok in… | 83cbad997f |
| B-2026-10-04-79 | codegen+runtime | medium | AFTER B-2026-10-04-56 KATA 340'S `Map[char, i64]` BENCH STILL RUNS 1.85x RUST AT EQUAL HASHING (399 ms vs 216): each hash costs 92 instructions again… | 9abd7da4c8 |
| B-2026-10-14-29 | interp+codegen | medium | The MIR borrow checker keeps a `Vec[(ref Node, i64)]` borrowed for as long as a `ref Node` copied out of one of its elements is live: `let (a, n) = v… | 2c6b4da665 |
| B-2026-10-14-39 | codegen+runtime | low | A one-integer f-string `f"{n}"` still runs about 3x legacy on the v2 compiled backend after B-2026-10-13-20's fix: 44M `f"{i % 7}"` take 0.86 s on `k… | bc3451e2bc |
| B-2026-10-15-10 | runtime | medium | `std.iter.Extend` (design.md §9) is specified but not implemented: `import std.iter.{Extend}` says "is in the library spec (docs/library) but not imp… | a321db3d4e |
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
| B-2026-10-22-43 | typecheck | low | E0517 prints a MIR temporary instead of the source place: `let g = v[0];` over a `Vec[Fn(i64) -> i64]` reports "move of (*_7) through a shared refere… | 8163f29eae |
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
| B-2026-10-24-2 | codegen | low | A long `sort_by` of shuffled pairs on the MIR compiled backend runs 1.5x legacy and about 1.9x Rust after B-2026-10-13-18's partition: kata 252's ben… | 2a2e1f8e4b |
| B-2026-10-24-3 | interp+codegen | high | `v.push(m.get(k).unwrap())` into a `Vec[i64]` stores the ADDRESS of the map's value, not the value: on mir-llvm and `karac build --mir` the element p… | bd015b5c7c |
| B-2026-10-24-4 | mir | medium | The MIR builder refuses an adaptor called on a value of generic type `I: Iterator[Item = i64]`: `it.map(\|x\| x + 1).count()` checks, and `__mir-run` a… | bc575797b1 |
| B-2026-10-24-5 | mir | high | MIR borrowck lets the borrowed place change while a ref read through a borrowed struct's `mut ref` field is still live, a use-after-free that `karac… | 930b560ed5 |
| B-2026-10-24-6 | mir-llvm+runtime | low | After the in-line Map probe (9abd7da4c8), kata 340's sliding window still paid about 200 instructions per `remove` in karac_mir_map_remove_entry (Fen… | c175914b59 |
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
| B-2026-10-25-31 | mir+mir-llvm | low | A table's hasher is not part of its type on v2, where collections.md makes `H` the last type parameter of `Map[K, V, H]` / `Set[T, H]`, and no BuildH… | a1e167c756 |
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
| B-2026-10-26-17 | mir+mir-llvm | medium | After B-2026-10-26-14's fixes, at 9030678cd0 `KARAC_AUTO_PAR=1` builds still run under the M4b floor of 0.9x their own seq build: on one worker, the… | 45524985d1 |
| B-2026-10-26-18 | typecheck | low | `karac check` types a `VecDeque` with `Vec`'s whole method surface, so `d.sort()`, `d.dedup()`, `d.binary_search(x)`, `d.as_slice()` check though col… | 687ad99102 |
| B-2026-10-27-1 | ownership | low | `karac check` refuses returning a `Vec[(ref Node, i64)]` whose pushed tuples hold a borrow of a parameter ("returned borrowed collection holds an ele… | 098fea6a67 |
| B-2026-10-27-2 | mir | low | A `par` over calls through a stored callback ignores the callback type's effect clause: `struct Job { run: Fn(i64) -> i64 with reads(Clock) }` and `p… | ee104fa4a0 |
| B-2026-10-27-3 | cli+interp | medium | The v2 REPL garbles an item's non-ASCII text: `fn greet() -> Str { "héllo" }` then `println(greet());` prints `hÃ©llo`, because `strip_main` (src/rep… | 0f5289b733 |
| B-2026-10-27-4 | cli | low | The v2 build refuses `--enable-hot-swap` on a default-profile project and a cross `--target` / `[build].target` by name, where legacy honoured both;… | 6be9fd1111 |
| B-2026-10-27-5 | mir-llvm+runtime | medium | On the pool (threads on by default since 72ce9e026c), sleepers wake out of deadline order: the first park opens the reactor's epoll set under the exe… | 808e37ce9b |
| B-2026-10-27-7 | typecheck | low | A `let p: P = Json.deserialize(s).map_err(f)?` (or `.context(msg)?`) is refused with "cannot infer type parameter 'T' of `Result[T, DeserializeError]… | 0ab7f4850c |
| B-2026-10-27-8 | mir | low | A `par` branch that calls a captured, annotated `let` (`let f: Fn(i64) -> i64 with reads(Clock) = ..; par { f(3), f(4) }`) is still refused as `unkno… | 802932ff9d |
| B-2026-10-27-9 | typecheck | high | An impl method's signature is not checked against its trait's: `impl Mk2 for S { type A = bool; fn mk2(self) -> Option[i64] { Some(7) } }` under `tra… | 75990e74c3 |
| B-2026-10-27-10 | mir | medium | A trait impl's method named like an inherent method of the same type shares its MIR instance name (`Bag.into_iter`), so a call through the bound (`c.… | c264a31df1 |
| B-2026-10-27-11 | typecheck+mir | medium | `Iterable` (design.md § `Iterator` and `Iterable`, a lang-item trait: `type Item; fn iter(self) -> impl Iterator[Item = Self.Item]`) does not exist,… | ddfda1c71b |
| B-2026-10-27-12 | ownership+mir | low | Passing a function value held in a collection element to a non-escaping function-typed parameter (`apply(v[0], 4)` over `fn apply(f: Fn(i64) -> i64,… | a526d3fe73 |
| B-2026-10-27-13 | typecheck | medium | The library's operator traits are declared without the spec's `Rhs` parameter and `Output` type (runtime/stdlib/add.kara: `trait Add { fn add(own sel… | f91c8a7d29 |
| B-2026-10-27-14 | typecheck | medium | Arithmetic on a `T: Add` (`Sub`, `Mul`, ...) type parameter is typed `T`, not the bound's `T.Output`, so `fn g[T: Sub](a: own T, b: own T) -> T { a -… | 09ef1b3e15 |
| B-2026-10-27-15 | parser | low | A type's generic parameters take no default (`struct Map[K, V, H = SipHash13BuildHasher]`, design.md § Default generic arguments): the parser refuses… | 3bb4dac262 |
| B-2026-10-27-16 | mir-llvm | medium | On v2 a wasm build emits no v128 instruction for `Vector[T, N]` arithmetic, and ignores `KARAC_OPT_LEVEL` and `--target-features`, so CI's WASM E2E j… | 62992ffd35 |
| B-2026-10-27-17 | mir-llvm | low | tests/mir_isolating_built.rs mir_isolating_built_contains_every_kind_of_panic failed once in a full llvm gate: `drop r` and `q err` printed in a diff… | 5eaa25d31a |
| B-2026-10-27-18 | typecheck | medium | An associated-type projection on a type parameter (`T.Output`) is keyed by the parameter, not by the trait that declares it, so under `T: Add[Output… | 5fc21d28ff |
| B-2026-10-27-19 | typecheck | medium | A rigid type parameter is a wildcard in `types_compatible`, so `fn f[T](x: own T) -> T { 5 }` passes check and only the MIR validator refuses it ("in… | d7caf55aa6 |
| B-2026-10-27-20 | typecheck+mir+runtime | medium | All of `std.log` (docs/library/log.md, fcb3ddf9c) is specified but not implemented: the four level functions with open `key: value` fields, `Level`,… | 0b968aee09 |
| B-2026-10-27-21 | typecheck | low | `std.log.init` accepts a backend that cannot cross tasks: docs/library/log.md requires `B` to be `CrossTask` (core-semantics §6.3), since `emit` runs… | 5bc6433d9a |
| B-2026-10-27-22 | mir+mir-llvm | medium | Auto-par is not yet invisible to logging (docs/library/log.md, ruled 2026-10-27): the interim keeps every statement and loop that logs out of auto-pa… | 4fc726165c |
| B-2026-10-27-23 | resolver | low | Renaming a type imported from a gated library module (`import std.encoding.{DecodeError as DE}`, `import std.log.{Level as L}`) makes a second, disti… | 043ee4083f |
| B-2026-10-27-24 | parser | low | A match pattern cannot name a variant through a module binding: `import std.log;` then `match record.level { log.Level.Debug => . | ef19081ebc |
| B-2026-10-27-25 | typecheck | low | The `K: Hash + Eq` bound on a table's key is checked only at a Map / Set method call and passes a type parameter, a type-only use and a generic ADT's… | 918d380872 |
| B-2026-10-27-26 | interp+mir-llvm | medium | A table key whose PART has a hand-written PartialEq is compared structurally, so `Set[(Name, i64)]` with a case-insensitive `impl PartialEq for Name`… | 20238d4ed5 |
| B-2026-10-27-27 | typecheck | medium | A variant pattern spelled through the WRONG enum is accepted and matches the scrutinee's own variant of that name: with `enum Mode { Slow, P(i64) }`… | 768279da9a |
| B-2026-10-27-28 | typecheck+mir | low | The standard collections are not `Iterable`: `fn total[C: Iterable[Item = ref i64]](c: C)` refuses `total(vec![1, 2, 3])` with "trait bound `C: Itera… | 6319a798b2 |
| B-2026-10-27-29 | typecheck+mir | medium | A program function named like a builtin the library calls (`fn panic(s: Str) -> i64 { 7 }`) replaces it inside library bodies: `let o: Option[i64] =… | 62243405c1 |
| B-2026-10-27-30 | mir+mir-llvm | low | Auto-par and std.log, remainder of B-2026-10-27-22: a record an earlier auto-par statement would log can be missing when a later one panics first | 192217f2f9 |
| B-2026-10-27-33 | typecheck | medium | A generic function that spawns or sends a value of a type parameter need not require `T: CrossTask`, so a `shared` value enters a task through it: `f… | 48de9df718 |
| B-2026-10-27-34 | typecheck | medium | A caller's type parameter named like a library generic's own leaks into the library call: inside `fn run_all[T](x: own T)`, `g.spawn(\|\| { let y = x;… | 16a2058c4a |
| B-2026-10-27-31 | cli | high | `karac check` reports no `par` conflict and no MIR borrow error in a program that imports std.log: `import std.log.{info}; fn main() { par { println(… | 7a16756c13 |
| B-2026-10-27-32 | ownership | medium | A `mut ref S` parameter passed to a static method of a type parameter (`T.make(s)`, the serde `T.deserialize(d)`) is read as a move by `karac check`:… | 01e7e98a0c |
| B-2026-10-27-35 | typecheck+mir-llvm | medium | `None` (or `Some(x)` / `Ok(x)` / `Err(e)`) given to a table's `insert`, `contains`, `remove`, `get` or `contains_key` is typed alone as `Option[()]`,… | 472e0d7dc6 |
| B-2026-10-27-36 | typecheck+mir | medium | A program with its own `enum Level` beside `import std.log;` logs every record as DEBUG: `log.init(log.text_backend(log.Stream.Stdout), level: log.Le… | 043ee4083f |
| B-2026-10-28-1 | mir-llvm | low | After 45524985d1 a one-worker auto-par build runs the in-order build's instructions, yet 16 of 129 M4b programs still time under 0.9x of it in one 9-… | 630f13fdf6 |
| B-2026-10-28-2 | typecheck | low | A generic struct's or enum's declared parameter bounds are not checked where the type is written with arguments: `struct M[T: Hash + Eq] { m: Map[T,… | b9294ac5ff |
| B-2026-10-28-4 | mir+mir-llvm | high | A function with a `mut ref` parameter that calls an auto-par site is refused by the default build: "invalid MIR: tally: bb3[term]: copy of _2 whose t… | a9f56911ac |
| B-2026-10-28-5 | typecheck+mir+mir-llvm | high | `Vec.extend_from_slice` and the `try_` writers (`try_push`, `try_insert`, `try_reserve`, `try_resize`, `try_extend_from_slice`) borrow their receiver… | a9f56911ac |
| B-2026-10-28-6 | mir-llvm | medium | An auto-par build (the default) of apps/cli/mini_inflect (6,994 lines) takes 84 s against 9.5 s with KARAC_AUTO_PAR=0: compile_mir (MIR to LLVM IR) 6… | bdf62fdac0 |
| B-2026-10-28-7 | typecheck+ownership | high | `karac check` refuses correct programs at offset-specific places: the baked stdlib (STDLIB_PROGRAMS, e.g | 71d0bd3bdf |
| B-2026-10-28-8 | mir-llvm | medium | Auto-par default-on links the task pool into every binary with a parallel site: two_sum brute_force is 511,928 B against 412,672 B with KARAC_AUTO_PA… | 6074183596 |
| B-2026-10-28-9 | typecheck+ownership | medium | `karac check` verdict on a trait default body depends on impl order: desugar copies the body into every impl with the same spans, the typechecker kee… | 09e7ad69b5 |
| B-2026-10-28-10 | mir-llvm | low | An auto-par build of apps/cli/mini_inflect takes about 13 s against 6 s with KARAC_AUTO_PAR=0 (20 s before 630f13fdf6 and the unit schedule, KARAC_MI… | 149931f960 |
| B-2026-10-28-11 | typecheck+ownership | medium | `karac check` accepts a use-after-move in a trait default body copied into two impls whose call in it borrows in one impl and consumes in the other (… | 5f981f27a0 |
| B-2026-10-28-12 | typecheck+ownership | medium | A trait default body copied into two impls rewrites `v[0] + v[1]` by the LAST impl's operand types for every copy, and drops an element move program-… | 936eab079a |
| B-2026-10-28-13 | typecheck+ownership | low | `karac check` reports one identical diagnostic per impl for a mistake in a trait default body copied into several impls: a use after a move in `Use.r… | ac5a67f639 |
| B-2026-10-28-14 | mir-llvm | low | apps/pipeline/grain_intake's auto-par build runs under 0.9x of its in-order build at four workers (21-run medians 0.886, 0.881, 0.989, 0.88, 0.91 ove… | 01665b3473 |
| B-2026-10-28-15 | mir-llvm | low | A native program that reaches an auto-par site pays a fixed cost for going multi-threaded: about 0.2-0.3 ms per worker to start the pool on a 4-core… | 9a4a2338b8 |
| B-2026-10-28-16 | typecheck | medium | A binary operator whose left operand is a primitive and whose impl is the program's (`impl Mul[Col] for f64`, then `2.0 * col`) is refused by `karac… | b3b51adb23 |
| B-2026-10-28-17 | typecheck+mir | medium | A trait-qualified call of an operator method (`Add.add(p, q)`, `Mul.mul(2.0, col)`, `Add.add(2, 3)`) passes `karac check` but leaves its result untyp… | 5ae4458dbf |
| B-2026-10-28-18 | typecheck | low | A method call of a generic operator trait's method whose receiver type has several impls of it that differ only in the right operand (`col.mul(2.0)`… | 64981e4919 |
| B-2026-10-28-19 | interp+mir-llvm | low | An operator trait's method called by name on a primitive (`2.add(3)`) passes `karac check` but does not run: the interpreter stops with "call of unkn… | 5ae4458dbf |
| B-2026-10-28-20 | typecheck | low | A type-qualified call of a trait method (`P.add(a, b)` for `impl Add for P`) is refused by `karac check` with "expected 1 argument(s), found 2", thou… | 5ae4458dbf |
| B-2026-10-28-21 | typecheck | medium | A type with two `Index` impls that differ in the index type (`Index[String]` and `Index[i64]` for a `Frame`, so `f["a"]` and `f[0]`) only consults th… | 35bc726a82 |
| B-2026-10-28-22 | typecheck | medium | A user operator impl is not found for an operand of explicit `ref T` type: `fn add_refs(x: ref Col, y: ref Col) -> Col { x + y }` is refused with "ty… | eba10af936 |
| B-2026-10-28-23 | typecheck+mir | medium | A closure whose error type comes only from a `?` inside it passes `karac check` but does not build on MIR: `let get = \|s: Str\| { if s.len() > 0 { Ok(… | ed940b7730 |
| B-2026-10-28-24 | typecheck+mir | medium | Calling a closure bound by `let` moves its non-Copy arguments, contrary to core-semantics §5.10 (a closure's parameters are ordinary bare, borrowed,… | da37ab3afe |
| B-2026-10-28-25 | ownership+mir | medium | `let r = match rx { Some(ref r) => r, None => return . | 0d1ad90014 |
| B-2026-10-28-26 | typecheck | medium | `karac check` passes a program that builds a `Column`, `DataFrame`, `LazyExpr` or `Tensor` from the legacy library's baked surface, and `karac run` t… | 816f2e1a52 |
| B-2026-10-28-27 | cli | low | The manifest keys design.md §4 promises for import names are not implemented: a dependency's `alias = "..."`, the optional `[package].import = "..."`… | 14e6159356 |
| B-2026-10-28-28 | typecheck | medium | A closure that calls a method on its parameter and passes a captured `String` to `contains` (`\|s\| s.to_uppercase().contains(upper)`) is typed as cons… | 80e76392cf |
| B-2026-10-28-29 | mir | medium | `Ok((n, it.map(..).collect()))` and `Some((n, it.map(..).collect()))` pass `karac check` but the MIR build refuses them (an expression whose type doe… | cd5a50673e |
| B-2026-10-28-30 | mir-llvm | medium | `s = s + (n - x).to_string()` copies `s` on every add, so building a string in a loop is quadratic: the concat pass (src/mir/concat.rs) gives up when… | f59be6dac3 |
| B-2026-10-28-31 | parser+cli | low | `if c { a = x } else { b = y }` (an assignment ending each arm, no `;`) gets a correct `;` fix-it for the first arm only, and `karac fix` refuses to… | d9df626fe4 |
| B-2026-10-28-32 | typecheck | medium | `prefix ?? default` on a bare (borrowed) `Option[String]` parameter is refused as "'??' requires an `Option` or a `Result` on the left, found 'ref Op… | 03e92379b1 |
| B-2026-10-28-33 | mir-llvm | medium | The MIR compiled backend runs kata 5's expand-around-center seq bench 1.22x legacy's instructions and 1.45x its cycles, turning a kata that beat Rust… | 3344983712 |
| B-2026-10-28-35 | autopar+runtime | medium | Kata 4's auto-par binary executes a constant +207M instructions (+37%) whenever it has two or more workers, and two workers buy no speedup at twice t… | 5b6aab2a3c |
| B-2026-10-28-36 | mir-llvm+runtime | medium | Every compute-only binary is about 250 KB larger on the MIR backend: a seq kata binary is 337 KB at g04628b0eb against 85 KB from legacy, and the aut… | 1f4c30c7de |
| B-2026-10-28-37 | other | low | Compiling a 40-line kata now takes about 158 ms and 68 MB peak against 92 ms and 24 MB in July, slower and larger than rustc -O on the same program (… | 6bae6aa8d2 |
| B-2026-10-28-39 | mir-llvm | medium | The MIR backend never ported legacy's partial-unroll hint (`llvm.loop.unroll.count 4` on pure-scalar counted `while` loops, B-2026-07-08-24), so kata… | 721602d810 |
| B-2026-10-28-40 | mir-llvm | medium | MIR checks the full upper bound on indexes legacy proved in range from the loop guard, and the extra checks push kata 139's 4-trip inner loop over LL… | 594852a575 |
| B-2026-10-28-41 | mir-llvm | medium | MIR never ported legacy's binary-search midpoint assume (B-2026-06-16-1) and passes a Slice through memory, so `len` reaches IPSCCP only after inlini… | b8ad892458 |
| B-2026-10-28-42 | mir-llvm+runtime | high | MIR heap-copies every string literal a function returns and builds `a + " " + b` one realloc per operand (copying a dead left operand), where legacy… | de113d6d66 |
| B-2026-10-28-43 | mir-llvm | medium | MIR stores the panic location into `@karac.site` / `@karac.site.len` on every loop latch and before every unwrap, and since da5bdc59fc (B-2026-10-26-… | e1bd415c72 |
| B-2026-10-28-44 | mir-llvm+runtime | low | On macOS, `karac run` of a program with several pool workers can report ANOTHER task's source location for a panic: the JIT keeps `@karac.site` / `@k… | a4ea53a5ee |
| B-2026-10-29-1 | parser | medium | A function-typed parameter in Rust's spelling, `f: impl Fn(i64) -> i64` or `f: fn(i64) -> i64`, gets only a bare parse error (`Expected RightParen, f… | 38876d470b |
| B-2026-10-29-2 | typecheck | high | A closure that passes a captured `String` to `replace`, `replacen`, `split`, `find` or `strip_prefix` on its parameter (`\|s\| s.replace(old, new)`) is… | 80e76392cf |
| B-2026-10-29-3 | typecheck+cli | low | `s.bytes().collect()` is refused with a message that spells out the rewrite ("iterator adaptors/terminals require an explicit `.iter()` — write `s.by… | 803f7ede31 |
| B-2026-10-29-4 | typecheck | low | A borrowed parameter or match binding put in an array literal where an owned `Vec[String]` is wanted (`count([key].to_vec())`, `key` a bare `String`… | c3fb7ea323 |
| B-2026-10-29-5 | typecheck | medium | A `fold` whose result is the payload of an enum-variant constructor in tail position is refused: in `fn total(v: Vec[i64]) -> Val { Val.I(v.iter().fo… | 62fb839cf8 |
| B-2026-10-29-6 | typecheck | medium | An `if` or `match` whose type is known (a function's tail, or a `let` with an annotation) does not pass it into its branches, so `strs.iter().map(\|s\|… | d2677b5af5 |
| B-2026-10-29-7 | parser | low | The fix for a Rust closure return type (`\|r: i64\| -> Vec[String] { ..collect() }`) deletes `-> Vec[String]` and leaves a closure whose body cannot in… | 0a5d255d8a |
| B-2026-10-29-8 | parser | low | The turbofish fix on a `collect::[Vec[String]]()` in the middle of a chain (`"(" + it.collect::[Vec[String]]().join(", ") + ")"`) deletes the type ar… | 0a5d255d8a |
| B-2026-10-29-9 | mir | medium | `let v: Vec[Option[String]] = [None].to_vec();` passes `karac check` but does not build on MIR: `an expression whose type does not lower (Type(Unknow… | af7bbf1e39 |
| B-2026-10-29-10 | ownership+mir | medium | `for n in v { out.push(n ?? String.new()); }` over a `Vec[Option[String]]` (so `n` is `ref Option[String]`) passes `karac check` and is refused only… | 03e92379b1 |
| B-2026-10-29-11 | interp | medium | `for x in v.iter()` over a `Vec` calls the library `VecIter.next` once an element, so in the MIR interpreter it runs 8.5 times slower than `for x in… | 70e887275e |
| B-2026-10-29-12 | mir-llvm | low | Kata 1011's bench built with auto-par on (the default) runs 1.23 times the time and 16% more instructions than with `KARAC_AUTO_PAR=0`, though nothin… | 70e887275e |
| B-2026-10-29-13 | parser | low | A keyword used as a binding name (`let and = op == "&";`) gets E0003 with a fix renaming the binding to `r#and`, but each use (`if and { . | 9d11090195 |
| B-2026-10-29-14 | typecheck | low | `total([a, b].to_vec())` with `a` and `b` borrowed parameters and `total(ps: Vec[P])` is E0200 "expected 'ref Vec[P]', found 'Vec[ref P]'" with no he… | c3fb7ea323 |
| B-2026-10-29-15 | effect+mir | low | A `FileSystem` provider whose handle methods (`read_handle`, `write_handle`, `close`) use a resource overridden where the provider is bound is refuse… | 084367df50 |
| B-2026-10-29-16 | parser | low | `for x in mut v { . | c602325768 |
| B-2026-10-29-17 | parser | low | A struct item left unclosed (`struct A { x: i64,` with no `}`) before the next item reports only E0003 "'struct' is a reserved keyword and cannot be… | c24082c6ee |
| B-2026-10-29-18 | typecheck | low | Comparing `x.count_ones()` (a `u32`) with an `i64` is refused with E0208 and no machine-applicable fix, though `as i64` widens without loss and E0200… | 4ff0c29ccf |
| B-2026-10-29-19 | typecheck | low | `count_ones`, `count_zeros`, `leading_zeros` and `trailing_zeros` return `u32`, while design.md says Kāra code uses `i64` for every size, length, cou… | 4ff0c29ccf |
| B-2026-10-29-20 | effect+mir | medium | Under a `Network` override, TLS, WebSocket, `UdpSocket` (ruled cc02f4f3e) and the `Server.serve` family must be refused by `karac check` with `E_PROV… | 084367df50 |
| B-2026-10-29-21 | cli | medium | `karac test` on a package refuses a program that uses a v2-only library item (`NetError`, `NetworkProvider`) with `E0104 undefined type`, where `kara… | 927b03363c |
| B-2026-10-29-22 | mir+mir-llvm | high | The implicit numeric widening of a CALL's result is dropped when the call initializes an annotated `let` (`let x: f64 = f()` with `f() -> f32`, `let… | 46a1f3e2e3 |
| B-2026-10-29-23 | parser | low | Rust-style angle-bracket generics in a type (`-> Result<(), String>`, `let v: Vec<i64>`, `x: Option<i64>`) get a bare token error ("Expected LeftBrac… | 028b93b8ce |
| B-2026-10-29-24 | typecheck | medium | An unsuffixed integer literal inside `Vec.filled(n, 0)` does not take its element type from the expected type when the call is a METHOD argument (`d.… | 5dd03bfb7d |
| B-2026-10-29-25 | mir-llvm | high | `d.push(Vec.filled(n, None))` with `d: Vec[Vec[Option[T]]]` clones its `None` from a stack slot too small for an `Option[T]` on the compiled backends… | e5f935eb0b |
| B-2026-10-29-26 | interp+mir-llvm+runtime | medium | (v1) A `Request` a program builds with `Request.new` / `with_header` / `with_body` is never freed, on either MIR backend: each call appends to a proc… | a1b960dd5d |
| B-2026-10-29-27 | typecheck | medium | Inside a subtrait's generic default method, `let (i, u) = self.tag(u)` calling a SUPERTRAIT's generic default `tag[T](self, t: own T) -> (i64, T)` is… | 8b40f7b792 |
| B-2026-10-29-28 | typecheck+mir | medium | An unsuffixed literal on the left of `<<` / `>>` with a non-constant count is typed `i64` whatever the context: `r \| (255 << b)` with `r: u64` passes… | ef433a7c5b |
| B-2026-10-29-29 | parser+cli | low | For `!x` on an integer, `karac check` says "use `not` instead" and its JSON `replacement` is `not `, while `karac fix` correctly writes `~x`: a reade… | 08ffa923e1 |
| B-2026-10-29-30 | mir-llvm | high | `m[k]` (and `ref m[k]`) on a `Map[K, dyn T]` is wrong compiled: the index stores a POINTER to the map's value into a `ref dyn T` destination, which i… | 4d4e122e46 |
| B-2026-10-29-31 | mir+mir-llvm | medium | A call through `dyn Display` (or another library trait the MIR library does not carry: `Debug`, `Hash`, `Eq`, `Ord`) is not dispatched: `d.to_string(… | cb4fa14b71 |
| B-2026-10-29-32 | mir-llvm | medium | On the MIR compiled backend, `String.contains` with a needle of at most 16 bytes calls `memchr` and then `memcmp` at every place the needle's first b… | 28baa4f6a0 |
| B-2026-10-29-33 | runtime | low | `f"{x:b}"` (any format spec but `{v:N}` / `{v:0N}`) re-parses its spec text on every call: `karac_mir_fmt_spec` copies the spec into a String, runs `… | 6f860bee0b |
| B-2026-10-29-34 | parser | low | `ref mut m` in a pattern (Rust's order for Kāra's `mut ref m`) is reported as "'mut' is a reserved keyword and cannot be used as an identifier" plus… | c602325768 |
| B-2026-10-29-35 | typecheck | low | A comparison does not give a constructor or tuple operand the other operand's type, so a `ref` to a Copy value inside it is refused: with `n: ref i64… | 6bbbaf1fb5 |
| B-2026-10-29-36 | typecheck | medium | A call whose result type nothing fixes passes `karac check` when it is an operand of `==` and fails in the MIR builder: `s.chars().filter(..).collect… | 9d8dfb194d |
| B-2026-10-29-37 | ownership | medium | `karac check` refuses moving a binding after a non-escaping closure that only reads it ("moved inside a loop" with no loop in the program); both MIR… | 39fd8d115b |
| B-2026-10-29-38 | interp | low | The MIR interpreter's `Str.bytes()` copies the whole string into a fresh snapshot on every call, so code that calls `s.bytes()` once per step pays th… | d86cab30a8 |
| B-2026-10-29-39 | parser+cli | low | `s.parse::<i64>()` gets E0001 whose machine fix deletes `::<i64>`, so `karac fix` leaves `s.parse()`, which E0236 then refuses for want of a type (wi… | d9c29910ec |
| B-2026-10-29-40 | mir | low | `Vec[String].from_iter(it)` (an associated call on a type written with its generic arguments) passes `karac check` and fails `karac run` / `karac bui… | bd4d547c50 |
| B-2026-10-29-41 | mir-llvm+runtime | medium | A MIR binary that uses a Map or Set still links std's panic hook and backtrace symbolizer: kata 1's hash_map is 358,680 B on Linux against 71,768 B f… | b8cb6d95f0 |
| B-2026-10-29-42 | mir-llvm+runtime | low | A MIR program that reaches an auto-par site peaks about 1 MiB higher in RSS than its legacy build: kata 1's auto-par brute_force and hash_map at 2.4… | 0e42c916c4 |
| B-2026-10-29-43 | ownership | medium | `karac check` still refuses moving a binding after an iterator adapter's closure that only reads it (`h.iter().filter(\|u\| mask[*u % 2]).count(); let… | faedb308f5 |
| B-2026-10-29-44 | typecheck | medium | A closure whose annotated parameter borrows (`\|x: ref String\|`), passed to an owning slot (`OnceFn(own T) -> U`: `Option.map`, `Result.map`, `into_it… | 3bf606519c |
| B-2026-10-29-45 | typecheck | low | `karac fix` has no `own` fix for a closure's bare parameter given away by an `own self` method (it offers `#[derive(Copy)]`), by a writing method (it… | 864e30b6f9 |
| B-2026-10-29-46 | ownership | medium | `method_param_modes` is keyed by `"Type.method"`, so a type with two impls of one operator trait (`impl Add for Col` with a borrowing `rhs: Col` and… | d07ed81b82 |
| B-2026-10-29-47 | ownership | medium | A program's own `struct Mask` is read as the SIMD scalar `Mask` by D5 (`COPY_SCALARS` in src/d5_params.rs lists "Mask" by name), so a bare `m: Mask`… | d07ed81b82 |
| B-2026-10-29-48 | effect | low | With a program `impl Mul[Col] for f64`, every `f64 * f64` in the program is attributed to that impl's `f64.mul` by the effect checker: a helper doing… | 1b299d7160 |
| B-2026-10-29-49 | ownership | medium | A `let` closure whose parameter is typed `Str` still moves a `String` argument in `karac check` (`let skip = \|n: Str\| ..; skip(n); n.len()` is "moved… | fb03e6df09 |
| B-2026-10-29-50 | autopar | low | An auto-par task's loop takes its run's bounds as plain captures, so LLVM knows nothing of the counter's range: kata 4's `k % R` becomes a signed 64-… | 30e4af977d |
| B-2026-10-29-51 | effect | medium | An operator whose operands differ in type (`v * 2.0` on `impl Mul[f64] for V`, `2.0 * c` on `impl Mul[Col] for f64`) is never charged its impl's effe… | 1b299d7160 |
| B-2026-10-29-52 | mir | high | `mir::concat` moved the left side of `a + b` into the add while a closure still borrowed it: `let f = \|\| println(a); let b = a + "y"; f()` printed fr… | de113d6d66 |
| B-2026-10-30-1 | ownership+mir | medium | Check reads a bare `dyn Trait` parameter as owned (d5_params skips `dyn`) while the MIR builder borrows it, so `fn keep(v: mut ref Vec[dyn Shape], s:… | d423e29660 |
| B-2026-10-30-2 | ownership+mir | medium | A call through a `let` closure never moves an owned argument in check: `let take = \|n: own String\| n.len(); take(n); take(n)` passes check, and `__mi… | f9f5e22871 |
| B-2026-10-30-3 | mir | high | A closure that writes a provider, made into a `Fn` value (`twice(\|\| { Clk.bump(); Clk.now() })` under `with_provider`), is refused by MIR: "a closure… | d0e0acb943 |
| B-2026-10-30-4 | cli | medium | `karac test` refuses a package `karac check` and `karac build` accept: its per-module typecheck reports type errors (`map` on a `Vec` from `split`, a… | d0e0acb943 |
| B-2026-10-30-5 | mir | high | A closure that reaches a provider and is run by the runtime under `with_provider` (`tg.spawn(\|\| Clk.now())`, a `sort_by` comparison, a `retain` predi… | d0e0acb943 |
| B-2026-10-30-6 | effect | medium | `karac check` lets two tasks inside a `with_provider` write the provider at once (core semantics §14.9): `tg.spawn(\|\| { Clk.bump(); . | fcd68a7848 |
| B-2026-10-30-7 | effect | medium | `karac check` misses §14.5 when the provider is bound out of sight: a callback that reaches a resource stored through an `escaping` parameter in a fu… | e104887c31 |
| B-2026-10-30-8 | cli | low | `karac test` reports a MIR build error at a line of the combined package source (`borrow check __test__root__103_..: 292:5: ..`), not at the file and… | 71d1ac24b9 |
| B-2026-10-30-9 | parser | low | `fn make() -> dyn Shape with reads(Clock) { . | 9421ed2be2 |
| B-2026-10-30-10 | effect | low | `karac check` passes two `TaskGroup` tasks that both write an override of a program-rooted resource (`with_provider[Stdout](cap, ..)` with each task… | 8f3182f638 |
| B-2026-10-30-11 | typecheck+mir | medium | `karac check` accepts `*r = other` through a `mut ref dyn Trait`, which design.md § dyn Trait makes an error (the new value may have another type and… | 72540f41c6 |
| B-2026-10-30-12 | typecheck | low | `E_DYN_NOT_DISPATCHABLE` never judges a standard-library supertrait: `trait T: PartialEq` makes `dyn T` legal although `PartialEq.eq` names `Self` ou… | 72540f41c6 |
| B-2026-10-30-13 | typecheck+ownership | high | A plain `dyn Trait` MOVED into a `TaskGroup.spawn` closure passes `karac check` and runs, so a concrete value holding a `shared` handle (non-atomic c… | 15479807f6 |
| B-2026-10-30-14 | typecheck | medium | design.md § `as` casts promises `redundant_cast` (an identity cast) and `lossless_cast` (an `as` whose widening is implicit where it stands, its fix… | 8bd1a4d307 |
| B-2026-10-30-15 | effect | medium | A resource method with a bare `self` is charged `writes(R)`, not `reads(R)` as core-semantics §14.2 says: `karac query effects` reports `writes(UserD… | 1a5e7ccb08 |
| B-2026-10-30-16 | cli | high | `karac fmt` drops every comment: `// top`, a comment line inside a body and a trailing `// note` all vanish from its output, so formatting any real f… | 99e4590cf5 |
| B-2026-10-30-17 | cli | low | `karac query ownership app.kara` (a bare file, as the other kinds accept) fails with `error: cannot read 'app': No such file or directory`, splitting… | 76b022a676 |
| B-2026-10-30-18 | cli | low | `karac run` with no file inside a project fails with `error: missing file argument`, while `karac check`, `karac build` and `karac test` with no file… | 76b022a676 |
| B-2026-10-30-19 | cli | low | `karac run main.kara in.txt` is refused with `error: unexpected argument 'in.txt'`, without saying that program arguments go after `--` | 76b022a676 |
| B-2026-10-30-20 | cli | low | `karac new <name> --cli` scaffolds the same hello-world `src/main.kara` and placeholder test as a plain `karac new`, though `karac help` describes `-… | 76b022a676 |
| B-2026-10-30-21 | ownership | low | Moving a value while a returned reference into it is live (README's `name_of` example) says `help: drop the slice borrow before moving the source`, t… | 5a2a4a0b48 |
| B-2026-10-30-22 | cli | low | `karac lint --list`, which design.md § Built-in lints and src/lints.rs name as the way to list the lint registry, is an unknown command; design.md's… | cbbf20c5e9 |
| B-2026-10-30-23 | mir | medium | A `dyn Handler with E` stored inside `Option` (`Vec[Option[dyn Handler with E]]`) loses its declared `with E` clause: a call through `if let Some(h)… | 09b4bb3178 |
| B-2026-10-30-24 | mir | low | E_STATEFUL_DEFAULT reached through a `dyn` method or a closure names the resource as its own caller ("`Ids` calls a `mut ref self` method of `Ids`")… | 92df153c0b |
| B-2026-10-30-25 | typecheck | medium | A struct's effect variable is fixed by the FIRST value bound into it, so `router.add("/hello", Hello {}); router.add("/count", Counter { . | 09b4bb3178 |
| B-2026-10-30-26 | typecheck | medium | An element of a tuple literal did not coerce into its `dyn` slot: `routes.push((path, Hello {}))` into a `Vec[(String, dyn Handler)]`, `let t: (Strin… | 6c744eaf84 |
| B-2026-10-30-27 | cli | medium | `karac fix` applies a cascaded suggestion from a mistyped import: with `import text.stats.analyse;` (the item is `analyze`) the import's E0113 sugges… | e11ef9ce09 |
| B-2026-10-30-28 | cli | low | `karac fix` with no file inside a package fails with `error: missing file argument`, so the Mend loop's fix step runs one file at a time while `karac… | e11ef9ce09 |
| B-2026-10-30-29 | cli | high | `karac fmt` prints source that does not mean the same or does not parse: a test's name loses its quotes (`test analyze counts {`), `{{` in an f-strin… | c36dbb07d3 |
| B-2026-10-30-30 | cli | medium | `karac fmt` has no package mode and no check mode: with no file it fails (`karac fmt requires a file argument`), `--check` is read as a file name, an… | 7f4624cebb |
| B-2026-10-30-31 | cli | low | `karac build` in a package writes the binary to the package root (`./wordstat`), not to `dist/` as design.md § Build cache says, so the scaffolded `.… | 710e7be317 |
| B-2026-10-30-32 | cli | medium | `karac check --output=json` in a package names a file two ways: resolver diagnostics (E0111, E0112, E0113) carry an absolute path, typecheck ones `sr… | e11ef9ce09 |
| B-2026-10-30-33 | mir | low | A failed `assert` / `assert_eq` in `karac test` reports "assertion failed" / "assertion failed: left != right" without the assertion's source text, a… | 710e7be317 |
| B-2026-10-30-34 | typecheck | low | `prelude_shadow` warns that a user `struct Stats` shadows a prelude type, though `Stats` is the post-v1 `kara-data` type and not in the prelude (libr… | e11ef9ce09 |
| B-2026-10-30-35 | ownership | medium | `karac query ownership` reports a parameter declared `s: own String` as mode "ref": the mode it prints is inferred from the body's use of the paramet… | d21c0ecf97 |
| B-2026-10-30-36 | mir-llvm | medium | kata 74's closed binary search over a flattened matrix (`while lo <= hi`, `m[mid / cols][mid % cols]`) runs 1.32x legacy's instructions on the MIR ba… | e95c0028a0 |
| B-2026-10-31-1 | cli | low | A resolve error inside a `_test.kara` file is reported against its production sibling (`src/main.kara:42:43` for a line of `src/main_test.kara`), by… | 1bc60ed3f0 |
| B-2026-10-31-2 | mir-llvm | low | MIR still stores the call site into `karac.site` / `karac.site.len` before every library call that can abort, two stores a loop trip where legacy pas… | a4ea53a5ee |
| B-2026-10-31-3 | autopar | low | An auto-par task's counter is known to lie in the loop's constant range (30e4af977d) but LLVM 18 still divides in 64 bits: kata 4's `k % R` in the ta… | 287a18e111 |
| B-2026-10-31-4 | parser | medium | An effect clause cannot root a resource at `self`: `with sends(self)` and `with writes(self.n)` are parse errors (`Expected identifier`), though desi… | 181b03c625 |
| B-2026-10-31-5 | mir | medium | examples/protobuf_schema.kara passes `karac check` but `karac run`, `karac build` and `karac __mir-run` all stop with `error[mir]: resolve: .. | 9797536787 |
| B-2026-10-31-6 | typecheck | low | A `#[derive(Debug)]` type has no callable `fmt_debug`: `u.fmt_debug()` is `no method 'fmt_debug' on type 'User'`, though design.md § Display and Debu… | 103da49c5f |
| B-2026-10-31-7 | mir | low | `karac build` (and `karac run`) runs the whole front end twice: `check_v2`'s pipeline, then `build_source_in_order_later` parses, resolves, typecheck… | 6777e6ea51 |
| B-2026-10-31-9 | resolver | medium | An effect clause cannot root a resource at a parameter that is not a channel endpoint: `with receives(listener)` on a `listener: TcpListener` and `wi… | 181b03c625 |
| B-2026-10-31-10 | cli | low | git_fetch and registry_proxy lib tests fail on a second run that reuses a pid: their temp dirs are named by pid and a counter and never removed, so a… | e9c07ff635 |
| B-2026-10-31-11 | mir | medium | `t.to_string()` under `T: Display` at a `#[derive(Display)]` type does not build: `the method call .to_string with no resolved callee is not lowered… | 103da49c5f |
| B-2026-10-31-12 | interp+mir-llvm | low | `dbg(x)` and a failed `assert_eq` ignore a hand-written `impl Debug`: `dbg(p)` with `impl Debug for Pt { fn fmt_debug(self) -> String { f"Pt<{self.x}… | 5fd7621902 |
| B-2026-10-31-13 | ownership | medium | `karac check` refuses moving a binding after a closure literal passed as a call argument (an `own MutFn` parameter, `with_provider`) that writes it:… | 6f3c7e976e |
| B-2026-10-31-14 | typecheck | medium | A bare (borrowing) parameter of a refinement type does not take its argument the way a parameter of the base type does: an owned `NonEmpty[T]` (`type… | a4cbb4119a |
| B-2026-10-31-15 | mir | medium | A closure that consumes a captured iterator (`for line in lines` over a `split` captured from outside) and is called once passes `karac check`, but t… | 6b6b058afa |
| B-2026-10-31-17 | mir | low | A builtin method called straight on a unit-variant path does not build: `Slot.Empty.to_string()` (derived Display) and `Slot.Empty.fmt_debug()` are `… | 50b9950071 |
| B-2026-10-31-18 | typecheck | medium | `karac check` accepts a field access on an enum value (`e.message` on an `IoError`, `e.x` on a user enum), then `karac run`/`build` refuse it as "a t… | aa06e8793b |
| B-2026-10-31-19 | typecheck+cli | medium | kara-lsp checks a different language from `karac check`: `karac check` passes on the `karac new --cli` scaffold, but the editor shows 3 errors (no `c… | afe068e2fe |
| B-2026-10-31-20 | runtime | medium | Every HTTP server program prints `BOUND_PORT=8080` on its standard output (`Server.serve`/`serve_ws`, run and build alike): the test-harness line B-2… | 3d8fe2ab97 |
| B-2026-10-31-21 | parser | low | Parse errors show the Rust `Debug` form of the token: `Expected LeftBracket, found Identifier { name: "tool", raw: false }` | bdcc591b6d |
| B-2026-10-31-23 | mir | medium | DELIBERATE DIVERGENCE (struct `invariant` stays unlowered for 0.9; `karac check` now refuses it as a later track): The MIR builder refuses every prog… | 76062d0e3a |
| B-2026-10-31-24 | typecheck+mir | medium | `x.eq(y)` and `x.partial_cmp(y)` on a type that derives `PartialEq` / `PartialOrd` are `no method 'eq'` / `no method 'partial_cmp'` at check, though… | b468c6c249 |
| B-2026-10-31-26 | mir-llvm | medium | `Child.wait_with_output()` leaks the child's 32-byte handle on the compiled backend: the call takes `own self`, and nothing frees the `Kid` box `kara… | 6a105f63ba |
| B-2026-10-31-29 | mir | high | An `ensures(result)` clause that matches on a non-Copy returned value (`match result { Ok(row) => row.n > 0, Err(_) => true }`) moves the value out o… | 76062d0e3a |
| B-2026-10-31-30 | mir | low | A method whose `ensures` reads `old(self.field)` is refused by the MIR builder ("a type that failed to check is not lowered yet" at the body's tail)… | f06ae29b62 |
| B-2026-10-31-31 | other | medium | `karac fmt` drops a type's effect argument: `EventBus[with E]` and `EventBus[with writes(OrderDb)]` come back as a bare `EventBus`, so a program `kar… | aba8cf09d2 |

</details>

<!-- BUG-LEDGER:GENERATED:END -->
