# karac-rust bench tracks

Bench infrastructure for compiler-quality gates and microbenchmarks. Each subdirectory is an independent track with its own corpus and reproduction script; the top-level `bench.sh` is a thin aggregator that runs everything.

## Tracks

| Track | What it measures | Gate-shape |
|---|---|---|
| [`compile_speed/`](compile_speed/) | Cold compile-elapsed time of `karac build` vs `rustc -O` (peer baseline); `clang -O2` opportunistic; `go build` reference at launch-time | **CI gate** — PR-trigger, fails on >30% regression vs `baseline.json` |
| [`hash_quality/`](hash_quality/) | Hash function collision rate + distribution quality | Manual reproduction |
| [`hot_swap_cost/`](hot_swap_cost/) | `--enable-hot-swap` AOT cost on tight vs moderate function bodies | Manual reproduction |
| [`indirection_cost/`](indirection_cost/) | Type-erasure tax on collection operations (Rust microbench) | Manual reproduction |

## Bench-setup protocol

Every track follows the same measurement discipline. New tracks should follow this template.

### Tools

- **`hyperfine`** for elapsed-time measurements (`brew install hyperfine`). Statistical reporting; multiple runs; warmup support.
- **`/usr/bin/time -l`** (macOS BSD `time`) for memory measurements. `rusage` peak footprint; single sample (memory is much more stable run-to-run than wall-clock).
- **Native `karac` + `rustc`** invocations, never wrapped in shells the measurement might attribute to.

### Discipline

**Elapsed-time measurements** (use `hyperfine`):
- **Short workloads (<50ms)**: `hyperfine --warmup 5 --runs 30 --shell=none` — startup jitter dominates at low run counts.
- **Long workloads (>1s)**: `hyperfine --warmup 2 --runs 10 --shell=none` — already <2% RSD at 10 runs; bumping runs adds wall-time without information.
- **Cold compile-elapsed measurements**: `hyperfine --warmup 1 --runs 10 --prepare 'rm -f <artifact>' --shell=none` — `--prepare` deletes the build artifact before each run so each invocation measures a full cold compile.

**Memory measurements** (use `/usr/bin/time -l`):
- Single sample; no averaging needed (memory is stable run-to-run, no scheduling/cache variance).
- **Cold compile-memory measurements**: delete the build artifact first, then invoke `karac build` / `rustc -O` directly under `/usr/bin/time -l` so rusage measures the compiler process itself, not a wrapping shell.

**Comparison baselines**:
- **`rustc -O`** is the always-on peer baseline. Same family as karac (LLVM, monomorphization, ownership), same optimization-vs-compile-speed tradeoff curve. The honest comparison.
- **`clang -O2`** is opportunistic — measured where a C translation exists in the track. Shares LLVM backend with rustc, so the karac-vs-clang delta isolates karac-specific frontend cost from LLVM-backend cost. Don't write fresh C translations to satisfy benches.
- **`go build`** is a reference measurement, not a CI baseline. Go was designed for fast compilation as an explicit tradeoff against optimization; a karac-vs-go ratio measures design choices, not engineering quality. Publish numbers with the philosophy caveat ("Go optimizes for compile speed by design; Kāra optimizes runtime perf with proportional compile cost, like Rust"). Not gated on PRs.

### Output format

Each track's bench output is human-readable on a TTY (hyperfine's default rendering) and machine-readable for CI (JSON via `--export-json`). CI workflows parse the JSON to extract per-benchmark mean times and compare against `baseline.json` files committed to the repo.

### Adding a new track

1. Create `bench/<track-name>/` with a `README.md` (purpose, reproduction command, expected output).
2. Wire into `bench/bench.sh` (thin shell invocation of the track's own reproduction).
3. If the track gates merges, add a `baseline.json` to the directory and a CI workflow that compares PR runs against it.
4. Update this table.

## Relationship to `kara-katas`

Runtime-perf benchmarks live in [`kara-katas`](https://github.com/.../kara-katas) (separate repo). The kata corpus measures end-to-end algorithmic workloads with parallel `.kara` / `.py` / `.rs` implementations. Kata `bench/bench.sh` files follow the same hyperfine + rusage discipline documented here (mirrored in [`kara-katas/BENCH.md`](https://github.com/.../kara-katas/BENCH.md)).

The two corpora are independent — no sync infrastructure. The `compile_speed/` track here copies *selected* katas as plain files for the compile-speed gate (specific selection evolves over time); kara-katas keeps the full kata corpus for runtime-perf signal.

## See also

- [`docs/roadmap.md § Phase 8.5 Track 5`](../docs/roadmap.md) — compile-speed CI gate deliverables.
- [`docs/investigations/bench_robustness.md`](../docs/investigations/bench_robustness.md) — robustness gaps in the Parallax bench (different infrastructure; same discipline).
- [`brainstorming/archive/v69_go_parity_gaps.md § Gap 1`](../brainstorming/archive/v69_go_parity_gaps.md) — resolution provenance for the compile-speed track.
