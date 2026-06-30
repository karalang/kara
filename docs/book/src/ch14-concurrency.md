# Concurrency

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

A `par` block runs each of its branches concurrently and waits for all of them before control falls through. It's structured concurrency — no dangling tasks, no fire-and-forget. Each branch is an independent statement; they share data through a concurrency-safe type rather than by writing the same plain variable:

```kara
par struct Counter { count: Atomic[i64] }

fn bump(counter: ref Counter) {
    let _ = counter.count.fetch_add(1, MemoryOrdering.Relaxed);
}

fn main() {
    let counter: Counter = Counter { count: Atomic.new(0) };
    par {
        bump(counter);
        bump(counter);
        bump(counter);
    }
    println(counter.count.load(MemoryOrdering.Relaxed));   // 3
}
```

`par struct` marks a type as safe to share across branches; its `Atomic[T]` field carries the shared state. The effect system enforces this: if a `par` branch tried to write an ordinary `let mut` shared by a sibling, the compiler would reject it and tell you to reach for `Atomic`, `Mutex`, or a `par struct`. No data races slip through.

## TaskGroup for dynamic fan-out

`par` is for a fixed set of branches written out in the source. When the number of tasks is decided at runtime — split an image into `workers` bands, process N rows — use a `TaskGroup`: spawn each task, collect its `TaskHandle[T]`, then `join` each to gather the results.

```kara
fn square(n: i64) -> i64 { n * n }

fn main() {
    let mut pool: TaskGroup = TaskGroup.new();
    let mut handles: Vec[TaskHandle[i64]] = Vec.new();
    let mut k = 1;
    while k <= 4 {
        let n = k;                          // a fresh binding per task
        handles.push(pool.spawn(|| square(n)));
        k = k + 1;
    }

    let mut total = 0i64;
    for handle in handles {
        total = total + handle.join();      // wait for each, collect its result
    }
    println(total);                          // 1 + 4 + 9 + 16 = 30
}
```

`pool.spawn` takes a thunk (a zero-argument closure) and returns a `TaskHandle[T]`; `.join()` waits for that task and hands back its return value. Bind a fresh `let n = k` inside the loop so each closure captures its own value rather than the shared loop counter. There is no `async`/`await` — a spawned task is just a function call that happens elsewhere, with `suspends` tracking any cooperative yielding (see [Effects](./ch11-effects.md#execution-verbs-blocks-and-suspends)).

## Parallel failure

When one branch of a `par` block fails:

1. Sibling branches are cancelled cooperatively.
2. Each branch's cleanup (`defer`/`errdefer`) runs.
3. The first error is returned.

No orphaned tasks. No silent failures. Structured concurrency means the scope waits for everything to finish before proceeding.

## The runtime

Kāra's concurrency runtime uses work-stealing with a thread pool. The details are an implementation choice — your code doesn't depend on them. You write sequential-looking code with effect annotations; the compiler and runtime handle the rest.
