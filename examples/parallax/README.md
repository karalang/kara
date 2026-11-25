# Parallax

A worked example of Kāra's auto-parallelization: write straight-line
sequential code, the compiler runs it concurrently.

> **What holds on 0.8.** The package builds and runs, but **auto-par runs
> the four fetches in order** in this version of the example: each provider's
> `fetch_*` prints a "loaded ..." line, and two statements that both write
> `Stdout` must keep their order, or the output would change from run to run.
> That is the specified rule, not a compiler gap: automatic parallelism runs
> pieces in parallel only where the conflict test `par` uses proves them
> independent ([design.md](../../docs/design.md), "The compiler also
> parallelizes loops and statements on its own"; core-semantics.md §11.5),
> and two `writes(Stdout)` conflict. [parallax_lite](../parallax_lite/README.md),
> whose fetches print nothing, runs its three calls in parallel.
> `karac query concurrency` (on the flattened package, as in
> [parallax_lite](../parallax_lite/README.md#what-it-demonstrates)) reports
> the four `let`s as one parallel group, then `tasks: false` with the reason
> "`writes(Stdout)` and `writes(Stdout)` must keep their order". A default
> build and a `KARAC_AUTO_PAR=0` build both ran in 0.027 s (examples probe,
> 2026-11-16, 4-core Linux x86_64 container). The description below is the
> design the example was written for, on the pre-M3 backend.

## What it demonstrates

The canonical fan-out + join shape (`docs/dogfooding.md § Parallax`) —
four typed effect resources, four provider implementations, and a
`get_dashboard(user_id)` function whose four `let` bindings the
compiler auto-parallelizes into a single concurrent group, joined
into a `Dashboard` aggregate.

```kara,ignore
pub fn get_dashboard(user_id: i64) -> Dashboard
    with reads(UserDB) reads(OrderDB) reads(NotifDB) reads(RecommendDB)
{
    let profile = fetch_profile(user_id);
    let latest_orders = fetch_latest_orders(user_id);
    let top_notification = fetch_top_notification(user_id);
    let top_recommendation = fetch_top_recommendation(user_id);
    Dashboard {
        profile: profile,
        latest_orders: latest_orders,
        top_notification: top_notification,
        top_recommendation: top_recommendation,
    }
}
```

The auto-par analyzer sees no conflict edges between the four `let`s
(`reads(UserDB)` vs `reads(OrderDB)` are disjoint resources, no shared
writes), groups them into one parallel group, fans out across four
worker threads, and joins their results back into the outer scope so
the tail expression's `Dashboard { ... }` constructor reads them as
ordinary in-scope bindings.

No `async`. No `await`. No `Promise.all`. No explicit `par {}` block.
The parallelism is inferred entirely from the effect declarations.

## Differences from `parallax_lite`

[`examples/parallax_lite/`](../parallax_lite/) is the sister demo —
three resources, write-only effects, no joined return. Lighter surface,
runs as a microbenchmark.

This one adds:

- **Four resources instead of three** — exercises a four-deep
  `with_provider[R]` chain in `main.kara`.
- **Typed return values** — `Profile`, `Order`, `Notification`,
  `Recommendation` flow back from each parallel branch into the
  joined `Dashboard`.
- **Read-only fetches** with `ref self` receivers — the natural shape
  for a database-fetch workload.

## How to run

The project uses the package layout (`kara.toml` + `src/*.kara`);
`karac build` from the package root builds it to `dist/native/parallax`:

```sh
cd examples/parallax/

# auto-parallelized (default)
karac build
./dist/native/parallax

# sequential baseline — same source, same binary, opt-out via env var
KARAC_AUTO_PAR=0 karac build
./dist/native/parallax
```

`KARAC_AUTO_PAR=0` turns auto-par off for that build.

`karac build --concurrency-report` prints what auto-par did and why.

## Files

- `kara.toml` — project manifest.
- `src/types.kara` — `Profile`, `Order`, `Notification`,
  `Recommendation`, `Dashboard`.
- `src/traits.kara` — four single-method provider traits
  (`UserDatabase`, `OrderDatabase`, `NotificationDatabase`,
  `RecommendationDatabase`), each with one `fetch_*` method
  taking `ref self`.
- `src/resources.kara` — four typed effect resources
  (`UserDB / OrderDB / NotifDB / RecommendDB`) bound to their
  respective traits.
- `src/providers.kara` — four `InMemory*DB` provider implementations.
  Each `fetch_*` runs a CPU-bound busy-compute kernel sized
  proportionally to "simulated I/O latency" (10M / 30M / 15M / 20M
  iterations) before returning a canned result.
- `src/workload.kara` — `fetch_*` wrapper functions (each declared
  `with reads(R)`) and the `get_dashboard(user_id)` workload itself.
- `src/main.kara` — entry point. Four-deep `with_provider` chain
  wraps a single `get_dashboard(42)` call.

## See also

- [`examples/parallax_lite/`](../parallax_lite/) — sister demo;
  three-resource fan-out without join.
- `docs/dogfooding.md § Parallax` — the demo's design storyboard.

## Implementation notes

The auto-par decision is made in `src/mir/effects/auto_par.rs`; the effect
analysis that forms the groups is `src/concurrency.rs`. The pre-M3 backend
this example was first built on (`src/codegen.rs`, its return-slot ABI and
`with_provider` lowering) was deleted at M3.
