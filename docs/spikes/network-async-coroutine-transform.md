# Design spike — network async transform (phase-6 bug C fix)

**Status:** design spike, 2026-05-30. **DECIDED — A2 chosen and SHIPPED** (the
network-async coroutine transform landed and is default-on; slice table below
tracks 2b→5 done, 5c mechanism done). Follow-on correctness fixes (slice 4 Drop-
across-suspend; `a5fd2798` owned-param reap) recorded inline below.
**Context:** phase-6 line 17 "effect-routed task parking"; bug C in
`docs/implementation_checklist/phase-6-runtime.md`.

## 1. Problem

The network-concurrency subsystem is a **half-built prototype**, and bug C
(network code in any non-`main` function is mis-compiled to a no-op/hang —
confirmed E2E, demo-affecting) is the visible symptom. Two things are
incomplete:

1. **The transform** — `emit_state_machine_poll_fn_for_key`
   (`src/codegen/declarations.rs`) is a *statement classifier*
   (`BodySplitStmt` = a fixed menu of `Let`/`Assign`/`FreeFnCall`/
   `MethodCall` over a narrow `BodyArg` set). It silently drops anything
   outside that menu — confirmed: it drops `let _ = s.write(reply.bytes())`
   (wildcard pattern + method-call arg), producing a degenerate poll-fn
   that never runs the work. It structurally cannot represent control flow
   (`match`/`if`/`while`/`loop`) around yields.
2. **The drive** is a placeholder — the park leaf blocks the *caller OS
   thread* on a condvar (`park_slot_wait`), and the network-boundary call
   intercept (`src/codegen/call_dispatch.rs`) busy-spins with
   `sched_yield`. Neither is the efficient "suspend the task, free the
   thread, re-poll on fd-readiness" model the 1M-connection goal needs.

Completing this is the central remaining concurrency-model work.

## 2. What already exists and is reusable

- **Runtime** (`runtime/src/event_loop.rs`): mio epoll/kqueue event loop,
  `karac_runtime_event_loop_register_fd(fd, dir, parked) -> token`, a
  background poller, a dispatcher thread that on fd-readiness does
  `take_registration(token) -> parked` and re-invokes it, and the
  parked-task ABI `KaracParkedTask { poll_fn, state }`. The fd-registration
  + wakeup-routing is independent of *how* a task suspends, so it carries
  over to any of the options below.
- **Compiler**: yield-point identification + state-struct layout
  (`src/cli.rs` `build_yield_points_table` / `build_state_struct_layouts`),
  network-boundary classification (effect-driven), and — critically — the
  *normal* `@<fn>` body compile is **correct and complete** (it's only the
  poll-fn twin that's degenerate).

## 3. The three options

### A1 — hand-rolled CPS / generator transform
Replace the body-splitter with a real CFG-based state-machine generator
(live-range analysis across yields, split the CFG at suspend points, drop
scheduling, …). **Stackless → preserves density.** But this is the single
hardest thing in the compiler; the broken attempt is evidence. It is
essentially re-implementing LLVM's CoroSplit by hand.

### A2 — LLVM coroutine intrinsics  ← **RECOMMENDED**
Emit each network-boundary function with its **normal, correct body**, but
at each network park emit the `llvm.coro.suspend` dance, mark the function
`presplitcoroutine`, and let LLVM's **CoroSplit** pass generate the state
machine. **Control flow is handled by construction** (LLVM splits the CFG).
**Stackless** (the coro frame holds only cross-suspend live state, ≈ a
hand-tuned state struct) → **preserves the density thesis**. Offloads the
hardest sub-problem to a battle-tested LLVM pass.

### B — stackful coroutines (Go model)
Each task gets its own small growable stack; functions run normally; a park
switches stacks. Simplest compiler (no transform at all — `@handle` just
runs). **But it sacrifices the density thesis**: per-task stacks (Go: 2KB→
8KB) are far larger than a stackless frame, inverting "better than Rust" to
"Go-class / worse than Rust". Plus an FFI-on-small-stack hazard with the
rustls/hyper/syscall-heavy runtime (deep C call stacks need a system-stack
switch). The simplicity is partly illusory here.

## 4. Density (the deciding axis — it *is* the commercial headline)

| Model | Per-task memory | vs the "better than Rust" claim |
|---|---|---|
| A1 stackless (hand) | frame ≈ live cross-suspend state (often <1KB) | preserved |
| **A2 stackless (LLVM coro)** | frame ≈ same, slightly more conservative liveness | **preserved** |
| B stackful | per-task stack 2–8KB+ | **inverted** (Go-class) |

## 5. Recommendation: **A2**

It is the only option that **preserves the density differentiator AND
offloads the hardest compiler problem.** A1 preserves density but is
research-grade hard. B is easy but surrenders the thesis.

**A2 is proven feasible in Kāra's exact toolchain** (inkwell 0.9 / LLVM
18.1). Verified via a throwaway probe (now removed) that:
1. resolved the `presplitcoroutine` attribute kind-id (non-zero);
2. found every `llvm.coro.*` intrinsic by name;
3. **ran a minimal valid coroutine through `coro-early,coro-split,
   coro-cleanup` via `Module::run_passes` and confirmed CoroSplit emitted
   a `.resume` clone** — i.e. the state-machine generation works.

The probe IR (minimal LLVM-18 switched-resume coroutine) is preserved at
the end of this doc as the seed for slice 2.

## 6. A2 implementation sketch (against the existing seams)

