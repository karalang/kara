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
> **Connection-density addendum (2026-06-19):** the prior runs all measured
> connection *churn* (~16 live at once); a dedicated density run then held
> **45,000 concurrent persistent connections** clean — flat 94 MB / 46.5k handles
> across a 90 s hold, ~2 KB & ~1 handle per connection, liveness OK, no wedge; the
> ceiling is the OS loopback port range, not the runtime. See **Finding 5**.
> **All code changes are now DONE.** Finding 2's Windows timer fix landed +
> validated natively 2026-06-18 (`8f0c56c6`) — but the measurement **refuted the
> spike's own prediction**: raising the timer to 1 ms drops conc-1 p50 only
> 15.24 ms → 10.52 ms (a real ~31 % win, not the anticipated ~15×), because the
> residual ~10.5 ms is a per-in-flight-connection IOCP/AFD readiness latency that
> **concurrency hides** — the spawn server pays the *identical* 10.5 ms at conc 1
> and only diverges at conc ≥ 4 (spawn conc-4 p50 0.53 ms). See the (now closed-out)
> Finding 2 + worklist item 2 below.

## ⇒ Finding 2 Windows timer fix — DONE + the prediction it refuted (`8f0c56c6`, 2026-06-18)

*All code changes in this doc are now closed. This section records what landed
and — more importantly — how the native measurement **refuted the fix's own
predicted outcome**, which re-root-causes the conc-1 floor.*

**What landed.** `event_loop.rs::ensure_high_res_timer` (a `#[cfg(windows)]`
one-shot called from the `event_loops()` `get_or_init` reactor init) raises the
process timer resolution to 1 ms via a minimal `#[link(name = "winmm")]`
`timeBeginPeriod(1)` extern — no `windows-sys`/`winapi` dependency added. The AOT
linker driver (`src/codegen/driver.rs`) gained `winmm` in its Windows system-lib
list, because that driver names system libs explicitly and does **not** honor the
runtime crate's `#[link]` directive (which only covers cargo's own test/build
links) — without it every karac-built binary fails to link with `undefined symbol:
timeBeginPeriod`. `timeBeginPeriod` is process-scoped on Windows 10 2004+, so the
blast radius is just the server process; no `timeEndPeriod` (the process serves
until exit).

**Measured, Windows Server 2025, serial `ws_echo`, conc 1 (`ws_loop_client_soak.py`):**

| | conc-1 p50 | system timer (`NtQueryTimerResolution`) |
|---|---|---|
| before | 15.24 ms | 15.625 ms |
| after | **10.52 ms** | **1.000 ms** |

The timer change is real and causal (verified: the finest the OS offers is 0.5 ms,
the fix pins `current` at 1.000 ms; with no high-res requester the box idles at
15.625 ms). But it is a **~31 % win, not the ~15× the spike predicted** — conc-1
p50 did **not** drop to ~1 ms.

**Why the "→1 ms" prediction was wrong (the real root cause).** The residual
~10.5 ms is **not** the system timer and **not** the inline/main-thread wait
mechanism. Two measurements on the same box settle it:

- The **spawn** server (`ws_echo_spawn.kara`, coro dispatcher path) pays the
  **identical 10.52 ms p50 at conc 1** — so the floor is *not* inline-vs-coro.
  Finding 2's original "the floor lives in the main-thread blocking-wait path"
  premise is **refuted**: both execution contexts pay it equally at conc 1.
- The **same spawn server at conc 4** drops to **p50 0.53 ms** (p99 still 10.78 ms).
  So a single connection's full cycle *can* complete in ~0.5 ms; the ~10.5 ms at
  conc 1 is a **per-in-flight-connection latency that concurrency overlaps away** —
  the Windows IOCP/AFD readiness-notification latency for a single outstanding
  socket (most plausibly the listener's accept-readiness re-arm between sequential
  connections). It is paid once per concurrently-live connection, so it vanishes
  from the median the moment ≥2 connections are in flight.

This also reconciles Finding 3's table, which already shows **both** serial and
spawn at **90/s · p50 ~15 ms at conc 1** and divergence only from conc 4 up: the
conc-1 row was never measuring the inline path's overhead — it was measuring this
shared readiness floor, with the (then-15.6 ms) timer stacked on top.

