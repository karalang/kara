//! Task scheduler — fresh-task dispatch for `spawn()` and `TaskGroup`.
//!
//! See `docs/design.md § Explicit Concurrency: par {} and spawn()` and
//! `docs/implementation_checklist/phase-6-runtime.md` line 218 slice 3.
//!
//! ## v1 architectural commitments
//!
//! - **Reuses the global `Pool`** from `lib.rs`. Every `spawn()` call wraps
//!   the closure in a 1-task `ParCall` (`remaining = 1`, frame-tracking off)
//!   and pushes a single `Task` onto the same MPMC queue that drains
//!   `karac_par_run` / `karac_par_reduce` work. The worker count is
//!   `resolve_pool_workers()` (honors `KARAC_PAR_WORKERS`; auto-detect floor
//!   at 2; M5 Pro = 18 cores). Sharing the pool is what lets a single
//!   process amortize OS-thread creation across explicit + auto concurrency.
//! - **`KaracTaskHandle` is heap-allocated.** The handle's address (cast to
//!   `i64`) is what `runtime/stdlib/task_group.kara`'s `TaskHandle[T]`
//!   stores in its `task_id` field — codegen produces a `TaskHandle { task_id:
//!   handle_ptr_as_i64 }` literal at the call site and feeds the same i64
//!   back into `karac_runtime_task_join` on `.join()`. Stable addresses
//!   matter: the closure's wrapper writes the result into a pre-allocated
//!   slot inside the handle, and the join thread reads it back from the
//!   same address.
//! - **Result transport: caller-allocated buffer inside the handle.**
//!   `karac_runtime_spawn` allocates `result_size` bytes at `result_align`
//!   (matching the closure's return type as known to codegen) inside the
//!   handle. The spawn wrapper writes the T-typed result into the buffer;
//!   join `memcpy`s from the buffer into the caller's out-slot. This keeps
//!   the runtime monomorphization-free: a single `karac_runtime_spawn`
//!   symbol handles every `T` (`i64`, `String`, user structs) without
//!   per-type expansion.
//! - **Cancellation surface.** v1 ships a per-handle `AtomicBool` cancel
//!   flag the spawn wrapper observes through `*const AtomicBool` (same
//!   convention as `KaracBranch::func` and `KaracParkedTask::poll_fn`).
//!   The flag is always `false` in slice 3 — slice 5's `TaskGroup.drop`
//!   integration will flip it for fail-fast cancellation.
//! - **No work-stealing per-worker queues.** Per the slice plan's "ship
//!   v1 minimum" stance: the global-queue `Pool` already handles
//!   thousands of tasks/sec, and the spawn-rate ceiling for v1 demos
//!   is bounded by Demo 1's accept-loop fan-out (1M+ idle connections
//!   parked on the event loop, not actively scheduling). Per-worker
//!   deques + work-stealing land iff a real workload measures
//!   global-queue contention as the bottleneck.

use std::alloc::Layout;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::{pool, ParCall, Task};

/// Task lifecycle discriminant. Pinned u8 values so codegen-side reads of
/// the join return code are stable across runtime/compiler builds.
///
/// `Pending` is the initial state; the worker writes one of the terminal
/// values before signalling the join Condvar.
pub const TASK_STATE_PENDING: u8 = 0;
pub const TASK_STATE_COMPLETED: u8 = 1;
pub const TASK_STATE_PANICKED: u8 = 2;
pub const TASK_STATE_CANCELLED: u8 = 3;

/// Runtime-side handle for an in-flight or completed `spawn()` task. Its
/// address (cast to `i64`) is the `task_id` field of the Kāra-side
/// `TaskHandle[T]`. Heap-allocated through `Box::into_raw`; freed by
/// `karac_runtime_task_join` (success path) or
/// `karac_runtime_task_handle_free` (drop-without-join path).
pub struct KaracTaskHandle {
    /// One of the `TASK_STATE_*` constants above. Written by the worker
    /// thread under the `notify_mutex`; read by the joining thread after
    /// it observes the Condvar signal.
    ///
    /// Atomic so a concurrent free-after-completion read (e.g. polling a
    /// status field from another thread before joining) is well-defined.
    /// The worker also stores the terminal value through this atomic
    /// before notifying — joiner reads it post-wait.
    pub state: AtomicU8,
    /// Heap-allocated buffer that holds the task's return value once
    /// `state == COMPLETED`. The wrapper closure (codegen-emitted)
    /// memcpys the T-typed result into this buffer right before
    /// signalling completion. `result_layout` is the matching `Layout`
    /// for dealloc; `result_buf` is null when `result_size == 0` (unit-
    /// returning tasks).
    result_buf: *mut u8,
    result_layout: Layout,
    /// Per-task cancel flag. Pointer-stable for the handle's lifetime so
    /// the wrapper can keep observing it across state-machine yields
    /// (relevant once spawn-of-state-machine integration lands in a
    /// later slice). v1 leaves it always `false`.
    cancel: AtomicBool,
    /// Condvar machinery the joiner waits on. The worker takes the
    /// mutex, writes the terminal state, drops the mutex, then notifies.
    /// The joiner reacquires the mutex, checks state, releases.
    notify_mutex: Mutex<()>,
    notify_cv: Condvar,
    /// A2 slice 5a — density-optimal non-blocking coroutine spawn. Null
    /// for an ordinary `spawn` (the worker runs the closure to completion
    /// and the `notify_cv` above is the join signal). Non-null for a
    /// `karac_runtime_spawn_coro` handle: the worker only *ramps* the
    /// coroutine (register fd + suspend + return) and the OS thread is
    /// freed immediately; the dispatcher drives the parked coroutine to
    /// completion, whose body `park_slot_signal`s **this** slot. So a
    /// coro-handle's `karac_runtime_task_join` waits on `coro_slot`, not
    /// `notify_cv` — the worker is long gone and never stores a terminal
    /// state on the normal-completion path (state stays `PENDING`, which
    /// the join reads back as COMPLETED; the ramp-panic path stores
    /// PANICKED + signals the slot so the joiner still wakes).
    coro_slot: *mut crate::event_loop::KaracParkSlot,
    /// B-2026-06-09-1 — set true by `karac_runtime_taskgroup_register`
    /// when this handle is registered with a `TaskGroup`. A registered
    /// handle is freed **exactly once**, by the group's scope-exit
    /// `karac_runtime_taskgroup_join_and_free`; an explicit `.join()` on
    /// it waits + copies the result but does NOT free. Without this, a
    /// `g.spawn(...)` child that the user *also* explicitly `.join()`s is
    /// consumed twice (the join frees, then group-drop joins a dangling
    /// pointer) → use-after-free → SIGSEGV.
    registered: AtomicBool,
    /// B-2026-06-17-2 — set true by [`karac_runtime_task_detach`] when codegen
    /// determines the call-site `TaskHandle` is **discarded** (a bare
    /// `spawn(...);` / `tg.spawn(...);` expression-statement, never bound or
    /// joined). A detached handle has no joiner, so the join path can never
    /// free it; instead it is reaped eagerly — a free-spawn detached handle
    /// self-reaps on completion (see the worker run-closure + the detach
    /// path's [`try_reap_detached_free_spawn`]), and a `tg.spawn` detached
    /// child is reaped by the group's register-time sweep
    /// ([`karac_runtime_taskgroup_register`]). Without this, every discarded
    /// spawn in a long-lived accept loop leaks its handle (+ park slot for the
    /// coro path), ~100 B/conn unbounded.
    detached: AtomicBool,
    /// B-2026-06-17-2 — claims the detached-self-reap free exactly once. Both
    /// the worker's completion block and [`karac_runtime_task_detach`] race to
    /// reap a free-spawn detached handle (either may observe "detached AND
    /// terminal" second); whichever sets this from `false` performs the free.
    /// All transitions happen under `notify_mutex`, so the bool is effectively
    /// a guarded flag — atomic only for the `Sync` requirement. Unused by the
    /// group-sweep path (the group is the sole freer of a registered child).
    reaped: AtomicBool,
}

// SAFETY: KaracTaskHandle stores a raw `*mut u8` (result_buf) which is
// !Send by default. The buffer is exclusively written by the worker
// thread and exclusively read by the joiner thread, with the Condvar
// barrier providing the happens-before edge that orders the worker's
// write before the joiner's read. No concurrent access — the buffer is
// quiescent at every observation point.
unsafe impl Send for KaracTaskHandle {}
unsafe impl Sync for KaracTaskHandle {}

/// Spawn-side wrapper closure signature. Codegen emits one of these per
/// `spawn(closure)` call site. It receives:
/// - `env`: pointer to the captured-environment struct codegen alloca'd
///   (or heap-allocated, for moves out of the source scope);
/// - `result_out`: writable buffer of `result_size` bytes (or null when
///   `result_size == 0`); the wrapper writes the T-typed return value
///   here before returning;
/// - `cancel`: per-task `AtomicBool` pointer the closure body may poll
///   at yield points (state-machine integration lands later — v1 leaves
///   the flag always false).
pub type SpawnFn =
    unsafe extern "C" fn(env: *mut c_void, result_out: *mut u8, cancel: *const AtomicBool);

