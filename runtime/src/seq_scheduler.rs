//! Sequential task scheduler — the WASM-default lowering of `spawn()` /
//! `TaskGroup` (phase-10 "WASM concurrency lowering — sequential default").
//!
//! On `wasm_browser` / `wasm_wasi` without threads there is no worker pool:
//! the target is single-threaded by construction (no `std::thread::spawn`,
//! no atomics-backed SharedArrayBuffer until the `--features wasm-threads`
//! opt-in entry lands). This module provides the same `karac_runtime_*`
//! extern surface `scheduler.rs` exports on native, implemented as a
//! **cooperative sequential scheduler** on the main thread:
//!
//! - `spawn(closure)` **enqueues** the closure on a FIFO ready queue and
//!   returns its handle immediately — it does not run the closure inline.
//! - Join points **drive** the queue: `karac_runtime_task_join`,
//!   `karac_runtime_taskgroup_join_and_free`, and (defensively)
//!   `karac_runtime_task_handle_free` pop and run ready tasks to
//!   completion until the joined handle reaches a terminal state.
//! - Tasks are run-to-completion units at v1: with no suspension points
//!   there is nothing to interleave *within* a task. The "yield to the
//!   host event loop on channel-receive blocks" contract is the separate
//!   phase-10 "Scheduler — yield-to-event-loop on WASM channel-receive"
//!   entry; [`drive_until_terminal`] is where that yield lands (the spot
//!   where the queue runs dry while a join is still pending).
//!
//! Source-level semantics match the native lowering modulo scheduling:
//! every spawned closure runs exactly once, results transport through the
//! same caller-side out-slot memcpy, `TaskGroup` joins every registered
//! child before its drop returns, and the cooperative cancel flag is
//! observable by closure bodies through the same `*const AtomicBool` ABI.
//! Execution order is deterministic (spawn order) — a legal schedule of
//! the native pool's racy one.
//!
//! ## What is deliberately absent
//!
//! - **`karac_runtime_spawn_coro`** — the density-optimal non-blocking
//!   coroutine spawn is event-loop substrate (`event_loop.rs`, net-gated);
//!   its users are network handlers, which aren't wasm-buildable (the
//!   whole native net surface is compiled out of the wasm archive). A
//!   program that somehow reaches it fails at wasm link naming the
//!   symbol — same posture as every other net-only runtime export.
//! - **Panic recovery** — the release archive builds `panic = "abort"`
//!   (workspace profile), so a panicking task aborts the module. That
//!   matches the native release posture, where `catch_unwind` never gets
//!   a chance to store `TASK_STATE_PANICKED` either.
//!
//! ## cfg shape
//!
//! The module compiles on sequential wasm (the real consumer) **and**
//! under `cfg(test)` on native so the queue/join/group logic is
//! unit-testable without a wasm host; only the `#[no_mangle]` exports
//! are wasm-gated (additionally on `not(feature = "net")` and
//! `not(feature = "wasm-threads")` so exactly one scheduler exports the
//! `karac_runtime_*` task surface per archive — `scheduler.rs` under
//! `net`, `wasm_threads_scheduler.rs` under `wasm-threads`, this module
//! otherwise on wasm).

use std::alloc::Layout;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::AtomicBool;

/// Task lifecycle discriminants — pinned to the same values as
/// `scheduler.rs`'s `TASK_STATE_*` so codegen-side reads of the join
/// return code are identical across the native and wasm archives.
pub const TASK_STATE_PENDING: u8 = 0;
pub const TASK_STATE_COMPLETED: u8 = 1;
#[allow(dead_code)] // pinned ABI value; unreachable under panic=abort
pub const TASK_STATE_PANICKED: u8 = 2;
pub const TASK_STATE_CANCELLED: u8 = 3;

/// Spawn-side wrapper closure signature — same shape as
/// `scheduler.rs::SpawnFn` (codegen emits one wrapper per `spawn` site;
/// the ABI is shared between the native and sequential schedulers).
pub type SpawnFn =
    unsafe extern "C" fn(env: *mut c_void, result_out: *mut u8, cancel: *const AtomicBool);

