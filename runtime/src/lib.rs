//! Kāra runtime library. Statically linked into every compiled Kāra binary.
//!
//! The compiler emits calls into this library for parallel execution, task
//! scheduling, and (eventually) event-loop integration and atomic primitives.
//! See design.md § Runtime Distribution.
//!
//! All public symbols are `extern "C"` — the compiler emits LLVM calls against
//! this ABI, so the surface must remain stable across compiler/runtime
//! versions built in lockstep and is NOT stable across independently built
//! pairs. Distribution is always compiler+runtime bundled.
//!
//! ## Debugger Contract (design.md § Debugger Contract)
//!
//! The four-piece contract surface that gives slice 5's
//! `std.runtime::list_par_blocks()` / `list_tasks()` / `has_debug_metadata()`
//! and the future `std.panic` crash-report `parallel_context` field a stable
//! shape to read against:
//!
//! 1. **`SpawnSiteId` metadata table** — `KARAC_SPAWN_SITES` /
//!    `KARAC_SPAWN_SITES_LEN` / `KARAC_SPAWN_SITES_ENABLED` globals emitted
//!    by codegen (slice 3, `c6d8b44`). Per-binary stable IDs for every
//!    `par {}` block (explicit + inferred) joined to `(file, line, col)`.
//! 2. **Parent-frame reference on worker frames** — `KaracFrame::parent`
//!    (slice 4): every worker frame produced by `karac_par_run` carries a
//!    pointer back to the frame that created it; root tasks have `null`.
//!    Slice 5 walks this graph to reconstruct the structured-concurrency
//!    tree.
//! 3. **Await-chain pointer on suspended tasks** — `KaracFrame::wait_target`
//!    (slice 4 contract surface only; v1 always populates `KaracWaitTarget::None`).
//!    Real values land when Phase 6.3's network event loop ships and registers
//!    `WaitTarget`s at I/O-effect-boundary operations.
//! 4. **Crash-report `parallel_context` field** — co-developed with these
//!    globals, lands with `std.panic` (separate Phase 8 entry).

mod clone;
mod map;

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

/// A single branch of a `par {}` block: a function pointer and its opaque
/// context. The context is heap-allocated by the compiler and freed by the
/// runtime after the branch returns.
#[repr(C)]
pub struct KaracBranch {
    pub func: unsafe extern "C" fn(*mut c_void, *const AtomicBool),
    pub ctx: *mut c_void,
}

// SAFETY: The compiler guarantees that each branch's ctx is exclusively owned
// by that branch for the duration of karac_par_run. Branches never share
// mutable state through ctx; any shared state goes through separately
// allocated Arc values (see Rc→Arc promotion in ownership.rs).
unsafe impl Send for KaracBranch {}

// ── Debugger Contract — frame tracking (slice 4) ───────────────────────────
//
// See module-level doc-comment for the four-piece contract overview. This
// section ships pieces (2) and (3): per-worker `KaracFrame`s carrying a
// parent-frame pointer + a `wait_target` field, and the cross-thread
// `ACTIVE_FRAMES` registry slice 5 will enumerate.

/// Wait-target discriminator on `KaracFrame`. Item (3) of the four-piece
/// Debugger Contract; see module-level doc.
///
/// **v1 ships single-variant `None`.** The `wait_target` field exists on
/// every `KaracFrame` and the enum's name is stable, but no other variants
/// are defined yet because v1's blocking runtime has no real suspension to
/// track — `Receiver.recv()` returns `Unit` on empty rather than blocking,
/// no event loop exists yet. Phase 6.3's network event loop will add
/// `PeerTask { task: *const KaracFrame }` and `IoHandle { handle: *const c_void }`
/// variants additively (non-breaking under `#[non_exhaustive]` per
/// design.md § Stability) once it registers real `WaitTarget`s at
/// I/O-effect-boundary operations.
///
/// `#[repr(u8)]` pins the discriminant width at 1 byte for stable FFI.
/// The single-variant v1 form is `{ tag: u8 }` (one byte total — see the
/// `test_wait_target_size_pinned` runtime test). When Phase 6.3 adds
/// payload-carrying variants, the representation upgrades to `#[repr(C, u8)]`
/// (C-style tagged union with `u8` discriminant) — additive change, since
/// the existing single-variant `None` keeps discriminant 0 and the C-style
/// upgrade is wire-compatible for that variant. Rustc rejects `#[repr(C, u8)]`
/// on a no-payload enum (`E0566 conflicting representation hints`), so v1
/// uses `#[repr(u8)]` standalone; the plan-side spec said `#[repr(C, u8)]`
/// but the no-payload form requires a single repr hint.
#[repr(u8)]
#[non_exhaustive]
pub enum KaracWaitTarget {
    /// Worker is running (or, in v1, always — until Phase 6.3 lights up).
    None,
}

/// Per-worker frame produced by `karac_par_run`. Item (2) of the four-piece
/// Debugger Contract; see module-level doc.
///
/// Allocated on the worker thread's stack inside the `thread::scope` body,
/// so `*const KaracFrame` pointers are valid for the lifetime of that
/// worker's branch invocation. Pointers stored in `ACTIVE_FRAMES` are
/// removed at frame teardown (success or panic, via `FrameGuard`'s `Drop`)
/// before the stack frame deallocates. Pointers stored as a child's
/// `parent` field are safe because Rust's `thread::scope` guarantees the
/// parent thread's stack outlives all scope-spawned children.
///
/// Slice 5's `std.runtime::list_par_blocks()` joins `spawn_site_id` against
/// the slice-3 `KARAC_SPAWN_SITES` table to fill `(file, line, col)`; the
/// future `std.panic` crash-report reads the same fields for its
/// `parallel_context` block.
#[repr(C)]
pub struct KaracFrame {
    /// Frame of the worker that spawned this one, or `null` for root tasks.
    /// Walked by slice 5 to reconstruct the structured-concurrency tree.
    pub parent: *const KaracFrame,
    /// Index into the slice-3 `KARAC_SPAWN_SITES` table — identifies the
    /// `par {}` site (file, line, col, worker_count) this frame was forked
    /// from.
    pub spawn_site_id: u32,
    /// 0-based branch index within the par block — first branch is 0,
    /// second is 1, etc.
    pub worker_index: u32,
    /// What this worker is currently waiting on. Always `KaracWaitTarget::None`
    /// in v1 (no real suspension exists yet); Phase 6.3's event loop will set
    /// real values at I/O-effect-boundary operations.
    pub wait_target: KaracWaitTarget,
}

