# `sortbench` — the `Vec.sort_by` benchmark harness

Backs [`../sort-algorithm-gap.md`](../sort-algorithm-gap.md) and ledger rows
B-2026-08-10-19 / -20 / B-2026-08-11-10.

**It lives in the repo because the previous one did not.** The original harness
sat in a session scratchpad; that session ended, the container was reclaimed,
and the next investigation had to rebuild it from the prose in § Method before
it could measure anything. Rebuilding it also silently re-opened every
methodology question the first one had already settled. This copy exists so
that cost is paid once.

## Files

| file | role |
|---|---|
| `bench.tmpl.kara` | the Kāra benchmark; `@PATTERN@` / `@ROUNDS@` / `@SORT@` are substituted per variant |
| `gen.py` | generates + builds all 24 variants (6 patterns × {1,25} rounds × {sort, cloneonly}) |
| `measure.py` | slope-based pure-sort wall-clock timing |
| `drift2.rs` | the Rust/driftsort baseline, input generator mirrored line-for-line |
| `one.rs` | single-sort Rust driver, for instruction counting |
| `ir.sh` | host-independent instruction counts via callgrind |
| `mirror.rs` | faithful Rust mirror of karac's own sort algorithm, `plain` vs `gallop`, for budget-checking a merge-side change without writing an emitter |

## Running it

```bash
python3 gen.py /path/to/karac      # build the 24 Kāra variants -> ./bin
python3 measure.py bin 7           # pure-sort wall clock, best-of-7
rustc -O -o drift2 drift2.rs && ./drift2    # driftsort wall clock
rustc -O -o one one.rs && ./ir.sh drift     # driftsort instructions
./ir.sh                                     # karac instructions

rustc -O -o mirror mirror.rs                # the algorithm mirror
./mirror few_unique plain                   # correctness (asserts sorted AND stable)
./mirror few_unique time                    # plain vs gallop wall clock
```

**Use `mirror.rs` before writing an emitter.** It runs karac's own algorithm on
the real input, so a candidate merge-side change can be measured — instructions
*and* wall clock — in minutes rather than after ~500 lines of IR emission. Its
own validation is the same checksum below: `plain` must land within ~10% of
karac. It is also the reason the galloping direction was refuted cheaply
(spike § Direction 6), and its stability assertion caught a real bug in that
prototype that produced correctly *sorted* output on every pattern.

## Method, and why each piece is there

- **Slope, not total.** `(t[25 rounds] − t[1 round]) / 24` removes process
  startup *and* the one-time input build. Subtracting the `cloneonly` slope
  removes the per-round `base.clone()`. That clone lands at 1.13–1.24 ms and is
  flat across patterns — if a rebuild of this harness does not reproduce that,
  the harness is wrong before any sort number is worth reading.
- **The Rust generator must match the Kāra one exactly.** Same fixed-seed LCG,
  same modulus, same key expressions, same second field. § Caveats of the spike
  records that the *original* karac-vs-driftsort ratios were corrupted by a
  generator that did not match (two LCG steps per element, different key
  ranges). This is the single easiest way to get a wrong answer here.
- **`black_box` on the Rust side is not optional.** Without it the random
  baseline measures 6.77 ms; with it, 8.65 ms. The unhardened number silently
  flatters Rust by 28% and does not match the spike's own recorded 8.2–8.9.
- **`drift2.rs` prints the distinct key count.** "150k over 8 distinct keys" is
  then verified rather than assumed — the few-unique result is entirely a
  function of that cardinality.
- **Instruction counts via callgrind, not `perf`.** No PMU access in a
  container (`perf_event_paranoid=2`, and `linux-perf` has no install
  candidate), but callgrind needs none and is deterministic. Wall clock on a
  shared 4-vCPU container drifts; instruction counts do not, which makes them
  the metric to argue from. `--branch-sim=yes` adds simulated mispredicts if a
  branch-prediction question comes up.

## Validating a rebuild

Two numbers are the checksum on the harness itself, both host-independent:

```
karac few_unique   48.8M instructions
karac sawtooth     22.5M instructions
```

The 2026-08-11 rebuild reproduced the 2026-08-10 originals (48.85M / 22.53M) to
three significant figures. If a future rebuild does not, fix the harness before
trusting anything it says about the sort.