/// Sequential-scheduler handle for an enqueued or completed `spawn()`
/// task. Address (cast to `i64`) is the Kāra-side `TaskHandle[T].task_id`,
/// exactly like the native `KaracTaskHandle` — the layout is runtime-
/// private, so the two schedulers can differ freely behind the pointer.
///
/// Single-threaded by construction: `Cell` for state, no Condvar. The
/// `cancel` flag stays an `AtomicBool` because the codegen-emitted
/// closure wrapper observes it through `*const AtomicBool` (shared ABI
/// with the native scheduler); uncontended atomics on wasm32 are plain
/// loads/stores.
pub struct SeqTaskHandle {
    state: Cell<u8>,
    result_buf: *mut u8,
    result_layout: Layout,
    cancel: AtomicBool,
    /// B-2026-06-09-1 — set true by `seq_taskgroup_register`. A
    /// group-registered handle is freed exactly once, by the group's
    /// scope-exit `seq_taskgroup_join_and_free`; an explicit `.join()`
    /// on it drives + copies the result but does NOT free (the group is
    /// the sole freer). `Cell` not `AtomicBool` — this scheduler is
    /// single-threaded. Mirror of `scheduler.rs`'s `registered` field.
    registered: Cell<bool>,
}

/// One enqueued task: the closure wrapper + its captured environment,
/// tied to the handle the join will consume.
struct SeqTask {
    handle: *mut SeqTaskHandle,
    fn_ptr: SpawnFn,
    env: *mut c_void,
}

thread_local! {
    /// The ready queue. FIFO — spawn order is execution order, the
    /// deterministic legal schedule of the native pool. `thread_local!`
    /// rather than a `static` because wasm32 has exactly one thread (so
    /// it costs nothing) and native unit tests get per-test isolation
    /// for free.
    static READY: RefCell<VecDeque<SeqTask>> = const { RefCell::new(VecDeque::new()) };
}

