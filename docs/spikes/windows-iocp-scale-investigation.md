# Windows IOCP — 1M-scale investigation (gaps & frictions)

> **Status: investigation complete 2026-06-17.** A follow-on to
> [`windows-iocp-eventloop.md`](windows-iocp-eventloop.md) (the port itself,
> DONE). After the 250k re-validation, we pushed the loopback functional run to
> **1,000,000 connections** on the Windows Server 2025 box explicitly to surface
> *gaps and frictions* at scale rather than to reconfirm a green number. It
> surfaced four, one of them a real unbounded **memory leak** (B-2026-06-17-2).
> The IOCP event loop itself — the subject of the port — held flawlessly; every
> friction lives in the concurrency layer *above* it.

## What was run

- Server: `examples/std_net/ws_echo.kara` (the existing serial echo server) plus
  two new variants written for this investigation:
  `examples/std_net/ws_echo_spawn.kara` (`TaskGroup.spawn(|| echo(ws))` per
  connection) and `examples/std_net/ws_echo_freespawn.kara` (free
  `spawn(|| echo(ws))`), all AOT-built with the native Windows toolchain.
- Client: `examples/std_net/ws_loop_client_soak.py` — a bounded-worker-pool
  harness (fixed `conc` threads, no unbounded futures), RST/abortive close
  (`SO_LINGER 0`) so the client never parks ephemeral ports in TIME_WAIT, with
  per-connection latency percentiles and throughput-over-time reporting.
- Server-side time-series sampler (handles / threads / RSS every 3s).

## The 1M soak (serial server) — clean

`1,000,000 / 1,000,000` round-trips, conc 16, **1,131/s, 883.8s**. Over the full
~15 min / 260 samples: **handles flat 95–97, RSS pinned at 4.5 MB, threads 9–10,
throughput flat (no cliff), no wedge, no counter wraparound.** The IOCP
register→park→wake→deregister churn is solid at 1M. Latency p50=15.48ms,
p99=16.47ms — extremely tight (the queueing signature; see Finding 2).

## Finding 1 — unbounded memory leak in the spawn/structured-concurrency model ⚠️ (B-2026-06-17-2)

**Any long-lived spawning loop leaks ~100 bytes per spawned task, linearly,
unbounded** — confirmed empirically and from source. This is the canonical
server shape (`loop { tg.spawn(|| handle(conn)) }`, exactly what
`examples/ws_idle_holder/src/main.kara` does), and the code is **not**
`cfg`-gated, so it is **platform-agnostic** (affects Linux/macOS too), not a
Windows artifact.

| server shape | leak? | RSS over 200k conns (4×50k bursts) |
|---|---|---|
| serial inline `echo(ws)` | **no** | flat 4.5 MB (and over the 1M soak) |
| `TaskGroup.spawn(\|\| echo(ws))` | **yes** | 17.6 → 23.0 → 27.8 → 32.7 → 37.7 MB |
| free `spawn(\|\| echo(ws))` | **yes** | 5.4 → 10.9 → 15.6 → 20.3 → 25.1 MB |

Both spawn forms: ~5 MB / 50k conns ≈ **~100 B/conn, perfectly linear**, while
OS **handle count stays flat** (it's heap memory — the `KaracTaskHandle` — not an
OS handle). The serial server never spawns and never leaks.

### Root cause (runtime/src/scheduler.rs)

Two distinct paths, both retaining the per-task `KaracTaskHandle` (allocated by
`karac_runtime_spawn` / `karac_runtime_spawn_coro`; for the coro path also a
`KaracParkSlot`):

- **`TaskGroup.spawn`** — `karac_runtime_taskgroup_register` pushes the child
  handle onto `KaracTaskGroupHandle.children: Mutex<Vec<*mut KaracTaskHandle>>`.
  Children are freed **only** in `karac_runtime_taskgroup_join_and_free`, emitted
  at the group's **scope exit**. A server's accept loop creates the group once
  and loops forever, so scope exit never happens → the Vec and every child grow
  without bound.
- **free `spawn`** — codegen passes `group_ptr = None`, so there is **no**
  register call *and* no `task_handle_free` at the discard site (see
  `src/codegen/task_group.rs`). The handle is simply **orphaned** — never freed.

The canonical accept-loop handler does blocking I/O, so it lowers through the
**coro-spawn** path (`use_coro_spawn`): the leaked unit per connection is the
`KaracTaskHandle` **plus** its `KaracParkSlot`.

### Why it isn't trivially fixed (the hazard)