// Per-thread current-frame pointer. Workers set this to their
// stack-allocated `KaracFrame` for the duration of their branch invocation;
// root tasks (and threads outside any par-block context) read `null`.
//
// **`Cell`, not `RefCell`** — the inner value is `*const KaracFrame`
// (a `Copy` raw pointer), so `Cell::set` / `Cell::get` is sufficient and
// avoids `RefCell` borrow-tracking overhead.
//
// **TLS-during-atexit caveat does not apply.** The `karac_error_trace_*`
// section above (line ~115) explains why `thread_local!` is unsafe to read
// during `atexit` (TLS destructors run during thread shutdown, *before*
// the C runtime's atexit handlers, so reads from inside `atexit` panic).
// Slice 4's reads happen inside live Kāra code via
// `karac_runtime_get_current_frame`, never inside an atexit handler, so
// the constraint doesn't apply here. Future readers conflating the two
// surfaces should re-check this comment before redirecting frame tracking
// through a global mutex.
thread_local! {
    static CURRENT_FRAME: Cell<*const KaracFrame> = const { Cell::new(ptr::null()) };
}

/// Newtype around `*const KaracFrame` that opts into `Send + Sync` for
/// storage in the cross-thread `ACTIVE_FRAMES` registry. Raw pointers are
/// `!Send` by default; the soundness comes from the structured-concurrency
/// invariant that `thread::scope` joins all workers before `karac_par_run`
/// returns, and `FrameGuard::drop` removes each entry from the registry
/// before its stack frame deallocates. Iteration via
/// `karac_runtime_for_each_active_frame` is gated on the registry lock to
/// rule out reading-while-deregistering races.
#[derive(Copy, Clone, PartialEq, Eq)]
struct FramePtr(*const KaracFrame);

// SAFETY: see the doc-comment above. The runtime is the only writer to
// `ACTIVE_FRAMES`; pointers are valid by construction (stack-allocated
// inside a `thread::scope`-bounded worker) and removed before invalidation.
unsafe impl Send for FramePtr {}
unsafe impl Sync for FramePtr {}

/// Cross-thread registry of currently-active worker frames. Slice 5's
/// `karac_runtime_for_each_active_frame` enumerates this list under the
/// lock to materialize `Vec[ParBlockInfo]` for `std.runtime::list_par_blocks()`.
///
/// `Mutex<Vec<FramePtr>>` chosen over `RwLock<HashMap<ThreadId, _>>` because
/// slice 5 doesn't query by thread (it just enumerates), v1 has few
/// par-blocks (a linear `retain` on deregister is fine), and write/read
/// frequencies are roughly balanced (each fork = 1 lock at register +
/// 1 lock at deregister; iteration is rare). `RwLock` is worth its overhead
/// only when reads dominate writes ~10x+.
///
/// **Pointer lifetime constraint.** Entries point into worker thread stacks.
/// They are valid only while the worker is running its branch —
/// `FrameGuard::drop` removes the entry before the stack frame deallocates.
/// Slice 5's iteration **must** happen while holding the registry lock so a
/// worker can't exit and invalidate an entry between the enumerator's read
/// and the consumer's dereference. `karac_runtime_for_each_active_frame`'s
/// callback API enforces this by firing the callback under the lock.
static ACTIVE_FRAMES: Mutex<Vec<FramePtr>> = Mutex::new(Vec::new());

/// Lazy gating helper — read `KARAC_RUNTIME_DEBUG_METADATA` once and cache.
/// Mirrors codegen's `read_runtime_debug_metadata_env` exactly; both sides
/// independently honor the same env var.
///
/// - `Ok("0")` → `false` (gate explicitly off).
/// - `Ok(_)`   → `true` (any other value, including empty).
/// - `Err(_)`  → `true` (dev default; profile-aware defaults land in
///   Phase 8.5 Track 2).
///
/// The result is cached for the process lifetime via `OnceLock`. Tests that
/// flip the env var between runs can't observe a re-read once the cache is
/// initialized — they go through `runtime_debug_metadata_enabled_uncached`
/// (cfg(test)) instead.
fn runtime_debug_metadata_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(read_runtime_debug_metadata_env)
}

fn read_runtime_debug_metadata_env() -> bool {
    !matches!(std::env::var("KARAC_RUNTIME_DEBUG_METADATA"), Ok(v) if v == "0")
}

/// Test-only re-read of the gating env var that bypasses the `OnceLock`
/// cache used by `runtime_debug_metadata_enabled`. Used by
/// `test_runtime_debug_metadata_disabled_skips_tracking` so the test's
/// env-var mutation actually takes effect — otherwise the first slice-4
/// test to fire would freeze the cache to `true` and the disabled-path
/// test would silently pass against the wrong code path.
///
/// Tests serialize on `FRAME_TRACKING_ENV_LOCK` to prevent races on the
/// env var.
#[cfg(test)]
fn runtime_debug_metadata_enabled_uncached() -> bool {
    read_runtime_debug_metadata_env()
}

/// RAII guard that registers a frame in `ACTIVE_FRAMES` + `CURRENT_FRAME`
/// on construction and deregisters on `Drop`. Drop runs on both normal
/// return *and* unwind, so a panicking branch fn still cleanly removes its
/// entry from the registry — pinned by `test_frame_deregistered_on_panic`.
///
/// Hand-rolled rather than pulling in `scopeguard` to keep runtime deps
/// minimal (zero-heavy-deps policy; runtime is no_std-adjacent).
struct FrameGuard {
    frame_ptr: FramePtr,
    prev_current: *const KaracFrame,
}

