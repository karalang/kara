# Slice 6 (strip run-leniency) — blast-radius sweep + strip

**Status:** sweep COMPLETE across all three mandated corpora (`examples/` +
`examples/mend` + `kara-katas`), 0 real breaks after fixes. **Correctness
leniency STRIPPED (6a) — landed on `main` 2026-07-08.** `karac run` now rejects
the same type/effect correctness violations `check`/`build` reject; target-gate
(E0411) portability findings stay lenient. The second leg (**6b: route `karac
run` → JIT**) is separate and tracked in the spike.

## What the strip would do

`karac run`'s lenient script path downgrades most static-contract violations
(type, effect) to `warning[...]` and executes anyway; only a narrow allowlist
(`TypeErrorKind::is_run_fatal` — invalid-cast, string-index, shared-field-mut,
atomic-missing-ordering, impl-Trait-multi-witness) is fatal. Slice 6 strips this
so `karac run` rejects the same set `karac check` / `karac build` do, and routes
`run` through the JIT (which *forces* strict typing — a program that doesn't
type-check can't be codegen'd). The two are coupled: run→JIT ⇒ no leniency for
anything codegen can't emit.

## Method

`scratchpad/leniency_sweep.py`: for every `.kara` under `examples/` (78 files),
run `karac check --output=json` and `karac run`, then classify:
- **CHECK-CLEAN** — no error-severity diagnostics → unaffected by the strip.
- **BREAKS** — check reports type/effect errors AND `run` exits 0 today → the
  strip would newly reject it.
- **ALREADY-FATAL** — check errors AND run already exits non-zero → no change.

## Result (examples/, 78 files)

| Class | Count |
|---|---|
| CHECK-CLEAN (unaffected) | 71 |
| ALREADY-FATAL (no change) | 6 |
| **Real BREAKS** | **0** (after the fix below) |

- **1 genuine break, now FIXED:** `examples/leetcode/two_sum.kara` — declared
  `-> Option[(u64, u64)]` but `enumerate()` yields `i64` indices and `Map.new()`'s
  value type went uninferred (`?T1`); check rejected, run downgraded-and-ran with
  *correct* output (stale annotation, not a logic bug). Fixed to
  `Option[(i64, i64)]` + `seen: Map[i64, i64]`; checks clean, output unchanged.
  Committed separately.
- **1 false positive (not a break):** `examples/elevator_project/src/scheduler.kara`
  reports `Option<i64>` vs `Option<ref i64>` under *single-file* check, but the
  project type-checks clean in project mode — the error is an artifact of
  checking one project module without its siblings. The sweep runs single-file
  by construction; project-mode `karac check` from `examples/elevator_project`
  reports 0 errors. Not affected by the strip.
- **6 already-fatal** (run already exits non-zero, no change):
  `fathom/mandelbrot.kara` [effect], `leetcode/course_schedule.kara` [typecheck],
  `leetcode/merge_sorted_lists.kara` [typecheck], `parallax/src/providers.kara`
  [typecheck], `parallax/src/workload.kara` [typecheck], `plume/plume.kara`
  [effect]. (The `parallax` two are single-file timeouts on project modules.)

## Result (kara-katas, 269 files — swept after owner authorized the repo add)

| Class | Count |
|---|---|
| CHECK-CLEAN (unaffected) | 269 (after fixes) |
| Real BREAKS (pre-fix) | 5 |

The 5 breaks, all stale type annotations over already-correct logic (output
byte-identical before/after fix), committed to `karalang/kara-katas` (`6eb1317`):
- `Slice[u8]` vs `ref Vec[u8]`: three "recursive" variants declared helper
  params `ref Vec[u8]` but `String.bytes()` yields `Slice[u8]` —
  `67-add-binary/add_binary_recursive`, `171-excel-.../column_number_recursive`,
  `415-add-strings/add_strings_recursive`. Fixed params → `Slice[u8]`.
- uninferred `Vec<?T2>`: `133-clone-graph/{bfs,dfs}` did `a2.push(Vec.new())`
  into `Vec[Vec[i64]]` (push-arg type doesn't flow into inline `Vec.new()`) →
  switched to `row([])`, the file's empty-row idiom (empty-array literals DO get
  expected-type inference).

## Strip implemented (6a)

With all three corpora clean, `cmd_run` (`src/cli.rs`) now aborts on any type
error or hard effect error (E0411 target-gate + FFI hints excepted), mirroring
`check`/`build` acceptance. Tests `run_effect_violation_aborts` /
`run_soft_type_error_aborts` (inverted from the old warns-and-executes pair) pin
it; `run_value_corrupting_cast_aborts` / `run_raii_across_yield_violation_aborts`
still pass. design.md § "`karac run` Leniency" updated to reflect the strip.

## Inference-gap note (not a blocker)

Both the `two_sum`/`clone-graph` fixes worked around the same typechecker
limitation: an inline `Map.new()` / `Vec.new()` doesn't receive its element type
from a later `insert` or from a `push`-argument context (Rust infers these via
bidirectional inference). This is a genuine inference gap, not a kata bug —
annotating or using an expected-typed literal is the current workaround. Worth a
future typechecker slice / bug-ledger entry.