> **Implementation constraint discovered during slice-1 prep — coro
> intrinsics must be emitted via raw `llvm-sys`, not inkwell's safe API.**
> inkwell 0.9 *panics* on the LLVM `token` type (`LLVMTokenTypeKind =>
> panic!("FIXME: Unsupported type: Token")` in `types/enums.rs`), and the
> coro intrinsics are token-typed (`coro.id -> token`, `coro.begin(token)`,
> `coro.save -> token`, `coro.suspend(token)`, `coro.end(…, token)`). So the
> coro-scaffolding calls (id/begin/save/suspend/end/free) are emitted with
> `llvm-sys` raw FFI — `LLVMTokenTypeInContext`, `LLVMFunctionType`,
> `LLVMAddFunction`, `LLVMBuildCall2` — interleaved with the inkwell-built
> body (same builder/blocks; grab raw refs via inkwell's `as_*_ref()`).
> Kāra already depends on `llvm-sys` and uses it directly
> (`src/codegen/lljit.rs`), so the interop is established; this is verbosity,
> not a blocker. The **first task of slice 2 is to validate exactly this**:
> emit a minimal coroutine via inkwell-builder + llvm-sys coro intrinsics,
> run it through the (now-wired) coro pipeline, and confirm CoroSplit emits
> the `.resume` clone and it runs — the builder-path analogue of the IR-text
> probe that's already green.
>
> **DONE (slice 2a) — the builder-path emission is validated.** New module
> `src/codegen/coro.rs` carries `CoroIntrinsics` (declare-once raw-`llvm-sys`
> intrinsic table) + `build_demo_coroutine` (the minimal switched-resume
> coroutine emitted through the real codegen API). Its unit test
> `builder_emitted_coroutine_splits` builds the coroutine, verifies the
> module, runs `coro-early,coro-split,coro-cleanup`, and asserts CoroSplit
> emitted `demo_coro.resume` + the post-split module re-verifies — green.
> Two facts the validation pinned down, both load-bearing for slice 2b:
>   1. **The interleave bridge is bidirectional with no memory round-trip.**
>      inkwell value → llvm-sys via `Context::raw()` / `Module::as_mut_ptr()`
>      / `Builder::as_mut_ptr()` / `AsValueRef::as_value_ref()`; llvm-sys
>      result → inkwell via inkwell 0.9's `pub unsafe fn new(LLVMValueRef)` on
>      `IntValue` / `PointerValue` / `FunctionValue`. So the i8 `coro.suspend`
>      result crosses straight back into an inkwell `build_switch` (the resume
>      dispatch) — the transform doesn't need to spill suspend results through
>      memory to stay in inkwell-land.
>   2. **`llvm-sys` had to move from the `lljit_prototype` feature onto the
>      base `llvm` feature** (`llvm = ["inkwell", "dep:llvm-sys"]`) because the
>      AOT codegen path itself now needs it. Same 18.1 pin + `prefer-dynamic`,
>      so still a single LLVM copy; verified the default (no-llvm) build, full
>      codegen (965) + ASAN (73) suites stay green.

- **Pass pipeline** (`src/codegen/driver.rs::apply_optimization_passes`):
  CoroSplit is a *correctness* pass, not an optimization — it must run even
  at `-O0`. **DONE (slice 1):** `coro-early,coro-split,coro-cleanup` now runs
  unconditionally (before the opt pipeline / the `-O0` early-return); pure
  no-op for non-coroutine modules (verified — full codegen + ASAN suites
  green, normal programs build/run identically at `-O0` and `-O2`).
- **The transform** (replace `emit_state_machine_poll_fn_for_key`): compile
  the network-boundary function's body *normally* (reuse `compile_function`
  /`@handle`), but emit `coro.id`/`coro.begin` in the entry ramp, and at
  each network-yield call site emit `coro.save` + register the fd with the
  parked-task = the coroutine handle + `coro.suspend`. The function becomes
  a coroutine; CoroSplit produces the resume/destroy clones + the frame.
- **The drive** (replace the spin-loop in `call_dispatch.rs` + the
  thread-block at the park leaf): the ramp runs to the first suspend,
  registers the fd, and returns the **coroutine handle** to the scheduler.
  The dispatcher, on fd-readiness, calls `llvm.coro.resume(handle)` (instead
  of re-invoking the `poll_fn`); `llvm.coro.done(handle)` checks completion;
  `llvm.coro.destroy(handle)` frees the frame. The parked-task record
  carries the handle.
- **Spawn / TaskGroup**: the spawn wrapper allocates + ramps the coroutine;
  if suspended, hands the handle to the scheduler; on completion stores the
  result. Reuses the existing event-loop registration + dispatcher.
- **Drop-across-suspend** (the main correctness work): Kāra's heap-owning
  locals live in the coro frame; their drops must run on completion AND on
  the `coro.destroy`/cancel path. This is the same problem Rust async
  solves with drop tracking in the generator — it needs deliberate
  scheduling onto the cleanup edges, not a freebie.

## 6½. Key integration finding (the seam map, 2026-05-30)

A read of the existing runtime + drive paths (event_loop.rs, declarations.rs,
call_dispatch.rs, tcp.rs) collapsed slice 2b's risk substantially — two facts
mean the coroutine model drops into the existing machinery almost verbatim:

1. **The runtime dispatcher drives any `KaracParkedTask { poll_fn, state }`
   unchanged.** Its loop is literally `(task.poll_fn)(task.state, &cancel)` →
   interpret `i8` (0 = Pending, 1 = Ready) (event_loop.rs ~2992). So a
   coroutine plugs in with **zero runtime changes** by registering an fd with
   `parked = { poll_fn: @__kara_coro_resume, state: <coro handle> }`. The shim
   (`@__kara_coro_resume`, slice 2b.1) is built to that exact ABI: it
   `coro.resume`s the handle, and on `coro.done` destroys the frame + returns
   Ready, else returns Pending and stays parked. The frame must hold its own
   `KaracParkedTask` (so `&frame.parked` outlives the registration — same
   lifetime contract the current state-struct path already honours).
