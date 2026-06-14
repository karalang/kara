//! Threaded task scheduler — the `--features wasm-threads` lowering of
//! `spawn()` / `TaskGroup` (phase-10 "WASM concurrency lowering —
//! `--features wasm-threads` opt-in").
//!
//! On wasm32-wasip1-threads, `std::thread` is real: pthreads ride the
//! wasi-threads ABI (the module imports `wasi.thread-spawn`, which the
//! JS glue services by spawning a Web Worker that calls the module's
//! exported `wasi_thread_start`; wasi-libc sets up the worker's stack +
//! TLS there), and Mutex / Condvar / atomics are futex-backed
//! (`memory.atomic.wait32`/`notify` over the SharedArrayBuffer-backed
//! shared memory). So this module is `scheduler.rs` with the event-loop
//! surface removed: the same pool-backed spawn over `lib.rs`'s
//! `ParCall`/`Task`/`pool()` substrate (widened to compile under
//! `wasm-threads`), the same Condvar join with the same release/acquire
//! result-transport edge, the same TaskGroup drain-outside-the-lock
//! re-entrancy posture.
//!
//! Because the compiler emits a **dual artifact** (the sequential module
//! is always built alongside; the JS glue picks at load time by
//! SAB/cross-origin-isolation feature detection), this archive may
//! assume threads work — there is no zero-worker degradation path here.
//! SAB-unavailable fallback is the glue loading the sequential module
//! (whose archive carries `seq_scheduler.rs` instead).
//!
//! ## What is deliberately absent (vs `scheduler.rs`)
//!
//! - **`coro_slot` / `karac_runtime_spawn_coro`** — the non-blocking
//!   coroutine spawn is event-loop substrate (`event_loop.rs`,
//!   net-gated); its users are network handlers, which aren't
//!   wasm-buildable. A program reaching it fails at wasm link naming
//!   the symbol — the `seq_scheduler.rs` posture.
//! - **Cancel-sweep on `taskgroup_cancel`** — the sweep tears down
//!   coroutines parked on idle fds; no event loop, no parked
//!   coroutines. The cooperative flag flip is the whole contract here,
//!   matching what a flag-only native build would do.
//! - **`TASK_STATE_PANICKED` on the normal path** — the release archive
//!   builds `panic = "abort"`, so a panicking task aborts the module
//!   (`catch_unwind` is still in the worker closure for native-test
//!   parity, where unwinding exists).
//!
//! ## Main-thread caveat (why the glue proxies `_start`)
//!
//! `memory.atomic.wait32` traps on the browser's main thread (a
//! non-blockable agent), and every blocking primitive here — the join's
//! Condvar, a contended pool-queue Mutex — bottoms out in it. The JS
//! glue therefore runs the program's `_start` in a **primary worker**
//! (the PROXY_TO_PTHREAD model), so the "main thread" of the Kāra
//! program is itself a blockable worker. Nothing in this module needs
//! to know; it is recorded here because the assumption is load-bearing.
//!
//! ## cfg shape
//!
//! Module logic compiles under `all(target_family = "wasm", feature =
//! "wasm-threads")` (the real consumer) **and** under `cfg(test)` on
//! native — the implementation is plain std-thread code, so the
//! spawn/join/group semantics are unit-testable without a wasm host
//! (the `seq_scheduler.rs` pattern). Only the `#[no_mangle]` exports
//! are wasm+wasm-threads-gated, completing the one-exporter-per-archive
//! matrix: `scheduler.rs` under `net`, `seq_scheduler.rs` on wasm
//! without `wasm-threads`, this module on wasm with it.

use std::alloc::Layout;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::{pool, ParCall, Task};

/// Task lifecycle discriminants — pinned to the same values as
/// `scheduler.rs` / `seq_scheduler.rs` so codegen-side reads of the
/// join return code are identical across all three archives.
pub const TASK_STATE_PENDING: u8 = 0;
pub const TASK_STATE_COMPLETED: u8 = 1;
pub const TASK_STATE_PANICKED: u8 = 2;
pub const TASK_STATE_CANCELLED: u8 = 3;