/// Submit a fresh task for execution on the global worker pool.
///
/// Allocates a `KaracTaskHandle` on the heap, pre-allocates a result
/// buffer matching `(result_size, result_align)`, and pushes a `Task`
/// onto the pool queue. Workers drain the queue and invoke
/// `fn_ptr(env, handle.result_buf, &handle.cancel)`. On normal return
/// the worker stores `TASK_STATE_COMPLETED` + signals the Condvar; on
/// panic it stores `TASK_STATE_PANICKED` and signals.
///
/// Returns a non-null pointer on success. The returned pointer is the
/// caller's responsibility to either join via `karac_runtime_task_join`
/// (which frees the handle) or release via
/// `karac_runtime_task_handle_free` (for handles dropped without
/// joining — TaskGroup-side cleanup will integrate this later).
///
/// # Safety
///
/// - `fn_ptr` must be a valid function pointer with the documented
///   signature.
/// - `env` is opaque to the runtime; lifetime + safety is the caller's
///   responsibility. The wrapper invocation runs on a pool worker
///   thread, so any captures must be `Send`-equivalent (cross-task-safe
///   per design.md § Structured Concurrency Lifetime Guarantees, already
///   enforced structurally by the typechecker).
/// - `result_size` / `result_align` must match the closure's return
///   type. `result_align` must be a power of two when `result_size > 0`;
///   `result_size == 0` is the unit-return convention (no buffer
///   allocated, wrapper receives `null` for `result_out`).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_spawn(
    fn_ptr: SpawnFn,
    env: *mut c_void,
    result_size: usize,
    result_align: usize,
) -> *mut KaracTaskHandle {
    // Allocate the result buffer (or skip for unit-returning tasks).
    let (result_buf, result_layout) = if result_size == 0 {
        (
            std::ptr::null_mut(),
            // A zero-size layout with align 1 satisfies the Drop path's
            // "layout matches the allocation" requirement when result_buf
            // is null — we skip dealloc in that case, but Layout demands
            // a valid align value regardless.
            Layout::from_size_align(0, 1).unwrap(),
        )
    } else {
        let layout = match Layout::from_size_align(result_size, result_align) {
            Ok(l) => l,
            Err(_) => return std::ptr::null_mut(),
        };
        let buf = std::alloc::alloc(layout);
        if buf.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        (buf, layout)
    };

    // Leak the handle to produce a stable raw pointer. Drop happens in
    // `karac_runtime_task_join` / `karac_runtime_task_handle_free`.
    let handle_box = Box::new(KaracTaskHandle {
        state: AtomicU8::new(TASK_STATE_PENDING),
        result_buf,
        result_layout,
        cancel: AtomicBool::new(false),
        notify_mutex: Mutex::new(()),
        notify_cv: Condvar::new(),
        coro_slot: std::ptr::null_mut(),
        registered: AtomicBool::new(false),
        detached: AtomicBool::new(false),
        reaped: AtomicBool::new(false),
    });
    let handle_ptr: *mut KaracTaskHandle = Box::into_raw(handle_box);

    // Per-task ParCall: 1 work unit, no frame tracking. Reuses the
    // existing pool dispatch path so spawn-tasks and par-tasks contend
    // for the same workers.
    let call = Arc::new(ParCall {
        cancel: AtomicBool::new(false),
        remaining: Mutex::new(1),
        notify: Condvar::new(),
        spawn_site_id: 0,
        parent_addr: 0,
        track_frames: false,
    });

    // Capture raw addresses as `usize` so the closure stays `Send`
    // without requiring an `unsafe impl` for the FFI pointers (same
    // pattern as `karac_par_run` / `karac_par_reduce`).
    let env_addr = env as usize;
    let handle_addr = handle_ptr as usize;

    let task = Task {
        call,
        branch_idx: 0,
        run: Box::new(move |_pool_cancel: &AtomicBool| {
            // SAFETY: handle_ptr is the just-leaked Box; nothing else
            // owns or aliases it before this closure runs. The worker is
            // the exclusive writer of handle.state until it signals.
            let handle = unsafe { &*(handle_addr as *const KaracTaskHandle) };
            let env_ptr = env_addr as *mut c_void;
            let cancel_ptr = &handle.cancel as *const AtomicBool;

            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                fn_ptr(env_ptr, handle.result_buf, cancel_ptr);
            }))
            .is_err();

            // Take the mutex, write the terminal state, then notify.
            // Holding the mutex during the write ensures the joiner's
            // wait_while check observes the post-write state.
            //
            // B-2026-06-17-2 — under the same lock, decide whether this
            // completion is responsible for reaping a *detached* free-spawn
            // handle (a discarded `spawn(...);` with no joiner to free it).
            // Deciding under the lock serializes against the detach path
            // ([`karac_runtime_task_detach`]), which takes the same mutex, so
            // the free is claimed exactly once: whichever party observes
            // "detached AND terminal" second sets `reaped` and frees, and the
            // first never touches the handle after releasing the lock. Only
            // free-spawn (unregistered), non-coro handles self-reap here —
            // registered children are the group's to free (the register-time
            // sweep), and coro free-spawn completion is the dispatcher's.
            let should_free = {
                let _g = handle
                    .notify_mutex
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let terminal = if panicked {
                    TASK_STATE_PANICKED
                } else {
                    TASK_STATE_COMPLETED
                };
                handle.state.store(terminal, Ordering::Release);
                handle.notify_cv.notify_all();
                !handle.registered.load(Ordering::Acquire)
                    && handle.coro_slot.is_null()
                    && handle.detached.load(Ordering::Acquire)
                    && !handle.reaped.swap(true, Ordering::AcqRel)
            };
            if should_free {
                // SAFETY: we claimed the sole free via `reaped` under the
                // lock; the handle is detached (no joiner) and the detach
                // path observed `reaped` set, so no other reference remains.
                unsafe { free_handle(handle_addr as *mut KaracTaskHandle) };
            }
        }),
    };

    let p = pool();
    {
        let mut q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(task);
    }
    p.cv.notify_all();

    handle_ptr
}

/// Coroutine-spawn wrapper signature (A2 slice 5a). Codegen emits one per
/// `spawn(|| handle(conn))` site whose closure body is a **tail coroutine
/// call**. Unlike [`SpawnFn`], it receives the bound completion **slot**
/// (not a result buffer): the wrapper unpacks `env` → args and calls the
/// coroutine *ramp* with this slot, which registers the fd + suspends +
/// returns — so the wrapper returns while the coroutine is still parked.
/// The coroutine body later `park_slot_signal`s this same slot at
/// completion (its existing `emit_coro_finish` path), which is what the
/// join waits on.
/// - `env`: captured-environment struct pointer (freed by the wrapper);
/// - `slot`: the `KaracParkSlot` the coroutine ramp is handed as its
///   hidden completion-slot param;
/// - `cancel`: the handle's per-task cancel flag (observed by the
///   cancellation slice, 5c — unused at 5a).
pub type CoroSpawnFn = unsafe extern "C" fn(
    env: *mut c_void,
    slot: *mut crate::event_loop::KaracParkSlot,
    cancel: *const AtomicBool,
);