/// Enqueue a fresh task and return its handle. Mirrors
/// `scheduler.rs::karac_runtime_spawn`'s allocation contract: a
/// `result_size`-byte buffer at `result_align` (null when size is 0),
/// handle leaked via `Box::into_raw`, freed by the join/free paths.
///
/// # Safety
///
/// Same contract as the native `karac_runtime_spawn`: `fn_ptr` valid,
/// `env` outlives the task's execution (here: until a join point drives
/// it), `result_size`/`result_align` match the closure's return type.
pub(crate) unsafe fn seq_spawn(
    fn_ptr: SpawnFn,
    env: *mut c_void,
    result_size: usize,
    result_align: usize,
) -> *mut SeqTaskHandle {
    let (result_buf, result_layout) = if result_size == 0 {
        (std::ptr::null_mut(), Layout::from_size_align(0, 1).unwrap())
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

    let handle = Box::into_raw(Box::new(SeqTaskHandle {
        state: Cell::new(TASK_STATE_PENDING),
        result_buf,
        result_layout,
        cancel: AtomicBool::new(false),
        registered: Cell::new(false),
    }));

    READY.with(|q| {
        q.borrow_mut().push_back(SeqTask {
            handle,
            fn_ptr,
            env,
        })
    });

    handle
}

/// Run one dequeued task to completion and mark its handle terminal.
///
/// # Safety
///
/// `task.handle` must still be live (guaranteed: handles are freed only
/// by join/free, which never leave the task enqueued behind them).
unsafe fn run_one(task: SeqTask) {
    let h = &*task.handle;
    (task.fn_ptr)(task.env, h.result_buf, &h.cancel as *const AtomicBool);
    h.state.set(TASK_STATE_COMPLETED);
}

/// Drive the ready queue (FIFO) until `handle` reaches a terminal state.
/// No-op when it already has. This is the cooperative scheduling core —
/// a join on task C first runs earlier-spawned A and B to completion,
/// interleaving ready closures exactly as the phase-10 entry specifies.
///
/// The yield-to-event-loop slice (phase-10 "Scheduler — yield-to-event-
/// loop on WASM channel-receive") hooks in here: today a dry queue with
/// the join still pending is unsatisfiable (no host-async producer can
/// exist yet), so it traps loudly instead of spinning.
///
/// # Safety
///
/// `handle` must be live. Re-entrant via `run_one` (a task body may
/// itself spawn + join), which is sound because the queue borrow is
/// released before each `run_one` call.
unsafe fn drive_until_terminal(handle: *mut SeqTaskHandle) {
    while (*handle).state.get() == TASK_STATE_PENDING {
        let next = READY.with(|q| q.borrow_mut().pop_front());
        match next {
            Some(task) => run_one(task),
            None => {
                // A PENDING handle whose task is neither queued nor
                // completable: the only way here is a task joining its
                // own handle (self-join deadlocks on native too). Trap
                // with a diagnosable message rather than spinning.
                panic!(
                    "karac sequential scheduler: join on a task that is \
                     neither ready nor complete (self-join deadlock?)"
                );
            }
        }
    }
}

/// Sequential `karac_runtime_task_join`: drive until terminal, copy the
/// result into `out_slot`, free the handle, return the terminal state.
///
/// # Safety
///
/// Same contract as the native join: `handle` from [`seq_spawn`], not
/// yet joined/freed; `out_slot` writable at the spawn-time size/align or
/// null for unit-returning tasks.
pub(crate) unsafe fn seq_task_join(handle: *mut SeqTaskHandle, out_slot: *mut u8) -> u8 {
    if handle.is_null() {
        return TASK_STATE_CANCELLED;
    }
    // B-2026-06-09-1 — a group-registered handle is freed by the group's
    // scope-exit drop, not here; an explicit `.join()` drives + copies
    // the result but leaves the handle alive for the group to reclaim.
    let do_free = !(*handle).registered.get();
    seq_task_join_inner(handle, out_slot, do_free)
}

/// Drive `handle` to terminal, copy the result into `out_slot` (when
/// non-null and completed), and free iff `do_free`. The single
/// waiter+reaper shared by the explicit `.join()` path and the
/// `TaskGroup` join barrier; `drive_until_terminal` is a no-op on an
/// already-terminal handle, so calling this twice (explicit join with
/// `do_free = false`, then the group with `do_free = true`) is sound.
///
/// # Safety
///
/// `handle` must be live; when `do_free` is false the caller guarantees
/// a later `do_free = true` call reclaims it exactly once.
unsafe fn seq_task_join_inner(handle: *mut SeqTaskHandle, out_slot: *mut u8, do_free: bool) -> u8 {
    drive_until_terminal(handle);

    let h = &*handle;
    let terminal = h.state.get();
    if terminal == TASK_STATE_COMPLETED
        && !out_slot.is_null()
        && !h.result_buf.is_null()
        && h.result_layout.size() > 0
    {
        std::ptr::copy_nonoverlapping(h.result_buf, out_slot, h.result_layout.size());
    }
    if do_free {
        free_handle(handle);
    }
    terminal
}

/// Release a handle without transporting a result. A still-pending task
/// is driven to completion first — on native the pool runs every spawned
/// task eventually, so dropping its side effects here would be an
/// observable divergence, not a scheduling difference. (Codegen emits no
/// free-of-pending-handle today — every handle flows through `.join()`
/// or a group join — so the drive is defensive parity, not a hot path.)
///
/// # Safety
///
/// `handle` must be from [`seq_spawn`] and not already joined/freed.
pub(crate) unsafe fn seq_task_handle_free(handle: *mut SeqTaskHandle) {
    if handle.is_null() {
        return;
    }
    drive_until_terminal(handle);
    free_handle(handle);
}

/// Sequential `karac_runtime_task_detach` (B-2026-06-17-2 / -8). Codegen emits
/// this for a discarded `spawn` / `tg.spawn` handle (never joined), to reap it
/// when safe. In the single-threaded sequential model there is no worker race:
/// a `tg.spawn`-registered child is freed by the group's `join_and_free` (the
/// sole-freer invariant, B-2026-06-09-1), so detaching it is a no-op; a discarded
/// free `spawn` has no joiner and no group, so it is driven to completion (parity
/// with the native pool, which runs every spawned task) and freed here.
///
/// # Safety
///
/// `handle` must be from [`seq_spawn`] and not already joined/freed. Codegen
/// emits this at most once per handle (the handle is discarded), so the free is
/// claimed exactly once.
pub(crate) unsafe fn seq_task_detach(handle: *mut SeqTaskHandle) {
    if handle.is_null() {
        return;
    }
    if (*handle).registered.get() {
        return;
    }
    drive_until_terminal(handle);
    free_handle(handle);
}

/// Free a handle and its result buffer. Single free site shared by the
/// join + free paths (mirror of `scheduler.rs::free_handle`).
///
/// # Safety
///
/// `handle` must be live and never touched again after this returns.
unsafe fn free_handle(handle: *mut SeqTaskHandle) {
    let boxed = Box::from_raw(handle);
    if !boxed.result_buf.is_null() && boxed.result_layout.size() > 0 {
        std::alloc::dealloc(boxed.result_buf, boxed.result_layout);
    }
}

/// Non-joining state read — same return convention as the native
/// `karac_runtime_task_state`.
///
/// # Safety
///
/// `handle` must be live.
pub(crate) unsafe fn seq_task_state(handle: *const SeqTaskHandle) -> u8 {
    if handle.is_null() {
        return TASK_STATE_CANCELLED;
    }
    (*handle).state.get()
}

/// Sequential `TaskGroup` container. `RefCell` instead of the native
/// `Mutex` — single thread, and the register/join sites never overlap a
/// live borrow (children are drained out of the cell before any join
/// runs, same re-entrancy posture as the native drain-outside-the-lock).
pub struct SeqTaskGroupHandle {
    children: RefCell<Vec<*mut SeqTaskHandle>>,
}

/// Allocate a fresh group handle (Kāra-side `TaskGroup.id` is its
/// address cast to `i64`, as on native).
pub(crate) fn seq_taskgroup_new() -> *mut SeqTaskGroupHandle {
    Box::into_raw(Box::new(SeqTaskGroupHandle {
        children: RefCell::new(Vec::new()),
    }))
}

/// Register a spawned child with the group.
///
/// # Safety
///
/// `group` live and from [`seq_taskgroup_new`]; `child` live and from
/// [`seq_spawn`].
pub(crate) unsafe fn seq_taskgroup_register(
    group: *mut SeqTaskGroupHandle,
    child: *mut SeqTaskHandle,
) {
    if group.is_null() || child.is_null() {
        return;
    }
    // B-2026-06-09-1 — mark the child group-owned so an explicit
    // `.join()` waits without freeing; the group is the sole freer.
    (*child).registered.set(true);
    (*group).children.borrow_mut().push(child);
}

/// Drive every registered child to a terminal state (discarding
/// results — the design.md `TaskGroup` contract is "wait for children"),
/// then free the group. The structured-concurrency join barrier:
/// `@TaskGroup.drop` lowers here, so children complete before the
/// owning scope exits, sequentially in spawn order.
///
/// # Safety
///
/// `group` live, from [`seq_taskgroup_new`], not already freed.
pub(crate) unsafe fn seq_taskgroup_join_and_free(group: *mut SeqTaskGroupHandle) {
    if group.is_null() {
        return;
    }
    // Drain outside the borrow so a child that recursively spawns into
    // the same group can re-borrow `children` without panicking.
    let children: Vec<*mut SeqTaskHandle> = std::mem::take(&mut *(*group).children.borrow_mut());
    for child in children {
        // B-2026-06-09-1 — sole freer: call the inner with `do_free =
        // true` directly. The public `seq_task_join` would see
        // `registered == true` and skip the free → leak.
        let _status = seq_task_join_inner(child, std::ptr::null_mut(), true);
    }
    drop(Box::from_raw(group));
}

/// Flip every registered child's cooperative cancel flag. No sweep —
/// there is no event loop with parked coroutines on this target; queued
/// tasks still run (matching native, where the pool executes a
/// cancelled-flagged task and the closure body observes the flag).
///
/// # Safety
///
/// `group` live; registered children live (freed only at group drop,
/// which cannot overlap a `cancel()` call on the same live group).
pub(crate) unsafe fn seq_taskgroup_cancel(group: *mut SeqTaskGroupHandle) {
    if group.is_null() {
        return;
    }
    for &child in (*group).children.borrow().iter() {
        if !child.is_null() {
            (*child)
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

// ── extern surface (sequential wasm archive only) ──────────────────────────
//
// Same symbol names + ABI as `scheduler.rs`'s native exports and
// `wasm_threads_scheduler.rs`'s threaded ones; the `not(feature = "net")`
// + `not(feature = "wasm-threads")` legs guarantee exactly one scheduler
// exports them per archive.

#[cfg(all(
    target_family = "wasm",
    not(feature = "net"),
    not(feature = "wasm-threads")
))]
mod exports {
    use super::*;

    /// Sequential `spawn()` — see [`seq_spawn`].
    ///
    /// `result_size` / `result_align` are `u64`, not `usize`: codegen
    /// declares them `i64` (its host `usize` width) for every target,
    /// and wasm32 traps signature mismatches at the call — i32-width
    /// `usize` params here would land on a `signature_mismatch` stub
    /// (the `karac_par_run` / `__karac_malloc64` size_t class). The
    /// native `scheduler.rs` export is unaffected: 64-bit `usize`
    /// already matches the declaration there.
    ///
    /// # Safety
    ///
    /// See [`seq_spawn`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_spawn(
        fn_ptr: SpawnFn,
        env: *mut c_void,
        result_size: u64,
        result_align: u64,
    ) -> *mut SeqTaskHandle {
        seq_spawn(fn_ptr, env, result_size as usize, result_align as usize)
    }

    /// Sequential join — see [`seq_task_join`].
    ///
    /// # Safety
    ///
    /// See [`seq_task_join`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_task_join(
        handle: *mut SeqTaskHandle,
        out_slot: *mut u8,
    ) -> u8 {
        seq_task_join(handle, out_slot)
    }

    /// Sequential drop-without-join — see [`seq_task_handle_free`].
    ///
    /// # Safety
    ///
    /// See [`seq_task_handle_free`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_task_handle_free(handle: *mut SeqTaskHandle) {
        seq_task_handle_free(handle)
    }

    /// Detach a discarded spawn handle — see [`seq_task_detach`]. Codegen emits
    /// this for every discarded `spawn` / `tg.spawn` handle (B-2026-06-17-2);
    /// without it the sequential-wasm archive fails to link (B-2026-06-17-8).
    ///
    /// # Safety
    ///
    /// See [`seq_task_detach`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_task_detach(handle: *mut SeqTaskHandle) {
        seq_task_detach(handle)
    }

    /// Non-joining state read — see [`seq_task_state`].
    ///
    /// # Safety
    ///
    /// See [`seq_task_state`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_task_state(handle: *const SeqTaskHandle) -> u8 {
        seq_task_state(handle)
    }

    /// Sequential `TaskGroup` allocation — see [`seq_taskgroup_new`].
    #[no_mangle]
    pub extern "C" fn karac_runtime_taskgroup_new() -> *mut SeqTaskGroupHandle {
        seq_taskgroup_new()
    }

    /// Child registration — see [`seq_taskgroup_register`].
    ///
    /// # Safety
    ///
    /// See [`seq_taskgroup_register`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_taskgroup_register(
        group: *mut SeqTaskGroupHandle,
        child: *mut SeqTaskHandle,
    ) {
        seq_taskgroup_register(group, child)
    }

    /// Join barrier at group drop — see [`seq_taskgroup_join_and_free`].
    ///
    /// # Safety
    ///
    /// See [`seq_taskgroup_join_and_free`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_taskgroup_join_and_free(group: *mut SeqTaskGroupHandle) {
        seq_taskgroup_join_and_free(group)
    }

    /// Cooperative cancel — see [`seq_taskgroup_cancel`].
    ///
    /// # Safety
    ///
    /// See [`seq_taskgroup_cancel`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_taskgroup_cancel(group: *mut SeqTaskGroupHandle) {
        seq_taskgroup_cancel(group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    // i64-returning wrapper: writes (env_val + 1) into result_out.
    unsafe extern "C" fn add_one_wrapper(
        env: *mut c_void,
        result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        let val = *(env as *const i64);
        *(result_out as *mut i64) = val + 1;
    }

    // Unit wrapper: appends the env's id to a shared order log.
    // env points at (id: i64, log: *mut Vec<i64>).
    unsafe extern "C" fn log_order_wrapper(
        env: *mut c_void,
        _result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        let (id, log) = *(env as *const (i64, *mut Vec<i64>));
        (*log).push(id);
    }

    // Wrapper that records whether its cancel flag was set when it ran.
    // env points at (slot: *mut bool,).
    unsafe extern "C" fn observe_cancel_wrapper(
        env: *mut c_void,
        _result_out: *mut u8,
        cancel: *const AtomicBool,
    ) {
        let slot = *(env as *const *mut bool);
        *slot = (*cancel).load(Ordering::Relaxed);
    }

    #[test]
    fn spawn_is_deferred_and_join_transports_result() {
        unsafe {
            let env: i64 = 41;
            let h = seq_spawn(
                add_one_wrapper,
                &env as *const i64 as *mut c_void,
                std::mem::size_of::<i64>(),
                std::mem::align_of::<i64>(),
            );
            // Deferred: the task has not run at spawn time.
            assert_eq!(seq_task_state(h), TASK_STATE_PENDING);
            let mut out: i64 = 0;
            let status = seq_task_join(h, &mut out as *mut i64 as *mut u8);
            assert_eq!(status, TASK_STATE_COMPLETED);
            assert_eq!(out, 42);
        }
    }

    #[test]
    fn ready_queue_runs_fifo_in_spawn_order() {
        unsafe {
            let mut log: Vec<i64> = Vec::new();
            let log_ptr = &mut log as *mut Vec<i64>;
            let envs: Vec<(i64, *mut Vec<i64>)> = (1..=3).map(|i| (i, log_ptr)).collect();
            let handles: Vec<_> = envs
                .iter()
                .map(|e| {
                    seq_spawn(
                        log_order_wrapper,
                        e as *const (i64, *mut Vec<i64>) as *mut c_void,
                        0,
                        0,
                    )
                })
                .collect();
            // Joining the LAST handle drives the earlier ones first —
            // the cooperative interleave is FIFO over the ready queue.
            let status = seq_task_join(handles[2], std::ptr::null_mut());
            assert_eq!(status, TASK_STATE_COMPLETED);
            assert_eq!(log, vec![1, 2, 3]);
            // The first two handles are terminal now; join just reaps.
            assert_eq!(seq_task_state(handles[0]), TASK_STATE_COMPLETED);
            assert_eq!(seq_task_join(handles[0], std::ptr::null_mut()), 1);
            assert_eq!(seq_task_join(handles[1], std::ptr::null_mut()), 1);
        }
    }

    #[test]
    fn taskgroup_join_drives_every_registered_child() {
        unsafe {
            let mut log: Vec<i64> = Vec::new();
            let log_ptr = &mut log as *mut Vec<i64>;
            let envs: Vec<(i64, *mut Vec<i64>)> = (1..=5).map(|i| (i, log_ptr)).collect();
            let group = seq_taskgroup_new();
            for e in &envs {
                let child = seq_spawn(
                    log_order_wrapper,
                    e as *const (i64, *mut Vec<i64>) as *mut c_void,
                    0,
                    0,
                );
                seq_taskgroup_register(group, child);
            }
            assert!(log.is_empty(), "children must not run before the join");
            seq_taskgroup_join_and_free(group);
            assert_eq!(log, vec![1, 2, 3, 4, 5]);
        }
    }

    // B-2026-06-09-1 — a group-registered child that is ALSO explicitly
    // `.join()`ed must be freed exactly once: the explicit join reads the
    // result without freeing, and the group's scope-exit drop reaps it.
    // Before the fix this double-freed the handle (the explicit join and
    // the group join both called `free_handle`).
    #[test]
    fn registered_child_explicit_join_then_group_drop_frees_once() {
        unsafe {
            let env: i64 = 41;
            let group = seq_taskgroup_new();
            let child = seq_spawn(
                add_one_wrapper,
                &env as *const i64 as *mut c_void,
                std::mem::size_of::<i64>(),
                std::mem::align_of::<i64>(),
            );
            seq_taskgroup_register(group, child);
            // Explicit join transports the result and must NOT free the
            // handle (the group still owns it).
            let mut out: i64 = 0;
            let status = seq_task_join(child, &mut out as *mut i64 as *mut u8);
            assert_eq!(status, TASK_STATE_COMPLETED);
            assert_eq!(out, 42);
            // Group drop is now the sole freer — no double-free / UAF.
            seq_taskgroup_join_and_free(group);
        }
    }

    #[test]
    fn handle_free_drives_pending_task_to_completion() {
        unsafe {
            let mut log: Vec<i64> = Vec::new();
            let log_ptr = &mut log as *mut Vec<i64>;
            let env: (i64, *mut Vec<i64>) = (7, log_ptr);
            let h = seq_spawn(
                log_order_wrapper,
                &env as *const (i64, *mut Vec<i64>) as *mut c_void,
                0,
                0,
            );
            seq_task_handle_free(h);
            assert_eq!(log, vec![7], "free must not drop the task's effects");
        }
    }

    #[test]
    fn taskgroup_cancel_flags_visible_to_queued_children() {
        unsafe {
            let mut observed = false;
            let observed_ptr: *mut bool = &mut observed;
            let env: *mut bool = observed_ptr;
            let group = seq_taskgroup_new();
            let child = seq_spawn(
                observe_cancel_wrapper,
                &env as *const *mut bool as *mut c_void,
                0,
                0,
            );
            seq_taskgroup_register(group, child);
            seq_taskgroup_cancel(group);
            // The queued child still runs (cooperative cancel, as on
            // native) and observes the flipped flag.
            seq_taskgroup_join_and_free(group);
            assert!(observed, "child must observe the cancel flag when run");
        }
    }

    #[test]
    fn null_handles_are_tolerated() {
        unsafe {
            assert_eq!(
                seq_task_join(std::ptr::null_mut(), std::ptr::null_mut()),
                TASK_STATE_CANCELLED
            );
            assert_eq!(seq_task_state(std::ptr::null()), TASK_STATE_CANCELLED);
            seq_task_handle_free(std::ptr::null_mut());
            seq_taskgroup_register(std::ptr::null_mut(), std::ptr::null_mut());
            seq_taskgroup_join_and_free(std::ptr::null_mut());
            seq_taskgroup_cancel(std::ptr::null_mut());
            seq_task_detach(std::ptr::null_mut());
        }
    }

    // Unit wrapper: bumps the i64 the env points at.
    unsafe extern "C" fn bump_i64_wrapper(
        env: *mut c_void,
        _result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        *(env as *mut i64) += 1;
    }

    #[test]
    fn task_detach_free_spawn_runs_and_reaps() {
        // B-2026-06-17-8: a discarded free `spawn` is detached — the sequential
        // model drives it to completion (its side effect must run) and frees the
        // handle. No joiner exists, so detach is the sole reaper.
        unsafe {
            let mut n: i64 = 0;
            let h = seq_spawn(bump_i64_wrapper, &mut n as *mut i64 as *mut c_void, 0, 1);
            seq_task_detach(h);
            assert_eq!(n, 1, "detached free-spawn task must run to completion");
        }
    }

    #[test]
    fn task_detach_registered_is_noop_group_frees() {
        // A `tg.spawn`-registered child detached: detach is a no-op (the group is
        // the sole freer, B-2026-06-09-1), and `join_and_free` drives + frees it
        // exactly once — no double free.
        unsafe {
            let mut n: i64 = 0;
            let group = seq_taskgroup_new();
            let h = seq_spawn(bump_i64_wrapper, &mut n as *mut i64 as *mut c_void, 0, 1);
            seq_taskgroup_register(group, h);
            seq_task_detach(h);
            seq_taskgroup_join_and_free(group);
            assert_eq!(n, 1);
        }
    }
}