**Consequences for the runtime.** conc-1 latency is the wrong yardstick for the
runtime's capability — it is dominated by an OS readiness floor that real
(concurrent) workloads never hit. The right yardsticks are the conc ≥ 4 figures
(spawn p50 0.53 ms) and the persistent-connection throughput in Finding 4
(65k echoes/s). Driving conc-1 to ~1 ms would require shrinking the per-socket
AFD readiness latency itself — a deeper mio-integration change, **not** worth it
for a metric no real workload is bound by, and explicitly **not** "route inline
I/O through the spawn path" (off Windows that path is *slower*, Finding 3; on
Windows it pays the same conc-1 floor anyway). The timer fix is kept as a cheap,
correct latency win + server hygiene.

**Repro (Windows).** Build per the [Reproduction](#reproduction) block below, then:
```
karac build examples/std_net/ws_echo.kara          # serial / inline path
ws_echo                                            # binds 127.0.0.1:8080 (run in another shell)
python examples/std_net/ws_loop_client_soak.py 127.0.0.1 8080 2000 1   # conc 1 → p50 ~10.5 ms
python examples/std_net/ws_loop_client_soak.py 127.0.0.1 8080 4000 4   # spawn conc 4 → p50 ~0.5 ms
```
The harness' `SO_LINGER` was already made cross-platform (commit `6136446e`) and
runs as-is on Windows.

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
> The free-spawn + coro residual (`ws_echo_freespawn`) is **also fixed now** —
> **B-2026-06-17-3 (`69a03439`)**, a slot-armed self-reap (the completion signal
> frees handle+slot when the slot is armed by detach). So all spawn shapes —
> `tg.spawn` and free `spawn`, coro and non-coro — are leak-clean.

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

## Finding 2 — ~15 ms per-connection latency floor at conc 1 (timer + a deeper readiness floor)

> **FIXED + RE-ROOT-CAUSED 2026-06-18 (`8f0c56c6`, native Windows).** The
> `timeBeginPeriod(1)` fix landed and measurably lowered conc-1 p50 (15.24 → 10.52 ms,
> timer 15.6 → 1.0 ms), but the native A/B **refuted both the floor's diagnosis and
> the predicted outcome**: the floor is NOT the main-thread wait mechanism (the spawn
> server pays the identical 10.52 ms at conc 1) and does NOT drop to ~1 ms — the
> residual is a per-in-flight-connection IOCP/AFD readiness latency that concurrency
> hides (spawn conc-4 p50 0.53 ms). Full write-up + the corrected root cause are in
> the closed-out **"Finding 2 Windows timer fix — DONE"** section near the top of this
> doc; the historical analysis below is left for provenance. Read the two together:
> the "≈15× win" / "the timer is the whole problem" claims below are the ones the
> measurement overturned.

> **RE-CORRECTED BY MEASUREMENT 2026-06-17 (macOS / Apple Silicon, current code).**
> The floor **is** the Windows 15.6 ms timer quantum after all — it does **not**
> reproduce off Windows. Driving the real benchmark
> (`examples/std_net/ws_loop_client_soak.py`) against the serial `ws_echo` server
> on macOS measured **conc-1 = 10,617/s, p50 0.09 ms** (and conc-16 = 12,716/s,
> p50 1.20 ms) — ~170× faster than the Windows 15.4 ms, with **no floor**. So the
> inline path's condvar handoff (`park_slot_wait` ← dispatcher `park_slot_signal`)
> is **sub-0.1 ms** when the OS scheduler isn't timer-quantizing the wakeup; the
> floor is the *timer*, not the wait mechanism (reversing the claim below). Two
> consequences: (1) the fix is **Windows-only** — raise the timer resolution
> (`timeBeginPeriod(1)` or a high-resolution waitable-timer wait in the
> Windows-cfg runtime init) — and is only validatable on a Windows box; (2) the
> proposed "route inline I/O through the spawn/coro path" fix would **backfire** —
> on macOS the **serial path is faster than spawn at every concurrency** (12.7k vs
> 6.3k/s at conc 16; see Finding 3), because with no floor the coro machinery is
> pure overhead. The original Windows observation + analysis below stands; only
> the "platform-agnostic / not the timer / route-through-dispatcher" conclusions
> were wrong.

At **conc 1, every variant ≈ 90/s, p50 ≈ 15.3 ms** (min 2.4 ms). The floor is
*not* in the IOCP wake path: the reactor's `dispatcher_thread_main` blocks in
`run_once(None)` — an **infinite, event-driven** IOCP wait that returns the
instant a socket is ready (`runtime/src/event_loop.rs`). The proof is in the
sweep: a **spawned** handler (a coroutine the dispatcher drives) hits **0.53 ms
p50 at conc 4**, while the **serial/inline** handler stays ~15 ms — same I/O,
same box, only the execution context differs. So the floor lives in the
**main-thread (non-coroutine) blocking-wait path** that an *inline* handler's
`recv`/`accept` use, not in the event loop.

~~This is platform-agnostic…~~ **— refuted by the macOS measurement above.** The
"macOS parity ~1,400/s at conc 16" data point that motivated this paragraph does
not match the current code: measured macOS serial conc-16 is **12,716/s**
(≈9× the cited figure), and conc-1 is 0.09 ms p50 with **no floor**. The bimodal
min-2.4/p50-15.3 distribution is exactly the Windows 15.6 ms timer quantizing the
wakeup (two parks per echo round-trip × ~7.8 ms half-tick ≈ 15.6 ms), and
`timeBeginPeriod(1)` lowering that tick to ~1 ms is therefore the **right** lever
(≈15× win), not a cosmetic one. **Real fix: Windows-only timer-resolution change,
validated on the Windows box.** Do **not** route inline I/O through the
spawn/coro dispatcher path — off Windows that path is *slower* (Finding 3's
"spawn is ~5× faster" is itself a Windows-floor artifact; with the floor gone the
ordering inverts).

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

## Finding 4 — saturation / tail-latency cliff above ~conc 64 — ROOT-CAUSED 2026-06-17 (measured, macOS / Apple Silicon, 18 cores)

Original observation: both servers p99 jumps to ~500 ms at conc 256, spawn
throughput "regresses" 7,031 → 6,195/s; a "~7k/s contention ceiling." **Driving
the real benchmark and drilling in decomposed this into two separate things,
*neither* a runtime contention point:**

**(1) The "~7k/s ceiling" was the single Python benchmark client, not the
runtime.** `ws_loop_client_soak.py` burns ~3.1 cores of per-connection Python CPU
(WS frame masking via a byte-loop, parse) and tops out a single client around
~6.6k/s. Pointing **more clients at the same server** lifts it immediately: 1
client = 6.6k/s, 2 = 8.5k/s, 4 = **16.0k/s**, 6 = 16.2k/s. The server's real
ceiling is **~16k/s (spawn) / ~13k/s (serial)** — ~2.4× the reported figure. The
"throughput regression at conc 256" is just the tail (see (3)) stealing wall-clock.

**(2) The ~16k/s ceiling is the kernel's loopback connection-churn rate, NOT the
runtime — parallel accept does not help (tested).** It *looked* like the
single-threaded `main` accept loop (flat throughput, latency = `conc/throughput`
Little's-Law signature, ~13 of 18 cores idle at the plateau), so the natural fix
was "parallelize accept." **Measured: it doesn't.** A `SO_REUSEPORT` prototype
running **4 independent accept-loop processes on the same port** delivered the
**same ~16k/s** aggregate (16,486/s) as one process — if the accept loop were the
gate, 4 of them would give ~4×. So the limit is **shared system-wide**: the macOS
loopback connection setup/teardown path (SYN/accept/close per round-trip), which
the benchmark hammers because it opens a *new* connection per echo.

The runtime itself is **not** the bottleneck — it parallelizes fine on the work
that isn't connection churn. **Steady-state echo on *persistent* connections**
(the flagship workload's actual shape) measured **65,070 echoes/s scaling to ~15
of 18 cores** (4 clients × 16 persistent conns), 4× the churn ceiling and rising
with cores — vs the same single client's 6.6k *conn/s* churn. So there is **no
accept-loop fix to make**: the churn ceiling is the loopback, and the persistent
workload already scales. (`SO_REUSEPORT` is still worth exposing later as an
opt-in for multi-process deployment / graceful restart — but as a deployment
feature, not a fix for this, since it gave zero throughput win here.)

**(3) The tail collapse (p99 → ~1 s at conc > ~128) is the OS listen-backlog
cap, not the runtime.** macOS `kern.ipc.somaxconn = 128` silently caps the
`listen(2)` backlog at 128 regardless of the runtime's requested 16384; once
**in-flight connects exceed 128**, excess SYNs are dropped and the client
retransmits after the ~1 s RTO — exactly the 1,008 ms p99 / 2,041 ms max seen.
Onset confirmed at the boundary: conc 130 is clean (max 48 ms), conc 160 tails
(p99.9 1,022 ms). The doc's Windows ~500 ms tail is the same phenomenon (Windows
has its own backlog cap). Mitigation is deployment-side (`sysctl
kern.ipc.somaxconn` / Windows equivalent), not a userspace runtime change.

**Caveat — the benchmark over-weights connection churn.** It opens a *new*
connection per round-trip (connect + handshake + 1 echo + RST-close), so it
measures connection-*setup* throughput, which is exactly what the kernel loopback
churn rate (2) and the backlog cap (3) gate. The flagship workload (1M
*persistent* idle connections) does its accept churn only at ramp-up and is
steady-state idle after — it does **not** hit either ceiling, and its steady-state
I/O scales to ~15 cores (the 65k-echoes/s measurement in (2)). So both ceilings
matter only for connection-churn workloads (proxies, short-lived RPC) and are
kernel/OS-side; for the launch headline neither is on the critical path, and
neither is a runtime defect.

## Finding 5 — connection DENSITY (the untested dimension) — clean to 45k held, 2026-06-19

Every Windows run above — the 10k/250k/1M soaks — measured connection **churn**
(open → echo → RST-close, only ~16 live at any instant). None measured connection
**density**: many *persistent* connections held open simultaneously, which is the
flagship workload's actual shape (`examples/ws_idle_holder` — "M1: 100K stable idle
connections … M3: 1M+", a target set and validated on **Linux**, never on Windows).
Density stresses what the port specifically changed: N **simultaneous** persistent
mio sources / `SockState`s / AFD polls (the Problem-4 per-socket model under
sustained load, not the register→park→wake→deregister *cycle* churn exercised), plus
steady-state per-connection memory/handle scaling.

**Run.** Plaintext `examples/std_net/ws_echo_spawn.kara` (`tg.spawn(|| echo(ws))`
per connection; an idle client leaves each handler parked in `recv_text`, so each
is a held, density-contributing connection) driven by the new
`examples/std_net/ws_density_client.py` — it opens N persistent ws:// connections,
holds them all open, samples the server at peak, runs a liveness probe, then closes
them. **45,000 concurrent connections held** on a single Windows Server 2025 box,
default multi-shard (8 shards/8 cores).

**Result — clean, linear, flat.** Server at the 45k peak: **46,525 handles,
94.14 MB RSS, 17 threads**, and the plateau held **dead-flat at 94.14 MB / 46,525
handles for the entire 90 s hold** (17 identical samples — zero drift under
sustained density). Per-connection cost is **perfectly linear** across the 2k→45k
ramp:

| held conns | handles | RSS (MB) |
|---|---|---|
| ~5,645 | 5,645 | 22.56 |
| ~15,498 | 15,498 | 40.23 |
| ~30,216 | 30,216 | 65.89 |
| **45,000** | **46,525** | **94.14** |

- **~1.03 OS handles / connection** ((46,525 − 99 baseline)/45,000) — one socket
  handle each, nothing else accumulating.
- **~1.97 KB RSS / connection** ((94.14 − 5.68 baseline)/45,000) — the
  `KaracTaskHandle` + `KaracParkSlot` + persistent mio source/`SockState` +
  registration + the handler's 1 KiB coro-frame buffer + socket buffers. Linear,
  no superlinearity.
- **Threads flat at 17** (baseline 12) regardless of connection count — the
  handlers are coroutines on a **bounded pool**, not thread-per-connection.
- **`LIVENESS_OK` at peak** — a brand-new connection still established while 45k
  were held, so the accept/register/park path does **not** wedge at density.
- **Clean reap on close** — after the client closed all 45k, the server drained to
  **119 handles** (≈ baseline + 20, not + 45k) and RSS to 21.5 MB (allocator
  high-water retention, not a leak): the held-then-closed connections' handles +
  slots are reaped (clean EOF → handler completes → detached-reap, B-2026-06-17-2/-3).

**Ceiling is the OS, not the runtime.** 45k is **port-bound, not runtime-bound**:
each held connection burns one *client* ephemeral port (the server side shares
:8080 via the 4-tuple — a handle, not a port), and Windows loopback is only
`127.0.0.1` (no `127/8` source-IP fan-out, which is how the Linux 1M runs scaled).
The default 16,384-port range was widened to ~55k for this run
(`netsh int ipv4 set dynamicport tcp start=10000 num=55000`, reverted after); the
linear-and-flat profile extrapolates cleanly past it. Pushing to the Linux 1M
number on Windows needs loopback aliasing or a second client box — a *harness*
limit, not a runtime one. (Establishment ran at ~279 conns/s during ramp — the same
Windows loopback connection-setup rate Finding 4 identified, orthogonal to density.)

**Bottom line:** the IOCP per-socket persistent-source model holds flat and leak-free
at 45k concurrent held connections — the density dimension is validated on Windows
to the box's port ceiling, with no runtime ceiling in sight.

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
   263/263** (`scripts/lsan-local.sh`). The free-spawn + coro residual
   (`ws_echo_freespawn`) — the ramp-worker returns before completion and there is
   no group to sweep it — is **also fixed now: B-2026-06-17-3 (`69a03439`)**, a
   slot-armed self-reap: `karac_runtime_task_detach` arms the bound slot and the
   coroutine's completion signal frees handle+slot (the `done` lock claims the
   free exactly once; sound because signal is the slot's last use on every
   completion path). Covered by runtime unit tests
   `detached_free_spawn_coro_reaps_when_{already_complete_at_detach,
   completion_follows_detach}` + the `coro_e2e`
   `coroutine_discarded_free_spawn_self_reaps_under_asan` E2E (real connection, 2s
   barrier → deterministic Linux LSan leak gate). **All spawn shapes are now
   leak-clean.** (An orthogonal anomaly surfaced while testing — moving a channel
   `Sender` into a free-spawn coroutine closed the channel before the send landed,
   so `rx.recv()` saw the closed-sentinel `0`. **Already FIXED the same day by
   `691117f6`** — logged retroactively as **B-2026-06-17-9**: the coroutine now owns
   its moved channel-end params and closes the channel only at completion, after the
   send. Regression: `coroutine_free_spawn_channel_send_lands_before_close`.)
2. **Finding 2 — conc-1 latency floor. ✅ DONE 2026-06-18 (`8f0c56c6`), native
   Windows — but the fix's predicted outcome was REFUTED by the measurement.**
   `timeBeginPeriod(1)` landed in the reactor init (+ `winmm` in the AOT linker
   driver) and the system timer verifiably drops 15.625 → 1.000 ms, lowering serial
   conc-1 p50 **15.24 → 10.52 ms** (~31 %, not the predicted ~15×). The remaining
   ~10.5 ms is **not** the timer and **not** the inline-vs-coro wait mechanism: the
   spawn server pays the *identical* 10.52 ms at conc 1 and only diverges at conc ≥ 4
   (spawn conc-4 p50 0.53 ms), so it is a per-in-flight-connection IOCP/AFD readiness
   latency that concurrency hides. The earlier "the floor is purely the Windows timer,
   `timeBeginPeriod` is the ~15× lever" conclusion is therefore wrong — see the
   closed-out **"Finding 2 Windows timer fix — DONE"** section at the top of this doc
   for the corrected root cause and why conc-1 is the wrong yardstick. No further work:
   conc-1 latency is bound by an OS readiness floor real (concurrent) workloads never
   hit. (Do **not** route inline I/O through the spawn/coro path — off Windows it is
   *slower*, and on Windows it pays the same conc-1 floor.)
3. **Finding 4 — saturation/tail cliff above ~conc 64. ✅ ROOT-CAUSED 2026-06-17
   (measured) — see the rewritten Finding 4 above.** Three findings, **no runtime
   fix needed**: (a) the "~7k/s ceiling" was the single Python client (server
   scales to ~16k/s with 4–6 clients); (b) the ~16k/s churn ceiling is the
   **kernel loopback connection-setup rate, NOT the accept loop** — a
   `SO_REUSEPORT` 4-process test gave the *same* 16k (parallel accept = zero win),
   while **steady-state echo on persistent connections hit 65k echoes/s scaling to
   ~15 of 18 cores**, so the runtime is not the bottleneck; (c) the p99→~1 s tail
   is the **OS listen-backlog cap** (macOS `somaxconn=128`) dropping SYNs under
   connection-storm, mitigated deployment-side. The benchmark over-weights
   connection churn; the 1M-persistent-connection launch workload hits none of
   these and already scales. No follow-up — `SO_REUSEPORT` is worth exposing later
   only as a *deployment* opt-in (multi-process / graceful restart), not as a perf
   fix.
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