/// Density-optimal non-blocking coroutine spawn (A2 slice 5a — the per-conn
/// density headline; see `docs/spikes/network-async-coroutine-transform.md`
/// § 6⅞). Where [`karac_runtime_spawn`] blocks a pool worker for the whole
/// time the coroutine is parked (the wrapper's nested `park_slot_wait`),
/// this allocates a `KaracParkSlot`, enqueues a worker that only **ramps**
/// the coroutine and **returns immediately** (freeing the OS thread), and
/// binds the returned `KaracTaskHandle`'s completion to that slot. The
/// dispatcher drives the parked coroutine; its body signals the slot at
/// completion; [`karac_runtime_task_join`] on this handle waits on the
/// slot. Result is unit at v1 (coroutine network handlers return unit), so
/// no result buffer is allocated.
///
/// The worker's run-closure does **not** mark the handle terminal on the
/// normal path — the worker returns the moment the ramp suspends, long
/// before the coroutine completes. State stays `PENDING` and the join reads
/// that back as COMPLETED. Only the ramp-panic path stores `PANICKED` +
/// signals the slot (so a joiner still wakes rather than hanging on a
/// coroutine that never registered).
///
/// Returns a non-null handle pointer; the caller joins it via
/// `karac_runtime_task_join` (which waits on the slot, then frees both the
/// slot and the handle) exactly as for an ordinary spawn handle.
///
/// # Safety
/// - `wrap_fn` must be a valid `CoroSpawnFn` that hands `slot` to a
///   coroutine ramp and frees `env`.
/// - `env` is opaque to the runtime; its lifetime + cross-thread safety is
///   the caller's responsibility (same contract as [`karac_runtime_spawn`]).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_spawn_coro(
    wrap_fn: CoroSpawnFn,
    env: *mut c_void,
) -> *mut KaracTaskHandle {
    // The bound completion slot — the coroutine ramp's hidden param, and
    // the object the join waits on.
    let slot = crate::event_loop::karac_runtime_park_slot_new();

    let handle_box = Box::new(KaracTaskHandle {
        state: AtomicU8::new(TASK_STATE_PENDING),
        result_buf: std::ptr::null_mut(),
        result_layout: Layout::from_size_align(0, 1).unwrap(),
        cancel: AtomicBool::new(false),
        notify_mutex: Mutex::new(()),
        notify_cv: Condvar::new(),
        coro_slot: slot,
        registered: AtomicBool::new(false),
        detached: AtomicBool::new(false),
        reaped: AtomicBool::new(false),
    });
    let handle_ptr: *mut KaracTaskHandle = Box::into_raw(handle_box);

    // Bind the slot's per-task cancel flag to this handle's `cancel`, so the
    // dispatcher (and the cancel-sweep) can hand the coroutine its own flag.
    // The codegen park-suspend copies it from the slot into the parked record.
    // SAFETY: `slot` is live (just allocated); `handle_ptr` is the just-leaked
    // Box — its `cancel` field is stable for the handle's lifetime, which
    // outlives the coroutine (freed by the joiner after teardown/completion).
    unsafe {
        crate::event_loop::karac_runtime_park_slot_bind_cancel(
            slot,
            &(*handle_ptr).cancel as *const AtomicBool,
        );
    }

    let call = Arc::new(ParCall {
        cancel: AtomicBool::new(false),
        remaining: Mutex::new(1),
        notify: Condvar::new(),
        spawn_site_id: 0,
        parent_addr: 0,
        track_frames: false,
    });

    let env_addr = env as usize;
    let handle_addr = handle_ptr as usize;
    let slot_addr = slot as usize;

    let task = Task {
        call,
        branch_idx: 0,
        run: Box::new(move |_pool_cancel: &AtomicBool| {
            // SAFETY: handle_ptr is the just-leaked Box; the worker is the
            // exclusive accessor until the ramp suspends. `slot` outlives
            // this closure (freed only in task_join, after the coroutine
            // signals it).
            let handle = unsafe { &*(handle_addr as *const KaracTaskHandle) };
            let env_ptr = env_addr as *mut c_void;
            let slot_ptr = slot_addr as *mut crate::event_loop::KaracParkSlot;
            let cancel_ptr = &handle.cancel as *const AtomicBool;

            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                // Ramp: register fd + suspend + return. Returns ~immediately;
                // the worker is freed while the coroutine stays parked under
                // the dispatcher.
                wrap_fn(env_ptr, slot_ptr, cancel_ptr);
            }))
            .is_err();

            // Normal path: do NOT touch terminal state — the coroutine
            // completes later and signals the slot; the join reads PENDING
            // back as COMPLETED. Only a ramp panic (the coroutine never
            // registered → the dispatcher will never signal the slot) needs
            // to wake a waiting joiner here.
            if panicked {
                let _g = handle
                    .notify_mutex
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                handle.state.store(TASK_STATE_PANICKED, Ordering::Release);
                // SAFETY: slot is live (not yet freed by task_join).
                unsafe { crate::event_loop::karac_runtime_park_slot_signal(slot_ptr) };
            }
        }),
    };

    let p = pool();
    {
        let mut q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(task);
    }
    p.cv.notify_all();

    handle_ptr
}

/// Block until the task completes, copy the result into `out_slot`, and
/// free the handle. Returns one of the `TASK_STATE_*` terminal codes
/// (never `PENDING` — the function only returns after a terminal
/// observation).
///
/// `out_slot` may be null when the task's return type is unit
/// (`result_size == 0` at spawn time). On a non-COMPLETED terminal
/// (panic / cancel), the contents of `*out_slot` are unspecified;
/// callers should branch on the return code before reading.
///
/// **Free contract.** This function takes ownership of the handle and
/// frees it before returning. The caller must not dereference `handle`
/// after the call returns.
///
/// # Safety
///
/// - `handle` must be a non-null `*mut KaracTaskHandle` produced by
///   `karac_runtime_spawn` and not already passed to
///   `karac_runtime_task_join` / `karac_runtime_task_handle_free`.
/// - `out_slot` must point at writable storage of at least
///   `result_size` bytes at `result_align` alignment (the values
///   passed at spawn time), or be null when the task is unit-returning.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_task_join(
    handle: *mut KaracTaskHandle,
    out_slot: *mut u8,
) -> u8 {
    if handle.is_null() {
        return TASK_STATE_CANCELLED;
    }
    // B-2026-06-09-1 — a handle registered with a `TaskGroup` is freed by
    // the group's scope-exit `karac_runtime_taskgroup_join_and_free`, not
    // here. An explicit `.join()` on such a handle must wait + copy the
    // result but leave the handle alive for the group to reclaim; freeing
    // it here too would double-consume it (use-after-free at group drop).
    let do_free = !(*handle).registered.load(Ordering::Acquire);
    join_inner(handle, out_slot, do_free)
}

/// Wait for `handle`'s task to reach a terminal state, copy its result
/// into `out_slot` (when non-null and the task completed), and free the
/// handle iff `do_free`. The single waiter+reaper for both the explicit
/// `.join()` FFI path and the `TaskGroup` join barrier.
///
/// The wait is idempotent on an already-terminal task — both the
/// `notify_cv` state-wait and the `coro_slot` `park_slot_wait` (a
/// `while !done` loop) return immediately once the task is done — so it
/// is sound to call this twice on one handle (explicit `.join()` with
/// `do_free = false`, then the group with `do_free = true`).
///
/// # Safety
///
/// `handle` must be a non-null live `*mut KaracTaskHandle`. When
/// `do_free` is false the caller guarantees a later call with
/// `do_free = true` reclaims it exactly once.
unsafe fn join_inner(handle: *mut KaracTaskHandle, out_slot: *mut u8, do_free: bool) -> u8 {
    let h = &*handle;

    // A2 slice 5a — coroutine-spawn handle: completion is the dispatcher
    // signalling the bound slot (the worker that ramped it is long gone and
    // never stores a terminal state on the normal path). Wait on the slot,
    // not the worker's `notify_cv`. A `PENDING` state after the slot fires
    // means the coroutine completed normally (the run-closure left state
    // untouched); a non-PENDING state is the ramp-panic override.
    if !h.coro_slot.is_null() {
        crate::event_loop::karac_runtime_park_slot_wait(h.coro_slot);
        let st = h.state.load(Ordering::Acquire);
        let terminal = if st == TASK_STATE_PENDING {
            TASK_STATE_COMPLETED
        } else {
            st
        };
        if do_free {
            free_handle(handle);
        }
        return terminal;
    }

    // Wait until the worker writes a terminal state.
    let guard = h.notify_mutex.lock().unwrap_or_else(|p| p.into_inner());
    let _final_guard = h
        .notify_cv
        .wait_while(guard, |_| {
            h.state.load(Ordering::Acquire) == TASK_STATE_PENDING
        })
        .unwrap_or_else(|p| p.into_inner());

    let terminal = h.state.load(Ordering::Acquire);

    // On successful completion, transfer the result into the caller's
    // slot. The worker's release-store + our acquire-load orders the
    // memcpy after the wrapper's result write.
    if terminal == TASK_STATE_COMPLETED
        && !out_slot.is_null()
        && !h.result_buf.is_null()
        && h.result_layout.size() > 0
    {
        std::ptr::copy_nonoverlapping(h.result_buf, out_slot, h.result_layout.size());
    }

    // Drop the result buffer + reclaim the handle (unless a registering
    // group owns the free — see `do_free`).
    // Re-take ownership of the Box and let it drop the handle struct;
    // the result buffer is freed manually since it was alloc'd via
    // `std::alloc::alloc`, not boxed.
    drop(_final_guard);
    if do_free {
        free_handle(handle);
    }

    terminal
}

/// Release a `KaracTaskHandle` without joining. For TaskGroup-side
/// cleanup paths that drop unjoined handles (slice 5 will integrate
/// this into `TaskGroup.drop`) and for unit tests that need to discard
/// handles eagerly.
///
/// **Does not wait for the worker to finish.** Calling this while a
/// pool worker still holds a reference to the handle through its
/// in-flight closure is unsound. v1 callers are expected to either join
/// every spawned task or to free only handles whose tasks have already
/// observed a terminal state (e.g., via a polling status check —
/// reserved for a follow-up FFI export when a user needs it).
///
/// # Safety
///
/// `handle` must be a non-null `*mut KaracTaskHandle` produced by
/// `karac_runtime_spawn` and not already freed. The task it identifies
/// must have completed (terminal state stored). Caller is responsible
/// for ensuring no other thread holds a reference.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_task_handle_free(handle: *mut KaracTaskHandle) {
    if handle.is_null() {
        return;
    }
    free_handle(handle);
}

/// Free a `KaracTaskHandle` and its result buffer.
///
/// # Safety
///
/// Same contract as `karac_runtime_task_handle_free`. Internal helper
/// shared between the join + free paths.
unsafe fn free_handle(handle: *mut KaracTaskHandle) {
    // Take ownership of the buffer fields before dropping the Box so we
    // can dealloc through the saved Layout.
    let boxed = Box::from_raw(handle);
    if !boxed.result_buf.is_null() && boxed.result_layout.size() > 0 {
        std::alloc::dealloc(boxed.result_buf, boxed.result_layout);
    }
    // A2 slice 5a — free the bound completion slot for a coroutine-spawn
    // handle. The coroutine has signalled it (the join's slot-wait returned)
    // and will not touch it again, so this is the single free site.
    if !boxed.coro_slot.is_null() {
        crate::event_loop::karac_runtime_park_slot_free(boxed.coro_slot);
    }
    // `boxed` drops here, releasing the Mutex / Condvar / AtomicU8 / etc.
}