2. **The leaf already separates park from syscall.** `TcpStream.read/write` /
   `TcpListener.accept` lower (tcp.rs) to *park-then-syscall*: the park
   (`park_on_fd` → register + `park_slot_wait` thread-block) only establishes
   readiness, then a *separate* blocking FFI (`karac_runtime_tcp_read/write/
   accept`) does the actual syscall on the now-ready fd. The coroutine
   transform therefore **keeps the syscall call verbatim** and only swaps the
   park's thread-block for `register_fd(&frame.parked) + coro.suspend`; the
   syscall moves onto the resume edge. No new non-blocking I/O path needed.

Net: the only genuinely new codegen is (a) emitting the network-boundary fn as
a coroutine (ramp/suspend/end) and (b) the leaf suspend swap; the drive,
fd-registration, dispatcher, and syscalls are all reused. Drop-across-suspend
(slice 4) remains the real correctness risk.

## 6¾. Drive-model correction (the 2b.3 design, 2026-05-31)

Designing the 2b.3 *drive* (how a call site runs a coroutine-compiled callee to
completion) surfaced that the original sketch's **synchronous resume-loop**
("`hdl = ramp(args); while !coro.done(hdl) { coro.resume(hdl); sched_yield }`")
is **semantically wrong**, for one concrete reason:

- **The connection fds are non-blocking.** The listener is
  `set_nonblocking(true)` (event_loop.rs ~2692/3351) and accepted sockets
  inherit `O_NONBLOCK` (macOS). `karac_runtime_tcp_read/accept/write` are
  *pure-syscall, no parking* — a `read(2)` on a not-yet-ready fd returns
  `-EWOULDBLOCK`. The FFI's own doc says so: *"EAGAIN/EWOULDBLOCK surface as
  -EAGAIN here too (the readiness assumption was wrong); the parking
  primitive's readiness check should normally prevent it."* So
  **readiness-before-syscall is mandatory** — a caller that `coro.resume`s
  *before the dispatcher has signalled fd-readiness* runs the post-park syscall
  on an unready fd and gets `EWOULDBLOCK`, not data. The main thread must **not**
  drive resume; the **dispatcher** must, exactly when the fd fires.

**Corrected drive (design v2)** — dispatcher-driven, caller waits on a
completion slot. No hidden params, the slice-2b.1 shim is unchanged:

- **Ramp** (`compile_function` for a coroutine key): `coro.id`/`begin → hdl`;
  `slot = karac_runtime_park_slot_new()` stashed **frame-resident**; run the
  normal body; the **first** park's suspend-return edge returns `slot` (not
  `hdl`) to the original caller.
- **Each park** (`emit_state_machine_invocation_for_park_on_fd`, coro branch):
  `start_dispatcher()` (idempotent) → **`save = coro.save(hdl)`** → `register_fd(
  fd, dir, &frame.parked={ poll_fn: @__kara_coro_resume, state: hdl })` →
  `coro.suspend(save, false)` → `switch [0→resume, 1→cleanup]
  default→suspend-return`; the resume edge deregisters and the existing
  post-park syscall lands there verbatim. **The `coro.save` before `register_fd`
  is load-bearing, not optional** (§6's design called for it; this v2 recipe
  originally dropped it to a bare `coro.suspend(false)` / `token none`). The
  instant `register_fd` publishes `&frame.parked` to the (sharded) reactor, the
  dispatcher thread can resume → `coro.done` → `coro.destroy` (free) this frame —
  while the parking thread (a worker, on the first park) is still committing the
  suspend. With `token none`, CoroSplit commits the frame's resume state *at the
  suspend*, after the publish → a cross-thread frame use-after-free (the
  `ws_idle_holder` reconnect-storm glibc heap-corruption crash). `coro.save`
  moves that commit before the publish, per LLVM's "may be resumed on another
  thread before the suspend" contract. Fixed 2026-06-05 — see
  `phase-7-codegen.md` "Cross-thread coroutine-frame use-after-free at an I/O
  park".
- **Body completion**: `karac_runtime_park_slot_signal(slot)` then a **final**
  `coro.suspend(true)`. The dispatcher's last `@__kara_coro_resume` sees
  `coro.done` and `coro.destroy`s the frame; the signal has already woken the
  caller.
- **Call site** (both intercepts) for a coroutine callee: `slot = ramp(args);
  karac_runtime_park_slot_wait(slot); karac_runtime_park_slot_free(slot)`. The
  caller **never** resumes — the dispatcher does all resuming + the destroy.
  Correct for non-blocking fds, and this is exactly 2b.4's spawn shape minus the
  inline wait (spawn returns a handle wrapping `slot`; join waits on it).

**Two consequences that reshape the slice's risk:**

1. **The 2b.2a demo topology is split-correct but drive-INCOMPATIBLE.**
   `build_demo_park_coroutine` runs `coro.free` on its *normal-completion* path
   (resume → cleanup → free → end). That is fine for its only claim — CoroSplit
   survival — but a coroutine that self-frees on completion **UAFs** when the
   dispatcher's shim then calls `coro.done`/`coro.destroy` on the freed frame.
   The production topology must `coro.free` **only on the destroy edge** (suspend
   case 1), reached via `coro.destroy`; normal completion goes through a *final
   suspend* and leaves the frame alive for the shim to destroy. Returning a
   frame-resident value (`slot`) from a block shared with the freed-frame
   cleanup edge is itself a UAF trap — the cleanup edge must `ret` a non-frame
   value (e.g. `null`/`hdl`), only the live-frame suspend edges `ret slot`.