/// Spawn-side wrapper closure signature — same shape as
/// `scheduler.rs::SpawnFn` (codegen emits one wrapper per `spawn` site;
/// the ABI is shared by all three schedulers).
pub type SpawnFn =
    unsafe extern "C" fn(env: *mut c_void, result_out: *mut u8, cancel: *const AtomicBool);

/// Threaded-scheduler handle for an in-flight or completed `spawn()`
/// task. Address (cast to `i64`) is the Kāra-side `TaskHandle[T].task_id`
/// — layout is runtime-private, so the three schedulers differ freely
/// behind the pointer. `scheduler.rs::KaracTaskHandle` minus `coro_slot`.
pub struct WasmTaskHandle {
    state: AtomicU8,
    result_buf: *mut u8,
    result_layout: Layout,
    cancel: AtomicBool,
    notify_mutex: Mutex<()>,
    notify_cv: Condvar,
    /// B-2026-06-09-1 — set true by `wt_taskgroup_register`. A
    /// group-registered handle is freed exactly once, by the group's
    /// scope-exit `wt_taskgroup_join_and_free`; an explicit `.join()` on
    /// it waits + copies the result but does NOT free (the group is the
    /// sole freer). Mirror of `scheduler.rs`'s `registered` field.
    registered: AtomicBool,
}

// SAFETY: same reasoning as `scheduler.rs::KaracTaskHandle` — the raw
// `result_buf` is exclusively written by the worker and exclusively read
// by the joiner, with the Condvar barrier providing the happens-before
// edge between the two. No concurrent access at any observation point.
unsafe impl Send for WasmTaskHandle {}
unsafe impl Sync for WasmTaskHandle {}