impl FrameGuard {
    /// Register `frame` as the current frame on this thread and add it to
    /// `ACTIVE_FRAMES`. Caller must keep the underlying `KaracFrame` alive
    /// (e.g. on the worker's stack) until the guard drops.
    fn new(frame: &KaracFrame) -> Self {
        let frame_ptr = FramePtr(frame as *const KaracFrame);
        let prev_current = CURRENT_FRAME.with(|c| c.replace(frame_ptr.0));
        // Lock-poison handling: a poisoned mutex still has a valid Vec
        // inside; recover the inner state and proceed (matches the
        // `print_trace_at_exit` pattern above).
        let mut guard = ACTIVE_FRAMES.lock().unwrap_or_else(|p| p.into_inner());
        guard.push(frame_ptr);
        drop(guard);
        FrameGuard {
            frame_ptr,
            prev_current,
        }
    }
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        let mut guard = ACTIVE_FRAMES.lock().unwrap_or_else(|p| p.into_inner());
        guard.retain(|&p| p != self.frame_ptr);
        drop(guard);
        CURRENT_FRAME.with(|c| c.set(self.prev_current));
    }
}

/// Execute branches concurrently using a fixed-size thread pool and join
/// before returning.
///
/// **Thread pool**: min(branch_count, available_parallelism) worker threads.
/// Each worker grabs the next branch index via an atomic counter —
/// simple work distribution without external dependencies.
///
/// **Fail-fast cancellation**: an internal `AtomicBool` cancel flag is set
/// when any branch panics. Workers check the flag before picking up new
/// branches, so remaining branches are skipped after a failure. Branches
/// already running complete (completion-wins at branch granularity).
///
/// **Frame tracking (Debugger Contract slice 4).** When
/// `runtime_debug_metadata_enabled()` is `true`, each branch runs inside a
/// `FrameGuard` that stack-allocates a `KaracFrame { parent, spawn_site_id,
/// worker_index, wait_target: KaracWaitTarget::None }` and registers it in
/// `ACTIVE_FRAMES` for slice 5's enumeration surface. `parent` is captured
/// from the calling thread's `CURRENT_FRAME` before `thread::scope` starts,
/// so workers spawned inside another worker's branch see the outer worker's
/// frame as their parent (nested-par chain). When the gate is off the
/// function runs the existing thread::scope loop unchanged — no allocation,
/// no bookkeeping.
///
/// **Result collection**: not yet implemented — branches return void.
/// Error propagation via typed results is a Phase 6.2 follow-up.
///
/// # Parameters
///
/// - `branches` / `count`: array of `KaracBranch` descriptors (one per
///   parallel statement in the source `par {}` block).
/// - `spawn_site_id`: identifies the par site for slice 4's `KaracFrame`
///   metadata. Indexes into the slice-3 `KARAC_SPAWN_SITES` table emitted
///   by codegen so slice 5 can join `(file, line, col)`. Ignored when
///   `runtime_debug_metadata_enabled() == false`.
///
/// # Safety
///
/// `branches` must point to `count` valid `KaracBranch` values; each
/// branch's `func` must be a valid function pointer and `ctx` must be a
/// pointer the `func` is prepared to receive. The compiler always satisfies
/// these preconditions.
#[no_mangle]
pub unsafe extern "C" fn karac_par_run(
    branches: *const KaracBranch,
    count: usize,
    spawn_site_id: u32,
) {
    if count == 0 {
        return;
    }

    let pool_size = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(count);

    // Copy branch descriptors so thread closures can capture them by reference.
    let copied: Vec<(unsafe extern "C" fn(*mut c_void, *const AtomicBool), usize)> = (0..count)
        .map(|i| {
            let b = &*branches.add(i);
            (b.func, b.ctx as usize)
        })
        .collect();

    let cancel = AtomicBool::new(false);
    let next_idx = AtomicUsize::new(0);
    let track_frames = runtime_debug_metadata_enabled();
    // Capture the calling thread's current frame *before* `thread::scope`
    // — children's `parent` field points at this. Cast through `usize` so
    // the closure captures a `Send + Sync` integer rather than the raw
    // pointer (which Rust's auto-trait inference flags as `!Send`).
    // Soundness: `thread::scope` guarantees the calling thread's stack
    // outlives all scope-spawned children, so the address remains valid
    // for the duration of the join.
    let parent_addr: usize = if track_frames {
        CURRENT_FRAME.with(|c| c.get()) as usize
    } else {
        0
    };

    thread::scope(|s| {
        for _ in 0..pool_size {
            s.spawn(|| {
                loop {
                    // Check cancel before picking up new work.
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                    if idx >= count {
                        break;
                    }
                    let (func, ctx) = copied[idx];
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                            if track_frames {
                                let frame = KaracFrame {
                                    parent: parent_addr as *const KaracFrame,
                                    spawn_site_id,
                                    worker_index: idx as u32,
                                    wait_target: KaracWaitTarget::None,
                                };
                                let _guard = FrameGuard::new(&frame);
                                func(ctx as *mut c_void, &cancel as *const AtomicBool);
                                // `_guard` drops here on normal return,
                                // deregistering the frame. On panic the
                                // unwind path still runs Drop.
                            } else {
                                func(ctx as *mut c_void, &cancel as *const AtomicBool);
                            }
                        }));
                    if result.is_err() {
                        // Fail-fast: signal other workers to stop.
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
        // Implicit join at scope end — all workers finish before we return.
    });
}

/// Public extern getter for slice 5 / tests. Returns the current thread's
/// active worker frame, or `null` for root tasks (and any thread outside a
/// par-block context, including any thread when
/// `runtime_debug_metadata_enabled() == false`).
///
/// Slice 5's `std.runtime::list_tasks()` reads through this symbol to find
/// the calling task's position in the structured-concurrency tree, then
/// walks `KaracFrame::parent` to enumerate ancestors.
///
/// # Safety
///
/// The returned pointer is valid only while the worker thread that owns
/// the frame is alive — that is, while the `karac_par_run` call that
/// produced the frame has not yet returned. Callers must not store the
/// pointer beyond the current par-block's join boundary. Slice 5's wrapper
/// dereferences-and-copies inside the same call frame, so this constraint
/// is naturally upheld.
#[no_mangle]
pub extern "C" fn karac_runtime_get_current_frame() -> *const KaracFrame {
    CURRENT_FRAME.with(|c| c.get())
}

/// Public extern iteration callback for slice 5. Invokes `callback` once
/// per currently-active worker frame, passing the frame pointer plus the
/// caller's opaque `userdata`. Slice 5's wrapper builds its
/// `Vec[ParBlockInfo]` inside the callback.
///
/// **Hold-the-lock-during-iteration is intentional.** `*const KaracFrame`
/// lifetimes are tied to the worker thread's stack; releasing the lock
/// before the slice-5-side reader finishes inspecting could let a worker
/// exit and invalidate the pointer (its `FrameGuard` deregisters on Drop,
/// then the stack frame deallocates). Callbacks fire under the
/// `ACTIVE_FRAMES` mutex.
///
/// # Safety
///
/// `callback` must be a valid function pointer with the documented
/// signature; it is invoked synchronously from the calling thread.
/// Callbacks MUST NOT call back into the runtime in ways that would
/// re-enter `ACTIVE_FRAMES` (e.g. spawning a new par block) — that would
/// deadlock. Read-only inspection of the `KaracFrame` fields is safe.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_for_each_active_frame(
    callback: unsafe extern "C" fn(*const KaracFrame, *mut c_void),
    userdata: *mut c_void,
) {
    let guard = ACTIVE_FRAMES.lock().unwrap_or_else(|p| p.into_inner());
    for &frame in guard.iter() {
        callback(frame.0, userdata);
    }
}

// ── Error return trace ─────────────────────────────────────────────────────
//
// Mirrors the interpreter's `error_trace` (src/interpreter.rs:592). On each
// `?` failure site, the codegen emits a call to `karac_error_trace_push`
// before propagating the `Err` / `None`. On a `?` success, codegen emits a
// `karac_error_trace_clear` so a successful path doesn't leak frames into
// later failures.
//
// Storage: a single global `Mutex<ErrorTraceState>` (depth-64 ring buffer).
// We deliberately do NOT use a `thread_local!` here: Rust's TLS destructors
// run during thread shutdown, BEFORE the C runtime's atexit handlers, so
// reading TLS from inside `atexit` triggers a "cannot access a Thread Local
// Storage value during or after destruction" panic. A global mutex sidesteps
// that — it remains valid for the entire process lifetime, including during
// atexit.
//
// Multi-threaded `?` use (par branches doing their own propagation) writes
// to the same buffer; pushes serialize through the lock. For v1 this is
// acceptable — the typical workload has `?` in serial call chains, and par
// branches in the MVP runtime discard their `Err` returns anyway, so they
// never reach the trace surface.
//
// Output format: defaults to the interpreter's text mode (cli.rs:1651-1664):
//
//     Error return trace:
//       <file>:<line>:<col>
//       ... (trace truncated, max 64 frames)         (only when truncated)
//
// At process exit the printer consults the `KARAC_ERROR_TRACE_FORMAT` env
// var and dispatches to one of three emitters:
//
//   - `text`   (default, missing/unrecognized values fall back here): the
//              stderr lines shown above. Backwards-compatible with the
//              pre-env-var build.
//   - `json`   single-document pretty-ish JSON on stderr matching the
//              interpreter's `format_error_trace_json` shape: a bare array
//              `[{"file":"…","line":N,"column":N},…]` when not truncated,
//              or `{"frames":[…],"truncated":true}` when truncated.
//   - `jsonl`  line-delimited JSON (NDJSON), one event per line:
//              `{"type":"frame","file":"…","line":N,"column":N}` per frame
//              and an optional trailing `{"type":"truncated","max":64}`
//              line when the ring buffer dropped older frames.
//
// The env var is read once at atexit-time (after the printer wakes); the
// runtime never observes mid-process changes — out of scope per the slice
// plan. The atexit registration is lazy — the first `karac_error_trace_push`
// call arms it. Programs that never `?`-propagate pay zero atexit overhead.

const ERROR_TRACE_MAX_DEPTH: usize = 64;

#[derive(Clone)]
struct ErrorTraceFrame {
    file: String,
    line: u32,
    col: u32,
}

struct ErrorTraceState {
    frames: Vec<ErrorTraceFrame>,
    truncated: bool,
}

impl ErrorTraceState {
    const fn new() -> Self {
        ErrorTraceState {
            frames: Vec::new(),
            truncated: false,
        }
    }
}

static ERROR_TRACE: Mutex<ErrorTraceState> = Mutex::new(ErrorTraceState::new());

extern "C" {
    /// POSIX `atexit(3)` — register a handler to run on normal program
    /// termination (return from main). Not invoked on `_exit` / `abort`.
    fn atexit(callback: extern "C" fn()) -> i32;
}

/// Push a frame onto the global error-return trace buffer. Called by
/// codegen at every `?` failure block before the early-return.
///
/// `file_ptr` / `file_len` describe a UTF-8 byte range identifying the
/// source file the `?` site lives in; the byte slice need not outlive this
/// call (the runtime copies into an owned `String`). Pass a null pointer or
/// zero length when the source filename is unavailable; the frame still
/// records line/col.
///
/// # Safety
///
/// `file_ptr` must either be null or point to `file_len` initialized,
/// readable bytes. The compiler always satisfies this — the slice lives in
/// the program's read-only string-pool section.
#[no_mangle]
pub unsafe extern "C" fn karac_error_trace_push(
    file_ptr: *const u8,
    file_len: usize,
    line: u32,
    col: u32,
) {
    register_trace_atexit_once();
    let file = if file_ptr.is_null() || file_len == 0 {
        String::new()
    } else {
        let bytes = std::slice::from_raw_parts(file_ptr, file_len);
        String::from_utf8_lossy(bytes).into_owned()
    };
    if let Ok(mut state) = ERROR_TRACE.lock() {
        if state.frames.len() >= ERROR_TRACE_MAX_DEPTH {
            state.frames.remove(0);
            state.truncated = true;
        }
        state.frames.push(ErrorTraceFrame { file, line, col });
    }
}

/// Reset the global error-return trace buffer. Called by codegen at every
/// `?` success site so subsequent failures don't include stale frames from
/// a recovered earlier propagation.
#[no_mangle]
pub extern "C" fn karac_error_trace_clear() {
    if let Ok(mut state) = ERROR_TRACE.lock() {
        state.frames.clear();
        state.truncated = false;
    }
}

/// Idempotently register the atexit-time printer the first time a `?` site
/// pushes a frame. Programs that never propagate via `?` skip the
/// registration entirely.
fn register_trace_atexit_once() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        // SAFETY: `atexit` accepts an `extern "C" fn()` pointer. The
        // handler reads the global mutex-protected state (still valid
        // during atexit, unlike thread_local) and writes to stderr.
        // A non-zero return from `atexit` would mean registration failed;
        // we ignore that — the program continues, the trace silently
        // won't print.
        unsafe {
            let _ = atexit(print_trace_at_exit);
        }
    });
}