2. **Drive-correctness is only provable by a runtime E2E under ASAN**, not by
   CoroSplit-survival + IR grep. The topology has UAF traps (above) that pass
   `module.verify()` and split cleanly yet fault at runtime. So 2b.3's
   acceptance gate is a *linked, executed, ASAN-clean* handler servicing a real
   connection — the bulk of the slice is iterating the topology against that
   gate, not the emission scaffolding.

Net effect on the estimate: 2b.3 is **not** the "drop into existing machinery
almost verbatim" the §6½ map suggested for the drive. The fd-registration,
dispatcher, slot primitives, and syscalls are still reused — but the
coroutine-frame lifetime/return topology is new, correctness-sensitive, and
runtime-gated. Re-rated below.

**Implemented (2b.3, green E2E + ASAN).** The runtime gate did exactly what it
was supposed to — the topology took **two corrections** against the real LLVM,
neither of which CoroSplit-survival/IR-grep would have caught:

1. The first emission used **two `coro.end`s** (one in cleanup, one in the
   suspend-return). LLVM aborts: *"Only one coro.end can be marked as
   fallthrough."* The canonical shape is a **single** `coro.end` that the
   cleanup edge branches into after `coro.free`.
2. But a single shared `coro.end` block returning the frame-resident `slot`
   re-introduces the §6¾ UAF (the destroy clone loads `slot` after `coro.free`).
   Fix: the **caller passes the completion slot in** as a hidden trailing `ptr`
   param (no `park_slot_new` in the ramp), and the ramp returns **`hdl`** — a
   value, never dereferenced, safe to return from the one `coro.end` on every
   clone including the freed-frame destroy clone. The caller ignores the return
   and waits on the slot it owns. (This is strictly simpler than the
   self-allocate-and-return design and needs no extra runtime support.)

Landed shape, gate, and file map are in the §7 table's "2b.2b + 2b.3 (done)"
row. `tests/coro_e2e.rs` is the gate (functional + `-fsanitize=address`).

## 6⅞. Slice-5 drive design (non-blocking spawn + cancellation)

Slice 5 is the **density headline** — and, like the 2b.3 drive, it has a
semantic trap that must be designed around before coding. Decomposed into three
sub-slices with the constraints surfaced by reading the runtime + spawn codegen.

**The thread-block today (2b.4).** A spawned handler `spawn(|| handle(conn))`
compiles the closure body through the normal `compile_expr` path, so the
coroutine call inside hits the **inline** drive (`call_dispatch.rs`):
`slot=park_slot_new(); ramp(args,slot); park_slot_wait(slot); park_slot_free`.
The `park_slot_wait` **blocks the pool worker** for the entire time the
coroutine is parked → one OS thread per concurrent handler. That is the
opposite of the per-conn-density thesis (the whole reason A2 chose stackless
coroutines): at 1M idle conns it would need ~1M blocked threads.

