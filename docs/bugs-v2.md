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
| diagnostics | 450 |
| false-positive | 246 |
| missing-feature | 188 |
| perf | 182 |
| other | 111 |
| miscompile | 81 |
| soundness | 81 |
| run-vs-build | 55 |
| crash | 47 |
| codegen-gap | 33 |
| use-after-free | 9 |
| leak | 4 |
| double-free | 2 |

### By surface

| surface | total |
|---|---|
| typecheck | 578 |
| mir-llvm | 281 |
| mir | 242 |
| cli | 190 |
| parser | 103 |
| ownership | 99 |
| interp | 87 |
| runtime | 83 |
| resolver | 40 |
| effect | 38 |
| other | 33 |
| codegen | 22 |
| autopar | 6 |
| lexer | 1 |
## Current state

_Generated from `bugs-v2.jsonl` by `scripts/bug-curve.py` (2026-09-26 → 2026-11-17). Do not edit this block by hand; edit the ledger and regenerate._

### Open

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-11-04-33 | 2026-11-04 | mir-llvm | low | Kata 1665 (a gated `sort_by` partition) still executes 1.17x legacy's instructions on the M5 at 3dc7786b7f; 314 and 318 are within the 1.15x bar, and 214, 247, 288 and 895's par lane wait on their first M5 figures since filing | — |
| B-2026-11-04-39 | 2026-11-04 | mir-llvm | low | Kata 333 executes 1.28x legacy's instructions on the M5 at 3dc7786b7f (1.19x at eb17818b45): ce0cf3209b's `dereferenceable` on its `mut ref` parameter added 6.0% (x86 too); 321, 337, 338, 339 and 343 are within | — |
| B-2026-11-04-79 | 2026-11-04 | typecheck | low | Tracker for the next minor release: `mut` on a field of a plain (non-`shared`) struct or enum becomes a compile error; in 0.8.x it is a `deprecated` warning with a `karac fix` that deletes it (ad5c10d728, design.md §5 Structs 25d27bd53a) | — |
| B-2026-11-05-33 | 2026-11-05 | mir-llvm | low | x86 host-dependent: a jump across a 32-byte line costs kata 661 1.5x on one Cascade Lake host and nothing on another; aligning branches is 0.914x geomean there and loses kata 207 2.3x here, so it is opt-in (KARAC_MIR_ALIGN_BRANCHES=1); the over-2x list was a cross-host artifact | — |
| B-2026-11-07-10 | 2026-11-07 | typecheck | low | Tracker for the next minor release: an implied derive (`Copy` without `Clone`, `Eq` or `PartialOrd` without `PartialEq`, `Ord` without `PartialOrd`/`Eq`, `Hash` relying on the filled-in `Eq`) becomes a compile error; in 0.8.x it is a `deprecated` warning with a `karac fix` that adds the derive (bb8531c2c4, design.md §9 37d71cee7f) | — |
| B-2026-11-07-32 | 2026-11-07 | parser | low | Tracker for the next minor release: an f-string inside an f-string's `{..}` becomes a compile error; in 0.8.x it is a `deprecated` warning with a `karac fix` that writes the inner f-string's text in the hole when the hole is that f-string alone (d978271697, design.md §3 Interpolated strings) | — |
| B-2026-11-08-73 | 2026-11-08 | mir-llvm | low | arm64: kata 498's zigzag loop in find_diagonal_order is if-converted into about 20 selects on its loop-carried path where Rust keeps a `tbz` branch on the direction, 1.27x checked Rust on the M5; x86 is not affected (0.72x Rust's instructions, 0.8x its time) | — |
| B-2026-11-09-21 | 2026-11-09 | typecheck | low | Tracker for the next minor release: `is_ok()`/`is_err()` on an `Option` become a compile error; in 0.8.x they are a `deprecated` warning with a `karac fix` to `is_some()`/`is_none()` (85675b25f1, core-types.md 49ba9850cd) | — |
| B-2026-11-09-22 | 2026-11-09 | mir-llvm | low | Kata 404 (sum of left leaves) runs 1.73x checked Rust's time on the M5 against 1.23x its instructions on x86: the arm64 share of the gap is unattributed | — |
| B-2026-11-10-36 | 2026-11-10 | ownership | low | Tracker for the next minor release: an unmarked argument at a method's `mut ref` parameter becomes a compile error; in 0.8.x it is a `deprecated` warning with a `karac fix` that inserts `mut ` (e705e5be7e, design.md § Call-site `mut` markers) | — |
| B-2026-11-11-81 | 2026-11-11 | typecheck | low | Tracker for the next minor release: `concat()` joining the `Display` forms of elements that are neither sequences nor text becomes a compile error; in 0.8.x it is a `deprecated` warning whose `karac fix` writes `join("")` (design.md § Vec) | — |
| B-2026-11-16-255 | 2026-11-16 | typecheck | medium | Prefix collection literals `Vec[1, 2]`, `Set[1, 2]`, `Map["a": 1]`, `Array[0; 4]` are accepted with no diagnostic, though design.md §5 says there is no `Vec[...]` or `Set[...]` prefix literal; book ch09 teaches all four | — |
| B-2026-11-16-256 | 2026-11-16 | typecheck | low | `effect group io = reads(FileSystem) + writes(FileSystem) + reads(Env);` checks with no diagnostic, though effect groups are deferred (design.md scope list, deferred.md §Effect Groups); book ch11 teaches it as current | — |
| B-2026-11-16-257 | 2026-11-16 | parser | medium | A module-level `layout swarm: Vec[Particle] { group hot { x, y } cold { name } }` is accepted but binds nothing (`swarm` in main is E0100 undefined), and inside a function it is a parse error; layout blocks are deferred and `layout` is not a keyword in v1 (design.md line 279), yet book ch15 teaches them as working | — |
| B-2026-11-16-258 | 2026-11-16 | resolver | low | A module file named `src/Db.kara` is accepted and `import Db.one;` resolves, though design.md CN-2 makes a Type-class module file name a compile error (its own example: `src/DbClient.kara`); book ch13 repeats the rule | — |
| B-2026-11-16-260 | 2026-11-16 | typecheck | low | `struct HTTPClient` checks with no diagnostic; design.md CN-4 says Type names treat acronyms as words (`HttpClient`, not `HTTPClient`) but does not say whether that is an error or a lint, and book ch02 lists it among the compile errors | — |
| B-2026-11-16-261 | 2026-11-16 | typecheck | low | `#[noblock]` and `@noblock` on an extern fn are accepted, though neither is in design.md's attribute table (an unknown bare attribute is E_UNKNOWN_ATTRIBUTE and `@` is not attribute syntax), and the attribute no longer does what appendix D says since an extern with no `with` clause has every effect | — |
| B-2026-11-16-262 | 2026-11-16 | typecheck | low | `#[derive(Display(...))]` accepts any argument silently: `Display(bogus_arg)` and appendix C's `Display(all_fields)` check and change nothing, while design.md names only the casing argument (`Display(snake_case)`, enums) | — |
| B-2026-11-16-275 | 2026-11-16 | runtime | low | The HTTP server labels every response `content-type: application/json` whatever the body: the README's own `Server.serve` example answers `/hello` as JSON, and the shortener's 302 and plain-text usage page carry it too; library/io.md does not say so | — |
| B-2026-11-16-282 | 2026-11-16 | cli | medium | `karac run --example <NAME>` compiles a project example's src/main.kara as a lone file, so 6 of the 9 project examples fail (E0207, E0200, E0516 'a type that failed to check is not lowered yet') although `karac run` inside each package works | — |
| B-2026-11-16-283 | 2026-11-16 | cli | medium | `karac run --output=json` on a program whose `main` returns `Err` prints two separate JSON objects on stdout (`{"error":"boom"}` then `{"error_return_trace":[...]}`), so the output is not one JSON document, and neither matches design.md §17's error-return-trace shape | — |
| B-2026-11-16-284 | 2026-11-16 | typecheck+cli | low | `#[test(requires: [UserDB])]` on a resource that is not program-rooted is reported as E0419 'no provider for UserDB' at `<generated>:47:30` inside the test harness, not as core-semantics §14.13's error naming R at the attribute; a `#[with_provider]` case then also warns `unused_mut` on the harness's own `let mut __karac_p0` at `<generated>:23` | — |
| B-2026-11-16-285 | 2026-11-16 | cli | medium | `karac migrate shared-to-par` writes code that does not parse: it wraps uses in `lock self.n { ... }` blocks (`lock` is not a v1 keyword, design.md line 279) and renames a free function's parameter `c` to `self` | — |
| B-2026-11-16-286 | 2026-11-16 | cli | low | `karac build --output=jsonl` on a failing build never emits `phase_skipped` for `codegen`, and a program whose effect phase reported an error gets `program_effects: []`, which is neither its effect set nor the `null` design.md §17 reserves for an unreached effect phase | — |
| B-2026-11-16-287 | 2026-11-16 | cli | low | `karac help` and flags drift from behaviour: `--sequential` is listed as 'Disable parallel execution in par blocks' but `run` refuses it and `build` calls it unknown; `install` documents `path = ...` but only `path=...` parses; `cache key --pkg x` is refused (only `--pkg=x`); `fix` exits 0 on a file that still has an error; `debug` renders crash files nothing writes | — |
| B-2026-11-16-288 | 2026-11-16 | cli | low | Malformed values of KARAC_HASH_SEED and KARAC_THREADS are ignored silently (`KARAC_HASH_SEED=abc` gives a fresh order each run; `KARAC_THREADS=0` runs one thread, `=abc` the default), unlike KARAC_OPT_LEVEL / KARAC_TARGET_CPU / KARAC_TARGET_FEATURES, which refuse; and KARA_BACKTRACE=1 (design.md § main error exit) is read nowhere | — |
| B-2026-11-16-289 | 2026-11-16 | other | low | [doc-only] design.md describes `karac explain --edition <NEXT>` and `karac fix --edition <NEXT>` in the present tense (§4 editions, §3 `r#name`), but both are 'unknown flag'; with one edition (2026) there is nothing to migrate yet, so the text should say they arrive with the first edition boundary | — |
| B-2026-11-16-290 | 2026-11-16 | resolver+cli | medium | The value-position did-you-mean offers the built-in `print` (`prnt`, `point` -> `print`) as a machine fix, and `karac fix` applies it into a fresh E0100 'print is a built-in function and cannot be used as a value'; `format` gets 'did you mean forget' | — |
| B-2026-11-16-291 | 2026-11-16 | ownership+cli | medium | E0213's machine fix rewrites `fn bad() -> ref Holder` to `-> Holder`, and the fixed file fails with E0516 x2 | — |
| B-2026-11-16-292 | 2026-11-16 | typecheck+cli | medium | E0254 (not CrossTask) fix adds `+ CrossTask` to `-> dyn Bump` on a function whose concrete type holds a non-CrossTask `S`, and the fixed file fails with E0200 'D is not CrossTask: it holds S' | — |
| B-2026-11-16-293 | 2026-11-16 | typecheck+cli | medium | E0288 (receiver stronger than the trait's) fix rewrites `fn look(mut ref self)` to `fn look(self)` while the body does `self.n += 1`, and the fixed file fails with E0517 | — |
| B-2026-11-16-294 | 2026-11-16 | typecheck+cli | low | E_INDEX_MOVE_NON_COPY fix rewrites `v[0]` to `v[0].clone()` where the element is an unbounded type parameter, and the fixed file fails with E0236 'no method clone on type parameter T' | — |
| B-2026-11-16-295 | 2026-11-16 | typecheck+cli | medium | The E0520 shared->sync migration fix rewrites the fields to `Mutex[i64]` but leaves the literal `Counter { val: 0, count: 0, tag: 0 }`, so the fixed file fails with E0200 at the two wrapped fields | — |
| B-2026-11-16-296 | 2026-11-16 | cli | medium | 275 of the 491 codes `karac explain` knows have no prose of their own: their entry prints only phase/kind and either the class's generic paragraph or 'class: OTHER - this diagnostic has no class page yet' | — |
| B-2026-11-16-297 | 2026-11-16 | cli | medium | Class-level prose mis-describes the code: deferred-feature, archived and lint codes inherit the paragraph of an unrelated class (E_TAIT_NOT_IMPLEMENTED_YET reads 'Method-call dispatch found no matching impl'; E_DEFERRED_M4A, E_ARCHIVED, E_TRY_BLOCK_NOT_IMPLEMENTED_YET read as TYPE_MISMATCH) | — |
| B-2026-11-16-298 | 2026-11-16 | typecheck+cli | medium | Deferred-feature stubs emit wire code E0200 (class TYPE_MISMATCH) and put the catalogued symbolic code only inside the message text (`try { }` -> E0200 'try block syntax is recognized...'; a trait-alias bound -> E0200 'trait alias B is recognized but not yet expanded'), so `explain <code>` from the JSON never reaches E_TRY_BLOCK_NOT_IMPLEMENTED_YET / E_TRAIT_ALIAS_NOT_IMPLEMENTED_YET | — |
| B-2026-11-16-299 | 2026-11-16 | typecheck+cli | medium | One warning code covers several unrelated lints (W0295: redundant_clone, unused_mut, irrefutable_let_pattern, unreachable_errdefer; W0291: lossless_cast and redundant_cast; W0299: f16_software_emulated, map_value_clone_reinsert), and `karac explain` accepts neither the lint name nor tells them apart | — |
| B-2026-11-16-300 | 2026-11-16 | cli | low | `karac explain --help` says parse / effect / ownership codes 'are not catalogued yet', but they are (explain E0003, E0400, E0500 work) and the E9999 message says every numbered code has an entry | — |
| B-2026-11-16-301 | 2026-11-16 | typecheck+cli | low | E0208 (comparison without the trait) offers the `#[derive]` machine fix at an `==` site but none at a `<` / `>` site | — |
| B-2026-11-16-302 | 2026-11-16 | other | low | [doc-only] design.md's streaming-phase-events example shows code E0099, which the compiler never emits (parse errors are E0001/E0002) and explain does not know | — |
| B-2026-11-16-303 | 2026-11-16 | other | low | 389 of the 491 codes `karac explain` knows are produced by no corpus program under `karac check` (102 distinct codes appear over all 21918 sources), so most catalogue entries have no pin that the code is reachable or that its text matches | — |
| B-2026-11-16-310 | 2026-11-16 | runtime+mir-llvm | high | On wasm_wasi a capacity argument is truncated to 32 bits instead of reporting CapacityOverflow: `w.try_reserve(4294967297)` returns Ok with capacity 4, `v.try_reserve(1 << 58)` returns Ok, and `String.try_with_capacity(i64.MAX)` reports OutOfMemory of 4294967295 bytes | — |
| B-2026-11-16-311 | 2026-11-16 | mir-llvm+runtime | medium | Dropping a deep two-child `shared` chain overflows the stack on wasm_wasi at about 20000 levels (RangeError: Maximum call stack size exceeded, exit 1), while native drops a 5,000,000-level chain; a one-field chain drops fine at 100000 on both | — |
| B-2026-11-16-312 | 2026-11-16 | mir-llvm | medium | A 21,824-line program (apps/cli/mini_tldextract) takes 246 s and 11.4 GB peak RSS to build for wasm_wasi at the default -O2, against 18 s and 1.5 GB for native; KARAC_MIR_OPT=0 builds it for wasm in 9 s, O1 in 197 s | — |
| B-2026-11-16-313 | 2026-11-16 | cli | low | `karac build --target=wasm32-wasip1` (or -threads) is taken as a native triple and fails at link with `wasm-ld: cannot open crt1.o`, `unable to find library -lpthread`, instead of mapping to wasm_wasi or pointing at it | — |
| B-2026-11-16-314 | 2026-11-16 | cli | low | `karac build --help` describes `--target=wasm_browser` as emitting `.wasm` + `.js` glue + `.d.ts` with no 'Not in 0.8' mark, but the build refuses it ('builds the browser wasm bindings, which is not in 0.8') | — |
| B-2026-11-16-315 | 2026-11-16 | other | low | [doc-only] design.md's resource table marks `Env` on wasm_wasi as '(limited)' without saying what is limited, and §16 names `libkarac_runtime_wasm_threads_{unicode,regex}.a` although no 0.8 build can link a threaded wasm module | — |
| B-2026-11-16-316 | 2026-11-16 | runtime+mir-llvm | high | `v.reserve(n)` with n*8 past 2^64 (e.g. `reserve(1 << 61)`, `reserve(i64.MAX)`) sets a wrapped capacity without allocating, and the next pushes write past the buffer: heap-buffer-overflow under ASan, SIGSEGV in a plain build | — |
| B-2026-11-16-317 | 2026-11-16 | runtime+mir-llvm+interp | medium | Allocation failure is not a panic: natively it prints `panic: out of memory` / `panic: capacity overflow` and aborts with SIGABRT (exit 134, no `runtime error:`, no location, no panic record, no handler); on wasm it traps with exit 1; the interpreter reports 'explicit panic' or does not fail at all | — |
| B-2026-11-16-318 | 2026-11-16 | mir-llvm | medium | The compiled lanes drop the panic's `at file:line:col` when the panic is raised inside a `#[track_caller]` function: a user `#[track_caller] fn at(v, i) { v[i] }` and `Str.split_at` inside a char print the message with no location on llvm and wasm, while the interpreter prints the caller's location | — |
| B-2026-11-16-319 | 2026-11-16 | mir-llvm+interp | medium | Stack overflow is not a specified outcome and differs per lane: the interpreter exits 134 with 'fatal runtime error: stack overflow', the -O2 native build turns the recursion into a loop and exits 0 with the answer, and wasm traps with exit 1; the corpus pins 134 | — |
| B-2026-11-16-320 | 2026-11-16 | runtime+mir | low | The string-slicing panic text carries a diagnostic-style code (`runtime error: E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY: byte range 0..2 does not fall on UTF-8 char boundaries of "héllo"`) that `karac explain` treats as a compile-time code, and quotes the whole string into the report | — |
| B-2026-11-16-321 | 2026-11-16 | other | medium | [doc-only] The library docs give the panic text for only windows, chunks, step_by and the provider-default methods; every other panic message the lanes print is undocumented, and design.md says the human report 'may change freely', which conflicts with documenting it word for word | — |
| B-2026-11-16-322 | 2026-11-16 | mir-llvm | medium | A shift whose amount is an i128 at or above 2^64 does not trap on llvm or wasm: `x << a` with `a: i128 = 1 << 64` returns x unchanged (the amount is cut to its low 64 bits, 0), while the interpreter panics 'shift amount out of range' | — |
| B-2026-11-16-323 | 2026-11-16 | typecheck+mir | medium | A constant shift whose amount is at least the width (`let y: i8 = 1 << 8;`, `let y: i32 = 1 << 32;`, `let y: u8 = 255 >> 8;`) passes `karac check` and evaluates to 0 on every lane, though the same shift through a variable traps and constant arithmetic out of range is a compile error | — |
| B-2026-11-16-324 | 2026-11-16 | mir-llvm+runtime | medium | On wasm_wasi `isize` and `usize` are 64-bit (`usize.MAX` is 18446744073709551615, `(-1i32) as usize` is 2^64-1, shifts by 32..63 are in range), while design.md §5 says their width is the target's pointer width (32 on wasm32) | — |
| B-2026-11-16-273 | 2026-11-16 | runtime+mir-llvm | medium | A `TaskGroup.new(limit: 2)` group ran up to four of its tasks at once on the ASan lane, where `spawn` blocks while `n` of its tasks are running (design.md §13 `TaskGroup`) | — |
| B-2026-11-16-335 | 2026-11-16 | typecheck | low | Tracker for the next minor release: a derived `Clone`, `Debug` or `Display` on a type with a field that lacks the trait becomes a compile error; in 0.8.x it is a `deprecated` warning naming the field and its type (design.md §9 Derive) | — |
| B-2026-11-16-340 | 2026-11-16 | typecheck | low | An unsuffixed float literal that a context types as `f32`/`f16`/`bf16` is not range-checked there: `let a: f32 = 1e39;` is accepted as an f32 infinity | — |
| B-2026-11-16-341 | 2026-11-16 | parser | low | `|>` is a token the parser accepts and the typechecker refuses late, so its stage's own type errors are reported first: `1 |> f` with a 2-parameter `f` gives `expected 2 argument(s), found 1` before `there is no pipe operator` | — |
| B-2026-11-17-1 | 2026-11-17 | typecheck+runtime | medium | An f-string width has no upper bound below 2^64: `f"{n:10000000000000000000}"` passes `karac check`; the native build then grows memory until the OOM killer ends it (or aborts 'panic: out of memory', exit 134, at width 4294967296), and the interpreter aborts with Rust's 'memory allocation of N bytes failed' and a backtrace | — |
| B-2026-11-17-2 | 2026-11-17 | parser | low | A `#` flag in a format spec (`{n:#x}`, `{n:#b}`) is refused as 'unsupported type `#` (expected one of x, X, o, b, d)', and `{n:+-5}` as 'unsupported type `-`', though neither is a type: the message should say the alternate form (`#`) is not in v1, or name the misplaced flag | — |
| B-2026-11-17-3 | 2026-11-17 | other | low | [doc-only] strings.md does not say what the empty pattern does for find, contains, starts_with, ends_with, split, splitn, split_once, rsplit_once, strip_prefix/suffix and replace, nor design.md §3 what `+` and `0` do to NaN and inf; all lanes follow Rust (`"ab".split("")` is ["", "a", "b", ""], `split_once("")` is ("", s), `replace("", x)` inserts at every char boundary, `{nan:+}` is `NaN`, `{inf:08.2}` is `00000inf`) | — |
| B-2026-11-17-4 | 2026-11-17 | mir | medium | VecDeque.with_capacity(n) and try_with_capacity(n) with n < 0 panic "explicit panic" on all three lanes; collections.md says cap < 0 counts as 0, as for Vec | — |
| B-2026-11-17-6 | 2026-11-17 | typecheck | low | rev on a chain, flat_map or open range says only "no method 'rev' on type 'IterChain'" (or IterFlatMap, RangeFrom); skip-over-range rev explains the missing bound | — |
| B-2026-11-17-7 | 2026-11-17 | typecheck | medium | impl From[VarError] for IoError disappears from a program that also uses From[NetError] for IoError: `?` on env.var inside a function that also `?`s a socket is refused, the case io.md names as the reason both impls exist | — |
| B-2026-11-17-8 | 2026-11-17 | runtime | low | wasm_wasi: an empty path "" names the preopened directory: fs.exists("") is true, read_to_string("") fails EBADF and fs.write("", ..) fails EISDIR, where native and interp say not found / false | — |
| B-2026-11-17-9 | 2026-11-17 | other | low | [doc-only] io.md is silent on wasm_wasi file behaviour: set_permissions and symlink return Unsupported, metadata.mode is 0o444 for a file the program just wrote, and Other messages carry WASI errno numbers | — |
| B-2026-11-17-11 | 2026-11-17 | interp+mir-llvm | low | stdin.lines() goes on past an invalid-UTF-8 line (yields Err, then the next line) while BufReader.lines() ends at its first error by the 11-16 ruling; io.md gives stdin.lines no rule | — |
| B-2026-11-17-13 | 2026-11-17 | effect | medium | A Drop body's effects are left out of every function effect set: a pub fn whose local's Drop prints needs no `with writes(Stdout)`, #[no_effect(writes(Stdout))] passes, and `karac query effects` shows [] (par conflict checking does see them) | — |
| B-2026-11-17-14 | 2026-11-17 | effect | medium | with_provider[R] does not take R out of the enclosing function's effects (core-semantics 14.4): a pub fn that binds its own provider must declare reads(Db)/writes(RandomSource), and two such calls in par are refused as a conflict | — |
| B-2026-11-17-15 | 2026-11-17 | cli | low | #[test(requires: [R])] does not validate R: a provider-rooted resource fails with E0419 at `<generated>:25:30` quoting the harness's `__karac_case_0()`, an undeclared name is skipped as unsatisfied_requires, and on a plain fn the attribute passes check silently | — |
| B-2026-11-17-16 | 2026-11-17 | typecheck | low | E0600 calls Env "provider-rooted" though Env is program-rooted (14.7), and `karac query effects` derivations name the internal `__builtin_index` | — |
| B-2026-11-17-17 | 2026-11-17 | typecheck | medium | Coherence: a blanket `impl[T: Display] Tr for T` and `impl Tr for i64` are both accepted, and `5.t()` runs the concrete impl, which is the specialization design.md rules out | — |
| B-2026-11-17-18 | 2026-11-17 | typecheck | medium | A `where T: Display` on an inherent method is not checked at the call: `W { v: N {} }.show()` compiles for an N without Display and prints `[N]` through a Display nobody wrote | — |
| B-2026-11-17-19 | 2026-11-17 | typecheck | medium | `fn mk() -> impl Iterator[Item = i64] { 0..2 }` is refused E_IMPL_TRAIT_ASSOC_MISMATCH "body type Range[i64] declares Item = i64": an unsuffixed integer literal's Item is not unified with the promised i64, and the fix (`0i64..2i64`) warns redundant_suffix | — |
| B-2026-11-17-20 | 2026-11-17 | typecheck | low | `impl PartialEq[i64] for M { fn eq(self, other: i64) -> bool }` is refused E0288 ("takes other by value, but PartialEq.eq takes it as ref") for a Copy Rhs; PartialEq[String] and PartialEq[N] are accepted; the interpreter, which skips check, evaluates `m == 3` as false | — |
| B-2026-11-17-21 | 2026-11-17 | mir | low | A second `Err` held in the scope a `?` leaves keeps its frames ahead of the error going out: `let r1 = work(false); step(false)?` prints r1's `step(b)?` frame first in the trace of `step(false)?` | — |
| B-2026-11-17-22 | 2026-11-17 | mir-llvm | low | A `Map`/`Set` insert or removal probes the index twice: compiled code finds the key, then `karac_mir_map_insert_hv` / `remove_entry` probe again for the cell (kata 220 still 1.51x Rust's wall time, 391 1.87x) | — |
| B-2026-11-17-23 | 2026-11-17 | ownership | high | A closure that captures a view, stored in a struct field and returned (`fn mk(v: Slice[D]) -> H { H { f: || v[0].n } }`), outlives the view's origin: check passes, interp reports use after free, ASan heap-use-after-free; the bare and tuple returns are refused E0516 | — |
| B-2026-11-17-24 | 2026-11-17 | ownership | high | Beyond B-2026-11-17-23: a closure capturing a view also loses its origin in a nested struct, enum payload, tuple-in-struct or Vec element, through an identity generic, a Self-returning method, a builder method or a mut-ref push; a Vec of such closures escapes even inside a block. Check passes, interp reports use after free, ASan heap-use-after-free | — |
| B-2026-11-17-25 | 2026-11-17 | ownership | high | Coercing a value that holds a view to `dyn Trait` erases its origin, even inside one block: `let r: dyn Get = { let x: Vec[D] = ..; Vh { v: x[0..2] } }` passes check, interp reports use after free, ASan heap-use-after-free; the same Vh without dyn is refused E0516 | — |
| B-2026-11-17-26 | 2026-11-17 | ownership | high | A borrow projected through a handle-typed `mut` field of a shared value (`ref a.child.items`, `child: C` with C shared) holds no borrow flag and no count on the child, so replacing the field through another handle (`b.child = C {..}` with a and b the same object) frees the child under the live reference: check passes, interp use after free, ASan heap-use-after-free, llvm prints stale or garbage values | — |
| B-2026-11-17-27 | 2026-11-17 | ownership | low | `freeze` refuses a value that is plainly unique with E0512 "has mutable state, so it cannot be frozen yet": `freeze mkroot()` on a call result, and `let r = leaf(1); r.kids.push(leaf(2)); let g = freeze r;` after building it in place; `freeze` is also a parse error as a block tail or `if` arm | — |
| B-2026-11-17-28 | 2026-11-17 | ownership | high | Matching a shared value's handle-typed field (`match a.s { S.One(v) => .. }`, s a `shared enum`) does not count the handle, so assigning the field through another handle in the arm (`b.s = S.Two(1)`, a and b the same object) frees the payload under `v`: check passes, interp use after free, ASan heap-use-after-free, llvm prints a stale value | — |
| B-2026-11-17-29 | 2026-11-17 | typecheck | medium | Or-pattern alternatives that bind one name at different types or modes pass check (`Ok(x) | Err(x)` on `Result[i64, String]`, `T.L(ref d) | T.R(d)`, `E2.P(a) | E2.Q(a)` with D vs i64) and then fail in MIR on every lane: 'assigning String to _4 of type i64' | — |
| B-2026-11-17-30 | 2026-11-17 | ownership | medium | Slice and array patterns do not follow design.md's binding rules: a plain element binding over a `Vec[String]` moves the vector (`match v { [s, ..] => .. }` then `v.len()` is E0500); destructuring an owned `Array[D, N]` is refused E0516 'move out of a borrowed place' in `let` and `match`; a named rest over non-Copy elements is 'not lowered yet'; `..mut ref rest` does not parse | — |
| B-2026-11-17-31 | 2026-11-17 | typecheck | low | `match x { }` on a `Never` scrutinee is refused E0205 "pattern `_` not covered", though design.md calls it the one well-formed empty match; also E0514's partial-move message still cites the removed "RC fallback" | — |
| B-2026-11-17-32 | 2026-11-17 | ownership | high | Full matrix for B-2026-11-17-26/-28: any borrow reached through a handle-typed field (direct, nested shared-in-shared at either level, or a shared enum) is freed when the field is replaced through another handle; 37 of 99 cells use after free on interp and ASan, across ref, element ref, for, iter, slice match, if let, while let, bare Vec/element arguments, closure ref capture, a receiver or dyn param inside a replaced parent, and replacement via a local alias, a method, a closure or an upgraded weak | — |
| B-2026-11-17-33 | 2026-11-17 | mir | medium | A struct pattern on a shared struct with a Drop body (`match a.child { C { items } => .. }`) passes `karac check`, then MIR typecheck refuses it on every lane ('this pattern moves `items` out of `C`, which has its own impl Drop'); a shared scrutinee's part bindings are refs, so nothing moves | — |
| B-2026-11-17-35 | 2026-11-17 | mir | high | A `Drop` body that reaches a suspension point (`sleep`, a channel `recv`, a `Mutex.lock`) passes check, then every lane fails: interp 'call of unknown function Slow.drop', build 'error[mir]: llvm: no body named `Slow.drop`'. This holds in plain `main`, in a task, in a Vec's elements, and in a cancellation teardown | — |
| B-2026-11-17-36 | 2026-11-17 | typecheck | medium | The orphan rule and the inherent-impl rule are not enforced between user packages: a library implements another package's trait for a std type (`impl lib.Tw for String`), writes a blanket impl of a foreign trait (`impl[T: Mine] Display for T`, `impl[T: Mine] lib.Tw for T`), and adds inherent methods to another package's type (`impl lib.S { .. }`, also from the application). All of these pass check, as the root package and as a dependency, and run | — |
| B-2026-11-17-37 | 2026-11-17 | mir | medium | A user type named like a prelude enum (`struct Option { v: i64 }`, or `Result`, declared locally or imported from a module) passes check with only the prelude_shadow warning, then `karac run`/`build` fail with an internal 'error[mir]: typecheck: 18:13: pattern `Some(..)` is an enum-variant pattern, but the scrutinee has type `Option`': the user's name leaks into library code lowered in the same unit | — |
| B-2026-11-17-38 | 2026-11-17 | resolver | low | A package can import a transitive dependency it never declares: app depends only on `mid`, mid depends on `lib`, and `import lib.f;` in app passes check, builds and runs | — |
| B-2026-11-17-39 | 2026-11-17 | typecheck | low | A `pub fn` bounded on a package-internal trait (`trait Hid { .. } pub fn f[T: Hid](t: T) -> i64`) passes check with no E0221, though users of the package cannot name the bound they must satisfy | — |
| B-2026-11-17-40 | 2026-11-17 | runtime | high | `Json.deserialize` into a typed `String` field refuses every surrogate-pair escape: `{"v":"\ud83d\ude00"}` gives 'v: invalid code point at line 1 column 13', so any non-BMP character written as JSON's standard escape (Python's json.dumps default, many encoders) cannot be read; `Json.parse` reads the same text correctly | — |
| B-2026-11-17-41 | 2026-11-17 | runtime | medium | `Json.deserialize` accepts text that is not JSON, which `Json.parse` refuses: a leading zero (`[01]` reads 1), a bare fraction (`.5`), a raw tab inside a string, `1e400` (read as infinity and written back `null`), `-0` into an i64 or u64 field, and map keys `"01"` and `" 1"` for an i64 key | — |
| B-2026-11-17-42 | 2026-11-17 | mir | medium | A derived `Serialize` on an enum whose struct variant has a field named `s` (`enum Un { S { s: i64 }, B }`) passes check, then fails on every lane: 'error[mir]: typecheck: 2:35: no method `serialize_variant_begin` on type `i64`'. The field binding shadows the derive's serializer parameter | — |
| B-2026-11-17-43 | 2026-11-17 | runtime | low | Small JSON divergences from json.md: typed `Json.serialize` writes `\b` and `\f` as `\u0008` and `\u000c` (stringify and serde_json write `\b`, `\f`); a BOM before `[1]` is reported at column 3, not 1; and `"New"` for a newtype variant reports 'expected value at line 1 column 6' instead of naming the variant shape | — |
| B-2026-11-17-44 | 2026-11-17 | runtime | high | The HTTP server frames a request that carries both Content-Length and Transfer-Encoding by the chunked coding and then keeps the connection, takes the first of two differing Content-Length headers, and accepts `Content-Length: +5`: each is a request-smuggling seam behind a proxy that frames the other way (RFC 9112 §6.1, §6.3) | — |
| B-2026-11-17-45 | 2026-11-17 | runtime | high | A handler's response header name or value containing CR LF is written to the wire as is, so `("X-I", "a\r\nSet-Cookie: evil=1")` sends a second header (response splitting); neither io.md nor the server refuses or strips it | — |
| B-2026-11-17-46 | 2026-11-17 | runtime | medium | The HTTP server accepts request heads that io.md's 'malformed request line or header is answered 400' should refuse: a space before the colon (`X-A : 1`), an empty header name, CR or NUL inside a value, NUL in the target, a version `HTTP/1.1x` or `HTTP/2.0`, and an HTTP/1.1 request with no Host or two Hosts all reach the handler | — |
| B-2026-11-17-47 | 2026-11-17 | runtime | medium | The HTTP server sends a body where HTTP forbids one: the answer to a HEAD request carries the handler's body, and a 204 or 304 from the handler goes out with its body and a content-length, so a kept-alive client reads those bytes as the start of the next response | — |
| B-2026-11-17-48 | 2026-11-17 | runtime | low | The HTTP server sends whatever status number and framing headers a handler returns: a status of 99, 1000 or -1 goes out as `HTTP/1.1 -1 `, a 101 goes out as an ordinary response with a body, a handler's `Transfer-Encoding: chunked` is sent next to the server's content-length over an unchunked body, and a handler's `Connection: close` is sent while the server keeps the connection open | — |
| B-2026-11-17-49 | 2026-11-17 | runtime | high | `Server.serve` (and `serve_tls`, `serve_ws`, `serve_ws_tls`) returns for good when one `accept` fails with EMFILE, so a client that opens about as many idle connections as the process's file limit stops the whole server; since the server has no read timeout, ~1000 half-sent requests (the default `ulimit -n` 1024) end a Kāra service permanently | — |
| B-2026-11-17-50 | 2026-11-17 | runtime | medium | The HTTP and WebSocket servers have no time or size limit on a connection: a half-sent request, a connection that sends nothing and an idle kept-alive connection all stay open indefinitely (checked at 70 s), and a 4 MB header or 1 MB request target is read whole and handed to the handler (no 431 or 414) | — |
| B-2026-11-17-51 | 2026-11-17 | runtime | medium | The HTTP server re-parses a request from its first byte after every 8 KiB read, so reading a request head is quadratic in its size: a 4 MB header takes 39.9 s on the interpreter and 0.45 s compiled, against 2.7 s and 0.04 s for 1 MB | — |
| B-2026-11-17-52 | 2026-11-17 | runtime | low | The HTTP server reads `Connection` as one exact word: `Connection: keep-alive, close` keeps the connection open and gets no `connection: close`, though the header is a comma-separated token list and `close` in it means close (RFC 9110 §7.6.1) | — |
| B-2026-11-17-53 | 2026-11-17 | runtime | low | The HTTP server never answers `Expect: 100-continue`: a client that waits for the interim 100 before sending its body (curl does so for bodies over 1 MiB) stalls for its own timeout, 1 s for curl, on every such upload | — |
| B-2026-11-17-54 | 2026-11-17 | runtime | high | The HTTP client lets a request's own `Content-Length` header override the body it sends: `.header("Content-Length", "1").body("hello")` sends 1 byte and drops 4 without an error, `"50"` with a 5-byte body leaves the server waiting for 45 more, and a `Host` header set by the program is sent as a second Host | — |
| B-2026-11-17-55 | 2026-11-17 | mir+mir-llvm | low | An HTTP request with an empty method, `c.request("", url).send()`, is refused by the interpreter (`bad method: invalid HTTP method`) and sent as `GET` by a compiled program | — |
| B-2026-11-17-56 | 2026-11-17 | runtime | low | The HTTP client mishandles three URL forms: an IPv6 literal `http://[::1]:18431/` fails with 'failed to lookup address information', a port of 99999 is accepted and connected to (refused) rather than refused as a bad URL, and `http://127.0.0.1:18431?x=1` (no path) is refused 'bad request: invalid format' | — |
| B-2026-11-17-57 | 2026-11-17 | runtime | low | The WebSocket server echoes close codes RFC 6455 forbids on the wire (1005, 999, 5000) instead of failing the connection with 1002, accepts a close reason that is not UTF-8, and upgrades a handshake that is a POST, has Sec-WebSocket-Version 8 or none, or a key that is not 16 bytes of base64 | — |
| B-2026-11-17-58 | 2026-11-17 | mir | low | When `Server.serve` stops because `accept` failed, the interpreter's HttpError says `accept failed: os error -1` where a compiled program says `os error 24`: the interpreter loses the errno | — |
| B-2026-11-17-59 | 2026-11-17 | interp+mir-llvm+runtime | high | `env.set` with a name containing `=`, an empty name, or a NUL in the name or value crashes in Rust's `std::env::set_var` on every lane: the interpreter aborts karac itself (SIGABRT), a compiled program prints a Rust panic and dies by SIGSEGV, and the ASan build exits 101 with a Rust panic and no Kāra panic record; the program's buffered stdout is lost on the compiled lanes | — |
| B-2026-11-17-60 | 2026-11-17 | cli | medium | `karac run prog.kara -- <arg>` aborts with SIGABRT (exit 134, a Rust `unwrap()` panic in std/src/env.rs) when an argument is not valid UTF-8, while the built binary of the same program runs and sees the argument with each invalid sequence replaced by U+FFFD | — |
| B-2026-11-17-61 | 2026-11-17 | runtime | low | `spawn` fails with kinds io.md says it never returns: an executable that is not a valid binary or script gives `Other("Exec format error (os error 8))`, a `current_dir` that is a file gives `Other("Not a directory (os error 20)")`, and a NUL in the program, an argument or an env value gives `InvalidInput`; meanwhile an env key containing `=` or an empty key is accepted and passed on corrupted | — |
| B-2026-11-17-62 | 2026-11-17 | runtime | low | A `PATH` set on the Command (`Command.new("tool").env("PATH", dir)`) is used to find the program, though io.md says a name without `/` is looked up on the parent's PATH | — |
| B-2026-11-17-63 | 2026-11-17 | runtime | low | `exit(n)` and `ExitCode.from(n)` take an i32 but the process status keeps only its low 8 bits, so `exit(256)` and `ExitCode.from(256)` end the process with 0, success; -1 gives 255 and 300 gives 44, and neither io.md nor design.md §10 says so | — |
| B-2026-11-17-64 | 2026-11-17 | runtime | medium | On wasm_wasi the default `Map`/`Set` hasher is keyed with a fixed constant: a program iterates its keys in the same order on every run, and `KARAC_HASH_SEED` changes nothing, though collections.md says the key is 'a random value chosen once per process' and that KARAC_HASH_SEED pins it | — |
| B-2026-11-17-65 | 2026-11-17 | typecheck+mir | medium | A `.hash(mut h)` call written in a program is refused for `F64`, `F32` and `()` (E0516 'hashing this type is not lowered yet') and for `i128`, `u128` and a tuple held in a local (E0236 'no method hash'), though all of them work as Map and Set keys and collections.md lists the bytes F64 feeds; so a hand-written `impl Hash` for a struct with an F64 field cannot delegate to it | — |
| B-2026-11-17-66 | 2026-11-17 | typecheck+runtime | low | `SipHash13BuildHasher` and `FxBuildHasher` have no `build()` and do not satisfy a `B: BuildHasher` bound, though collections.md calls both 'ordinary BuildHasher types', so a program cannot compute the default hasher's digest or hand either hasher to generic code | — |
| B-2026-11-17-67 | 2026-11-17 | typecheck | low | `Duration` and `Instant` have no text form: `println(f"took {t.elapsed()}")` is refused E0232 (no Display), Debug is not implemented yet, and time-random-env.md gives no format, so every timing message is written as `{d.as_ms()} ms` by hand | — |
| B-2026-11-17-68 | 2026-11-17 | runtime | medium | On wasm_wasi, once `Regex.compile` has returned its size-limit error ('Compiled regex exceeds size limit of 10485760 bytes'), the program's next print kills the host: node exits by SIGSEGV (139), though the same program runs to the end on interp, llvm and asan | — |
| B-2026-11-17-69 | 2026-11-17 | typecheck | low | A bare `ref String` arm beside a `Str` arm is refused with no fix: `match p.split_once(" ") { Some((w, _)) => w, None => p }` over `p` from `phrases.iter()` (the `.as_str()` edit covered only an arm that reads `ref x`), and `if let` / `else` had neither the fix nor the hint | — |
| B-2026-11-17-70 | 2026-11-17 | parser | low | `karac fix` opens `from_iter(` inside a parenthesized receiver: `(0..n).flat_map(..).collect::<BTreeSet<_>>().into_iter()` becomes `(SortedSet.from_iter(0..n).flat_map(..))`, refused as no `flat_map` on `SortedSet`; the `parse::<T>()` fix took the same span | — |

### Relocated

<details><summary>Relocated — real and still scheduled, but with no action item today and a concrete external trigger, so the work now lives on a canonical tracker. Not wontfix (that is 'measured to a standstill'); follow the tracker column.</summary>

| id | date | surface | sev | title | tracker |
|---|---|---|---|---|---|
| B-2026-10-27-6 | 2026-10-27 | mir-llvm | low | The v2 wasm build always emits a core module: `--bindings=component` (the wasm_wasi project default) does not embed the WIT world or componentize, `[toolchain] wasm-tools` is never consulted, and a `host fn` imports from `kara_host` instead of the canonical-ABI `kara:<pkg>/host`; 2 cli tests ignored | docs/deferred.md § Web: Bindings, component model, entry points |
| B-2026-10-31-16 | 2026-10-31 | mir | low | `karac build` and `karac run` still do the front end's work twice in CPU: `check_v2` and the MIR builder's `frontend()` now run on two threads at once (6777e6ea51), but each parses, resolves, typechecks and ownership-checks the program and its library | docs/deferred.md § Tooling: Shared Front End for `check` and `build` |
| B-2026-11-01-40 | 2026-11-01 | effect | low | CORRECTED 11-02 (kata thread): no index-disjointness analysis exists on v2 for `par {}` OR `par for` (`par for i in 0..2 { v[i] = i + 1 }` is refused too, and so is an Array); the analysis core-semantics 11.2 calls "existing" is deferred.md "Auto-Parallel Loops over Provably-Disjoint Indexed Writes". Was: Two `par {}` branches that write CONSTANT, different indices of one `Vec` (`{ v[0] = 1; 0 }` and `{ v[1] = 2; 0 }`) are refused as conflicting mutable borrows of `v`, where core-semantics 11.2 says disjoint index ranges proven by the disjointness analysis do not conflict | docs/deferred.md § Auto-Parallel Loops over Provably-Disjoint Indexed Writes |
| B-2026-11-02-5 | 2026-11-02 | mir-llvm | low | Kata 246's strobogrammatic check runs 1.31x legacy's instructions on the MIR backend (1.24x its cycles), on top of the wontfix unrolling gap B-2026-08-15-31: the seq lane went from 1.57x to 1.87x rustc -O | docs/deferred.md § Interprocedural Range Facts |
| B-2026-11-03-2 | 2026-11-03 | mir-llvm | low | Kata 302's default build no longer auto-parallelizes on the MIR backend: the item 13 cost gate declines min_area's group as reached from a loop, a 1.9x loss on x86 at 4 workers that no static rule can take without flipping kata 165's 318x pin; relocated to a runtime work estimate | docs/deferred.md § Runtime Work Estimate for Auto-Par Groups in Hot Bodies |
| B-2026-11-06-8 | 2026-11-06 | mir-llvm | low | Kata 354 on arm64 ran 1.36x rustc -O's cycles after d70e6d474a and 1.237x after 5e869cb573 (LIS phase 1.088x); what remains is the sort phase (1.316x), relocated to B-2026-11-06-34 | B-2026-11-06-34 |
| B-2026-11-07-24 | 2026-11-07 | resolver+typecheck | low | std.secret exports only Secret: Zeroize and ConstantTimeEq cannot be imported or named (so the documented user `impl Zeroize` cannot be written), and `impl Debug for Secret[i64]` is accepted though secret.md makes it a compile error | docs/implementation_checklist/phase-11-stdlib-longtail.md |
| B-2026-11-09-12 | 2026-11-09 | mir-llvm | low | `PriorityQueue`'s sifts swap elements a level at a time where Rust's `BinaryHeap` moves a hole, three element copies a level against one: kata 786 runs 1.34x and kata 778 1.22x checked Rust on x86 | B-2026-11-08-87 |
| B-2026-11-09-28 | 2026-11-09 | typecheck | low | Every `karac check` (and every kara-lsp edit) re-parses, re-registers and re-checks the whole standard library: 305M instructions and ~66 ms for a 3-line program; cache the checked library | docs/deferred.md § Pre-checked Standard Library and Package Interfaces |
| B-2026-11-11-82 | 2026-11-11 | cli | low | An allocator swap (mimalloc as karac's #[global_allocator]) speeds `karac check` by about a quarter at 20-70% more peak RSS; not landed, re-measure once the library cache changes the CLI's allocation profile | docs/deferred.md § Pre-checked Standard Library and Package Interfaces |

</details>

### Wontfix

<details><summary>Wontfix — real and reproduced, measured to a standstill, no action left. Titles are kept in full: they carry the measurements that closed the question, so read one before reopening its subject.</summary>

| id | date | surface | sev | title |
|---|---|---|---|---|
| B-2026-10-28-3 | 2026-10-28 | mir-llvm | low | A compiled program's run time moves 10-20% with where its hot function lands, and the runtime archive is one of the things that moves it: kata 125 runs about 10% slower whenever karac.main sits at 16 mod 64 (the full archive's larger headers shift user code by 0x3130 bytes), kata 28 KMP 0.41 vs 0.52 s between layouts; no alignment setting tried removes the swing without slowing another program 7-20% |
| B-2026-10-31-8 | 2026-10-31 | mir-llvm | low | Every `karac` process starts at 10-12 ms (shipped static PIE) and the link step of a small kata's `karac build` is its largest phase at 72-102 ms: PIE kept by decision (non-PIE would save about 3 ms but drop ASLR), bfd kept (lld and gold rejected); the allocator experiment split to B-2026-11-11-82 |
| B-2026-10-31-41 | 2026-10-31 | mir | medium | karac compiles a kata 1.54x slower than rustc -O at twice its peak memory on the M5 (median over 24 katas: 165 ms vs 103 ms, 59.8 MiB vs 28.3 MiB): a deliberate divergence, within ADR-2 (0.73x legacy build time over the frozen set at f1416771a4); rustc parity is not a v1 bar |
| B-2026-11-01-17 | 2026-11-01 | runtime | medium | Kata 71's auto-par binary at exactly two workers runs 1.22x slower than at one on the M5 (143.1 ms against 117.6 ms at eb17818b45, from 1.6-2.6x), ATTRIBUTED to macOS's xzone allocator: free() reads mach_absolute_time on each of simplify()'s per-iteration buffer frees, not the pool; Linux scales cleanly |
| B-2026-11-01-22 | 2026-11-01 | mir-llvm | low | Kata 116's populate-next-right-pointers loop runs legacy's instruction count on the MIR backend but 1.23x its cycles (M5), 1.08x wall on x86: WONTFIX, DELIBERATE: core-semantics §7 drops a struct's fields in reverse declaration order (next, right, left), legacy dropped them in declaration order, and the resulting free order is the whole gap |
| B-2026-11-01-47 | 2026-11-01 | mir-llvm | medium | Kata 216's combination-sum-III recursion runs 1.28x legacy's instructions on the MIR backend (x86 callgrind; was 1.41x on M5): a deliberate trade, since legacy's IPSCCP fold needs a monotone fact MIR's step shape loses and the i1-branch form that recovers 1.23x costs kata 139 10% |
| B-2026-11-07-33 | 2026-11-07 | mir-llvm | low | Kata 404's recursive `visit(nodes: Vec[Node], ...)` reloads its Vec header after the first recursive call: 1.23x checked Rust's instructions on x86, where every remedy tried (readonly, header by value) is slower; 473 and 498 were fixed by `ref` noalias (d308599fbb) |
| B-2026-11-09-51 | 2026-11-09 | mir-llvm | low | Kata 1087's bench (20 million short words cloned into vectors and freed) runs 26% behind `rustc -O -C overflow-checks=on -C target-cpu=x86-64-v3` with the same allocations: 13.65G instructions against 10.45G, the extra in glibc's malloc slow path (14.8M `_int_malloc` calls against 2.7M) and in `malloc_consolidate` (0.72G, none in Rust) |
| B-2026-11-11-4 | 2026-11-11 | interp | low | The MIR interpreter keeps a `SortedMap` / `SortedSet` as one sorted vector of values, so 100000 inserts of falling keys take 12.7 s under `__mir-run` against 0.32 s rising; the compiled backend moved to a B+-tree at c5303d91b3 |
| B-2026-11-14-62 | 2026-11-14 | interp | low | `karac __mir-run` runs an iterator chain several times slower than the loop it stands for: a `flat_map` over an inner `map` costs about 2.6 us an item, 7 to 10 times the nested loop, and a `bytes().iter().map(..).collect()` about 1.2 us a byte over an indexed loop; kata 2151's iterator arm takes 94 s against the corpus's 60 s cap |
| B-2026-11-17-12 | 2026-11-17 | mir-llvm | low | Buffered stdout is lost on a native stack overflow (deliberate: stack exhaustion aborts without flushing, core-semantics §10.1) |

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
| B-2026-10-28-34 | mir-llvm | medium | The MIR compiled backend runs kata 2's add-two-numbers seq bench 1.15x legacy's instructions and 1.17x its cycles, so kara drops from clang -O3 parit… | 5c91ea5617 |
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
| B-2026-10-30-37 | runtime | low | tests/mir_panic_free_floor.rs fails 3 of 4 on macOS arm64: a print/Vec, an overflow-abort and a Map/Set program's binaries still contain std's panic-… | 333f5427d4 |
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
| B-2026-10-31-22 | cli | medium | `karac build` refuses the project `karac new --lib` just made ("the v2 build makes an executable from src/main.kara … v2 does not build a library pac… | 7da6557e44 |
| B-2026-10-31-23 | mir | medium | DELIBERATE DIVERGENCE (struct `invariant` stays unlowered for 0.9; `karac check` now refuses it as a later track): The MIR builder refuses every prog… | 76062d0e3a |
| B-2026-10-31-24 | typecheck+mir | medium | `x.eq(y)` and `x.partial_cmp(y)` on a type that derives `PartialEq` / `PartialOrd` are `no method 'eq'` / `no method 'partial_cmp'` at check, though… | b468c6c249 |
| B-2026-10-31-25 | typecheck | medium | `x.hash(mut h)` with a user `Hasher` is `no method 'hash'` on a type that derives `Hash` and on the primitives (`7.hash(mut h)`), though design.md §… | 56f675ddce |
| B-2026-10-31-26 | mir-llvm | medium | `Child.wait_with_output()` leaks the child's 32-byte handle on the compiled backend: the call takes `own self`, and nothing frees the `Kid` box `kara… | 6a105f63ba |
| B-2026-10-31-27 | mir | medium | A provider through a generic trait (`trait Channel[T] { fn send(ref self, v: T) -> T; }`, `impl Channel[i64] for Echo`) passes `v` by value where the… | 02e3dbefde |
| B-2026-10-31-29 | mir | high | An `ensures(result)` clause that matches on a non-Copy returned value (`match result { Ok(row) => row.n > 0, Err(_) => true }`) moves the value out o… | 76062d0e3a |
| B-2026-10-31-30 | mir | low | A method whose `ensures` reads `old(self.field)` is refused by the MIR builder ("a type that failed to check is not lowered yet" at the body's tail)… | f06ae29b62 |
| B-2026-10-31-31 | other | medium | `karac fmt` drops a type's effect argument: `EventBus[with E]` and `EventBus[with writes(OrderDb)]` come back as a bare `EventBus`, so a program `kar… | aba8cf09d2 |
| B-2026-10-31-32 | mir | medium | An `unsafe extern "C"` import with an `f64` (or any float) parameter or return passes `karac check` but `karac run`, `karac build` and `__mir-run` re… | 189722227c |
| B-2026-10-31-33 | typecheck+cli | medium | User-facing diagnostics, CLI errors and help text name internal plan jargon ("goes to the archive with the legacy backend at M3 and returns on v2 on… | 4da5433a9d |
| B-2026-10-31-34 | mir-llvm | high | Under the multi-worker pool, `send` on a full bounded `channel(n)` from several producer tasks returns `Err` instead of waiting: compiled programs lo… | 59a342fad1 |
| B-2026-10-31-35 | typecheck+cli | medium | User-facing diagnostics and help say "in v1", "v1.5", "post-v1", "v1.x" for what 0.9 does not support ("identifiers must be ASCII in v1", "batched ma… | 4f8cb2f424 |
| B-2026-10-31-36 | cli | high | `karac build --target=wasm_wasi` on a package, in a binary without the llvm feature, checks the package for the HOST platform: `import host` with onl… | b478379db3 |
| B-2026-10-31-37 | mir-llvm | medium | kata 38 (count_and_say) runs legacy's instruction count but 1.22-1.28x its cycles on the MIR backend, 1.44x rustc -O on wall time where legacy was 1.… | 3d8ac8a2c5 |
| B-2026-10-31-38 | mir-llvm | high | MIR reloads a value the previous loop iteration just stored (kata 64's `dp[c] = cost + min(dp[c], dp[c - 1])` loads dp[c-1] back through an `ldp` tha… | 4eb76cba00 |
| B-2026-10-31-39 | mir-llvm | medium | Kata 9's auto-par binary executes a constant +1.85G instructions (+75%) whenever it has two or more workers, flat from 2 to 18: the par lane runs 2.1… | f117a8f69f9eb1d23ba79ec433aaeded4593c983 |
| B-2026-10-31-40 | mir-llvm | medium | Kata 10's regex DP runs 1.37x legacy's instructions and 1.50x its cycles on the MIR backend: the seq lane went from 1.11x to 1.60x rustc -O | 4e55fcd778 |
| B-2026-10-31-42 | mir-llvm | medium | Kata 20's valid-parentheses stack scan runs 1.41x legacy's instructions on the MIR backend: the seq lane went from 0.74x to 0.90x rustc -O and the pa… | 11d7eb7f01 |
| B-2026-10-31-43 | mir | medium | Implicit auto-par no longer fires on kata 23 (merge k sorted lists), which the legacy backend parallelized: its par lane now runs the sequential prog… | 11d7eb7f01 |
| B-2026-10-31-44 | mir-llvm | high | MIR never full-unrolls a constant-trip loop that calls or exits early, where the June legacy fix (B-2026-06-17-7) did: kata 36 went from 0.55x to 1.6… | c26d97bd87 |
| B-2026-10-31-45 | mir-llvm | medium | `unsafe { xs.get_unchecked(i) }` is bounds-checked on the MIR backend: the escape hatch design.md documents does nothing, and kata 28's unchecked KMP… | 62aae3a95a |
| B-2026-11-01-1 | mir | medium | `d ?? Node.Leaf(0)` on a bare `d: Option[Node]` parameter, `Node` a `shared enum`, passes `karac check` and is refused by the MIR build as a move out… | f667cd24ea |
| B-2026-11-01-2 | cli+mir | medium | `karac check` prints "All checks passed" for a program whose MIR lowering fails, and then reports none of the program's MIR borrow errors anywhere: `… | 3e19124daa |
| B-2026-11-01-3 | mir | medium | `match n { Some(k) => kind_name(k), None => "NoneType" }` with `n: ref Option[Node]`, `Node` a `shared enum` and `kind_name(n: ref Node) -> Str`, is… | cc078c37f4 |
| B-2026-11-01-4 | typecheck | low | `s.substring(0, len)` with `len` a `ref i64` from `for len in lengths.iter()` is refused (E0200 "'substring' expects integer byte indices, found 'ref… | b740a6831e |
| B-2026-11-01-5 | parser | low | A Rust-spelled mutable receiver or parameter type (`fn find(mut self, ..)`, `&mut self`, `ref mut self`, `self: mut ref Self`, `v: &mut Vec[i64]`) ge… | 59f20988a7 |
| B-2026-11-01-6 | typecheck+mir | medium | A struct variant named like a library type (`Stmt.Match { cases: Vec[i64] }`, also `Response`, `Client`, `Regex`, `Stats`) passes `karac check` but `… | d07a6cee24 |
| B-2026-11-01-7 | mir | high | A call, operator or array literal reads a place operand only after the later operands have run, against core-semantics 8.1 (left to right): `pair(c.n… | d8e47c345a |
| B-2026-11-01-8 | parser | medium | `karac fix` on `println!("{}", n)` (and `print!` / `eprintln!` with format arguments) only deletes the `!`, leaving `println("{}", n)`, which fails E… | d9ce354001 |
| B-2026-11-01-9 | typecheck | low | Rust string-method carry-overs `trim_start_matches`, `trim_end_matches`, `trim_matches` and `to_owned` get a bare E0236 "no method 'X' on type 'Strin… | da4e21d14c |
| B-2026-11-01-10 | typecheck | low | Comparing a library lookup with an owned Option (`m.get(k) == Some(5)`, `m.get(k) == o`, `v.get(i) == o.map(..)`) fails E0208 "cannot compare 'Option… | f5a8142cd5 |
| B-2026-11-01-11 | ownership+cli | medium | `(x, y) = (y, x)` on two `String` bindings gets the right E0287 fix (drop the parentheses) plus four cascade E0500 move errors with `.clone()` fixes,… | b5520ddad6 |
| B-2026-11-01-12 | mir | high | A `mut` field of handle-aggregate type passed to a borrowed parameter (`g(h.left, h)` with `a: Option[N]`) is lent in place rather than counted (core… | bf1ae02cf1 |
| B-2026-11-01-13 | parser | low | `karac fix` gives `println!("{} {}", c.n, s.replace("a", "b"))` no edit, because format_macro_rewrite refuses any argument holding a `"`, though an f… | 51cbfabbb4 |
| B-2026-11-01-14 | cli | low | `karac run` with no file inside a `karac new --lib` project says "missing file argument" and suggests "`karac run` inside a project to run its src/ma… | 51cbfabbb4 |
| B-2026-11-01-15 | typecheck | medium | `h.d ?? x` with `h` a borrowed struct whose field `d: Option[Node]` holds a `shared` handle is refused by check as a move out of a borrow; core-seman… | 8df360f9f9 |
| B-2026-11-01-16 | mir | medium | An assignment whose place is a method-call chain evaluates the value before the place, against core-semantics 8.2 (place operands first): `m.entry(1)… | edc88dd260 |
| B-2026-11-01-18 | mir-llvm | medium | Kata 87's scramble-string DP runs 1.67x legacy's instructions on the M5 (arm64) MIR backend (1.13x its cycles): the seq lane went from 0.95x to 1.09x… | cd7fd85dec |
| B-2026-11-01-19 | mir-llvm | low | Kata 88's merge-sorted-array two-pointer loop runs 1.38x legacy's instructions and 1.23x its cycles on the MIR backend: 0.81x -> 1.00x rustc -O | 80a4fd013b |
| B-2026-11-01-21 | mir-llvm | medium | Kata 118's Pascal's-triangle build loop runs 1.72x legacy's instructions on the MIR backend (1.30x its cycles): the seq lane went from 0.56x to 0.76x… | 11d7eb7f01 |
| B-2026-11-01-23 | mir-llvm | medium | Kata 120's default build auto-parallelizes its one-off 200-row triangle build and then runs +2.0G instructions (+29%) whenever it has two or more wor… | 3a1355614d98e3157c3d3187a375b4f0c4ccef97 |
| B-2026-11-01-24 | mir-llvm | medium | MIR `shared` boxes carry a 16-byte strong+weak count header where legacy carried 8, so a 24-byte node body (a tree's val + left + right) takes a 48-b… | 498a852019e616d880d51340d941042bb46ad0f2 |
| B-2026-11-01-25 | mir-llvm | high | Under the task pool every borrow-flag operation goes through ONE process-wide locked table, so parallel tasks that write `mut` fields of their own `s… | 57efe5bc2fbb907120a3e7aca5bcd7dbb052c710 |
| B-2026-11-01-26 | typecheck | high | A `shared` handle crosses a task boundary inside a function value whose type is erased by a SIGNATURE: `fn mkf(s: own S) -> Fn() -> i64 { \|\| { s.n =… | 3139c3c5bf |
| B-2026-11-01-27 | typecheck | high | A task spawned into a `TaskGroup` made OUTSIDE a `with_provider` call captures the provider and outlives it: `let tg = TaskGroup.new(); let h = with_… | 7bb878c704 |
| B-2026-11-01-28 | mir | medium | A `Map` index place reads its key operand after the assigned value has run, against core-semantics 8.2 (place operands first): `let mut k = 1; m[k] =… | 84c96d6599 |
| B-2026-11-01-29 | mir | medium | B-2026-11-01-16 is wider than method chains: an assignment whose place is rooted in ANY call evaluates the value first, against core-semantics 8.2: `… | edc88dd260 |
| B-2026-11-01-30 | mir | medium | A temporary created in a `match` guard drops at the end of the enclosing statement instead of the end of the guard, against core-semantics 7.4: `matc… | 420746ccf0f869fc37acce8d415e5ddd30ef1082 |
| B-2026-11-01-31 | mir | low | `let x: i64 = None?;` passes `karac check` and stops `karac run` / `karac build` with `invalid MIR: f: bb5[0]: assigning () to _1 of type i64`; `let… | 5ae1af9472 |
| B-2026-11-01-32 | typecheck | medium | A `TaskGroup` task that READS a `Copy` local does not borrow it: `let mut x = 1; let tg = TaskGroup.new(); let h = tg.spawn(\|\| x + 1); x = 5;` passes… | 1d70880a1f |
| B-2026-11-01-33 | typecheck | low | `with_provider[R](p, ..)` moves `p` (core-semantics 14.4), but `karac check` accepts a later use of `p`, which `karac run` and `karac build` then ref… | 2ebd405d13 |
| B-2026-11-01-34 | mir | low | A closure that writes one field through a `mut ref` parameter (`let mut f = \|\| { p.a += 1; };` with `p: mut ref P2`) holds the whole `*p`, so writing… | 2bc12f367e |
| B-2026-11-01-35 | mir | low | A generic call with a `Copy` place argument followed by an argument that writes the place (`pr(c.n, bump(mut c))` with `fn pr[T: Display](a: T, b: T)… | 6a51dbc47e |
| B-2026-11-01-36 | mir-llvm | medium | Kata 139's word-break DP inner loop ran 1.18x legacy's instructions on the MIR backend (x86 callgrind; 1.75x on M5): LLVM's O2 unroll threshold left… | 9cde7995eb |
| B-2026-11-01-37 | effect | medium | `par { 1, 2 }` inside a `TaskGroup` task is refused as a `writes(Stdout)` conflict between its two branches when the task holds a local whose `Drop`… | d0ee689070 |
| B-2026-11-01-38 | mir | low | A whole `Array[i64, 2]` passed to a bare (copying) parameter is held as a borrow across the later arguments: `sh(a, set(mut a))` and `sh(a, { a[0] =… | 6a51dbc47e |
| B-2026-11-01-39 | effect | medium | The drop of a `dyn` value is charged no effects, against core-semantics 12 default 3 (a drop's effects are its type's drop glue's): `par { { let d =… | 58b9c2df3e |
| B-2026-11-01-41 | typecheck | low | Two later-track features named in core-semantics 13 are accepted silently and run as ordinary code: `comptime fn sq(n: i64) -> i64 { n * n }` checks… | d68355351e |
| B-2026-11-01-42 | mir-llvm | medium | Kata 147's insertion-sort-list runs legacy's instruction count on the MIR backend but 1.36x its cycles: the seq lane went from 0.72x to 0.99x rustc -… | cd7fd85dec |
| B-2026-11-01-43 | mir-llvm | low | Kata 164's maximum-gap runs 1.30x legacy's instructions on the M5 (arm64) MIR backend (1.19x its cycles); on x86 it measures 1.06x, so the gap is arm… | cd7fd85dec |
| B-2026-11-01-44 | mir-llvm | medium | Kata 170's Map[i64, i64]-heavy two-sum-III runs FEWER instructions on the MIR backend than legacy (0.75x) but 1.43x its cycles: the seq lane went fro… | 591fc8693a |
| B-2026-11-01-46 | mir | low | A map index assignment still reads a moved (non-`Copy`) place key after the value, against core-semantics 8.2: `m[key] = { key = "b".to_string(); 5 }… | 0e9a42f043 |
| B-2026-11-02-1 | mir-llvm+runtime | medium | Cancelling a `TaskGroup` task while it waits inside a `par {}` (or `par for`) whose other branch has already finished leaks: `par { { sleep(2 s); 1 }… | 59908af3782752651ec380b11bcb90bc0103f540 |
| B-2026-11-02-2 | mir-llvm | medium | Kata 225's queue-rotation loop (Vec push + index read + field update through a `mut ref` struct) runs 1.43x legacy's instructions on the MIR backend… | 7310a8f945 |
| B-2026-11-02-3 | mir-llvm | medium | Kata 242's valid-anagram byte loop runs 1.11x legacy's instructions on the MIR backend but 1.21x its cycles: the seq lane went from 1.08x to 1.30x ru… | 3d8ac8a2c5 |
| B-2026-11-02-4 | mir-llvm | medium | Kata 244's Map[String, i64] word-index lookups run 1.27x legacy's instructions on the MIR backend and 1.56x its cycles: the seq lane went from 1.16x… | 9f3cc6a792 |
| B-2026-11-02-6 | mir-llvm | medium | Kata 249's Map[String, Vec[String]] grouping (entry(k).or_insert(Vec.new()).push(w)) runs 1.24x legacy's instructions and 1.24x its cycles on the MIR… | 9f3cc6a792 |
| B-2026-11-02-7 | mir-llvm | medium | Kata 253 (meeting rooms II), dominated by sorting a fresh copy of 120k intervals by start each round, runs 1.17x legacy's instructions on the M5 (arm… | 3d8ac8a2c5 |
| B-2026-11-02-8 | mir-llvm | medium | The auto-par collect lowering (`#[par_order_free]` loop pushing into a Vec) holds the collected Vec twice at peak once there are two or more workers:… | 2939cedca0 |
| B-2026-11-02-9 | mir-llvm | medium | Growing a fresh Vec[i64] to 2M elements with push, once per round, runs 1.37x legacy's instructions on the MIR backend (1.13x its cycles): found in k… | 11d7eb7f01 |
| B-2026-11-02-10 | mir-llvm | low | Kata 276's par lane runs 1.44x slower than its 2026-08-28 feed while its comparators moved 1.06-1.09x and its seq lane held, so it scales 4.6x over s… | cd7fd85dec |
| B-2026-11-02-11 | mir-llvm | medium | Kata 294's memoised flip-game search (per-node successor Strings built char by char, Map[String, bool] memo) runs 1.60x legacy's instructions and 1.5… | 9f3cc6a792 |
| B-2026-11-02-12 | typecheck | high | A `shared` handle crosses tasks inside an unmarked `dyn T` that a task or `par` branch only VIEWS: `fn mkd(s: own S) -> dyn Bump` and two `TaskGroup.… | bc157b9903 |
| B-2026-11-02-13 | typecheck | low | A 0.8.0 program giving a `Vec[dyn Handler]` or `Vec[Fn(..)]` where the `+ CrossTask` form is expected gets a bare E0200 mismatch (dyn) or an opaque E… | 9dc35df0a8 |
| B-2026-11-02-14 | typecheck | high | A `shared` handle crosses tasks through a COLLECTION of unmarked `Fn` values coerced into a `+ CrossTask` slot: a `Vec[Fn() -> i64]` a call hands bac… | 9dc35df0a8 |
| B-2026-11-02-15 | typecheck | high | An unmarked `Fn` reaches a `+ CrossTask` slot through a GENERIC parameter (`collect()` into `Vec[Fn() -> i64 + CrossTask]`, `id[T](mkf(..))`, `mem.re… | 37030edcb2 |
| B-2026-11-02-16 | mir | medium | `mem.replace(mut f, g)` on a local of an erased `Fn` type is refused after check with `error[mir]: invalid MIR: assigning Fn() -> i64 to _6 of type c… | 66c81328c7 |
| B-2026-11-02-17 | mir-llvm | low | After B-2026-10-31-39, an auto-par task's loop still ran about 10% more instructions than the in-order one on kata 9 (Linux): the buffer reload and k… | e888029d05 |
| B-2026-11-02-18 | typecheck | high | After the B-2026-11-02-15 fix, five more generic paths still let an unmarked `Fn` into a `+ CrossTask` slot and race: a generic impl's `Bx.new(x)`, `… | 0d618d8646 |
| B-2026-11-02-19 | mir | high | Generic std ops on erased `Fn` values miscompile when a closure LITERAL is involved: `mem.swap` of two `Fn` locals is a no-op, and `Option[Fn].replac… | 66c81328c7 |
| B-2026-11-02-20 | parser | low | `(Fn() -> Fn() -> i64) + CrossTask` is a parse error (`Expected =, found +`); §6.3 says to parenthesize to mark the outer function type, and only `Fn… | 04ae52b256 |
| B-2026-11-02-21 | typecheck | medium | `*a = \|\| 9` through a `a: mut ref Fn() -> i64` parameter is refused with E0517 "cannot write to `a`: it is reached through a shared `ref`", though `a… | 206d1362de |
| B-2026-11-02-22 | mir-llvm | medium | The drop-soundness fuzzer was deleted with the legacy pipeline at M3 and has no v2 port: nothing fuzzes drop elaboration against ASan/LSan and a drop… | 041a8ce231 |
| B-2026-11-02-23 | mir-llvm | medium | Kata 71's simplify-path runs 1.40x legacy's instructions and 1.42x its cycles in the seq lane on the MIR backend, and its par lane slipped from 0.91x… | 791fe05de0 |
| B-2026-11-02-24 | mir-llvm | low | Kata 38's seq binary stays at 312 KiB on g96fca5370 where the B-2026-10-28-36 fix took 12 other kata seq binaries to 52-69 KB | ad984ef96b |
| B-2026-11-02-25 | parser | medium | 128 nested `{` followed by a statement keyword (`if`, `let`, `while`, ...) runs the parser out of memory: the refused statement is retried forever, o… | 1d2fc6d232 |
| B-2026-11-02-26 | mir-llvm | medium | A `Vec.with_capacity(n)` filled by a counting push loop runs 12.6x legacy's instructions on x86: mir::presize only takes a `Vec.new()` start, so ever… | 791fe05de0 |
| B-2026-11-02-27 | typecheck+mir-llvm | high | `tx.send(s)` on a `Sender[ref String]` with a plain `String` local passes check, though §5.7 forbids a view in a channel; at run the receiver reads g… | 5c73fea787 |
| B-2026-11-02-28 | typecheck | medium | A method with a `self` receiver that returns a reference may return a borrowed ARGUMENT (`fn pick(self, k: ref String) -> ref String { k }`): §5.4's… | 77d7baa2b1 |
| B-2026-11-02-29 | typecheck | medium | Returning `m.get(k) ?? ""` from a function is refused (E0516 'cannot drop the temporary value'), and so is `let o = m.get(k); o ?? ""` ('returned bor… | 77d7baa2b1 |
| B-2026-11-02-30 | typecheck | medium | Two `TaskGroup` tasks that write DISJOINT fields of one local (`p.x` and `p.y`) are refused: 'a task of `g` captures `p` by `mut ref`, which no other… | 77d7baa2b1 |
| B-2026-11-02-31 | typecheck | low | A non-escaping `Fn` parameter may be captured by a closure the callee returns (`fn wrap(f: Fn() -> i64) -> Fn() -> i64 { \|\| f() + 1 }`); §9.3 lets th… | 77d7baa2b1 |
| B-2026-11-02-33 | effect | low | Coercing a struct whose trait method uses a provider-rooted resource into `dyn At` reports E_NO_PROVIDER ('no provider for Clk .. | 55c64b1eca |
| B-2026-11-02-34 | ownership | medium | A local moved BEFORE an `if`/`match` and reassigned in EVERY branch is refused at the later use ('value s moved here, used again here'), though §3.4… | 25b7bffde2 |
| B-2026-11-02-35 | typecheck | medium | `return` inside a `par for` body is refused ('`return` may not cross out of a `par for` body'), though §11.7 makes `return` in a branch the same earl… | 1ca64acaf0 |
| B-2026-11-02-36 | effect | medium | Two `par` branches calling a generic `fn ap[F: Fn(i64) -> i64](x: i64, f: F)` with pure closures or a pure named fn conflict as 'unknown (a function… | 8876b6f720 |
| B-2026-11-02-37 | mir+mir-llvm | high | A string or integer LITERAL stored into a library `ref` slot (`v.push("lit")` on `Vec[ref String]`, `tx.send("lit")` / `tx.send(7)` on `Sender[ref T]… | def3886d61 |
| B-2026-11-02-38 | mir-llvm | high | llvm natives for `ref` element types: `Vec[ref String].fill(r)` panics out of memory (ASan global-buffer-overflow), and iterating a `SortedSet[ref St… | ebfe307c38 |
| B-2026-11-02-39 | ownership | low | A closure in an element of a let-bound tuple is treated as non-escaping by `karac check` (core_escape gives tuple elements tail_of(pos)), so a non-Co… | 59ca8f5a88 |
| B-2026-11-03-1 | parser | medium | The empty effect clause `with ()` on a function type (design.md § How a call through a function value gets its effects, Gowtham 2026-11-03) was a par… | 1446dde436 |
| B-2026-11-03-3 | typecheck | low | `[\|x: i64\| x + 1].to_vec()` into a `Vec[Fn(i64) -> i64]`, and `.clone()` of a `Vec[Fn(i64) -> i64]`, pass `karac check` and fail at run ('clone of Fn… | f907cfec34 |
| B-2026-11-03-4 | parser | low | `move \|..\|` is refused with 'use `own \|...\|` for closure capture-by-value', naming a capture prefix core semantics §9.6 removed (and refuses as E0519) | f907cfec34 |
| B-2026-11-03-5 | typecheck | low | A method call on a function value (`f.clone()`, `f.foo()`) checks to an error with no diagnostic; MIR then reports "a type that failed to check is no… | ea349dd9ce |
| B-2026-11-03-6 | ownership | low | Moving a place a live closure captures by `ref` (`let f = \|\| s.len(); let t = s; f()`) is refused with the §3.3 LOOP message ('value s is moved insid… | faa987cba1 |
| B-2026-11-03-7 | typecheck | low | `Err(1)?;` as a statement (nothing fixes the Ok type) is refused by `karac check` with the MIR builder's internal message `an expression whose type d… | d168fd2a53 |
| B-2026-11-03-8 | typecheck | high | A `shared` handle enters a `par` branch or `par for` body through a local closure's environment: `let a = n(1); let f = \|\| a.id; let (x, y) = par { f… | 21c89bae17 |
| B-2026-11-03-9 | interp | medium | `karac __mir-run` runs an iterator adaptor chain with a closure (`v.iter().filter(\|x\| ..).count()`, `.map(\|x\| *x).max()`) about 11x slower than the s… | dff4157d25 |
| B-2026-11-03-10 | typecheck | low | `karac fix` left two edits its diagnostics name unapplied: `==` / `!=` on a type without `PartialEq` says "add #[derive(PartialEq)]" with no fix-it (… | 67f5b2209d |
| B-2026-11-03-11 | typecheck | low | `s + t` with a `Str` on the left said only "type 'Str' does not implement trait Add", with no hint and no `karac fix` edit, where `String + Str` alre… | 592000a587 |
| B-2026-11-03-12 | typecheck | medium | Mixed-width arithmetic is refused where design.md § Mixed-width operands widens the narrower operand within its kind: `u + w` (u: u8, w: i64), `s * i… | 6f7041393d |
| B-2026-11-03-13 | ownership | low | `String.from_utf8(v)` moves its argument, though strings.md declares `from_utf8(bytes: Slice[u8])`, a borrowed parameter: calling it twice on one loc… | d41d935108 |
| B-2026-11-03-14 | typecheck | low | A generic parameter fixed only by an integer literal is reported unsolved instead of taking the literal's default: `c.or_else(\|e\| Err(0))` (c: Result… | 8e206842a3 |
| B-2026-11-03-15 | mir+mir-llvm | low | A negative capacity: `String.with_capacity(-3)` gives an empty string on the interpreter and panics with a bare `explicit panic` on the compiled back… | 3331af4516 |
| B-2026-11-03-16 | mir+mir-llvm | low | `substring` with a negative start gives "" instead of clamping the start to 0 as strings.md says ("Out-of-range, negative or inverted bounds saturate… | 3331af4516 |
| B-2026-11-03-17 | typecheck | low | A binding destructured from a `Map` / `SortedMap` `iter_mut()` item is typed `V`, not `mut ref V`, when `V` is Copy: `for (k, v) in m.iter_mut() { *v… | 597c6ce7e2 |
| B-2026-11-03-18 | mir+mir-llvm | high | `Client.get` / `Client.post` under a `Network` fake crash on every lane: the interpreter stops with `in Fh.__get at bb13[term]: a String place holds… | 17c180c647 |
| B-2026-11-03-19 | parser | medium | `Some(ref place)`, the spec's own way to build an `Option[ref T]` (design.md § `ref place` borrows a place: `let first = Some(ref self.items[0]);`),… | 53e7f5f11a |
| B-2026-11-03-20 | ownership | medium | `karac check` refuses moving a binding after an adaptor chain rooted at a range reads it (`let rest: Vec[i64] = (1..n + 1).filter(\|i\| not side.contai… | 53e7f5f11a |
| B-2026-11-03-21 | typecheck | low | A type mismatch whose found type is an `Option`/`Result` wrapping the expected one (`s.push(t.char_at(i))`, `let c: char = t.char_at(0)`, `take(t.cha… | 1a547cb615 |
| B-2026-11-03-22 | cli | medium | `karac fix` on `'L' => (dx, dy) = (-dy, dx),` (a braceless match arm) writes `'L' => dx, dy = -dy, dx,`, which does not parse, so it refuses to write… | 1a547cb615 |
| B-2026-11-03-23 | cli | medium | `let undo = vec!["LR", "RL"][k];` (an element of a TEMPORARY, binding only read) gets E_INDEX_MOVE_NON_COPY's borrow fix `ref vec![..][k]`, and `kara… | 1a547cb615 |
| B-2026-11-03-24 | typecheck | medium | `karac fix` appends `.to_string()` / `.clone()` after an expression that binds looser than a postfix `.`, so `names.contains(*r)` (r: ref Str, names:… | c42cbe441c |
| B-2026-11-03-25 | typecheck | low | `*a` on a non-reference, non-`Copy` value compared with `==` (`seqs.iter().position(\|el\| el.id == *a)` with `a: (i64, String)` bound by value) gets "… | c42cbe441c |
| B-2026-11-03-26 | typecheck | medium | A `Map` / `SortedMap` key is writable through `iter_mut()`: `for (k, v) in m.iter_mut() { k.push_str("x"); }` (m: Map[String, i64]) is accepted and c… | 597c6ce7e2 |
| B-2026-11-03-27 | parser | low | Rust's `&` sigil gets no fix in a pattern (`for &(x, y) in v.iter()`, `\|&x\| ..`, `&3 =>`, `&mut z`), on an argument (`f(&x)`, `f(&mut x)`) or as a bo… | 25a40f3d26 |
| B-2026-11-03-28 | typecheck | low | `x.as_str()` passed where a `ref String` is wanted (`["rc", "si", "ser"].contains(w[2].as_str())`, `w: Vec[String]`) gets the fix that appends `.to_s… | 3bd1b54849 |
| B-2026-11-03-29 | mir-llvm | low | `mir_serve_ws_upgrades_while_serving` is schedule-sensitive: under the full `cargo test --features llvm` load the built server's output lacks `closed… | 8c67849f18 |
| B-2026-11-04-1 | parser | low | `let s = mut place;` (a call argument's marker where a borrow for writing is wanted) is refused as "'mut' is a reserved keyword and cannot be used as… | 4230ad12ec |
| B-2026-11-04-2 | typecheck | low | `items.into_iter()` on a bare `Vec[Item]` parameter whose `Item` is not clonable gets the fix `items.clone().into_iter()`, which does not check eithe… | 4230ad12ec |
| B-2026-11-04-3 | ownership | low | `let s = v[i].field;` with `s` only read gets the fix that clones the field, where `let s = v[i];` already gets `ref`; under `self`, a write to any o… | 4230ad12ec |
| B-2026-11-04-4 | mir | medium | `for f in rx.into_iter()` over a `Receiver[Fn(i64) -> i64 + CrossTask]` passes `karac check` and fails the MIR borrow check inside the library: `borr… | 555b8efd68 |
| B-2026-11-04-5 | parser | medium | Remainder after c2a612b69c (which names and fixes a Rust `::` path in an expression or a pattern: `Vec::new()`, `String::from("ab")`, `i64::MAX`, `Ha… | 75832f76f1 |
| B-2026-11-04-6 | parser | low | Rust angle-bracket generics on a DECLARATION (`struct Bx<T> { v: T }`, `impl<T> Bx<T> { . | 6932c9f1e9 |
| B-2026-11-04-7 | parser | low | Two Rust-isms are fixed into a program that is still refused: `fn first(s: &[i64])` is fixed to `ref [i64]`, whose `[T]` diagnostic then offers no ma… | 6932c9f1e9 |
| B-2026-11-04-8 | mir | high | UFCS on a program trait (`A.m(s)`, design.md §9 Method resolution) is refused by `karac check`, `karac run` and `karac build` with "a call that leave… | 5bb971cb18 |
| B-2026-11-04-9 | mir | high | In a generic body under `T: Named`, `T.label()` calls the concrete type's INHERENT associated function of the same name instead of the trait's (print… | 5bb971cb18 |
| B-2026-11-04-10 | mir | medium | A program associated function named like a comptime reflection method (`fn name() -> String` in a trait or inherent impl) passes `karac check` but `P… | 5bb971cb18 |
| B-2026-11-04-11 | mir | medium | A generic body under one bound `[T: B]` calling `x.m()` is refused ("the method call `.m` with no resolved callee is not lowered yet") when the concr… | 5bb971cb18 |
| B-2026-11-04-12 | ownership | medium | The ownership check charges `s.m()` with the TRAIT method's `own self` when an inherent `m(self)` is the method chosen: `let s = S {}; println(s.m())… | ee3803b7f8 |
| B-2026-11-04-13 | typecheck | medium | An impl method with a STRONGER mode than its trait (`fn look(mut ref self)` for the trait's `fn look(self)`) is not refused at the impl (design.md §9… | d9cf47b567 |
| B-2026-11-04-14 | typecheck | medium | Coherence is not checked where impls are declared for three overlap shapes: a duplicate `impl T for S` twice, `impl[X: Eq] T for B[X]` beside `impl[X… | d9cf47b567 |
| B-2026-11-04-15 | typecheck | low | An inherent impl on a standard type, `impl Vec[i64] { fn total(self) -> i64 { . | d9cf47b567 |
| B-2026-11-04-16 | typecheck | medium | A blanket impl of the program's own trait, `impl[T: Display] Twice for T { . | d9cf47b567 |
| B-2026-11-04-17 | typecheck | low | `for x in it` with `it: I` or `it: mut ref I` (I: Iterator[Item = i64], a borrowed iterator) reports "expected 'i64', found 'ref I'" at the first use… | ee3803b7f8 |
| B-2026-11-04-18 | effect | high | The effects of the `From` impl that a `?` inserts are not charged to the enclosing function (design.md §10: "The conversion is a real call for effect… | 7c0ff1e85c |
| B-2026-11-04-19 | parser | medium | Script mode's implicit `main` returns `()`, so a top-level `?` is refused with "'?' requires the enclosing function to return `Result` or `Option`, f… | ab80e422aa |
| B-2026-11-04-20 | mir | low | `#[track_caller]` on a program function is accepted but does nothing: a wrapper `#[track_caller] fn must(o: Option[i64]) -> i64 { o.unwrap() }` calle… | aed0200c63 |
| B-2026-11-04-21 | resolver | medium | In a package, importing a NESTED module itself (`import db.connection;`, then `connection.open(7)`) is refused with "error[E0112]: unknown module `db… | ab80e422aa |
| B-2026-11-04-22 | typecheck | medium | Struct field visibility does not follow design.md §5's three levels: a field with no keyword is refused from another module of the SAME package ("pri… | c32f787973 |
| B-2026-11-04-23 | cli | low | `karac run` in a package whose src/ has both main.kara and lib.kara runs main.kara and exits 0, while `karac check` and `karac build` refuse it ("a K… | ab80e422aa |
| B-2026-11-04-24 | typecheck | low | A type mismatch between two same-named types from different modules prints the compiler's internal mangled names: `expected 'ref P__a', found 'P__b'`… | 3942d674ca |
| B-2026-11-04-25 | effect | high | Two `par` branches that write the same USER-DECLARED resource are never refused: with `effect resource Db;` and `fn put() with writes(Db)`, `par { pu… | 7c0ff1e85c |
| B-2026-11-04-26 | effect | medium | Call sites are charged the callee's BODY effects, not the declared contract: a `pub fn g() -> i64 with writes(Stdout) { 1 }` called in two `par` bran… | 9ae967f6a6 |
| B-2026-11-04-27 | effect | medium | A declared effect clause is not verified on a PRIVATE function or on a trait's DEFAULT method body: `fn f() with reads(Clock) { println("x"); }` and… | 98ef92e24b |
| B-2026-11-04-28 | effect | medium | `#[no_effect(allocates(Heap))]` (design.md §12's real-time guarantee) misses most allocations: `[1, 2].to_vec()`, `"a".to_string()`, an f-string, a `… | 98ef92e24b |
| B-2026-11-04-29 | effect | medium | Integer `/` and `%` and checked-mode overflow arithmetic are not `panics` atoms: `#[no_effect(panics)] fn f(a: i64, b: i64) -> i64 { a % b }` (and `a… | 98ef92e24b |
| B-2026-11-04-31 | mir-llvm | low | Seq binaries of katas 297, 315, 330, 331, 332, 336 and 394 are still 319-336 KB on gd33cb285d and gf0017054a, after ad984ef96b took katas 38, 282 and… | 5984ed3080 |
| B-2026-11-04-32 | mir-llvm | low | Kata 297's peak RSS is 1.35x Rust's (233-235 MiB vs 173 MiB) on the MIR backend, where the 2026-08-28 feed read 179.9 vs 174.3 MiB | 76e7c027a5 |
| B-2026-11-04-34 | ownership | medium | A closure or task made in each arm of an `if`, each moving the same value in, is refused as "moved into a closure, used again here" (or "moved into a… | f575bce6f0 |
| B-2026-11-04-35 | ownership | medium | `match p.c { C.At(s) => . | f575bce6f0 |
| B-2026-11-04-36 | cli | low | `karac fmt` rewrites a script-mode file into an explicit `fn main()`, its top-level statements moved after every item; since B-2026-11-04-19 it also… | e5ccf4fc0c |
| B-2026-11-04-40 | mir | medium | A trait's default method body calling `self.m()` is refused as ambiguous ("ambiguous method 'm' on receiver of type 'S'") in the copy for a type that… | d78c610976 |
| B-2026-11-04-41 | mir-llvm | medium | Kata 344's two-pointer reverse stays scalar: LLVM 18's SCEV cannot count a loop whose bound moves (`while i < j { ...; i += 1; j -= 1 }`) and its dep… | 6dcdc1a604 |
| B-2026-11-04-37 | mir-llvm | low | Kata 342 ran 1.20x legacy's instructions and 1.39x its cycles on the MIR backend; arm64 loop branched twice (ctpop over SimplifyCFG's phi-fold thresh… | a723c25762 |
| B-2026-11-04-38 | mir-llvm | low | Kata 3629's seq peak RSS rose from 59.9 to 88.9 MiB on the MIR backend while Rust held at 60.1 MiB | d273d89f37 |
| B-2026-11-04-42 | mir | medium | The MIR effect pass gives every call of an `extern` function every effect, ignoring its declaration: two `par` branches that each call a `pure fn` fr… | b8f73d1a3f |
| B-2026-11-04-43 | resolver+parser | low | Rust's `Box.new(e)` (what `Box::new(e)` becomes) gets `undefined name 'Box', did you mean 'Hex'?` with no fix, though Kāra has no `Box` (design.md §… | a3e6d2fd30 |
| B-2026-11-04-44 | typecheck | low | An empty list literal with no expected type (`let mut w = [];`) is refused by `karac check` with MIR's internal "a type that failed to check is not l… | aa857c6355 |
| B-2026-11-04-45 | typecheck | low | A `let` keeping a borrow of a prefix `Vec` literal's temporary (`let g = Vec["a"].last();`, and `vec!["a"].last()`, the same node) is refused late, a… | aa857c6355 |
| B-2026-11-04-46 | typecheck | low | Reading a `private` field from another directory is refused with code E0221 (PrivateTypeInPublicSignature) instead of E0111 PrivateItemAccess, which… | 3942d674ca |
| B-2026-11-04-47 | mir | high | With two `From` impls on one type, `Out.from(c)` calls the FIRST impl's `from` whatever the argument (prints 6 for `Out.from(C { n: 6 })` whose impl… | c922a27b91 |
| B-2026-11-04-48 | mir | medium | `self.raw()` in a method of a `distinct type` whose receiver is a bare (borrowed, D5) `self` is refused after check: "`raw` of a borrowed distinct va… | 36c03e2120 |
| B-2026-11-04-49 | mir | medium | A trait default method that calls another of the trait's methods (`fn shout(self, n: i64) -> String { let s = self.tag(); . | 36c03e2120 |
| B-2026-11-04-50 | mir | medium | An inherent impl on a type alias of the program's own struct (`type Wrap = W; impl Wrap { fn get(self) -> i64 { . | fbe0caf3bc |
| B-2026-11-04-51 | typecheck | low | An iterator method called straight on a sequence draws an edit-distance guess with no fix for `find`, `position`, `nth`, `rev` and `flatten`: `window… | 6ef646c141 |
| B-2026-11-04-52 | typecheck | low | `.iter().flatten()` over collection items names the exact rewrite ("write `.flat_map(\|v\| v.iter())` in place of `.flatten()`") but carries no fix, so… | e9fa6bddd1 |
| B-2026-11-04-53 | cli | low | `karac fmt` breaks an expression holding two `unsafe { . | 19775ae374 |
| B-2026-11-04-54 | typecheck | medium | With two impls of a user generic trait on one type (`impl Make[i64] for Out` + `impl Make[String] for Out`, `fn make(x: T) -> Self`), `Out.make("abcd… | 2b657fbeba |
| B-2026-11-04-55 | mir-llvm | low | `PriorityQueue.pop` sifts down by swapping, two comparisons a level, where Rust's `BinaryHeap::pop` sifts a hole to the bottom with one: kata 1046's… | 5bd3da78cc |
| B-2026-11-04-56 | mir | medium | `karac check` does not run the MIR borrow check on a function that `main` never reaches, so a library package's `pub fn bad() -> Str { let s = "abc".… | 8c480bd9df |
| B-2026-11-04-57 | mir | medium | Three pattern shapes design.md §5 specifies pass the front end but are refused with "this pattern is not lowered yet": an or-pattern whose alternativ… | 6ad941be02 |
| B-2026-11-04-58 | typecheck | medium | A list literal with a `Set[T]` expected type is refused: `let s: Set[i64] = [1, 2, 2, 3];` -> "expected 'Set[i64]', found 'Vec[i64]'", though design.… | 167711e424 |
| B-2026-11-04-59 | mir | medium | Mixed-width compound assignment passes `karac check` and then fails in MIR: `total_i64 += n_i32` (legal per design.md §5) stops `run`/`build` with "i… | c25557486e |
| B-2026-11-04-60 | typecheck | low | Small §5 divergences in refusals and lints: an or-pattern whose alternatives bind different names (`E.A(x) \| E.B(y)`) is accepted; `mut port: i64` on… | 167711e424 |
| B-2026-11-04-61 | typecheck+cli | low | `m.get(k) == Some(v)` with a non-Copy value (Map[String, String]) refuses E0208 "cannot compare 'Option[ref String]' and 'Option[String]'" with no ed… | be4628d4bc |
| B-2026-11-04-62 | typecheck | medium | Rebinding an `own MutFn` / `own Fn` parameter with `let mut f = f;`, the spelling design.md §7 Function types and kinds prescribes for calling an own… | e06766fe96 |
| B-2026-11-04-63 | parser | medium | A struct pattern with a `..` rest in parameter position is a parse error ("Expected identifier, found `..`"), though design.md §7 Destructuring param… | 1776ad9f5e |
| B-2026-11-04-64 | typecheck | medium | An unbound method value `User.adult` gets the type `Fn() -> bool` with the receiver dropped, so `let check = User.adult; check(u)` is refused ("expec… | 8b04a3215e |
| B-2026-11-04-65 | typecheck | medium | An escaping closure that captures a String and calls `println(s)` is classified OnceFn ("consumes captured binding `s`") and refused for an Fn slot;… | e06766fe96 |
| B-2026-11-04-66 | effect | medium | A function type with no effect clause in a struct field or a return type is treated as pure ("has effect writes(Stdout) not declared in slot [pure]")… | d50ea9e470 |
| B-2026-11-04-67 | ownership | medium | A use after move of a function value (a OnceFn closure called twice, or an Fn value called after `let h2 = h`) passes karac check and is refused only… | e06766fe96 |
| B-2026-11-04-68 | typecheck | low | A named-parameter default that names a function (`g: Fn(i64) -> i64 = id`) is refused ("'id' is not a known const"); design.md §7 says a function nam… | e06766fe96 |
| B-2026-11-04-69 | typecheck | low | §7 functions/closures: five spec errors accepted (MutFn called from a plain `let`; OnceFn bare parameter; UFCS receiver without `mut`; `a == r` with… | e06766fe96 |
| B-2026-11-04-70 | ownership | medium | A closure cannot capture a `frozen` binding at all ("`frozen` alias `g` is captured by a closure"), and a TaskGroup.spawn closure over one is also E_… | bf270628d9 |
| B-2026-11-04-71 | typecheck | medium | design.md §11 #[must_use] mandate unmet: a discarded `m.lock()` guard, iterator adapter (`v.iter().map(..);`) and `s.trim()` give no warning, so a lo… | 75fe0ae07f |
| B-2026-11-04-72 | typecheck | low | §11 sharing roll-up: `own self` on a sync type and `weak` on a non-shared type accepted; borrow-flag panic does not name the field or both spans; sev… | b378b20b59 |
| B-2026-11-04-73 | typecheck | low | A `for` over a borrow of a concrete non-`Copy` iterator (`it: VecIntoIter[i64]`, a user `impl Iterator for Up`) is refused with MIR's internal "a `fo… | aa857c6355 |
| B-2026-11-04-74 | mir+mir-llvm | low | A shift by an out-of-range amount (`x << 32` on an i32, `x << -1`) panics with "integer overflow" on run and build; design.md §5 says it panics with… | 6f9132f62b |
| B-2026-11-04-75 | typecheck | low | An empty list literal compared with a typed `Vec` (`v == []`) is refused with MIR's internal "a type that failed to check is not lowered yet"; the ot… | e06766fe96 |
| B-2026-11-04-76 | parser | low | A closure parameter written `mut acc` (Rust's `fold(init, \|mut acc, x\| ..)`) is refused with "'mut' is a reserved keyword and cannot be used as an id… | dd0d3d3fa3 |
| B-2026-11-04-77 | typecheck | low | `Vec` has no `drain(range)`: Rust's `v.drain(i..i + 2);` gets only "no method 'drain' on type 'Vec'" with no hint, though `remove`, `truncate` and `s… | 4e87c65465 |
| B-2026-11-04-78 | typecheck | medium | A borrowed parameter passed where a library constructor takes `own Vec[T]` (`PriorityQueue.from(v)`, `max_first_from(v)` with `v: Vec[i64]`) draws "e… | 5190a456b6 |
| B-2026-11-04-80 | typecheck | low | Remainder of B-2026-11-04-71: `.unwrap()` on an Option gets no `.expect("...")` suggestion with a fix (design.md §11 Ergonomics), and a use of a `#[d… | 7d007768dd |
| B-2026-11-04-81 | interp+mir-llvm | low | Remainder of B-2026-11-04-72: the shared-field borrow-flag panic says "already borrowed: this `shared` field is borrowed through another handle" with… | 872c092c69 |
| B-2026-11-04-82 | mir-llvm | high | An integer tuple literal passed as a collection key or compared with `==`/`<` keeps the default (i64, i64) layout instead of the declared narrow tupl… | 89324c3d8f |
| B-2026-11-04-83 | mir-llvm | low | The backend does not emit the facts LLVM 19-22 added: `range` attributes on parameters and returns, `nuw`/`nusw`/`inbounds` flags on GEPs beyond inbo… | e75d97a500 |
| B-2026-11-05-1 | typecheck | low | `a == r` with a non-Copy `T` beside a non-literal `ref T`: Gowtham decided 2026-11-05 that comparisons see through `ref` on either side for any T (de… | 2a3202d89d |
| B-2026-11-05-2 | ownership | low | A MutFn closure passed to an `own MutFn()` parameter twice (`take(f); take(f);`) is accepted on every lane and runs as if copied; design.md §7 says a… | d78c610976 |
| B-2026-11-05-3 | typecheck+other | low | Remainder of B-2026-11-04-80: the editor code action that rewrites `.unwrap()` on an ordinary (non-`weak`) Option to `.expect("…")` does not exist; d… | 5fd8c20172 |
| B-2026-11-05-4 | mir-llvm | medium | Teemo_attacking ran 2.8x slower on LLVM 22 than on LLVM 18 (x86, 357 ms vs 126): LLVM 22's LICM stopped hoisting the Vec's data-pointer load on karac… | ce0cf3209b |
| B-2026-11-05-5 | effect | medium | Two `par` branches that each apply `?` to a `Result[D, E]` whose `D` has a printing Drop body conflict on writes(Stdout), though nothing drops inside… | 8982776215 |
| B-2026-11-05-6 | typecheck | low | §13 concurrency roll-up: `return` out of a `par for` body accepted; par conflicts never carry E0408; E_NOT_CROSS_TASK calls a type that contains a sh… | c519e4b881 |
| B-2026-11-05-7 | resolver | high | In a package (kara.toml + src/), `import std.sync.{..}`, `import std.net.{..}` and `import std.json.{..}` are refused "unknown module `std.sync`" by… | 8b95f3e4b1 |
| B-2026-11-05-8 | ownership | medium | `assert_eq(v, e)` and `assert_ne(s, "b")` move a non-Copy operand, so using it afterwards is E0500; design.md §14 says both operands are borrowed and… | c519e4b881 |
| B-2026-11-05-9 | typecheck | low | §14 testing roll-up: assert_eq/assert_ne failure messages omit the `left:`/`right:`/`value:` lines; assert_eq accepts a type with no Debug; `karac te… | 984142c99a |
| B-2026-11-05-10 | mir-llvm | high | Kata 73's bench runs 21x slower on the default pool than on one worker (10.1 s against 0.48 s, 4 workers, x86): auto-par runs its 20-row matrix build… | ebd092c397 |
| B-2026-11-05-11 | effect | medium | The target gate never refuses ProcessTable on wasm_wasi: a program that spawns a process passes `karac check --targets=all` and a `[build] targets =… | 8982776215 |
| B-2026-11-05-12 | cli | low | §16 targets roll-up: artifact paths differ from the spec; the multi-target check JSON is neither tagged nor deduplicated; `--targets=all` also runs w… | c48ad61f26 |
| B-2026-11-05-13 | cli | medium | `--output=jsonl` (check and build) never emitted a `diagnostic` event for resolve, typecheck, effect or ownership errors, so a client could not rebui… | 984142c99a |
| B-2026-11-05-14 | cli | medium | `karac fmt` drops the trailing comma of a one-element tuple TYPE, so `(i64,)` becomes `(i64)` and the formatted program no longer typechecks (and a s… | 0dcb18ba9e |
| B-2026-11-05-15 | other | medium | kara-lsp treats a file inside a package as package-less: `import util;` in src/main.kara of a package with kara.toml gets a false `unknown module uti… | 8dff1b2177 |
| B-2026-11-05-16 | typecheck+cli | medium | In a package, `karac check` drops the cross-module `deprecated` warning unless the check also has an error; `karac build` reports it, and a single fi… | 8b95f3e4b1 |
| B-2026-11-05-17 | cli | low | §17 tooling roll-up: batch JSON lacks the 20-per-file cascade cap; `--emit-timings` unknown; explain rejects the symbolic codes the text prints; tool… | 8039f0b7dd |
| B-2026-11-05-18 | runtime+cli | low | §17 panic record and debugger contract are not emitted: no JSONL panic event on stderr under `karac run`/`karac test`, no record `kind`, so `karac de… | fe1f97379f |
| B-2026-11-05-19 | resolver | high | Parallel assignment (`a, b = b, a`, `v[i], v[j] = v[j], v[i]`) in ANY package panics the compiler: `entered unreachable code: StmtKind::MultiAssign i… | 8b95f3e4b1 |
| B-2026-11-05-20 | mir+mir-llvm+interp | high | A `for` over an open range `lo..` wraps silently past the type's MAX on JIT, debug and release AOT (`for i in (i64.MAX - 1)..` yields MAX-1, MAX, MIN… | 479d7a3253 |
| B-2026-11-05-21 | resolver | medium | In a package, every user operator-trait impl (`impl Add for V`, `impl Index[(i64, i64)] for Grid`, ...) is refused with E0108 `user-defined impl Add… | 8b95f3e4b1 |
| B-2026-11-05-22 | ownership | medium | Definite assignment ignores diverging paths: `let x: i64; if k > 0 { x = k; } else { return 0; } x` and the same with `panic(..)` in the else or in a… | c519e4b881 |
| B-2026-11-05-23 | typecheck+parser | low | §6 expressions roll-up: `if c { 5 }` in statement position is accepted; an unparenthesized struct literal in an `if` condition is accepted; `u == ()`… | a78baaf0dd |
| B-2026-11-05-24 | typecheck | low | A use of a deprecated item through a module binding (`import util;` then `util.old()`) is warned with the caret on the call's closing paren, not on `… | 244ab1cf33 |
| B-2026-11-05-25 | mir-llvm | low | The backend still emits none of the LLVM 19-22 facts B-2026-11-04-83 left: `range` attributes on parameters and returns, `icmp samesign`, and `captur… | 049261c58f |
| B-2026-11-05-26 | cli | low | package mode: `karac check --output=jsonl` on a package member prints `type_error` lines (no `type` stream, no ids) and stops at typecheck, `karac bu… | 532e17a65b |
| B-2026-11-05-27 | mir-llvm | medium | x86, host-dependent: min_stack (155) ran about 3.5x slower on LLVM 22 than on LLVM 18 because a four-case switch with an unreachable default became a… | 787f0559a5 |
| B-2026-11-05-28 | cli | low | diagnostics carry no `concept` id, and `karac explain` knows no concept ids from the diagnostic taxonomy (`ownership/move-semantics`) and no items (`… | 7966b30c9e |
| B-2026-11-05-29 | cli | low | E0400 carries no `derivation`, `inferred_effects` or `declared_effects` and `karac fix` cannot add the missing clause; a match missing an arm is not… | 49c047aa9a |
| B-2026-11-05-30 | cli | low | fix hints have two shapes, neither the spec's `diff {file, line, old, new}`: single-file JSON gives `replacement {offset, length, text}` beside a pro… | 7449bc90ba |
| B-2026-11-05-31 | cli | low | `karac fix` has none of the five style lints (counter `while` to `for`, `x = x + e` to `+=`, a final `return e;` to `e`, a redundant `.iter()` / `.to… | 2f7442d85a |
| B-2026-11-05-32 | cli | low | `karac fmt` does not sort methods (an impl's `zeta` stays before `alpha`; imports are sorted) (design.md § Canonical formatting: methods sorted, impo… | a81fc12853 |
| B-2026-11-05-34 | mir-llvm | low | The v2 backends emit none of the debugger contract's runtime metadata (design.md § Debugger contract, elements 1-3): no spawn-site ids per `par {}` b… | 12215de11b |
| B-2026-11-05-35 | typecheck | low | `(0..n).step_by(2).rev()` is "no method 'rev' on type 'IterStepBy'": Rust's `StepBy` walks back over a range, Kāra's has no `next_back` and the diagn… | 9ad19508aa |
| B-2026-11-05-36 | mir-llvm | low | The v2 debugger metadata lacks two pieces of the contract: `list_tasks()` rows carry no effect summary, and `WaitTarget.PeerTask` does not name the t… | 7241c6d35a |
| B-2026-11-05-37 | typecheck+mir | low | `RangeTo[T]`, `RangeToInclusive[T]` and `RangeFull` are undefined type names, and `..2`, `..=2` and `..` as values are `the expression kind Range is… | 28d010faad |
| B-2026-11-05-38 | interp | high | `m.get_or(k, "lit")` on a `Map[String, String]` stops the MIR interpreter with `a String place holds no box` when the default is a string literal, hi… | 8a9475c0f8 |
| B-2026-11-05-39 | typecheck | medium | Passing a struct's `Fn` field to a bare `Fn` parameter is refused as a move out of a borrowed place (`apply(self.f, 1)` with `fn apply(f: Fn(i64) ->… | 34cb06e29c |
| B-2026-11-05-40 | typecheck | medium | A pattern over a bare parameter binds differently from one over a `ref` parameter: `match arg { E.R { xs, ms } => (xs, ms) }` reached through `fn get… | 34cb06e29c |
| B-2026-11-05-41 | typecheck | low | Rust's map methods `values_mut`, `retain`, `into_values`/`into_keys`, and on `SortedMap` `first_key_value`/`last_key_value`/`pop_first`/`pop_last`, a… | 2a3f845fd5 |
| B-2026-11-05-42 | cli | low | In a package, `import utils;` naming no module is reported as E0223 `circular module dependency: <crate root> → <crate root>` instead of an unknown m… | 8dff1b2177 |
| B-2026-11-06-1 | mir-llvm | low | Kata 354 runs 0.81x legacy's instructions but 1.46x its cycles on the MIR backend; FIXED d70e6d474a (integer cmp as one scmp): M5 0.954x legacy's cyc… | d70e6d474a |
| B-2026-11-06-2 | mir-llvm | low | Kata 346's seq binary is 336,240 B on g162ee7a0e, after 5984ed3080 and e8466b1435 took the seven B-2026-11-04-31 katas to 83-99 KB | 8f6325386f |
| B-2026-11-06-3 | typecheck | low | A Rust `Vec<usize>` element type, once `karac fix` spells it `Vec[Vec[usize]]`, draws four `i64`/`usize` coercion and comparison errors with no fix;… | 52c81c1375 |
| B-2026-11-06-4 | typecheck | low | An explicit `ref name` pattern binding over an owned scrutinee cannot go into a tuple: `match p { P.R { ref xs, ms } => (xs, ms) }` (and `let H { ref… | 5f60e598f2 |
| B-2026-11-06-5 | cli | medium | `karac query` refuses a program that uses a library type `karac check` resolves (`undefined type 'WsMessage'`), so every query kind fails on the `kar… | 02a406c30c |
| B-2026-11-06-6 | typecheck | medium | two impls of one trait whose headers do not unify, `impl[T] Name for W[A[T]]` and `impl[T] Name for W[B[T]]`, are refused as a conflicting impl ("`W`… | 30f3ec3af7 |
| B-2026-11-06-7 | typecheck | medium | A type parameter that only a call's return type names is refused at the `let` (`let mut heap = PriorityQueue.new()`, a user `Bag.new()`, `fn empty[T]… | 2783b8c546 |
| B-2026-11-06-9 | cli | low | `karac query queries` and `query cost-summary` still report RC fallbacks (`rc_fallback_decision` with keep_rc / prefer_rc, `rc_ops`), which core sema… | f053625e94 |
| B-2026-11-06-10 | typecheck | medium | `let H { xs, n } = h` on a `shared struct H` is refused with `struct pattern `H` used but type is `H``, though core semantics §4.6 lets a shared scru… | bd1c1521a1 |
| B-2026-11-06-11 | typecheck | low | Remainder of B-2026-11-06-7: a later use still does not fix a call's return-only type parameter when the result is an `Option`/`Result` (`let mut bes… | 5f1222f497 |
| B-2026-11-06-12 | typecheck+mir | high | Polymorphic recursion hangs the compiler: `fn deep[T: Display](x: own T, n: i64) -> i64 { . | d4c94d8b4b |
| B-2026-11-06-13 | typecheck | medium | [Literal half split to B-2026-11-07-3; this row closed on shapes 5-6] Numeric literals do not default last: a literal bound by `let` (directly or pus… | 9bee6df335 |
| B-2026-11-06-14 | typecheck | medium | Const generic parameters cannot be used as values or solved from the expected type: `N` in a body is `a type that failed to check is not lowered yet`… | c79be19373 |
| B-2026-11-06-15 | typecheck | low | §8 roll-up: a `where` bound on a struct is never checked and an inline struct bound is not checked on an unannotated literal; `if c { 1i32 } else { 2… | bd8e607903 |
| B-2026-11-06-16 | typecheck+mir | high | A `const` in a pattern binds a fresh name instead of comparing against the constant: with `const LIMIT: i64 = 3`, `match x { LIMIT => .., _ => . | 473651262e |
| B-2026-11-06-17 | mir+mir-llvm | medium | Const-generic functions pass `karac check` but do not compile: the instance is named `first[i64, ()]` (N instantiated as `()`), so `a.len()` / `a[0]`… | c79be19373 |
| B-2026-11-06-18 | parser+typecheck | low | §3 roll-up: the value-class naming rule is enforced only on parameters, `struct _Hidden` / `const _MAX` are accepted, `requires`/`ensures`/`invariant… | d978271697 |
| B-2026-11-06-19 | typecheck | high | A cycle between constants crashes the compiler: `const AA: i64 = BB; const BB: i64 = AA;` (or `const AA: i64 = AA + 1;`) aborts `karac check` and `ka… | 609dbaa6da |
| B-2026-11-06-20 | typecheck+mir | medium | Constant initializers are not evaluated at compile time: `const SH: i32 = 1 << 40` silently reads as 0 (the same shift in a function panics `shift am… | 609dbaa6da |
| B-2026-11-06-21 | typecheck+ownership+parser | medium | A constant is treated as a place, not a fresh value: a second use of a non-Copy constant is `value 'ORIGIN' moved`, `ORIGIN.x` on a struct constant i… | 8f0eb3eae2 |
| B-2026-11-06-22 | resolver+typecheck+mir | medium | `#[target]` is not honoured on impl members, struct fields or enum variants: with `#[target(native)] fn f(self) -> i64 { 1 }` and `#[target(wasm_wasi… | e963bf0239 |
| B-2026-11-06-23 | cli+typecheck | low | §4 roll-up: the spec's own `[build].target = "x86_64-linux"` makes `karac build` demand a runtime archive for that triple, `main_linux.kara` is ignor… | e963bf0239 |
| B-2026-11-06-24 | typecheck | medium | `Copy` and `Drop` on one type are accepted, and the drop body then runs once per copy: `#[derive(Copy, Clone)] struct Pt` with `impl Drop for Pt` pri… | bd8600adec |
| B-2026-11-06-25 | typecheck | medium | Three standard trait shapes from §9 are refused: `#[derive(Default)]` on a generic struct (`Pair[T]`) is E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT inste… | bd8600adec |
| B-2026-11-06-26 | typecheck+mir | medium | A generic function returning `impl Iterator[Item = T]` cannot be consumed: `fn evs[T](xs: own Vec[T]) -> impl Iterator[Item = T] { xs.into_iter() }`… | e0abbf681d |
| B-2026-11-06-27 | typecheck | low | §9 roll-up of accepted-invalid programs: derive chains (`Eq` without `PartialEq`, `Ord` without `PartialOrd`, `Copy` without `Clone`), a derive plus… | bb8531c2c4 |
| B-2026-11-06-28 | typecheck+ownership | low | §10: a `defer` block that moves a binding (`defer { eat(a); }`) is accepted though §10 says cleanup blocks never capture by move (the unchecked_div/u… | ec6e0f84cc |
| B-2026-11-06-29 | effect | medium | Trait-method effect ceilings count default-permitted effects: §12's own `ReadOnlyView.save` (`with writes(Data) { panic("read-only view"); }`) is ref… | bd8600adec |
| B-2026-11-06-30 | typecheck | low | collections.md roll-up: `Vec.from_iter(dq)`, which collections.md and the compiler's own VecDeque diagnostic both recommend, is refused (`VecDeque[i6… | 69889deae9 |
| B-2026-11-06-31 | typecheck+mir | medium | check accepts String-only building methods (push_str, push, clear, truncate, pop, reserve) on a Str, even a borrowed parameter; every backend then re… | e0abbf681d |
| B-2026-11-06-32 | typecheck | low | CString lacks the CStr reading surface (to_str, to_string) and has no constructor from Slice[u8], which strings.md says exists | 69889deae9 |
| B-2026-11-06-33 | mir-llvm | medium | `c.to_digit(10)` on a char compiles to a `karac_mir_char` call that matches the method's name among twenty strings: half of kata 1058's benchmark | 2fee84b590 |
| B-2026-11-06-34 | mir-llvm | low | `Vec.sort_by(\|a, b\| b.cmp(a))` on 500 `i64`s takes about 1.9x Rust's `sort_by` in kata 1058's benchmark (2.5x before a53992a785), where a plain `sort… | a806504986 |
| B-2026-11-06-35 | typecheck | low | `Vec` has no `rotate_left` or `rotate_right`: `v.rotate_left(1)` is refused with "no method 'rotate_left' on type 'Vec'" | 2fee84b590 |
| B-2026-11-06-36 | ownership | medium | A closure in a method chain that ends in `?` (`let h = half(n).map_err(\|_\| bad.clone())?;`) is paired with a later move of what it reads (`return Err… | aeb2ac0023 |
| B-2026-11-06-37 | typecheck | medium | `Slice[T]` has no `iter_mut`, though docs/library/collections.md lists it (`mut ref T` items): `fn bump(xs: mut Slice[i64]) { for x in xs.iter_mut()… | aeb2ac0023 |
| B-2026-11-06-38 | typecheck | medium | A `TaskGroup.spawn` closure whose `Result` error type only a later annotation fixes is refused at lowering when its handles are kept in a `Vec`: `let… | b07a360a6d |
| B-2026-11-06-39 | typecheck | low | A let-chain (`if let Some(s) = v and kind == "group" { . | b07a360a6d |
| B-2026-11-07-1 | mir-llvm | low | Kata 7 runs 1.11x the cycles on arm64 at SimplifyCFG's phi-fold threshold 6 (a723c25762) than at 4: the rare `result == min_div and digit < -8` guard… | 5e869cb573 |
| B-2026-11-07-2 | typecheck+mir+mir-llvm | high | A generic function passed as an `Fn` value is instanced with its type arguments unsolved: `apply(shout, [7], 1)` with `fn apply[U](g: Fn(own U, i64)… | 957a835c43 |
| B-2026-11-07-3 | typecheck | medium | Numeric literals do not default last (split from B-2026-11-06-13): an unsuffixed literal is typed i64/f64 when inferred, so a later use cannot fix it… | 87e598fee9 |
| B-2026-11-07-4 | ownership | medium | `karac check` refuses moving a binding after an iterator chain's closure that reads it when the chain's root holds user records (`if ps.iter().any(\|p… | f50642eaba |
| B-2026-11-07-5 | mir | medium | A `let`-bound closure stored in an enum payload (`let c = \|d\| seen.n + d; let l = L.F(c);`) captures by `ref` and is refused with E0509 "a view of a… | f50642eaba |
| B-2026-11-07-6 | typecheck | medium | A program whose private type reuses a library type's name (`trait Receiver`, `struct Sender`) passes `karac check` but every `karac run` and `karac b… | f50642eaba |
| B-2026-11-07-7 | typecheck | medium | A `weak T` reached through a `ref weak T` binding (a loop over a `Vec[weak T]`'s `iter()`, a `weak` payload bound by a `match`) cannot be read or upg… | f50642eaba |
| B-2026-11-07-8 | typecheck | medium | Since 5f1222f497 (B-2026-11-06-11) a method call on a part of a tuple element of a generic `Vec[(K, K)]` walked by `for` is left unsolved: `for (k, v… | fbf880fff9 |
| B-2026-11-07-9 | typecheck | low | A const parameter only the return type names is not solved by a later use: `let b = zeros(); let c: Array[i64, 4] = b;` is refused at the `let` with… | 8a446aedae |
| B-2026-11-07-11 | typecheck | low | §9 remainder of B-2026-11-06-27: `dbg` on a Display-only type and derived `Debug`/`Clone` on a generic type ignore the bound (items 8, 9); `Option.fr… | 4fdb64b0c6 |
| B-2026-11-07-12 | mir | high | A plain binding of a non-`Copy` part of a `shared` value held in a local (`match m.a { Some(i) => . | 9d3631f381 |
| B-2026-11-07-13 | typecheck | medium | A closure that matches a captured enum against dotted unit variants only (`match cmp { Cmp.Lt => x < v, Cmp.Ge => x >= v }`) types as `OnceFn` on the… | 9d3631f381 |
| B-2026-11-07-14 | ownership | medium | `karac check` refuses moving a binding made by a `let . | 78f0e2b7e5 |
| B-2026-11-07-15 | typecheck | low | For `let job = jobs[j];` whose fields are then written (`job.status = 2`), `karac fix` derives `Clone` on the element type and adds `mut`, steering t… | 849eb1f1b9 |
| B-2026-11-07-16 | ownership | high | Option.take / replace / get_or_insert / get_or_insert_with (all `mut ref self`) are accepted through a bare borrowed parameter, a field of one, an in… | 44258c543c |
| B-2026-11-07-17 | typecheck | medium | i128.MIN, i128.MAX and u128.MAX are refused with the internal `a type that failed to check is not lowered yet`; the other widths' MIN/MAX work | a44db5f5ac |
| B-2026-11-07-18 | typecheck | low | f32 copysign / pow / hypot / atan2 refuse a negative float literal argument (`h.copysign(-1.0)`: expects f32, got f64); a positive literal, and min /… | d411135900 |
| B-2026-11-07-19 | mir-llvm+runtime | high | BufReader.read_line / read_to_string and ChildStdout.read_to_string append into a `mut ref String` whose buffer is still a string literal's static da… | a17505d129 |
| B-2026-11-07-20 | effect | low | par branches writing two different Files conflict when both pass the same read-only buffer (`par { f.write(b.as_slice()), g.write(b.as_slice()) }`),… | 39dbc078da |
| B-2026-11-07-21 | typecheck | low | check accepts Atomic[T] for T outside the documented set (i8, u8, i16, u16, i128, usize all pass); Atomic[i128] then fails to link under ASan (undefi… | 3f39b2aafd |
| B-2026-11-07-22 | typecheck | medium | A struct literal of another package's struct is refused even when every field is `pub` (`Opts { port: 1 }` -> E0111 field 'port' is not `pub`); the l… | b92f71fe7d |
| B-2026-11-07-23 | typecheck | low | #[non_exhaustive] is not enforced outside the defining package: a match naming every variant with no wildcard is accepted, for a user package's enum… | 03ecef223d |
| B-2026-11-07-25 | runtime+mir-llvm | high | A compiled TLS server (Server.serve_tls, TlsListener) is killed by SIGPIPE (exit -13) when a client disconnects without close_notify: one client that… | 81bda056fb |
| B-2026-11-07-26 | runtime | medium | For an absolute-form request target (`GET http://example.com/z?k=v HTTP/1.1`) the server's Request.path() returns `http://example.com/z` and for `GET… | 81bda056fb |
| B-2026-11-07-27 | effect | low | Socket methods with no declared effects (local_addr, set_read_timeout, try_clone on TcpListener/TcpStream/UdpSocket) carry an `unknown (native ..)` e… | 39dbc078da |
| B-2026-11-07-28 | runtime+mir-llvm | medium | A compiled HTTP client sends a plain HTTP request for any URL scheme (`ftp://127.0.0.1:P/x` and `gopher://..` get `OK 200` from an HTTP server), whil… | a3e88251f4 |
| B-2026-11-07-29 | interp | low | Through a CONNECT tunnel (`https_proxy` set, https:// URL) the interpreter's client sends the request in absolute form (`GET https://proxied.invalid:… | a3e88251f4 |
| B-2026-11-07-30 | ownership | low | A bare `for a in args` over an owned `Vec` (an `own` parameter not used after the loop) whose element's payload is moved out (`Value.List(items) => l… | ec6e5415ba |
| B-2026-11-07-31 | typecheck | low | An unannotated list literal of a `ref` to a `Copy` value (`let chain = [k];` with `k` from `order.iter()`) is typed `Vec[ref i64]`, and its later use… | ec6e5415ba |
| B-2026-11-07-34 | typecheck | medium | A `collect` into a written collection whose items hold a call's unsolved parameter (`let mut cells: Vec[Vec[String]] = tops.iter().map(\|_\| Vec.new())… | bf2c7db3d5 |
| B-2026-11-07-35 | typecheck | low | A generic function's `Atomic.new(x)` over `x: own T` is not judged at the instance: `wrap(1i8)` with `fn wrap[T](x: own T) -> Atomic[T]` passes check… | efeab13898 |
| B-2026-11-07-36 | effect | medium | Number parsing in two `par` branches or a `par for` is refused as a conflict: `par { i64.parse(a), i64.parse(b) }` reports `unknown (native str.char_… | 670fbcb5ec |
| B-2026-11-07-37 | typecheck+ownership+mir | medium | `karac check` accepts calling an `OnceFn` struct field twice (`(job.run)(); (job.run)();`); every run lane then stops with the internal `move check m… | 3dacb8cfc5 |
| B-2026-11-07-38 | interp+mir-llvm | medium | An early exit in a `par {}` branch does not cancel the branches after it: with `par { quick()?, slow()? }` where `slow` sleeps 2 s, the function retu… | e0b7c1b723 |
| B-2026-11-07-39 | ownership+typecheck | medium | The item of a bare `for x in xs` over a borrowed parameter cannot be returned: `fn last(xs: Slice[String]) -> ref String { let mut best = ref xs[0];… | 3dacb8cfc5 |
| B-2026-11-07-40 | resolver+parser+cli | medium | `karac fix` applies did-you-mean edits to foreign keywords and builtins that produce nonsense: `null` -> `Mul`, `nil` -> `n` (or `Div`), `range(3)` -… | c15666711c |
| B-2026-11-07-41 | parser+typecheck | medium | `std::thread::spawn(\|\| ..)` is told `Kāra's spawn needs no module path: write spawn`, and that fix is then refused as `free spawn is removed (§11.1)`… | 71eb8d8b97 |
| B-2026-11-07-42 | parser | low | A `Self { x: x }` struct literal in a method body is `expected ; after this expression statement` at `Self`, and `karac fix` inserts the `;` (`Self;… | 71eb8d8b97 |
| B-2026-11-07-43 | typecheck | low | Two obvious repairs have no machine fix: a function whose last statement is `a + b;` under `-> i64` (fix: drop the `;`), and `?` in a `fn main()` ret… | 71eb8d8b97 |
| B-2026-11-07-44 | cli | low | `karac fmt` moves comments off the item they annotate: a `// first` after a parameter lands after the fn's `{` (where it reads as a comment on the bo… | 8b1e2b6970 |
| B-2026-11-07-45 | cli | low | kara-lsp go-to-definition returns null on a method call (`u.greet()`), a field access (`u.age`, `self.name`) and a library call (`xs.iter()`), and ho… | 981a577ebb |
| B-2026-11-07-46 | cli | low | kara-lsp diagnostics carry the phase (`ownership`, `typecheck`) as the LSP `code` instead of the diagnostic code (`E0500`), and drop the help line, s… | a9fd3117a5 |
| B-2026-11-07-47 | typecheck | low | `karac explain E0517` prints the TYPE_MISMATCH class text ("Operand, argument, or return-value type doesn't match ...") for the write-through-a-borro… | e2e61a6643 |
| B-2026-11-07-48 | cli | medium | The v0.8.0 Linux release binaries need glibc 2.39 (karac, karac_jit_runner and kara-lsp import pidfd_spawnp/pidfd_getpid@GLIBC_2.39 and __isoc23_strt… | 533b9ea5e9 |
| B-2026-11-07-49 | cli | low | README.md's Install section still says prebuilt binaries "arrive with the 0.8.0 beta | 533b9ea5e9 |
| B-2026-11-07-50 | cli | low | `karac build` with no C compiler on PATH prints only `error[mir]: Failed to invoke linker: No such file or directory (os error 2)`: it names neither… | c1efb60ae8 |
| B-2026-11-07-51 | typecheck | medium | `match (self, other)` in a `ref self` (or `mut ref self`) method is refused as "cannot move a non-`Copy` value out of a borrowed place", though the s… | 255b769106 |
| B-2026-11-07-52 | typecheck | low | `impl Ord for T` with no `impl PartialOrd for T` is refused ("impl Ord for Value requires impl PartialOrd for Value") with no `karac fix` edit, thoug… | 255b769106 |
| B-2026-11-07-53 | resolver | low | `Dict[String, i64]` / `Dict.new()` (Python's `typing.Dict`) is "undefined type 'Dict', did you mean 'Div'?"; `HashMap`, `BTreeMap` and `HashSet` alre… | 255b769106 |
| B-2026-11-07-54 | typecheck | low | `opt == Some("")` with `opt: Option[Str]` is refused ("cannot compare 'Option[Str]' and 'Option[String]'") though `let a: Option[Str] = Some("")` is… | 670fbcb5ec |
| B-2026-11-07-55 | mir-llvm | medium | Katas 100 and 101 (recursive `shared` tree code) ran about 1.5x slower on the M5 at karac 97681763d than at 6697158fa while checked Rust held steady… | b5581f4232 |
| B-2026-11-07-56 | interp+mir-llvm | medium | A method called straight on a constant whose value is a bare literal reads it at i64, not its declared type (corrected: not only wide ones; `CC.leadi… | a3a7d1eeb0 |
| B-2026-11-07-57 | typecheck | medium | A literal's type variable is not fixed by an assignment or a generic call (follow-up of B-2026-11-07-3; corrected: the `b += y` shape is a deliberate… | b85cf41506 |
| B-2026-11-08-1 | mir-llvm | medium | The overflow-safe midpoint `lo + (hi - lo) / 2` keeps its signed division: halving (B-2026-11-06-8) does not see `hi - lo` as non-negative under the… | a8696f186e |
| B-2026-11-08-3 | mir-llvm | low | Kata 277's bench runs 1.83x Rust's instructions at karac's default O2 (3.19G vs 1.74G, x86 callgrind) and 0.62x at O3 (1.08G): O2 misses the verify l… | 03e9186ea3 |
| B-2026-11-08-4 | resolver | low | `karac fix` rewrites Rust's `BinaryHeap<Reverse<i32>>` in a type (a struct field) to `PriorityQueue[Release[i32]]`: the Reverse-around-heap-items fix… | cdb6382000 |
| B-2026-11-08-5 | mir-llvm | low | Kata 481's bench calls karac_mir_vec_reserve on every entry to its inner push loop (66.3M calls): the presize lowering of a counting loop that refill… | bfa904694f |
| B-2026-11-08-6 | mir-llvm | low | Iterating a String's chars costs 17 instructions a char against Rust's 11 when collected, 35 against 27 in a for loop: the inlined decode reloads the… | d026b1bc15 |
| B-2026-11-08-7 | mir-llvm | low | A Map or Set allocates five buffers for its first entry and frees five (the MirMap header, buf, hashes, index, dead): kata 491's bench makes 13.0M ma… | 267075716b |
| B-2026-11-08-8 | mir-llvm | low | A Map lookup calls SipHash out of line, reloading the hash state from a global and checking the seed on every call, and probes its index one 8-byte c… | 42bfb964f0 |
| B-2026-11-08-9 | mir-llvm | low | Dropping a shared-struct handle calls an out-of-line, self-recursive drop function even when the count does not reach zero (154M calls, 21% of kata 5… | ca44c9a984 |
| B-2026-11-08-10 | mir-llvm | low | PriorityQueue's sift loops (runtime/stdlib/priority_queue.kara) take 24-27 instructions a level against Rust BinaryHeap's 9-12: bounds checks on ever… | 833870bb07 |
| B-2026-11-08-11 | parser | low | Rust's `mut arr: Vec<i32>` parameter has no `karac fix` edit even in a program written as Rust, where it always means an owned copy (`arr: own Vec[i3… | 0919199c6f |
| B-2026-11-08-12 | ownership | low | `karac check` refuses `let (at, _) = v[i..j].iter().enumerate().min_by_key(..).unwrap(); v.remove(at);`: it keeps the range slice `v[i..j]` borrowed… | edc6b01e22 |
| B-2026-11-08-13 | typecheck | low | `karac fix` rewrites Rust's `if let Some(&i) = pos[d].last()` to `if let Some(i) = ...`, which changes `i` from a copied usize into a borrow of `pos[… | f4b3b2a3b7 |
| B-2026-11-08-14 | mir-llvm | medium | Read-only iteration of a field of a `shared` struct that is not declared `mut` pays a run-time borrow flag (acquire + release through the thread-loca… | 4fdf0cf1e3 |
| B-2026-11-08-15 | typecheck | low | `s.split_whitespace().rev()` (and `s.split(" ").rev()`) is refused with a bare "no method 'rev' on type 'StrWords'": the string-splitting iterators a… | fd8d318ca2 |
| B-2026-11-08-16 | typecheck+cli | low | A `usize` length from a Rust template (`let n = n as usize; Vec::with_capacity(n * (n + 1) / 2)`) is refused as an implicit `usize` to `i64` coercion… | 20031ded3d |
| B-2026-11-08-17 | typecheck | low | A known unit variant (`None`) in a `match` arm over a scrutinee that already failed to type is also reported as `` `None` is Type-class but binding n… | 8b1e2b6970 |
| B-2026-11-08-20 | typecheck | medium | A binding whose literal keeps `i64` because only an operator or a method receiver used it is refused far from the binding, with an `as` cast as the o… | 7ae9bc82c0 |
| B-2026-11-08-21 | typecheck | medium | A binding's literal is not fixed by a collection literal it is an element of: `let p = 0; let v: Vec[i32] = [p, 1];` and `a.push([p, 1])` on a `Vec[V… | b07678a0f8 |
| B-2026-11-08-22 | typecheck | low | `m.get(k) == Some(3)` (Map[String, i64]) is refused with "cannot compare 'Option[ref i64]' and 'Option[i64]'", as are `it.peek() == Some('a')`, `fl.f… | 36f65c6291 |
| B-2026-11-08-23 | typecheck | medium | `collect()` into an annotated `Result[Vec[i64], String]` does not fix the error type of the mapping closure's `Ok(..)`: `let m: Result[Vec[i64], Stri… | cb995527b5 |
| B-2026-11-08-24 | mir | low | `Option`'s derived order puts `None` after every `Some(x)` (it is declared `Some(T), None`), so `sort()` on a `Vec[Option[T]]` puts `None` last; the… | 9972d813a |
| B-2026-11-08-25 | mir | high | A program that `karac check` passes fails on every run lane with the internal `error[mir]: borrow check patch: .. | 2eed872610 |
| B-2026-11-08-26 | typecheck | low | Declaring `struct Response { status: i64, body: String, headers: Vec[(String, String)] }`, the form io.md § HTTP server documents for sending headers… | a8cbf37e72 |
| B-2026-11-08-27 | interp+mir-llvm | low | `Json.deserialize` errors give a byte offset and no field (`expected '"' at character 13`) where `Json.parse` on the same text gives `expected value… | bdf7c200c3 |
| B-2026-11-08-28 | interp | low | `karac run --interp prog \| head -1` keeps running after the pipe closes and exits 0, printing nothing more but running every remaining iteration (a 2… | 456b9c6e58 |
| B-2026-11-08-29 | typecheck | medium | The unimported, deferred `std.cli` parser's types sit in the prelude, so a program declaring its own `Parser`, `Arg`, `Args`, `CliError` or `Subcomma… | b130382d0c |
| B-2026-11-08-30 | interp+mir-llvm | low | `IoError` and `VarError` display as their variant names (`PermissionDenied`, `NotPresent`, `Other(disk full)`), and `IoError` has no `Debug` (`dbg(Io… | adf37211ee |
| B-2026-11-08-18 | typecheck | medium | (corrected: the `a += n` shape is a deliberate divergence, design.md §8 Literals at 7a4e06ce1c, beside an i32 the literal keeps i64; its refusal now… | 7ae9bc82c0 |
| B-2026-11-08-19 | typecheck+mir-llvm | high | An untyped `Map.new()` whose key type comes from a `for x in v.iter()` element is inferred `Map[ref i64, _]`; `*m.entry(x).or_insert(0) += 1` then ch… | df02f42eac |
| B-2026-11-08-31 | typecheck+interp+mir-llvm | high | A `ref u8` from a `Slice`'s `iter()` passed where an `i64` parameter is expected is accepted as an implicit widening, but the compiled program passes… | b4a08a05b1 |
| B-2026-11-08-32 | typecheck | medium | A blanket impl whose method calls its bound's method (`impl[S: Store] Counter for S { fn peek(self) -> i64 { self.get() + 1 } }`) is refused at check… | 2731d331f0 |
| B-2026-11-08-33 | typecheck | low | `assert_eq(s, "x")` with `s: Str` is refused: "`assert_eq` of two types is not lowered yet"; the literal should take `Str` from the other operand (st… | 2660652075 |
| B-2026-11-08-34 | cli | low | `karac doc` renders wrong signatures and drops members: generic arguments vanish (`-> Result`, field `Vec`, `SortedMap`, `fn add_to(s: mut ref S, ..)… | a165a62532 |
| B-2026-11-08-35 | resolver | low | E0113 for an unknown item in a dependency lists non-importable names as available: `import textkit.nope` says "available: Table, Store, Counter, MemS… | 380985f138 |
| B-2026-11-08-36 | typecheck+interp+mir-llvm | high | A `fold` closure whose parameters are annotated bare (`\|acc: Vec[i64], row: Vec[i64]\|`) checks, but the program fails on both backends: the interpret… | 1740abdd1 |
| B-2026-11-08-37 | mir-llvm+cli | medium | Networking on `wasm_wasi` passes `karac check --targets=wasm_wasi`, then `karac build --target=wasm_wasi` dies in wasm-ld with raw runtime symbols (`… | c33623ba28 |
| B-2026-11-08-38 | typecheck+cli | medium | In a package whose kara.toml declares two or more `[build] targets`, `karac check` runs a different checker: `s.split(" ").count()` and `s.split_whit… | bfe76c98a7 |
| B-2026-11-08-39 | effect | low | The E0411 target-gate message cites a section that does not exist ("design.md § Cross-target Compilation"; the rule is §16 "Effect-driven target gati… | 42045bc039 |
| B-2026-11-08-40 | cli | medium | `karac::run_playground` (the playground, and the book test's only gate for `kara,run` blocks) runs a program that `karac check` and kara-lsp refuse:… | 91d981fdda |
| B-2026-11-08-41 | typecheck+resolver | low | Some diagnostics print two headers on one line: `error[typecheck]: f.kara:2:1: error[E_SECRET_TRAIT_FORBIDDEN]: cannot implement ...` and `error[reso… | 89825abf23 |
| B-2026-11-08-42 | typecheck | medium | A literal inserted into a `Map` or `SortedMap` is not fixed by the parameter the map is handed to: `let mut d = SortedMap.new(); d.insert(1, 2); m(d)… | 695f9ff6f4 |
| B-2026-11-08-43 | interp+mir-llvm | high | A temporary passed to a `mut ref` parameter (`total(mut v.iter().map(..).collect())`) checks clean and is then handed over as if it were a reference:… | 4f7a9a30a8 |
| B-2026-11-08-44 | typecheck | medium | `v.iter().skip(1).fold(0, \|x, a\| x + 1)` returned as `i32` is refused (`implicit coercion from 'i64' to 'i32'`), though `v.iter().fold(0, ..)` and `(… | af14c52dbd |
| B-2026-11-08-45 | typecheck | low | Rust's `scan` closure over a `&mut` state (`\|m, x\| { *m = (*m).max(x); Some(*m) }`) draws six `drop the *` fixes that leave `Some(m)` and a type erro… | 3af0db7bb7 |
| B-2026-11-08-46 | typecheck | low | Rust's `match ans.last_mut() { Some(last) if last[1] == prev && last[2] == avg => last[1] = pos, _ => ans.push(..) }` gets `no method 'last_mut' on t… | 3af0db7bb7 |
| B-2026-11-08-47 | typecheck | low | A sequence has no `split` by a predicate: Rust's `line.split(\|&ch\| ch == '#')` over a `&Vec<char>` is refused "no method 'split' on type 'Vec'" with… | 3af0db7bb7 |
| B-2026-11-08-48 | mir | medium | An `isize` operand beside an `i64` passes `karac check` and the MIR builder then refuses the program: `fn f(i: isize, k: i64) -> i64 { i - k }` is 'i… | af14c52dbd |
| B-2026-11-08-49 | typecheck+cli | low | `karac fix` half-rewrites `usize` to `i64`: the 'usize mixes with i64' fix retypes a nested `fn dfs(u: usize, p: usize, adj: ref Vec[Vec[usize]])`'s… | 0600aebf3 |
| B-2026-11-08-50 | mir+mir-llvm | high | A `ref` number passed to a library native reaches it as its address: `"A".repeat(n)` with `n` a `ref i64` from `.iter().map(\|n\| ..)` stops the interp… | 9671efaf31 |
| B-2026-11-08-51 | parser | medium | Two `format!` calls in one condition (`if format!(..) != format!(..) {`) end the condition at the first call's name: only that call is reported, the… | 9671efaf31 |
| B-2026-11-08-52 | mir-llvm | low | `v.iter().skip(k)` walks the k skipped items one at a time, so a `skip(i * n).take(n)` row walk over a Vec is quadratic where Rust's slice iterator j… | 871134eeb3 |
| B-2026-11-08-53 | typecheck | low | `karac fix` drops a closure's `-> i32` and with it the type of the literal-typed tuple binding it returns, leaving `res.max(x as i32)` ("`max` expect… | bfd2f7b286 |
| B-2026-11-08-54 | parser | medium | Rust's array type inside a generic argument (`Vec[[i64; 2]]`) is taken for a shape literal: check reports "Expected `]`, found `;`" instead of the `[… | 274ec2990d |
| B-2026-11-08-56 | mir | high | A call to a closure puts its arguments' borrows in the closure itself, not in what the closure's `mut ref` captures reach: `\|extra: Vec[i64]\| total +… | b4d55d0ffb |
| B-2026-11-08-55 | typecheck | low | RULING WANTED (Str vs String): `["inc", "dec"].contains(cmd)` with `cmd: Str`, and `names.contains(s)` on a `Vec[String]`, are refused "expected 'ref… | e59006b616 |
| B-2026-11-08-57 | typecheck+cli | low | An unannotated array or `Vec` literal's element type is not fixed by where the binding goes: `let mut cnt = [0; 26]; ..; cnt` returned as `Array[i32,… | 8da3c36f75 |
| B-2026-11-08-58 | typecheck | medium | A closure returning `Some((Vec.new(), x))` to a generic `Fn(S) -> Option[(S, O)]` leaves `O` uninferred, and with the state unannotated reports two u… | fbc3d33e01 |
| B-2026-11-08-59 | resolver | low | An undefined `Deque`, `ArrayDeque`, `deque` or `Queue` gets no suggestion, though `HashMap`, `HashSet`, `BTreeMap` and `BinaryHeap` are each pointed… | e99826bcc0 |
| B-2026-11-08-60 | parser | medium | A `const` declared inside a function body (`const MOD: i64 = 1_000_000_007;`, the usual first line of a LeetCode Rust solution) is a parse error that… | a90821c8d |
| B-2026-11-08-61 | mir-llvm | low | An index into a local Vec, inside a loop bounded by that Vec's length, keeps its bounds check: 459's `for i in d..n { cs[i] . | 774d3877a5 |
| B-2026-11-08-62 | typecheck+cli | medium | A copy of a defaulted literal binding gets no annotation fix: `let mut carry = 0; let mut x = carry; x += d; out.push(x & 1)` with `out: Vec[i32]` is… | a0bb430292 |
| B-2026-11-08-63 | typecheck+mir | low | A constant whose value is computed from another constant is refused as a pattern: `const AA: i64 = 5; const BB: i64 = AA + 1;` then `match 6 { BB => . | 429cb5a08c |
| B-2026-11-08-64 | typecheck | low | A mutating method on a read-only Slice binding (`let mut b = s.bytes(); b.swap(i, j)`, also sort, reverse, fill, sort_by, sort_by_key, split_at_mut)… | 34d42091dd |
| B-2026-11-08-65 | resolver | medium | `karac fix` carries LeetCode's binary-tree template (`use std::rc::Rc; use std::cell::RefCell;`, `Option<Rc<RefCell<TreeNode>>>`, `n.borrow()`) to `i… | a4aa93dce |
| B-2026-11-08-66 | mir | low | `rest = rest[1..]` on a `Slice[T]` local is refused E0516 ("cannot assign to `rest` while .. | 76bc1b9842 |
| B-2026-11-08-69 | typecheck+mir | high | A number literal checked against a closure's return type or a map entry's value keeps its default `i64`: `let g: Fn() -> i32 = \|\| 5` returns an `i64`… | 1de47226fa |
| B-2026-11-08-70 | typecheck | medium | A call to a method returning `Self` in an impl of a `shared struct` is refused against any annotation: `let y: Node = Node.new(2);` and `Some(Node.ne… | a4aa93dce |
| B-2026-11-08-67 | mir | low | Assigning the `ref i32` a `SortedMap` walk yields to an `i64` local, `cur = d`, is refused E0509 "a view .. | bb6dcf8503 |
| B-2026-11-08-71 | mir-llvm | low | Kata 520's `word.chars().filter(\|c\| c.is_ascii_uppercase()).count()` branches on each char's answer and overflow-tests the count, where Rust's `Filte… | b8e953e4f9 |
| B-2026-11-08-72 | mir-llvm | low | Kata 540's binary search divides `(low + high) / 2` and tests `mid % 2 == 1` as signed `i64`s, 3 instructions a step more than Rust's `usize` (8.08G… | 871ac6133e |
| B-2026-11-08-68 | parser | medium | A closure whose whole body is an assignment (Rust's `m.entry(k).and_modify(\|v\| *v += 1)`, `.for_each(\|x\| last = *x)`) stops the parser at the operato… | 5fe8006173 |
| B-2026-11-08-74 | typecheck | medium | A float format spec without a precision is refused (`{x:>8}`, `{x:+}`, `{x:<5}` on an f64: "a float format spec needs a precision"), and any spec on… | 4be4cb4b9c |
| B-2026-11-08-75 | typecheck | medium | A `select` whose value is the function's result (its tail expression) gets false W0278 "discarded `Result` value" warnings (five for a two-arm select… | 2233d104ea |
| B-2026-11-08-76 | typecheck | medium | A user type that implements both `Iterable` and `IntoIterator` cannot be the subject of a bare `for`: "type 'Bag' cannot be iterated by a `for` loop:… | c8a1782220 |
| B-2026-11-08-77 | typecheck | medium | Two impls of one generic trait on a type (`impl Conv[String] for N`, `impl Conv[f64] for N`) leave the method uncallable: `n.conv()` is ambiguous (ri… | 3d45c76221 |
| B-2026-11-08-78 | typecheck+mir | medium | `#[derive(Serialize, Deserialize)]` on a struct with a tuple field (`pair: (i64, bool)`) fails: alone it is refused at check with an internal message… | ef6cd921e9 |
| B-2026-11-08-79 | typecheck | low | `Json.deserialize(text)` takes `text: String`, so a `Str` (a line from `text.lines()`, a slice) is refused ("expected 'ref String', found 'Str'") and… | ef6cd921e9 |
| B-2026-11-08-80 | mir | low | A missing field in `Json.deserialize` is reported by its Kāra name, not its JSON key: a `#[serde(rename: "userName")] name: String` field absent from… | ef6cd921e9 |
| B-2026-11-08-81 | runtime | medium | `s.chars().last()` walks the whole string: `StrChars` and `StrViewChars` take `Iterator.last`'s default rather than reading from the back, so kata 20… | 89d9feafb3 |
| B-2026-11-08-82 | typecheck | medium | Map, SortedMap, Set and SortedSet do not implement PartialEq/Eq, and the refusal suggests `#[derive(PartialEq)]` on a std type | 68aa472f3f |
| B-2026-11-08-83 | mir+mir-llvm | medium | A borrow of a `shared` field that stays live across a recursive call makes recursion quadratic in depth: the run-time flag table is a linear list, an… | d308599fbb |
| B-2026-11-08-84 | mir-llvm | high | Index assignment into a `Vec[dyn Trait]` (`xs[i] = v`) leaves a freed block in the slot on the LLVM backend: heap-use-after-free on the next call thr… | d308599fbb |
| B-2026-11-08-85 | mir-llvm+runtime | medium | `sort()` on a Vec of tuples runs about 3.3x Rust's stable `sort`: anything but a plain integer goes to the runtime's byte-wise `merge_sort` with the… | d308599fbb |
| B-2026-11-08-86 | typecheck | medium | A range of bare literals takes `i64` where it is written, so `(1..=5).collect()` (or `.filter(..)`, `.map(..)` first) into a `Vec[i32]` return or bin… | 1a8ab6b981 |
| B-2026-11-08-87 | mir-llvm | low | PriorityQueue's sift-to-bottom loop still takes 18 instructions a level against Rust BinaryHeap's 9-12 after B-2026-11-08-10: a swap (2 loads, 2 stor… | f6dfaaa342 |
| B-2026-11-08-88 | typecheck+cli | low | A plain list literal's element literal is not fixed by where the binding goes (the remainder of B-2026-11-08-57): `let mut v = [0, 0]; g(mut v)` with… | 1a8ab6b981 |
| B-2026-11-08-89 | typecheck | high | A `Vec[(i64, i64)]` is accepted for a `Vec[(i64, u64)]` parameter (a tuple inside a generic argument is compared by the value rule, not the layout ru… | ee2d9fee79 |
| B-2026-11-09-1 | mir-llvm | low | A tuple comparison (`a >= b` on `(i64, i64, i64)`) lowers through `cmp`'s Ordering: each element takes setg/inc/cmp/cmov (about 7 instructions) where… | 6d16b370c8 |
| B-2026-11-09-2 | mir-llvm | low | A by-value function's `mut ref Vec[i64]` parameter gets no `noalias` (mark_noalias skips by-value signatures), so every store into an element reloads… | d308599fbb |
| B-2026-11-09-3 | mir | high | Inside a generic body, `T.default()` (any `T.f(..)` path call) with `T` a type parameter calls the program's own `struct T`'s `f` at every instance w… | ef6cd921e9 |
| B-2026-11-09-4 | typecheck | medium | `T.from(x)` under a `T: From[X]` bound is refused with an internal message ("a type that failed to check is not lowered yet" at the callee `T.from`),… | 6d16b370c8 |
| B-2026-11-09-5 | mir-llvm | low | A literal table of constants iterated in a hot loop (`for step in [1, 9]`) is slower promoted to a static (src/mir/literal_tables.rs) than built in p… | 64f7f708eb |
| B-2026-11-09-6 | mir | low | A counting loop that starts at a local, `for i in low..low + k`, is not presized: `mir::presize` takes only a constant start, so kata 658's window co… | d308599fbb |
| B-2026-11-09-7 | mir-llvm | low | `i64.parse` of a negative number calls the runtime: the in-line decimal parse takes only digits, so kata 682's `-` scores each paid a runtime call (1… | d308599fbb |
| B-2026-11-09-8 | mir-llvm | low | A by-value function's `ref Vec` parameter gets no `noalias`, so every store into another vector reloads its header and re-tests its bounds: kata 718'… | d308599fbb |
| B-2026-11-09-9 | mir-llvm | low | `String.push_str` is a runtime call and a `memcpy` call per append: kata 763's strings built a letter at a time run 1.84x checked Rust on x86 | d308599fbb |
| B-2026-11-09-10 | mir | low | A vector made before its loop's bound is worked out (`let mut seat_of = Vec.new(); for _ in 0..row.len() { seat_of.push(0) }`) is not presized, since… | d308599fbb |
| B-2026-11-09-11 | mir-llvm | low | A library body's located function (`<fn>.at`, every library body a program calls) gets none of the parameter attributes the plain function gets: no `… | d308599fbb |
| B-2026-11-09-13 | parser+resolver | high | `karac fix` turns a plain Rust `BinaryHeap` into a smallest-first `PriorityQueue.new()` whenever any other heap in the program wraps its items in `Re… | 4899210bd7 |
| B-2026-11-09-14 | typecheck+cli | medium | Rust's `binary_search` taken apart as a `Result` has no fix: `xs.binary_search(&v).unwrap_or_else(\|x\| x)` is refused with "expected 'OnceFn() -> i64'… | 182b518950 |
| B-2026-11-09-15 | typecheck+cli | low | A defaulted `i64` literal that reaches a narrower slot through a collection (`let mut a = 0; a += nums[i]; left[k].push(a)`, then `best.min(x + ..)`… | 6464505444 |
| B-2026-11-09-16 | ownership | medium | A move inside a `let … else` else block is reported E0500 "value 'x' moved here, used again here (moved at line L)" against a move of the same bindin… | 6db1634956 |
| B-2026-11-09-17 | typecheck | low | `.slice(a, b)` on a `Vec`, an array or a slice (JavaScript's spelling of a run of elements) gets a bare "no method 'slice' on type 'Vec'", with no po… | e9cca2f951 |
| B-2026-11-09-18 | typecheck | low | `c as u8` on a `char` (LeetCode's Rust template for 1079, `count[(c as u8 - b'A') as usize]`) is refused with a help naming `c as u32 as u8` but no m… | 10ef8e8c7a |
| B-2026-11-09-19 | mir | medium | `v.contains(d)` with a float needle on a `Vec[u8]` (`let d = 1.5`) passes `karac check`; the interpreter then stops with "Eq on Int(1) and Float(1.5)… | 2e41a1c552 |
| B-2026-11-09-20 | mir | low | A raw pointer cannot reach a non-`Copy` element: `ptr.mut(xs[i])` is refused as a move out of the collection, `p.read()` as a move out of a borrowed… | f6dfaaa342 |
| B-2026-11-09-24 | typecheck | low | A `pub fn` in an inherent impl of a type that is not `pub` (LeetCode's `struct Solution; impl Solution { pub fn sufficient_subset(root: Option<Rc<Ref… | 52b677beea |
| B-2026-11-09-25 | typecheck | low | A `char` handed to `replace` or `replacen` (Rust's `rest.replace(c, "")`) is refused with "expects String arguments" and no machine fix, though `find… | 5eee63ee71 |
| B-2026-11-09-26 | mir-llvm | low | A presized push fill whose trip count is not a constant keeps two exits a trip, its bound and its reservation's reach, so LLVM does not vectorize it:… | 1cbfcd7e92 |
| B-2026-11-09-27 | ownership | low | `let Json.Array(queries) = row[3] else { … };` with `queries` only read is refused with E0515 "cannot move a non-`Copy` value out of a collection ele… | 4e69fdaed4 |
| B-2026-11-09-29 | typecheck | low | A program's inherent impl on a library type that redefines a library method is also refused at the library's own impl, past the program's end, under… | 8adc0b0e29 |
| B-2026-11-09-30 | typecheck | medium | JsonError, PoolError, SendError, RecvTimeoutError and SendTimeoutError do not implement `Error`, so `?` cannot convert them into `AnyError` | 8adc0b0e29 |
| B-2026-11-09-31 | typecheck | medium | A program's `impl Clone for String` is accepted and replaces the library's `clone`: an impl overlapping a baked library trait impl is not refused | 0352915c4b |
| B-2026-11-09-32 | mir | medium | `Clone.clone(s)` on a `String` passes `karac check`, then `__mir-run` refuses it: "a type that failed to check is not lowered yet" | 0352915c4b |
| B-2026-11-09-33 | cli | medium | kara-lsp: incremental user-code checking -- a completion re-checks the whole buffer and the unpruned library on every request: 186 ms on a 5-line fil… | de8310019f |
| B-2026-11-09-34 | mir-llvm | high | A program's own `struct Response { status, body, headers }` (io.md § HTTP server) is also what `Client.get`/`send` return, and the compiled client wr… | 76b6fbd33a |
| B-2026-11-09-35 | typecheck+mir+mir-llvm | high | A scalar `ref` handed to a type parameter a closure returns solved it as the `ref`: `m.map_or(x, \|k: i64\| k + 1)` with `x: ref i64` made `U` a `ref i… | 5a9b370550 |
| B-2026-11-09-36 | typecheck | low | `v.iter().fold(None, \|acc: Option[i64], x\| Some(x))` is refused: the tail `Some(x)` with `x: ref i64` is an `Option[ref i64]`, though `Some(x)` again… | 0a401ccd53 |
| B-2026-11-09-37 | typecheck+cli+other | low | No iterator or slice has `chunk_by`, yet `v.chunk_by(\|a, b\| a == b)` on a `Vec` is refused as an iterator adaptor with the machine fix `v.iter().chun… | 7842702532 |
| B-2026-11-09-38 | parser+typecheck+cli | medium | A tuple value cannot be assigned to existing places: `(lo, hi) = if c { (lo, mid) } else { (mid + 1, hi) };` (Rust's destructuring assignment) is ref… | b415f2e55f |
| B-2026-11-09-39 | typecheck+mir+mir-llvm | medium | A closure whose body fixes nothing about its parameter, handed to a bounded generic `F: Fn(i64) -> i64`, passes `karac check` but types its parameter… | 3545dc7fee |
| B-2026-11-09-40 | typecheck+mir | medium | A closure whose body ends in `collect()` takes its target from nowhere: `let to32 = \|v: ref Vec[i64]\| v.iter().map(\|x\| x as i32).collect();` used as… | 86c3102afc |
| B-2026-11-09-41 | typecheck+mir | medium | `PriorityQueue` has no `iter`, `into_iter`, `for`, `Clone` or `Display`, though collections.md says every collection works with `for` and is `Display… | 2f62c55005 |
| B-2026-11-09-42 | parser+resolver | low | Rust's `heap.into_iter().map(\|Reverse(s)\| s)` gets no fix for the `Reverse` parameter, and `or_insert_with(BinaryHeap::new)` is not seen as a heap's… | 2f62c55005 |
| B-2026-11-09-43 | typecheck | medium | `let t = text?;` on a bare (borrowed) `text: Option[String]` parameter is refused as "'?' operator requires `Result` or `Option`, found 'ref Option[S… | b490bc1d36 |
| B-2026-11-09-44 | interp+mir-llvm+runtime | medium | The HTTP client turns a status of 400 or more into `Err(HttpError)` carrying only a message ("<url>: status code 409"): a program cannot read the sta… | 9ca01280c9 |
| B-2026-11-09-45 | typecheck+mir | medium | A `mut ref` cursor cannot walk to a list's tail: Rust's `let mut tail = &mut list; while tail.is_some() { tail = &mut tail.as_mut().unwrap().next; }… | e82c519a66 |
| B-2026-11-09-46 | interp | medium | `karac __mir-run` builds an owned (non-`shared`) recursive list in quadratic time: `head = Some(N { v: i, next: head })` n times takes 3.2 s at n = 5… | 8f5b9afb81 |
| B-2026-11-09-47 | mir-llvm | low | `s.chars().collect()` into a Vec[char] runs 14 instructions a char against Rust's 11: the UTF-8 decode loop tests the reserved length on each push, a… | 0d3ccaa7cd |
| B-2026-11-09-48 | typecheck | medium | `Json` has no `.clone()`: `Some(v.clone())` on a field of a parsed request body is refused as "no method 'clone' on type 'Json': `Json` does not deri… | b8b6871a6c |
| B-2026-11-09-49 | typecheck | medium | `["a", "b"].to_vec()` passed to a `Vec[Str]` parameter, or `[("event", v)].to_vec()` to a `Vec[(Str, Json)]` one, is refused as "expected 'Vec[Str]',… | b8b6871a6c |
| B-2026-11-09-50 | typecheck+mir | low | `let v: Vec[Str] = ["a", "b"].to_vec();` is refused as a reference to a temporary that outlives its statement (typecheck §5.5, and E0516 in MIR borro… | a9fc6d3b59 |
| B-2026-11-09-52 | cli | low | `karac fmt` writes the 30 soft words as raw identifiers: `let group = 1` comes out as `let r#group = 1`, and so does every binding, field, parameter… | 7732ad152a |
| B-2026-11-09-53 | parser+typecheck | medium | Six Rust-isms were diagnosed with no fix `karac fix` applies: `pub(crate)` (and `pub(super)`, `pub(self)`), an unreachable `_ =>` arm, `?` in a unit… | 01d64b96db |
| B-2026-11-09-54 | typecheck+mir | medium | Eleven library error types did not implement `Display` and `Error` though design.md §10 says the library's error types do (Utf8Error, NulError, NetEr… | 01d64b96db |
| B-2026-11-09-55 | ownership | low | No lint reports a `let mut` binding that is never written: Rust's habit of `let mut` on a binding that is only read draws nothing (the Rust-ism audit… | df878ae40f |
| B-2026-11-09-56 | mir | medium | `let v = it.collect();` whose `Vec` target a later use decides passes the type checker and is refused by MIR: `fn count(v: Vec[char]) -> i64` and `le… | 86c3102afc |
| B-2026-11-09-57 | typecheck | low | Strings have no `match_indices` (nor `char_indices`): kata 2052's text arm wrote Rust's `sentence.match_indices(" ").map(\|(at, _)\| at + 1)` to find e… | 758eca16fe |
| B-2026-11-09-67 | typecheck | medium | `karac fix`'s `fn main() -> Result[(), AnyError]` for a `?` in `main` left each bare `return;` of `main` as it was, so the fixed program failed to ch… | f42a83543c |
| B-2026-11-09-68 | parser | medium | A format spec in Python's spelling, `{x:.1f}` or `{name:>8s}`, is refused as "unsupported type `f` (expected one of x, X, o, b, d)" with no fix, thou… | f42a83543c |
| B-2026-11-09-69 | interp | medium | An HTTP request past its `timeout` fails in `karac run` with ureq's "Network Error: Network Error: Error encountered in the status line: timed out re… | f42a83543c |
| B-2026-11-09-70 | typecheck+mir | medium | `File` has no `write_all`: `f.write_all(text.bytes())?` is "no method 'write_all' on type 'File'", though `TcpStream`, `TlsStream`, `BufWriter` and `… | f42a83543c |
| B-2026-11-09-71 | mir+mir-llvm | high | Under a `FileSystem` override, a compiled `File.open`, `File.create` or `OpenOptions.open` of a `String` path built at run time hands the fake's `ope… | f42a83543c |
| B-2026-11-09-72 | typecheck | low | There is no `drop(x)`: ending an owned value early (closing a `BufWriter[File]` before reopening its file) is "undefined name 'drop'" with no hint, a… | a9fc6d3b59 |
| B-2026-11-09-73 | interp+mir-llvm+runtime | low | `set_read_timeout` on a socket a `Network` fake opened returns `Err(Unsupported)`, so a TCP client that sets a timeout, as io.md tells a client to, c… | a9fc6d3b59 |
| B-2026-11-09-74 | typecheck | high | A chain of two literal lists is refused: `v.iter().chain(w.iter())` over `[1, 2]` and `[10, 20, 30]` reports `J.Item = ref i64` is not satisfied by `… | 7c40eb7a02 |
| B-2026-11-09-75 | typecheck+mir-llvm | high | `chain` accepts items of another numeric width or kind: `Vec[f64]` chained with `Vec[i64]` builds and prints `[1.5, 5e-324]`; `Vec[i32]` with `Vec[i6… | 7c40eb7a02 |
| B-2026-11-09-76 | mir | medium | A program declaring its own `struct F64` or `struct F32` fails to build: `typecheck: 3694:5: no associated function 'from' on type 'F64'` from the li… | 7c40eb7a02 |
| B-2026-11-09-77 | interp | low | A hand-rolled `BoundedChannel { handle_id: 0 }` stops the interpreter (`BoundedChannel[i64].send of a channel never made`) instead of failing closed… | 7c40eb7a02 |
| B-2026-11-09-78 | mir-llvm | low | A hand-rolled `BoundedChannel { handle_id: 0 }` segfaults the built binary (exit 139; `__mir-llvm-run` exits 101 with no output) where bounded_channe… | a2b8b07dad |
| B-2026-11-09-81 | cli | medium | A `let` after a `#[derive]`d type had no hover type and no inlay hint: derived items were spliced in numbered from 0, so lowering's renumbering moved… | b6179e77bc |
| B-2026-11-09-82 | mir-llvm | high | Under `KARAC_AUTO_PAR=0` a write through a `mut Slice` view of a `Vec` is lost: `poke(mut w[2..4])` with `xs[0] = 99` leaves `w` as `1 2 3 4` in a pr… | ba29c00d5c |
| B-2026-11-09-91 | parser | low | Every bad format spec of an f-string is reported at the f-string's closing quote: `println(f"x {a:?} y {b:?}")` gives two "format spec `?`: `?` is De… | e922bfc642 |
| B-2026-11-09-92 | parser | medium | A field named by a structural marker (`struct Directives { private: bool }`, a Cache-Control parser's natural field) says "Expected identifier, found… | e922bfc642 |
| B-2026-11-09-93 | typecheck | high | A program's own `struct Entry` loses its own methods: `impl Entry { fn age(self, ..) }` then `e.age(now)` is "no method 'age' on type 'Entry'", becau… | e922bfc642 |
| B-2026-11-09-94 | typecheck | low | `opt ?? "" == value` (`??` binds loosest, so it is `opt ?? ("" == value)`) gives three type mismatches (`expected 'String', found 'bool'`, the closur… | e922bfc642 |
| B-2026-11-09-95 | typecheck | low | A `ref Str` (an element of `pairs.iter()` over a `Slice[(Str, Str)]`) handed to a `String` parameter gets "expected 'String', found 'ref Str'" with n… | e922bfc642 |
| B-2026-11-09-96 | typecheck | low | A tuple of borrowed elements moved where owned ones are wanted (`for (k, v) in headers { out.push((k, v)); }` with a bare, so borrowing, `headers: Ve… | 18c7e2b429 |
| B-2026-11-09-97 | typecheck | low | `req.header("X-Purge-Token") != Some(PURGE_TOKEN)` (an `Option[String]` against an `Option[Str]` of a constant) is refused with "cannot compare 'Opti… | 18c7e2b429 |
| B-2026-11-09-98 | mir | medium | A `Str` view of a parameter passed through a closure combinator is refused as returning a borrow of a temporary: `fn unquote(s: Str) -> Str { s.strip… | 1a5b6091e7 |
| B-2026-11-09-99 | ownership | medium | A `let` inside a block outlived it for the `let mut` rule: `let mut q = 7; if c { let q = 3; } q = 8;` was refused as a write to the inner, immutable… | df878ae40f |
| B-2026-11-09-100 | typecheck+ownership | medium | A method's `mut ref` argument needs no `mut` marker, so an immutable binding is written through it unreported: `let s = String.new(); r.fill(s);` wit… | e705e5be7e |
| B-2026-11-09-101 | ownership | medium | The `let mut` rule misses receiver writes its hand-written method table lacks: `let w: VecDeque[i64] = VecDeque.new(); w.push_back(1);` checks clean | e705e5be7e |
| B-2026-11-10-1 | mir-llvm | medium | A plain `karac build` whose auto-par verdicts parallelize nothing shipped the recording lowering, which puts a block between every statement of a blo… | ba29c00d5c |
| B-2026-11-10-2 | mir-llvm | high | `v.as_slice_mut()` and a range view passed `mut v[a..b]`, on a `Vec` of literals, lowered on a shared borrow, so the literal-table pass made the `Vec… | ba29c00d5c |
| B-2026-11-10-3 | typecheck | low | A `flat_map` closure whose body is an `if`/`else` or a `match` giving a list from each branch (`\|x\| if x == 0 { [0, 0] } else { [x] }`, as Rust's `fl… | ba29c00d5c |
| B-2026-11-10-4 | mir | medium | A range view passed `mut v[a..b]` is lowered on a shared borrow of `v` (`_r = &v; Vec[T].slice(move _r, a, b)`), so MIR borrowck does not see it writ… | 8819ec4dff |
| B-2026-11-10-5 | mir-llvm | medium | `karac run` of apps/cli/mini_tldextract (21.8k lines) takes 92 s on one core, where `karac build` takes 16 s wall (42 s CPU over four units) and the… | 5f76094346 |
| B-2026-11-10-6 | typecheck | medium | A block, `if` or `match` whose every value is an unsuffixed literal is typed `i64` at once, not as the literal variable design.md §8 says later uses… | 547252a43f |
| B-2026-11-10-7 | mir | medium | A user iterator over a `Str` field whose `next` spells its return `Option[Str]` (or `Option[(i64, Str)]`) instead of `Option[Self.Item]` passes `kara… | 7087121aa9 |
| B-2026-11-10-8 | typecheck | medium | An unannotated closure parameter used with an operator is refused at the operator (`let f = \|x, n\| x + n; f(1, 2)` -> "type '?T0' does not implement… | cb5f6da53e |
| B-2026-11-10-14 | typecheck+mir-llvm | high | `BufReader.new(stream)` over a `TcpStream` (`BufReader.new(conn.try_clone().unwrap())`, then `.lines()`) passes check, though a `BufReader` wraps onl… | e1523f1438 |
| B-2026-11-10-15 | interp | medium | The interpreter's readiness peek for a waiting socket read swallowed a pending socket error: a reset TCP connection read as an end of stream (`Ok(0)`… | 1ca1f71e30 |
| B-2026-11-10-16 | resolver | low | A call through a path that starts at `std` (`std.mem.take(mut v)`, design.md's own example `std.fs.read_to_string(path)`) fails "undefined name 'std'… | 47514a3c77 |
| B-2026-11-10-17 | parser | low | `format!` whose argument is an `if`, `match` or block expression (`format!("{}={}", n, if up { "up" } else { "down" })`) gets E0001 with no machine f… | 3be914cd0e |
| B-2026-11-10-18 | mir | low | An immediately invoked closure whose body ends in `collect()` (`let s: Set[i64] = (\|v: ref Vec[i64]\| v.iter().map(\|x\| x + 1).collect())(a);`) reports… | 3be914cd0e |
| B-2026-11-10-19 | ownership | low | `unused_mut` misses a `let mut` binding of a `mut ref` handle that is only passed as `mut child`: the call-site marker counts as a write to the bindi… | 3be914cd0e |
| B-2026-11-10-20 | typecheck | medium | `s.extend(v.iter())` on a `String`, with `v` a `Vec[char]` (an iterator lending `ref char`), is refused as ambiguous between `Extend[char]` and `Exte… | e609854dde |
| B-2026-11-10-21 | resolver | low | LeetCode's Rust template `impl Solution { pub fn f(..) }` with no `struct Solution;` (the site supplies it) gets only "undefined type 'Solution'", an… | e609854dde |
| B-2026-11-10-22 | ownership | medium | An immutable `let` of a `shared` value can be lent as `mut` and rebound by the callee: `let tail = T { v: 1 }; advance(mut tail);` with `fn advance(t… | e705e5be7e |
| B-2026-11-10-24 | mir | high | `while let Some(x) = it.find(\|x\| ..)` (or `next_if`) over `v.iter()` is refused E0516: the closure passed by reference is taken to be stored into the… | 1b5fe88816 |
| B-2026-11-10-25 | mir-llvm | low | The char-pattern fix-it's `c.to_string()` allocates a String on every call: kata 2062's `"aeiou".contains(c.to_string())` vowel test makes its compil… | 380b5381f1 |
| B-2026-11-10-23 | typecheck | medium | A closure with no declared return type that returns a non-`i64` integer (`return i as i32;`) and has an unsuffixed literal tail or `return` (`0`) is… | 900693f6bb |
| B-2026-11-10-28 | interp+mir-llvm | medium | `BufReader`/`BufWriter` over a `TlsStream` is refused at check (E0297, "not built yet"), and over a socket a `Network` provider opened stops at the c… | 23ce3eab43 |
| B-2026-11-10-29 | mir-llvm | low | `karac build` was ~21% slower than at f1416771a4 on the ADR-2 frozen set (0.886x legacy, was 0.734x): the in-order program was lowered after the auto… | 2991314acf |
| B-2026-11-10-37 | mir-llvm | low | LLVM 22 keeps hsearch's (275) bounds check on `citations[mid]` in a binary search: instcombine rewrites karac's `assume(mid >= lo)` into `assume(hi -… | 5a072c1314 |
| B-2026-11-10-38 | mir-llvm | low | A program that names no library method still kept and checked Iterator's min/max/sum/count/last/find/collect/cloned/take/enumerate and ExactSizeItera… | 5244494204 |
| B-2026-11-10-26 | mir | low | Rust's `Iterator::eq` is missing: `a.iter().eq(b.iter().rev())` is refused with "`VecIter` does not implement `PartialEq`", and adding `eq` to the li… | 36c8731fe3 |
| B-2026-11-10-39 | mir-llvm | medium | `SortedMap.insert` of a key smaller than every key present is O(n): the map is one sorted array, so 100000 inserts of falling keys take 1.8 s compile… | c5303d91b3 |
| B-2026-11-10-40 | typecheck | low | A tuple of literals was typed `(i64, i64)` where it was written, not left open as design.md §8 leaves a bare literal -- filed as a diagnostics gap (t… | d70ec4a181 |
| B-2026-11-10-41 | resolver | low | Rust's `Reverse` on one part of a heap's tuple item has no Kāra spelling and no fix: `BinaryHeap<(Reverse<i32>, String)>` and `BinaryHeap<(i32, Rever… | 3d987fca00 |
| B-2026-11-10-33 | typecheck | medium | A bare handle parameter handed to `BufReader.new` / `BufWriter.new` (`fn serve(conn: TcpStream) { BufReader.new(conn) }`, a Rust port's owned paramet… | 0e5453a321 |
| B-2026-11-10-34 | typecheck | low | `let s: String = pieces.map(\|x\| f"[{x}]").collect()`, Rust's `FromIterator<String> for String`, is refused with only "`String` implements `FromIterat… | 0e5453a321 |
| B-2026-11-10-44 | typecheck+interp+mir-llvm | medium | `read_exact` on `TcpStream`, `TlsStream`, `File` and the child pipes was missing; Rust's `conn.read_exact(&mut hdr)?` failed "no method 'read_exact'… | 8279013053 |
| B-2026-11-10-45 | typecheck | medium | `port.to_be_bytes()` / `u16.from_be_bytes(b)` (and the `le` forms) are refused, "did you mean 'to_ne_bytes'" (a different byte order, reported twice)… | 6fe383ed8a |
| B-2026-11-10-46 | typecheck | low | `Result.unwrap_or_default()` does not exist (`String.from_utf8(v).unwrap_or_default()` fails "no method 'unwrap_or_default' on type 'Result'"), thoug… | fff48b5d17 |
| B-2026-11-10-47 | typecheck | low | Rust's `let local = [127u8, 0, 0, 1];` (an array in Rust, a `Vec` by default in Kāra) passed to an `Array[u8, 4]` parameter gets "expected 'Array[u8,… | 662c8ece18 |
| B-2026-11-10-48 | typecheck | low | An unknown method in a `let` initializer (`text.trim_end_matches('\n').split("\n").collect()`) is followed by a spurious "a reference to a temporary… | e30304b318 |
| B-2026-11-10-49 | typecheck | medium | A `let` bound to an `if`, `match` or block whose every value is an unsuffixed literal is typed `i64` at once, not as the literal variable design.md §… | a6491451e1 |
| B-2026-11-10-30 | typecheck | medium | A binary search over an `i32` interface written as Rust writes it, `let mut lo = 0; let mut hi = n - 1;` (n: i32), is refused at every `mid` use with… | 6a4c75838e |
| B-2026-11-10-31 | typecheck | low | Rust's open range on a `SortedSet`/`SortedMap`, `seen.range(..k - x).next_back()`, is refused with advice naming the end to supply but `karac fix` ha… | e6bd0300b4 |
| B-2026-11-10-32 | typecheck | low | A Rust `scan` closure with a `return None;` guard, `(1..).scan(candies, \|left, gift\| { if *left <= 0 { return None; } let given = gift.min(*left); *l… | a54501be77 |
| B-2026-11-10-50 | parser | low | `format!("{}{}{}", &s[..i], if v { "t" } else { "f" }, &s[j..])` gets advice alone: the f-string rewrite refuses any argument containing `f"`, which… | d7b33992ad |
| B-2026-11-10-51 | typecheck | low | `let top = frames.last_mut().unwrap();` followed by `top.seen_true = true;` gets advice alone: the `last_mut` rewrite (B-2026-11-08-46) covers only `… | 79dc9537fd |
| B-2026-11-10-35 | parser+typecheck | low | Rust's `use std::iter::Peekable; use std::str::Chars;` and `fn read(chars: &mut Peekable<Chars>)` reach `std.iter has no item Peekable` and `unknown… | 7d95ec9646 |
| B-2026-11-10-42 | parser | low | `address.split('.').collect::<Vec<&str>>().join("[.]")` gets advice alone (bind a typed `let` first), though `Vec[Str].from_iter(address.split(".")).… | 33ef2a7ad2 |
| B-2026-11-10-43 | mir | low | `String.from_iter(s.chars().rev())` passes the type checker and stops in the MIR builder: "a call with no resolved callee is not lowered yet" | 33ef2a7ad2 |
| B-2026-11-11-1 | mir-llvm | medium | The sync HTTP client shared one current-thread tokio runtime across threads: a program calling it from more than one thread raced on tokio's IO drive… | b92145453b |
| B-2026-11-11-2 | typecheck | low | B-2026-11-09-97's `karac fix` allocates: `opt != Some(TOKEN)` with `opt: Option[String]` and `TOKEN: Str` became `opt != Some(TOKEN.to_string())`; th… | 67ebc5faa6 |
| B-2026-11-11-3 | typecheck | low | `Option[Vec[T]].as_deref() -> Option[Slice[T]]` (library/core-types.md) is missing: 0.8 refuses a second specialized inherent impl that defines `as_d… | 27a5b91f4b |
| B-2026-11-11-21 | parser | low | `format!("'{}'", String::from_utf8_lossy(&payload))` (and `println!`) gets no f-string fix when an argument holds a `::` path: the refusal names the… | 2776b10314 |
| B-2026-11-11-23 | typecheck | low | `u16.from_be_bytes([a, b])` refuses its list-literal argument as a `Vec[u8]` ("expects an `Array[u8, 2]`, got `Vec[u8]`"), where Rust's `u16::from_be… | 6008a2790e |
| B-2026-11-11-24 | typecheck | medium | `String.from_utf8_lossy(bytes)` does not exist ("no associated function 'from_utf8_lossy' on type 'String'"): text from a socket or a file with a bad… | 202af9c0be |
| B-2026-11-11-25 | typecheck | low | `?` on a `TcpStream` read inside `fn read_packet(..) -> Result[Option[Packet], IoError]` is refused "cannot convert error 'NetError' to 'IoError': no… | 8279013053 |
| B-2026-11-11-27 | typecheck | medium | Rust's `.scan(0, \|depth, c\| { let g = if c == '(' { *depth += 1; . | 3402c1954a |
| B-2026-11-11-28 | typecheck | low | `.scan(0, ..)` collected into a `Vec[i32]` has no fix: the state's literal `0` takes the default `i64`, its items are computed from it, and the colle… | 3402c1954a |
| B-2026-11-11-29 | typecheck | medium | `match v.last() { Some(g) => 1 - g, None => 0 }` over a `Vec[i32]` is refused: the `None` arm's literal `0` takes `i64` against the `Some` arm's `ref… | 3402c1954a |
| B-2026-11-11-30 | typecheck | low | Rust's `Duration::from_millis(n)` / `from_secs(n)` get `no associated function` and no fix, where Kāra's are `Duration.ms(n)` / `Duration.secs(n)`; `… | 3402c1954a |
| B-2026-11-11-31 | typecheck+parser | medium | Rust's `use std::sync::mpsc::{channel, Receiver, Sender};` with `Mutex<Sender<()>>` and `self.tx.lock().unwrap().send(())` leaves an import of a modu… | 3402c1954a |
| B-2026-11-11-32 | interp+mir-llvm+runtime | medium | `sleep(Duration.ms(0))` never lets another task run: a task that waits on a flag with a zero sleep between reads spins forever on the MIR interpreter… | 1267e2aa72 |
| B-2026-11-11-33 | runtime | low | `Duration` has no unit below the millisecond: `Duration.ms(n)` and `Duration.secs(n)` build it and `as_ms()` reads it, so a pause or an elapsed time… | 86a48f366b |
| B-2026-11-11-34 | interp+mir-llvm+runtime | medium | The prelude `Semaphore` does not wait under tasks: `acquire(timeout)` on an exhausted semaphore fails at once with `Timeout` though another task will… | c47fd80f23 |
| B-2026-11-11-35 | typecheck | low | LeetCode's `class H2O` as `struct H2O` is refused as a Const-class name with `consider renaming to H2o` and no fix, though the rename is mechanical:… | cc0bc927f2 |
| B-2026-11-11-36 | typecheck | low | `String.from_iter(atoms.iter())` over a `Vec[char]` says `.cloned()` but offers no fix: the assoc-eq bound's fix reached only a method whose bound na… | 3402c1954a |
| B-2026-11-11-37 | typecheck | medium | `g.value = Molecule { . | 3402c1954a |
| B-2026-11-11-38 | typecheck | medium | An `if` or `match` of unsuffixed literals as an element of a list or tuple literal is typed `i64` at once: `let v: Vec[i32] = [1, if b { 2 } else { 3… | 3b45dc0f0e |
| B-2026-11-11-39 | runtime | medium | `String` has no `retain`: `s.retain(\|c\| c != ' ')`, Rust's `String::retain`, is refused as `no method 'retain' on type 'String'` though `Vec`, `VecDe… | 3c79788215 |
| B-2026-11-11-40 | parser | low | Rust's `let n = n.borrow();` in an `Rc<RefCell<TreeNode>>` walk becomes `let n = n;` when the `RefCell` goes: a binding that only renames, left in th… | c80adee665 |
| B-2026-11-11-41 | typecheck | medium | A `chain` of literal lists collected into a `Vec[i32]` is refused with no fix: `let v: Vec[i32] = [0].into_iter().chain([1, 2].into_iter()).collect()… | a65b4dc882 |
| B-2026-11-11-42 | runtime | low | On Linux the runtime's per-thread small-chunk lists start only with the auto-par worker pool, and small_realloc bypasses them, so a sequential progra… | ba5b881217 |
| B-2026-11-11-43 | typecheck+parser | low | The `lossless_cast` fix (W0291) deletes only ` as i64` from a cast written in parentheses of its own and leaves the parentheses: `longest * (k as i64… | 57c72847ab |
| B-2026-11-11-46 | typecheck | medium | `karac fix` turns Rust's `pkt[2..].split(\|&b\| b == 0).collect()` into `.split(..).iter().collect()`, which the next check refuses with E0516: the `.i… | a193908f55 |
| B-2026-11-11-47 | mir-llvm+runtime | medium | A compiled `Pool.acquire` can stall for its whole timeout though a value is free: its `__ready` (karac_mir_ready_pool) sees the pool free and lets th… | 30b8a505a3 |
| B-2026-11-11-60 | mir | medium | A tuple, array, `Vec`, `Option` or derived-`Ord` struct holding a value with a hand-written `Ord` sorts and keys sorted tables by its parts' plain or… | 3d987fca00 |
| B-2026-11-11-61 | typecheck | low | A `scan` whose state starts at an unsuffixed literal settles the state to `i64` before the `collect` that would fix it, so kata 1118's Rust shape `[0… | e036c0dfdd |
| B-2026-11-11-62 | typecheck | medium | `self` in a generic impl (`impl[R] Wrap[R]`) is typed `Wrap` with no arguments, so handing it to `fn helper[R](w: ref Wrap[R])` is refused "expected… | d763f8b1fa |
| B-2026-11-11-63 | typecheck+interp+mir-llvm | low | `BufReader` has no `read_exact` (the remainder of B-2026-11-10-44): the sockets, `File` and the child pipes have it, but a library `impl[R] BufReader… | a267acdbe9 |
| B-2026-11-11-50 | typecheck | medium | A method on a const (`NAME.as_bytes()`, `CODES.map(..).sum()`) gets no fix-it, or one in the wrong place: the path call it parses as becomes a method… | 4e2cef2b9b |
| B-2026-11-11-51 | typecheck | medium | `concat` of a sequence of `Vec`s is not Rust's flatten: on a `Slice[Vec[u8]]` it is refused with a `T: Display` bound, and on a `Vec[Vec[u8]]` it is… | cbf2b8ae4b |
| B-2026-11-11-64 | typecheck+mir | medium | Two tuple match arms, one holding a `ref` to a `Copy` value and the other the value, are refused: `match place.get(x) { Some(i) => (0, i), None => (1… | f6489288db |
| B-2026-11-11-65 | parser | low | `karac fix` deleting a Rust `use std::collections::HashMap;` line between two blank lines leaves the two blank lines in a row. | 75af478a7c |
| B-2026-11-11-66 | runtime | medium | A compiled program's group join sometimes waited out a task's whole acquire timeout after every task had finished: the first worker, which alone poll… | 316e6be03d |
| B-2026-11-11-70 | parser | medium | An argument's `&&` (Rust's borrow of a borrow, `MONTHS.contains(&&rest[..3])`) is a parse error with no fix, "Expected expression, found `&&`", which… | 37af642c5e |
| B-2026-11-11-71 | typecheck | low | Rust's `s.into_bytes()` on a String is a bare "no method 'into_bytes' on type 'String'", with no hint or fix toward `s.bytes().to_vec()` | 37af642c5e |
| B-2026-11-11-72 | runtime | low | `Duration` has no arithmetic: `a + b`, `a - b` and `d * n` are refused ("type 'Duration' does not implement trait Add"), where Rust's, Go's and Java'… | 5c76d17174 |
| B-2026-11-11-73 | parser | low | Rust's `thread::sleep(d)` does not mend: `use std::thread;` becomes `import std.thread;` ("unknown module `std.thread`") and `std::thread::sleep(d)`… | 5c76d17174 |
| B-2026-11-11-74 | parser | low | `karac fix` deleting a Rust `use std::rc::Rc;` / `use std::cell::RefCell;` line between two blank lines leaves the two blank lines in a row (the rema… | e70aabada4 |
| B-2026-11-11-75 | parser | medium | `karac fix` writes a Rust parameter's `&Option<Rc<RefCell<T>>>` as `ref Option[T]`, so a node matched out of it is a `ref T` and `Some(n)` cannot be… | 628eb9974d |
| B-2026-11-11-83 | parser | low | `karac fix` drops the `&` of a Rust argument `&(expr)` but leaves its parentheses: `seen.get(&(score - 1))` becomes `seen.get((score - 1))` (kata 112… | 46ef5d349a |
| B-2026-11-11-84 | typecheck | medium | `karac fix` puts the `.clone()` for a moved `let mut next = team;` re-binding of a `for` element at the move, leaving `next.push(p)` writing through… | afb930a6c1 |
| B-2026-11-11-85 | effect | medium | A call on an ambient module (`fs.write(...)`) in a program that also uses sockets is charged `TcpStream.write`'s `sends(Network) receives(Network)`,… | e92c63a596 |
| B-2026-11-11-86 | typecheck | medium | A number a closure gives to `map` takes `i64` before `collect` can type it: `(0..n).map(\|i\| if i % 2 == 0 { [4, 7] } else { [7, 4] }).collect()` into… | 5a92d2ae01 |
| B-2026-11-11-87 | parser+typecheck | low | `karac fix` turns `let run = (j - i) as i64;` into `let run = (j - i);`: the redundant cast's fix keeps parentheses that group nothing (kata 1128's s… | 0385ddd3ac |
| B-2026-11-12-1 | typecheck | medium | A number inside a tuple pushed into a tuple slot keeps `i64`: `let x = 0; q.push((x, 1))` into a `Vec[(i32, i32)]` is refused as narrowing `x` (kata… | 2d47b8e713 |
| B-2026-11-12-2 | typecheck | low | A number read out of a tuple a literal was pushed into, then kept `i64` by a comparison, narrows into an `i32` slot with no machine fix: kata 1129's… | 4a5457da30 |
| B-2026-11-12-3 | parser | low | `karac fix` rewrites Rust's `&[..]` on a list literal to `ref [..]`, which the checker refuses: `for &(p, q) in &[(1, 1), (1, -1)]` (kata 1131) | 73ec8b248f |
| B-2026-11-12-4 | typecheck | low | A value made an `i64` by two literal bindings has no machine fix: `let p = 1; let q = 2; m = m.max(p * a + q * a)` with `a: i32` (kata 1131) | 73ec8b248f |
| B-2026-11-12-5 | typecheck | low | `lossless_cast` misses a widening cast pushed or inserted into a list and one used as a list position, so `karac fix` leaves `graph[r[0] as i64].push… | d361886e40 |
| B-2026-11-12-6 | typecheck | high | A literal beside a narrower branch takes that branch's type even when it does not fit: `show(if c < 128 { c } else { 0xFFFD })` with `c: u8` and `sho… | 3436c6649a |
| B-2026-11-12-7 | typecheck | medium | An assignment refuses an `if`/`match` whose branches widen to the place: `y = if c { a } else { y }` with `a: u8`, `y: i64` is "if/else branches have… | 3436c6649a |
| B-2026-11-12-8 | typecheck | low | `lossless_cast` calls a cast unnecessary where it fixes an open binding's type: `letter = b[j] as i64` after `let mut letter = -1` with `b: Vec[u8]`,… | 3436c6649a |
| B-2026-11-12-9 | typecheck | low | A nested list literal passed as a nested `Array` is refused with no explanation or fix: `let mut result = [[1, 0, 0], [0, 1, 0], [0, 0, 1]]; result =… | 97dd97098e |
| B-2026-11-12-10 | mir-llvm | medium | Programs built for macOS keep no frame records: every function saved x29 (rbp) without pointing it at its own record, against Apple's arm64 and x86_6… | c5e9a24b82 |
| B-2026-11-12-11 | typecheck | low | A narrowing refused at a binding that holds an element of a fill offers only `as i32`: LeetCode's `let max_k = min(left[i][j], up[i][j]); for k in (1… | 032df1a4c2 |
| B-2026-11-12-12 | typecheck | low | A literal stored into an element of an open fill settles the fill's literal: `let mut t = Vec.filled(3, 0); t[1] = 4; let r: i32 = t[1];` is refused… | 4a5457da30 |
| B-2026-11-12-13 | typecheck | medium | A `fold` seed read as an index in its closure is refused, `index must be an integer or range, found 'i64'`: the seed is held open while the closure i… | d34854564c |
| B-2026-11-12-14 | typecheck | medium | `(0..=n).map(\|_\| Set.new()).collect()` into a `Vec[Set[i64]]` is refused, `Vec[Set[i64]]` implements `FromIterator[Set[i64]]`, not `FromIterator[Set[… | d34854564c |
| B-2026-11-12-15 | mir-llvm | low | `"U".repeat(n)` of a one-byte string is a call into `karac_mir_str_repeat`, an allocation through `karac_alloc_or_panic` and up to three `memcpy` cal… | 3f394d9182 |
| B-2026-11-12-16 | typecheck | low | A narrowing read of an element of an array of literals offers only `as i32`: LeetCode's `let mut res = [0, 0]; .. | 9049969b63 |
| B-2026-11-12-17 | mir-llvm | low | `for i in (low..n).step_by(2)` over an `i64` range runs about 1.2x the same loop written `while i < n { ...; i += 2 }`: `IterStepBy[Range].next` take… | ad30c70d4e |
| B-2026-11-12-18 | typecheck | low | `(1..10).filter(\|x\| *x % 2 == 0)` is accepted while `(1..n).filter(\|x\| *x % 2 == 0)` is refused with "drop the `*`": over a range of bare literals th… | 18ea960272 |
| B-2026-11-13-1 | mir-llvm+cli | medium | `String.normalize` and `std.regex` pass `karac check --targets=wasm_wasi` and then fail `karac build --target=wasm_wasi` with a bare wasm-ld `undefin… | 637125377c |
| B-2026-11-13-2 | typecheck | low | A list literal compared with `==` / `!=` against an `Array` is typed `Vec` instead of taking the other operand as its expected type: `m != [75u8, 82u… | 16d2756e18 |
| B-2026-11-13-3 | typecheck+ownership | medium | A `MutFn` stored in a struct field or a `Vec` can be called through shared access (a non-`mut` binding, a borrowed `self`, a bare parameter), which c… | e809f0725c |
| B-2026-11-13-4 | typecheck+mir | medium | A `par for` whose body value is a tuple is refused at check with the internal `an expression with no recorded type is not lowered yet` | 820ad8250d |
| B-2026-11-13-5 | typecheck | low | `div_euclid` and `rem_euclid` exist only on `i64`: `i32`, `u8` and the other integer widths, and `f64`, have neither, although design.md names them a… | c6d15118a0 |
| B-2026-11-13-6 | typecheck+cli | medium | Rust's `binary_search_by_key` and `binary_search_by` taken apart as a `Result` have no fix: `match h.binary_search_by_key(&snap_id, \|&(s, _)\| s) { Ok… | 4341008d1e |
| B-2026-11-13-7 | typecheck | low | `Vec.filled(n as i64, 0)` with `n: i32` is not linted as a lossless cast, while `let x: i64 = n as i64` and `g(n as i64)` at a user function's `i64`… | 4341008d1e |
| B-2026-11-13-15 | typecheck | medium | Rust's `dst.copy_from_slice(src)` is "no method 'copy_from_slice' on type 'Slice'" with no fix, and the error type sets off a borrow error at every l… | b4be496812 |
| B-2026-11-13-16 | mir-llvm | high | `s == a` with a `Slice` on the left and an `Array` of the same element type on the right is never equal in compiled code (the array is read as a slic… | b4be496812 |
| B-2026-11-13-17 | typecheck | low | Comparing a `Slice` with an `Array` or `Vec` is accepted with the slice on the left and refused with it on the right; design.md § Slices does not say… | 1e72807626 |
| B-2026-11-13-18 | typecheck | medium | In a generic fn with `it: own I` and `I: Iterator[Item = T]`, the item of `for x in it` is typed as `I` itself for method calls and annotations: `x.a… | 820ad8250d |
| B-2026-11-13-19 | mir | low | A `par` or `par for` that calls an un-annotated local holding a returned function value is refused as "`unknown (a call through a value)`", even when… | 820ad8250d |
| B-2026-11-13-20 | cli | low | 86 of the 96 symbolic `E_*` codes minted in src/ are unknown to `karac explain`, against design.md §17's "every code is catalogued" | 127efa7131 |
| B-2026-11-13-21 | mir | low | `ParseError` variants differ from core-types.md Parsing in two edge cases: `u8.parse("-0")` gives `Invalid` where `InvalidDigit` is specified, and wh… | 127efa7131 |
| B-2026-11-13-22 | typecheck | low | A run-time value as a const generic argument (`Array[i64, n]` with `n` a parameter or local, or `n + 1`) is not reported at the argument as design.md… | 2d1651fa5d |
| B-2026-11-13-23 | ownership | low | Assigning into a field of a value moved as a whole (`eat(p); p.a = 1;`) is correctly refused, but the help suggests deriving `Clone` and cloning `p`… | 2d1651fa5d |
| B-2026-11-13-30 | cli+resolver | medium | A script inside a package but outside `src/` does not get the package's dependencies: `karac run app/scripts/s.kara` importing a path dependency is "… | 0e77ed613d |
| B-2026-11-13-31 | ownership | high | A method with a `self` or `mut ref self` receiver may return a view-typed ARGUMENT (`fn pick(self, other: Str) -> Str { other }`, also `Option[ref T]… | acae88f3b9 |
| B-2026-11-13-32 | effect | low | An effect slot's refusal prints a verb that names no resource with empty parentheses: a closure that calls `sleep` in a field `Fn() with reads(Clock)… | 416dbf25cc |
| B-2026-11-13-33 | mir-llvm | low | `==` and `!=` on two `Slice[u8]` compile to a byte-by-byte loop where `Str` equality calls `memcmp`: kata 1147's greedy arm written over `text.bytes(… | 901f19afe4 |
| B-2026-11-13-34 | mir-llvm | low | `partition_point` is the branching binary search (runtime/stdlib/source/collections.kara) where Rust's is branch-free: kata 1150's 10,000,000 searche… | 5674385c18 |
| B-2026-11-13-35 | typecheck | medium | A `scan` closure whose next state is not computed from the state but whose item is, `data.iter().scan(0, \|prev, x\| Some((*x, (prev, *x))))` (each val… | ba87bfcf21 |
| B-2026-11-13-24 | interp | low | On the MIR interpreter `i128.MIN.abs()` returns `i128.MIN` instead of trapping as integer overflow, and `i128.MIN.abs_diff(3)` prints 2^127 - 3 (...1… | 5d1858adec |
| B-2026-11-13-25 | typecheck+mir | low | `SortedMap.floor` / `ceiling` are typed `Option[(K, V)]` where docs/library/collections.md:213-214 says `Option[(ref K, ref V)]`: `let e: Option[(ref… | 861ab09e35 |
| B-2026-11-13-26 | ownership+cli | low | `unused_mut` ignores statement and block lint attributes: `#[allow(unused_mut)] let mut x = 1;` still warns, `#[deny]` / `#[expect]` on the statement… | 483e7d5400 |
| B-2026-11-13-27 | parser | low | An attributed block-like statement needs a trailing `;`: `#[allow(unused_mut)] { let mut x = 1; println(x); }` and `#[allow(x)] if c { . | 483e7d5400 |
| B-2026-11-13-28 | parser+typecheck | low | An unsuffixed literal in `u128`'s upper half is refused at parse even under a `u128` annotation: `let h: u128 = 1701411834604692317316873037158841057… | 8048a5fb01 |
| B-2026-11-13-36 | typecheck | medium | `assert(1)`, `assert("x")` and `assert_eq(1, "a".to_string())` pass check and then fail inside the compiler's MIR stage ("invalid MIR", "not lowered… | 601dac91cc |
| B-2026-11-13-40 | resolver+cli | low | An import's segment and name spans are not shifted with the rest of a module's: a merged `_test.kara`'s E0111 names its sibling's file, and `karac fi… | 9d85811653 |
| B-2026-11-13-41 | cli | low | `karac check` and `karac run` validate a package's manifest and dependencies differently from `build` and `test`: a registry or git dependency passes… | 9d85811653 |
| B-2026-11-13-42 | cli | medium | `karac check src/x_test.kara` on a package's test companion reports "All checks passed" while the package check refuses that very file | d65304bffb |
| B-2026-11-13-43 | runtime | high | The library's `split`, `splitn`, `match_indices` and `matches` iterators keep a `Str` view of the PATTERN, while §5.4 has the method's result borrow… | 0afc89e2e8 |
| B-2026-11-13-44 | mir+mir-llvm | high | Two impls of one generic trait for a primitive (`impl Conv[String] for i64`, `impl Conv[f64] for i64`) run one impl for every call, whichever instanc… | ddc4a7ab43 |
| B-2026-11-13-45 | typecheck | low | `#[derive(PartialEq)] struct Pair[T]` is accepted as PartialEq at an argument that is not (`Pair[NoEq] == Pair[NoEq]` checks and prints true), agains… | ddc4a7ab43 |
| B-2026-11-13-46 | mir+mir-llvm | medium | Two non-generic traits with a method of one name, both implemented for a primitive (`impl A for i64 { fn m }`, `impl B for i64 { fn m }`): `A.m(n)` c… | 978cd90710 |
| B-2026-11-13-47 | typecheck | low | A type parameter bounded by two instances of one generic trait (`X: Conv[String] + Conv[f64]`) cannot call the second: `let f: f64 = Conv[f64].conv(x… | 299830c914 |
| B-2026-11-13-48 | typecheck | medium | An operator bound on a projection in a where clause is not consulted by the operator lookup: design.md §9's own `fn total[I: Iterator](it: own I, zer… | 63087c10a5 |
| B-2026-11-13-50 | cli | low | A virtual workspace root (only `[workspace]` and `[workspace.dependencies]`) is refused, and a `path` in `[workspace.dependencies]` resolves against… | a36e3f7376 |
| B-2026-11-13-60 | cli | medium | Workspace members do not share one `kara.lock` and one output directory: each member resolves and writes its own, so two members can lock different v… | 4061fcf76f |
| B-2026-11-13-70 | parser | medium | `karac fix` leaves `println!("{} CRLF", text.matches("\r\n").count())` unrewritten: a format argument whose string or char literal holds an escape, a… | e65451aad4 |
| B-2026-11-13-71 | typecheck | low | `addr.rsplit(':')` is answered "did you mean 'split'?", which starts from the other end; Kāra's spelling of taking the last piece is `rsplit_once` | e65451aad4 |
| B-2026-11-13-72 | typecheck | low | `group.join()` on a `TaskGroup` is a bare "no method 'join'": a group joins its tasks when dropped, so the repair is `drop(group)` | e65451aad4 |
| B-2026-11-13-73 | typecheck+mir | medium | `TcpStream` (and `TlsStream`) have no `read_to_end`: a connection that carries one payload and closes, such as an FTP data connection, can only be re… | e65451aad4 |
| B-2026-11-13-74 | typecheck | low | `karac fix` writes Rust's `if c { ".." } else { &arg }` as `{ ref arg }`, which the check then refuses ("if/else branches have incompatible types: 'S… | 2ed0a65169 |
| B-2026-11-13-75 | mir-llvm+runtime | medium | Output printed before a foreign `exit()` is lost in compiled code: `unsafe extern "C" { fn exit(code: i32) with panics; }` then `println("a"); exit(3… | e84c3d02ad |
| B-2026-11-13-76 | cli+resolver | low | A `test "..." { }` block in a package's `src/main.kara` (not a `_test.kara` file) is silently never run: `karac check` passes and `karac test` report… | e84c3d02ad |
| B-2026-11-13-77 | mir | low | E0509 for a view stored where its borrow is not kept says "a view of a temporary value" where the program names the place (`t`, `t.s`), and the share… | e84c3d02ad |
| B-2026-11-13-78 | typecheck | low | A default generic argument is not applied to a type written with no arguments: `struct Holder[T = i64]` then `let a: Holder = Holder { v: 3 }` is "ex… | e84c3d02ad |
| B-2026-11-13-79 | mir | medium | Results borrow what a view parameter views (core semantics §5.4 ruling, 6980a490b6): a parameter or receiver of a view type (`struct Cursor { src: St… | 7bf1787ef8 |
| B-2026-11-13-80 | typecheck | medium | A struct or enum's `const` parameter could not be given an argument: design.md § Const generic parameters gives user types `const` parameters (`struc… | 5a2e3d6aa6 |
| B-2026-11-13-81 | typecheck | low | A view literal built in place at an `own` parameter is taken for a temporary the result borrows: `pick([t.as_str()])` with `fn pick(other: own Vec[St… | 222b88e109 |
| B-2026-11-13-82 | mir | medium | Moving a `String` on a `return` inside a loop over its `split(..)` is refused (E0516) since the split iterator keeps its own `String` pattern (0afc89… | 7bf1787ef8 |
| B-2026-11-14-1 | typecheck | low | Whether a let-bound literal compared with an `i32` can still be fixed to `i32` by a later argument depends on statement order: `let i = 0; take_i32(i… | 4a5457da30 |
| B-2026-11-14-2 | ownership | low | The ownership-cycle help on a recursive plain enum offers `Box[T]`, which Kāra does not have, instead of design.md's `shared enum` | c1fddd8097 |
| B-2026-11-14-3 | typecheck | low | `f(x.clone())` where `f`'s parameter only borrows copies `x` for nothing, and no lint flags it: a LeetCode Rust template's adapter keeps `Solution.nu… | 940c5678af |
| B-2026-11-14-4 | interp | medium | A `VecDeque` worked from the front (`pop_front`, `pop_front`, `push_front`, `push_back` a round) runs in quadratic time under the MIR interpreter: `p… | 3d1fc8e26 |
| B-2026-11-14-5 | ownership | medium | A range view made in a `match` / `if let` scrutinee stays borrowed through the arms even when the scrutinee's value holds no borrow: `match v[1..].it… | 3d1fc8e26 |
| B-2026-11-14-6 | typecheck | low | `Array[i64, 26].filled(0)` is refused with `no method 'filled' on `Array[…]``: the type is elided, and nothing points to `[0; 26]`, Kāra's fixed-arra… | 940c5678af |
| B-2026-11-14-7 | autopar+runtime | medium | An auto-par loop whose body recurses deeply runs the recursion on a pool worker's 8 MiB stack, where the main thread has 1 GiB: a program that finish… | f7a52809e7 |
| B-2026-11-14-8 | cli | low | Five refusals name their rewrite but carry no edit, so `karac fix` leaves the program refused; `karac fix --help` describes the removed closure-prefi… | a72be5f5d1 |
| B-2026-11-14-9 | typecheck | medium | `c.eq_ignore_ascii_case(d)` on a `char` (and on a `u8`) is refused with `no method 'eq_ignore_ascii_case' on type 'char'`, and `b.to_ascii_lowercase(… | 5a1b79539 |
| B-2026-11-14-10 | cli | low | 168 symbolic codes written only into a diagnostic's message (`error[E_CHAR_AS_NARROW_INT]: …`) are unknown to `karac explain`, which reads each as a… | 15692a607a |
| B-2026-11-14-11 | mir-llvm | low | Kata 1546's `Set[i64]` greedy (contains, insert, and a `clear` at every cut) runs 1.34x checked Rust's `HashSet` by median (0.876 s vs 0.653 s) and e… | 760a40f78a |
| B-2026-11-14-12 | typecheck | low | `w.bytes().iter().map(\|b\| b - 96).collect()` into a `Vec[i64]` is refused with a bare `C: FromIterator[u8]` trait-bound error that names no remedy, t… | 16f434ada8 |
| B-2026-11-14-13 | typecheck | low | `vec![false; n as usize]` checks with only a deprecation warning, but the warning's own fix, `Vec.filled(n as usize, false)`, is refused (`usize` mix… | 1812f3875c |
| B-2026-11-14-14 | typecheck | low | A bit count (`count_ones`, `leading_zeros`, `i64` by the 10-30 ruling) going into a Rust port's `u32` (`ones += x.count_ones()`, `max_bits.max(32 - x… | 67f08e83c5 |
| B-2026-11-14-15 | mir-llvm | low | `(1..=end).chain(start..=n).collect()` into a `Vec[i64]` grows the vector by realloc instead of sizing it once from the two ranges' exact lengths: ka… | 4c1227e77d |
| B-2026-11-14-16 | mir-llvm | medium | `SortedSet.range` / `range_inclusive` (and `SortedMap`'s) find their bounds by a binary search over positions in the library source (`bound_index`, r… | 77271a3d45 |
| B-2026-11-14-17 | typecheck+cli | low | A prefix table that `karac fix` carried from `vec![0; n + 1]` to `Vec.filled(n + 1, 0)` takes `i64` from its literal, and a read of it passed to an `… | 360519d3dc |
| B-2026-11-14-18 | typecheck+cli | low | Three counters bound to literals that feed one another (`best = best.max(positive)`, `positive = negative + 1`) and reach an `i32` return take three… | 360519d3dc |
| B-2026-11-14-19 | typecheck+mir | medium | An early exit inside a cleanup block crashes the compiler: `loop { defer { break; } }`, `defer { continue; }`, `defer { return 3; }` and `?` inside a… | a868f2a903 |
| B-2026-11-14-20 | mir | medium | A by-move capture of a struct's field takes the whole struct in MIR, so design.md § Captures' disjoint capture holds only for ref and mut-ref capture… | a868f2a903 |
| B-2026-11-14-21 | cli | low | `karac fix`'s `redundant_ref_param` lint skips a method whose name is also a local variable read anywhere in the program: `functions_used_as_values`… | 49a607740 |
| B-2026-11-14-22 | mir | medium | An index whose base is a literal evaluates the index before the literal's elements: `[p(1), p(2)][p(0)]` prints e0 e1 e2 where design.md §8.1 gives e… | 4dda39eb88 |
| B-2026-11-14-25 | parser | low | Rust lifetimes have no fix: `&'static str` stops the parse ("Expected type, found `'static`"), and `struct P<'a>`, `impl<'a>`, `fn f<'a, T>` stop it… | bdba568e93 |
| B-2026-11-14-26 | typecheck | low | A closure whose tail is `*n`, with `n` lending a non-`Copy` value (`.iter().map(\|(_, n)\| *n)`, `.map(\|n\| *n)`), passes the check and is refused only… | bdba568e93 |
| B-2026-11-14-27 | interp | medium | The MIR interpreter refuses an array handed by value to a byte-reading native: `sock.send_to(b"STOP", to)` stops with "expected a sequence, found Agg… | bdba568e93 |
| B-2026-11-14-23 | typecheck+cli | low | Two fields of borrowed receivers put in a tuple to pick one by a test (`let (short, long) = if self.pairs.len() <= other.pairs.len() { (self.pairs, o… | 572956a03 |
| B-2026-11-14-24 | typecheck | low | A struct field written `Map[usize, i32]` (Rust's `HashMap<usize, i32>` as `karac fix` carries it) filled from `enumerate()`'s `i64` index is refused… | 9c19e5e6b |
| B-2026-11-14-28 | mir-llvm | low | A two-pointer merge over two receivers' `Vec` fields (`while i < self.pairs.len() and j < other.pairs.len() { let (a, x) = self.pairs[i]; let (b, y)… | a63659722a |
| B-2026-11-14-29 | typecheck | low | A read-only view bound by `let mut s = str1.bytes()` and passed as `mut s` to a `mut ref Vec[u8]` parameter is refused with only the bare type mismat… | 6c99bc3328 |
| B-2026-11-14-30 | mir+mir-llvm | low | `SendError`'s `Display` prints its derived Debug form on both MIR backends: after `tc.try_send(2)` on a full `channel(1)`, `e.to_string()`, `f"{e}"`… | eb69ab0c7d |
| B-2026-11-14-31 | cli+typecheck | high | An `unsafe_op_in_unsafe_fn` error is printed and then ignored: `karac check` says "All checks passed." and exits 0, `karac run` executes a null deref… | 672a66f1f0 |
| B-2026-11-14-32 | ownership | medium | `let mut x` whose only mutable use is `ptr.mut(x)` draws unused_mut, and applying its fix (deleting `mut`) makes `ptr.mut(x)` refused, so `karac fix`… | 672a66f1f0 |
| B-2026-11-14-33 | effect | medium | An `extern` fn with no `with` clause had only its ABI default (`blocks`), not every effect as design.md § 15 says, so calling it passed `with reads(E… | 672a66f1f0 |
| B-2026-11-14-34 | effect | medium | A call through a `dyn Trait` field or element whose type writes no clause had the union of the impls this program coerces into it (none when it coerc… | 672a66f1f0 |
| B-2026-11-14-35 | parser | low | The empty effect clause on a `dyn` type, `dyn Sh with ()`, does not parse in a field or a return type | 672a66f1f0 |
| B-2026-11-14-36 | interp+mir-llvm | low | `Vec.as_ptr()` (used in § 15's own example) passes check but __mir-run and the LLVM backends fail with "call of unknown function Vec[i64].as_ptr" | 672a66f1f0 |
| B-2026-11-14-37 | typecheck | low | `Slice[T].get_unchecked(i)` and `Vec[T].get_unchecked(i)` return the element by value, but design.md § Bounds checks says `ref T`, so the spec's own… | 672a66f1f0 |
| B-2026-11-14-38 | typecheck+mir | low | A C-like enum has no generated `.values()` (design.md § 15): `UsbClass.values()` is "no associated function 'values'" | 672a66f1f0 |
| B-2026-11-14-39 | typecheck | low | `#[repr(...)]` arguments are not checked: `#[repr(frob)]`, `#[repr(u128)]` on a struct, and the deferred `packed` and `align(8)` all pass silently | 672a66f1f0 |
| B-2026-11-14-40 | typecheck | low | An unsuffixed literal in `u128`'s upper half that nothing types, e.g | 672a66f1f0 |
| B-2026-11-14-41 | mir+mir-llvm | low | x/X/o/b on a negative narrow integer prints the 64-bit two's complement on both MIR backends: with `m: i8 = -1` and `k: i32 = -2`, `f"{m:x}\|{k:x}\|{m:… | 6d8fd72d67 |
| B-2026-11-14-42 | mir+mir-llvm | low | With a width and no align, a non-number (String, Str, char, bool, a user Display) right-aligned on both MIR backends; design.md §3 now says it left-a… | 6d8fd72d67 |
| B-2026-11-14-44 | typecheck | low | `.map(\|s\| s.parse().unwrap()).collect()` into an annotated `Vec[i32]` gets only "name the type" with no edit: the closure tail's `s.parse()` took `ma… | dba1dbedaf |
| B-2026-11-14-45 | typecheck | low | `days[..(m - 1) as usize].iter().sum() + d` returned as an `i32` gets the bare narrowing error with no edit, though `days.iter().sum() + d` gets the… | dba1dbedaf |
| B-2026-11-14-43 | mir-llvm | low | `i64.parse(date[5..7]).unwrap()` costs about 70 instructions per call on a two-to-four-digit slice, against about 13 for a hand-written digit loop: k… | 3d963ed084 |
| B-2026-11-14-60 | ownership | medium | A field captured by a closure passed to a lazy adaptor stays borrowed after the chain is drained, and a call through a local closure binding counts a… | d2b676af88 |
| B-2026-11-14-61 | interp | low | The MIR interpreter walks a plain (non-`shared`) linked list in quadratic time: a `ref` cursor or a recursion over `next: Option[ListNode]` takes 0.8… | cb94554ca6 |
| B-2026-11-14-63 | typecheck | low | Rust's `VecDeque::from(v)`, `dq.drain(..)`, a `String`'s `remove(i)` / `insert(i, c)` and a slice's or array's `rotate_left` / `rotate_right` are ref… | 1617e3a972 |
| B-2026-11-14-64 | typecheck | low | Rust's `scan` writing its state through an index (`counts[d] += 1` with no `*`) is not rewritten by `karac fix`, though a `*acc` write is: it drew a… | 1617e3a972 |
| B-2026-11-14-65 | parser | low | `karac fix` carries Rust's `mut num1: i32` parameter to `num1: own i32`, which takes nothing a plain `i32` does not; a scalar's `mut` parameter is a… | 17c3e234b6 |
| B-2026-11-14-66 | typecheck | medium | `nums.iter().skip(1).fold(Map.new(), \|m, x\| { let mut m = m; m.insert(x, ..); m })` into an annotated `Map[i64, i64]` (and the same with `Set.new()`)… | b2eff8ea4d |
| B-2026-11-14-46 | mir+mir-llvm | medium | A redirect's `Location` is not resolved as RFC 3986 says: `?page=2` from `/q` goes to `/?page=2` on both backends, and the compiled client sends `../… | b947610ef4 |
| B-2026-11-14-47 | mir | medium | The interpreter's HTTP client returns a cut-short body as a whole answer: a body shorter than its Content-Length, a chunked body with no last chunk,… | 171f807df7 |
| B-2026-11-14-48 | mir | low | The interpreter's HTTP client returns an interim 1xx response as the answer: a server that sends `100 Continue` before its `200` gives `status() == 1… | 171f807df7 |
| B-2026-11-14-49 | mir+mir-llvm | medium | The two HTTP clients send different requests: the interpreter adds `User-Agent: ureq/2.12.1`, `Accept: */*` and `accept-encoding: gzip` and decodes a… | 171f807df7 |
| B-2026-11-15-1 | typecheck+cli | low | A tuple `let` slot whose binding is read only inside a closure still gets `.clone()` from `karac fix` where `ref` would do: `let (small, large) = if . | 67f08e83c5 |
| B-2026-11-15-2 | mir-llvm | low | Walking a `String`'s chars costs 2x to 2.8x Rust's `chars()` in a `karac build` binary: kata 1573's `s.chars().enumerate().filter(\|(_, c)\| c == '1').… | 3d963ed084 |
| B-2026-11-15-3 | mir-llvm | high | Every `dyn` call traps on wasm_wasi ("unreachable", or "null function or function signature mismatch" in the callee): the vtable is `{ptr drop, i64 s… | 65adcd1548 |
| B-2026-11-15-4 | mir-llvm | high | `zip` then `map` reads zeros or garbage on wasm_wasi: a `(ref A, ref B)` pair handed to a closure went as LLVM `{ptr, ptr}`, which puts the second po… | 65adcd1548 |
| B-2026-11-15-5 | mir-llvm+runtime | medium | Every `std.log` program fails to link on wasm_wasi, wasm-ld refusing `karac_mir_log_dispatch` (declared `(i32, i64, i32)` by the module, defined `(i3… | 65adcd1548 |
| B-2026-11-15-6 | runtime | medium | `CString.to_str()` returns an empty `Str` on wasm_wasi while `to_string()` and `len()` are right: `karac_mir_cstr_to_string` wrote the view's two wor… | 65adcd1548 |
| B-2026-11-15-7 | interp+mir-llvm | low | `VecDeque.truncate` drops the removed elements back to front, where collections.md says it drops them from index `len` to the back and every other el… | 65adcd1548 |
| B-2026-11-15-8 | mir+mir-llvm | high | A compiled program's HTTP `Response.body()` (and `HttpError.body()`) of a body that is not UTF-8 is a `String` holding the raw bytes, which breaks St… | 987cb09457 |
| B-2026-11-15-9 | parser+cli | low | `karac fix` moves Rust's nested `fn` to the top level and leaves behind a `const` of the body around it that the `fn` names, so the moved `fn` names… | 76e5a13ad |
| B-2026-11-15-12 | typecheck | low | Rust's `*guard` on a `MutexGuard` (`*m.lock() = v`, `take(mut *m.lock())`) is refused with "unary '*' requires 'ref T' ..." and no fix; the guard's v… | 2e243f86c1 |
| B-2026-11-15-13 | typecheck | low | `unwrap_or((ref line, ""))` against `(Str, Str)`, which karac fix writes for Rust's `(&line, "")`, is refused ("expected '(Str, Str)', found '(ref St… | 2e243f86c1 |
| B-2026-11-15-14 | typecheck+mir | medium | Rust's `const X: &[T] = &[..]` becomes `const X: Slice[T] = [..]`, which the spec does not allow and `for x in X` fails to build ("a constant used at… | 2e243f86c1 |
| B-2026-11-15-15 | mir | medium | A binding of a loop item destructured through its `Option` (`for (n, _) in arr.iter() { kept.push(n) }` over an `Array`) keeps the item borrowed: "wr… | 2e243f86c1 |
| B-2026-11-15-16 | mir-llvm | low | `(0..n).map(\|_\| ..).collect()` into a `Vec[i64]` costs about 28 instructions an item in a `karac build` binary against 4.4 for the push loop it stand… | 3d963ed084 |
| B-2026-11-15-17 | parser | low | A Rust file's `use std::...` lines mend one per `karac fix` round: after the first line's diagnostic, the parser's item recovery skipped to the next… | f5f9065de8 |
| B-2026-11-15-18 | effect | low | A `FileSystem` fake whose `close` prints passes `karac check` when its file is used inside a `with_provider[Stdout]` nested in the `FileSystem` overr… | 0dd548c8b8 |
| B-2026-11-15-19 | resolver | low | `std.random.next_u64()` is refused as "a Rust path .. | 0dd548c8b8 |
| B-2026-11-15-20 | cli | low | `karac explain` refuses 15 live typechecker codes (E_INT_AS_BOOL, E_SHAPE, E_IMPL_TRAIT_ASSOC_MISMATCH and 12 more) plus W_DEPENDENCY_YANKED and the… | 0dd548c8b8 |
| B-2026-11-15-21 | typecheck | low | A `collect()` passed where a call takes a `Slice[T]` is refused: `String.from_utf8(s.bytes().iter().map(\|c\| c + 1).collect())` (strings.md's `from_ut… | bae463436e |
| B-2026-11-15-22 | interp+mir-llvm | high | A tuple literal of owned places passed for a tuple parameter with `ref` elements (`g((a[0], b[0]))` for `p: (ref i64, ref i64)`, or a closure's `\|(x,… | 76f25d4356 |
| B-2026-11-15-23 | mir-llvm+runtime | medium | A trap on wasm_wasi loses everything the program printed before it: stdout was block-buffered on a pipe, and a trap (the host's stack limit) ends the… | 76f25d4356 |
| B-2026-11-15-24 | parser | low | A `///` doc comment before a method's receiver (`self`, `ref self`, `mut ref self`, `own self`) is a parse error naming `self` / `ref` as a reserved… | 1ea33f776c |
| B-2026-11-15-25 | mir+mir-llvm | medium | A server has no way to read a request body as it was sent: `Request` has `body()` (a `String`, so a body that is not UTF-8 is decoded lossily, B-2026… | de1d38948a |
| B-2026-11-15-26 | cli | low | `karac fmt` moves a `///` before a function's first parameter (or before a method's receiver) into the function body, where it documents nothing and… | e5597ca22f |
| B-2026-11-15-27 | typecheck | low | `trailing_ones()` and `leading_ones()`, which Rust has on every integer, are refused with "no method 'trailing_ones' on type 'i64'", though `count_on… | b545bd3ba1 |
| B-2026-11-15-28 | parser+cli | medium | `karac fix` writes `vec![false; 1 << (m * n)]` as `Vec.filled(1 << (m * n, false)` and then refuses to write the result: the repeat literal's count i… | b545bd3ba1 |
| B-2026-11-15-29 | typecheck | medium | `Some(n).map(\|x\| (0..x).collect()).unwrap_or(Vec.new())` returned as a `Vec[i64]` is refused with "expected '?T3', found 'Vec[?T4]'": the `Option`'s… | b545bd3ba1 |
| B-2026-11-15-30 | typecheck | high | A generic free function called inside a method call's closure argument (`v.iter().map(\|x\| apply(x, \|y\| y * 2))`, with `fn apply[T, F: Fn(ref T) -> T]… | 35b04a7e5b |
| B-2026-11-15-31 | typecheck | low | Rust's `v.sort_by_cached_key(\|x\| key(x))` is refused with a bare "no method 'sort_by_cached_key' on type 'Vec'" and no fix, though `sort_unstable_by_… | c93ef18010 |
| B-2026-11-15-32 | mir+mir-llvm | high | A closure handed to `flat_map` that returns a closure using one of its non-`Copy` captures (`(0..2).flat_map(\|m\| (0..3).map(\|c\| m + clears[c]))`, `cl… | 558a84bb49 |
| B-2026-11-15-33 | typecheck | low | `Option.as_mut()` is refused with "no method 'as_mut' on type 'Option'" and no fix, though `as_ref` is there; Rust's linked-list templates walk `whil… | 558a84bb49 |
| B-2026-11-15-34 | typecheck+mir | medium | `successors(Some(x), \|lap\| Some(lap * 2))` with `x: ref i64` from `v.iter()` is refused with E0509 "the result borrows a temporary value, which this… | 1415b79cfb |
| B-2026-11-15-35 | typecheck | low | `rotate_left` and `rotate_right` are `Vec` methods only: Rust's `s[k..=r].rotate_left(1)` on a part of a `Vec`, a `mut Slice`, is refused ("turning t… | 0475af1b42 |
| B-2026-11-15-36 | parser+cli | medium | Rust's `impl fmt::Display for T { fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "{}{}", a, b) } }` is a `karac fix` dead end: afte… | 31302bad5d |
| B-2026-11-15-37 | typecheck+cli | low | `return nodes.get(v);` from a `-> Option[TreeNode]` is refused with a bare "expected 'Option[TreeNode]', found 'Option[ref TreeNode]'" and no fix, th… | 809d1c0c18 |
| B-2026-11-15-38 | typecheck | medium | A list literal returned by a closure (`d.iter().map(\|e\| [e[0], e[1], 1 - e[2]]).collect()` into a `Vec[Array[i64, 3]]`) is a `Vec`, so the `collect`… | 0475af1b42 |
| B-2026-11-15-39 | mir+mir-llvm | high | `for a in 1..3usize { let s = a + a; println(f"{s % a}"); }` passes `karac check` and then stops both backends with "invalid MIR: main: bb5[2]: binar… | cad67cae2b |
| B-2026-11-15-40 | typecheck+cli | medium | `println(f"{v} -> {total(v)}")` with `v` owned and `total(v: own Vec[i64])` is refused with E0516 ("cannot move `v` while it is still borrowed") and… | 0475af1b42 |
| B-2026-11-15-41 | cli | medium | A `karac repl` cell ending in a bare expression printed nothing (`1 + 2`, `x * 2`, `binary_search(nums, 7)`, where the book shows `Some(3)`), and wit… | 92d18d909c |
| B-2026-11-15-42 | cli | low | `karac repl`'s `:effects` printed about 200 lines of the standard library's effect table in Rust Debug form (`Arena.new: Effect { verb: Allocates, re… | 92d18d909c |
| B-2026-11-15-43 | cli | medium | `karac repl`'s `:save` wrote cells the REPL had refused into `main` (s2: a `println(s)` after `s` moved), so the saved file failed `karac check`; a b… | 92d18d909c |
| B-2026-11-15-44 | cli | low | `karac repl`'s `:type x` printed `<error>` for every session binding (`:type [1,2]` worked): the expression was typed alone, without the session's bi… | 92d18d909c |
| B-2026-11-15-45 | cli | low | `karac repl`'s `:help` said a mutation does not survive to the next cell and that bindings are recomputed (source replay) every cell; the REPL keeps… | 92d18d909c |
| B-2026-11-15-46 | other | low | The book's REPL transcript (ch01b) failed as written: its `binary_search` uses `usize`, which the checker now refuses, so every later cell met an und… | 92d18d909c |
| B-2026-11-15-47 | mir | low | A `()` passed for a generic parameter that borrows (`fn f[T](v: T)`, `f(())`, or a unit-valued `if` such as `f(if c { () } else { () })`) was invalid… | 92d18d909c |
| B-2026-11-15-48 | typecheck | low | A `collect()` after a misspelt method in its `map` closure reports a second, cascading error naming the poisoned type: `trait bound `C: FromIterator[… | 1bdebaaa70 |
| B-2026-11-15-49 | cli | low | A `match` over an `Option[ref T]` of a `shared` or `Copy` value gets no machine fix for E0516, though the message names the repair and an `if let` or… | 1bdebaaa70 |
| B-2026-11-15-50 | typecheck | low | An `as usize` index over a `u8` takes two fix rounds and keeps its parentheses: `pos[(b - b'a') as usize]` becomes `pos[(b - b'a') as i64]`, then `po… | 88d7d6ee97 |
| B-2026-11-15-51 | mir-llvm | medium | `karac build --target=x86_64-unknown-linux-musl` prints "Built" for a binary that cannot run: the clang --target fallback linked against the host gli… | 109b0d6051 |
| B-2026-11-15-52 | cli | low | `--target=thumbv7em-none-eabihf` fails at link asking for a runtime archive built for that triple, which cannot work (the runtime needs std), and a b… | 109b0d6051 |
| B-2026-11-15-53 | parser | low | `#[cfg(target: "native")]` says "unknown attribute `cfg` .. | 109b0d6051 |
| B-2026-11-15-54 | cli | low | `[build].targets` accepts `wasm_browser` and `gpu` and check passes, while `karac build --target=gpu` says "not in 0.8"; the unknown-target error lis… | 109b0d6051 |
| B-2026-11-15-55 | cli | low | `karac build --help` says under --no-proxy that registry and git dependencies are not in 0.8, so the flag only prints a note; they are fetched (desig… | 109b0d6051 |
| B-2026-11-15-56 | cli | low | After `#[target(native)] fn name` and `#[target(wasm_wasi)] fn name` twins, `karac check` notes "1 item NOT checked — gated away from every checked t… | 109b0d6051 |
| B-2026-11-15-57 | interp | low | An `extern "C"` `cbrt(27.0)` gave 3.0000000000000004 on the MIR interpreter and 3.0 compiled: the interpreter's `dlsym` found glibc's `cbrt`, while e… | 838346a6da |
| B-2026-11-15-58 | resolver | medium | design.md §15's opaque-type example did not compile: `#[kara_name("CFile")] type FILE;` left `CFile` undefined, spelling it `FILE` failed in the MIR… | 838346a6da |
| B-2026-11-15-59 | typecheck | medium | An `extern "C"` signature crossed only integers, `bool`, floats and raw pointers: a `#[repr(C)]` struct by value (glibc's `div` / `ldiv` results), an… | 838346a6da |
| B-2026-11-15-60 | resolver | low | `mem.zeroed()` (design.md §15) did not exist: "undefined name 'mem'" though `mem.replace` resolves, no `Pod` trait to bound it (`#[derive(Pod)]` was… | 838346a6da |
| B-2026-11-15-61 | interp | low | The MIR interpreter refused a pointer to a `#[repr(C)]` struct crossing to C ("a pointer to Agg([...]) cannot cross to C", glibc's `gmtime_r(t, ptr.m… | 838346a6da |
| B-2026-11-15-70 | parser | low | `t += &format!(..)` is fixed to `t += ref f".."`, which is refused (E_REF_OPERAND_UNSUPPORTED): the `&` fix writes a borrow of a temporary | 3e72c72f48 |
| B-2026-11-15-71 | parser | medium | Rust's `impl fmt::Display for T { fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, ..) } }` stops the parse ("Expected `{`, found `::… | 3e72c72f48 |
| B-2026-11-15-72 | typecheck | medium | `Err("unknown fid".into())` is refused ("`.into()` on 'String' cannot pick a conversion: nothing here fixes the target type") at a return, a tail and… | 3e72c72f48 |
| B-2026-11-15-73 | typecheck | low | A `MutexGuard` passed for its value (`sess.handle(mut guard, ..)` for a `mut ref Server`) or read for a field of its value (`srv.lock().counts`) is r… | 3e72c72f48 |
| B-2026-11-15-74 | typecheck | low | Rust's `let (sock, _) = listener.accept().unwrap();` is refused ("tuple pattern used but type is `TcpStream`") with no fix | 3e72c72f48 |
| B-2026-11-15-75 | typecheck+interp+mir-llvm | medium | `Mutex` has no `into_inner` ("no method 'into_inner' on type 'Mutex'"), and the natives both backends kept for it crash the LLVM backend on a struct… | 3e72c72f48 |
| B-2026-11-15-76 | typecheck | low | `self.take(2)?.try_into().unwrap()` for `u16.from_le_bytes(..)` is refused as "no method 'try_into' on type 'Slice'" with nothing about what to write | 3e72c72f48 |
| B-2026-11-15-77 | typecheck+mir | low | No conversion from a `Slice[T]` to an `Array[T, N]` (Rust's `TryFrom<&[T]> for [T; N]`, `b[..4].try_into()`), so reading an integer from a byte slice… | 3751c0d9be |
| B-2026-11-15-78 | mir | medium | `PriorityQueue[Reverse[F64]]` passes `karac check` and then the MIR builder refuses it: `Reverse`'s generic `T.cmp` under `T: Ord` at `T = F64` had n… | ee3c8ec9c4 |
| B-2026-11-15-79 | mir+mir-llvm | medium | `let Reverse { value: top } = heap.pop().unwrap();` over `Reverse[i64]` reads `top` as moved after one use: a field of a generic struct bound by a `l… | ee3c8ec9c4 |
| B-2026-11-15-80 | mir | high | `karac fix` renamed `let mut heap: BinaryHeap<i64> = it.collect();` to a plain `PriorityQueue[i64]`, which pops the smallest first where Rust's heap… | ee3c8ec9c4 |
| B-2026-11-15-81 | mir | low | The fix for reusing a moved value whose type could be `Copy` wrote `#[derive(Copy)]`, which the check then flags at once as deprecated (`Copy` needs… | e6b72f3957 |
| B-2026-11-15-82 | cli | low | `karac doc` rendered a cross-reference to an item that exists but has no doc comment as a link to a page it never writes: `[helper_two]` became `help… | 1887cb8dd9 |
| B-2026-11-15-83 | cli | low | `karac doc` dropped the `///` doc on a trait method's parameter: `Greet.hello`'s `name` doc "who" was missing from Greet.html, while a free function'… | 1887cb8dd9 |
| B-2026-11-15-84 | cli | low | `karac doc` printed a source file's parse errors, rendered the rest anyway and exited 0, so a script or CI step read incomplete docs as a success | 1887cb8dd9 |
| B-2026-11-15-85 | cli | medium | A `[dev-dependencies]` entry that cannot resolve (a registry entry with no proxy, an unreachable git URL, a missing path): with a `_test.kara` presen… | 1887cb8dd9 |
| B-2026-11-15-86 | resolver | high | `karac fix` turns Rust's smallest-first `let mut heap: BinaryHeap<Reverse<i32>> = v.into_iter().map(Reverse).collect();` into a largest-first `Priori… | ed156ca6d4 |
| B-2026-11-15-87 | effect | medium | E0418 between two different channels when one `par` branch both sends on one and receives on another: `par { { txb.send(1) }, { txa.send(2); rxb.recv… | 7a337d9532 |
| B-2026-11-15-88 | typecheck | medium | `isize.abs_diff` is typed `isize`, not `usize`: `o(isize.MIN).abs_diff(o(0))` prints -9223372036854775808 under `karac build` (9223372036854775808 in… | d00b138745 |
| B-2026-11-15-89 | typecheck | low | An unsuffixed integer literal past `i64.MAX` at a `usize` context (`let a: usize = 18446744073709551614`, `o_usize(9223372036854775808)`) is refused… | d00b138745 |
| B-2026-11-15-91 | mir | medium | A plain `break` (or `continue`) inside a labeled block inside a loop passes check, then every lane fails with `invalid MIR: assigning () to _7 of typ… | ecdc3f1527 |
| B-2026-11-15-92 | mir-llvm+runtime | low | A ThreadSanitizer build (`__mir-llvm-tsan`) of a std.regex program fails with a raw `undefined reference to karac_regex_mir` where CLAUDE.md promises… | ecdc3f1527 |
| B-2026-11-15-93 | ownership+mir | medium | A frozen handle cannot match on a `mut` Option field (`match n.next { Some(m) => m.val, None => -1 }` is refused as `cannot be projected through .nex… | ecdc3f1527 |
| B-2026-11-15-94 | ownership | medium | `karac check` passed a use after a move carried by `break 'b`: `eat(d1); if k == 5 { break 'b 50; } d1 = D { n: 2 }; 45` inside `let v = 'b: { . | e7de9f1cb3 |
| B-2026-11-15-95 | ownership | medium | Reassigning a local after a closure took it by move was refused (`let c = \|\| { eat(a); }; a = D { n: 2 }; eat(a); c();` gave "'a' is moved at line 8:… | e7de9f1cb3 |
| B-2026-11-15-96 | typecheck | low | A fixed-length slice arm after an earlier rest arm that covers it gets no unreachable_arm warning (`[_, _]` after `[x, ..]`, `[0]` after `[.., _]`),… | 41bab687c5 |
| B-2026-11-15-97 | typecheck+cli | medium | `[.., 0]` after `[0, ..]` was reported as an unreachable arm though `[1, 0]` reaches it, and `karac fix` deleted the live arm | 41bab687c5 |
| B-2026-11-15-98 | typecheck | low | A trait impl may declare a method the trait lacks, and that method is then callable; a misspelled override silently leaves the trait's default running | 41bab687c5 |
| B-2026-11-15-120 | mir | medium | An iterator chain over an `Array` constant returned from a function (`MX.iter().filter(..).map(\|m\| (m.1, m.2)).collect()`, `let it = NAMES.iter(); it… | 83d072f4fc |
| B-2026-11-15-121 | cli | medium | kara-lsp signatureHelp inside `println(..)` shows another function's signature: with `fn add` first in the file it shows `fn add(a: i64, b: i64) -> i… | 83d072f4fc |
| B-2026-11-15-122 | cli | high | kara-lsp rename of a struct field edits the declaration and struct-literal fields but not member accesses (`p.x`, `f"{helper(p.x)}"`), so the renamed… | 83d072f4fc |
| B-2026-11-15-123 | cli | low | kara-lsp rename accepts a keyword as the new name (`fn` gives an unparseable file) and answers a non-name (`1 bad`) with a null result and no reason;… | 83d072f4fc |
| B-2026-11-15-124 | cli | low | kara-lsp references on a struct name returns a struct literal's whole range (`P { x: helper(1) }`) rather than the name `P`, and the declaration's wh… | 83d072f4fc |
| B-2026-11-15-125 | cli | low | kara-lsp signatureHelp is null for a call inside an f-string interpolation (`f"{helper(p.x)}"`), though hover, definition and references work there | 83d072f4fc |
| B-2026-11-15-126 | mir | medium | A `par` branch or `TaskGroup` task that reads `p.a` and writes `p.b` is charged a mutable borrow of `p.a`: `par { { p.a }, { let t = p.a; p.b = 2; t… | ed156ca6d4 |
| B-2026-11-15-127 | mir | medium | A move inside a `par` branch inside a closure is refused: `run(\|\| { let (x, y) = par { eat(s), 1 }; x + y })` with `run(f: own OnceFn() -> i64)` give… | ed156ca6d4 |
| B-2026-11-15-128 | ownership | low | A move in an earlier `par` branch and a read in a later one is reported as a sequential use after move: `par { { eat(s1) }, { s1.len() } }` gives "va… | ed156ca6d4 |
| B-2026-11-15-129 | mir | high | A trait default method's `self.req()` dispatches to the last impl for every type: with `impl Tr for Pr[i64]`, `impl Tr for Pr[bool]` and `impl Tr for… | 32d67e4304 |
| B-2026-11-15-130 | typecheck | medium | Inside a trait default body, `self.def1()` took the type's inherent `def1` instead of the trait's: a default `def2` calling `self.def1()` on `B` with… | 32d67e4304 |
| B-2026-11-15-131 | cli | low | karac fix is not idempotent on a file that checks clean: manual_compound_assign turns `i = i + 1` into `i += 1`, and counter_while_loop only sees the… | 9452e21278 |
| B-2026-11-15-132 | ownership+cli | medium | karac fix applies two fixes on one span and produces a refused program: `let hop = hs[0].inner.word;` with `word` in a `shared struct` gets both owne… | 9452e21278 |
| B-2026-11-15-133 | typecheck+cli | low | A match on a temporary whose arm yields a `ref` binding gets a fixless typecheck error as a call argument, where the same match in a `let` gets borro… | 9452e21278 |
| B-2026-11-15-134 | runtime | medium | The HTTP client returns Ok for a 3xx it does not follow (300, 304, a 302 with no Location), where io.md makes every status outside 2xx an Err that ke… | 4c174caf42 |
| B-2026-11-15-135 | runtime | medium | Past five redirects the HTTP client's Err drops the response that arrived and names the unfollowed Location instead of the URL asked for | 4c174caf42 |
| B-2026-11-15-136 | runtime | medium | RequestBuilder.timeout(0) or a negative timeout silently means no timeout; io.md now refuses a timeout below 1 ms with an Err from send | 4c174caf42 |
| B-2026-11-15-137 | mir | low | A panic printed a stale error return trace from an `Err` the program had already handled: after `match twice(5) { . | 9107bd0aff |
| B-2026-11-15-138 | cli | low | E_REGISTRY_DEP_UNSUPPORTED's help said registry dependencies are not in 0.8, though they resolve through `KARAC_REGISTRY_PROXY` and are refused only… | 13e7f2c08a |
| B-2026-11-15-150 | typecheck | low | `handle.join().unwrap()` on a `TaskHandle[T]` (Rust's `JoinHandle::join` returns a `Result`) says only "no method 'unwrap' on type 'T'", with no fix | 4dbfebec42 |
| B-2026-11-15-151 | parser | low | `it.collect::<Vec<_>>().join("")` (a turbofish with `_` holes as a further call's receiver) gets no fix, only "bind it first with a typed `let` (`let… | 4dbfebec42 |
| B-2026-11-15-152 | parser | low | `use std::thread;` in a program that spawns becomes `import std.thread;`, "unknown module `std.thread`" with no fix, and once the spawn is moved into… | 4dbfebec42 |
| B-2026-11-15-153 | typecheck | medium | `set_read_timeout(Some(Duration::from_millis(n)))` mends to `Some(Duration.ms(n))`, then "expected 'i64', found 'Duration'" with no fix: Kāra's takes… | 4dbfebec42 |
| B-2026-11-15-154 | mir+ownership | medium | A `filter` predicate's capture is held against the collected result: `self.options.iter().filter(\|(n, _)\| *n == num).map(\|(_, v)\| v).collect()` retur… | 4dbfebec42 |
| B-2026-11-15-155 | typecheck | high | `Some(w)` with `w: u64` passed where an `Option[i64]` is wanted is accepted with no `as`, and `u64.MAX` prints -1; `let x: i64 = w` is refused | 9fb41e9f76 |
| B-2026-11-15-156 | typecheck | low | The checker without the library (bare `crate::typecheck`: the wasm playground's run_static_checks, the REPL's ownership view, the borrowck core-pin c… | 9903db3d4f |
| B-2026-11-15-157 | typecheck | high | `karac check` overflowed its stack on a generic struct literal holding `None` inside `Some`: `let s: Option[G[Option[i64]]] = Some(G { v: None })` wi… | 649b27f77e |
| B-2026-11-15-158 | typecheck | medium | An empty `[]` one level inside a tuple inside `Some`/`Ok`, or inside a nested tuple, was refused with "a type that failed to check is not lowered yet" | 649b27f77e |
| B-2026-11-15-159 | typecheck | low | `.clone()` on `Result[H, String]` with a `#[derive(Clone)]` `H` was "no method clone on type Result", while `Option[H].clone()` and `Result[i64, Stri… | 649b27f77e |
| B-2026-11-16-1 | interp+mir-llvm | low | A derived `Debug` on an enum that also derives `Display(snake_case)` prints the snake_case name (`Dir.BigDown.fmt_debug()` gives `big_down`, not `Big… | 9452e21278 |
| B-2026-11-16-2 | ownership | low | Shadowing a method receiver's name inside its own argument (`m.push({ let mut m: Vec[i64] = Vec.new(); m.push(1); m })`) is refused as a conflicting… | 9452e21278 |
| B-2026-11-16-3 | mir | high | `Vec.filled`, `resize` and `fill` never ran an element type's user `Clone` unless the program wrote `.clone()` somewhere: the copies were field by fi… | f8fb1c6a10 |
| B-2026-11-16-4 | mir | low | `windows(0)` / `chunks(0)` panicked with the message "explicit panic", and a float `Map`/`Set` key's diagnostic suggested `to_bits()` where design.md… | f8fb1c6a10 |
| B-2026-11-16-5 | interp+mir-llvm | low | base64_decode and base64url_decode report more than two trailing `=` (`Z===`, `====`, `Zm9v====`) as InvalidByte at the third `=`, where docs/library… | 9cb3bbd69e |
| B-2026-11-16-6 | resolver | high | In a package, a local binding anywhere in a file with the name of a file-level import erases that import for the whole file: `import a.load as ld;` w… | d1d8bdcc45 |
| B-2026-11-16-170 | parser | low | Rust's `use std::io::{Read, Write};` and `Shutdown` in a `std::net` import mend to imports of names Kāra does not have, with no fix, and `sock.shutdo… | 9cbbd28e56 |
| B-2026-11-16-171 | mir | medium | An array literal of arrays passed as a `Slice[Slice[u8]]` (`pdu([[5], be16(n)])`) passes `karac check` and stops at "invalid MIR: array element of th… | 9cbbd28e56 |
| B-2026-11-16-172 | typecheck+mir | low | Rust's `parts.concat()` over a `Slice[Slice[u8]]` takes the deprecated `Display` join, whose fix writes `join("")`, which does not check (`Slice[u8]`… | 9cbbd28e56 |
| B-2026-11-16-173 | typecheck+mir | low | `concat()` over a sequence of arrays (`Vec[Array[u8, 2]]`) still takes the deprecated `Display` join, where Rust's flattens; remainder of B-2026-11-1… | 5e1014fd7c |
| B-2026-11-16-174 | interp+mir-llvm | low | Json.deserialize reports text that is not JSON with its own message and a position one column past Json.parse's, prefixed with the field before the f… | 3d28b388dc |
| B-2026-11-16-175 | interp+mir-llvm | medium | A derived Deserialize takes the last value of a repeated field (`{"a":1,"a":2}` reads a = 2), so text can pass a validator that read the first copy (… | 3d28b388dc |
| B-2026-11-16-176 | typecheck | low | `v.iter().skip(1).rev()` and `take(n).rev()` are refused: `IterSkip` and `IterTake` were not `DoubleEndedIterator` or `ExactSizeIterator` | 756fad5000 |
| B-2026-11-16-177 | typecheck+mir | low | `let mut cur = head; while let Some(node) = cur { cur = node.next; }` over a borrowed list is refused by the MIR's move check with no fix; the fix is… | 756fad5000 |
| B-2026-11-16-178 | typecheck+mir | medium | `#[derive(Copy, Clone)]` on a struct that holds itself (`next: Option[Node]`) overflows karac's stack, and `karac fix` offers that derive for `Some(n… | 756fad5000 |
| B-2026-11-16-179 | interp+mir-llvm | low | std.path `parent`, `file_name`, `extension` and `stem` read a trailing `.` component as the last component (`"a/."` gave parent `a` and file name Non… | 5ebaabec71 |
| B-2026-11-16-180 | typecheck | medium | A dependency's trait the file never imported still supplied its methods: with import kdep.util.W; alone, W { v: 1 }.show() resolved through kdep's Sh… | ec7adfe00e |
| B-2026-11-16-7 | mir-llvm | low | `(0..n).map(\|_\| . | 817ac3649e |
| B-2026-11-16-8 | mir-llvm | low | `f64.total_cmp` / `f32.total_cmp` is a runtime call (`karac_mir_total_cmp`) per comparison, so a `PriorityQueue[Reverse[F64]]` costs 2.43x checked Ru… | 817ac3649e |
| B-2026-11-16-9 | mir-llvm | low | `"aeiou".contains(c.to_string())` in a loop costs 3.37x checked Rust: the hoisted literal is a zeroed global filled at startup, so its length is unkn… | 817ac3649e |
| B-2026-11-16-10 | mir-llvm | low | `i64.parse(token)` of a word (`"box"`) calls the runtime's parser to learn it is not a number: kata 2042's token scan costs 2.55x checked Rust | 817ac3649e |
| B-2026-11-16-11 | mir-llvm | low | `String.from_utf8([col, row].to_vec()).unwrap()` copies the fresh `Vec` into a new allocation and frees it: kata 2194 costs 1.85x checked Rust | 817ac3649e |
| B-2026-11-16-12 | mir-llvm | low | A `sort_by` comparator `nums[b].cmp(nums[a]).then(a.cmp(b))` branches on `nums[b] > nums[a]` inside the merge: kata 2099 runs 2.2x checked Rust's wal… | 817ac3649e |
| B-2026-11-16-181 | typecheck | medium | An array literal a `map` / `filter_map` closure returns does not follow the element the `collect` names: `d.iter().map(\|e\| [e[0], e[1], 1 - e[2]]).co… | 7a81dbfcf1 |
| B-2026-11-16-182 | typecheck | medium | A `par` block written as statements (`par { a(); b(); }`, `par { let _a = read(c); let _b = read(c); }`) is refused (core-semantics §11.1, E0520) wit… | 7a81dbfcf1 |
| B-2026-11-16-183 | typecheck | medium | A `let` that keeps a view of a temporary (`let req: Vec[Str] = line.substring(a, b).split(" ").collect();`, core-semantics §5.5) is refused with "Bin… | 7a81dbfcf1 |
| B-2026-11-16-184 | typecheck | low | The legacy `ExitStatus.code` field (`if st.code == 0`) is refused with "`code` is a method now .. | 7a81dbfcf1 |
| B-2026-11-16-40 | typecheck | low | `flatten` over `Option` or `Result` items hints, and `karac fix` writes, `.flat_map(\|v\| v.into_iter())` / `.flat_map(\|v\| v.iter())`, but neither type… | 7040092362 |
| B-2026-11-16-41 | mir+interp+mir-llvm | low | `step_by` with a step of 0 or below stepped by 1 going forward, but `rev` panicked "division by zero" (step 0) or stepped by the magnitude (`(0..5).s… | 2ba6f53155 |
| B-2026-11-16-42 | mir | medium | A closure argument's captures were held by any call result or `mut ref` pointee that could hold a reference: `let f = it.find(\|z\| *z == t); t = 3; us… | 31d41b74ba |
| B-2026-11-16-43 | interp+mir-llvm | low | `u8.parse("-0")` (any unsigned type, and `from_str_radix`) was `Err(InvalidDigit)`; review ruled minus zero is zero, so it is `Ok(0)`, as `i64.parse(… | 331a01c410 |
| B-2026-11-16-44 | interp+mir-llvm | low | `char.from_digit`'s panic for a radix outside 2..=36 said "the radix is not in 2..=36" without the radix, where `to_digit` and `is_digit` print it ("… | 331a01c410 |
| B-2026-11-16-185 | parser | medium | `karac fix` carries Rust's `BinaryHeap<Reverse<usize>>`, read through `while let Some(&Reverse(i)) = heap.peek()`, to a smallest-first `PriorityQueue… | 3091098c58 |
| B-2026-11-16-45 | cli | low | `karac test` emits `test_fail` without the required `location` field when a test fails before its body runs: `provider_construction_failed` (a `#[wit… | 3b3777c885 |
| B-2026-11-16-46 | cli | medium | A successful `karac build --output=jsonl` prints one untyped `{"status":"ok","output":...}` line instead of the event stream (no `build_start`, phase… | 70c9592fce |
| B-2026-11-16-186 | typecheck | low | `karac fix` never rewrote the legacy `Stdio.Piped` / `Stdio.Null` / `Stdio.Inherit` to the constructors that docs/library/io.md:581 says it does: on… | e0d36b037a |
| B-2026-11-16-187 | resolver | low | A type that a gated library module defines, named without its import (`Pool[Conn, E]`, `PoolError`), got no import fix | e0d36b037a |
| B-2026-11-16-188 | ownership | low | An escaping closure that captures a bare parameter (`fn make(p: String) -> Fn(i64) -> i64 { \|n\| p.len() + n }`) is refused with "take `p` by value (`… | e0d36b037a |
| B-2026-11-16-189 | typecheck | low | `assert_eq` / `assert_ne` on the program's own struct or enum without `Debug` says to add `#[derive(Debug)]` but carried no fix | e0d36b037a |
| B-2026-11-16-190 | resolver | medium | Two imports of one gated library module (`import std.pool.{Pool};` then `import std.pool.{PoolError};`) are refused with "conflicting impl: `PoolErro… | e0d36b037a |
| B-2026-11-16-191 | resolver | medium | An undefined type was machine-renamed to a primitive or a value binding: `Pool[Conn]` became `bool[Conn]`, and in the same program `pool[Conn]` (the… | e0d36b037a |
| B-2026-11-16-192 | mir-llvm | medium | Dropping a `Secret[String]` frees its buffer with the secret's bytes still in it: the zeroize-on-drop that secret.md and std/secret.kara describe was… | 4ade899ce2 |
| B-2026-11-16-193 | interp | medium | The MIR interpreter loses every std.log record from TaskGroup tasks spawned inside auto-parallelized statements, and reports their call values leaked… | 4ade899ce2 |
| B-2026-11-16-194 | interp | low | In the MIR interpreter a std.log call made inside a custom Backend's own `emit` goes back into that backend instead of to the default JSON backend on… | 4ade899ce2 |
| B-2026-11-16-195 | interp+mir-llvm | medium | A loan released into a different pool (`p.release(conn_from_q)`) is dropped but still counts toward its own pool's max_connections, so that pool lose… | 4ade899ce2 |
| B-2026-11-16-196 | typecheck+mir | low | std.pool stored a non-escaping function value: Pool.new's create_fn and with_health_check's check were declared `own Fn` though the pool keeps them,… | 8e3feda3dc |
| B-2026-11-16-197 | mir-llvm | low | `karac build --target=wasm_wasi` of a program that uses std.crypto (sha256, hmac_sha256, constant_time_eq) stops at wasm-ld with `undefined symbol: k… | 8e3feda3dc |
| B-2026-11-16-198 | cli | medium | karac run --manifest <path> file.kara read the manifest but gave the file none of its dependencies: import dep.hello; was an unknown module, against… | 4de4c4d06c |
| B-2026-11-16-199 | cli | low | karac build --target-cpu=native was refused as an unknown CPU though design.md § CPU baseline offers it, and --target-cpu=help printed its header twi… | 4de4c4d06c |
| B-2026-11-16-200 | resolver | low | An import of a module that only other platforms' files define (poller_macos.kara alone, on Linux) was a bare E0112 unknown module, without design.md… | 4de4c4d06c |
| B-2026-11-16-201 | mir-llvm | high | A plain karac build targets x86-64-v3 (AVX2) on x86_64 and +v8.4a on aarch64 Linux, against design.md § CPU baseline's table (x86-64; generic +v8a,+o… | 40435b5a79 |
| B-2026-11-16-202 | interp+mir-llvm | low | A channel deadlock is not a panic record: "error: deadlock: ..." carries no file:line:col, and karac run --output=json emits no record with kind dead… | 511801808d |
| B-2026-11-16-203 | typecheck | medium | The pre-v1 `r.read_to_string().unwrap_or(x)` on a child's pipe left "expects 1 argument(s), found 0" with no machine fix, plus an `i64` cascade (`no… | fa56486e69 |
| B-2026-11-16-204 | typecheck | low | The pre-v1 `ws.recv_text(mut buf)` said only "no method 'recv_text' on type 'WebSocket'", with nothing naming `recv()` and `WsMessage` | fa56486e69 |
| B-2026-11-16-205 | typecheck | low | `karac fix` deletes `as u64` from `ans * i as u64` over a `usize` `i` (lossless_cast) in the same round that makes `i` an `i64` (`n as usize` to `n a… | 13f42dda1f |
| B-2026-11-16-47 | mir | low | Every panic printed an `Error return trace:` holding only the panic's own place, though no `?` propagated an error, on the interpreter and compiled (… | 96de92b3d3 |
| B-2026-11-16-48 | cli | low | `karac check`/`build --output=json` write a resource-less effect as `panics()` in `program_effects` and `public_function_effects`, a spelling the lan… | fbd0d70d4e |
| B-2026-11-16-49 | cli | low | A clean `karac build --output=json` success object has no `diagnostics` key, where design.md § 17's document always carries the array (empty on a cle… | 9d3350fc6e |
| B-2026-11-16-206 | typecheck | low | Giving away a part of a bare parameter dead-ended: `let job = o.job;` out of a bare `o`, then `self.dead.push(job)` | 3761b2f019 |
| B-2026-11-16-207 | typecheck | low | A closure that returns a bare parameter, called where an owned value is wanted (`fn f(w: W) -> W { let g = \|\| w; g() }`), is refused with "expected '… | 3761b2f019 |
| B-2026-11-16-208 | mir-llvm | medium | A compiled program wrote the KARAC_ANNOUNCE_PORT line `BOUND_PORT=<n>` to stderr while its stdout still held what it had printed, so a harness that w… | 8a9607ce78 |
| B-2026-11-16-209 | interp+mir-llvm | medium | `BufReader.lines()` returned the same `Err` for ever from a stream that keeps failing, so a `for` that only reported the error never ended; review-th… | 8a9607ce78 |
| B-2026-11-16-210 | interp+mir-llvm | medium | The HTTP server kept an HTTP/1.0 connection open for `Connection: keep-alive` but its response carried no `connection: keep-alive`, so a 1.0 client r… | 8a9607ce78 |
| B-2026-11-16-211 | typecheck | medium | A closure whose body is a join of literals (`\|s: i32\| if s < lower { -1 } else if s > upper { 1 } else { 0 }`) is typed `i64` where it is written, so… | a3ae3b2036 |
| B-2026-11-16-212 | typecheck | medium | A comparison of a signed and an unsigned integer is refused even when one side holds every value of the other (`count_ones() / 2 <= k` with `k: u32`,… | a3ae3b2036 |
| B-2026-11-16-213 | cli | low | `karac explain` / `query effects` and the impl-ceiling and public-declaration diagnostics write a resource-less effect as `blocks()` / `suspends()` | f5ebb66465 |
| B-2026-11-16-214 | cli | low | A derivation step into a library function places the library's line in the user's file (`f10.kara:180 sleep declares blocks`) | f5ebb66465 |
| B-2026-11-16-215 | cli | low | A derivation names the checker's internal keys: `mk calls __builtin_heap_alloc`, `ovf calls __builtin_int_overflow` | f5ebb66465 |
| B-2026-11-16-216 | cli | low | A derivation through a type-parameter bound names `__bound_ceiling.Store.load` | f5ebb66465 |
| B-2026-11-16-217 | mir | low | `dbg` in a `par` branch carries no task id, and `KARAC_DBG_OUTPUT=json` / `karac run --output=json` leave `dbg` in the terminal format, on every lane | f5ebb66465 |
| B-2026-11-16-218 | cli | medium | karac build had no -O0..-O3 flags though design.md § 16 karac build documents them, and KARAC_OPT_LEVEL outside 0-3 (s, z, 9) silently built at -O2 | e0cefb3802 |
| B-2026-11-16-219 | cli | medium | A package's [build] targets did not choose what a plain karac build builds, and karac run ran a package whose targets leave out native | e0cefb3802 |
| B-2026-11-16-220 | cli | low | [package].version accepted any string ("one", "1.0") though design.md § 4 makes it a semver version | e0cefb3802 |
| B-2026-11-16-221 | cli | low | Text diagnostics named the phase (error[typecheck], error[effect], error[ownership]) where --output=json reports the catalogue code (E0221, E0400, E0… | e0cefb3802 |
| B-2026-11-16-223 | mir | low | An error's `?` frames outlive the error value: an `Err` that is only printed and dropped leaves its frame in a later trace (review-thread ruling 2026… | 7fc87dc20e |
| B-2026-11-16-224 | cli | low | karac exits 101 with a panic message when the reader of its stdout closes (`karac check --output=json f.kara \| head -c 1`) | b921a4e5a2 |
| B-2026-11-16-225 | mir-llvm | low | Compiled lanes number `par` branches' task ids from 2 (`std.log` `task_id`, dbg `task:N`); the interpreter from 1 | b921a4e5a2 |
| B-2026-11-16-226 | effect | low | A call through a type-parameter bound is charged the instance's impl besides the trait's clause: `get[T: Store]` gets `panics` from `A.load` | b921a4e5a2 |
| B-2026-11-16-227 | cli | low | An empty function-type slot is written `[pure]` in its diagnostic; the source writes `with ()` | b921a4e5a2 |
| B-2026-11-16-228 | typecheck | medium | A literal in a `ref T` slot whose `T` is still open (`*count.get(k).unwrap_or(0)` over an unannotated `Map` counted with `*count.entry(m).or_insert(0… | 26ae8bff93 |
| B-2026-11-16-229 | typecheck | low | An open `Vec.new()` / `Vec.with_capacity(..)` / `VecDeque` binding whose element type its pushes fixed to `i64`, refused at a use that wants `Vec[i32… | 26ae8bff93 |
| B-2026-11-16-230 | typecheck | medium | A literal assigned through `*` to an open `T` (`*count.entry(m).or_insert(0) = 1`, or `let r = m.entry(k).or_insert(0); *r = 5`) fixes the map's open… | 5ed35584c3 |
| B-2026-11-16-231 | mir | medium | A closure passed to `with_span(span, body)` captured its locals by move in MIR though the parameter is `own OnceFn() -> T`, not `escaping`: `karac ch… | 2b5fa50f3f |
| B-2026-11-16-232 | interp | low | A function whose declared return type names its `map` adapter's closure as `Fn(i64) -> i64` over a `VecIter[i64]` crashes the MIR interpreter (`deref… | 492130ffba |
| B-2026-11-16-233 | mir+mir-llvm+interp | high | A C-like enum declared `#[repr(intN)]` or `#[repr(C)]` was one byte holding its variant index: a `#[repr(C)]` struct holding one was laid out unlike… | 2b5fa50f3f |
| B-2026-11-16-234 | typecheck | medium | An `unsafe { }` block did not pass its `let`'s annotation to its tail, so design.md §15's own `let recovered: *const u32 = unsafe { ptr.from_exposed(… | 2b5fa50f3f |
| B-2026-11-16-235 | ownership | medium | `ptr.mut(x)` of a `let` without `mut` passed check, so a write through the pointer changed a binding declared immutable (it printed 9 for `let x = 3`… | 2b5fa50f3f |
| B-2026-11-16-236 | cli | low | The `undocumented_unsafe` lint accepted a bare `// Safety:` with nothing after the colon, where design.md §15 requires at least one word | 2b5fa50f3f |
| B-2026-11-16-237 | typecheck | low | `E.try_from(9)` with an unsuffixed literal was refused as `expects its #[repr] type i32, got i64`, so design.md §15's TryFrom could be called only on… | 2b5fa50f3f |
| B-2026-11-16-238 | interp | low | The MIR interpreter passed C one element of an array of `#[repr(C)]` structs (`s1_arr_read(ptr.const(arr[0]), 3)`), so C indexing past the pointer re… | 2b5fa50f3f |
| B-2026-11-16-239 | resolver | low | Two `unsafe extern` blocks declaring the same opaque type are a hard `already defined` error, where design.md §15 makes the duplicate `warning[W_OPAQ… | 492130ffba |
| B-2026-11-16-240 | interp | low | `ptr.addr` / `ptr.expose` of a pointer to a Kāra local stop the MIR interpreter (`the address of a value the interpreter holds is not known here`), s… | 492130ffba |
| B-2026-11-16-241 | mir-llvm | low | A read-only walk over a `shared` value's `mut` vector field takes and gives back the field's borrow flag at every node, through the runtime's flag ta… | d4f87d8b6f |
| B-2026-11-16-242 | mir-llvm | low | A handle bound out of a borrowed node's `mut` field (`match n.left { Some(c) => . | d4f87d8b6f |
| B-2026-11-16-243 | mir-llvm | low | A hashed `Map` or `Set` that removes about as often as it inserts (a sliding window, a toggle set) pays the ordered table's holes, compaction and ind… | f642d87dcd |
| B-2026-11-16-244 | typecheck | medium | A range pattern's bounds are not checked against the scrutinee's type: `'a'..='z'` on an i64 is accepted and matches 100 | 62faab9326 |
| B-2026-11-16-245 | typecheck | low | An irrefutable `if let` or `while let` pattern gives no warning; design.md § Pattern exhaustiveness says it is one | 62faab9326 |
| B-2026-11-16-246 | parser | medium | Comparisons chain: `a == false == false` and `1 < 2 == true` are accepted; design.md § Operators and precedence says they do not chain | 62faab9326 |
| B-2026-11-16-247 | typecheck | low | `discriminant()` of a bare `#[repr(C)]` enum returned `u32`, so `Red = -1` read 4294967295 and would not bind to an `i32`, where a `#[repr(C)]` enum… | 492130ffba |
| B-2026-11-16-248 | interp | low | `ptr.const(f)` of `f: ref Foo`, a reference to an opaque C object made from a `*mut Foo`, stopped the MIR interpreter (`deref of non-reference`) whil… | 492130ffba |
| B-2026-11-16-249 | mir+mir-llvm+interp | low | `char` could not cross an `extern "C"` signature (`E0516 .. | 492130ffba |
| B-2026-11-16-250 | typecheck | low | `partition` over an array literal's `iter()` into `(Vec[i64], Vec[i64])` (`let v = [1, 2, 3]; let (e, o): (Vec[i64], Vec[i64]) = v.iter().partition(.… | 84c49b5431 |
| B-2026-11-16-251 | typecheck | low | Text where bytes are wanted (`fn f(b: Slice[u8])` called with a `String` or `Str`, a child process's `stdin.write(text)`) is refused with "expected '… | 84c49b5431 |
| B-2026-11-16-252 | typecheck+cli | medium | A `par` block written as statements whose tail computes from its `let`s (`let total = par { let a = f(); let b = g(); a + b };`) is refused (core-sem… | 84c49b5431 |
| B-2026-11-16-253 | interp | medium | On AArch64 (macOS CI) the MIR interpreter refused an `extern "C"` call whose arguments fill more than six integer registers ("8 integer and 0 float a… | 54a14fdd6b |
| B-2026-11-16-254 | typecheck | medium | `drop(x)` of a captured outer value inside a `par` branch is refused as a move out of a borrowed place, while passing it to an `own` parameter or mov… | a04638fd9e |
| B-2026-11-16-259 | typecheck | low | [doc-only] `b as char` with `b: u8` is accepted and gives the character, while design.md §5's cast table rejected every integer to char cast and book… | af808ebfb9 |
| B-2026-11-16-263 | other | medium | [doc-only] Book ch01, ch03 and appendix A still teach pre-D5 parameter modes: `name: String` 'owned by default', `karac explain greet` suggesting `re… | af808ebfb9 |
| B-2026-11-16-264 | other | medium | [doc-only] Book ch04, ch10 and appendix B teach a pipe operator `\|>`, which design.md line 369 says does not exist (the compiler refuses it with E052… | af808ebfb9 |
| B-2026-11-16-265 | other | medium | [doc-only] Book ch07 teaches optional chaining `user.address?.city?.name` that 'short-circuits to None', which design.md line 369 rules out (no `?.`)… | af808ebfb9 |
| B-2026-11-16-266 | other | medium | [doc-only] Book examples that no longer compile: ch06 `get_name(user: Option[User])` moves `name` out of a borrowed parameter (E0200), ch08 `sort[i32… | af808ebfb9 |
| B-2026-11-16-267 | other | low | [doc-only] Book ch02, ch09b and ch13 name the borrowed string type `StringSlice` (design.md has only `Str`), and ch02 calls `i64 as i32` 'widening —… | af808ebfb9 |
| B-2026-11-16-268 | other | medium | [doc-only] Book ch14 and ch20 describe par fail-fast cancellation and a crash JSON file (KARA_CRASH_DIR, `karac debug <file>`, 'panic while unwinding… | af808ebfb9 |
| B-2026-11-16-269 | other | low | [doc-only] Book ch18 says `#[repr(C)]` structs do not cross an import yet, but a libc `div` returning `#[repr(C)] struct DivT { quot: i32, rem: i32 }… | af808ebfb9 |
| B-2026-11-16-270 | other | low | [doc-only] Appendix A and C are stale: `use` listed as a keyword (it is not one), `dyn` 'not yet implemented' (it works), `defer` '(success path)' (i… | af808ebfb9 |
| B-2026-11-16-271 | other | low | [doc-only] Book code blocks that emit warnings on `karac check`: undocumented_unsafe (appendix D lines 63 and 114), redundant_suffix (ch02 line 79 `0… | af808ebfb9 |
| B-2026-11-16-274 | autopar | medium | Auto-par refuses to run parallax_lite's three provider writes (`record_a(); record_b(); record_c();`, writes(MetricsA/B/C)) as tasks, calling the pro… | ec237045da |
| B-2026-11-16-276 | other | low | [doc-only] README's `karac fix archive.kara` transcript says 'applied 1 fix(es)', but the fix applies 3 (the example's two `.to_string()` calls are r… | 614211f7ff |
| B-2026-11-16-277 | other | medium | [doc-only] README's Performance section is stale against the kara-katas feed it cites: it says 444 programs, 21 over 2x Rust and worst #163 at 10.2x;… | 614211f7ff |
| B-2026-11-16-278 | other | medium | [doc-only] Eight examples a visitor will try do not build on 0.8 and their READMEs do not say so: interop/ (exported C ABI), fathom, plume, iris and… | 614211f7ff |
| B-2026-11-16-279 | other | medium | [doc-only] The parallelism examples' READMEs promise speedups that do not reproduce: phase0's bench.sh times two hand-written Rust files yet prints '… | 614211f7ff |
| B-2026-11-16-280 | other | low | [doc-only] Smaller example README drift: tangle says `Editor.set`'s `cell` reports `mut_ref` (it reports `own`); interop-zlib says `./zpipe` (the bin… | 614211f7ff |
| B-2026-11-16-281 | other | low | [doc-only] First-run docs gaps: the book has no install section (ch00/ch01 never say how to get karac), the release tarball README lists `--target=wa… | 614211f7ff |
| B-2026-11-16-304 | typecheck | low | An irrefutable `let .. | b31d77e15a |
| B-2026-11-16-305 | typecheck | low | `let r = ref make_value()` says 'only `ref <sequence>[i]` is supported in 0.8' instead of the temporary-borrow error and its fix | b31d77e15a |
| B-2026-11-16-306 | typecheck | low | An empty `match e { }` on an enum says 'missing variants: _' instead of naming the variants | b31d77e15a |
| B-2026-11-16-307 | typecheck | low | `a, b = (1, 2, 3)` adds a spurious E0222 'refutable pattern in `let` binding' next to the arity error | b31d77e15a |
| B-2026-11-16-308 | parser | low | A float range pattern `0.0..=1.0` is a parse error at `..=`, not the spec's 'float ranges are not supported' | b31d77e15a |
| B-2026-11-16-309 | typecheck | low | A default naming an earlier parameter gets a duplicate 'not a known const' error, or only 'undefined name' once the function is called | b31d77e15a |
| B-2026-11-16-325 | other | low | [doc-only] core-types.md § Parsing does not say which spellings of the special float values `f64.parse` accepts; every lane accepts `inf`, `-inf`, `i… | ada66fb83f |
| B-2026-11-16-272 | mir | low | When a `par` branch or `par for` iteration leaves early (`?`, `return`), the block's other values dropped in source order, where core-semantics §11.7… | a04638fd9e |
| B-2026-11-16-326 | mir | high | `<` on a type whose derived ordering reaches itself (`#[derive(PartialEq, PartialOrd)] enum Tree { Leaf(i64), Node(Vec[Tree]) }`) overflows the stack… | a8c99e46a9 |
| B-2026-11-16-327 | mir | high | A NaN pair that decides a derived ordering makes `<=` and `>=` true: `P { x: NaN } <= P { x: 1.0 }`, `(NaN, 1) >= (1.0, 1)` and `[NaN] >= [1.0]` all… | a8c99e46a9 |
| B-2026-11-16-328 | typecheck | medium | `fn f() -> impl Iterator[Item = i64] { 0..3 }` is refused with `body type Range[i64] declares Item = i64` (E_IMPL_TRAIT_ASSOC_MISMATCH): a witness wh… | a8c99e46a9 |
| B-2026-11-16-329 | typecheck | high | `-> impl Iterator[Item = u8]` accepts a witness whose `Item` is `i64` (`let v: Vec[i64] = [1]; v.into_iter()`): the binding check takes any two integ… | a8c99e46a9 |
| B-2026-11-16-330 | typecheck | medium | A `#[derive(Clone)]`, `#[derive(Debug)]` or `#[derive(Display)]` whose field type lacks the trait is accepted silently, and the field is cloned or re… | a8c99e46a9 |
| B-2026-11-16-331 | typecheck | low | `Make.name()` for a trait's associated function names no type, and `karac check` prints MIR's internal `a type that failed to check is not lowered ye… | a8c99e46a9 |
| B-2026-11-16-332 | typecheck | low | One finding printed several times: a marker trait's body item reports `marker traits cannot have a body` at every token after the first, `fn f(x: dyn… | a8c99e46a9 |
| B-2026-11-16-333 | typecheck | low | A type parameter that appears in no parameter type and not in the return type (`fn who[T: Named]() -> String`) is accepted at the declaration althoug… | a8c99e46a9 |
| B-2026-11-16-334 | typecheck | low | `dbg(e)` on a standard-library error is refused by `karac check` (E0232, not `Debug`) for 15 error types, among them `DeserializeError`, `JsonError`,… | a8c99e46a9 |
| B-2026-11-16-336 | parser | medium | The lexer refuses escapes design.md §3 lists: `\'` in a string, f-string, `"""` string or C-string (`Unknown escape sequence: \'`), and `\"` in a cha… | cd7ef684f9 |
| B-2026-11-16-337 | parser | low | Combined string prefixes (`rf"…"`, `br"…"`, `cr"…"`, …) lex as an identifier and a string, where design.md §3 says the lexer reports them with a focu… | cd7ef684f9 |
| B-2026-11-16-338 | parser | low | A token that spans lines is reported at the line it ENDS on with the column it starts at: an unterminated literal points past the end of the file and… | cd7ef684f9 |
| B-2026-11-16-339 | parser | medium | A float literal past its type's range is accepted as infinity: `1e400` is `inf`, and `1e39f32` (or `65520.0f16`) is an f32 (f16) infinity, where desi… | cd7ef684f9 |
| B-2026-11-16-342 | parser | medium | One mistake, several errors or the wrong one: a missing `=>` in a match arm, an `else` with no `if`, a block missing its `}` before the next item, an… | cd7ef684f9 |
| B-2026-11-16-343 | resolver | low | The case-class rename advice can name a keyword: `let r#Async = 1;` is told `consider renaming to `async``, which is reserved, where it must be `r#as… | cd7ef684f9 |
| B-2026-11-16-344 | cli | medium | `karac fmt` drops an FFI export's ABI: `extern "C" fn add_one(x: i32) -> i32` comes back as a plain `fn`, so the function loses its external symbol | cd7ef684f9 |
| B-2026-11-16-345 | cli | medium | `karac fmt` drops an effect's partition key: `with writes(Db[id])` comes back as `with writes(Db)`, widening the effect to the whole resource and ser… | cd7ef684f9 |
| B-2026-11-16-346 | cli | medium | `karac fmt` drops a loop's attributes: `#[par_order_free]` before a `while`, `for` or `loop` disappears, changing what the concurrency analysis may d… | cd7ef684f9 |
| B-2026-11-16-347 | cli | low | `karac fmt` rewrites a C-string's non-ASCII text as byte escapes: `c"héllo"` becomes `c"h\xc3\xa9llo"` and `c"k\u{101}ra"` becomes `c"k\xc4\x81ra"` | cd7ef684f9 |
| B-2026-11-17-5 | other | low | [doc-only] collections.md and iterators.md leave negative counts unspecified: Vec/VecDeque.truncate(-1) clears, take(-1) is empty, skip(-1) yields al… | 038ed5c65d |
| B-2026-11-17-10 | other | low | [doc-only] io.md says read_lines "drops a trailing \r from each line", but a final line "a\r" (and a lone "\r") keeps it on all three lanes, as Rust'… | bf357e8030 |
| B-2026-11-17-34 | typecheck | low | A chained pipe `4 \|> f \|> g` reports E0520 'there is no pipe operator' twice, once per `\|>`, where one error for the chain is enough | 614211f7ff |

</details>

<!-- BUG-LEDGER:GENERATED:END -->
