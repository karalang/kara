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
    let h = &*handle;

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

    // Drop the result buffer + reclaim the handle.
    // Re-take ownership of the Box and let it drop the handle struct;
    // the result buffer is freed manually since it was alloc'd via
    // `std::alloc::alloc`, not boxed.
    drop(_final_guard);
    free_handle(handle);

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
    // `boxed` drops here, releasing the Mutex / Condvar / AtomicU8 / etc.
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
}
