# sso — small-string optimization: is it a net win, and where does it lose?

Reproduction harness for the open question in
[`docs/spikes/small-string-optimization.md`](../../docs/spikes/small-string-optimization.md).
SSO is **off by default** (`KARAC_SSO=1` turns it on); this track exists to
decide whether that should change.

```bash
bash bench/sso/bench.sh                 # all three rails, both settings
ITERS=1000000 PASSES=20 bash bench/sso/bench.sh   # fast iteration
```

## The rails, and why they disagree

| rail | shape | SSO |
|---|---|---|
| `lexer` | the real selfhost lexer over a large input. Allocation-dominated, and strings travel **by value into callees** — `keyword_or_ident(text: String)`, `make_spanned(token: Token)`, two consuming calls per token | **wins** |
| `lexlike` | slice a 3-byte token, compare against a keyword, discard. No consuming call | **loses** |
| `substr` | same shape via `String.substring` rather than slice syntax | wash |

Measured x86-64, 2026-09-12/13 — **as ratios; see the warning below**:

```
lexer     12-16% FASTER      lexlike   21-34% SLOWER      substr   ~1-5%, noise
```

The split is the finding. SSO removes a malloc per short string, which pays
when allocation dominates and the string is then *consumed by value*; it costs
a `cmov`-dependent data pointer and an opaque-pointer `bcmp` on every read,
which is all a tight slice-compare-discard loop does. `lexlike` and `substr`
differ only in how the slice is spelled, so the gap between them is worth
understanding too.

## Absolute milliseconds from this track mean nothing across runs

Only compare numbers **within one invocation**. The script rebuilds and
re-times every rail back to back for exactly this reason. Measured 2026-09-12:
the lexer rail moved **3.4x** in one day (1203 → 354 ms at `KARAC_SSO=0`) for a
reason unrelated to SSO, while the SSO ratio held at 13–16%. The input is also
generated from `selfhost/src/*.kara` at run time, so it changes as the tree
does. Ratios are the claim.

## The open question

**Where does `lexlike`'s remaining regression go?** It has never been profiled.

The reason to think it is worth profiling rather than accepting: it used to be
**66%**, and half of that turned out to be a store-to-load forwarding stall in
the inline encoder — which built a 24-byte stack array, wrote 3 content bytes
and a 1-byte trailer into it, then read three overlapping 8-byte integers back
out. That was removed as a *side effect* of an unrelated correctness fix
(B-2026-09-12-20), and nobody was looking for it. One implementation artifact
of that size has already been found by accident; the rest deserves a look
before anyone concludes the loss is inherent to the representation.

### Already ruled out — do not re-run these

Each was eliminated by measurement, not argument:

| hypothesis | how it died |
|---|---|
| emit a **branch instead of the `cmov`** | LLVM's SimplifyCFG folds the two-entry phi straight back into a `select`. The gate fires (pre-opt IR: 4 selects → 16 blocks) and the binaries are **byte-identical**. The version that would test the hypothesis has to sink the *load* into the arms, which means duplicating every consumer of the data pointer — a redesign, not a tweak |
| machine load | old and new binaries timed in the same loop, same minute |
| runtime **archive build form** | relinking against a deliberately wrong-form `cargo build` archive: 355 ms vs 354. It costs binary size, not speed |
| **optimization level** | `KARAC_OPT_LEVEL=0` gives 946 ms at 631 KB, against preserved binaries at 436–449 KB |
| `c9570e0`'s drop-handover fix | reverting just `src/codegen/exprs.rs` to its parent: 347/299, unchanged |
| a **leak** in the older builds | inverted — the *newer*, faster binaries peak at 33–35 MB RSS against the old ones' 16 MB |

Best remaining candidate for the (SSO-neutral) 3.4x: enum-payload free/drop
fixes that landed from other sessions in the same window. Needs a real bisect.

## Per-kata timing on a cloud container is not trustworthy

Measured 2026-09-13, sweeping the kata corpus. Two separate readings had to be
thrown away before one survived, and the same two traps will catch the next
person:

* **Cross-sweep comparison is confounded.** The `SSO=0` rail — identical
  configuration, untouched by the change under test — drifted **34%** between
  two sweeps an hour apart, with no stray process on the box (load 0.45). Host
  throttling. Per-kata deltas did not reproduce: one kata moved 93.5% -> 29.4%
  while another moved 49.2% -> 92.7%, under a change that could only help both.
* **Best-of-5 is below the effect size.** A three-rail design (baseline, real,
  probe) gets a free self-check, because a probe that strictly removes work can
  never be slower than what it is probing. That impossible ordering fired on
  **15/79 katas (19%)** at N=5, and on **1/29** at N=15 restricted to katas
  whose effect clears ~40 ms.

**So: trust corpus AGGREGATES, distrust any individual kata's delta** unless it
clears ~40 ms and survives a high-N three-rail pass — and publish the
impossible-sign count next to any per-kata claim. It is the cheapest noise floor
available, and it is what caught both bad readings.

## Profiling notes

**A cloud container cannot do this.** The VM exposes no hardware PMU —
`/sys/bus/event_source/devices/` lists only `software`/`tracepoint`/`msr`, with
no `cpu`, so `perf_event_open` cannot read cycles or stalls no matter what is
installed. `perf` is not a missing package there, it is a missing counter.
`valgrind` and `llvm-mca` are the ceiling.

**On macOS** (this repo's bench convention already assumes it — see
[`../README.md`](../README.md)):

- `xctrace record --template 'CPU Counters' --launch -- ./build/lexlike.rail1`,
  or Instruments' *CPU Counters* template, gives cycle and stall attribution.
- `sample`/`Time Profiler` is too coarse for a 200 ms loop; use counters.
- **Apple Silicon is a different microarchitecture**, and that cuts both ways.
  The x86 story here is a `cmov` on a load's address-dependency chain plus a
  store-forwarding stall; arm64 has `csel` and different forwarding rules, so
  the magnitudes will not transfer. That makes the arm64 run *worth doing on its
  own terms* — if the regression is much smaller there, the loss is
  microarchitecture-specific and the default-flip question changes shape.

**Static alternative, no counters needed:** `llvm-mca` models the pipeline
including data dependencies and port pressure for a single basic block. Extract
the hot loop with `llvm-objdump -d` and feed it in. It answers "is the `cmov` on
the critical path" directly, and it runs anywhere.

## What would settle the default-flip question

Not this track alone — it is three workloads. The missing measurement is the
**kata corpus timed at both settings** (1063 programs, already verified
byte-identical at both). They were swept for correctness and never timed, and
they are mostly `lexlike`-shaped, which is the shape that loses.
