# `karac inspect <pid>` — feasibility + design spike

**Status:** **blocked, not started.** This is a design/feasibility artifact, not
an implementation. `karac inspect` (roadmap Track 6 § CLI surface) is blocked on
runtime infrastructure that does not exist in v1; this spike records the
investigation (2026-07-23), the sound design for when the prerequisites land,
and the sequencing. Two sibling Track 6 items — `karac debug` and grouped-form
LSP hover — shipped; `inspect` is the one that could not be built honestly yet.

## The goal (roadmap)

> `karac inspect <pid>` (Linux + macOS at v1) — attaches to a running process
> via `ptrace` (Linux) or `task_for_pid` (macOS); reads runtime metadata via the
> per-worker cooperation hook from Track-5-adjacent Gap 2 work (same surface,
> reused); dumps `list_tasks()` / `list_par_blocks()` output without requiring
> code changes. Equivalent to Go's `go tool stack`. `--once` (default) for
> one-shot; `--watch` for periodic re-dump.

**Done-when:** "attaches to a running Kāra HTTP server and dumps task state
without code changes."

## Why it is blocked — three findings

### 1. v1 has no tracked suspension, by design — so `list_tasks()` has nothing to enumerate

`KaracWaitTarget` (`runtime/src/lib.rs`) ships exactly one variant:

```rust
#[repr(u8)]
#[non_exhaustive]
pub enum KaracWaitTarget {
    /// Worker is running (or, in v1, always — until Phase 6.3 lights up).
    None,
}
```

The doc-comment is explicit: *"v1's blocking runtime has no real suspension to
track … Phase 6.3's network event loop will add `PeerTask { … }` and
`IoHandle { … }` variants additively once it registers real `WaitTarget`s at
I/O-effect-boundary operations."*

Consequently `runtime/stdlib/runtime.kara`'s `list_tasks()` is
`fn list_tasks() -> Vec[TaskInfo] { Vec.new() }` — **empty is correct for v1**,
not a stub oversight. There are no registered suspended tasks because nothing
registers them. `karac inspect` reading `list_tasks()` today would faithfully
report "no suspended tasks."

This is the load-bearing blocker: the done-when target is *a running HTTP
server*, which spends its life **parked in the event loop's accept/poll path**,
not inside a `par {}` block. That parked state is precisely what is not tracked
in v1. So even with a perfect transport, `inspect` on the target workload prints
nothing.

### 2. The metadata that *does* exist is stack-lifetime and par-block-scoped

`list_par_blocks()` works in-process (slice 5 of the Debugger Contract). It
joins two live sources:

- `ACTIVE_FRAMES` — a `Mutex`-protected registry of `*const KaracFrame`
  pointers, one per currently-executing `par {}` branch. Frames are
  **stack-allocated on the pool worker** inside `execute_task` and deregistered
  by `FrameGuard::drop` when the branch returns.
- `KARAC_SPAWN_SITES` — a static LLVM-emitted table mapping `spawn_site_id →
  (file, line, col, worker_count)`.

So the only always-available live task metadata is "which `par` branches are
executing *right now*", and those pointers are valid only while their worker is
mid-branch. Useful for a parallel-compute workload caught mid-`par`; empty for
an idle/parked server.

### 3. No cross-process read path, and the obvious shortcuts are unsound

- **ptrace / `task_for_pid`** (the roadmap's stated mechanism) would have to walk
  `ACTIVE_FRAMES` — a `Mutex<Vec<…>>` of raw pointers — and the `KaracFrame`
  linked structure from *outside* the process, reconstructing Rust data layout
  across the boundary. Fragile, layout-coupled, and unverifiable in the CI
  container (no `yama/ptrace_scope`; ptrace attach is restricted).
- **Signal-triggered dump handled *in the signal handler*** is unsound:
  `list_par_blocks` holds the `ACTIVE_FRAMES` mutex while iterating. If the
  signal interrupts a thread already holding that mutex, taking it in the
  handler deadlocks. This is exactly why the design routes profiler sampling
  through a **lock-free** per-worker cooperation hook rather than the locking
  registry.

## The cooperation hook (Gap 2) — what it is and who needs it

Roadmap line 498 (the `std.runtime.profiler` item, `[ ]`): *"a per-worker atomic
'current task' slot, updated by the scheduler on task entry/exit, readable from
the signal handler in async-signal-safe context."*

- The **profiler** needs it lock-free because it reads from a `SIGPROF` handler
  running in the interrupted worker's own context (async-signal-safe: only
  atomic loads of a fixed-address array, no locks, no allocation).
- **`inspect`** does *not* strictly need lock-free reads **if** it uses a
  dedicated responder thread (below) — a normal thread may take the
  `ACTIVE_FRAMES` lock. But the hook is still the cleanest current-task source
  and is the shared Gap 2 primitive both features are meant to reuse.

**Sound hook design (for when it is built):**

```
static WORKER_FRAME_SLOTS: [AtomicUsize; MAX_WORKERS];   // fixed address → signal-safe
thread_local WORKER_ID: Cell<Option<usize>>;             // assigned in pool() at spawn
```

- `pool()` assigns each spawned worker a stable `0..N` id; `worker_loop` stores
  it in `WORKER_ID`.
