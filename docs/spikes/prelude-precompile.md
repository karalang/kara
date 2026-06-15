# Spike: build-time prelude precompilation (karac startup cost)

**Status:** design + decomposition done 2026-06-14; implementation not started.
Origin: B-2026-06-09-2 close-out surfaced that every `karac` invocation
re-parses + re-registers the 5,271-line baked stdlib prelude. See
[[project_prelude_startup_cost]] (user memory) and `phase-4-interpreter.md`
B-2026-06-09-2 entry. User authorized "build-time precompile" 2026-06-14.

## Problem

`runtime/stdlib/*.kara` (5,271 lines, ~80 files) is `include_str!`'d into karac
(`src/prelude.rs` `STDLIB_SOURCES`) and **parsed + signature/type-registered into
the typechecker env on every invocation** (`run`/`build`/`check`). Stdlib fn
*bodies* are NOT re-body-checked (`items.rs::check_items` is user-items-only), so
the cost is parse + registration, not body verification.

## Decomposition (warm, no-llvm release, `/usr/bin/time -l` instructions-retired)

Discard the first run of a fresh binary — it is cold-dyld inflated ~+10 M (see
[[feedback_perf_bisect_use_instruction_count]]). Warm:

| stage | instructions | notes |
|---|---|---|
| `karac --version` (pure process startup) | **20.9 M** | binary load / dyld / init — NOT prelude; a separate lever |
| `karac check empty.kara` | 53.9 M | + full front-end incl. prelude register |
| `karac run empty.kara` | 59.8 M | + lower + interp of empty `main` |

→ `run empty` ≈ **20.9 M startup + ~33 M front-end + ~6 M lower/interp**.
Instrumented sub-split of the front-end (wall, warm): `parse_stdlib` 2.4 ms,
`register_baked` 1.1 ms, `register_intrinsic` 0.17 ms (~10–15 M combined). The
remaining front-end (~18–23 M) is resolve (prelude names → scope-0),
`STDLIB_VARIANCE` LazyLock walk, the other `STDLIB_PROGRAMS` walks (items.rs:788,
expr_method_call), and the empty program's own typecheck/effect/ownership passes.

## What precompile CAN and CANNOT recover

- **Recoverable (~20–25 M):** parse + `register_baked_stdlib` + variance +
  resolve-prelude-names — i.e. the per-invocation prelude processing.
- **NOT recoverable by this work:** the ~20.9 M process-startup floor (binary
  load — attack separately if it matters: profile `--version`; suspects are a
  heavy arg parser or a stray eager init), and the ~6 M lower/interp.

So for a **CLI invocation** the win is ~40% of `run empty` (60 M → ~38 M), with
the 20.9 M startup floor remaining. For the **`cargo test` suite** the win is
larger in aggregate: tests run in-process, so `parse_stdlib` is LazyLock-shared
once per test binary, but `register_baked_stdlib` (+ resolve) runs **per
TypeChecker instance = per test** (~5 M × thousands of tests). Process startup is
amortized once per test binary. Precompile mainly helps the test suite + short
CLI scripts; production AOT binaries are entirely unaffected (this is
compile-time only).

**Honest verdict:** real but partial win, multi-day invasive. Worth it primarily
for test-suite iteration speed. If the goal is raw CLI startup, the 20.9 M
process-startup floor may be the cheaper lever to attack first.

## Architecture (recommended): bake the registered env at build time

The stdlib is baked into karac via `include_str!`, so the precompiled artifact
can ship *with its matching stdlib* — no disk-cache invalidation. A `build.rs`
runs the front-end on `STDLIB_SOURCES` at karac-build time, serializes the
registered-env state, and bakes the blob into the binary; runtime deserializes
into a fresh `TypeChecker` env instead of re-parsing+registering.

What to serialize (the register_baked_stdlib output): registered structs / enums
/ traits / impls / fn signatures, the `STDLIB_VARIANCE` table, `compiler_builtins`
set, impl-assoc-type maps, prelude name lists (resolver scope-0). NOT bodies.

**The hard part:** `Serialize`/`Deserialize` on `Type` / `Symbol` / `SymbolId` /
the env `HashMap`s / the AST nodes they reference — recursion (`Box<Type>`),
interning (SymbolId tables), and `Span`s. This is the multi-day, invasive surface.

## Incremental slices (each committable + verifiable)

1. **Serde foundation — AST + Type.** Derive/impl `serde` on `Type`, `TypeExpr`,
   the `Item`/`Expr`/`Pattern` AST, `Span`, `Symbol`/`SymbolId`. Verify:
   round-trip a parsed `STDLIB_PROGRAMS` entry (serialize→deserialize→`assert_eq`).
   No behavior change yet. (Largest slice.)
2. **Serde the registered env.** Make `register_baked_stdlib`'s output a
   serializable struct (or impl serde on the relevant `Env` fields). Verify:
   build the env two ways (live register vs serialize→deserialize) and assert the
   typecheck results on a corpus sample are byte-identical.
3. **build.rs bake.** `build.rs` runs slices-1/2 on `STDLIB_SOURCES`, writes the
   blob to `OUT_DIR`, `include_bytes!`'d. Gate behind a cargo feature initially.
4. **Runtime wire-up + flip.** `register_baked_stdlib` deserializes the blob
   instead of walking `STDLIB_PROGRAMS`. Keep the live path behind a fallback
   env var for A/B. Verify: full `--features llvm` suite byte-identical + the
   decomposition re-measured (expect `check empty` ≈ 53.9 → ~33 M).
5. **Cleanup + re-bench.** Remove the live-path scaffolding once stable;
   re-measure CLI + test-suite wall time.

## Verification strategy (correctness is paramount)

The deserialized env MUST be observationally identical to the live-registered
env. Gate: (a) round-trip equality on every `STDLIB_PROGRAMS` entry; (b) a
differential pass — typecheck the whole kata corpus + examples + tests under
both env-build paths, asserting identical diagnostics + identical
`expr_types`/`method_callee_types`/etc. for every program; (c) the full
`--features llvm` suite green under the precompiled path. A mismatch = a serde
bug, never ship.

## Cheaper alternative levers (tracked, not this spike)

- **Process-startup 20.9 M — PROFILED 2026-06-14, IRREDUCIBLE (not a cheap win).**
  Instrumented `main`: in-`main` work (16 MB-stack thread spawn + `parse_args` +
  `execute --version`) is only ~76 µs ≈ **0.2 M** instructions. The other ~21 M
  is **pre-`main`** — dyld + libstd init. A trivial Rust binary
  (`fn main(){println!()}`) already costs **14.6 M** on this macOS box (the
  platform Rust-startup floor every binary pays); karac (8.8 MB binary) adds only
  ~6.7 M on top (binary-size relocations/page-in). No `__mod_init_func`
  load-time initializers, no `ctor`. So there is NO stray-init to remove; the only
  sub-lever is shrinking the 8.8 MB binary (strip/LTO/fewer deps) for the ~6.7 M
  marginal — marginal payoff, separate effort. **Conclusion: the ~21 M floor is
  effectively fixed; precompile is the only lever for the recoverable front-end**
  (`run empty` 60 M → ~38 M, the 38 M being mostly the irreducible 21 M floor +
  ~6 M interp + residual front-end).
- **`expr_method_call.rs:100`:** re-walks all `STDLIB_PROGRAMS` per `T: Bound`
  method dispatch (O(calls × stdlib)). Cache into a `LazyLock<HashMap<(trait,
  method), &Method>>`. Narrow (only generic-bounded dispatch) but trivial + safe.
