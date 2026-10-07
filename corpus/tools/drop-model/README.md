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

## Source-level model (corpus programs)

- `kparse.py`: lexer and recursive-descent parser for a subset of Kara (structs, enums, impls incl.
  `Drop`, traits, generics read loosely, match/if/while/while let/for/loop, `?`, defer/errdefer,
  `shared` types, f-strings). Raises `Unsupported` or `ParseError` outside the subset.
- `kmodel.py`: dynamic executor over that AST, same rules as `model.py` (§3, §4, §6, §7, §8) plus:
  shared handles as counted boxes (§6.1), refs with origins and the §5.5 temporary check, the §3.3
  loop back-edge check, `?` with `From`, operator modes from the impl (§4.7), and builtins for
  `Vec`, `String`, `Option`/`Result`, `Map`/`Set` (Map drops flagged unordered). `run_source(src)` returns
  `(stdout, exit, flags)` or raises `ModelError` for a program v2 rejects (executed path only, so a
  lower bound). `KARA_MODEL_COUNT_AGG=0` switches handle aggregates (`Option[Node]`, tuples of handles
  and Copy parts) from counted copies to Rust's move-only rule; the default is counted, pending
  Gowtham's decision on that card.
- `corpus_run.py [--tag T] [--path P] [--json OUT] [--include-v2]`: runs the model on every corpus
  program and compares with the recorded legacy output. `KARA_ROOT` (default: the repo above this
  directory), `MODEL_TIMEOUT` seconds per program (use 3 for the whole corpus).
  Verdicts: SAME, ORDER (same lines, other order), ORDER-UNSPEC (differs only in Map drop order),
  DIFF, V2-REJECT, LEGACY-REJECT, UNSUP, PARSE, CRASH.
- `validate.py`: the model against `corpus/core` and `corpus/drop-matrix`; exits 1 on a FAIL.
  Result: core 30 agree, 3 unsupported (escaping capture, `par`, nonescaping param);
  drop-matrix 256/256. Fault injection (fields dropped first to last) gives 113 matrix failures.

### Whole corpus, 7002 programs, counting rule, 2026-10-06

| Verdict | Count |
|---|---|
| UNSUP | 2948 (timeout ~330 at 3 s, `Vec.iter` 187, `env` 187, `Vec.filled` 104, `Vector` 87, closures, weak references, integer widths other than i64, hand-written `Hash`/`Ord` used outside an operator, impls chosen per instantiation) |
| SAME | 2514 |
| ORDER | 641 |
| PARSE | 458 (`effect` items 84) |
| V2-REJECT | 336 (index move 63, Drop-type payload move 52, ref passed to owned param 51, move out of ref 22, a match arm yielding a ref beside an owned arm 21, ref returned where an owned value is expected 11, use after move) |
| DIFF | 32: 22 differ in output, 8 are failed asserts (v2 exits 101, legacy 1; §10.1), 2 are legacy aborts (-6) the model runs to completion |
| LEGACY-REJECT | 73 |
| CRASH | 0 |

The 22 output DIFFs read so far are legacy running too few `Drop` bodies (a local moved into a temporary
`match` scrutinee, fields destructured out of `self`, a by-value param part left unmoved on one path),
legacy running one twice (a binding moved out of a borrowed `if let` place), or legacy evaluating a compound
assignment's subscripts twice (§3). Legacy is frozen, so these stay here and not in the ledger.

Under Rust's move-only rule for handle aggregates the V2-REJECT count is 505, of which 188 are
"move out of a shared place", 181 of them `Option[Handle]` (103 programs, 39 katas, from `a = n.next`).

`corpus-verdicts.json` (per program verdict and reason) and `v2-reject-list.tsv` (corpus dir,
legacy expectation, model reason; 17 of the 336 legacy already rejects) are in the review thread's
shared folder `review/drop-model/`; regenerate with `corpus_run.py --json`.

## Limits

- Moves are checked on the executed path only. A maybe-moved use on a path not taken is not reported;
  generators must produce statically valid programs.
- `model.py` does not model closures, `while let`, `?`, `Map`/`Set`, shared handles, views or `par`;
  `kmodel.py` adds `while let`, `?`, `Map`/`Set` and shared handles, and still lacks closures and
  captures (§9), views (§5), `par`, effect items, `env` and most iterator adapters.
- One spec gap read deliberately: an `errdefer` runs when the function exits with an `Err` by any route,
  including a tail `Err(...)`. The draft names only `?` and `return Err(...)`.

## Next

1. ~~A parser for the subset.~~ Done: `kparse.py` + `kmodel.py`.
2. Store the model's stdout per corpus program (`model.out`, verdict in `meta.toml`) as the class-c oracle
   for the MIR interpreter.
3. Widen the subset: closures, `Vec.iter` chains, `effect` items, `env`, integer widths.
4. ~~Land beside the corpus.~~ Done by the kata thread 2026-10-06: `corpus/tools/drop-model/` and `corpus/drop-matrix/`.