- `FrameGuard::new` publishes the frame pointer to `WORKER_FRAME_SLOTS[id]`
  (relaxed store); `FrameGuard::drop` clears it to 0. Two relaxed atomics per
  task — negligible hot-path cost, no control-flow change, no new lock.
- Readers load slots atomically. A non-zero slot means "worker `id` is inside a
  branch with that frame"; the lifetime hazard (the branch may end right after
  the read) is inherent and identical to today's `CURRENT_FRAME` — consumers
  copy `spawn_site_id` best-effort and tolerate a torn/ended read.
- `MAX_WORKERS` cap (e.g. 256): workers beyond it simply don't publish
  (degraded, logged once), keeping the array fixed-address for signal safety.

This piece is bounded and unit-testable in-process (dispatch spinning work, read
the slots from another thread, assert the published frame ids). It is the one
part of `inspect` that could be landed early as tested groundwork — but it has
**no live consumer** until either the profiler or `inspect` transport lands, so
it should land *with* its first consumer, not speculatively ahead of both.

## Sound transport design (for when unblocked): responder thread, not ptrace

Prefer a **cooperative in-process responder** over external ptrace — it is
layout-safe (the dumping code is the same binary that owns the structs),
portable (no per-OS debug API), and verifiable.

1. When `runtime_debug_metadata_enabled()` and (say) `KARA_INSPECT=1`, the
   runtime starts one dedicated **responder thread** at init. It blocks on a
   dedicated realtime signal via `signalfd` (Linux) / a `sigwait` loop, or a
   self-pipe — i.e. it does the work in *normal thread context*, never in a
   signal handler, so taking the `ACTIVE_FRAMES` lock is safe.
2. `karac inspect <pid>` sends that signal (`kill(pid, SIGRTMIN+n)`).
3. The responder snapshots `list_par_blocks()` + `list_tasks()` (+ the
   cooperation-hook slots) and serializes a JSON **inspect snapshot** to a
   well-known path (`$KARA_INSPECT_DIR/kara-inspect-<pid>.json`, default
   `/tmp`), then signals completion (e.g. writes atomically via temp+rename).
4. `karac inspect` waits for the file (bounded timeout), reads, renders, deletes.
   `--watch` loops with an interval; `--once` (default) does one round.

Async-signal-safety is satisfied because **no work happens in a handler** — the
handler (if any) only wakes the responder thread; all snapshotting is ordinary
threaded code. This sidesteps finding #3's deadlock entirely and needs the
cooperation hook only for the (optional) "what is each worker running right now"
line, not for correctness.

### Snapshot schema + renderer

Mirror the `karac debug` split exactly (this spike's sibling work):

- A stable JSON **inspect-snapshot** wire format: `{ pid, captured_at,
  par_blocks: [ {spawn_site_id, file, line, col, worker_count, branches:[…]} ],
  tasks: [ {task_id, wait_target, file, line, callee, effects:[…],
  spawn_site_id, parent} ], workers: [ {worker_id, current_frame} ] }`.
- A Rust model + renderer in the compiler lib (a `runtime_snapshot` module
  alongside `crash_report`), reusing `effect_render::render_compact` for every
  task/par-block effect summary — so `inspect` output reads identically to
  `karac debug`, `karac query effects`, and LSP hover.
- `karac inspect --output=json` re-emits the snapshot; default renders human
  form (parent-task tree, per-worker "currently running", WaitTargets).

The renderer + schema are the *only* part safe to build ahead of the transport,
and only once there is real data to render (i.e. after Phase 6.3), to avoid
shipping display code for a perpetually-empty structure.

## Prerequisites & sequencing

`inspect` sits at the top of a dependency stack. In order:

1. **Phase 6.3 suspension tracking** (event-loop work): real `KaracWaitTarget`
   variants (`PeerTask`, `IoHandle`, channel-wait, timer-wait); register a
   parked task's `KaracFrame` + `WaitTarget` at each I/O-effect boundary in
   `event_loop.rs`; make `list_tasks()` enumerate them. **This is the gating
   item** — without it `inspect` has no data on the target workload. Large,
   concurrency-critical, and not E2E-verifiable without real network load.
2. **Cooperation hook** (Gap 2): the per-worker atomic slot above — lands with
   its first consumer (profiler or this).
3. **Responder-thread transport** + inspect-snapshot schema.
4. **`runtime_snapshot` model + renderer** (compiler lib; reuse `effect_render`).
5. **`karac inspect <pid> [--once|--watch] [--output=json]`** CLI.

Steps 3–5 are bounded and mostly verifiable; step 1 is the deep, risky, and
here-unverifiable prerequisite that makes `inspect` a genuine feature rather than
an empty transport. It should be scheduled as event-loop work with real
network-server load testing, not slipped in under the Track 6 polish umbrella.

## Verifiability note (why this stayed a spike)

The CI/dev container has no `yama/ptrace_scope` (ptrace attach restricted), no
standing long-running Kāra server to attach to, and cannot load-test the event
loop. The gating prerequisite (step 1) lives in the runtime's most
concurrency-critical file. Building and pushing it autonomously — unverified —
would risk a subtle event-loop race in the core runtime. The responsible move is
this design record plus an explicit hand-off, not speculative core-runtime
changes.
