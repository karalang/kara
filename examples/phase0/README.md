# Phase 0: Proof of Value

**Effect types + auto-concurrency, demonstrated end-to-end.**

## The Pitch (5 minutes)

### 1. The programmer writes sequential-looking code

See [`dashboard.kara`](dashboard.kara). A function loads a user dashboard by fetching from three data sources and assembling the results:

```
fn main() {
    let user_id = env.args().get(1).unwrap_or("1").parse_u64()?;

    let profile = fetch_profile(user_id)?;       // reads(UserDB)
    let orders = fetch_orders(user_id)?;          // reads(OrderDB)
    let notifs = fetch_notifications(user_id)?;   // reads(NotifDB)

    let dashboard = build_dashboard(profile, orders, notifs);
    println(dashboard.greeting);
    println(dashboard.order_summary);
    println(dashboard.alert);
}
```

No `async fn`. No colored functions. No channels. No thread pools. Just sequential code with effect annotations declaring *what* each function touches.

### 2. The compiler parallelizes it

The compiler analyzes effects:

| Pair | Same resource? | Verdict |
|------|---------------|---------|
| `reads(UserDB)` + `reads(OrderDB)` | No | **Safe** |
| `reads(UserDB)` + `reads(NotifDB)` | No | **Safe** |
| `reads(OrderDB)` + `reads(NotifDB)` | No | **Safe** |

No conflicts. No data dependencies between the three fetches. The compiler spawns three concurrent tasks and inserts a sync point before `build_dashboard`, which consumes all three results.

Compare the generated code:
- [`sequential.rs`](sequential.rs) — what a naive translation produces (~600ms)
- [`parallel.rs`](parallel.rs) — what the Kāra compiler generates (~200ms)

### 3. The benchmark proves it

```bash
$ bash bench.sh

=== Kāra Phase 0: Effect-Driven Auto-Concurrency Benchmark ===

--- Sequential (no concurrency) ---
  Average: ~600ms

--- Parallel (compiler auto-parallelized) ---
  Average: ~200ms

=== Result: 3.0x speedup ===

The programmer wrote sequential-looking code with effect annotations.
The compiler parallelized it automatically. Zero effort. 3.0x faster.
```

### 4. The compiler explains its decisions

See [`compiler_output.json`](compiler_output.json) for the full structured output. Key excerpts:

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
# Compile and run both versions
rustc examples/phase0/sequential.rs -o /tmp/kara_seq && /tmp/kara_seq
rustc examples/phase0/parallel.rs   -o /tmp/kara_par && /tmp/kara_par

# Run the benchmark
bash examples/phase0/bench.sh
```

## Why this matters

In other languages, getting that 3x speedup requires the programmer to:
1. Identify independent operations (manual analysis)
2. Choose a concurrency mechanism (`async/await`, threads, channels)
3. Restructure code to be concurrent (non-trivial refactor)
4. Handle cancellation, error ordering, synchronization (error-prone)

In Kāra, effect annotations provide all the information the compiler needs. The programmer writes the obvious sequential code. The compiler does the rest.
