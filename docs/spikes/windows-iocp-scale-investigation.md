# Windows IOCP — 1M-scale investigation (gaps & frictions)

> **Status: investigation complete 2026-06-17.** A follow-on to
> [`windows-iocp-eventloop.md`](windows-iocp-eventloop.md) (the port itself,
> DONE). After the 250k re-validation, we pushed the loopback functional run to
> **1,000,000 connections** on the Windows Server 2025 box explicitly to surface
> *gaps and frictions* at scale rather than to reconfirm a green number. It
> surfaced four, one of them a real unbounded **memory leak** (B-2026-06-17-2 —
> **fixed 2026-06-17, `849030b6`**; see worklist item 1. The residual
> free-spawn+coro variant is split out as B-2026-06-17-3).
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

## Finding 1 — unbounded memory leak in the spawn/structured-concurrency model ✅ (B-2026-06-17-2)

> **FIXED 2026-06-17 (`849030b6`).** Detached-gated eager-reap, per the fix
> design below. Codegen marks a discarded `spawn`/`tg.spawn` handle detached
> (`karac_runtime_task_detach`); `karac_runtime_taskgroup_register` sweeps and
> frees detached, terminal children (bounding the `children` Vec to ~live
> children — UAF-safe terminal peek: `notify_mutex` non-coro, the new
> `karac_runtime_park_slot_done` for coro), and a free-spawn non-coro handle
> self-reaps on completion. Green on the full Linux ASAN+LSan suite (263/263).
> **Residual:** free-spawn + coro (`ws_echo_freespawn`) self-reap is deferred to
> **B-2026-06-17-3** (needs dispatcher signal-path surgery); the canonical
> server shape here (`tg.spawn`, `examples/ws_idle_holder`) is fully closed.

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

## Finding 2 — ~15 ms per-connection latency floor on the main-thread blocking-I/O path (platform-agnostic)

> **Corrected after deeper analysis (the first pass mis-attributed this to the
> Windows timer quantum — it isn't Windows-specific).**

At **conc 1, every variant ≈ 90/s, p50 ≈ 15.3 ms** (min 2.4 ms). The floor is
*not* in the IOCP wake path: the reactor's `dispatcher_thread_main` blocks in
`run_once(None)` — an **infinite, event-driven** IOCP wait that returns the
instant a socket is ready (`runtime/src/event_loop.rs`). The proof is in the
sweep: a **spawned** handler (a coroutine the dispatcher drives) hits **0.53 ms
p50 at conc 4**, while the **serial/inline** handler stays ~15 ms — same I/O,
same box, only the execution context differs. So the floor lives in the
**main-thread (non-coroutine) blocking-wait path** that an *inline* handler's
`recv`/`accept` use, not in the event loop.

This is **platform-agnostic**: the macOS parity run hit the *same* ~1,400/s at
conc 16 (≈ the Windows serial figure), so the coarse main-thread wait is present
there too. The only Windows-specific detail is the **bimodal quantization** of
the distribution (min 2.4 ms vs p50 15.3 ms — the Windows 15.6 ms timer
resolution sharpening it); `timeBeginPeriod(1)` would smooth that *shape* but not
lift the floor, since the floor is the wait mechanism, not the timer. **Real
fix (Linux follow-up): route main-thread blocking I/O through the same
event-driven dispatcher path the spawned-coroutine handlers already use** — then
an inline handler gets the spawn path's sub-ms latency.

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
runtime's (~7,000/s peak with spawn). The serial/inline path also carries
Finding 2's ~15 ms floor (stays ~15 ms through conc 16 where spawn drops to
sub-ms) — the same inline-handler blocking-wait inefficiency.

## Finding 4 — saturation / tail-latency cliff above ~conc 64

Both servers: **p99 jumps to ~500 ms at conc 256**, and spawn throughput
*regresses* (7,031 → 6,195/s). A contention ceiling around ~7k/s with tail
collapse under overload — worth a deeper look (single accept loop? a hot
listener shard?). Not yet root-caused.

## Follow-up worklist (all platform-agnostic — do on a Linux box)

Deeper analysis found **none of the four findings is Windows-specific** — they
all reproduce on macOS/Linux (the scheduler/runtime code involved is not
`cfg`-gated). Windows merely surfaced them. So this work belongs on a
Linux-capable machine, where it can also clear the authoritative leak gate
(`scripts/lsan-local.sh`, Linux ASAN+LSan). The only genuinely Windows-specific
item found — the `bug-curve.py` injector crashing under cp1252/CRLF — is fixed
in this same change.

1. **Leak fix (B-2026-06-17-2), highest priority. ✅ DONE 2026-06-17 (`849030b6`).**
   Implemented the detached-gated eager-reap above: `detached`/`reaped:
   AtomicBool` on `KaracTaskHandle` + `karac_runtime_task_detach` FFI; codegen
   marks discarded `spawn`/`tg.spawn` handles detached (`pending_spawn_detach` in
   `compile_stmt` → `lower_spawn_shared`); `taskgroup_register` reaps terminal
   detached children (peek `state` under `notify_mutex` non-coro, the new
   `karac_runtime_park_slot_done` for coro), bounding the Vec to ~live children;
   free-spawn non-coro self-reaps on completion, the worker and the detach path
   claiming the free exactly once under `notify_mutex`. Regression coverage:
   runtime unit `taskgroup_register_reaps_detached_completed_children` (children
   Vec stays bounded — fails pre-fix) + `detached_free_spawn_self_reaps_after_
   completion`, and the `tests/memory_sanitizer.rs` E2E `asan_discarded_
   taskgroup_spawn_loop_eager_reap_no_double_free`. **Green under Linux ASAN+LSan,
   263/263** (`scripts/lsan-local.sh`). **Residual:** free-spawn + coro
   (`ws_echo_freespawn`) self-reap — the ramp-worker returns before completion and
   there is no group to sweep it, so detach only flags it — is split out as
   **B-2026-06-17-3** (needs a dispatcher signal-or-reap path). The canonical
   server shape (`tg.spawn`) is fully closed.
2. **Finding 2 — main-thread blocking-I/O latency floor.** Route a non-coroutine
   (inline-handler / main-thread) blocking `recv`/`accept` through the
   event-driven dispatcher path the spawned coroutines already use, so it gets
   sub-ms latency instead of the ~15 ms floor. Validate with the conc-1 row of
   the `ws_loop_client_soak.py` sweep (expect 90/s → multi-k/s).
3. **Finding 4 — saturation/tail cliff above ~conc 64.** Root-cause the ~7k/s
   ceiling + p99→500 ms collapse (single accept loop? hot listener shard?
   global pool-queue contention?). Use the same soak client's percentile output.
4. **Finding 3 is informational** — no fix; just prefer the spawn shape for any
   throughput benchmark, and note that the spike's "~1,400/s" headline is the
   serial example's ceiling, not the runtime's.

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
