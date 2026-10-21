# Connection pools: `Pool`

`Pool[T, E]` keeps a bounded set of reusable values, usually connections to a database or another service, and lends them out one at a time. Opening a connection is slow and a server limits how many it accepts, so a program opens a few, reuses them, and waits when all are in use. Database drivers, HTTP clients and other resource pools build on it. The items are in `std.pool`, which a program imports. (The generational-handle allocator is a different type, `SlotMap[T]`; see [collections.md](collections.md).)

```kara
import std.pool.{Pool, PoolError}
import std.time.{Duration}

struct Conn { id: i64 }

enum ConnectError { Refused, AuthFailed }

fn connect() -> Result[Conn, ConnectError] { Ok(Conn { id: 1 }) }

fn main() {
    let pool: Pool[Conn, ConnectError] = Pool.new(connect, 8, 16);
    match pool.acquire(Duration.ms(500)) {
        Ok(conn) => println(f"using {conn.val.id}"),
        Err(PoolError.Timeout) => println("timed out"),
        Err(PoolError.Busy) => println("busy"),
        Err(PoolError.CreateFailed(e)) => println("could not connect"),
        Err(PoolError.PoolClosed) => println("shutting down"),
    }
}
```

## Types

| Type | Meaning |
|---|---|
| `Pool[T, E]` | The pool of `T` values, made by a factory that fails with `E`. It has one owner and no `clone`; share it by reference. |
| `PooledConnection[T]` | One value lent out by the pool. Its field `val: T` is the value itself. Dropping it gives the value back. |
| `PoolError[E]` | Why `acquire` failed: `Timeout`, `Busy`, `PoolClosed` or `CreateFailed(E)`. |

## `Pool[T, E]`

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new(create_fn: own Fn() -> Result[T, E], max_connections: i64, max_waiters: i64) -> Pool[T, E]` | An empty pool. `create_fn` makes a value when the pool needs a new one; at most `max_connections` exist at once; at most `max_waiters` callers wait in `acquire` at once. |
| `acquire` | `fn acquire(ref self, timeout: Duration) -> Result[PooledConnection[T], PoolError[E]]` | Lends out a value. It reuses an idle one first; otherwise, below `max_connections`, it calls `create_fn`, and an `Err(e)` from it is `CreateFailed(e)`; otherwise it waits up to `timeout` for a value to come back, and fails with `Timeout`. When `max_waiters` callers are already waiting, it fails with `Busy` at once. |
| `release` | `fn release(ref self, conn: own PooledConnection[T])` | Gives `conn` back now rather than when it drops. A value lent out by a different pool is dropped, not added to this one. |
| `with_health_check` | `fn with_health_check(own self, check: own Fn(T) -> bool) -> Pool[T, E]` | Registers a check that `acquire` runs on each idle value before reusing it, and returns the pool. A value that fails is dropped and no longer counts toward `max_connections`, and `acquire` makes a new one. Values `create_fn` has just made are not checked. The check's parameter borrows the value, so it cannot keep or consume it. Chain it off `new`: `Pool.new(connect, 8, 16).with_health_check(\|c\| c.ping())`. |
| `close` | `fn close(ref self)` | Shuts the pool down. Its idle values drop at once, each loan's value drops when it comes back, and every later `acquire` fails with `PoolClosed`. |

## Giving values back

A `PooledConnection` goes back to its pool when it drops, so most code never calls `release`. A value goes back once per loan: calling `release` and then letting the same loan drop does not add a second idle value. `PooledConnection` is `#[must_use]`, because discarding the result of `acquire` gives the value straight back, unused.

When the `Pool` itself drops, its idle values drop. A loan that outlives its pool keeps its value until the loan drops, and the value drops then.

## Errors

`Timeout` means the caller waited the whole `timeout` and no value came back, which usually points at a slow backend or loans that are never given back. `Busy` means `max_waiters` callers were already waiting, so the caller was turned away without waiting: it is load shedding, the case a service answers with a 503. `PoolClosed` means `close` was called and the pool lends nothing more. `CreateFailed(e)` carries the error `create_fn` returned; the pool keeps nothing from the failed attempt, so the attempt does not count toward `max_connections`.

## Not yet implemented

- The signatures above. Today the pool is `Pool[T]` with `create_fn: own Fn() -> T`, `PoolError` has no payload and `CreateFailed` is never produced (B-2026-10-21-41); `acquire` takes `timeout: i64` milliseconds (B-2026-10-21-42); and `with_health_check` takes `ref self` (B-2026-10-21-43). The items are in the prelude, and there is no `std.pool` module (B-2026-10-21-42). These land together after M3 removes the legacy backend, which shares today's pool source.
- Waiting. `acquire` fails with `Timeout` at once when every value is in use, whatever the `timeout`, `max_waiters` limits nothing, and `PoolError` has no `Busy` variant yet (B-2026-10-21-44).
- `#[must_use]` on `PooledConnection` is recorded but not enforced. A bare `pool.acquire(..);` statement is still warned about, because `Result` is always must-use.
