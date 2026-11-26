# Concurrency

Kāra's concurrency story is built on a simple idea: **if the compiler can prove two operations don't interfere, it can run them in parallel.** The effect system makes this possible.

## Automatic parallelization

Consider a function that fetches data from three independent sources:

```kara,uses=b-dashboard
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

A `par` block runs each of its branches concurrently and waits for all of them before control falls through. It's structured concurrency — no dangling tasks, no fire-and-forget. Each branch is an expression, separated by commas; they share data through a concurrency-safe type rather than by writing the same plain variable:

```kara,run
import std.sync.{Atomic};

sync struct Counter { count: Atomic[i64] }

fn bump(counter: Counter) {
    let _ = counter.count.fetch_add(1);
}

fn main() {
    let counter: Counter = Counter { count: Atomic.new(0) };
    par {
        bump(counter),
        bump(counter),
        bump(counter),
    };
    println(counter.count.load());   // 3
}
```

`sync struct` marks a type as safe to share across branches; its `Atomic[T]` field carries the shared state. The effect system enforces this: if a `par` branch tried to write an ordinary `let mut` shared by a sibling, the compiler would reject it (`E0418`, naming both accesses); shared state between branches goes through `Atomic`, `Mutex` or a `sync struct`. No data races slip through.

## TaskGroup for dynamic fan-out

`par` is for a fixed set of branches written out in the source. When the number of tasks is decided at runtime — split an image into `workers` bands, process N rows — use a `TaskGroup`: spawn each task, collect its `TaskHandle[T]`, then `join` each to gather the results.

```kara,run
import std.sync.{TaskGroup, TaskHandle};

fn square(n: i64) -> i64 { n * n }

fn main() {
    let pool: TaskGroup = TaskGroup.new();
    let mut handles: Vec[TaskHandle[i64]] = Vec.new();
    let mut k = 1;
    while k <= 4 {
        let n = k;                          // a fresh binding per task
        handles.push(pool.spawn(|| square(n)));
        k = k + 1;
    }

    let mut total = 0;
    for handle in handles.into_iter() {
        total = total + handle.join();      // wait for each, collect its result
    }
    println(total);                          // 1 + 4 + 9 + 16 = 30
}
```

`pool.spawn` takes a thunk (a zero-argument closure) and returns a `TaskHandle[T]`; `.join()` waits for that task and hands back its return value. Bind a fresh `let n = k` inside the loop so each closure captures its own value rather than the shared loop counter. There is no `async`/`await` — a spawned task is just a function call that happens elsewhere, with `suspends` tracking any cooperative yielding (see [Effects](./ch11-effects.md#execution-verbs-blocks-and-suspends)).

## Parallel failure

What happens when a `par` branch fails depends on how it fails.

- **An error (`?`) ends that branch.** Its drops, `defer`s and `errdefer`s run.
  The branches written before it run to completion. The ones after it are
  cancelled at their next suspension point; a branch that never suspends runs
  to completion too.
- **When every branch is done,** the enclosing function returns the
  *source-earliest* error, whichever branch failed first in time. The result is
  the same on every run.
- **A panic ends the process** with exit code 101, in any branch or task. No
  drops, `defer`s or `errdefer`s run. Chapter 20 shows what the panic prints.

A block whose branches produce `Result` values without `?` returns all of them,
in source order, so you can report every failed field rather than only the
first.

No orphaned tasks: the scope waits for every branch before it goes on.

## The runtime

Kāra's concurrency runtime uses work-stealing with a thread pool. The details are an implementation choice — your code doesn't depend on them. You write sequential-looking code with effect annotations; the compiler and runtime handle the rest.
