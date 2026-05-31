# Design spike — network async transform (phase-6 bug C fix)

**Status:** design spike, 2026-05-30. Decision pending (A1 vs **A2** vs B).
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

- **Pass pipeline** (`src/codegen/driver.rs::apply_optimization_passes`):
  CoroSplit is a *correctness* pass, not an optimization — it must run even
  at `-O0`. Today that function returns early when opt level is 0; add the
  coro passes (`coro-early,coro-split,coro-elide,coro-cleanup`)
  unconditionally before the opt pipeline.
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

## 7. Effort estimate (honest)

| Slice | Work | Size |
|---|---|---|
| 0 (done) | Toolchain de-risk — CoroSplit works in our LLVM/inkwell | ✅ proven |
| 1 | Run coro passes unconditionally (incl. `-O0`); keep existing tests green | small |
| 2 | Emit a coroutine for one fn with one network suspend; ramp + register-fd + dispatcher-`coro.resume`. **Goal: a straight-line spawned echo handler services a real connection E2E** (the current no-op) | 2–3 |
| 3 | Control flow (`loop`/`match` around suspends) — should "just work" via CoroSplit; validate against the demo handler shape | 1 (mostly testing) |
| 4 | Drop-across-suspend correctness (heap locals freed on completion + on destroy/cancel) — ASAN-gated | 1–2 (trickiest) |
| 5 | Spawn + TaskGroup + cancellation; retire the spin-loop / thread-block drive | 2 |
| 6 | Full E2E (demo-faithful echo + the actual `ws_idle_holder` demo) + **re-measure per-conn density vs Rust** | 1–2 |

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
  record.
- **Drop ordering across suspends:** the real correctness risk (slice 4);
  ASAN is the gate.
- **Self-borrows across suspend** (Pin-like aliasing): Kāra's ownership/
  borrow model may already preclude the dangerous shapes; needs a look
  before slice 2.
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