/// Pool-backed `spawn()`. Mirrors `scheduler.rs::karac_runtime_spawn`'s
/// allocation contract: a `result_size`-byte buffer at `result_align`
/// (null when size is 0), handle leaked via `Box::into_raw`, freed by
/// the join/free paths; the task rides the shared `pool()` queue as a
/// 1-task `ParCall` so spawn-tasks and par-tasks contend for the same
/// workers.
///
/// # Safety
///
/// Same contract as the native `karac_runtime_spawn`: `fn_ptr` valid;
/// `env` opaque and caller-lifetime-managed (the wrapper runs on a pool
/// worker, so captures must be cross-task-safe — enforced structurally
/// by the typechecker); `result_size`/`result_align` match the
/// closure's return type (`result_size == 0` is the unit convention).
pub(crate) unsafe fn wt_spawn(
    fn_ptr: SpawnFn,
    env: *mut c_void,
    result_size: usize,
    result_align: usize,
) -> *mut WasmTaskHandle {
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

    let handle_ptr: *mut WasmTaskHandle = Box::into_raw(Box::new(WasmTaskHandle {
        state: AtomicU8::new(TASK_STATE_PENDING),
        result_buf,
        result_layout,
        cancel: AtomicBool::new(false),
        notify_mutex: Mutex::new(()),
        notify_cv: Condvar::new(),
        registered: AtomicBool::new(false),
    }));

    // Per-task ParCall: 1 work unit, no frame tracking — the
    // `scheduler.rs` shape.
    let call = Arc::new(ParCall {
        cancel: AtomicBool::new(false),
        remaining: Mutex::new(1),
        notify: Condvar::new(),
        spawn_site_id: 0,
        parent_addr: 0,
        track_frames: false,
    });

    // Raw addresses as `usize` so the closure is `Send` without unsafe
    // impls on the FFI pointer types (the `karac_par_run` pattern).
    let env_addr = env as usize;
    let handle_addr = handle_ptr as usize;

    let task = Task {
        call,
        branch_idx: 0,
        run: Box::new(move |_pool_cancel: &AtomicBool| {
            // SAFETY: handle_ptr is the just-leaked Box; the worker is
            // the exclusive writer of handle.state until it signals.
            let handle = unsafe { &*(handle_addr as *const WasmTaskHandle) };
            let env_ptr = env_addr as *mut c_void;
            let cancel_ptr = &handle.cancel as *const AtomicBool;

            // `catch_unwind` is a no-op under the release archive's
            // `panic = "abort"`; it exists for native-test parity (and
            // for any future unwind-aware wasm profile).
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                fn_ptr(env_ptr, handle.result_buf, cancel_ptr);
            }))
            .is_err();

            // Mutex-held terminal-state write, then notify — the joiner's
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

/// Threaded `karac_runtime_task_join`: Condvar-wait until terminal, copy
/// the result into `out_slot` (release/acquire orders the memcpy after
/// the wrapper's result write), free the handle, return the terminal
/// state.
///
/// # Safety
///
/// Same contract as the native join: `handle` from [`wt_spawn`], not yet
/// joined/freed; `out_slot` writable at the spawn-time size/align or
/// null for unit-returning tasks.
pub(crate) unsafe fn wt_task_join(handle: *mut WasmTaskHandle, out_slot: *mut u8) -> u8 {
    if handle.is_null() {
        return TASK_STATE_CANCELLED;
    }
    // B-2026-06-09-1 — a group-registered handle is freed by the group's
    // scope-exit drop, not here; an explicit `.join()` waits + copies the
    // result but leaves the handle alive for the group to reclaim.
    let do_free = !(*handle).registered.load(Ordering::Acquire);
    wt_task_join_inner(handle, out_slot, do_free)
}

/// Condvar-wait `handle` to terminal, copy its result into `out_slot`
/// (when non-null and completed), and free iff `do_free`. The single
/// waiter+reaper shared by the explicit `.join()` path and the
/// `TaskGroup` join barrier; the `wait_while` returns immediately on an
/// already-terminal task, so calling this twice (explicit join with
/// `do_free = false`, then the group with `do_free = true`) is sound.
///
/// # Safety
///
/// `handle` must be live; when `do_free` is false the caller guarantees
/// a later `do_free = true` call reclaims it exactly once.
unsafe fn wt_task_join_inner(handle: *mut WasmTaskHandle, out_slot: *mut u8, do_free: bool) -> u8 {
    let h = &*handle;

    let guard = h.notify_mutex.lock().unwrap_or_else(|p| p.into_inner());
    let final_guard = h
        .notify_cv
        .wait_while(guard, |_| {
            h.state.load(Ordering::Acquire) == TASK_STATE_PENDING
        })
        .unwrap_or_else(|p| p.into_inner());

    let terminal = h.state.load(Ordering::Acquire);
    if terminal == TASK_STATE_COMPLETED
        && !out_slot.is_null()
        && !h.result_buf.is_null()
        && h.result_layout.size() > 0
    {
        std::ptr::copy_nonoverlapping(h.result_buf, out_slot, h.result_layout.size());
    }
    drop(final_guard);
    if do_free {
        free_handle(handle);
    }
    terminal
}

/// Release a handle without joining. Same contract (and same "task must
/// already be terminal" caveat) as the native
/// `karac_runtime_task_handle_free` — codegen emits no
/// free-of-in-flight-handle today; every handle flows through `.join()`
/// or a group join.
///
/// # Safety
///
/// `handle` from [`wt_spawn`], not already joined/freed, and its task
/// must have reached a terminal state (no worker still holds it).
pub(crate) unsafe fn wt_task_handle_free(handle: *mut WasmTaskHandle) {
    if handle.is_null() {
        return;
    }
    free_handle(handle);
}

/// Free a handle and its result buffer. Single free site shared by the
/// join + free paths (mirror of `scheduler.rs::free_handle`, minus the
/// coro-slot leg).
///
/// # Safety
///
/// `handle` must be live and never touched again after this returns.
unsafe fn free_handle(handle: *mut WasmTaskHandle) {
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
pub(crate) unsafe fn wt_task_state(handle: *const WasmTaskHandle) -> u8 {
    if handle.is_null() {
        return TASK_STATE_CANCELLED;
    }
    (*handle).state.load(Ordering::Acquire)
}

/// Threaded `TaskGroup` container — `scheduler.rs::KaracTaskGroupHandle`
/// verbatim (Mutex-protected children; real threads register/join
/// concurrently here, unlike the sequential scheduler's RefCell).
pub struct WasmTaskGroupHandle {
    children: Mutex<Vec<*mut WasmTaskHandle>>,
}

// SAFETY: see `scheduler.rs::KaracTaskGroupHandle` — registration and
// the join drain are ordered through the Mutex.
unsafe impl Send for WasmTaskGroupHandle {}
unsafe impl Sync for WasmTaskGroupHandle {}

/// Allocate a fresh group handle (Kāra-side `TaskGroup.id` is its
/// address cast to `i64`, as on native).
pub(crate) fn wt_taskgroup_new() -> *mut WasmTaskGroupHandle {
    Box::into_raw(Box::new(WasmTaskGroupHandle {
        children: Mutex::new(Vec::new()),
    }))
}

/// Register a spawned child with the group.
///
/// # Safety
///
/// `group` live and from [`wt_taskgroup_new`]; `child` live and from
/// [`wt_spawn`].
pub(crate) unsafe fn wt_taskgroup_register(
    group: *mut WasmTaskGroupHandle,
    child: *mut WasmTaskHandle,
) {
    if group.is_null() || child.is_null() {
        return;
    }
    // B-2026-06-09-1 — mark the child group-owned so an explicit
    // `.join()` waits without freeing; the group is the sole freer.
    (*child).registered.store(true, Ordering::Release);
    let g = &*group;
    let mut children = g.children.lock().unwrap_or_else(|p| p.into_inner());
    children.push(child);
}

/// Join every registered child (discarding results — the design.md
/// `TaskGroup` contract is "wait for children"), then free the group.
/// Children are drained **outside** the lock so a child closure that
/// recursively `tg.spawn(...)`s into the same group can't deadlock on a
/// re-entrant acquire — the `scheduler.rs` posture.
///
/// # Safety
///
/// `group` live, from [`wt_taskgroup_new`], not already freed.
pub(crate) unsafe fn wt_taskgroup_join_and_free(group: *mut WasmTaskGroupHandle) {
    if group.is_null() {
        return;
    }
    let children: Vec<*mut WasmTaskHandle> = {
        let g = &*group;
        let mut guard = g.children.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut *guard)
    };
    for child in children {
        // B-2026-06-09-1 — sole freer: call the inner with `do_free =
        // true` directly. The public `wt_task_join` would see
        // `registered == true` and skip the free → leak.
        let _status = wt_task_join_inner(child, std::ptr::null_mut(), true);
    }
    drop(Box::from_raw(group));
}

/// Flip every registered child's cooperative cancel flag, under the
/// children lock so a concurrent recursive `register` can't race the
/// walk. No cancel-sweep — that tears down event-loop-parked coroutines,
/// which don't exist on this target (see the module header).
///
/// # Safety
///
/// `group` live; registered children live (freed only at group drop,
/// which cannot overlap a `cancel()` call on the same live group).
pub(crate) unsafe fn wt_taskgroup_cancel(group: *mut WasmTaskGroupHandle) {
    if group.is_null() {
        return;
    }
    let g = &*group;
    let children = g.children.lock().unwrap_or_else(|p| p.into_inner());
    for &child in children.iter() {
        if child.is_null() {
            continue;
        }
        // SAFETY: each registered child handle is live for the group's
        // lifetime. Same-module access to the private `cancel` flag.
        (*child).cancel.store(true, Ordering::Release);
    }
}

// ── extern surface (threaded wasm archive only) ────────────────────────────
//
// Same symbol names + ABI as `scheduler.rs`'s native exports and
// `seq_scheduler.rs`'s sequential ones; the gate completes the
// one-exporter-per-archive matrix (this module is never compiled with
// `net` — the wasm archives are `--no-default-features` — but the
// module-level decl gate in lib.rs already pins that).

#[cfg(all(target_family = "wasm", feature = "wasm-threads"))]
mod exports {
    use super::*;

    /// Threaded `spawn()` — see [`wt_spawn`].
    ///
    /// `result_size` / `result_align` are `u64`, not `usize`: codegen
    /// declares them `i64` for every target, and wasm32 traps signature
    /// mismatches at the call — i32-width `usize` params here would
    /// land on a `signature_mismatch` stub (the `karac_par_run` /
    /// `__karac_malloc64` size_t class; `seq_scheduler.rs`'s export
    /// carries the same widening).
    ///
    /// # Safety
    ///
    /// See [`wt_spawn`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_spawn(
        fn_ptr: SpawnFn,
        env: *mut c_void,
        result_size: u64,
        result_align: u64,
    ) -> *mut WasmTaskHandle {
        wt_spawn(fn_ptr, env, result_size as usize, result_align as usize)
    }

    /// Threaded join — see [`wt_task_join`].
    ///
    /// # Safety
    ///
    /// See [`wt_task_join`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_task_join(
        handle: *mut WasmTaskHandle,
        out_slot: *mut u8,
    ) -> u8 {
        wt_task_join(handle, out_slot)
    }

    /// Drop-without-join — see [`wt_task_handle_free`].
    ///
    /// # Safety
    ///
    /// See [`wt_task_handle_free`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_task_handle_free(handle: *mut WasmTaskHandle) {
        wt_task_handle_free(handle)
    }

    /// Non-joining state read — see [`wt_task_state`].
    ///
    /// # Safety
    ///
    /// See [`wt_task_state`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_task_state(handle: *const WasmTaskHandle) -> u8 {
        wt_task_state(handle)
    }

    /// Threaded `TaskGroup` allocation — see [`wt_taskgroup_new`].
    #[no_mangle]
    pub extern "C" fn karac_runtime_taskgroup_new() -> *mut WasmTaskGroupHandle {
        wt_taskgroup_new()
    }

    /// Child registration — see [`wt_taskgroup_register`].
    ///
    /// # Safety
    ///
    /// See [`wt_taskgroup_register`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_taskgroup_register(
        group: *mut WasmTaskGroupHandle,
        child: *mut WasmTaskHandle,
    ) {
        wt_taskgroup_register(group, child)
    }

    /// Join barrier at group drop — see [`wt_taskgroup_join_and_free`].
    ///
    /// # Safety
    ///
    /// See [`wt_taskgroup_join_and_free`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_taskgroup_join_and_free(
        group: *mut WasmTaskGroupHandle,
    ) {
        wt_taskgroup_join_and_free(group)
    }

    /// Cooperative cancel — see [`wt_taskgroup_cancel`].
    ///
    /// # Safety
    ///
    /// See [`wt_taskgroup_cancel`].
    #[no_mangle]
    pub unsafe extern "C" fn karac_runtime_taskgroup_cancel(group: *mut WasmTaskGroupHandle) {
        wt_taskgroup_cancel(group)
    }

    /// 16-aligned scratch shadow stack for the JS glue's main-thread
    /// "service" instance (phase-10 host-async producers — `std.web.time`).
    ///
    /// On `--features wasm-threads` the program's `_start` runs in a
    /// *primary worker*; a host timer/event callback that must
    /// `karac_runtime_channel_send` into a parked `recv` runs on the main
    /// thread, through a SECOND wasm instance over the same shared memory
    /// (the only agent that can `WebAssembly.Memory`-share *and* mutate the
    /// channel under its lock). That service instance is never
    /// thread-started, so its `__stack_pointer` global still points at the
    /// linker-reserved stack — the SAME region the primary worker's
    /// `_start` is actively using. The glue retargets the service
    /// instance's exported `__stack_pointer` at the top of this dedicated
    /// buffer right after instantiation so its `channel_send` frames can
    /// never clobber the parked worker's live frames. 64 KiB is ample: the
    /// only chain that runs on it is `channel_send`/`drop_sender` →
    /// futex/dlmalloc, a few hundred bytes deep. Lives in BSS (zeroed,
    /// counted in the module's initial memory), so its address is a
    /// link-time constant valid the moment memory exists.
    // Only the buffer's *address* is ever used (as the service instance's
    // shadow-stack region); its bytes are written/read by wasm stack
    // traffic, never by Rust — hence `dead_code` on the field.
    #[repr(align(16))]
    struct ServiceStack(#[allow(dead_code)] [u8; 65536]);
    static mut KARAC_SERVICE_STACK: ServiceStack = ServiceStack([0u8; 65536]);

    /// Top (high address) of [`KARAC_SERVICE_STACK`] — the value the glue
    /// stores into the service instance's exported `__stack_pointer` (the
    /// shadow stack grows downward). A link-time-constant address: this fn
    /// reads no memory and, as a constant-returning leaf, uses no shadow
    /// stack itself, so the glue may call it while still on the shared
    /// default stack without risking the very collision it sets up to
    /// avoid. `u32` (wasm32 address); JS reads it as a number.
    #[no_mangle]
    pub extern "C" fn karac_runtime_service_stack_top() -> u32 {
        let base = (&raw const KARAC_SERVICE_STACK) as *const u8 as usize;
        ((base + 65536) & !15) as u32
    }

    /// 16-aligned scratch buffer in shared linear memory for host-async
    /// *event-data* producers (phase-10 `Channel[T]`, `T != ()` — e.g.
    /// `std.web.events.pointer_moves` → `Channel[PointerEvent]`).
    ///
    /// Unit-payload producers (`after`/`animation_frames`) send 0 bytes, so
    /// `channel_send(ch, 0, 0)` needs no source buffer. A structured event
    /// payload must instead live somewhere in linear memory before
    /// `channel_send` copies it into the queue. The glue's main-thread event
    /// callback marshals the event fields here (single writer — the main
    /// thread) then calls `channel_send(ch, scratch, size)`, which copies the
    /// bytes out *immediately* under the channel lock; so one reused buffer
    /// suffices and there is never concurrent access (the parked worker reads
    /// its own `recv` out-slot, never this buffer). 64 bytes covers every v1
    /// event struct. Like [`KARAC_SERVICE_STACK`] it lives in BSS, so its
    /// address is a link-time constant valid the moment memory exists.
    // Only the buffer's *address* is ever used (handed to JS, which writes it
    // through the shared `WebAssembly.Memory`); its bytes are never
    // read/written by Rust — hence `dead_code` on the field.
    #[repr(align(16))]
    struct EventScratch(#[allow(dead_code)] [u8; 64]);
    static mut KARAC_EVENT_SCRATCH: EventScratch = EventScratch([0u8; 64]);

    /// Address of [`KARAC_EVENT_SCRATCH`] — where the glue marshals a host
    /// event payload before `channel_send`. A constant-returning leaf (reads
    /// no memory, uses no shadow stack), so the glue may call it on the
    /// shared default stack just like [`karac_runtime_service_stack_top`].
    /// `u32` (wasm32 address); JS reads it as a number.
    #[no_mangle]
    pub extern "C" fn karac_runtime_event_scratch() -> u32 {
        (&raw const KARAC_EVENT_SCRATCH) as *const u8 as usize as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    // i64-returning wrapper: writes (env_val + 1) into result_out.
    unsafe extern "C" fn add_one_wrapper(
        env: *mut c_void,
        result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        let val = *(env as *const i64);
        *(result_out as *mut i64) = val + 1;
    }

    // Wrapper that records the executing thread's id (hashed to u64)
    // into the env-pointed slot. Used to pin "ran on a pool worker, not
    // inline on the spawner".
    unsafe extern "C" fn record_thread_wrapper(
        env: *mut c_void,
        _result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        std::thread::current().id().hash(&mut hasher);
        *(env as *mut u64) = hasher.finish();
    }

    // Wrapper that bumps a shared counter (env points at AtomicU32).
    unsafe extern "C" fn bump_counter_wrapper(
        env: *mut c_void,
        _result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        (*(env as *const AtomicU32)).fetch_add(1, Ordering::SeqCst);
    }

    // Wrapper that spin-waits for its cancel flag, then records that it
    // observed it (env points at (flag_seen: *mut AtomicBool,)).
    unsafe extern "C" fn await_cancel_wrapper(
        env: *mut c_void,
        _result_out: *mut u8,
        cancel: *const AtomicBool,
    ) {
        let seen = *(env as *const *const AtomicBool);
        while !(*cancel).load(Ordering::Relaxed) {
            std::thread::yield_now();
        }
        (*seen).store(true, Ordering::SeqCst);
    }

    // Lock-probe fixture: a child that takes the group's children lock
    // (via `wt_taskgroup_cancel`, whose flag-flip walk acquires it)
    // while the join drain is blocked on this very child. Deadlocks iff
    // the drain held the lock across its joins — pinning the
    // drain-outside-the-lock posture without spawning a straggler task
    // that could outlive the test's stack frame.
    struct LockProbeEnv {
        group: *mut WasmTaskGroupHandle,
        counter: *const AtomicU32,
    }
    unsafe extern "C" fn lock_probe_wrapper(
        env: *mut c_void,
        _result_out: *mut u8,
        _cancel: *const AtomicBool,
    ) {
        let penv = &*(env as *const LockProbeEnv);
        // Acquires the group's children lock; the join drain is
        // currently waiting on this child's terminal state. (Flips our
        // own cancel flag as a side effect — benign; this wrapper never
        // polls it.)
        wt_taskgroup_cancel(penv.group);
        (*penv.counter).fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn wt_spawn_join_transports_result() {
        unsafe {
            let env: i64 = 41;
            let h = wt_spawn(
                add_one_wrapper,
                &env as *const i64 as *mut c_void,
                std::mem::size_of::<i64>(),
                std::mem::align_of::<i64>(),
            );
            assert!(!h.is_null());
            let mut out: i64 = 0;
            let status = wt_task_join(h, &mut out as *mut i64 as *mut u8);
            assert_eq!(status, TASK_STATE_COMPLETED);
            assert_eq!(out, 42);
        }
    }

    #[test]
    fn wt_spawn_runs_on_pool_not_inline() {
        use std::hash::{Hash, Hasher};
        unsafe {
            let mut task_tid: u64 = 0;
            let h = wt_spawn(
                record_thread_wrapper,
                &mut task_tid as *mut u64 as *mut c_void,
                0,
                0,
            );
            let status = wt_task_join(h, std::ptr::null_mut());
            assert_eq!(status, TASK_STATE_COMPLETED);
            let mut hasher = std::hash::DefaultHasher::new();
            std::thread::current().id().hash(&mut hasher);
            let my_tid = hasher.finish();
            assert_ne!(
                task_tid, my_tid,
                "spawned task must run on a pool worker, not inline on the spawner"
            );
            assert_ne!(task_tid, 0, "wrapper must have recorded a thread id");
        }
    }

    #[test]
    fn wt_task_state_nonjoining_read_reaches_terminal() {
        unsafe {
            let env: i64 = 1;
            let h = wt_spawn(
                add_one_wrapper,
                &env as *const i64 as *mut c_void,
                std::mem::size_of::<i64>(),
                std::mem::align_of::<i64>(),
            );
            // Poll without consuming the handle: PENDING or COMPLETED
            // are the only legal observations pre-join.
            let early = wt_task_state(h);
            assert!(
                early == TASK_STATE_PENDING || early == TASK_STATE_COMPLETED,
                "unexpected pre-join state {early}"
            );
            // The join is the consume point.
            let mut out: i64 = 0;
            assert_eq!(
                wt_task_join(h, &mut out as *mut i64 as *mut u8),
                TASK_STATE_COMPLETED
            );
            assert_eq!(out, 2);
        }
    }

    #[test]
    fn wt_taskgroup_join_waits_all_children() {
        unsafe {
            let counter = AtomicU32::new(0);
            let group = wt_taskgroup_new();
            const N: u32 = 8;
            for _ in 0..N {
                let child = wt_spawn(
                    bump_counter_wrapper,
                    &counter as *const AtomicU32 as *mut c_void,
                    0,
                    0,
                );
                wt_taskgroup_register(group, child);
            }
            wt_taskgroup_join_and_free(group);
            assert_eq!(
                counter.load(Ordering::SeqCst),
                N,
                "group join must wait for every registered child"
            );
        }
    }

    // B-2026-06-09-1 — a group-registered child that is ALSO explicitly
    // `.join()`ed must be freed exactly once: the explicit join transports
    // the result without freeing (the group still owns it), the group's
    // scope-exit drop reaps it. Before the fix both freed the same handle
    // → use-after-free.
    #[test]
    fn wt_registered_child_explicit_join_then_group_drop_frees_once() {
        let env_val: i64 = 41;
        unsafe {
            let group = wt_taskgroup_new();
            let child = wt_spawn(
                add_one_wrapper,
                &env_val as *const i64 as *mut c_void,
                std::mem::size_of::<i64>(),
                std::mem::align_of::<i64>(),
            );
            wt_taskgroup_register(group, child);
            let mut out: i64 = 0;
            let status = wt_task_join(child, &mut out as *mut i64 as *mut u8);
            assert_eq!(status, TASK_STATE_COMPLETED);
            assert_eq!(out, 42);
            // Group drop is the sole freer — no double-free / UAF.
            wt_taskgroup_join_and_free(group);
        }
    }

    #[test]
    fn wt_taskgroup_join_drains_outside_lock() {
        unsafe {
            let counter = AtomicU32::new(0);
            let group = wt_taskgroup_new();
            let penv = LockProbeEnv {
                group,
                counter: &counter,
            };
            let child = wt_spawn(
                lock_probe_wrapper,
                &penv as *const LockProbeEnv as *mut c_void,
                0,
                0,
            );
            wt_taskgroup_register(group, child);
            // The drain joins the child, which meanwhile acquires the
            // group's children lock from its worker thread. A lock-held
            // drain would deadlock right here; drain-outside-the-lock
            // completes. (The child's lock acquisition happens strictly
            // before its terminal store, which the drain's join waits
            // for — so the group is guaranteed live when probed.)
            wt_taskgroup_join_and_free(group);
            assert_eq!(
                counter.load(Ordering::SeqCst),
                1,
                "lock-probe child must have run to completion"
            );
        }
    }

    #[test]
    fn wt_taskgroup_cancel_flags_visible_to_children() {
        unsafe {
            let seen = AtomicBool::new(false);
            let seen_ptr: *const AtomicBool = &seen;
            let group = wt_taskgroup_new();
            let child = wt_spawn(
                await_cancel_wrapper,
                &seen_ptr as *const *const AtomicBool as *mut c_void,
                0,
                0,
            );
            wt_taskgroup_register(group, child);
            // The child spin-waits on its cancel flag; flipping it is
            // the only thing that lets the join below complete.
            wt_taskgroup_cancel(group);
            wt_taskgroup_join_and_free(group);
            assert!(
                seen.load(Ordering::SeqCst),
                "child must observe the cooperative cancel flag"
            );
        }
    }

    #[test]
    fn wt_null_handles_are_tolerated() {
        unsafe {
            assert_eq!(
                wt_task_join(std::ptr::null_mut(), std::ptr::null_mut()),
                TASK_STATE_CANCELLED
            );
            assert_eq!(wt_task_state(std::ptr::null()), TASK_STATE_CANCELLED);
            wt_task_handle_free(std::ptr::null_mut());
            wt_taskgroup_register(std::ptr::null_mut(), std::ptr::null_mut());
            wt_taskgroup_join_and_free(std::ptr::null_mut());
            wt_taskgroup_cancel(std::ptr::null_mut());
        }
    }
}