**Sub-slice 5a — non-blocking spawn (the density win).** The worker must
**ramp and return**, freeing the thread while the dispatcher drives the parked
coroutine to completion. The binding mechanism (validated against the runtime
structs):
  * New runtime `karac_runtime_spawn_coro(wrap_fn, env)` allocates a
    `KaracTaskHandle` **plus a `KaracParkSlot`** stored on the handle, and
    enqueues a worker task whose run-closure calls `wrap_fn(env, slot, cancel)`
    and **returns without marking the handle complete** (the existing
    `karac_runtime_spawn` marks COMPLETED the moment `fn_ptr` returns — wrong
    for a ramp, which returns while the coroutine is still parked).
  * The coroutine's completion is its existing `park_slot_signal(slot)` in
    `emit_coro_finish` — unchanged. `karac_runtime_task_join` on a coro-handle
    **waits on the bound slot** (not the handle's own condvar), so the
    coroutine's completion signal unblocks the joiner directly. The worker is
    long gone.
  * New wrapper ABI `CoroSpawnFn = fn(env, *KaracParkSlot, *AtomicBool)`.
    Codegen emits `__spawn_coro_wrap_N` that unpacks `env` → args, calls the
    **ramp** with the passed slot (register fd + suspend + return — no
    `park_slot_new`/`wait`/`free`), frees `env`. A new `Codegen.coro_spawn_slot:
    Option<PointerValue>` flips the `is_coroutine_compiled` intercept into
    "emit just `ramp(args, slot)`" mode for the wrapper-body compile.
  * **Semantic restriction (load-bearing): tail position only.** The ramp
    returns immediately after the *first* suspend, so any code after the
    coroutine call in the closure would run *while the coroutine is still
    parked* — wrong. So 5a applies only when the closure body **is** a single
    coroutine call (`spawn(|| handle(conn))` — the demo-faithful shape); a
    closure with post-coroutine work or a non-unit tail value falls back to the
    2b.4 blocking spawn. v1 coroutine handlers return unit, so the join result
    is unit. Gate: an E2E that spawns **more handlers than pool workers** and
    services them all concurrently — impossible under the blocking drive.

**Sub-slice 5b — TaskGroup over coroutine handlers + cancel routing.** Make
`tg.spawn(|| handle(conn))` route through 5a's non-blocking path and bind each
child's slot to the group. `TaskGroup.drop` (`taskgroup_join_and_free`) already
joins each child — with 5a, join waits on the coro slot, so the group waits for
the parked coroutines, not blocked workers. Expose `TaskGroup.cancel()` →
flips each child handle's `cancel: AtomicBool` (already plumbed).

**Sub-slice 5c — cooperative cancellation (activates slice 4's destroy edge).**
A flipped `cancel` flag must actually tear the coroutine down via slice 4's
per-park destroy edge. Two pieces, both with traps:
  * The resume shim `__kara_coro_resume(handle, cancel)` checks `cancel`
    *before* `coro.resume`; if set, it calls `coro.destroy(handle)` (runs the
    per-park destroy edge → deregister fd + `emit_coro_destroy_edge_drops` +
    `coro.free`) and returns Ready. **But** the shim only runs on an fd-readiness
    wakeup — a cancelled-but-never-ready coroutine needs the dispatcher to
    *proactively* destroy it (a cancel sweep), which is the harder half.
  * **Slot-signal-on-cancel:** the destroy edge must `park_slot_signal(slot)`
    so a waiter (inline caller, or 5a's join) wakes with a CANCELLED status —
    otherwise cancelling a coroutine **hangs its joiner forever** (slice 4
    deliberately made the destroy edge *not* signal; cancellation has to add
    it, with a status distinct from normal completion). This is why 5c can't be
    a pure shim tweak — it touches the destroy-edge emission and the join ABI.
  * Then the slice-4-deferred **live mid-flight-cancel ASAN+leak test** lands: a
    handler parked across a `Vec[u8]`/`String`, cancelled, must free heap +
    frame exactly once (Linux `detect_leaks=1`), and its joiner must wake.
  * Decide defer-on-cancel: 5a/5b leave `UserDefer` unrun on the destroy edge
    (slice 4's `emit_coro_destroy_edge_drops` skips it); 5c decides whether
    cancel runs user `defer` and, if so, threads the defer-body emission in.

This file's §7 row tracks 5a/5b/5c. Order is forced: 5a before 5c (you can't
cancel a coroutine that's blocking a worker — it has to be parked under the
dispatcher first), 5b composes 5a's binding with the group.

## 7. Effort estimate (honest)

| Slice | Work | Size |
|---|---|---|
| 0 (done) | Toolchain de-risk — CoroSplit works in our LLVM/inkwell | ✅ proven |
| 1 (done) | Run coro passes unconditionally (incl. `-O0`); keep existing tests green | ✅ small |
| 2a (done) | Validate the builder + llvm-sys coro-emission path: `src/codegen/coro.rs` (`CoroIntrinsics` + `build_demo_coroutine`) emits a coroutine through the real codegen API, survives CoroSplit (`.resume` clone), re-verifies. Bidirectional inkwell⇄llvm-sys bridge confirmed; `llvm-sys` promoted to the base `llvm` feature | ✅ small |
| 2b.1 (done) | **Drive bridge.** `CoroIntrinsics` gains the drive intrinsics (`coro.resume`/`coro.done`/`coro.destroy`) + `emit_coro_resume_shim` → `i8 @__kara_coro_resume(ptr handle, ptr cancel)`, whose signature is *exactly* the runtime `KaracParkedTask.poll_fn` ABI. Test `resume_shim_lowers_alongside_coroutine`: the shim lowers cleanly through the coro pipeline (no leftover `@llvm.coro.*`) and re-verifies. This is the zero-runtime-change bridge — see § 6½ | ✅ small |
| 2b.2a (done) | **Leaf suspend EMISSION de-risk.** `src/codegen/coro.rs::build_demo_park_coroutine` emits the production leaf shape — frame-resident parked slot `{poll_fn=@__kara_coro_resume, state=hdl, token}`, `register_fd(fd, dir, &parked)` before `coro.suspend`, deregister+syscall on the resume edge — and `park_shaped_coroutine_splits_with_frame_resident_slot` proves CoroSplit (i) lifts the parked slot into the coro frame so the pointer register_fd captures is a `%demo_park_coro.Frame` GEP (stable address the dispatcher can deref **while suspended**, not a dangling ramp stack alloca), (ii) keeps the registration in the ramp, and (iii) lands the post-park syscall in the `.resume` clone (not dropped — the bug-C failure mode). The resume-edge token reload is what forces frame-residency. Also closed the §8 self-borrow-across-suspend risk (see below) | ✅ small |
| 2b.2b + 2b.3 (done) | **Network-boundary free fn compiles + drives as a coroutine, E2E.** Landed together. Gate: `Codegen.set_coro_enabled` (default off — the ~60 poll-fn/drive tests + ASAN suite stay green; `compile_to_object_with_coro` is the opt-in entry). `coro_fn_keys` = non-generic, non-`main`, non-dotted `state_struct_layouts` keys (free fns this slice; method-handler coroutines are a follow-on). Three coupled toggles on `is_coroutine_compiled`: `declare_function` → `ptr` return + a hidden trailing `ptr` completion-slot param; `compile_function` → `emit_coro_ramp` (coro.id/begin) + normal body + `emit_coro_finish`, body returns routed to the completion block; `tcp.rs::emit_state_machine_invocation_for_park_on_fd` → `emit_coro_park_suspend` (register `{shim,hdl}` + `coro.suspend`, deregister + the existing syscall on the resume edge); call site (`call_dispatch.rs`) → `slot=park_slot_new(); ramp(args, slot); park_slot_wait(slot); park_slot_free(slot)` (dispatcher drives resume via the unchanged 2b.1 shim; caller never resumes). **Final topology** (corrected twice against the real LLVM — see §6¾): the caller passes the slot in (no `park_slot_new` in the ramp) and the ramp returns `hdl`, so the **single canonical `coro.end`** is UAF-safe (returns a value, not the frame-resident `slot`); cleanup (destroy edge) `coro.free`s then branches into that one `coro.end`; completion routes through `park_slot_signal` + a final `coro.suspend(true)`. Gate met: `tests/coro_e2e.rs` — a free-fn handler services a real connection, **linked + executed + ASAN-clean** (two tests; the ASAN one links `-fsanitize=address`). Bug C fixed E2E (the post-park `accept(2)` runs on the resume edge). The degenerate `emit_state_machine_poll_fn_for_key` is left emitted-but-dead for coro keys (DCE'd at -O); flip-default + delete-degenerate is a later slice | ✅ 3–4 |
| 2b.4 (done) | **Spawn drive (functional) + method-handler coroutines.** (a) **Spawn:** a coroutine handler driven inside `spawn(\|\| handle(conn))` already services a real connection E2E with **zero new codegen** — the spawn wrapper (`task_group.rs`) runs 2b.3's coro-drive (`park_slot_new` + ramp + `park_slot_wait`) on a worker-pool thread, which the dispatcher unblocks via the shim; `join` returns after the coroutine completes. Functionally correct but **thread-blocking** (one pool worker per concurrent handler) — the density-optimal non-blocking spawn (wrapper *ramps and returns*, `TaskHandle` completion bound to the coroutine slot) is **slice 5** ("retire the thread-block drive"). (b) **Method handlers:** dropped the free-fn-only (`!key.contains('.')`) restriction and added the coro ramp-drive to `method_call.rs`'s intercept (receiver = the ramp's `self` arg at param 0, method args at 1..K, hidden slot last). Impl methods are named by the dotted key (`make_impl_method_function`), so `coro_fn_keys` / `declare_function` line up unchanged. Gate: `tests/coro_e2e.rs` — `coroutine_spawned_free_fn_services_connection` (spawn) + `coroutine_method_handler_services_connection` (impl method `Acceptor.run`) | ✅ small |
| 3 (done) | Control flow around suspends — it **does** "just work" via CoroSplit; no codegen change. Validated by `tests/coro_e2e.rs`: `coroutine_loop_handler_services_multiple_connections` (a `while` loop with one *static* `accept` suspend resumed across the back-edge, servicing 2 connections — frame-residency of loop locals + the parked-record re-registered each iteration), `coroutine_loop_handler_under_asan` (the same, ASAN-clean — no UAF/double-free across iterations, frame freed once), and `coroutine_if_branch_handler_services_connection` (a park inside a taken `if` branch; the post-`if` join stays reachable on the resume edge). Test-only slice | ✅ small |
| 4 (done) | **Drop-across-suspend correctness.** A heap local live across a park (`Vec[u8]`/`String`/Map handle/RC box) must be dropped exactly once on **every** exit path. The completion path already did — body-end `emit_scope_cleanup` runs before the `coro_return` branch, and the frame-resident buffer is freed on the resume edge. The gap was the **destroy/cancel edge**: the load-bearing trap is that after normal completion the coroutine parks at the *final* suspend and the dispatcher's `coro.destroy` runs the destroy clone down *that* suspend's cleanup edge — so a single shared cleanup block reached by both the final suspend and the mid-flight parks would **double-free** (completion already dropped) or, with no drops, **leak** a mid-flight cancel. Fix (matches Rust's per-suspend-point cleanup): `emit_coro_park_suspend` routes each park's suspend-switch case-1 to a **fresh per-park `kara.coro.destroy.N` block** that (a) **deregisters the fd** — the event loop still points into the about-to-be-freed frame; freeing without this dangles the dispatcher — then (b) drops the heap locals **live at that park** (`emit_coro_destroy_edge_drops`, a snapshot of the `scope_cleanup_actions` stack = the liveness oracle, skipping `UserDefer`/`UserErrDefer`), then branches to the shared free-only `cleanup_bb`. The **final** suspend's destroy edge stays free-only (body already dropped → no double-free). Per-park-vs-final edges are mutually exclusive, so every heap value drops exactly once. Today the per-park destroy edge is **emitted-but-unreached** (the only runtime `coro.destroy` is the dispatcher's post-completion one on the final suspend); a live cancel trigger is slice 5. Gate: `tests/coro_e2e.rs` — `coroutine_heap_local_across_park_services_connection` + `_under_asan` (a `Vec[i64]` live across the `accept` park, completion path, ASAN-clean — Linux `detect_leaks=1` is the leak gate), and `coroutine_heap_local_freed_on_destroy_edge` (structural: the CoroSplit `.destroy` clone deregisters + `free`s the buffer on `kara.coro.destroy.0`, with a non-heap contrast handler proving the free is tied to the live local). New: `compile_to_ir_with_coro_split` (post-CoroSplit IR for the structural assertion). **Known v1 limit:** the destroy edge drops heap but does not run user `defer` blocks (defer-on-cancel waits for slice-5 cancel semantics) | ✅ 1–2 (trickiest) |
| 5a (done) | **Non-blocking spawn (density headline).** Worker ramps + returns; `karac_runtime_spawn_coro(wrap_fn, env)` allocates a `KaracParkSlot` bound to the `KaracTaskHandle` and enqueues a worker whose run-closure calls `wrap_fn(env, slot, cancel)` and **returns without marking the handle terminal** (the existing `karac_runtime_spawn` marks COMPLETED the instant the wrapper returns — wrong for a ramp). `karac_runtime_task_join` on a coro-handle **waits on the bound slot** (the coroutine's existing `park_slot_signal` in `emit_coro_finish`), not the worker; `PENDING`-after-signal reads back as COMPLETED, the ramp-panic path stores PANICKED + signals so the joiner still wakes; `free_handle` frees the slot. Codegen: `Codegen.coro_spawn_slot: Option<PointerValue>` flips the `is_coroutine_compiled` call-site intercept (call_dispatch.rs + method_call.rs) into "emit `ramp(args, slot)` and return — no `park_slot_new`/`wait`/`free`" for the wrapper-body compile; `task_group.rs` detects a **tail free-fn coroutine call** (`spawn_coro_tail_fn_key` — body is exactly `handle(conn)`, directly or trivially block-wrapped), threads the wrapper's slot param through `coro_spawn_slot`, and calls `karac_runtime_spawn_coro` instead of `karac_runtime_spawn`. **Tail-coroutine free-fn closures only** (the ramp returns after the first suspend, so pre/post-call code would run while parked); method-handler / multi-statement / non-unit-tail shapes fall back to the 2b.4 blocking spawn. Gates: `runtime/src/scheduler.rs` — `spawn_coro_join_waits_on_completion_slot_not_worker` (join blocks ~60 ms on the slot while the ramp returned at once) + `spawn_coro_batch_larger_than_workers_all_complete` (64 ramps, no serialize/deadlock — impossible under the blocking drive); `tests/coro_e2e.rs` — `nonblocking_spawn_uses_spawn_coro_ffi` (IR: `spawn_coro` called, no blocking `spawn` call, wrapper has no `park_slot_wait`), `coroutine_nonblocking_spawn_services_connection` + `_under_asan` (`tg.spawn(\|\| serve_one(listener))` services a real connection, TaskGroup-drop joins via the slot, ASAN-clean). See § 6⅞ | ✅ 2 |
| 5b (done) | **TaskGroup over coroutine handlers + cancel routing.** `tg.spawn(\|\| handle(conn))` routes through 5a (landed with 5a's `spawn_coro_tail_fn_key`); `TaskGroup.drop` waits on the parked coroutines (not blocked workers). `TaskGroup.cancel()` → child `cancel` flags (5b-1) + (5c) a cancel-sweep request. See § 6⅞ | ✅ |
| 5c (done 2026-06-06) | **Cooperative cancellation (activates slice 4's destroy edge).** *Mechanism (`4186c3ac`, inert):* the resume shim `__kara_coro_resume(handle, cancel)` checks `cancel` before resuming → `coro.destroy` (per-park destroy edge) instead of resuming, reports Ready; the destroy edge `park_slot_signal`s the slot so a cancelled coroutine's waiter wakes; verified by `coro_e2e.rs::coroutine_cancel_mechanism_emitted`. *Trigger (done):* **(1) per-task cancel routing** — the flag lives on the **registration** (`FdState.cancel`), bound via a new `karac_runtime_event_loop_register_fd_cancel` that the park-suspend calls with the flag read off its slot (`karac_runtime_park_slot_cancel_ptr`); the dispatcher hands each `poll_fn` that flag (null → global fallback). **This supersedes the original `{poll_fn,state,cancel,token}` parked-record plan** — keeping cancel on the registration leaves the parked-record ABI unchanged across the coroutine + degenerate paths (no token-field reshuffle, no misread-token hazard in the degenerate `emit_park_on_fd_poll_body`). `spawn_coro` binds `slot.cancel = &handle.cancel`. **(2) `TaskGroup.cancel()`** → `karac_runtime_taskgroup_cancel` sets each child's `cancel` then `karac_runtime_request_cancel_sweep`. **(3) dispatcher cancel-sweep** (`collect_cancelled`/`sweep_cancelled`, per-shard `sweep_requested` + clear-before-snapshot; collect-then-invoke split so `poll_fn`'s destroy edge can re-take the `fds` lock; dedup via the one-shot `take_registration`) + a register-time park-vs-cancel race guard. **(4) live mid-flight-cancel ASAN test** (`coroutine_taskgroup_cancel_*` + runtime unit `cancel_sweep_tears_down_idle_parked_task`). **(5) defer-on-cancel: RAII-only** for v1 (destroy edge still skips `UserDefer`; carved as a fast-follow). See § 6⅞ | ✅ |
| 6 (density done) | Full E2E (demo-faithful echo + the actual `ws_idle_holder` demo) + **re-measure per-conn density vs Rust** — ✅ **done 2026-06-01**: working-handler 1M+2M, Kāra **12.1 KB/conn**, **2.30× vs Rust**, scale-invariant 1M↔2M (−0.03 %), x86-confirmed (`examples/ws_idle_holder/bench/REPORT.md` ✅ banner; commercial comparator tier — Go/Phoenix/Java/.NET/Node — landed 2026-06-06). Remaining is optional stretch comparators (SignalR/socket.io/Python), deferred, non-blocking | ✅ 1–2 |

**Total ≈ 7–10 bounded, individually-testable slices.** Multi-session, but
every slice has a concrete pass/fail, and the hardest part (state-machine
generation from arbitrary control flow) is LLVM's, not ours. This is a far
better risk profile than A1 (hand-rolling CoroSplit) and keeps the density
thesis B would surrender.

## 8. Risks / open questions

- **`-O0` correctness pass:** must not forget to run coro passes when opt is
  off — a miss = silently un-split coroutines (back to a no-op). Slice 1
  pins this with a test.
- **ABI bridge:** dispatcher switches from `poll_fn(state, cancel)` to
  `coro.resume(handle)` — moderate, localized to the dispatcher + parked
  record. **RESOLVED in principle (§6¾):** zero dispatcher change — the parked
  record's `poll_fn` is the slice-2b.1 shim (`@__kara_coro_resume`), which the
  dispatcher already drives unchanged.
- **Drive model = dispatcher-driven, not caller-resumed (§6¾, 2026-05-31).**
  Connection fds are non-blocking, so the caller must NOT `coro.resume` (it
  would hit `EWOULDBLOCK`); the dispatcher resumes on fd-readiness and the
  caller waits on a `park_slot`. The corrected drive is *simpler* at the call
  site (no resume loop, no hidden param) but the **coroutine-frame
  lifetime/return topology is new and runtime-gated**: `coro.free` only on the
  destroy edge, final-suspend on completion, and no frame-resident value
  returned from the freed-frame cleanup edge. CoroSplit-survival is necessary
  but not sufficient — **the 2b.3 gate is a linked, executed, ASAN-clean run.**
- **Drop ordering across suspends:** the real correctness risk (slice 4);
  ASAN is the gate. **RESOLVED (slice 4, 2026-05-31).** Completion-path drops
  already fired (body-end `emit_scope_cleanup` before the `coro_return`
  branch); the fix added **per-park destroy-edge** drops + fd-deregister
  (`emit_coro_destroy_edge_drops`, live set = the `scope_cleanup_actions`
  snapshot at the park), with the final-suspend edge kept free-only so no value
  double-frees. The destroy edge is emitted-but-unreached until slice 5 wires a
  live cancel; covered by an ASAN completion-path test + a CoroSplit `.destroy`
  clone structural test. v1 limit: heap is dropped on the destroy edge, user
  `defer` is not (defer-on-cancel is slice-5 cancel semantics).
  **Follow-on — owned user-`Drop` PARAMS (`a5fd2798`, 2026-06-05).** Slice 4's
  drop tracking covered heap *locals* live across a park (the `scope_cleanup_actions`
  snapshot = body-declared bindings). It did **not** cover the coroutine's owned
  by-value user-`Drop` **param** — e.g. `fn handle_connection(ws: WebSocket)`. The
  by-value caller-drops model assumes the caller drops the arg after the call
  returns; but a coroutine ramps+returns (or the parent moved the value into the
  task) *before* the body finishes, so nobody ran the param's `Drop`. Result: the
  fd + TLS session leaked on **every disconnect** (the `ws_idle_holder` connection-reap
  leak; identical at 50 conns and 1M). Fix — make the coroutine the owner, reusing
  this exact machinery: a coroutine-compiled fn now `track_user_drop_var`s its owned
  user-`Drop` params, so `emit_scope_cleanup` (completion) and `emit_coro_destroy_edge_drops`
  (per-park destroy edge) drop them; every caller of a coroutine fn suppresses its
  own drop of the owned arg/receiver (no double-drop). Gated to owned non-ref
  non-shared real-`impl Drop` params (`StructDrop`-only owned params excluded —
  `suppress_user_drop_for_var` removes only `UserDrop`). Auto-par: a network-boundary
  statement is no longer auto-parallelized (a coroutine call lifted into a `__par_branch`
  worker would double-drop the moved-in arg). At-scale-validated — co-located churn,
  1M loopback mass-disconnect, and cross-box real-NIC `kill -9`: server fds drain to
  baseline (residual 52, identical across a 500× scale increase and loopback-vs-NIC),
  CLOSE-WAIT 0. Tracked: `phase-7-codegen.md` "Coroutine-param owned user-Drop on
  completion"; tests `tests/coro_e2e.rs::coroutine_drops_owned_user_drop_param` +
  `_spawn_drops_owned_user_drop_param_exactly_once`.
- **Self-borrows across suspend** (Pin-like aliasing): **RESOLVED (2b.2a) —
  no hazard.** Kāra structurally cannot hold a borrow across a park: borrows
  are call-statement-scoped and drained at scope exit
  (`ownership/borrow.rs::drain_borrows_at_depth`; `expr_check.rs` snapshot/
  restore around every Call/MethodCall, so a `ref self` receiver borrow drops
  at the call boundary), there are no reference-typed struct fields
  (`design.md` §"Plain struct field mutability"), and no syntax binds a `ref`
  across a statement. So no coro frame can become self-referential — the
  stable-address-frame assumption is never even exercised.
- **Frame size vs a hand-tuned state struct:** LLVM's liveness can be
  conservative; re-measure density in slice 6 (it's the headline number).

## 9. Why not just finish the body-splitter (A1-lite)

Widening the statement classifier shape-by-shape never reaches control
flow — that path is re-implementing CoroSplit by hand. A2 gets exactly
that, for free and verified.

---

## Appendix — probe IR (seed for slice 2)

Minimal LLVM-18 switched-resume coroutine that survives
`coro-early,coro-split,coro-cleanup` (verified 2026-05-30):

```llvm
define ptr @demo_coro() presplitcoroutine {
entry:
  %id = call token @llvm.coro.id(i32 0, ptr null, ptr null, ptr null)
  %size = call i64 @llvm.coro.size.i64()
  %alloc = call ptr @malloc(i64 %size)
  %hdl = call ptr @llvm.coro.begin(token %id, ptr %alloc)
  %sp = call i8 @llvm.coro.suspend(token none, i1 false)
  switch i8 %sp, label %suspend [i8 0, label %resume
                                 i8 1, label %cleanup]
resume:
  br label %cleanup
cleanup:
  %mem = call ptr @llvm.coro.free(token %id, ptr %hdl)
  call void @free(ptr %mem)
  br label %suspend
suspend:
  %u = call i1 @llvm.coro.end(ptr %hdl, i1 false, token none)
  ret ptr %hdl
}
```
