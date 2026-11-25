# Phase 0: Proof of Value

> **Legacy.** Moved from `examples/phase0/` on 2026-11-25: a design-era mockup,
> kept for its history rather than beside the live examples.

**Effect types + auto-concurrency: the design-phase pitch.**

> **What this is today.** This directory predates the compiler. `parallel.rs`
> and `sequential.rs` are hand-written Rust models of the transformation, and
> `bench.sh` compiles and times those two Rust files: it never runs `karac`, so
> its speedup is a property of the model, not a measurement of Kāra.
> `compiler_output.json` is a hand-written mockup of the report the design
> called for (its `karac_version` is `0.1.0-dev`).
>
> On 0.8, `dashboard.kara` checks and runs (`karac run dashboard.kara`), and
> the compiler's real verdict comes from
> `karac query concurrency dashboard.kara.load_dashboard`: it **declines** to
> run the three fetches as tasks, with the reason "statement 1 may stop before
> statement 2 does `panics`". Each fetch ends in `?`, so the first can return
> early, and running the later ones anyway would perform effects the
> sequential program never reaches. Writing the three fetches as a
> `par { ... }` block is the explicit form
> ([Chapter 14](../../../book/src/ch14-concurrency.md)).

## The Pitch (5 minutes)

### 1. The programmer writes sequential-looking code

See [`dashboard.kara`](dashboard.kara). A function loads a user dashboard by fetching from three data sources and assembling the results:

```kara,ignore
fn load_dashboard(user_id: u64) -> Result[Dashboard, DbError] {
    let profile = fetch_profile(user_id)?;          // reads(UserDB)
    let orders = fetch_orders(user_id)?;            // reads(OrderDB)
    let notifs = fetch_notifications(user_id)?;     // reads(NotifDB)

    // Sync point: build_dashboard needs all three results.
    Ok(build_dashboard(profile, orders, notifs))
}
```

No `async fn`. No colored functions. No channels. No thread pools. Just sequential code: the compiler infers *what* each function touches.

### 2. The effect analysis

The design's analysis of the effects:

| Pair | Same resource? | Verdict |
|------|---------------|---------|
| `reads(UserDB)` + `reads(OrderDB)` | No | **Safe** |
| `reads(UserDB)` + `reads(NotifDB)` | No | **Safe** |
| `reads(OrderDB)` + `reads(NotifDB)` | No | **Safe** |

No resource conflicts and no data dependencies between the three fetches, so the design spawns three concurrent tasks and inserts a sync point before `build_dashboard`, which consumes all three results. (The 0.8 compiler also weighs the early return of each `?`; see the note above.)

The two hand-written Rust models of that transformation:
- [`sequential.rs`](sequential.rs) — the fetches one after another (~600ms of simulated latency)
- [`parallel.rs`](parallel.rs) — the fetches on three threads (~200ms)

### 3. The model's benchmark

```bash
$ bash bench.sh

=== Kāra Phase 0: Effect-Driven Auto-Concurrency Benchmark ===

--- Sequential (no concurrency) ---
  Average: ~600ms

--- Parallel (parallel.rs, three threads) ---
  Average: ~200ms

=== Result: 3.0x speedup ===

This times two hand-written Rust programs, sequential.rs and parallel.rs.
It models the transformation; it does not run karac.
```

### 4. The report the design called for

[`compiler_output.json`](compiler_output.json) is a hand-written mockup of the structured report. The 0.8 compiler's real report is `karac query concurrency`. Key excerpts from the mockup:

**Effect query** — what effects does `main` have?
```json
"declared_effects": ["reads(Env)", "reads(UserDB)", "reads(OrderDB)", "reads(NotifDB)", "writes(Stdio)"]
```

**Concurrency report** — what did the compiler parallelize and why?
```json
"decision": "PARALLELIZE",
"reason": "All three tasks have non-conflicting effects (reads on distinct resources) and no data dependencies. Spawning 3 tasks."
```

**Sync point** — where does it join?
```json
"statement": "let dashboard = build_dashboard(profile, orders, notifs)",
"reason": "build_dashboard consumes all three results — must join before proceeding"
```

## Running the demo

```bash
# The Kāra program itself
karac run examples/phase0/dashboard.kara
karac query concurrency examples/phase0/dashboard.kara.load_dashboard

# The two hand-written Rust models
rustc examples/phase0/sequential.rs -o /tmp/kara_seq && /tmp/kara_seq
rustc examples/phase0/parallel.rs   -o /tmp/kara_par && /tmp/kara_par

# Time the two Rust models
bash examples/phase0/bench.sh
```

## Why this matters

In other languages, getting that 3x speedup requires the programmer to:
1. Identify independent operations (manual analysis)
2. Choose a concurrency mechanism (`async/await`, threads, channels)
3. Restructure code to be concurrent (non-trivial refactor)
4. Handle cancellation, error ordering, synchronization (error-prone)

In Kāra, inferred effects give the compiler the information it needs. On 0.8 it takes the parallel step itself only when no early return can skip a later effect; otherwise one line says it explicitly, and the compiler checks that the branches are independent:

```kara,ignore
let (p, o, n) = par { fetch_profile(user_id), fetch_orders(user_id), fetch_notifications(user_id) };
```