/// B-2026-06-17-2 — non-blocking test for "this registered child is detached
/// **and** has reached a terminal state", used by the group's register-time
/// eager-reap. A `true` result means the child can be freed now without a
/// joiner and without blocking.
///
/// The terminal probe establishes the same happens-before edge the join path
/// relies on before freeing:
/// - **coro child** (`coro_slot` non-null): terminal == the dispatcher has
///   signalled the bound slot, read non-blockingly via `park_slot_done` (its
///   mutex-acquire orders our free after the dispatcher's last slot touch).
/// - **non-coro child**: terminal == the worker stored a terminal state;
///   acquiring `notify_mutex` guarantees the worker has finished its locked
///   store+notify and will not touch the handle again.
///
/// # Safety
///
/// `child` must be a non-null live `*mut KaracTaskHandle`.
unsafe fn child_is_terminal_detached(child: *mut KaracTaskHandle) -> bool {
    let h = &*child;
    if !h.detached.load(Ordering::Acquire) {
        return false;
    }
    if !h.coro_slot.is_null() {
        crate::event_loop::karac_runtime_park_slot_done(h.coro_slot)
    } else {
        let _g = h.notify_mutex.lock().unwrap_or_else(|p| p.into_inner());
        h.state.load(Ordering::Acquire) != TASK_STATE_PENDING
    }
}

/// Read the task's current state without joining. Returns one of the
/// `TASK_STATE_*` constants. Useful for tests + future polling APIs.
///
/// **Not a join.** A `PENDING` return means the task is still in flight;
/// callers needing the result must use `karac_runtime_task_join`.
///
/// # Safety
///
/// `handle` must be a non-null `*mut KaracTaskHandle` produced by
/// `karac_runtime_spawn` and not already freed.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_task_state(handle: *const KaracTaskHandle) -> u8 {
    if handle.is_null() {
        return TASK_STATE_CANCELLED;
    }
    (*handle).state.load(Ordering::Acquire)
}

/// B-2026-06-17-2 — mark a `spawn`/`tg.spawn` handle **detached**. Codegen
/// emits this at a spawn site whose result `TaskHandle` is discarded (a bare
/// `spawn(...);` / `tg.spawn(...);` expression-statement, never bound or
/// `.join()`ed). A detached handle has no joiner, so the ordinary
/// join-frees-the-handle path can never reclaim it — detach enables eager
/// reaping instead, closing the per-connection leak in long-lived accept
/// loops (`loop { tg.spawn(|| handle(conn)) }`).
///
/// For a **free-spawn** handle (not registered with any group, non-coro) this
/// also performs the detach side of the self-reap handshake: if the task has
/// already completed, the worker's completion block found `detached == false`
/// and skipped the free, so detach reclaims the handle here. The decision is
/// taken under `notify_mutex` — the same lock the worker holds while storing
/// terminal state — so the free is claimed exactly once across the two
/// parties (see `karac_runtime_spawn`'s completion block).
///
/// A **registered** (`tg.spawn`) handle is never freed here — the owning group
/// reaps detached children in [`karac_runtime_taskgroup_register`]'s sweep. A
/// **coro** free-spawn handle is likewise only flagged (its completion is
/// dispatcher-driven; the coro free-spawn self-reap is a follow-up slice).
///
/// # Safety
///
/// `handle` must be a non-null live `*mut KaracTaskHandle` from
/// `karac_runtime_spawn` / `karac_runtime_spawn_coro`, not already freed.
/// Codegen only emits this for discarded handles, which are never joined, so
/// it runs at most once per handle and never races an explicit `.join()`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_task_detach(handle: *mut KaracTaskHandle) {
    if handle.is_null() {
        return;
    }
    let should_free = {
        let h = &*handle;
        let _g = h.notify_mutex.lock().unwrap_or_else(|p| p.into_inner());
        h.detached.store(true, Ordering::Release);
        // Mirror the worker's claim (see `karac_runtime_spawn`): only a
        // terminal, non-coro, unregistered handle self-reaps, exactly once.
        !h.registered.load(Ordering::Acquire)
            && h.coro_slot.is_null()
            && h.state.load(Ordering::Acquire) != TASK_STATE_PENDING
            && !h.reaped.swap(true, Ordering::AcqRel)
    };
    if should_free {
        free_handle(handle);
    }
}

// ── TaskGroup container — slice 5 ──────────────────────────────────────────
//
// Structured-concurrency boundary per design.md § Explicit Concurrency.
// Every `tg.spawn(closure)` call site registers the returned handle with
// the group; `TaskGroup.drop` joins every registered child before
// returning. Children that haven't reached a terminal state by the time
// drop runs are blocked on (the same Condvar `karac_runtime_task_join`
// waits on); children that have completed are reaped without blocking.
//
// **v1 cancel-propagation discipline.** A child that panics aborts the
// process under `panic = "abort"` (Rust auto-aborts panics crossing
// `extern "C"` boundaries — see `karac_runtime_spawn`'s wrapper
// commentary). Fail-fast cancel propagation — flipping the
// `KaracTaskHandle.cancel` flag of every sibling on the first panicked
// child — is a follow-on slice (5b) that pairs with `catch_panic[T]`
// when the unwind-aware build profile lands. v1 ships the wait-for-all
// path because (i) the panic=abort posture makes the v1 panic case
// rare and unrecoverable anyway; (ii) the cooperative-cancel surface
// (where a child voluntarily checks the cancel flag and returns early)
// is the more useful surface and lands independently as the per-handle
// AtomicBool already plumbed in slice 3 — slice 5b will expose
// `TaskGroup.cancel()` (user-callable) routing to the same flag.

/// Runtime-side container for a `TaskGroup` value. Heap-allocated by
/// `karac_runtime_taskgroup_new`; freed by
/// `karac_runtime_taskgroup_join_and_free` at scope exit.
///
/// `children` holds raw pointers to child `KaracTaskHandle`s. The
/// pointers are valid for the duration of the group's lifetime: every
/// registered child handle is joined and freed inside
/// `karac_runtime_taskgroup_join_and_free`'s loop, so no child outlives
/// the group.
pub struct KaracTaskGroupHandle {
    children: Mutex<Vec<*mut KaracTaskHandle>>,
}

// SAFETY: see `KaracTaskHandle`'s Send/Sync impl. The same reasoning
// applies — children raw pointers are exclusively read by the
// `karac_runtime_taskgroup_join_and_free` loop in the drop-emitter
// thread, after every spawn site that registered them has completed
// (the spawn site's `karac_runtime_taskgroup_register` is a
// happens-before edge with the join read via the Mutex).
unsafe impl Send for KaracTaskGroupHandle {}
unsafe impl Sync for KaracTaskGroupHandle {}

/// Allocate a fresh `KaracTaskGroupHandle` and return its raw pointer
/// (cast to `i64` on the kara side as the `TaskGroup.id` field).
#[no_mangle]
pub extern "C" fn karac_runtime_taskgroup_new() -> *mut KaracTaskGroupHandle {
    Box::into_raw(Box::new(KaracTaskGroupHandle {
        children: Mutex::new(Vec::new()),
    }))
}

/// Register a freshly spawned child handle with the group. Called by
/// codegen at every `tg.spawn(closure)` site, right after
/// `karac_runtime_spawn` returns and before the `TaskHandle { task_id:
/// <child_ptr> as i64 }` wrap.
///
/// # Safety
///
/// - `group` must be a non-null pointer produced by
///   `karac_runtime_taskgroup_new` and not already freed.
/// - `child` must be a non-null `*mut KaracTaskHandle` produced by
///   `karac_runtime_spawn` for a closure spawned via this group's
///   `tg.spawn()` call.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_taskgroup_register(
    group: *mut KaracTaskGroupHandle,
    child: *mut KaracTaskHandle,
) {
    if group.is_null() || child.is_null() {
        return;
    }
    // B-2026-06-09-1 — mark the child group-owned so an explicit
    // `.join()` on it waits without freeing; the group is the sole freer.
    (*child).registered.store(true, Ordering::Release);
    let g = &*group;
    let mut children = g.children.lock().unwrap_or_else(|p| p.into_inner());
    children.push(child);

    // B-2026-06-17-2 — eager-reap detached, completed children. The canonical
    // server shape `loop { tg.spawn(|| handle(conn)) }` creates the group once
    // and never exits its scope, so without this every completed child's
    // handle (+ park slot, for the coro path) is retained in `children`
    // forever — ~100 B/conn, unbounded. Sweeping here bounds the Vec to the
    // count of concurrently-live children (~conc): each freshly registered
    // spawn pays one O(live) pass that frees any sibling that has since
    // finished. Only **detached** children (the discarded fire-and-forget
    // shape) are reaped — a child whose handle the user retained for `.join()`
    // is never detached, so it stays for scope-exit `join_and_free`, preserving
    // the structured wait guarantee and the B-2026-06-09-1 sole-freer
    // invariant. `child_is_terminal_detached` is a UAF-safe non-blocking probe.
    children.retain(|&c| {
        if child_is_terminal_detached(c) {
            // Detached + terminal + registered: the group is the sole freer and
            // no joiner exists, so free now and drop from the Vec — scope-exit
            // `join_and_free` then never sees (and never double-frees) it.
            free_handle(c);
            false
        } else {
            true
        }
    });
}

