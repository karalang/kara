# Concurrency

> *This chapter is a work in progress.*

Kāra's concurrency story is built on a simple idea: **if the compiler can prove two operations don't interfere, it can run them in parallel.** The effect system makes this possible.

## Automatic parallelization

Consider a function that fetches data from three independent sources:

```kara
fn build_dashboard(user_id: u64) -> Dashboard
    with reads(UserDB) reads(OrderDB) reads(Analytics)
{
    let profile = fetch_profile(user_id);       // reads(UserDB)
    let orders = fetch_orders(user_id);         // reads(OrderDB)
    let stats = fetch_analytics(user_id);       // reads(Analytics)
    Dashboard.new(profile, orders, stats)
}
```

The three fetches operate on different resources. The compiler proves they don't conflict and runs them concurrently — zero threading code from you.

This is possible *because* of the effect system. Without knowing which resources each call touches, the compiler couldn't prove independence. Effects are what make auto-concurrency safe.

## Explicit concurrency with par

When you want to be explicit about parallelism:

```kara
let (users, products) = par {
    fetch_users(),
    fetch_products(),
};
```

`par` runs its branches concurrently and waits for all to complete. It's structured concurrency — no dangling tasks, no fire-and-forget.

## spawn for background work

```kara
let handle = spawn(long_computation());
// ... do other work ...
let result = handle.await;
```

## Parallel failure

When one branch of a `par` block fails:

1. Sibling branches are cancelled cooperatively.
2. Each branch's cleanup (`defer`/`errdefer`) runs.
3. The first error is returned.

No orphaned tasks. No silent failures. Structured concurrency means the scope waits for everything to finish before proceeding.

## The runtime

Kāra's concurrency runtime uses work-stealing with a thread pool. The details are an implementation choice — your code doesn't depend on them. You write sequential-looking code with effect annotations; the compiler and runtime handle the rest.