The completed children can't simply be freed eagerly: a registered child whose
`TaskHandle` the user **retained** can still be `.join()`ed later (join on a
registered child waits+copies but does *not* free — the group is sole freer, per
B-2026-06-09-1). Eager-freeing such a child is a use-after-free. So a safe reap
must be **detached-gated**: only reap children whose handle was *discarded*
(the fire-and-forget server case), which the runtime can only know if **codegen
marks discarded spawn handles detached**.

### Fix design (the safe, complete shape)

1. `detached: AtomicBool` on `KaracTaskHandle`; `karac_runtime_task_detach(h)`
   FFI (or a `*_detached` spawn variant).
2. Codegen: when a `spawn`/`tg.spawn` call's result `TaskHandle` is **discarded**
   (expression-statement, not bound/joined), mark it detached.
3. Reaping:
   - `tg.spawn` detached child: on each `taskgroup_register`, sweep `children`
     and free+remove any that are terminal *and* detached. Terminal detection is
     a non-blocking peek — `state != PENDING` (non-coro) or the park slot's
     `done == true` (coro; the slot exposes `done: Mutex<bool>`). This bounds the
     Vec to the count of concurrently-live children (~`conc`), preserves the
     structured wait-at-scope-exit guarantee (a completed+reaped child is a no-op
     to wait on), and is UAF-safe (only detached children are reaped).
   - free `spawn` detached: self-free on completion (non-coro: the worker frees
     the handle after storing terminal state; coro: the completion/slot-signal
     path frees handle+slot).

The serial inline server is the only currently leak-free shape — and it pays
Findings 2–3, so "just use the serial server" is not the answer.

## Finding 2 — ~15.6 ms per-connection latency floor at low concurrency (Windows-specific)

At **conc 1, every variant ≈ 90/s, p50 ≈ 15.3 ms, min 2.4 ms** — bimodal, the
signature of the **Windows default 15.6 ms timer quantum** quantizing a timed
wait in the wake path. It amortizes away under load (spawn conc≥4 → p50 0.5 ms,
because one ~15.6 ms wake services many ready connections). Aggregate-throughput
tests at conc 16 never measured conc-1 latency, so it went unseen; the macOS
parity run wouldn't show it (hi-res timers). Likely fixable with
`timeBeginPeriod(1)` at runtime startup, or by making the hot-path wait fully
event-driven (untimed).

## Finding 3 — the validation example under-represents runtime throughput ~5×

Every 10k/250k/1M run used the **serial** `ws_echo.kara`. Concurrency sweep
(12k conns each):

| conc | serial `ws_echo` | spawn `ws_echo_spawn` |
|---|---|---|
| 1 | 90/s · p50 15.4ms | 90/s · p50 15.3ms |
| 4 | 448/s · p50 14.2ms | 2,753/s · p50 0.53ms |
| 16 | 1,202/s · p50 15.3ms | **6,662/s · p50 2.23ms** |
| 64 | 3,019/s · p50 22.3ms | 7,031/s · p50 8.84ms |
| 256 | 4,566/s · p99 **548ms** | 6,195/s · p99 **506ms** |

So the spike's "~1,400/s" headline is the *serial example's* ceiling, not the
runtime's (~7,000/s peak with spawn). The serial/inline path also fails to
amortize the 15 ms tick (stays ~15 ms through conc 16 where spawn drops to
sub-ms) — an inline-handler inefficiency on top of the timer floor.

## Finding 4 — saturation / tail-latency cliff above ~conc 64

Both servers: **p99 jumps to ~500 ms at conc 256**, and spawn throughput
*regresses* (7,031 → 6,195/s). A contention ceiling around ~7k/s with tail
collapse under overload — worth a deeper look (single accept loop? a hot
listener shard?). Not yet root-caused.

## Reproduction

```
# build (native Windows toolchain; LLVM 18.1 at C:\llvm18, clang on PATH)
karac build examples/std_net/ws_echo.kara          # serial (leak-free, ~1.3k/s)
karac build examples/std_net/ws_echo_spawn.kara     # TaskGroup.spawn (leaks)
karac build examples/std_net/ws_echo_freespawn.kara # free spawn (leaks)

# drive (host port count concurrency)
python examples/std_net/ws_loop_client_soak.py 127.0.0.1 8080 1000000 16
# leak repro: run repeated 50k bursts against a spawn server and watch RSS climb
# ~5 MB / 50k while handle count stays flat.
```

## Cross-links

- Parent: [`windows-iocp-eventloop.md`](windows-iocp-eventloop.md) (the port).
- Leak: `docs/bug-ledger.jsonl` **B-2026-06-17-2**.
- Structured-concurrency model: `runtime/src/scheduler.rs`, `docs/design.md §
  Explicit Concurrency`.
