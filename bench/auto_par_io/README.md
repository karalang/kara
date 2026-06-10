# auto_par_io — does auto-par overlap independent I/O?

Before/after harness for **path A**: making the auto-parallelizer overlap
independent I/O the way the design says it should, instead of serializing it.

## The question

`docs/design.md` promises auto-parallel I/O fan-out:

- **:5907** — *"Conflict analysis ignores [`blocks`/`suspends`]: two `blocks`
  tasks do not conflict in the effect sense, and the scheduler is still free to
  parallelize them"* (it uses the verbs only to choose a thread pool).
- **:9044** (worked example) — three independent `http_get` calls *"auto-concurrency
  runs them in parallel… the task parks while waiting for the network response."*

The implementation contradicts this. `src/concurrency.rs:1232-1233` marks
`(Blocks,Blocks) => true` and `(Suspends,Suspends) => true` as **conflicts**, so
independent I/O statements are serialized. Proven: adding `suspends` to two
otherwise-parallelizable `reads(Network)` calls flips the plan from `[[0,1]]`
to `[]`.

## What this measures

Three rails, all re-measured in the **same run** (so the verdict is a ratio —
load- and thermal-immune — not an absolute against a stale baseline):

| Rail | Build | Meaning |
|---|---|---|
| `seq` | `KARAC_AUTO_PAR=0 karac build <straight>.kara` | serial **floor** |
| `par` | `karac build <par>.kara` (explicit `par {}`) | overlap **ceiling** |
| `auto`| `karac build <straight>.kara` | **under test** |

`KARAC_AUTO_PAR` is a *compile-time* gate (read at `Codegen` construction), so
the floor needs a separately built binary — runtime `KARAC_AUTO_PAR=0 ./bin` is
a no-op.

The probe sources are **generated** by `bench.sh` (not committed) — for each `K`
it writes a straight-line file of `K` independent `usleep(D)` calls and a `par {}`
sibling. The shape is:

```kara
unsafe extern "C" { fn usleep(usecs: u32) -> i32; }
fn main() {
    usleep(200000);   // ×K, independent — no dataflow, default `blocks` effect
    ...
    println(0);
}
```

Two **deterministic** guards (exact, not timed) are the real assertions; wall-clock
just confirms grouping becomes overlap:

- **`grouped?`** — do `query concurrency` stmts 0 & 1 (two of the `usleep` calls)
  share a group? (More precise than raw `par_run` presence, which fires even for a
  useless trivial group like `[last-usleep, println]`.)
- **positive control** — `positive_control.kara` (the parallax distinct-resource
  shape) must still emit `par_run` + group `[0,1,2]`. If it doesn't, the rig is
  broken and the K-sweep RED is meaningless.

## Why a K-sweep, not a longer run

K=4 on an ~18-core machine is the *easy* case — every blocking call gets its own
pool worker. The robustness question a single K hides is **scaling past the pool
worker count**: at K=64 a correct fix overlaps in pool-bounded *waves*
(`~ceil(K/cores)×D`), and `par` hits the same ceiling, so it stays a fair
reference. A fix that overlaps at K=4 but wave-serializes at K=64 is caught here.
(Longer per-call *duration* adds wall-time, not signal — `usleep` is deterministic,
so the floor↔ceiling ratio is already unambiguous.)

## Run

```bash
./bench.sh                 # needs: karac on PATH, python3. timeout/gtimeout optional.
KSWEEP=4 ./bench.sh        # fast single-K during A1 dev
D_US=100000 ./bench.sh     # shorter waits (probes the dispatch break-even)
```

## Baseline — BEFORE path A (2026-06-10, M5 Pro ~18 cores, D=200ms)

```
   K | grouped? |   seq   |   par   |  auto   | verdict
   4 | no       |  1.01s  |  0.39s  |  1.03s  | SERIAL
  16 | no       |  3.45s  |  0.40s  |  3.47s  | SERIAL
  64 | no       | 13.36s  |  1.02s  | 13.24s  | SERIAL
positive ctrl: plan [0,1,2] · par_run yes  => OK (rig sees auto-par fire)
```

`auto` tracks the serial floor at every K. The runtime *already* overlaps blocking
work — `par` stays flat (~0.4s) to the pool size, then climbs in waves to ~1.0s at
K=64 (`ceil(64/18)·200ms` ≈ 0.8s + fixed runtime/dispatch startup ~0.2s). That
~0.2s floor is also why `par` at K=4 isn't quite `1×D`; at much smaller `D` it
becomes the break-even (the `D_US=` knob probes it). The gap auto-par leaves on
the table: **2.6× at K=4, 13× at K=64.**

## Result — AFTER A1 (`blocks`) ✅ DONE 2026-06-10

`(Blocks,Blocks) => false` in `src/concurrency.rs::two_effects_conflict`
(phase-5-diagnostics.md, "Auto-par conflict model…" A1). Measured, same machine:

```
   K | grouped? |   seq   |   par   |  auto   | verdict   win
   4 | yes      |  0.82s  |  0.20s  |  0.20s  | OVERLAP   4.1×
  16 | yes      |  3.24s  |  0.20s  |  0.20s  | OVERLAP   16×   (16 ≤ pool: full overlap)
  64 | yes      | 13.05s  |  0.81s  |  0.81s  | OVERLAP   16×   (≈4 pool-bounded waves)
```

`auto` migrated from the serial floor to the (pool-bounded) `par` ceiling at
*every* K, and stmts 0 & 1 co-group. The existing `par_run` fan-out overlaps the
blocking calls on the pool — no runtime/codegen change, only the conflict model.

## Not here yet — `suspends` (A2)

No `suspends` probe: it needs a deterministic *async* wait primitive (the runtime
exposes no async sleep today — see `examples/parallax_lite/src/resources.kara`).
That primitive is the first brick of **A2**, so the suspends probe lands with it.
Caveat for that probe: `par {}` of suspending calls overlaps via *threads* (a
modest ceiling); the real suspends win — millions of tasks parked on a few
threads — is a separate *scaling* bench, not this latency-overlap one.
