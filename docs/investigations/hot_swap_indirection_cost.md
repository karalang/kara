# Hot-swap indirection cost — bench

**Status:** ✓ Resolved (2026-05-18). **Hardware:** Apple M5 Pro.
**Tracker:** [`phase-7-codegen.md`](../implementation_checklist/phase-7-codegen.md) line 5 sub-item 2.

The `--enable-hot-swap` codegen flag landed in commits `7bd2519`
(CLI surface) + `70c6363` (codegen indirection). Per the entry's
own discipline — *"bench the indirection cost on a representative
workload before flag ships"* — this doc captures the measurement
that validates the flag is safe to expose. The earlier "tentative
<1% overall / worst-case 10–20%" framing pre-dated any measurement;
the numbers below replace it.

## Result

| Workload | Per-call work | Baseline (mean ± σ) | Hot-swap (mean ± σ) | Slowdown |
|---|---|---|---|---|
| `tight_call.kara` (N=500M) | ~3 ops (xor, mul-by-31, add) | 476.8 ± 3.9 ms | 486.5 ± 17.7 ms | **2 ± 1 %** |
| `moderate_call.kara` (N=50M) | ~30 ops (12-stage hash mix) | 406.1 ± 3.8 ms | 409.5 ± 7.3 ms | **1 ± 2 %** |

Both within statistical noise of the entry's "<1% overall" claim;
the worst-case microbench never reaches the entry's "10–20%"
upper bound. **Conclusion: the flag is safe to ship at the
landed shape. No fallback to per-symbol opt-in needed.**

## Method

Two `.kara` programs under [`bench/hot_swap_cost/`](../../bench/hot_swap_cost/),
each compiled twice through the same `karac` release binary —
once without the flag, once with `--enable-hot-swap`:

```
karac build tight_call.kara                  → tight_call_baseline
karac build --enable-hot-swap tight_call.kara → tight_call_hotswap
karac build moderate_call.kara               → moderate_call_baseline
karac build --enable-hot-swap moderate_call.kara → moderate_call_hotswap
```

Both pairs produce **bit-identical stdout** (confirms the codegen
shipped semantically equivalent binaries; the indirection only
changes the dispatch shape).

Timing with `hyperfine --warmup 2 --runs 7` on each pair. Machine
otherwise idle.

## Disassembly — what the flag actually changes

Baseline `tight_call_baseline` (M1 Pro `arm64`, opt-level 2): the
compiler inlines `step` into `main`'s loop. The whole inner loop
collapses to seven instructions:

```
add  x8, x8, #0x1     ; i++
eor  x9, x8, x9       ; acc ^= i
cmp  x8, x10          ; loop test
lsl  x11, x9, #5      ; tmp = acc << 5
sub  x9, x11, x9      ; acc = tmp - acc  (= acc * 31)
add  x9, x9, #0x7     ; acc += 7
b.lo                  ; loop back
```

Hot-swap `tight_call_hotswap`: the call site lowers as a load
from `@karac_hotswap_table` + indirect call. `step` survives as a
separately-emitted function — the indirect call cannot inline
across the table indirection:

```
ldr  x8, [x20]        ; x20 = &@karac_hotswap_table[step_slot]
add  x19, x19, #0x1   ; i++
mov  x1, x19          ; arg = i
blr  x8               ; call via table
cmp  x19, x21         ; loop test
b.lo                  ; loop back
```

The load is per-iter (LLVM did not hoist it via LICM — the global
isn't `readonly` from the optimizer's POV, since hot-swap is the
exact use case where it would be rewritten). Even so the M5 Pro's
branch predictor handles the indirect call essentially free
(target never changes during the run) and the deep pipeline
absorbs the load/call sequence into the same throughput envelope
as the inlined loop.

## Why the cost is much smaller than the original "10–20%" estimate

The entry framed the worst case around naive call/return overhead
on a hot inner loop. Two pieces of M5-class hardware change that:

1. **Indirect-call branch prediction.** When the target never
   varies (every call goes to the same `step` function), modern
   ARM cores predict the indirect branch with the same accuracy
   as a direct branch. The hot-swap reload story does eventually
   churn the prediction history, but only at reload-event
   cadence — the steady-state cost is the same as direct call.
2. **Out-of-order issue width.** The 8-wide perf cores absorb the
   extra load + register move into the existing data-dependency
   chain. The bottleneck on `tight_call` is the serial chain
   through `acc`, which both binaries inherit identically.

Older hardware (in-order cores, narrow issue, weak BTBs) would
show a wider gap. Servers with M-class / x86-64 modern perf cores
are the audience deferred.md anticipates anyway (production with
strict W^X is excluded), so this result is consistent with the
deployment shape.

## Caveat — what this does NOT measure

- **Multi-target indirect-call prediction.** The microbenches each
  have exactly one slotted pub fn. A workload with many slots
  exercised in random order may pressure the BTB harder; the
  steady-state cost there could be higher than 2%. Re-measure if
  a real workload of that shape surfaces.
- **Cross-DSO reload steady-state.** v1 binaries store direct
  pointers in the table. The post-v1 reload path (dlclose + dlopen
  + repopulate) is unmeasured here — that's downstream of v1 ship.
- **Code-size cost.** The hot-swap binaries are ~50 KB vs ~33 KB
  baseline (33 % larger). Mostly debug+ctor overhead; not measured
  against a real binary where the relative impact would be much
  smaller.

## Provenance

- Source: [`bench/hot_swap_cost/tight_call.kara`](../../bench/hot_swap_cost/tight_call.kara),
  [`bench/hot_swap_cost/moderate_call.kara`](../../bench/hot_swap_cost/moderate_call.kara).
- Runner: ad-hoc `hyperfine`; full reproduction in this doc.
- karac binary: `target/release/karac` built from `70c6363`.
