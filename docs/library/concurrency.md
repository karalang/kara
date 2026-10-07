# Concurrency

This file specifies the concurrency APIs in `std.sync`: channels, `TaskGroup`, `Mutex` and atomics. The language side (`par {}`, `par for`, what may enter a task, which branches conflict, `sync` types, and what happens when a branch fails) is [design.md §13](../design.md#13-concurrency), with the rules in [core-semantics.md §9.5 and §11](../core-semantics.md#11-concurrency-c9).

Tasks start only in `par {}`, `par for` and `TaskGroup.spawn`. There is no free `spawn` function.

## Channels

```kara
fn channel[T: CrossTask](cap: i64) -> (Sender[T], Receiver[T])
```

`channel` creates a bounded channel that holds at most `cap` messages, and returns its two ends: `Sender[T]` writes and `Receiver[T]` reads. `cap` must be at least 1; `channel(0)` is a compile error. For a one-shot notification, use capacity 1. `T` must be able to cross between tasks (`CrossTask`, [design.md §9](../design.md#crosstask)); in particular a view cannot go into a channel ([core-semantics.md §5.7](../core-semantics.md#5-references-and-views-c5)).

The element type comes from how the ends are used, or from an annotation, since calls take no type arguments:

```kara
let (tx, rx): (Sender[Job], Receiver[Job]) = channel(64);
```

**Operations.**

```kara
impl[T] Sender[T] {
    fn send(self, value: own T) -> Result[(), SendError[T]] with sends(self) blocks
    fn try_send(self, value: own T) -> Result[(), SendError[T]] with sends(self)
    fn clone(self) -> Sender[T]
}

impl[T] Receiver[T] {
    fn recv(self) -> Option[T] with receives(self) blocks
    fn try_recv(self) -> Option[T] with receives(self)
}

enum SendError[T] {
    Full(T),     // the channel is at capacity (from try_send only)
    Closed(T),   // no Receiver remains
}
```

- `send` moves `value` into the channel, waiting while the channel is full. If no `Receiver` remains, it returns `Err(SendError.Closed(value))`, handing the value back.
- `recv` waits while the channel is empty and returns the next message. It returns `None` once every `Sender` has dropped and the buffered messages are drained.
- `try_send` never waits: it returns `Err(SendError.Full(value))` when the channel is full and `Err(SendError.Closed(value))` when no `Receiver` remains.
- `try_recv` never waits: it returns `None` when no message is available.
- `Receiver[T]` implements `IntoIterator` with item `T`, so `for msg in rx.into_iter() { ... }` receives until the channel is closed and drained.

Sending returns a `Result` instead of panicking because a panic ends the whole process ([core-semantics.md §10](../core-semantics.md#10-panics-and-errors-c8)), while a send to a departed receiver is an ordinary condition that the caller may want to handle; the error carries the unsent value so nothing is lost.

**Multiple senders.** `Sender[T]` implements `Clone`, and every clone sends into the same channel. `Receiver[T]` does not implement `Clone`.

**Closing.** The channel closes when the last `Sender` drops: `recv` then drains the buffered messages and returns `None`. When the `Receiver` drops, every later `send`, and any `send` waiting on a full channel, returns `Err(SendError.Closed(value))`.

**Effects.** `tx.send(v)` has `sends(tx)` and `rx.recv()` has `receives(rx)`, keyed by the channel value ([core-semantics.md §12](../core-semantics.md#12-effects-soundness-defaults-c10), item 5), so two channels are two resources. Clones of one `Sender` name the same channel. Which `par` branches may use one channel together is [core-semantics.md §11.2](../core-semantics.md#11-concurrency-c9). `send` and `recv` also have `blocks`, because they may wait. On `wasm_wasi`, where tasks run one at a time, a waiting `recv` yields to the other tasks; if no task can run, the program panics with a deadlock message ([design.md §16](../design.md#concurrency-across-targets)).

**Fan-in.** Two `par` branches that send on clones of one `Sender` conflict, because sends on one channel keep their order ([core-semantics.md §11.2](../core-semantics.md#11-concurrency-c9)). To fan in from concurrent producers, run each producer, with its own clone, as a `TaskGroup` task: tasks are not checked for effect conflicts, and their messages arrive in no fixed order ([`TaskGroup`](#taskgroup)). `par` stays deterministic.

**Moving the ends.** `Sender[T]` and `Receiver[T]` are ordinary owned values. They can be moved into tasks, passed to functions and returned.

**Deferred.** `select` across several channels, receive and send timeouts, unbounded channels and fan-out or fan-in combinators come with the services track (M4a); see [deferred.md](../deferred.md#m4a-services).

```kara
import std.sync.{channel, Sender, Receiver, SendError};

enum Request { GoTo(i64), Stop }

fn producer(tx: own Sender[Request]) -> Result[(), SendError[Request]] {
    tx.send(Request.GoTo(5))?;
    tx.send(Request.GoTo(3))?;
    tx.send(Request.Stop)?;
    Ok(())
}   // tx drops here, which closes the channel

fn controller(rx: own Receiver[Request]) {
    for req in rx.into_iter() {   // ends once the channel is closed and drained
        handle(req);
    }
}

fn run() -> Result[(), SendError[Request]] {
    let (tx, rx) = channel(8);
    let (sent, _) = par { producer(tx), controller(rx) };
    sent
}
```

The two branches do not conflict: one only sends on the channel and the other only receives from it.

## `TaskGroup`

A `TaskGroup` runs a dynamic number of tasks, such as one per accepted connection, and joins them all when it drops.

```kara
impl TaskGroup {
    fn new(; limit: i64 = i64.MAX) -> TaskGroup
    fn spawn[T: CrossTask](self, f: own OnceFn() -> T) -> TaskHandle[T] with blocks
}

impl[T] TaskHandle[T] {
    fn join(own self) -> T with blocks
}
```

- **`new`** makes an empty group. `TaskGroup.new(limit: n)` lets at most `n` of its tasks run at once. Without `limit` the group is unbounded: the default, `i64.MAX`, is never reached.
- **`spawn`** starts `f` as a task that runs concurrently with the caller, and waits first while the group already runs `limit` tasks; that wait is its `blocks`. Its receiver is a borrowed `self`, so the functions and tasks that spawn into a group, such as a server's request handlers, take it as an ordinary borrowed parameter; spawns on one group are synchronized. `f` is not escaping: its captures are inferred place by place, so a task may borrow from the enclosing scope, and the group holds those borrows until it drops. The borrowing rules, including what a task spawned from inside another task may capture, are [core-semantics.md §9.5](../core-semantics.md#9-closures).
- **Dropping a group joins every task it started**, so every task finishes before the scope that owns the group ends, and the drop waits (`blocks`).
- **`TaskHandle[T]`** owns its task's result and borrows nothing, so it may outlive its group. `join` waits for the task and returns its result. After the group has dropped, every task is done, and `join` returns the stored result at once. A result that is never joined drops with its handle, or with the group if the handle dropped first.
- **Tasks are not checked against each other for effect conflicts.** The borrow rules and `CrossTask` keep them from racing on memory, but two tasks may both print, or both write to one database, and their order is not fixed ([design.md §13](../design.md#taskgroup)). `par {}` and `par for` are the deterministic constructs.
- **Failure.** A task that returns `Err` does not affect its siblings; the error reaches whoever joins its handle. A panic in any task ends the process ([core-semantics.md §11.4](../core-semantics.md#11-concurrency-c9)). v1 cancels nothing; cooperative cancellation and deadlines come with the services track (M4a).

An accept loop, one task per connection, at most 1000 at a time:

```kara
import std.net.{TcpListener, TcpError};
import std.sync.{TaskGroup};

fn serve(listener: TcpListener) -> Result[(), TcpError] {
    let tasks = TaskGroup.new(limit: 1000);
    loop {
        let conn = listener.accept()?;
        tasks.spawn(|| handle_client(conn));   // waits while 1000 handlers run
    }
}   // on an accept error, tasks drops and joins every running handler
```

The closure moves `conn` into its task, because `handle_client` takes it `own`. The handle `spawn` returns is dropped at once, so each handler's result drops when the group joins it.

**Collecting every result.** Keep the handles and join them one by one. Every task runs to completion whatever the others return, so for tasks that return `Result` this collects every result and every error:

```kara
fn fetch_all(urls: Slice[String]) -> Vec[Result[Page, FetchError]] {
    let group = TaskGroup.new();
    let mut handles = Vec.new();
    for url in urls {
        handles.push(group.spawn(|| fetch(url)));
    }
    let mut results = Vec.new();
    for h in handles.into_iter() {
        results.push(h.join());
    }
    results
}
```

Each task borrows its `url` from `urls`, which outlives the group. For a collection known up front, `par for` gives the same `Vec` more simply ([design.md §13](../design.md#par-for)).

## `Mutex[T]`

```kara
impl[T] Mutex[T] {
    fn new(value: own T) -> Mutex[T]
    fn lock(self) -> MutexGuard[T] with blocks
}

struct MutexGuard[T] {
    pub value: mut ref T,
    // private: the mutex it releases when it drops
}
```

`lock` waits until no other task holds the mutex, then returns a guard. The guard's `value` field is a `mut ref` to the protected value, so access is written through it: `g.value.count += 1`. Method lookup does not pass through the guard ([design.md §9](../design.md#method-resolution)), so a method of the protected value is called as `g.value.m()`. The guard releases the mutex when it drops at the end of its scope ([core-semantics.md §7](../core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)):

```kara
struct Totals { count: i64, sum: i64 }

sync struct Stats {
    name: String,
    totals: Mutex[Totals],
}

impl Stats {
    fn record(self, n: i64) {
        let g = self.totals.lock();
        g.value.count += 1;
        g.value.sum += n;
    }   // g drops here, releasing the lock
}
```

- **The guard is a view** of the mutex: it cannot outlive the mutex, and it cannot be stored in a field, a global or a channel ([core-semantics.md §5.7](../core-semantics.md#5-references-and-views-c5)).
- **No poisoning.** A panic ends the process, so a lock is never left held by a failed task.
- **Effect-transparent.** Code run while holding the lock has its ordinary effects, counted as if there were no lock; `lock` itself adds only `blocks`. The effect system orders access to whole resources at compile time, and a `Mutex` orders access within one resource at run time.

A `sync` type holds its mutable state in `Mutex[T]` and `Atomic[T]` fields. Their methods take a borrowed `self`, so those fields are never written `mut` ([design.md §11](../design.md#sync-types)). A `Mutex` can also be used on its own.

## Atomics

`Atomic[T]` exists for `T` = `bool`, `i32`, `i64`, `u32` and `u64`:

```kara
impl[T] Atomic[T] {
    fn new(value: T) -> Atomic[T]
    fn load(self) -> T
    fn store(self, value: T)
    fn swap(self, value: T) -> T
    fn compare_exchange(self, current: T, new: T) -> Result[T, T]
    fn fetch_add(self, n: T) -> T     // integer T only
    fn fetch_sub(self, n: T) -> T     // integer T only
}
```

- `load` reads the value and `store` replaces it.
- `swap` stores `value` and returns the previous value.
- `compare_exchange` stores `new` only if the value equals `current`. It returns `Ok(previous)` when it stored and `Err(actual)` when it did not.
- `fetch_add` and `fetch_sub` add or subtract `n` and return the previous value. They wrap on overflow, and never panic.

**Every operation is sequentially consistent**, as in Go: all tasks observe all atomic operations in one order, which agrees with each task's program order. There is no ordering argument.

A counter in a `sync` type, and a signal flag:

```kara
import std.sync.{Atomic};

sync struct Metrics {
    requests: Atomic[u64],
    bytes: Atomic[u64],
}

impl Metrics {
    fn record(self, n: u64) {
        self.requests.fetch_add(1);
        self.bytes.fetch_add(n);
    }
}

fn publish(ready: Atomic[bool]) {
    ready.store(true);
}

fn wait_ready(ready: Atomic[bool]) {
    while not ready.load() {}
}
```

**Atomics are effect-free.** Atomic operations contribute nothing to a function's effects. A function's effects come from the data the atomic guards, not from the atomic. Which task's operation comes first is not fixed ([design.md §13](../design.md#determinism-contract)); to order two updates across tasks, use a `Mutex` or a channel.

`fetch_and`, `fetch_or`, fences and weaker orderings are part of the systems track ([deferred.md](../deferred.md#systems)).