/// Output format selected by the `KARAC_ERROR_TRACE_FORMAT` env var.
/// `Text` is the default and preserves the pre-env-var behavior verbatim.
#[derive(Clone, Copy)]
enum TraceFormat {
    Text,
    Json,
    Jsonl,
}

impl TraceFormat {
    /// Parse the env var. Missing / empty / unrecognized values fall back
    /// to `Text` (no diagnostic — keeping startup quiet matches the
    /// "format-switching mid-process is out of scope" stance).
    fn from_env() -> Self {
        match std::env::var("KARAC_ERROR_TRACE_FORMAT")
            .unwrap_or_default()
            .as_str()
        {
            "json" => TraceFormat::Json,
            "jsonl" => TraceFormat::Jsonl,
            // Empty string, "text", or anything else → text.
            _ => TraceFormat::Text,
        }
    }
}

extern "C" fn print_trace_at_exit() {
    // `lock()` may fail only if a prior holder panicked. In that case we
    // can still try to print via `into_inner` on the poisoned guard.
    let state = match ERROR_TRACE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if state.frames.is_empty() {
        return;
    }
    match TraceFormat::from_env() {
        TraceFormat::Text => emit_text(&state),
        TraceFormat::Json => emit_json(&state),
        TraceFormat::Jsonl => emit_jsonl(&state),
    }
}