/// Block until every registered child has reached a terminal state,
/// then free the group itself. Each child handle is consumed here — the
/// group is the **sole freer** of a registered handle (B-2026-06-09-1):
/// registration sets the child's `registered` flag so any explicit
/// `.join()` waits + copies the result but does *not* free, leaving the
/// reclaim to this loop. The "freed exactly once" invariant holds
/// because registration is the only path that adds entries here, every
/// entry is freed here via `join_inner(.., true)`, and the group cannot
/// be referenced again after this returns (codegen emits the call from
/// `@TaskGroup.drop`).
///
/// **Discards each child's result.** TaskGroup's design.md contract
/// is "wait for children", not "collect children's results"; explicit
/// `.join()` on individual handles is the user-facing surface for
/// result extraction. Slice 5 ships join-and-discard via a
/// null-handled-out-slot `karac_runtime_task_join` call (the runtime
/// skips the result memcpy when `out_slot == null`).
///
/// # Safety
///
/// `group` must be a non-null pointer produced by
/// `karac_runtime_taskgroup_new` and not already freed.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_taskgroup_join_and_free(group: *mut KaracTaskGroupHandle) {
    if group.is_null() {
        return;
    }
    // Drain children outside the lock so a child whose closure body
    // calls `tg.spawn(...)` recursively (unlikely but defensible)
    // doesn't deadlock on a re-entrant lock acquire.
    let children: Vec<*mut KaracTaskHandle> = {
        let g = &*group;
        let mut guard = g.children.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut *guard)
    };
    for child in children {
        // `join_inner` blocks until terminal, frees the handle, returns
        // the terminal discriminant (v1 discards it). Call with
        // `do_free = true` directly — the group is the sole freer of a
        // registered child, and going through the FFI `..._task_join`
        // would see `registered == true` and skip the free → leak.
        let _status = join_inner(child, std::ptr::null_mut(), true);
    }
    // Reclaim the group itself.
    drop(Box::from_raw(group));
}

