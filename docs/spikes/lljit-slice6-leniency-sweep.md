# Slice 6 (strip run-leniency) — blast-radius sweep

**Status:** sweep partially complete — `examples/` + `examples/mend` done (0 real
breaks after one fix); **`kara-katas` UNSWEPT** (separate repo, not in this
container; adding it was denied in auto mode as unrequested scope creep). The
leniency strip itself is **NOT done** — it is owner-gated and the spike's
Gotchas require the `kara-katas` leg before stripping.

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

## Blocker

`kara-katas` — the third mandated corpus — is a **separate repo not present in
this cloud container**. `list_repos` shows `karalang/kara-katas` is accessible,
but `add_repo` was denied in auto mode (unrequested persistent integration).
Until it is swept, stripping leniency is stripping partially-blind against it.

## Recommendation

Do **not** strip leniency until `kara-katas` is swept. The `examples/` evidence
is encouraging (only 1 real break, itself a stale annotation over correct code),
but `kara-katas` is the larger, LLM-authored corpus most likely to lean on
leniency. Next step is owner's call: authorize adding `kara-katas` to sweep it
here, accept the `kara-katas` risk and strip on `examples/`-only evidence, or
hold Slice 6 entirely.
