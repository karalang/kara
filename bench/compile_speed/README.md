# compile_speed — cold compile-elapsed CI gate

**Status:** Scaffold, 2026-05-20. Graduated from [`brainstorming/archive/v69_go_parity_gaps.md § Gap 1`](../../brainstorming/archive/v69_go_parity_gaps.md). Corpus + harness + CI workflow to be implemented per the deliverables in [`docs/roadmap.md § Phase 8.5 Track 5`](../../docs/roadmap.md).

Tracks cold compile-elapsed time for `karac build` against `rustc -O` (peer baseline), with `clang -O2` opportunistic and `go build` as launch-time reference. PR-trigger CI workflow compares against `baseline.json` and fails on >30% regression (the initial threshold; tightens to ≤5% as data accumulates).

## Corpus

Three corpus members, two checked into this directory plus one cross-repo reference:

| Member | Source | Shape | v1 status |
|---|---|---|---|
| Seed kata (algorithmic) | Copy from `kara-katas/leetcode/...` | ~100 LOC, optimizer-heavy | **TBD** — specific kata selection deferred to corpus-setup time |
| Synthetic front-end stress | [`synthetic.kara`](synthetic.kara) (this dir) | ~10K LOC, generics + traits + effects | **Required at v1** |
| Backend-shape kata (real-world) | `kara-katas/leetcode/backend-service` or equivalent | ~500–1000 LOC, generics + IO + concurrency | **v1-required in [`kara-katas/PLAN.md § Priority 1`](../../kara-katas/PLAN.md)** — when it lands, copy here as the third corpus member |

Each corpus member ships:
- A `.kara` source file (the workload).
- A `.rs` parallel translation for the `rustc -O` baseline.
- Optionally a `.c` translation for `clang -O2` calibration (where it exists).
- A `.go` translation for the launch-time Go reference (added at launch, not on PRs).

## Reproduction

```bash
# Prerequisites
brew install hyperfine
cargo build --release --features llvm --bin karac

# Run the bench
./bench.sh

# Output: hyperfine table per benchmark + JSON in `latest.json`
```

The reproduction script:
1. Deletes any cached build artifacts so each invocation measures a full cold compile.
2. Runs `hyperfine --warmup 1 --runs 10 --shell=none --export-json latest.json --prepare 'rm -f <artifact>' '<command>'` per (workload, compiler) pair.
3. Emits a human-readable table and `latest.json` for CI consumption.

## CI gate

PR-trigger workflow:
1. Runs `./bench.sh`.
2. Parses `latest.json`.
3. Compares each benchmark's mean against `baseline.json` (committed to repo, updated on main-merge by a separate workflow).
4. Posts a PR comment with verdict + per-benchmark ratio (karac vs rustc) + per-benchmark delta vs baseline.
5. Fails the job if any benchmark exceeds **30%** over baseline.

The 30% threshold is the initial value. As the corpus stabilizes and baseline variance is characterized, this tightens (target: ≤5% long-term). See the threshold trajectory in `docs/roadmap.md § Phase 8.5 Track 5`.

## Baseline

`baseline.json` — current reference numbers for the gate. Committed to the repo; updated by the main-merge workflow as each PR lands. Format mirrors hyperfine's `--export-json` output.

Compare ratios (karac/rustc), not absolute karac times — both shift across CI runner generations, so the ratio is more stable than the absolute.

## See also

- [`../README.md`](../README.md) — bench-setup protocol covering hyperfine discipline, rusage discipline, baseline policy.
- [`kara-katas/BENCH.md`](https://github.com/.../kara-katas/BENCH.md) — mirror of the protocol for the kata corpus (compile-elapsed measurement template).
- [`brainstorming/archive/v69_go_parity_gaps.md § Gap 1`](../../brainstorming/archive/v69_go_parity_gaps.md) — full resolution provenance (corpus, threshold, baselines, hosting, CI shape).