/// Signal cooperative cancellation to every child task registered with the
/// group: flips each child handle's per-task `cancel` flag. The user-facing
/// `TaskGroup.cancel()` method lowers to this. Complements the implicit
/// fail-fast cancel-on-child-failure (handled at drop) with an explicit,
/// user-driven trigger.
///
/// **A2 slice 5c — live.** Flips each child's flag, then drives a cancel-sweep
/// via [`crate::event_loop::karac_runtime_request_cancel_sweep`]. The flag now
/// reaches the coroutine two ways: the dispatcher hands each `poll_fn` the
/// task's *own* cancel flag (copied into the parked record from the bound
/// slot), so a child that becomes fd-ready observes cancellation; and the sweep
/// proactively tears down children parked on idle fds that would never get a
/// readiness wakeup. The resume shim (`__kara_coro_resume`) runs `coro.destroy`
/// → the per-park destroy edge (deregister + RAII drops + slot-signal), so the
/// group's join wakes. RAII cleanup runs on cancel; user `defer {}`-on-cancel
/// is a documented fast-follow (the destroy edge still skips `UserDefer`).
///
/// # Safety
///
/// `group` must be a non-null pointer produced by `karac_runtime_taskgroup_new`
/// and not already freed. Registered child handles must still be live — they
/// are: a child is joined+freed only by `karac_runtime_taskgroup_join_and_free`
/// at group drop, which cannot overlap a `cancel()` call on the same live group.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_taskgroup_cancel(group: *mut KaracTaskGroupHandle) {
    if group.is_null() {
        return;
    }
    {
        let g = &*group;
        // Hold the children lock for the flip so a concurrent `register` (a
        // child closure recursively spawning into the same group) cannot race
        // the walk. Released before requesting the sweep so the wake path
        // never couples with the children lock.
        let children = g.children.lock().unwrap_or_else(|p| p.into_inner());
        for &child in children.iter() {
            if child.is_null() {
                continue;
            }
            // SAFETY: each registered child handle is live for the group's
            // lifetime (freed only at group drop, exclusive with this call).
            // Same-module access to the private `cancel` flag.
            (*child).cancel.store(true, Ordering::Release);
        }
    }
    // Slice 5c trigger: the flags are flipped (Release). Now drive a sweep so a
    // child parked on an idle fd (no readiness wakeup forthcoming) is torn down
    // promptly instead of hanging the group's join. `request_cancel_sweep`'s
    // per-shard stores are Release-ordered after the flag flips above, and the
    // dispatcher consumes them with an AcqRel swap — so every flipped flag is
    // visible to the sweep that observes the request.
    crate::event_loop::karac_runtime_request_cancel_sweep();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
    use std::time::{Duration, Instant};

    // Spawn-side wrapper for tests that return i64. Reads an i64 from
    // `env` and writes (env_val + 1) to `result_out`.
    unsafe extern "C" fn add_one_wrapper(
        env: *mut c_void,
        result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        let val = *(env as *const i64);
        *(result_out as *mut i64) = val + 1;
    }

    // Spawn-side wrapper that increments a shared AtomicU32 in env (no
    // result, used for unit-returning task tests).
    unsafe extern "C" fn inc_counter_wrapper(
        env: *mut c_void,
        _result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        let counter = &*(env as *const AtomicU32);
        counter.fetch_add(1, AtomicOrdering::Relaxed);
    }

    #[test]
    fn spawn_and_join_returns_i64_result() {
        let env_val: i64 = 41;
        unsafe {
            let handle = karac_runtime_spawn(
                add_one_wrapper,
                &env_val as *const i64 as *mut c_void,
                std::mem::size_of::<i64>(),
                std::mem::align_of::<i64>(),
            );
            assert!(!handle.is_null());

            let mut out: i64 = 0;
            let status = karac_runtime_task_join(handle, &mut out as *mut i64 as *mut u8);
            assert_eq!(status, TASK_STATE_COMPLETED);
            assert_eq!(out, 42);
        }
    }

    #[test]
    fn spawn_returning_unit_uses_null_result_slot() {
        let counter = AtomicU32::new(0);
        unsafe {
            let handle = karac_runtime_spawn(
                inc_counter_wrapper,
                &counter as *const AtomicU32 as *mut c_void,
                0,
                1,
            );
            assert!(!handle.is_null());
            let status = karac_runtime_task_join(handle, std::ptr::null_mut());
            assert_eq!(status, TASK_STATE_COMPLETED);
            assert_eq!(counter.load(AtomicOrdering::Relaxed), 1);
        }
    }

    // Panic propagation note: `TASK_STATE_PANICKED` is API-level
    // future-proofing for catch_panic[T] integration; v1 ships
    // panic=abort in release and Rust auto-aborts on a panic crossing
    // an `extern "C"` boundary (regardless of profile), so a panicking
    // SpawnFn aborts the process rather than producing PANICKED. The
    // catch_unwind in `karac_runtime_spawn`'s closure still guards
    // against Rust-side panics from runtime infrastructure (allocator
    // failure, poisoned locks during shutdown, etc.) — those are
    // currently not exercised by unit tests but the surface stays for
    // symmetry with `execute_task` (`lib.rs:432`).

    #[test]
    fn spawn_multiple_tasks_each_joins_independently() {
        // 64 concurrent spawns each adding 1 to their own env value.
        // Each must join with the right result; total sum verified after.
        const N: usize = 64;
        let env_vals: Vec<i64> = (0..N as i64).collect();
        let mut handles: Vec<*mut KaracTaskHandle> = Vec::with_capacity(N);

        unsafe {
            for v in &env_vals {
                let h = karac_runtime_spawn(
                    add_one_wrapper,
                    v as *const i64 as *mut c_void,
                    std::mem::size_of::<i64>(),
                    std::mem::align_of::<i64>(),
                );
                assert!(!h.is_null());
                handles.push(h);
            }

            let mut sum: i64 = 0;
            for (i, h) in handles.into_iter().enumerate() {
                let mut out: i64 = 0;
                let status = karac_runtime_task_join(h, &mut out as *mut i64 as *mut u8);
                assert_eq!(status, TASK_STATE_COMPLETED);
                assert_eq!(out, env_vals[i] + 1);
                sum += out;
            }
            // Sum of (0+1) + (1+1) + ... + (63+1) = sum(1..=64) = 2080
            assert_eq!(sum, 2080);
        }
    }

    #[test]
    fn task_state_observes_pending_then_completed() {
        // Spin until the task transitions; bounded by a generous timeout.
        let env_val: i64 = 0;
        unsafe {
            let handle = karac_runtime_spawn(
                add_one_wrapper,
                &env_val as *const i64 as *mut c_void,
                std::mem::size_of::<i64>(),
                std::mem::align_of::<i64>(),
            );

            // Poll until terminal — observation may catch either state
            // depending on scheduling, but must reach COMPLETED within the
            // budget.
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let s = karac_runtime_task_state(handle);
                if s == TASK_STATE_COMPLETED {
                    break;
                }
                assert!(
                    s == TASK_STATE_PENDING || s == TASK_STATE_COMPLETED,
                    "unexpected state {}",
                    s
                );
                if Instant::now() > deadline {
                    panic!("task did not complete within budget");
                }
                std::thread::sleep(Duration::from_millis(1));
            }

            let mut out: i64 = 0;
            let status = karac_runtime_task_join(handle, &mut out as *mut i64 as *mut u8);
            assert_eq!(status, TASK_STATE_COMPLETED);
            assert_eq!(out, 1);
        }
    }

    #[test]
    fn task_handle_free_releases_without_join() {
        // Spin until terminal so the free is safe.
        let env_val: i64 = 0;
        unsafe {
            let handle = karac_runtime_spawn(
                add_one_wrapper,
                &env_val as *const i64 as *mut c_void,
                std::mem::size_of::<i64>(),
                std::mem::align_of::<i64>(),
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            while karac_runtime_task_state(handle) == TASK_STATE_PENDING {
                if Instant::now() > deadline {
                    panic!("task did not reach terminal state");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            karac_runtime_task_handle_free(handle);
            // (No use-after-free assertion possible here without test-
            // harness instrumentation; this test pins the free path exists
            // and does not crash.)
        }
    }

    #[test]
    fn null_handle_join_returns_cancelled() {
        unsafe {
            let status = karac_runtime_task_join(std::ptr::null_mut(), std::ptr::null_mut());
            assert_eq!(status, TASK_STATE_CANCELLED);
        }
    }

    #[test]
    fn null_handle_state_returns_cancelled() {
        unsafe {
            let s = karac_runtime_task_state(std::ptr::null());
            assert_eq!(s, TASK_STATE_CANCELLED);
        }
    }

    #[test]
    fn task_state_constants_pinned() {
        // Codegen-side reads of the join return code depend on these
        // discriminants. A test failure here means the ABI moved and
        // codegen must be regenerated in lockstep.
        assert_eq!(TASK_STATE_PENDING, 0);
        assert_eq!(TASK_STATE_COMPLETED, 1);
        assert_eq!(TASK_STATE_PANICKED, 2);
        assert_eq!(TASK_STATE_CANCELLED, 3);
    }

    // ── TaskGroup container — slice 5 tests ──────────────────────

    #[test]
    fn taskgroup_new_returns_non_null_handle() {
        unsafe {
            let g = karac_runtime_taskgroup_new();
            assert!(!g.is_null());
            // Empty groups must still drain + free cleanly.
            karac_runtime_taskgroup_join_and_free(g);
        }
    }

    #[test]
    fn taskgroup_drains_registered_children_on_join_and_free() {
        // Spawn 16 tasks, register each with the group, join+free the
        // group, verify the shared counter reaches the expected sum.
        let counter = AtomicU32::new(0);
        unsafe {
            let group = karac_runtime_taskgroup_new();
            for _ in 0..16 {
                let h = karac_runtime_spawn(
                    inc_counter_wrapper,
                    &counter as *const AtomicU32 as *mut c_void,
                    0,
                    1,
                );
                assert!(!h.is_null());
                karac_runtime_taskgroup_register(group, h);
            }
            // Drop equivalent: join all + free group.
            karac_runtime_taskgroup_join_and_free(group);
            // All 16 children must have completed before drop returned.
            assert_eq!(counter.load(AtomicOrdering::Relaxed), 16);
        }
    }

    // ── B-2026-06-17-2 — eager-reap of detached children ─────────────

    #[test]
    fn taskgroup_register_reaps_detached_completed_children() {
        // Regression for B-2026-06-17-2: the canonical server shape
        // `loop { tg.spawn(|| handle(conn)) }` creates the group once and never
        // exits its scope, so without the register-time sweep every completed
        // child's handle is retained in `children` forever — ~100 B/conn,
        // unbounded. Drive the sweep directly: register a detached child, wait
        // for it to complete, then register the next; the `children` Vec must
        // stay bounded (the previous, now-terminal child is reaped on each
        // register) rather than growing with the iteration count.
        const ITERS: usize = 200;
        let counter = AtomicU32::new(0);
        unsafe {
            let group = karac_runtime_taskgroup_new();
            let mut max_children = 0usize;
            for _ in 0..ITERS {
                let h = karac_runtime_spawn(
                    inc_counter_wrapper,
                    &counter as *const AtomicU32 as *mut c_void,
                    0,
                    1,
                );
                assert!(!h.is_null());
                // Match codegen's discard lowering order: register, then detach.
                karac_runtime_taskgroup_register(group, h);
                karac_runtime_task_detach(h);

                // Wait for this child to reach a terminal state so the *next*
                // register's sweep is guaranteed to reap it.
                let deadline = Instant::now() + Duration::from_secs(5);
                while karac_runtime_task_state(h) == TASK_STATE_PENDING {
                    if Instant::now() > deadline {
                        panic!("detached child did not complete within budget");
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }

                let len = {
                    let g = &*group;
                    let children = g.children.lock().unwrap_or_else(|p| p.into_inner());
                    children.len()
                };
                max_children = max_children.max(len);
            }
            // With the sweep, `children` never holds more than the freshly
            // registered child plus at most one not-yet-swept sibling — far
            // below ITERS. Pre-fix this grows to ITERS (the assertion fails).
            assert!(
                max_children <= 4,
                "children Vec grew to {max_children} (expected bounded ≤ 4) — \
                 the detached eager-reap sweep is not bounding retention",
            );
            karac_runtime_taskgroup_join_and_free(group);
            assert_eq!(counter.load(AtomicOrdering::Relaxed), ITERS as u32);
        }
    }

    #[test]
    fn detached_free_spawn_self_reaps_after_completion() {
        // A discarded free `spawn(...)` (no group, non-coro) must self-reap its
        // handle on completion — there is no joiner to free it. We can't assert
        // the free directly without ASAN/LSan (that's the memory_sanitizer E2E),
        // but we can pin the handshake's observable behavior: detach AFTER the
        // task has already completed must still reclaim (and not deadlock /
        // double-panic). Run both orderings.
        let counter = AtomicU32::new(0);
        unsafe {
            // Ordering A: detach while the task may still be in flight (the
            // worker's completion block performs the reap).
            for _ in 0..32 {
                let h = karac_runtime_spawn(
                    inc_counter_wrapper,
                    &counter as *const AtomicU32 as *mut c_void,
                    0,
                    1,
                );
                karac_runtime_task_detach(h);
            }
            // Ordering B: wait for terminal, THEN detach (the detach path
            // performs the reap).
            for _ in 0..32 {
                let h = karac_runtime_spawn(
                    inc_counter_wrapper,
                    &counter as *const AtomicU32 as *mut c_void,
                    0,
                    1,
                );
                let deadline = Instant::now() + Duration::from_secs(5);
                while karac_runtime_task_state(h) == TASK_STATE_PENDING {
                    if Instant::now() > deadline {
                        panic!("free-spawn task did not complete within budget");
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                karac_runtime_task_detach(h);
            }
            // Give ordering-A workers time to finish incrementing.
            let deadline = Instant::now() + Duration::from_secs(5);
            while counter.load(AtomicOrdering::Relaxed) < 64 {
                if Instant::now() > deadline {
                    panic!("not all detached free-spawn tasks ran");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(counter.load(AtomicOrdering::Relaxed), 64);
        }
    }

    #[test]
    fn taskgroup_join_and_free_handles_null_gracefully() {
        unsafe {
            // Null group is a no-op (defensive contract, matches the
            // `karac_runtime_taskgroup_register` null guard above).
            karac_runtime_taskgroup_join_and_free(std::ptr::null_mut());
        }
    }

    #[test]
    fn taskgroup_register_null_inputs_are_no_ops() {
        unsafe {
            let g = karac_runtime_taskgroup_new();
            // null child is silently skipped.
            karac_runtime_taskgroup_register(g, std::ptr::null_mut());
            // null group + null child are silently skipped.
            karac_runtime_taskgroup_register(std::ptr::null_mut(), std::ptr::null_mut());
            karac_runtime_taskgroup_join_and_free(g);
        }
    }

    #[test]
    fn taskgroup_cancel_flips_all_child_cancel_flags() {
        // A2 slice 5b-1: `karac_runtime_taskgroup_cancel` must set the
        // per-task cancel flag on every registered child. Inert at this slice
        // (no dispatcher reads it yet), so we assert the flag state directly.
        let counter = AtomicU32::new(0);
        unsafe {
            let group = karac_runtime_taskgroup_new();
            let mut handles = Vec::new();
            for _ in 0..8 {
                let h = karac_runtime_spawn(
                    inc_counter_wrapper,
                    &counter as *const AtomicU32 as *mut c_void,
                    0,
                    1,
                );
                assert!(!h.is_null());
                // Flags start cleared.
                assert!(!(*h).cancel.load(AtomicOrdering::Acquire));
                handles.push(h);
                karac_runtime_taskgroup_register(group, h);
            }

            karac_runtime_taskgroup_cancel(group);

            // Every registered child now carries a set cancel flag. The flip is
            // synchronous under the children lock, independent of whether the
            // child task has completed — handles stay live until group drop.
            for &h in &handles {
                assert!(
                    (*h).cancel.load(AtomicOrdering::Acquire),
                    "child cancel flag must be set after taskgroup_cancel"
                );
            }

            // Children complete regardless of the inert flag; drop drains+frees.
            karac_runtime_taskgroup_join_and_free(group);
        }
    }

    #[test]
    fn taskgroup_cancel_null_and_empty_are_no_ops() {
        unsafe {
            // Null group: defensive no-op (matches register/join guards).
            karac_runtime_taskgroup_cancel(std::ptr::null_mut());
            // Empty group: no children to flip, must not crash, still drains.
            let g = karac_runtime_taskgroup_new();
            karac_runtime_taskgroup_cancel(g);
            karac_runtime_taskgroup_join_and_free(g);
        }
    }

    #[test]
    fn taskgroup_with_i64_returning_children_drains_correctly() {
        // Mix of i64-returning children; results are discarded by
        // taskgroup_join_and_free (slice 5 contract — wait for children,
        // don't collect results). Verify the children all reached
        // terminal state by joining the group then trying to read state
        // would be impossible (handles freed); instead, verify the
        // group's drop returned (test-thread continues) — implicit.
        let env_val: i64 = 100;
        unsafe {
            let group = karac_runtime_taskgroup_new();
            for _ in 0..4 {
                let h = karac_runtime_spawn(
                    add_one_wrapper,
                    &env_val as *const i64 as *mut c_void,
                    std::mem::size_of::<i64>(),
                    std::mem::align_of::<i64>(),
                );
                karac_runtime_taskgroup_register(group, h);
            }
            // No timeout — drop must return in bounded time. If the
            // join hangs, the test framework's per-test timeout
            // (defaults to 60s in cargo test) will catch it.
            karac_runtime_taskgroup_join_and_free(group);
        }
    }

    // B-2026-06-09-1 — a group-registered child that the user ALSO
    // explicitly `.join()`s must be freed exactly once: the explicit join
    // transports the result without freeing (the group still owns it),
    // and the group's scope-exit drop reaps it. Before the fix both the
    // explicit join and the group join called `free_handle` on the same
    // pointer → use-after-free → SIGSEGV (the codegen-level repro).
    #[test]
    fn registered_child_explicit_join_then_group_drop_frees_once() {
        let env_val: i64 = 41;
        unsafe {
            let group = karac_runtime_taskgroup_new();
            let child = karac_runtime_spawn(
                add_one_wrapper,
                &env_val as *const i64 as *mut c_void,
                std::mem::size_of::<i64>(),
                std::mem::align_of::<i64>(),
            );
            karac_runtime_taskgroup_register(group, child);
            // Explicit join: reads the result, must leave the handle alive.
            let mut out: i64 = 0;
            let status = karac_runtime_task_join(child, &mut out as *mut i64 as *mut u8);
            assert_eq!(status, TASK_STATE_COMPLETED);
            assert_eq!(out, 42);
            // Group drop is the sole freer — no double-free / UAF.
            karac_runtime_taskgroup_join_and_free(group);
        }
    }

    // ── A2 slice 5a — non-blocking coroutine spawn ──────────────────────

    /// Fake coroutine ramp for the slice-5a tests. Models the production
    /// ramp's observable contract: it returns ~immediately (the worker is
    /// freed) and arranges for the bound slot to be signalled LATER from a
    /// separate thread — exactly as the real dispatcher drives a parked
    /// coroutine to completion (whose body then `park_slot_signal`s the
    /// slot). `env` is a `*const RampProbe`: the ramp records that it ran +
    /// returned, then detaches a thread that sleeps `delay_ms` and signals.
    struct RampProbe {
        ran: AtomicBool,
        delay_ms: u64,
    }

    unsafe extern "C" fn delayed_signal_ramp(
        env: *mut c_void,
        slot: *mut crate::event_loop::KaracParkSlot,
        _cancel: *const AtomicBool,
    ) {
        let probe = &*(env as *const RampProbe);
        let delay = probe.delay_ms;
        let slot_addr = slot as usize;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay));
            // SAFETY: the test keeps the handle (and thus the slot) alive
            // until join returns; the signal happens-before the join wakes.
            unsafe {
                crate::event_loop::karac_runtime_park_slot_signal(
                    slot_addr as *mut crate::event_loop::KaracParkSlot,
                );
            }
        });
        // The ramp itself returns at once — the worker thread is now free.
        probe.ran.store(true, AtomicOrdering::Release);
    }

    /// The join of a non-blocking coroutine-spawn handle waits on the bound
    /// slot (the dispatcher's completion signal), NOT on the worker that
    /// ramped it — the worker returns ~immediately, while the coroutine is
    /// still "parked". A `PENDING` state at slot-fire time reads back as
    /// COMPLETED. This is the slice-5a density binding.
    #[test]
    fn spawn_coro_join_waits_on_completion_slot_not_worker() {
        let probe = Box::into_raw(Box::new(RampProbe {
            ran: AtomicBool::new(false),
            delay_ms: 60,
        }));
        unsafe {
            let handle = karac_runtime_spawn_coro(delayed_signal_ramp, probe as *mut c_void);
            assert!(!handle.is_null(), "spawn_coro must return a handle");

            let start = Instant::now();
            let status = karac_runtime_task_join(handle, std::ptr::null_mut());
            let elapsed = start.elapsed();

            assert_eq!(
                status, TASK_STATE_COMPLETED,
                "a normally-completing coroutine join must report COMPLETED"
            );
            assert!(
                (*probe).ran.load(AtomicOrdering::Acquire),
                "the ramp must have run on a worker"
            );
            // The join actually blocked on the slot for ~delay_ms — proof it
            // waited on the dispatcher's completion signal, not the (already
            // returned) worker. Lower bound is loose to absorb scheduling.
            assert!(
                elapsed >= Duration::from_millis(40),
                "join returned in {elapsed:?}, too fast — it did not wait on the \
                 completion slot (the density binding is broken)"
            );

            drop(Box::from_raw(probe));
        }
    }

    /// Many non-blocking coroutine spawns ramp concurrently and all of their
    /// joins resolve — the workers are freed after each ramp (returns fast),
    /// so a batch far larger than the worker count does not serialize or
    /// deadlock. Under the old thread-blocking drive each handler would pin
    /// a worker for the whole `delay_ms`; here the workers churn through all
    /// the ramps immediately and the dispatcher-signal threads complete them.
    #[test]
    fn spawn_coro_batch_larger_than_workers_all_complete() {
        const N: usize = 64;
        let probes: Vec<*mut RampProbe> = (0..N)
            .map(|_| {
                Box::into_raw(Box::new(RampProbe {
                    ran: AtomicBool::new(false),
                    delay_ms: 40,
                }))
            })
            .collect();
        unsafe {
            let handles: Vec<*mut KaracTaskHandle> = probes
                .iter()
                .map(|&p| karac_runtime_spawn_coro(delayed_signal_ramp, p as *mut c_void))
                .collect();
            // Join all — every coroutine's slot must fire.
            for h in handles {
                let status = karac_runtime_task_join(h, std::ptr::null_mut());
                assert_eq!(status, TASK_STATE_COMPLETED);
            }
            for &p in &probes {
                assert!(
                    (*p).ran.load(AtomicOrdering::Acquire),
                    "every ramp must have run"
                );
                drop(Box::from_raw(p));
            }
        }
    }

    // ── G13 — task-scheduling overhead microbenchmark ──────────────
    //
    // Phase 6 checklist "G13 — Work-stealing overhead on fine-grained
    // tasks". Measures the raw scheduling cost of the spawn/join and
    // par_run primitives at the runtime layer (no codegen / Kāra source
    // in the loop), and locates the task-granularity crossover where
    // parallel dispatch beats running the branches in-thread. Validates
    // the pinned cost-model constants (`DISPATCH_OVERHEAD_PER_CALL_UNITS`
    // = 10_000 ns in `src/codegen/reduce.rs`, mirrored at runtime) and
    // the <1µs spawn+join target in the checklist against measured
    // reality.
    //
    //   cargo test -p karac-runtime --release bench_g13_scheduling_overhead \
    //       -- --ignored --nocapture
    //
    // `--release` is mandatory: the busy-work kernel and dispatch path
    // are meaningless under a debug build.

    use crate::{karac_par_run, KaracBranch};

    /// Opaque-to-the-optimizer integer busy-loop. `#[inline(never)]` so
    /// the work can't be folded into the caller, `black_box` so the
    /// accumulator can't be dropped. Returns the accumulator to keep the
    /// loop live. ~1 cheap arithmetic op per iteration.
    #[inline(never)]
    fn busy_work(iters: u64) -> u64 {
        let mut acc: u64 = 0;
        let mut i: u64 = 0;
        while i < iters {
            acc = acc.wrapping_add(std::hint::black_box(i).wrapping_mul(2_654_435_761));
            i += 1;
        }
        std::hint::black_box(acc)
    }

    /// Spawn-side wrapper: `env` points to a `u64` iteration count; runs
    /// `busy_work` and writes the result so the result-transport path is
    /// exercised exactly as a real `-> u64` task would be.
    unsafe extern "C" fn busy_spawn(
        env: *mut c_void,
        result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        let iters = *(env as *const u64);
        let v = busy_work(iters);
        if !result_out.is_null() {
            *(result_out as *mut u64) = v;
        }
    }

    /// par_run-side branch wrapper: `ctx` points to a `u64` iteration
    /// count; runs `busy_work` and discards the result (par branches in
    /// the write-only fan-out shape don't return a value).
    unsafe extern "C" fn busy_branch(ctx: *mut c_void, _cancel: *const AtomicBool) {
        let iters = *(ctx as *const u64);
        let _ = busy_work(iters);
    }

    /// Median of `f` over `iters` measured runs after `warmup` discarded
    /// runs. Returns nanoseconds. Median (not mean) so a scheduler hiccup
    /// or a stray context switch doesn't skew the figure.
    fn median_ns(warmup: usize, iters: usize, mut f: impl FnMut() -> Duration) -> f64 {
        for _ in 0..warmup {
            let _ = f();
        }
        let mut v: Vec<u128> = (0..iters).map(|_| f().as_nanos()).collect();
        v.sort_unstable();
        v[v.len() / 2] as f64
    }

    /// Calibrate `busy_work`: nanoseconds per iteration on this machine,
    /// so the crossover sweep can target real wall-clock durations.
    fn ns_per_busy_iter() -> f64 {
        const CAL_ITERS: u64 = 50_000_000;
        let ns = median_ns(2, 5, || {
            let start = Instant::now();
            let _ = busy_work(CAL_ITERS);
            start.elapsed()
        });
        ns / CAL_ITERS as f64
    }

    /// Iteration count for a target busy-work duration in nanoseconds.
    fn iters_for_ns(target_ns: f64, ns_per_iter: f64) -> u64 {
        (target_ns / ns_per_iter).max(0.0) as u64
    }

    #[test]
    #[ignore = "perf benchmark; run with --release --ignored --nocapture"]
    fn bench_g13_scheduling_overhead() {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0);
        let workers = crate::resolve_pool_workers();
        println!("\n=== G13: task-scheduling overhead ===");
        println!("  cores(available_parallelism) = {cores}; pool workers = {workers}");

        let ns_iter = ns_per_busy_iter();
        println!("  busy_work calibration: {ns_iter:.4} ns/iter");

        // ── 1. spawn+join round-trip latency (one task in flight) ──
        // Spawn a trivial task, immediately block on its join, repeat.
        // No overlap — this is the full one-task lifecycle latency the
        // checklist's "<1µs for spawn + join" target refers to:
        // heap alloc of the handle + result buffer, ParCall + Task build,
        // queue push under the pool mutex, one notify_all, the worker's
        // wake + run + terminal store + notify, and our condvar wake.
        {
            let zero: u64 = 0;
            const N: u64 = 200_000;
            let per_ns = median_ns(1, 5, || {
                let start = Instant::now();
                for _ in 0..N {
                    unsafe {
                        let h = karac_runtime_spawn(
                            busy_spawn,
                            &zero as *const u64 as *mut c_void,
                            std::mem::size_of::<u64>(),
                            std::mem::align_of::<u64>(),
                        );
                        let mut out: u64 = 0;
                        let _ = karac_runtime_task_join(h, &mut out as *mut u64 as *mut u8);
                    }
                }
                start.elapsed()
            }) / N as f64;
            println!("\n── 1. spawn+join round-trip (1 in flight) ──");
            println!(
                "  {:.3} µs/task  ({:.0} ns)  [target: <1µs]",
                per_ns / 1000.0,
                per_ns
            );
        }

        // ── 2. spawn throughput, pipelined (B in flight, then join) ──
        // Amortized per-task cost when many tasks are outstanding — the
        // realistic shape for a TaskGroup fan-out. Spawn B handles, then
        // join all B; the workers run them concurrently while we collect.
        {
            let zero: u64 = 0;
            const B: usize = 4_096;
            let per_ns = median_ns(1, 5, || {
                let mut handles: Vec<*mut KaracTaskHandle> = Vec::with_capacity(B);
                let start = Instant::now();
                unsafe {
                    for _ in 0..B {
                        handles.push(karac_runtime_spawn(
                            busy_spawn,
                            &zero as *const u64 as *mut c_void,
                            std::mem::size_of::<u64>(),
                            std::mem::align_of::<u64>(),
                        ));
                    }
                    let mut out: u64 = 0;
                    for h in handles {
                        let _ = karac_runtime_task_join(h, &mut out as *mut u64 as *mut u8);
                    }
                }
                start.elapsed()
            }) / B as f64;
            println!("\n── 2. spawn+join pipelined ({B} in flight) ──");
            println!("  {:.3} µs/task amortized", per_ns / 1000.0);
        }

        // ── 3. par_run dispatch overhead (N trivial branches) ──
        // One karac_par_run call over N zero-work branches, repeated.
        // Per-call cost = build N Tasks + push under the mutex + one
        // notify_all + the work-helping join loop draining N branches +
        // N decrement/signals. This is the number the codegen
        // PAR_RUN_DISPATCH_THRESHOLD / runtime DISPATCH_OVERHEAD_PER_CALL
        // constants are meant to model.
        {
            println!("\n── 3. par_run dispatch overhead (trivial branches) ──");
            let zero: u64 = 0;
            for &n in &[2usize, 4, 8, 16] {
                let branches: Vec<KaracBranch> = (0..n)
                    .map(|_| KaracBranch {
                        func: busy_branch,
                        ctx: &zero as *const u64 as *mut c_void,
                    })
                    .collect();
                const CALLS: u64 = 50_000;
                let per_ns = median_ns(1, 5, || {
                    let start = Instant::now();
                    for _ in 0..CALLS {
                        unsafe {
                            karac_par_run(branches.as_ptr(), n as u64, 0, std::ptr::null());
                        }
                    }
                    start.elapsed()
                }) / CALLS as f64;
                println!(
                    "  N={n:2}  {:.3} µs/call  ({:.3} µs/branch)",
                    per_ns / 1000.0,
                    per_ns / 1000.0 / n as f64
                );
            }
        }

        // ── 4. granularity crossover ──
        // For N=4 branches each doing W ns of work, compare par_run
        // dispatch against running the four branch fns in-thread (the
        // sequential fallback the compiler emits below threshold). The
        // smallest W where parallel total < sequential total is the
        // empirical minimum task duration above which parallelizing
        // pays — i.e. the value the compiler's threshold must clear.
        {
            println!("\n── 4. granularity crossover (N=4 disjoint branches) ──");
            println!("  per-branch W   sequential   parallel   speedup   verdict");
            const N: usize = 4;
            let targets_us = [0.5f64, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0];
            let mut crossover_us: Option<f64> = None;
            for &w_us in &targets_us {
                let iters = iters_for_ns(w_us * 1000.0, ns_iter);
                let ctxs: Vec<u64> = vec![iters; N];
                let branches: Vec<KaracBranch> = (0..N)
                    .map(|i| KaracBranch {
                        func: busy_branch,
                        ctx: &ctxs[i] as *const u64 as *mut c_void,
                    })
                    .collect();
                // Sequential: run all N branch fns in this thread.
                let seq_ns = median_ns(1, 9, || {
                    let start = Instant::now();
                    unsafe {
                        let no_cancel = AtomicBool::new(false);
                        for b in &branches {
                            (b.func)(b.ctx, &no_cancel as *const AtomicBool);
                        }
                    }
                    start.elapsed()
                });
                // Parallel: one par_run over the N branches.
                let par_ns = median_ns(1, 9, || {
                    let start = Instant::now();
                    unsafe {
                        karac_par_run(branches.as_ptr(), N as u64, 0, std::ptr::null());
                    }
                    start.elapsed()
                });
                let speedup = seq_ns / par_ns;
                let wins = par_ns < seq_ns;
                if wins && crossover_us.is_none() {
                    crossover_us = Some(w_us);
                }
                println!(
                    "  {:7.1} µs    {:8.2} µs   {:8.2} µs   {:5.2}×    {}",
                    w_us,
                    seq_ns / 1000.0,
                    par_ns / 1000.0,
                    speedup,
                    if wins {
                        "parallel wins"
                    } else {
                        "sequential wins"
                    },
                );
            }
            match crossover_us {
                Some(w) => println!(
                    "\n  crossover: parallel first wins at per-branch W ≈ {w} µs \
                     (total group work ≈ {:.0} µs across N={N})",
                    w * N as f64
                ),
                None => println!(
                    "\n  crossover: parallel never won in the swept range \
                     (≤100 µs/branch) — dispatch overhead dominates"
                ),
            }
        }

        println!("\n── interpretation ──");
        println!("  The codegen gate models dispatch at 10_000 units (10µs) and");
        println!("  parallelizes a par_run group only above PAR_RUN_DISPATCH_");
        println!("  THRESHOLD_UNITS=500 (≈ measurement 3 per-call) with every");
        println!("  branch above 50 units; reduce loops gate at workers × 10µs.");
        println!("  Compare the crossover in measurement 4 to those constants.");
    }
}
