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

## Last measured — and why there are two columns

**A rail's SSO ratio is a property of the rail AND the host.** Same compiler
(`karac` at `27466ba`, built and run on both), `RUNS=15`, auto-par pinned off:

| rail | host A (2 samples) | host B, `nproc=4` (5 samples) | portable? |
|---|---|---|---|
| `lexer` | −17.5 … −17.7% | −13.4 … −19.1% | roughly |
| `lexlike` | **−12.7 … −13.7%** | **+20.9 … +24.0%** | **no — sign flips** |
| `substr` | +33.7 … +34.3% | +72.2 … +74.4% | **no — doubles** |
| `builder20` | +15.1 … +14.1% | +1.8 … +3.7% | no |
| `builder60` | +11.9 … +12.5% | +2.4 … +3.6% | no |
| `promote` | +7.8 … +7.9% | −0.8 … +0.8% (zero) | no |
| `pfx_idx` | +14.1 … +15.9% | +15.9 … +18.2% | yes, near-exactly |
| `pfx_chars` | −10.3% | −7.5 … −8.5% | roughly |
| **`substr` − `lexlike`** | **46.4 / 48.0 pts** | **48.8 … 51.9 pts** | **yes** |

**Read the SPREAD, not the third digit.** The harness reports integer
milliseconds, so a rail landing at 127–129 ms quantizes to ±0.8% and one at
44 ms to ±2.3%. `promote`'s `+0.8 / +0.8 / +0.8 / −0.8 / +0.8` is not a small
regression — it is zero, ±1 ms, and it flips sign between samples. `pfx_idx`'s
+15.9/+18.2 spread is the same artifact at 44 → 51/52 ms: the delta is real,
its third digit is not. `lexer` is the LOOSEST rail rather than the tightest —
5 points across five samples — which matters because it is the rail SSO's whole
case rests on, so state it as ~−16 ± 3% and not as −17.6%. Two samples cannot
show any of this; the host B column above is five.

Different rails are bound by different subsystems, so a different CPU
allocation reprices them non-uniformly: the malloc-dominated rails
(`builder*`, `promote`) got relatively cheaper against SSO's overhead while the
memory-bound ones (`lexlike`, `substr`) got relatively worse.

**Never compare a rail delta against a number from a previous session's table.**
Re-run both legs on one machine. Differentials between two rails measured
together are the durable unit here; levels are not.

This was established, not assumed: building `karac` at `27466ba` and running it
on host B reproduces host B's column, not host A's. Twenty commits, seven
touching `src/`, moved these rails by nothing.

**`lexlike` did not "stop regressing" — it never regressed on host A, and it
does regress on host B.** An earlier revision said it went +34% → −13% and
called that an unexplained win. Building the committed rail at five commits
spanning 09-12..09-15 gave −12 to −14% at every one, including `bab0491`, the
commit the +34% was attributed to; `bench/sso/lexlike.kara` was committed by
`5bcafe9` 1h42m AFTER `bab0491`, so that figure measured a pre-harness workload
and was transcribed above rails that replaced it. Refuted as B-2026-09-15-1
(invalid). On host B the same rail is +23%, which is the host effect above, not
a regression. Two numbers from different dates — or different boxes —
disagreeing is not a finding until both are reproduced together.

**Auto-par does not uniformly compress — it moves different rails in opposite
directions, which is why the pin is a control and not a tidier baseline.**
Measured on host B at `f72fac4`, `RUNS=15`, three samples of each leg:

| rail | pinned | default (fanned out) | |
|---|---|---|---|
| `lexlike` | +20.9 … +24.0% | +21.7 … +22.9% | unchanged — does not fan out |
| `substr` | +72.8 … +74.4% | **+0.0 / −1.7 / +1.7%** | masked to zero |
| `pfx_idx` | +15.9 … +18.2% | +7.1 … +14.3% | compressed |
| `builder60` | +2.4 … +3.6% | **+11.7 … +13.6%** | **amplified ~4x** |
| `builder20` | +1.8 … +3.7% | +4.5 … +7.0% | amplified |
| `promote` | −0.8 … +0.8% (zero) | +2.9 / +2.9 / +2.9% | zero → real |
| `pfx_chars` | −7.5 … −8.5% | −6.7 … −12.9% | roughly same |
| `lexer` | −13.4 … −19.1% | −17.4 … −18.2% | roughly same |

