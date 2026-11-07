# Kata 354 split by phase (B-2026-11-06-8)

`russian_doll.kara` (kara-katas `leetcode/301-400/354-russian-doll-envelopes/bench/`)
runs 1.30x rustc -O's cycles on the M5 and level with it on x86. Each program here
is one phase of it, with the same LCG input and a Rust mirror line for line, so
`scripts/bench/m5-rows.sh` can say which phase holds the arm64 gap:

- `gen_only`: draw the envelopes (two checked multiplications per envelope).
- `sort_only`: draw, clone, and `sort_by` width ascending, height descending.
- `lis_only`: draw the heights only, then the patience LIS over them unsorted.

Each folds its result into the sink as the bench does, so no round is skipped.

On x86 the LIS phase was the only one behind Rust (1.15x; generation 0.97x,
sorting 0.90x). On arm64 its search loop halved `lo + hi` with a signed
`/ 2`, which keeps a sign fixup (`add x, x, lsr #63`) before the `asr`, and
`(lo + hi) / 2` on a signed type is not a midpoint `src/mir/midpoint.rs`
takes. `src/mir/halving.rs` proves `lo` and `hi` never negative and makes
the division a shift: the loop loses the fixup on arm64 and the probe goes
to 1.09x on x86. The `env:KARAC_MIR_HALVING=0` row measures that rewrite
on the M5.