fn emit_text(state: &ErrorTraceState) {
    eprintln!("Error return trace:");
    for f in &state.frames {
        let file_part = if f.file.is_empty() {
            String::new()
        } else {
            format!("{}:", f.file)
        };
        eprintln!("  {}{}:{}", file_part, f.line, f.col);
    }
    if state.truncated {
        eprintln!(
            "  ... (trace truncated, max {} frames)",
            ERROR_TRACE_MAX_DEPTH
        );
    }
}

/// Single-document JSON matching the interpreter's
/// `cli.rs::format_error_trace_json` shape verbatim:
///
/// - Not truncated: bare array `[{"file":"…","line":N,"column":N},…]`.
/// - Truncated:     `{"frames":[…],"truncated":true}`.
///
/// Emitted on stderr (peer to text mode — keeps the program's stdout
/// clean for downstream pipelines).
fn emit_json(state: &ErrorTraceState) {
    let mut frames = String::new();
    for (i, f) in state.frames.iter().enumerate() {
        if i > 0 {
            frames.push(',');
        }
        write_frame_object(&mut frames, f);
    }
    if state.truncated {
        eprintln!("{{\"frames\":[{}],\"truncated\":true}}", frames);
    } else {
        eprintln!("[{}]", frames);
    }
}

/// Line-delimited JSON (NDJSON): one event per line, each line a
/// self-contained JSON object. Frames carry `"type":"frame"`; a trailing
/// `{"type":"truncated","max":N}` line is emitted only when the ring
/// buffer dropped older entries. The shape matches the interpreter's
/// JSONL channel idiom (`emit_jsonl_event` in `cli.rs`).
fn emit_jsonl(state: &ErrorTraceState) {
    for f in &state.frames {
        let mut line = String::from("{\"type\":\"frame\",");
        write_frame_fields(&mut line, f);
        line.push('}');
        eprintln!("{}", line);
    }
    if state.truncated {
        eprintln!(
            "{{\"type\":\"truncated\",\"max\":{}}}",
            ERROR_TRACE_MAX_DEPTH
        );
    }
}

/// Append a `{"file":…,"line":N,"column":N}` object literal to `out`.
fn write_frame_object(out: &mut String, f: &ErrorTraceFrame) {
    out.push('{');
    write_frame_fields(out, f);
    out.push('}');
}

/// Append the bare `"file":…,"line":N,"column":N` field set (no braces)
/// so callers can splice extra fields like `"type":"frame"` alongside.
fn write_frame_fields(out: &mut String, f: &ErrorTraceFrame) {
    out.push_str("\"file\":");
    write_json_string(out, &f.file);
    out.push_str(",\"line\":");
    push_u32(out, f.line);
    out.push_str(",\"column\":");
    push_u32(out, f.col);
}