The rails whose SSO cost is per-iteration serial work (`substr`, `pfx_idx`) get
COMPRESSED, because parallelism hides it; the rails that pay it in allocator
traffic (`builder*`, `promote`) get AMPLIFIED, plausibly because four workers
contend on the allocator. That mechanism is a hypothesis and has not been
measured. The amplification has: `builder60` is four times worse fanned out
than pinned, consistently across three samples.

`lexlike` is the control that makes the table readable, and it holds — the
analyzer declines to fan it out (175–177 ms pinned, 175–176 ms fanned), so its
delta is identical under both settings.

**Auto-par MASKS the pinned `substr` regression, which is what the pin is for.**
An earlier revision claimed `substr` was a 14.7% SSO *win* under default
auto-par and a 30.9% loss pinned — a sign flip. The win half does not
reproduce. Six trials at the default now: −1.7% / 0.0% / −1.7% and
+0.0% / −1.7% / +1.7%, straddling zero, none within 13 points of −14.7%; a
`KARAC_PAR_WORKERS` sweep gives roughly zero at 1, 2, 4, 8 and 18 workers. That
−14.7% was one run on a loaded box. The correct, weaker statement: fanned out,
`substr`'s delta collapses to about zero while pinned it is the worst rail
here. Consistent with the pre-pin header's "+5%".

**THE GAP WAS NOT A FINDING — B-2026-09-15-6 IS CLOSED `invalid`.** The two
rails cost the SAME under SSO. Best-of-15, auto-par pinned, both printing
`200000`:

| rail | `KARAC_SSO=0` | `KARAC_SSO=1` (pre-`9d3ceb9`) | `KARAC_SSO=1` now |
|---|---|---|---|
| `substr` | **126 ms** | 217 ms | **22 ms** |
| `lexlike` | **176 ms** | 215 ms | 215 ms (unchanged) |

**`9d3ceb9` moved `substr` and nothing else.** It made `String.substring` emit
its inline encoding as IR instead of calling `karac_string_try_inline_into`:
5.7x faster than the `KARAC_SSO=0` baseline on x86-64, and 13.9x faster than it
on arm64 (6.8x over the call). `lexlike` is untouched because `s[a..b]` routes
through `karac_string_slice_into`, a different entrypoint with the same
opaque-call shape — so the two rails that used to be equal at `SSO=1` no longer
are, and the reason is which construction path each one takes.

0.9% apart at `SSO=1`, samples interleaving. The "46–52 point gap" was
(+72%) − (+23%) — two ratios with **equal numerators** and unequal denominators.
The rails differ with SSO **off**, and SSO erases the difference. The arm64
measurement said the same thing from the other side ("at `SSO=1` the two rails
are indistinguishable") and was read as *no arm64 instance of the gap* rather
than as *the gap is arithmetic*.

**What the IR diff found instead: B-2026-09-15-13.** The hot-path string call
in each leg:

| leg | call |
|---|---|
| `substr` @ `SSO=0` | **none** — bounds/UTF-8 checks as IR blocks, `karac_alloc_or_panic` + `memcpy` |
| `substr` @ `SSO=1` | **`karac_string_try_inline_into`** ← inserted by SSO |
| `lexlike` @ `SSO=0` | `karac_string_slice` |
| `lexlike` @ `SSO=1` | `karac_string_slice_into` |

`lexlike` pays an opaque call in both legs, so SSO costs it little; `substr`'s
lowering was call-free and SSO adds one, so it lands where `lexlike` already
was. Replacing that call with the equivalent IR — `memcpy`, zero-fill, byte-23
`0x80 | len` flag, exactly `KaracString::write_inline` — runs the rail in
**22 ms** against 212 ms: 9.6x faster than the call and **5.7x faster than the
non-SSO baseline**, with correct output and linear scaling at 10M / 20M / 40M
iterations. Shipped as `9d3ceb9`; real codegen measures 22 ms, matching the
hand-patched prediction.

Two hypotheses were refuted on the way, both of them ours. Annotating the call
`memory(argmem: readwrite) nounwind willreturn` did nothing (213 vs 212 ms), and
hand-folding the comparison's literal side did nothing (211 ms) — so the
store-to-load-forwarding shape, real as it is, is not where the time goes. The
call is an optimization barrier.
