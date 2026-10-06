# Drop reference model (v2, M1 item of the review thread)

Independent oracle for drop timing, per `docs/core-semantics.md` §3, §4, §7, §8. Lives at `corpus/tools/drop-model/`; the matrix it generates is `corpus/drop-matrix/`.
It imports nothing from karac. Python 3, stdlib only.

- `model.py`: a small program AST, `emit(prog)` (Kara source) and `run(prog)` (expected stdout and
  exit code, or `ModelError` for a program v2 rejects). Execution is direct and dynamic: every holder is a
  Cell, a move writes MOVED into it, a scope end drops what is still there. No drop flags, no analysis.
- `pins.py`: 26 of the 33 core pins encoded for the model. `python3 pins.py` checks the model against
  the pins' hand-derived `.expected`. Result 2026-10-06: **26/26 agree**. Fault injection (tuple parts
  dropped first to last) turns `drop_aggregates` red, so the check is not vacuous. The 7 not encoded need
  integer side effects (`eval_order`, `ok_compound_assign_once`), closures, views or `par`.
  Run it as `python3 pins.py ../../core` from this directory. The encodings were checked against
  the pin programs by emitting each and running it on legacy (all 26 match), with a script kept in the
  review thread's notes rather than here, since it needs per-pin legacy recordings the corpus does not keep.
- `matrix.py OUT [--legacy KARAC]`: 9 value shapes x 24-30 positions = **257 programs**, each with
  `main.kara`, `expected.out` (from the model) and `meta.toml`; with `--legacy`, also `legacy.out` (stdout only) and `legacy_exit` in `meta.toml`.

## Matrix against legacy (`karac run --interp`, 53f31fbb), 2026-10-06

| Bucket | Count | Meaning |
|---|---|---|
| SAME | 28 | legacy already prints the v2 schedule |
| ORDER | 205 | same lines, different order: the class-c shape (last-use drops, tuple order, errdefer order...) |
| DIFF, panic | 9 | expected: v2 runs no drops or defers on panic (C8); legacy runs them all |
| DIFF, other | 6 | **legacy drops never run**: a temporary `match` scrutinee matched by `_`, or by a pattern that moves only one part, loses its Drop bodies (`tuple/vec/option/enum__match_temp_wild`, `tuple/enum__match_temp_part_move`). Legacy is frozen, so these are recorded here and not in the ledger; v2 fixes them by construction. |
| LEGACY-REJECT | 9 | `ref name` patterns, which legacy does not parse |

Every DIFF and REJECT is explained; none points at the model.

## Limits

- Moves are checked on the executed path only. A maybe-moved use on a path not taken is not reported;
  generators must produce statically valid programs.
- Not modelled yet: closures and captures (§9), `while let`, `?`, `Map`/`Set`, shared handles (§6),
  views (§5), `par`. Class-c corpus programs outside this subset need hand review (classification §4).
- One spec gap read deliberately: an `errdefer` runs when the function exits with an `Err` by any route,
  including a tail `Err(...)`. The draft names only `?` and `return Err(...)`.

## Next

1. A parser for the subset, so class-c corpus programs with five or fewer drop prints can be checked
   mechanically (classification §4.1).
2. Closures, `?`, `while let`, shared handles.
3. ~~Land beside the corpus.~~ Done by the kata thread 2026-10-06: `corpus/tools/drop-model/` and `corpus/drop-matrix/`.
