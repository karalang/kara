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

## Rails

| rail | shape | what it isolates |
|---|---|---|
| `lexer` | the real selfhost lexer over selfhost sources | end-to-end, the only non-synthetic rail |
| `lexlike` | slice a 3-byte token, compare, discard | transient-read tax |
| `substr` | the same via `String.substring` | transient-read tax, method spelling |
| `builder20` / `builder60` | `push` into a never-inline String, CONSTANT bound | per-push tax, upper bound only (see below) |
| `promote` | receiver inline at first mutation (from `substring`) | promotion cost |
| `pfx_idx` | `builder` with a RUNTIME bound | per-push tax, the honest short-string number |
| `pfx_chars` | exact mirror of `vertical`'s `prefix_string` | a rail SSO WINS on — the paired control |

**Read `builder*` and `promote` as a pair.** A change that makes the inline
check cheaper by making the promotion more expensive looks like a win on
`builder` alone; one such change made the same loop 4x worse before the pairing
caught it. Likewise `pfx_chars` is the rail that stops a mutation-path change
being credited with a win it did not earn — it is 9-15% FASTER under SSO, so a
regression there is a real cost even when every other rail improves.

**`builder20` is unrolled; `pfx_idx` is not.** `builder20`'s trip count is a
compile-time constant, so LLVM unrolls the loop and erases the per-iteration
cost the rail exists to measure. `pfx_idx` has the identical 0..20 length
distribution with a runtime bound. Where the two disagree, `pfx_idx` is the
number that describes real code.

## Auto-par is PINNED OFF, and that is a control

`bench.sh` builds every rail with `KARAC_AUTO_PAR=0`. The spike doc states the
reason as a control for the kata sweep -- "auto-par is a third surface and
letting it vary would make any difference unattributable" -- but this script did
not apply it until 2026-09-15, so **every rail number published before that date
was measured with the driver loops FANNED OUT.**

Each rail's `main` accumulates `total = total + s.len()`, which the analyzer
reads as a `+` reduction and parallelizes at these iteration counts. Both legs
of a comparison fanned out equally, so the SIGNS held -- but the magnitudes were
parallel-throughput deltas, and a per-iteration difference spread across cores
reads SMALLER than it is. It is not a small correction: pinning moved `pfx_idx`
from +4.1/+5.6% to +14.1/+15.9%, roughly 3x, and `substr` from +5% to +33.7%.

Not every rail was affected, which is why the error was easy to miss. Check any
individual rail with `karac build --concurrency-report <rail>.kara`:

| rail | fans out under default? |
|---|---|
| `substr`, `builder20`, `builder60`, `promote`, `pfx_idx`, `pfx_chars` | yes |
| `lexlike` | no — `declined_memory_bound` |

So `lexlike`'s history is comparable across the change and everything else's is
not.

Do not make a rail's accumulated value depend on the accumulator itself (e.g.
`let k = (i + total) % 21`). That shape is silently miscompiled under auto-par --
B-2026-09-14-31.

## Last measured (2026-09-15, x86-64, RUNS=15, auto-par pinned off)

Two independent samples; `delta` is `SSO=1` relative to `SSO=0`, negative means
SSO is faster.

| rail | SSO=0 | SSO=1 | delta |
|---|---|---|---|
| `lexer` | 2860 / 2844 ms | 2355 / 2345 ms | **-17.7 / -17.5%** |
| `lexlike` | 259 / 262 ms | 226 / 226 ms | **-12.7 / -13.7%** |
| `substr` | 172 / 172 ms | 230 / 231 ms | +33.7 / +34.3% |
| `builder20` | 219 / 220 ms | 252 / 251 ms | +15.1 / +14.1% |
| `builder60` | 329 / 329 ms | 368 / 370 ms | +11.9 / +12.5% |
| `promote` | 179 / 178 ms | 193 / 192 ms | +7.8 / +7.9% |
| `pfx_idx` | 64 / 63 ms | 73 / 73 ms | +14.1 / +15.9% |
| `pfx_chars` | 175 / 175 ms | 157 / 157 ms | **-10.3 / -10.3%** |

**`lexlike` no longer regresses, and that retires this track's stated open
question.** It stood at +34% on 2026-09-12 and is now a 12-14% WIN. The pin does
not explain it -- `lexlike` is the one rail that never fanned out, so both
numbers are sequential and directly comparable. Something between 2026-09-12 and
2026-09-15 fixed it; the candidates are B-2026-09-12-20's overlay fix, the
growth-test fold (c1adb9c), and keeping `Vec` off the SSO path, and NOTHING HERE
ATTRIBUTES IT to any of them. Do not re-derive the old 34% from this file: it is
gone, and the open question named in `bench.sh`'s header is answered only in the
sense that the symptom stopped.

**`substr` is now the worst rail at +34%**, having read as +5% while fanned out.
It is the same shape as `lexlike` -- slice, compare, discard -- but through
`String.substring`, and the two now disagree by 47 points. That is the open
question this track should be asking next.