/// Hand-written JSON string escape — the runtime intentionally avoids a
/// `serde_json` dependency (zero-heavy-deps policy; runtime is no_std-
/// adjacent). Escapes match the interpreter's `cli.rs::json_string`:
/// `"`, `\`, `\n`, `\r`, `\t`, and any other control byte (`< 0x20`)
/// goes through `\u00XX`. Everything else passes through untouched —
/// including non-ASCII, since the source filename arrives as UTF-8 from
/// `karac_error_trace_push` and the output stream is byte-transparent.
fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // `\u00XX` for the remaining control bytes (BS, FF, etc.).
                let bytes = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    hex_nibble(((c as u32) >> 4) as u8),
                    hex_nibble((c as u32) as u8),
                ];
                // SAFETY: every byte produced above is ASCII (`\\`, `u`,
                // `0`, and two lowercase hex digits) so the slice is
                // valid UTF-8.
                out.push_str(std::str::from_utf8(&bytes).unwrap());
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn hex_nibble(b: u8) -> u8 {
    let n = b & 0x0F;
    if n < 10 {
        b'0' + n
    } else {
        b'a' + (n - 10)
    }
}

fn push_u32(out: &mut String, n: u32) {
    use std::fmt::Write;
    let _ = write!(out, "{}", n);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Runtime unit tests for the Debugger Contract slice 4 surface
    //! (parent-frame ref + `KaracWaitTarget`).
    //!
    //! **Env-var test isolation.** Two factors complicate
    //! `KARAC_RUNTIME_DEBUG_METADATA` testing:
    //!
    //! 1. Cargo runs tests in parallel, so any test that mutates the env
    //!    var races peers reading it.
    //! 2. `runtime_debug_metadata_enabled` caches its result in a
    //!    `OnceLock<bool>` — once initialized the env-var read never
    //!    repeats, so a test mutating the var after another test has
    //!    triggered initialization observes nothing.
    //!
    //! Resolution: tests serialize on `FRAME_TRACKING_ENV_LOCK`, and the
    //! disabled-path test goes through `runtime_debug_metadata_enabled_uncached`
    //! (test-only re-read that bypasses the cache). This mirrors slice 3's
    //! `SPAWN_SITE_ENV_LOCK` pattern in `tests/codegen.rs`.
    //!
    //! Frame-pointer cross-thread shuttling uses `usize` casts so the
    //! `*const KaracFrame` (which is `!Send`) crosses the thread boundary
    //! as a plain integer; the runtime never relies on Rust's auto-Send
    //! inference for these pointers (the soundness comes from
    //! `thread::scope`'s lifetime guarantee, not Send).
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Barrier, Mutex};

    /// Serializes env-var-touching tests. See module-level comment.
    static FRAME_TRACKING_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// `KaracWaitTarget` v1 layout pin. Single-variant `None` under
    /// `#[repr(C, u8)]` is one byte total; future variants must be
    /// additive (non-breaking). If this assertion fails, slice 5 / FFI
    /// consumers built against the current layout would mis-read frames.
    #[test]
    fn test_wait_target_size_pinned() {
        assert_eq!(std::mem::size_of::<KaracWaitTarget>(), 1);
    }

    /// Outside any `par {}` block, `karac_runtime_get_current_frame()`
    /// returns null. Pins the root-task discriminator for slice 5.
    #[test]
    fn test_current_frame_null_at_root() {
        // Must run on a fresh thread so an earlier test (e.g.
        // `test_par_block_sets_worker_frame`) hasn't left state on this
        // thread's TLS. We can simply check the value on a freshly
        // spawned thread.
        let observed: usize = std::thread::spawn(|| karac_runtime_get_current_frame() as usize)
            .join()
            .unwrap();
        assert_eq!(observed, 0, "root task should observe null current_frame");
    }

    /// Synthesize a `KaracBranch` whose `func` captures the
    /// `karac_runtime_get_current_frame()` value at the moment the
    /// branch runs, then assert the captured frame has the expected
    /// shape (non-null, root parent, correct `spawn_site_id` /
    /// `worker_index`).
    #[test]
    fn test_par_block_sets_worker_frame() {
        let _guard = FRAME_TRACKING_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        // Captured frame fields per branch — `usize` to cross the Send
        // boundary cleanly. `(parent_addr, spawn_site_id, worker_index)`.
        struct Capture {
            slots: Mutex<Vec<Option<(usize, u32, u32)>>>,
        }
        let capture = Arc::new(Capture {
            slots: Mutex::new(vec![None, None]),
        });

        unsafe extern "C" fn branch_fn(ctx: *mut c_void, _cancel: *const AtomicBool) {
            let frame = karac_runtime_get_current_frame();
            assert!(!frame.is_null(), "worker should see non-null frame");
            let f = unsafe { &*frame };
            // ctx is a `*mut (Arc<Capture>, usize)` — index of this branch.
            let payload = unsafe { &*(ctx as *const (Arc<Capture>, usize)) };
            let mut slots = payload.0.slots.lock().unwrap();
            slots[payload.1] = Some((f.parent as usize, f.spawn_site_id, f.worker_index));
        }

        let mut payloads: Vec<(Arc<Capture>, usize)> =
            (0..2).map(|i| (capture.clone(), i)).collect();
        let branches: Vec<KaracBranch> = payloads
            .iter_mut()
            .map(|p| KaracBranch {
                func: branch_fn,
                ctx: p as *mut _ as *mut c_void,
            })
            .collect();

        unsafe {
            karac_par_run(branches.as_ptr(), branches.len(), 42);
        }

        let slots = capture.slots.lock().unwrap();
        let s0 = slots[0].expect("branch 0 captured no frame");
        let s1 = slots[1].expect("branch 1 captured no frame");
        // Both branches see root parent (null); spawn_site_id == 42 from
        // the call above. Worker indices are 0 and 1 in some order
        // (the work-stealing thread pool doesn't guarantee dispatch
        // order matches branch order, so check the set).
        assert_eq!(s0.0, 0, "branch 0 should have null parent");
        assert_eq!(s1.0, 0, "branch 1 should have null parent");
        assert_eq!(s0.1, 42);
        assert_eq!(s1.1, 42);
        let mut indices = [s0.2, s1.2];
        indices.sort();
        assert_eq!(indices, [0, 1]);
    }

    /// Inner par block invoked from inside an outer par block: the inner
    /// workers' `parent` should point at the outer worker's frame, not
    /// null. Pins the structured-concurrency tree shape that slice 5
    /// walks for `list_par_blocks()`.
    #[test]
    fn test_par_block_nested_parent_chain() {
        let _guard = FRAME_TRACKING_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        // Captured: outer-worker-frame address (the parent the inner
        // workers should observe) and the inner workers' captured
        // parents.
        struct Captures {
            outer_frame_addr: Mutex<Option<usize>>,
            inner_parent_addrs: Mutex<Vec<usize>>,
        }
        let captures = Arc::new(Captures {
            outer_frame_addr: Mutex::new(None),
            inner_parent_addrs: Mutex::new(Vec::new()),
        });

        unsafe extern "C" fn inner_branch_fn(ctx: *mut c_void, _cancel: *const AtomicBool) {
            let frame = karac_runtime_get_current_frame();
            assert!(!frame.is_null());
            let f = unsafe { &*frame };
            let cap = unsafe { &*(ctx as *const Arc<Captures>) };
            cap.inner_parent_addrs
                .lock()
                .unwrap()
                .push(f.parent as usize);
        }

        unsafe extern "C" fn outer_branch_fn(ctx: *mut c_void, _cancel: *const AtomicBool) {
            let frame = karac_runtime_get_current_frame();
            assert!(!frame.is_null());
            *((unsafe { &*(ctx as *const Arc<Captures>) }).outer_frame_addr)
                .lock()
                .unwrap() = Some(frame as usize);

            // Inner par block — two branches, both share the outer ctx.
            let cap = unsafe { &*(ctx as *const Arc<Captures>) };
            let inner_payloads: Vec<Arc<Captures>> = vec![cap.clone(), cap.clone()];
            let inner_branches: Vec<KaracBranch> = inner_payloads
                .iter()
                .map(|p| KaracBranch {
                    func: inner_branch_fn,
                    ctx: p as *const _ as *mut c_void,
                })
                .collect();
            unsafe {
                karac_par_run(inner_branches.as_ptr(), inner_branches.len(), 99);
            }
            // Keep payloads alive for the duration of the inner call.
            drop(inner_payloads);
        }

        // One-branch outer par so we get exactly one outer worker frame.
        // (`emit_par_run`'s codegen-side single-stmt skip doesn't apply
        // here — we're calling the runtime directly.)
        let payload = captures.clone();
        let outer_branches = [KaracBranch {
            func: outer_branch_fn,
            ctx: &payload as *const _ as *mut c_void,
        }];
        unsafe {
            karac_par_run(outer_branches.as_ptr(), outer_branches.len(), 7);
        }

        let outer_addr = captures
            .outer_frame_addr
            .lock()
            .unwrap()
            .expect("outer branch never ran");
        let inner_parents = captures.inner_parent_addrs.lock().unwrap().clone();
        assert_eq!(inner_parents.len(), 2);
        for p in &inner_parents {
            assert_eq!(
                *p, outer_addr,
                "inner worker's parent should match outer worker's frame address"
            );
        }
    }

    /// Long-running par block holds workers at a barrier so the main
    /// thread can call `karac_runtime_for_each_active_frame` and observe
    /// the registry mid-run. After the barrier releases and the par
    /// block joins, the registry must be empty again.
    #[test]
    fn test_active_frames_register_during_par() {
        let _guard = FRAME_TRACKING_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        // Three workers all wait on the same barrier (start: workers
        // wait; main thread observes registry; main thread releases).
        let barrier_workers = Arc::new(Barrier::new(4)); // 3 workers + 1 main
        let barrier_done = Arc::new(Barrier::new(4));

        struct Payload {
            start: Arc<Barrier>,
            done: Arc<Barrier>,
        }

        unsafe extern "C" fn branch_fn(ctx: *mut c_void, _cancel: *const AtomicBool) {
            let p = unsafe { &*(ctx as *const Payload) };
            p.start.wait();
            // Hold here until main signals via `done` so the registry
            // observation happens between the two barriers.
            p.done.wait();
        }

        let payloads: Vec<Payload> = (0..3)
            .map(|_| Payload {
                start: barrier_workers.clone(),
                done: barrier_done.clone(),
            })
            .collect();
        let branches: Vec<KaracBranch> = payloads
            .iter()
            .map(|p| KaracBranch {
                func: branch_fn,
                ctx: p as *const _ as *mut c_void,
            })
            .collect();

        // Run the par block on a side thread so this thread can observe
        // `ACTIVE_FRAMES` while it's populated.
        let branches_addr = branches.as_ptr() as usize;
        let count = branches.len();
        // Branches' `func` is fn-pointer (`Send`) and `ctx` points into
        // payloads which live for the test's stack frame; the side
        // thread joins before the test returns.
        let runner = std::thread::spawn(move || {
            // SAFETY: payloads / branches outlive this thread (joined
            // before the test function returns).
            unsafe {
                karac_par_run(branches_addr as *const KaracBranch, count, 11);
            }
        });

        // Wait for all workers to register their frames.
        barrier_workers.wait();

        // Count active frames via the iteration callback.
        struct Counter {
            count: u32,
        }
        unsafe extern "C" fn counter_cb(_frame: *const KaracFrame, ud: *mut c_void) {
            let c = unsafe { &mut *(ud as *mut Counter) };
            c.count += 1;
        }
        let mut counter = Counter { count: 0 };
        unsafe {
            karac_runtime_for_each_active_frame(counter_cb, &mut counter as *mut _ as *mut c_void);
        }
        assert_eq!(
            counter.count, 3,
            "expected 3 active frames during par run, got {}",
            counter.count
        );

        // Release workers and wait for join.
        barrier_done.wait();
        runner.join().unwrap();

        // Registry empty after join.
        let mut after = Counter { count: 0 };
        unsafe {
            karac_runtime_for_each_active_frame(counter_cb, &mut after as *mut _ as *mut c_void);
        }
        assert_eq!(
            after.count, 0,
            "expected empty active-frame registry after par join, got {}",
            after.count
        );

        // Keep payloads alive until here.
        drop(payloads);
    }

    /// `KARAC_RUNTIME_DEBUG_METADATA=0` flips the gate off — workers see
    /// null `current_frame` and `ACTIVE_FRAMES` stays empty. Goes through
    /// the test-only `runtime_debug_metadata_enabled_uncached` path so
    /// the env-var mutation actually takes effect (the production
    /// `OnceLock`-cached helper would freeze whichever value the first
    /// slice-4 test observed).
    #[test]
    fn test_runtime_debug_metadata_disabled_skips_tracking() {
        let _guard = FRAME_TRACKING_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let prior = std::env::var("KARAC_RUNTIME_DEBUG_METADATA").ok();
        std::env::set_var("KARAC_RUNTIME_DEBUG_METADATA", "0");
        let observed = runtime_debug_metadata_enabled_uncached();
        // Restore env var before any further code can observe it.
        match prior {
            Some(v) => std::env::set_var("KARAC_RUNTIME_DEBUG_METADATA", v),
            None => std::env::remove_var("KARAC_RUNTIME_DEBUG_METADATA"),
        }
        assert!(
            !observed,
            "expected runtime_debug_metadata_enabled_uncached() == false when env=0"
        );
    }

    /// `wait_target` is `None` for every v1 frame. Pins the contract —
    /// when Phase 6.3 ships real suspension and starts setting other
    /// variants, this test fails and signals the surface change.
    #[test]
    fn test_wait_target_always_none_in_v1() {
        let _guard = FRAME_TRACKING_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        struct Capture {
            tags: Mutex<Vec<u8>>,
        }
        let capture = Arc::new(Capture {
            tags: Mutex::new(Vec::new()),
        });

        unsafe extern "C" fn branch_fn(ctx: *mut c_void, _cancel: *const AtomicBool) {
            let frame = karac_runtime_get_current_frame();
            assert!(!frame.is_null());
            // Read the discriminant byte directly per the
            // `#[repr(C, u8)]` layout (tag at offset 0 of the
            // `wait_target` field). This is the FFI-stable read path
            // slice 5 / future debuggers will use, so a test that goes
            // through the discriminant byte verifies the same wire
            // shape.
            let f = unsafe { &*frame };
            let tag_byte = unsafe { *(&f.wait_target as *const KaracWaitTarget as *const u8) };
            unsafe { &*(ctx as *const Arc<Capture>) }
                .tags
                .lock()
                .unwrap()
                .push(tag_byte);
        }

        let payload = capture.clone();
        let branches = [
            KaracBranch {
                func: branch_fn,
                ctx: &payload as *const _ as *mut c_void,
            },
            KaracBranch {
                func: branch_fn,
                ctx: &payload as *const _ as *mut c_void,
            },
        ];
        unsafe {
            karac_par_run(branches.as_ptr(), branches.len(), 0);
        }

        let tags = capture.tags.lock().unwrap();
        assert_eq!(tags.len(), 2);
        for t in tags.iter() {
            // `KaracWaitTarget::None` is the only variant; under
            // `#[repr(C, u8)]` it has discriminant 0.
            assert_eq!(*t, 0, "v1 wait_target must always be KaracWaitTarget::None");
        }
    }

    /// `FrameGuard::drop` runs on the unwind path, so a frame is
    /// deregistered from `ACTIVE_FRAMES` even when the body between
    /// guard construction and guard drop panics. Pins the defer-style
    /// teardown against future regression.
    ///
    /// Note: we test `FrameGuard` directly rather than going through
    /// `karac_par_run` because the worker's `func` is `unsafe extern "C"`
    /// and Rust 1.81+ aborts on panics that cross a non-unwinding FFI
    /// boundary — codegen-emitted Kāra branches never panic across the
    /// FFI surface in practice (Kāra has its own panic protocol).
    /// What this test validates is the runtime-internal contract: if
    /// `FrameGuard` is alive and its scope unwinds, the registry is
    /// cleaned up. That's the whole reason for the RAII shape.
    #[test]
    fn test_frame_deregistered_on_panic() {
        let _guard = FRAME_TRACKING_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        struct Counter {
            count: u32,
        }
        unsafe extern "C" fn counter_cb(_frame: *const KaracFrame, ud: *mut c_void) {
            let c = unsafe { &mut *(ud as *mut Counter) };
            c.count += 1;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let frame = KaracFrame {
                parent: std::ptr::null(),
                spawn_site_id: 99,
                worker_index: 0,
                wait_target: KaracWaitTarget::None,
            };
            let _g = FrameGuard::new(&frame);
            // While the guard is alive the registry should hold one
            // entry. Sanity-check before we panic.
            let mut mid = Counter { count: 0 };
            unsafe {
                karac_runtime_for_each_active_frame(counter_cb, &mut mid as *mut _ as *mut c_void);
            }
            assert_eq!(mid.count, 1, "guard alive; registry should hold 1 entry");
            panic!("intentional panic — `FrameGuard::drop` must still fire");
        }));
        assert!(
            result.is_err(),
            "expected panic to bubble out of catch_unwind"
        );

        // After the guard's scope unwinds, the registry is empty.
        let mut after = Counter { count: 0 };
        unsafe {
            karac_runtime_for_each_active_frame(counter_cb, &mut after as *mut _ as *mut c_void);
        }
        assert_eq!(
            after.count, 0,
            "FrameGuard::drop must run on unwind; found {} active after panic",
            after.count
        );
    }
}
