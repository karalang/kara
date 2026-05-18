# hot_swap_cost — `--enable-hot-swap` AOT cost bench

Source for the measurement that closed phase-7 line 5 sub-item 2.
Full method, results, and disassembly in
[`../../docs/investigations/hot_swap_indirection_cost.md`](../../docs/investigations/hot_swap_indirection_cost.md).

## Reproduction

```bash
# from the karac-rust root
cargo build --release --features llvm --bin karac

cd bench/hot_swap_cost
KARAC=../../target/release/karac

for prog in tight_call moderate_call; do
  $KARAC build $prog.kara                       && mv $prog ${prog}_baseline
  $KARAC build --enable-hot-swap $prog.kara     && mv $prog ${prog}_hotswap
done

hyperfine --warmup 2 --runs 7 ./tight_call_baseline ./tight_call_hotswap
hyperfine --warmup 2 --runs 7 ./moderate_call_baseline ./moderate_call_hotswap
```

## Files

- `tight_call.kara` — worst-case microbench: a `pub fn` with ~3 ops
  per call body in a tight loop. Baseline inlines fully; hot-swap
  cannot inline across the indirection.
- `moderate_call.kara` — realistic-case microbench: a `pub fn` with
  ~30 ops per call body (12-stage hash mix). Hot-swap still cannot
  inline; baseline can vectorize inside the body.

Built binaries (`*_baseline`, `*_hotswap`) are gitignored — rebuild
from `.kara` source as shown above.
