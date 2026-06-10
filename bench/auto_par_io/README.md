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
| `seq` | `KARAC_AUTO_PAR=0 karac build blocks_fanout.kara` | serial **floor** |
| `par` | `karac build blocks_fanout_par.kara` (explicit `par {}`) | overlap **ceiling** |
| `auto`| `karac build blocks_fanout.kara` | **under test** |

`KARAC_AUTO_PAR` is a *compile-time* gate (read at `Codegen` construction), so
the floor needs a separately built binary — runtime `KARAC_AUTO_PAR=0 ./bin` is
a no-op.

Two **deterministic** guards (exact, not timed) are the real assertions; wall-clock
just confirms grouping becomes overlap:

- **`blocking grouped?`** — do `query concurrency` stmts 0 & 1 (two of the four
  `usleep` calls) share a group? (More precise than raw `par_run` presence, which
  fires even for a useless trivial group like `[last-usleep, println]`.)
- **positive control** — `positive_control.kara` (the parallax distinct-resource
  shape) must still emit `par_run` + group `[0,1,2]`. If it doesn't, the rig is
  broken and the blocks RED is meaningless.

## Run

```bash
./bench.sh        # needs: karac on PATH, python3. timeout/gtimeout optional.
```

## Baseline — BEFORE path A (2026-06-10, M5 Pro, K=4 usleep(400ms))

```
blocks fan-out:  blocking grouped? no   seq 1.61s · par 0.40s · auto 1.61s  => SERIAL
positive ctrl :  plan [0,1,2] · par_run yes  => OK (rig sees auto-par fire)
```

`auto` sits at the serial floor; the runtime can already overlap blocking work
(`par` = 0.40s, a **4×** gap left on the table).

## Acceptance target — AFTER A1 (`blocks`)

```
blocks fan-out:  blocking grouped? yes  auto ≈ par ≈ 0.40s  => OVERLAPPING
```

i.e. `auto` migrates from the 1.61s floor to the 0.40s ceiling, and stmts 0 & 1
co-group. A1 = stop treating `blocks+blocks` as a conflict; the existing
`par_run` fan-out then overlaps them on the blocking pool.

## Not here yet — `suspends` (A2)

No `suspends` probe: it needs a deterministic *async* wait primitive (the runtime
exposes no async sleep today — see `examples/parallax_lite/src/resources.kara`).
That primitive is the first brick of **A2**, so the suspends probe lands with it.
Caveat for that probe: `par {}` of suspending calls overlaps via *threads* (a
modest ceiling); the real suspends win — millions of tasks parked on a few
threads — is a separate *scaling* bench, not this latency-overlap one.
