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

mod alloc;
mod bounded_channel;
mod channel;
mod clone;
mod emutls;
#[cfg(feature = "net")]
pub mod event_loop;
mod file;
mod map;
mod mutex;
#[cfg(feature = "net")]
pub mod scheduler;
// Sequential spawn/TaskGroup scheduler — the WASM-default concurrency
// lowering (phase-10 "WASM concurrency lowering — sequential default").
// Compiled under cfg(test) on native too so the queue/join logic is
// unit-testable without a wasm host; its `karac_runtime_*` exports are
// wasm-gated inside the module. Compiled out of the threaded wasm
// archive (`--features wasm-threads`), where the pool-backed
// `wasm_threads_scheduler` below supplies the task surface instead.
#[cfg(any(all(target_family = "wasm", not(feature = "wasm-threads")), test))]
pub mod seq_scheduler;
// Threaded spawn/TaskGroup scheduler — the `--features wasm-threads`
// lowering (phase-10 "WASM concurrency lowering — wasm-threads opt-in"):
// pool-backed on wasm32-wasip1-threads (real std threads over the
// wasi-threads ABI). Compiled under cfg(test) on native too — the
// implementation is plain std-thread code, so the spawn/join/group
// semantics are unit-testable without a wasm host; its
// `karac_runtime_*` exports are wasm+wasm-threads-gated inside the
// module.
#[cfg(feature = "tls")]
pub mod tls;
pub mod tracing;
#[cfg(any(all(target_family = "wasm", feature = "wasm-threads"), test))]
pub mod wasm_threads_scheduler;
// Heap unification with wasi-libc `malloc` — wasm archive only; see the
// module doc for why cross-boundary frees require it.
#[cfg(all(target_family = "wasm", target_os = "wasi"))]
mod wasm_alloc;

// LLJIT integration (phase-7 L560 W3, 2026-05-29): when this crate is
// consumed as an rlib (test-binary `[dev-dependencies]` path under
// `karac`'s `lljit_prototype` feature), rustc's rlib emission may DCE
// the `#[no_mangle] pub extern "C" fn karac_*` symbols even though
// they're attribute-tagged, because nothing else in the binary takes
// their addresses. This helper threads each symbol's address through
// `black_box` so the linker preserves the surrounding objects, making
// every `#[no_mangle]` export resolvable via `dlsym(RTLD_DEFAULT, ...)`
// — which is what the LLJIT process-symbol-search generator uses at
// JIT-link time. Production `staticlib` consumers (the AOT path via
// `link_executable`) never call this — they extract symbols by name
// from the archive directly.
//
// Grow this list when codegen starts emitting declares for new runtime
// symbols not yet covered. Failure mode is loud: a karac test prints
// "Symbols not found: [_karac_<name>]" at lookup time.
#[doc(hidden)]
pub fn __preserve_no_mangle_symbols() -> usize {
    use std::hint::black_box;
    let mut acc: usize = 0;
    // The `black_box(fn as *const () as usize)` pattern forces the
    // compiler to take each function's address at runtime, which the
    // linker then preserves in the test binary's symbol table. Macro
    // is purely line-noise reduction.
    macro_rules! keep {
        ($($f:path),* $(,)?) => {
            $( acc = acc.wrapping_add(black_box($f as *const () as usize)); )*
        };
    }
    // Fallible / panicking allocation wrappers (`runtime/src/alloc.rs`,
    // phase-8-stdlib-floor item 8). Backs the `try_*` collection companions
    // (`karac_alloc_fallible`) and the panicking collection methods
    // (`karac_alloc_or_panic`).
    keep!(alloc::karac_alloc_fallible, alloc::karac_alloc_or_panic,);
    // Map runtime (`runtime/src/map.rs`).
    keep!(
        map::karac_map_new,
        map::karac_map_free,
        map::karac_map_free_with_drop_vec,
        map::karac_map_insert,
        map::karac_map_insert_old,
        map::karac_map_insert_borrowed_str_old,
        map::karac_map_get,
        map::karac_map_remove,
        map::karac_map_remove_old,
        map::karac_map_contains,
        map::karac_map_len,
        map::karac_map_clear,
        map::karac_map_clear_with_drop_vec,
        map::karac_map_iter_new,
        map::karac_map_iter_next,
        map::karac_map_iter_free,
        map::karac_map_entry,
        map::karac_map_lookup_slot,
    );
    // Channel runtime (`runtime/src/channel.rs`). Without these the LLJIT
    // path's `dlsym` symbol-search generator can't resolve `Channel.new()` /
    // `Sender.send` / `Receiver.recv` call sites.
    keep!(
        channel::karac_runtime_channel_new,
        channel::karac_runtime_channel_clone,
        channel::karac_runtime_channel_drop_sender,
        channel::karac_runtime_channel_drop_receiver,
        channel::karac_runtime_channel_send,
        channel::karac_runtime_channel_recv,
        channel::karac_runtime_channel_try_recv,
    );
    // Bounded-channel runtime (`runtime/src/bounded_channel.rs`). Backs
    // `BoundedChannel.new` / `.send` / `.recv` + the `BoundedChannel` Drop.
    keep!(
        bounded_channel::karac_runtime_bounded_channel_new,
        bounded_channel::karac_runtime_bounded_channel_send,
        bounded_channel::karac_runtime_bounded_channel_recv,
        bounded_channel::karac_runtime_bounded_channel_drop,
    );
    // Blocking-mutex slow path (`runtime/src/mutex.rs`). Backs the contended
    // branch of a `lock` block's futex acquire + the wake on its release;
    // the uncontended fast path is inline codegen and never calls these.
    keep!(
        mutex::karac_runtime_mutex_lock,
        mutex::karac_runtime_mutex_unlock_wake,
    );
    // String + comparison runtime (`runtime/src/clone.rs` + this file).
    keep!(
        clone::karac_string_clone,
        clone::karac_string_slice,
        clone::karac_string_slice_borrow,
        clone::karac_string_decode_char,
        clone::karac_string_encode_char,
        karac_string_cmp,
        karac_float_cmp,
        karac_runtime_f64_to_str,
        karac_vec_sort_by,
        karac_vec_reverse,
    );
    // par-block + reduce, error-return trace, test-runner outcome bridge.
    keep!(
        karac_par_run,
        karac_par_reduce,
        karac_error_trace_push,
        karac_error_trace_clear,
        karac_test_record_failure,
    );
    // Ambient built-in resource methods lowered by codegen
    // (`src/codegen/method_call.rs::compile_ambient_resource_method`).
    // Without these the LLJIT path's `dlsym` symbol-search generator
    // can't resolve `env.set` / `clock.now` call sites.
    keep!(
        karac_runtime_env_set,
        karac_runtime_clock_now,
        karac_runtime_rand_next_u64,
        karac_runtime_env_args_into,
        karac_runtime_env_var,
    );
    // Design-by-contract predicate runtime (`requires` / `ensures` /
    // `invariant`). Codegen wraps each predicate evaluation in
    // `karac_runtime_enter_predicate` / `_exit_predicate`, and a
    // violated contract aborts through `emit_panic`, which reads
    // `karac_runtime_panic_prefix` to categorize the fault. Without
    // these in the keep-list the LLJIT `dlsym` generator can't resolve
    // them, so any program with a contract (or one reaching the panic
    // path) fails to materialize `main` under the runner — surfaced by
    // the `KARAC_TEST_JIT` `karac test` audit (contract/invariant E2E
    // programs failed with `Symbols not found: [_karac_runtime_*]`).
    keep!(
        karac_runtime_enter_predicate,
        karac_runtime_exit_predicate,
        karac_runtime_panic_prefix,
    );
    // Emulated-TLS dispatch (LLJIT path; see `runtime/src/emutls.rs`).
    // LLVM-emitted `#[thread_local]` lowering under LLJIT calls
    // `__emutls_get_address`, which compiler-rt provides on platforms
    // that need it but isn't in the karac process. The custom impl
    // here is a `dlsym`-resolvable shim.
    keep!(emutls::__emutls_get_address);
    // Debugger Contract.
    keep!(
        karac_runtime_get_current_frame,
        karac_runtime_for_each_active_frame,
        karac_runtime_has_debug_metadata,
        karac_runtime_list_par_blocks_into,
        // W3.5: JIT helper publishes the JIT-module's SPAWN_SITES
        // addresses so debug-metadata reads see the right values
        // instead of the helper bin's stand-in zeros.
        karac_runtime_init_jit_spawn_sites,
    );
    // Providers (Feature 2 § Provider-Rooted Resources).
    keep!(
        karac_provider_push,
        karac_provider_pop,
        karac_provider_lookup,
        karac_provider_set_stack_head,
        karac_provider_get_stack_head,
    );
    // File runtime (`runtime/src/file.rs`).
    keep!(
        file::karac_runtime_file_open,
        file::karac_runtime_file_create,
        file::karac_runtime_file_append,
        file::karac_runtime_file_read_to_string,
        file::karac_runtime_file_read,
        file::karac_runtime_file_write,
        file::karac_runtime_file_flush,
        file::karac_runtime_file_close,
        file::karac_runtime_file_seek,
        file::karac_runtime_stdin_read_line,
        file::karac_runtime_stdin_read_to_string,
        file::karac_runtime_fs_write,
    );
    // JSON runtime (this file's `runtime_json_*` block).
    keep!(
        karac_runtime_json_parse,
        karac_runtime_json_stringify,
        karac_runtime_json_free_value,
        karac_runtime_json_free_string,
        karac_runtime_json_make_null,
        karac_runtime_json_make_bool,
        karac_runtime_json_make_number,
        karac_runtime_json_make_string,
        karac_runtime_json_alloc_items_buf,
        karac_runtime_json_alloc_keys_buf,
        karac_runtime_json_alloc_key,
        karac_runtime_json_make_array,
        karac_runtime_json_make_object,
    );
    // HTTP server (request/response) runtime.
    keep!(
        karac_runtime_http_response_set_body,
        karac_runtime_http_response_set_header,
        karac_runtime_http_response_set_status,
        karac_runtime_http_request_path,
        karac_runtime_http_request_method,
        karac_runtime_http_request_body_ptr,
        karac_runtime_http_request_body_len,
        karac_runtime_http_request_header,
        karac_runtime_http_request_headers_count,
        karac_runtime_http_request_header_key_at,
        karac_runtime_http_request_header_val_at,
        karac_runtime_http_request_query_count,
        karac_runtime_http_request_query_key_at,
        karac_runtime_http_request_query_val_at,
        karac_runtime_parse_i64,
        karac_runtime_parse_i64_radix,
    );
    // The serve loops themselves need the tokio/hyper substrate (`net`);
    // the request/response accessors above are plain FFI-struct reads and
    // stay in every archive.
    #[cfg(feature = "net")]
    keep!(karac_runtime_serve_http, karac_runtime_serve_http_static,);
    // Scheduler + event loop (pub modules). Gated behind `net` alongside
    // the modules themselves — the wasm archive (`--no-default-features`,
    // phase-10) has no mio/tokio substrate.
    #[cfg(feature = "net")]
    keep!(
        scheduler::karac_runtime_spawn,
        scheduler::karac_runtime_task_join,
        scheduler::karac_runtime_task_handle_free,
        scheduler::karac_runtime_task_state,
        scheduler::karac_runtime_taskgroup_new,
        scheduler::karac_runtime_taskgroup_register,
        scheduler::karac_runtime_taskgroup_join_and_free,
        scheduler::karac_runtime_taskgroup_cancel,
        event_loop::karac_runtime_scheduler_start_dispatcher,
        event_loop::karac_runtime_scheduler_shutdown_dispatcher,
        event_loop::karac_runtime_scheduler_stats_snapshot,
        event_loop::karac_runtime_park_slot_new,
        event_loop::karac_runtime_park_slot_wait,
        event_loop::karac_runtime_park_slot_signal,
        event_loop::karac_runtime_park_slot_free,
        event_loop::karac_runtime_event_loop_poll,
        event_loop::karac_runtime_event_loop_wake,
        event_loop::karac_runtime_event_loop_start_background_thread,
        event_loop::karac_runtime_event_loop_take_wakeups,
        event_loop::karac_runtime_event_loop_shutdown_background_thread,
        event_loop::karac_runtime_event_loop_register_timer,
        event_loop::karac_runtime_event_loop_cancel_timer,
    );
    // Raw-fd registration + TCP + WS — additionally `unix`-gated: these
    // FFI fns are `#[cfg(unix)]` at their definitions (mio's `SourceFd`
    // raw-fd model; Windows IOCP is an explicitly separate slice per the
    // event_loop.rs module docs), so naming them here on Windows is E0425
    // — the windows-latest CI redness back through 2026-06-01.
    #[cfg(all(feature = "net", unix))]
    keep!(
        event_loop::karac_runtime_event_loop_register_fd,
        event_loop::karac_runtime_event_loop_deregister_fd,
        event_loop::karac_runtime_tcp_bind,
        event_loop::karac_runtime_tcp_accept,
        event_loop::karac_runtime_tcp_read,
        event_loop::karac_runtime_tcp_write,
        event_loop::karac_runtime_tcp_close,
        event_loop::karac_runtime_ws_send_text,
        event_loop::karac_runtime_ws_send_binary,
        event_loop::karac_runtime_ws_send_text_masked,
        event_loop::karac_runtime_ws_send_binary_masked,
        event_loop::karac_runtime_ws_recv_text,
        event_loop::karac_runtime_ws_recv_binary,
        event_loop::karac_runtime_ws_accept,
    );
    // TLS surface (gated behind the `tls` feature — see Cargo.toml). The
    // lean (`--no-default-features`) archive compiles these out entirely so
    // the rustls/ring unwind machinery never reaches a compute binary.
    #[cfg(feature = "tls")]
    keep!(
        karac_runtime_http_client_get,
        karac_runtime_http_client_post,
        karac_runtime_serve_https,
        tls::karac_runtime_tls_config_new,
        tls::karac_runtime_tls_config_free,
    );
    // TLS raw-fd surface — `#[cfg(unix)]` at the definitions, same split
    // as the net block above.
    #[cfg(all(feature = "tls", unix))]
    keep!(
        event_loop::karac_runtime_ws_accept_tls,
        tls::karac_runtime_tls_listener_bind,
        tls::karac_runtime_tls_accept,
        tls::karac_runtime_tls_client_connect,
        tls::karac_runtime_tls_read,
        tls::karac_runtime_tls_write,
        tls::karac_runtime_tls_close,
    );
    acc
}

use std::cell::Cell;
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
// Pool-substrate imports — compiled wherever the pool itself is: native
// always, wasm only under `--features wasm-threads` (the phase-10
// threaded opt-in; the wasm *default* lowering is sequential — see
// `seq_par_run` / `seq_scheduler.rs`). On wasm32-wasip1-threads,
// std::thread / Mutex / Condvar are real (pthreads over the
// wasi-threads ABI, futex-backed atomics), so the same substrate
// serves both worlds unchanged.
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
use std::sync::{Arc, Condvar};
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
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
/// Allocated on the pool worker's stack inside `execute_task`, so
/// `*const KaracFrame` pointers are valid for the lifetime of that task's
/// branch invocation. Pointers stored in `ACTIVE_FRAMES` are removed at
/// frame teardown (success or panic, via `FrameGuard`'s `Drop`) before
/// the stack frame deallocates. Pointers stored as a child's `parent`
/// field are safe because `karac_par_run` blocks on the per-call
/// `Condvar` until every dispatched task has decremented `remaining`, so
/// the calling thread's stack frame containing the captured
/// `parent_addr` outlives all dispatched tasks.
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

// ── Contract-predicate context (design.md § Contracts rule 2) ─────────────
//
// A thread-local depth counter tracks whether the current thread is evaluating
// a contract predicate. Codegen's `emit_contract_assert` brackets a predicate's
// evaluation with `karac_runtime_enter_predicate` / `_exit_predicate`, and
// `emit_panic` reads `karac_runtime_panic_prefix` to categorize a fault: a panic
// raised while the depth is non-zero — inline in the predicate OR inside any
// function it transitively calls — is `contract predicate panicked`, distinct
// from `contract violated` (a predicate that simply returns false, evaluated
// with the depth back at 0). A counter rather than a bool keeps nested
// predicates correct: a predicate that calls a contracted function increments a
// second time and the callee's exit only decrements its own level, leaving the
// enclosing predicate's context intact. This subsumes the prior compile-time
// `in_contract_predicate` flag, which could only see panics lexically inside the
// predicate, not cross-call ones — matching the interpreter's global
// `pending_cf` behavior. Thread-local because Kāra tasks run on multiple
// scheduler threads, each evaluating its own predicates independently.
thread_local! {
    static CONTRACT_PREDICATE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Enter a contract-predicate evaluation context (increment the depth).
#[no_mangle]
pub extern "C" fn karac_runtime_enter_predicate() {
    CONTRACT_PREDICATE_DEPTH.with(|d| d.set(d.get().saturating_add(1)));
}

/// Leave a contract-predicate evaluation context (decrement the depth).
/// Saturating so an unbalanced exit can never underflow.
#[no_mangle]
pub extern "C" fn karac_runtime_exit_predicate() {
    CONTRACT_PREDICATE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}

/// The panic-message infix for the current predicate context, as a
/// null-terminated C string: `"contract predicate panicked: "` while a
/// predicate is on the stack, else `""`. Codegen's panic path printfs
/// `panic: %s%s\n` with this as the first `%s`, so the rendered fault is
/// byte-identical to the two historical forms. The returned pointer is to a
/// 'static byte string — never freed, always valid.
#[no_mangle]
pub extern "C" fn karac_runtime_panic_prefix() -> *const std::os::raw::c_char {
    const IN_PREDICATE: &[u8] = b"contract predicate panicked: \0";
    const NOT_IN_PREDICATE: &[u8] = b"\0";
    let in_predicate = CONTRACT_PREDICATE_DEPTH.with(|d| d.get() > 0);
    if in_predicate {
        IN_PREDICATE.as_ptr() as *const std::os::raw::c_char
    } else {
        NOT_IN_PREDICATE.as_ptr() as *const std::os::raw::c_char
    }
}

/// `env.set(name, value)` — POSIX `setenv` shape: overwrites if present,
/// creates otherwise. Codegen counterpart to the interpreter's
/// `("Env", "set")` arm (`src/interpreter/resource_method.rs`); the
/// AOT/JIT path lowers `env.set(..)` to a call here. Args are (ptr, len)
/// pairs for the two Kāra `String`s (UTF-8, never NUL-terminated).
///
/// # Safety
///
/// `name_ptr`/`val_ptr` must each point to `name_len`/`val_len` valid,
/// initialized UTF-8 bytes (the codegen always passes a Kāra `String`'s
/// `{ptr, len}`, which satisfies this). Additionally `std::env::set_var`
/// is only sound when no other thread concurrently reads the environment
/// block; the effect system upholds this by serializing `writes(Env)`
/// against concurrent env reads.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_env_set(
    name_ptr: *const u8,
    name_len: usize,
    val_ptr: *const u8,
    val_len: usize,
) {
    let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
    let val = std::str::from_utf8_unchecked(std::slice::from_raw_parts(val_ptr, val_len));
    std::env::set_var(name, val);
}

/// `clock.now()` — current Unix time in whole seconds. Codegen
/// counterpart to the interpreter's `("Clock", "now")` arm; returns 0 if
/// the system clock predates the Unix epoch (matching the interpreter's
/// `unwrap_or(0)`).
#[no_mangle]
pub extern "C" fn karac_runtime_clock_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `rand.next_u64()` — next draw from a non-cryptographic xorshift64 PRNG.
/// Codegen counterpart to the interpreter's `("RandomSource", "next_u64")`
/// arm. The algorithm (xorshift64) and the seeding strategy (wall-clock
/// nanoseconds `| 1`) match the interpreter's `seed_rand_state` +
/// `rand_state` exactly, so both backends produce statistically equivalent
/// — and, like the interpreter, run-to-run nondeterministic — sequences.
/// State is per-thread (`thread_local!`), so a draw on one worker never
/// perturbs another's stream. The result is reinterpreted as `i64`: a
/// lossless bit-for-bit cast matching the interpreter's `Value::Int(x as
/// i64)` convention.
#[no_mangle]
pub extern "C" fn karac_runtime_rand_next_u64() -> i64 {
    thread_local! {
        static RAND_STATE: std::cell::Cell<u64> = std::cell::Cell::new({
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            // `| 1` guarantees a non-zero seed — xorshift64 is stuck at 0.
            nanos | 1
        });
    }
    RAND_STATE.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        x as i64
    })
}

/// `env.args()` — process argv as a Kāra `Vec[String]`. Codegen
/// counterpart to the interpreter's `("Env", "args")` arm. First
/// aggregate-returning ambient method: the result is written through an
/// out-pointer (the codegen side allocas a `{ptr, i64, i64}` Vec slot and
/// hands its address here), mirroring `karac_runtime_list_par_blocks_into`.
///
/// The Vec's element type is `String` = `RuntimeKaracString`
/// (`{ptr, i64, i64}`, 24 bytes). The element buffer and each String's
/// bytes are heap-allocated via `std::alloc::alloc` in Kāra's `Vec`/`String`
/// shape (`cap == len`), so the codegen scope-exit cleanup frees them like
/// any other Kāra-owned aggregate. An empty argv writes the canonical
/// `{null, 0, 0}` (no allocation), matching `Vec.new()` so cleanup is a
/// no-op. Includes argv[0] (the binary path), matching the interpreter's
/// `std::env::args()` and design.md § Built-in Resources line 2799.
///
/// # Safety
///
/// `out` must point to a writable `{ptr, i64, i64}` slot, which codegen
/// always allocas on the caller's stack before invoking.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_env_args_into(out: *mut KaracVec) {
    if out.is_null() {
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    let count = args.len();
    if count == 0 {
        (*out) = KaracVec {
            data: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        return;
    }

    let elem_size = std::mem::size_of::<RuntimeKaracString>();
    let align = std::mem::align_of::<RuntimeKaracString>();
    let layout =
        std::alloc::Layout::from_size_align(elem_size * count, align).expect("env.args Vec layout");
    let buf = std::alloc::alloc(layout) as *mut RuntimeKaracString;
    if buf.is_null() {
        std::alloc::handle_alloc_error(layout);
    }

    for (i, arg) in args.iter().enumerate() {
        let bytes = arg.as_bytes();
        let s = if bytes.is_empty() {
            RuntimeKaracString {
                data: std::ptr::null_mut(),
                len: 0,
                cap: 0,
            }
        } else {
            let str_layout = std::alloc::Layout::array::<u8>(bytes.len()).unwrap();
            let str_buf = std::alloc::alloc(str_layout);
            if str_buf.is_null() {
                std::alloc::handle_alloc_error(str_layout);
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), str_buf, bytes.len());
            RuntimeKaracString {
                data: str_buf,
                len: bytes.len() as i64,
                cap: bytes.len() as i64,
            }
        };
        std::ptr::write(buf.add(i), s);
    }

    (*out) = KaracVec {
        data: buf as *mut u8,
        len: count as i64,
        cap: count as i64,
    };
}

/// `env.var(name) -> Result[String, VarError]` — read an environment
/// variable. Codegen counterpart to the interpreter's `("Env", "var")` arm.
///
/// The runtime half is intentionally split from the enum construction: it
/// returns `true` and writes a heap `String` (`RuntimeKaracString`, Kāra
/// `{ptr,len,cap}` shape, `cap==len`) into `out_str` when the variable is
/// present and valid UTF-8, and `false` (writing the canonical `{null,0,0}`
/// empty String) otherwise. Codegen then builds `Result.Ok(out_str)` on
/// `true` and `Result.Err(VarError.NotPresent)` on `false`, keeping all
/// Kāra enum-layout knowledge on the codegen side (codegen-containment).
///
/// `std::env::var` returns `Err(NotPresent)` for a missing var and
/// `Err(NotUnicode)` for a non-UTF-8 value; both collapse to the `false`
/// result here, matching the interpreter's collapse to `VarError.NotPresent`
/// (Kāra's strict-UTF-8 `String` cannot carry the offending bytes).
///
/// # Safety
///
/// `name_ptr`/`name_len` must describe a valid UTF-8 byte range (always
/// true of a Kāra `String`), and `out_str` must point to a writable
/// `{ptr, i64, i64}` slot the codegen side allocas before the call.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_env_var(
    name_ptr: *const u8,
    name_len: usize,
    out_str: *mut RuntimeKaracString,
) -> bool {
    let empty = RuntimeKaracString {
        data: std::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    if out_str.is_null() {
        return false;
    }
    let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
    match std::env::var(name) {
        Ok(v) => {
            let bytes = v.as_bytes();
            if bytes.is_empty() {
                // Present but empty — a valid empty Kāra String, still `Ok`.
                (*out_str) = empty;
                return true;
            }
            let str_layout = std::alloc::Layout::array::<u8>(bytes.len()).unwrap();
            let str_buf = std::alloc::alloc(str_layout);
            if str_buf.is_null() {
                std::alloc::handle_alloc_error(str_layout);
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), str_buf, bytes.len());
            (*out_str) = RuntimeKaracString {
                data: str_buf,
                len: bytes.len() as i64,
                cap: bytes.len() as i64,
            };
            true
        }
        Err(_) => {
            (*out_str) = empty;
            false
        }
    }
}

/// Newtype around `*const KaracFrame` that opts into `Send + Sync` for
/// storage in the cross-thread `ACTIVE_FRAMES` registry. Raw pointers are
/// `!Send` by default; the soundness comes from `FrameGuard::drop`
/// removing each entry from the registry before its stack frame
/// deallocates. Iteration via `karac_runtime_for_each_active_frame` is
/// gated on the registry lock to rule out reading-while-deregistering
/// races.
#[derive(Copy, Clone, PartialEq, Eq)]
struct FramePtr(*const KaracFrame);

// SAFETY: see the doc-comment above. The runtime is the only writer to
// `ACTIVE_FRAMES`; pointers are valid by construction (stack-allocated
// inside a pool worker's `execute_task` frame) and removed before
// invalidation.
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

// ── Long-lived worker pool for `karac_par_run` ─────────────────────────────
//
// One global pool of N = `resolve_pool_workers()` worker threads,
// lazy-initialized on the first call to `karac_par_run`. The resolver
// honors `KARAC_PAR_WORKERS=N` for explicit override (down to 1) and
// falls back to `available_parallelism()` floored at 2 when unset. Replaces
// the original per-call `thread::scope` + `s.spawn` impl, which created
// fresh OS threads on every fan-out — diagnosed as the dominant Parallax
// bench bottleneck (60 % of CPU in `mach_vm_protect` setting up pthread
// stack guard pages, 3,344 unique TIDs in 30 s of recording at 1,090 req/s).
// See `docs/investigations/parallax_perf.md § Findings` for the profile
// data and `docs/implementation_checklist/phase-7-codegen.md § "karac_par_
// run: long-lived worker pool"` for the design record.
//
// **Per-call sync.** Each `karac_par_run` invocation allocates one
// `Arc<ParCall>` carrying the cancel flag, remaining-count, and
// completion `Condvar`. Tasks for the call are pushed to the global queue
// and pop'd by free workers; each task decrements `remaining` after
// running and the last task signals `notify`. The caller waits on
// `notify` until `remaining == 0` before returning — same semantics as
// the original `thread::scope` join.
//
// **Soundness for parent-stack pointers.** The original impl relied on
// Rust's `thread::scope` guarantee (parent stack outlives all scope-
// spawned children). The pool impl gives the same guarantee through a
// different mechanism: `karac_par_run` blocks on the per-call Condvar
// until every dispatched task has either run to completion or been
// skipped due to cancel, so the calling thread's stack frame —
// containing the captured `CURRENT_FRAME` pointer that becomes children's
// `parent` field — remains valid for the duration of the call.
//
// **Nested par + work-helping.** A pool worker can call `karac_par_run`
// recursively (e.g., one auto-par fan-out's branch contains another).
// Naively the worker would block on its child call's Condvar; if N
// workers all do this simultaneously the pool deadlocks (no free worker
// can pick up the dispatched child tasks). The wait loop in
// `karac_par_run` therefore work-helps: while waiting for completion it
// pops + executes any task on the queue. This bounds nested-par recursion
// only by stack depth, not by pool size. Cost: one extra queue-lock per
// wait iteration when no help is available — negligible vs the syscall
// cost the pool replaces.
//
// **No graceful shutdown.** Pool workers are pure-compute daemon threads;
// process exit cleans them up. Real shutdown lands when a destructor or
// test-teardown surface needs it.

/// Per-call shared state. One `Arc<ParCall>` per `karac_par_run` invocation;
/// shared between the calling thread and every dispatched task.
///
/// **Reused by `scheduler::karac_runtime_spawn` for fresh-task dispatch.**
/// Each `spawn()` call builds a 1-task `ParCall` (with `remaining = 1`,
/// `track_frames = false`, defaults elsewhere) and pushes one `Task` onto
/// the global `Pool`. The cancel + notify machinery is reused for the
/// spawn-side join wait. See `scheduler.rs` for the dispatch surface.
///
/// The whole pool substrate (ParCall / Task / Pool / workers / the
/// dispatch + wait helpers) compiles wherever a thread pool exists:
/// native always, wasm only under `--features wasm-threads` (phase-10
/// threaded opt-in — wasm32-wasip1-threads has real std threads). The
/// WASM-*default* lowering is sequential (phase-10 "WASM concurrency
/// lowering — sequential default") and the default wasm archive
/// compiles all of this out — see `seq_par_run` and `seq_scheduler.rs`
/// for the sequential surface.
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
pub(crate) struct ParCall {
    pub(crate) cancel: AtomicBool,
    /// Number of tasks not yet completed (decremented by each task on
    /// finish, including skipped-due-to-cancel). Reaches 0 when the call
    /// is done; `notify` is signalled at that point.
    pub(crate) remaining: Mutex<u32>,
    pub(crate) notify: Condvar,
    pub(crate) spawn_site_id: u32,
    /// Calling thread's `CURRENT_FRAME` at the moment of the call,
    /// captured as a raw-pointer-as-`usize` (see soundness note above).
    /// Children's `parent` field points here when `track_frames` is true.
    pub(crate) parent_addr: usize,
    pub(crate) track_frames: bool,
}

/// One unit of work for the pool. The `Arc<ParCall>` shared state plus a
/// `Send` closure carrying everything the work needs to execute. The
/// closure receives the per-call cancel flag by reference so the runtime's
/// frame-tracking + panic-catch wrapper in `execute_task` stays unaware of
/// the closure's payload shape — the same Task struct handles `karac_par_run`'s
/// 2-arg branch-fn invocation and `karac_par_reduce`'s 5-arg worker-fn
/// invocation (slice 3b.7, 2026-05-20). The boxed closure adds one alloc
/// per dispatched task; for the workload sizes the runtime targets
/// (1–18 workers per call), that's negligible vs the thread-scheduling
/// overhead the pool was built to avoid.
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
pub(crate) struct Task {
    pub(crate) call: Arc<ParCall>,
    pub(crate) branch_idx: u32,
    pub(crate) run: Box<dyn FnOnce(&AtomicBool) + Send>,
}

#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
pub(crate) struct Pool {
    pub(crate) queue: Mutex<VecDeque<Task>>,
    pub(crate) cv: Condvar,
}

#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
static POOL: OnceLock<Arc<Pool>> = OnceLock::new();

/// Resolve the auto-par worker count.
///
/// Reads `KARAC_PAR_WORKERS` if set to a positive integer and honors that
/// value exactly (down to 1 — same posture as Rayon's `RAYON_NUM_THREADS`,
/// OpenMP's `OMP_NUM_THREADS`, Go's `GOMAXPROCS`). On a missing or
/// unparseable value, falls back to `available_parallelism()` floored at
/// 2 — the historical default. Floor on the auto-detect path is preserved
/// because the work-helping `dispatch_and_wait` pattern (slice 3b.7) was
/// validated against multi-worker pools; with N=1 it degrades cleanly
/// (single-worker fast path in `karac_par_reduce`, sequential branch
/// execution in `karac_par_run`), so honoring an explicit N=1 from the
/// env is safe.
///
/// Invalid values (0, negative, non-numeric) silently bypass to the
/// auto-detect default — same permissive parse posture as `KARAC_AUTO_PAR`
/// and `KARAC_OPT_LEVEL`. Read on each call rather than cached: pool
/// construction goes through `OnceLock::get_or_init` so it's read once
/// there anyway, and `karac_par_reduce`'s per-call read is cheap libc
/// getenv that lets a user override the count for a single command-line
/// invocation without rebuilding.
// On wasm32-wasip1-threads, `available_parallelism()` is
// `Err(Unsupported)` (no host CPU-count probe in preview1), so the
// auto-detect path bottoms at the `.unwrap_or(4).max(2)` default — the
// env tier is authoritative there: the JS glue injects
// `KARAC_PAR_WORKERS=<navigator.hardwareConcurrency | [wasm] pool-size>`
// through its WASI environ (phase-10 wasm-threads entry).
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
fn resolve_pool_workers() -> usize {
    if let Ok(s) = std::env::var("KARAC_PAR_WORKERS") {
        if let Ok(n) = s.parse::<usize>() {
            if n >= 1 {
                return n;
            }
        }
    }
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(2)
}

#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
pub(crate) fn pool() -> &'static Arc<Pool> {
    POOL.get_or_init(|| {
        let n = resolve_pool_workers();
        let pool = Arc::new(Pool {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
        });
        for _ in 0..n {
            let p = Arc::clone(&pool);
            thread::spawn(move || worker_loop(p));
        }
        pool
    })
}

#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
fn worker_loop(pool: Arc<Pool>) {
    loop {
        let task = {
            let mut q = pool.queue.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if let Some(t) = q.pop_front() {
                    break t;
                }
                q = pool.cv.wait(q).unwrap_or_else(|p| p.into_inner());
            }
        };
        execute_task(task);
    }
}

/// Execute one task: skip if its call has been cancelled, otherwise run
/// the boxed closure under a `FrameGuard` (when frame-tracking is on)
/// and `catch_unwind`. Always decrements `remaining` and signals `notify`
/// on the last task — even on panic, so the caller doesn't hang.
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
fn execute_task(task: Task) {
    let Task {
        call,
        branch_idx,
        run,
    } = task;
    let cancelled = call.cancel.load(Ordering::Relaxed);

    if !cancelled {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if call.track_frames {
                let frame = KaracFrame {
                    parent: call.parent_addr as *const KaracFrame,
                    spawn_site_id: call.spawn_site_id,
                    worker_index: branch_idx,
                    wait_target: KaracWaitTarget::None,
                };
                let _guard = FrameGuard::new(&frame);
                run(&call.cancel);
                // `_guard` drops here, deregistering the frame. On panic
                // the unwind path still runs Drop.
            } else {
                run(&call.cancel);
            }
        }));
        if result.is_err() {
            // Fail-fast: cancel siblings still in the queue.
            call.cancel.store(true, Ordering::Relaxed);
        }
    }

    // Decrement-and-signal happens unconditionally so the caller's
    // wait loop terminates regardless of cancel/panic.
    let last = {
        let mut r = call.remaining.lock().unwrap_or_else(|p| p.into_inner());
        *r -= 1;
        *r == 0
    };
    if last {
        call.notify.notify_all();
    }
}

/// Execute branches concurrently on the global worker pool and join
/// before returning.
///
/// **Pool dispatch**: tasks are pushed onto the global queue; the N
/// long-lived pool workers pop and execute them. The caller blocks on
/// the per-call `Condvar` until every task has decremented `remaining`,
/// work-helping while waiting (see module-level comment) so nested
/// `karac_par_run` calls from pool workers can't deadlock.
///
/// **Fail-fast cancellation**: an internal `AtomicBool` cancel flag is
/// set when any branch panics. Tasks still in the queue are skipped on
/// pickup; tasks already running run to completion (completion-wins at
/// branch granularity). On panic the cancel signal is the only thing
/// that survives — the panic payload is swallowed by `catch_unwind`;
/// caller-visible panic propagation is a deferred follow-up.
///
/// **Frame tracking (Debugger Contract slice 4).** When
/// `runtime_debug_metadata_enabled()` is `true`, each task runs inside a
/// `FrameGuard` that stack-allocates a `KaracFrame { parent,
/// spawn_site_id, worker_index, wait_target: KaracWaitTarget::None }`
/// and registers it in `ACTIVE_FRAMES`. `parent` is captured from the
/// calling thread's `CURRENT_FRAME` at call entry; tasks dispatched into
/// this call carry that pointer as their `parent` field. When the gate
/// is off the function runs without frame allocation or registry
/// bookkeeping.
///
/// **Result collection**: not yet implemented — branches return void.
/// Error propagation via typed results is a Phase 6.2 follow-up.
///
/// # Parameters
///
/// - `branches` / `count`: array of `KaracBranch` descriptors (one per
///   parallel statement in the source `par {}` block).
/// - `spawn_site_id`: identifies the par site for slice 4's `KaracFrame`
///   metadata. Indexes into the slice-3 `KARAC_SPAWN_SITES` table
///   emitted by codegen so slice 5 can join `(file, line, col)`.
///   Ignored when `runtime_debug_metadata_enabled() == false`.
///
/// # Safety
///
/// `branches` must point to `count` valid `KaracBranch` values; each
/// branch's `func` must be a valid function pointer and `ctx` must be a
/// pointer the `func` is prepared to receive. The compiler always
/// satisfies these preconditions.
#[no_mangle]
pub unsafe extern "C" fn karac_par_run(
    branches: *const KaracBranch,
    // `u64`, not `usize`: codegen declares this parameter `i64` (one
    // declaration for every target), and wasm32 traps signature
    // mismatches at the call — a `usize` here is i32-width on the wasm
    // archive and the call site lands on a `signature_mismatch` stub
    // (same class as the `__karac_malloc64` size_t note in
    // `Codegen::new`). ABI-identical on 64-bit native.
    count: u64,
    spawn_site_id: u32,
    parent_cancel: *const AtomicBool,
) {
    let count = count as usize;
    if count == 0 {
        return;
    }

    // phase-10 "WASM concurrency lowering — sequential default": the
    // default wasm target is single-threaded, so there is no pool to
    // dispatch to — run the branches in source order on the calling
    // thread. Cancel / cascade / frame-tracking semantics live in
    // `seq_par_run`. Under `--features wasm-threads` (the phase-10
    // threaded opt-in) the pool exists on wasm too, and the pooled arm
    // below takes over.
    #[cfg(all(target_family = "wasm", not(feature = "wasm-threads")))]
    {
        seq_par_run(branches, count, spawn_site_id, parent_cancel);
    }

    #[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
    {
        karac_par_run_pooled(branches, count, spawn_site_id, parent_cancel);
    }
}

/// Native `karac_par_run` body — pool dispatch + work-helping join.
/// Split out of the extern shell so the wasm sequential path above can
/// swap in without `#[cfg]`-wrapping the whole emission.
///
/// # Safety
///
/// Same contract as [`karac_par_run`].
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
unsafe fn karac_par_run_pooled(
    branches: *const KaracBranch,
    count: usize,
    spawn_site_id: u32,
    parent_cancel: *const AtomicBool,
) {
    let track_frames = runtime_debug_metadata_enabled();
    let parent_addr: usize = if track_frames {
        CURRENT_FRAME.with(|c| c.get()) as usize
    } else {
        0
    };

    let call = Arc::new(ParCall {
        cancel: AtomicBool::new(false),
        remaining: Mutex::new(count as u32),
        notify: Condvar::new(),
        spawn_site_id,
        parent_addr,
        track_frames,
    });

    let p = pool();
    {
        let mut q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
        for i in 0..count {
            let b = &*branches.add(i);
            // Round-trip the pointers through `usize` so the closure is
            // `Send` without an unsafe impl on the raw FFI types. Slice
            // 3b.7 (2026-05-20) refactored `Task` to carry a boxed
            // closure instead of a fixed (func, ctx_addr) pair — same
            // ABI, more flexible payload (par_reduce uses the same Task
            // shape with a 5-arg closure).
            let func = b.func;
            let ctx_addr = b.ctx as usize;
            q.push_back(Task {
                call: Arc::clone(&call),
                branch_idx: i as u32,
                run: Box::new(move |cancel: &AtomicBool| unsafe {
                    func(ctx_addr as *mut c_void, cancel as *const AtomicBool);
                }),
            });
        }
    }
    p.cv.notify_all();

    // Wait for all tasks to complete, work-helping while we wait and
    // propagating an enclosing cancellation inward (see `par_join_wait`).
    par_join_wait(&call, p, parent_cancel);
}

/// Sequential `par {}` execution — the WASM-default lowering (phase-10
/// "WASM concurrency lowering — sequential default"). Runs the branches
/// in source order on the calling thread; the join is implicit (the loop
/// returns only when every branch has run). Semantics preserved from the
/// pooled path, modulo scheduling:
///
/// - **Per-call cancel flag**: every branch receives the same
///   `*const AtomicBool` the pooled dispatch hands out; a branch that
///   fails stores through it (codegen's result-slot/err machinery) and
///   later branches observe it at their compiler-inserted entry +
///   effect-boundary checks — fail-fast at branch granularity, exactly
///   the native contract, with "later" now meaning source order.
/// - **Nested cascade**: an enclosing region's `parent_cancel` is polled
///   before each branch (the sequential analogue of `par_join_wait`'s
///   1 ms poll) and folded into this region's flag.
/// - **Frame tracking**: when `KARAC_RUNTIME_DEBUG_METADATA` is on, each
///   branch runs under a `FrameGuard` carrying the same
///   `(parent, spawn_site_id, worker_index)` triple the pool workers
///   register, so `std.runtime::list_par_blocks()` sees the running
///   branch on this target too.
/// - **Panic**: the wasm release archive builds `panic = "abort"`, so a
///   panicking branch aborts the module — the same observable outcome as
///   the native release archive (whose `catch_unwind` never runs under
///   `panic = "abort"` either).
///
/// Compiled under `cfg(test)` on native as well so the ordering/cascade
/// behavior is unit-testable without a wasm host. Under `--features
/// wasm-threads` the pooled path takes over and this arm is compiled
/// out (the tightened gate keeps the threaded archive's clippy free of
/// dead code).
///
/// # Safety
///
/// Same contract as [`karac_par_run`].
#[cfg(any(all(target_family = "wasm", not(feature = "wasm-threads")), test))]
pub(crate) unsafe fn seq_par_run(
    branches: *const KaracBranch,
    count: usize,
    spawn_site_id: u32,
    parent_cancel: *const AtomicBool,
) {
    let track_frames = runtime_debug_metadata_enabled();
    let parent_addr: usize = if track_frames {
        CURRENT_FRAME.with(|c| c.get()) as usize
    } else {
        0
    };

    let cancel = AtomicBool::new(false);
    for i in 0..count {
        // Cascade an enclosing cancellation inward before each branch.
        if !parent_cancel.is_null() && (*parent_cancel).load(Ordering::Relaxed) {
            cancel.store(true, Ordering::Relaxed);
        }
        let b = &*branches.add(i);
        if track_frames {
            let frame = KaracFrame {
                parent: parent_addr as *const KaracFrame,
                spawn_site_id,
                worker_index: i as u32,
                wait_target: KaracWaitTarget::None,
            };
            let _guard = FrameGuard::new(&frame);
            (b.func)(b.ctx, &cancel as *const AtomicBool);
        } else {
            (b.func)(b.ctx, &cancel as *const AtomicBool);
        }
    }
}

/// Block until `call.remaining` hits zero, opportunistically executing
/// pending pool tasks while waiting so a nested `karac_par_run` from a pool
/// worker can't exhaust the pool / deadlock.
///
/// **Nested cancellation cascade (phase-6 line 475).** When `parent_cancel`
/// is non-null this region is running *inside* an enclosing parallel region
/// (codegen passes the enclosing branch's cancel flag; the top-level call
/// passes null). The loop polls `parent_cancel` and, when the parent has
/// cancelled, flips *this* region's own `cancel`. The nested branches then
/// observe it at their next compiler-inserted effect-boundary check and
/// fail-fast — so an outer cancellation cascades inward through the *same*
/// cooperative mechanism, with no special cross-scope machinery (design.md
/// § Parallel Failure and Cleanup — "Cancellation cascades into nested
/// regions"). Because a busy nested branch in a pure-ish loop may never
/// signal `notify`, the nested wait uses a short `wait_timeout` so the
/// parent flag is observed even while every nested task is still running
/// (otherwise an unbounded inner loop would let an outer cancel hang
/// forever). The top-level call (null parent) keeps the plain blocking
/// `wait` — no polling overhead on the common path. Worst-case cascade
/// latency is the poll cadence plus the inner effect-boundary distance,
/// summed along the nesting path, matching the spec's stated bound.
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
unsafe fn par_join_wait(call: &Arc<ParCall>, p: &Arc<Pool>, parent_cancel: *const AtomicBool) {
    loop {
        // Done?
        {
            let r = call.remaining.lock().unwrap_or_else(|e| e.into_inner());
            if *r == 0 {
                return;
            }
        }
        // Cascade: an enclosing cancellation becomes this region's
        // cancellation, observed by nested branches at their next check.
        if !parent_cancel.is_null() && (*parent_cancel).load(Ordering::Relaxed) {
            call.cancel.store(true, Ordering::Relaxed);
        }
        // Try to help.
        let next_task = {
            let mut q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
            q.pop_front()
        };
        if let Some(task) = next_task {
            execute_task(task);
            continue;
        }
        // Nothing to help with — block until a task we dispatched signals
        // completion. When nested (parent_cancel set), bound the block with
        // a short timeout so we re-poll the parent flag even if no nested
        // task signals in the meantime.
        let r = call.remaining.lock().unwrap_or_else(|e| e.into_inner());
        if *r == 0 {
            return;
        }
        if parent_cancel.is_null() {
            let _r = call.notify.wait(r).unwrap_or_else(|e| e.into_inner());
        } else {
            let _r = call
                .notify
                .wait_timeout(r, std::time::Duration::from_millis(1))
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

// ── Auto-par reduction: karac_par_reduce (slice 2, 2026-05-19) ─────────────
//
// Sibling to `karac_par_run`. Splits a single loop's iteration space across
// N workers, each accumulating into a private slot via the codegen-provided
// `worker_fn`; the runtime then runs a serial combine pass over the slots
// into a caller-owned `out_slot`. The recognizer that surfaces reductions
// lives in `src/concurrency.rs` (slice 1, `LoopReduction`); the codegen
// lowering that calls into this entry point lands as slice 3.
//
// **ABI shape (opaque slot bytes + caller-provided init/combine fn).**
// The runtime treats accumulator slots as `slot_size` bytes at `slot_align`
// alignment with no type knowledge. Typing flows through the three function
// pointers (`init_slot`, `worker_fn`, `combine_fn`) that codegen emits per
// accumulator type. A single `karac_par_reduce` symbol therefore covers
// every op × type combination in the allow-list without ABI growth — adding
// a new (op, type) pair only requires a new codegen path, not a new runtime
// entry point.
//
// **Identity-element discipline.** Reductions need an identity (0 for `+`,
// 1 for `*`, `!0` for `&`, 0 for `|`/`^`). Each worker's slot starts at
// identity (caller calls `init_slot`), then the worker_fn folds its range
// into the slot. The final serial combine seeds `out_slot` at identity and
// folds each worker slot in — so a 0-iter call returns identity, matching
// the source-level semantics of a 0-iter for-loop over the accumulator.
//
// **Dispatch path (slice 3b.7, 2026-05-20): shared `karac_par_run` pool.**
// Earlier (slice 2) each invocation spawned N OS threads via
// `thread::scope` and joined them on return. That worked for one-shot
// reductions but paid per-call thread-creation cost — kata-7 measured
// ~+0.3 MiB peak RSS on top of Rust's serial baseline from worker stack
// reservations alone. The pool-sharing refactor pushes reduction tasks
// onto the same `Pool { queue, cv }` that `karac_par_run` already
// drains via N long-lived workers. The Task struct was generalized to
// carry a `Box<dyn FnOnce(&AtomicBool) + Send>` instead of a fixed
// (func, ctx_addr) pair — the closure captures the (slot, start, end,
// ctx) tuple per worker. `karac_par_reduce` builds an `Arc<ParCall>`
// for the per-call cancel / remaining / notify barrier, pushes
// `n_workers` tasks, then runs the same wait-with-work-help loop as
// `karac_par_run` so nested-par-reduce-inside-par-block can't deadlock
// the pool. Per-call cost drops to one `Box` alloc per task (~hundreds
// of ns total for N=18 workers) vs the prior thread::scope's tens of
// µs spawn cost; peak RSS returns near parity with Rust.

/// Helper for sharing the pool dispatch + work-helping wait loop between
/// `karac_par_run` and `karac_par_reduce`. Pushes `tasks` onto the global
/// pool's queue, notifies workers, then blocks until `call.remaining` hits
/// zero — opportunistically executing pool tasks while waiting so nested
/// par-block-inside-par-block calls from inside the pool can't deadlock.
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
fn dispatch_and_wait(call: &Arc<ParCall>, tasks: Vec<Task>) {
    let p = pool();
    {
        let mut q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
        for t in tasks {
            q.push_back(t);
        }
    }
    p.cv.notify_all();

    loop {
        {
            let r = call.remaining.lock().unwrap_or_else(|e| e.into_inner());
            if *r == 0 {
                return;
            }
        }
        let next_task = {
            let mut q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
            q.pop_front()
        };
        if let Some(task) = next_task {
            execute_task(task);
            continue;
        }
        let r = call.remaining.lock().unwrap_or_else(|e| e.into_inner());
        if *r == 0 {
            return;
        }
        let _r = call.notify.wait(r).unwrap_or_else(|e| e.into_inner());
    }
}

/// FFI descriptor for `karac_par_reduce`. Codegen emits an instance per
/// recognized reduction site; the runtime borrows the descriptor for the
/// duration of one call and never retains pointers across the boundary.
///
/// Field layout is `#[repr(C)]` so the codegen can stamp it directly
/// without needing a Rust-to-LLVM struct adapter.
#[repr(C)]
pub struct KaracReduceDescriptor {
    /// Iteration count — the worker fan-out splits `[0, iter_total)` into
    /// `min(pool_workers, iter_total)` contiguous chunks. When zero, no
    /// workers run and `out_slot` is left at identity.
    ///
    /// `u64`, not `usize` (likewise the three integer fields below and
    /// `worker_fn`'s `start`/`end`): codegen stamps the descriptor with
    /// `i64` fields for every target, and on wasm32 a `usize` here is
    /// i32-width — `#[repr(C)]` would then lay out four narrow fields
    /// against codegen's wide stores, silently corrupting the layout
    /// (worse than the `signature_mismatch` trap that catches mis-width
    /// *parameters*, e.g. `karac_par_run`'s `count`). Unreachable on
    /// wasm until `--features wasm-threads` re-enabled auto-par
    /// reductions there; ABI-identical on 64-bit native.
    pub iter_total: u64,
    /// Bytes per accumulator slot. Must match the size implied by the type
    /// the codegen-emitted `init_slot` / `worker_fn` / `combine_fn` operate on.
    pub slot_size: u64,
    /// Required alignment of an accumulator slot. The runtime aligns each
    /// per-worker slot to this value when carving up the slot buffer.
    pub slot_align: u64,
    /// Write the operator's identity element into `slot`.
    pub init_slot: unsafe extern "C" fn(slot: *mut u8),
    /// Accumulate iterations `[start, end)` into `slot`. The closure
    /// context `ctx` is the same pointer passed at the descriptor level
    /// — every worker receives it verbatim (it's the source-level
    /// closure capture record). `cancel` is the per-call atomic flag;
    /// today no worker is expected to consult it (reductions don't have a
    /// fail-fast story), but it's threaded for future cancellation work.
    /// `start`/`end` are `u64` per the field-width note on `iter_total`
    /// (codegen emits the worker helper with i64 index parameters).
    pub worker_fn: unsafe extern "C" fn(
        slot: *mut u8,
        start: u64,
        end: u64,
        ctx: *mut c_void,
        cancel: *const AtomicBool,
    ),
    /// Fold the partial in `src` into the accumulator at `dst` —
    /// `*dst = *dst <op> *src`. Codegen emits this with the op locked at
    /// compile time, so the runtime never needs to dispatch on op kind.
    pub combine_fn: unsafe extern "C" fn(dst: *mut u8, src: *const u8),
    /// Source-level closure context. Passed verbatim to every `worker_fn`
    /// invocation. May be null if the source loop body captures nothing.
    pub ctx: *mut c_void,
    /// Per-iter body-cost estimate in "1 unit ≈ 1 ns" — same convention
    /// as `src/codegen/reduce.rs::estimate_body_cost_units`. The runtime
    /// uses `iter_total * per_iter_cost_units` to decide whether to
    /// dispatch to the pool or run the worker once in the caller's
    /// thread (slice 3b.8). A value of `0` is a sentinel meaning "no
    /// estimate available — always dispatch"; codegen-emitted
    /// descriptors always set a real estimate (the source-level body's
    /// cost-units walk bottoms at 1, never 0). `u64` per the field-width
    /// note on `iter_total`.
    pub per_iter_cost_units: u64,
}

// SAFETY: The descriptor's pointer fields are exclusively borrowed by the
// runtime for the duration of one karac_par_reduce call (caller guarantees
// validity at call time; runtime joins all workers before returning).
unsafe impl Send for KaracReduceDescriptor {}
unsafe impl Sync for KaracReduceDescriptor {}

/// Per-call dispatch overhead estimate (slice 3b.8), in "1 unit ≈ 1 ns."
/// Mirrors the constant in `src/codegen/reduce.rs` so the codegen-time
/// gate (literal-K loops) and this runtime-time gate (variable-K loops
/// and any literal-K loops the codegen-side gate didn't catch) use the
/// same calibration. Threshold = `pool_workers * this`; when the loop's
/// estimated total work falls below, the runtime skips dispatch and runs
/// the worker once in the caller's thread.
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
const DISPATCH_OVERHEAD_PER_CALL_UNITS_RT: u64 = 10_000;

/// Split a loop's iteration space across N workers; each accumulates a
/// partial into a private slot; the runtime combines the partials into
/// `out_slot` and returns. Sibling to `karac_par_run` — see the
/// section-level comment above for the design discussion.
///
/// **Worker count.** `N = min(available_parallelism, iter_total).max(1)`.
/// When `iter_total == 0`, the runtime initializes `out_slot` to identity
/// and returns immediately. When `N == 1`, the runtime calls `worker_fn`
/// directly into `out_slot` and skips the slot buffer + combine pass.
///
/// **Determinism caveat.** The op the recognizer accepts is
/// associative + commutative, so the per-worker combine order doesn't
/// affect the result — the runtime is free to combine slot 0 + slot 1 +
/// slot 2 + … in any order. Float ops are intentionally *not* in the v1
/// allow-list because IEEE-754 addition is not associative; the
/// recognizer's allow-list and this runtime's combine-order freedom move
/// in lock-step.
///
/// # Safety
///
/// - `descriptor` must point to a valid `KaracReduceDescriptor`.
/// - `out_slot` must point to writable bytes of at least `slot_size`
///   with at least `slot_align` alignment.
/// - The descriptor's function pointers must satisfy the contracts
///   described on each field.
///
/// The compiler always satisfies these preconditions.
#[no_mangle]
pub unsafe extern "C" fn karac_par_reduce(
    descriptor: *const KaracReduceDescriptor,
    out_slot: *mut u8,
    _spawn_site_id: u32,
) {
    let desc = &*descriptor;

    // Seed the output slot at identity so 0-iter calls return identity
    // and the final combine pass can fold every worker slot uniformly.
    (desc.init_slot)(out_slot);

    if desc.iter_total == 0 {
        return;
    }

    // phase-10 "WASM concurrency lowering — sequential default": the
    // single-threaded default wasm target takes the single-worker shape
    // the native fast path below already defines — one `worker_fn` call
    // over the full range, directly into `out_slot`, no slot buffer, no
    // combine (and no pool, which doesn't exist on that target). Codegen
    // additionally skips emitting reduction fan-outs on sequential wasm
    // entirely (auto-par is pure overhead with no parallelism), so this
    // arm is the semantic backstop, not the expected hot path. Under
    // `--features wasm-threads` the pool exists and the pooled arm runs
    // — auto-par reductions are re-enabled there.
    #[cfg(all(target_family = "wasm", not(feature = "wasm-threads")))]
    {
        let dummy = AtomicBool::new(false);
        (desc.worker_fn)(out_slot, 0, desc.iter_total, desc.ctx, &dummy);
    }

    #[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
    karac_par_reduce_pooled(desc, out_slot, _spawn_site_id);
}

/// Native `karac_par_reduce` body — worker fan-out across the pool plus
/// the serial combine. Split out of the extern shell so the wasm
/// sequential arm above can swap in.
///
/// # Safety
///
/// Same contract as [`karac_par_reduce`].
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
unsafe fn karac_par_reduce_pooled(
    desc: &KaracReduceDescriptor,
    out_slot: *mut u8,
    _spawn_site_id: u32,
) {
    // Worker count: cap at `iter_total` so each worker gets at least one
    // iteration, and at least 1 so the single-thread fast path below
    // doesn't divide by zero. `resolve_pool_workers` honors
    // `KARAC_PAR_WORKERS` when set so the dispatch math matches the
    // actual `pool()` worker count — without this, an env override
    // would cap `pool_workers` here at the auto-detect value while
    // `pool()` spawned a different count, and the per-worker slot
    // allocation below would mis-size.
    // Worker count fits `usize` everywhere: it's `min`-capped by
    // `pool_workers` (a small host count), so the `u64 → usize` cast
    // after the min is lossless even on wasm32.
    let pool_workers = resolve_pool_workers();
    let n_workers = (pool_workers as u64).min(desc.iter_total).max(1) as usize;

    // Slice 3b.8 (2026-05-20): runtime-side cost gate. Even when the
    // codegen-time gate let the call through (e.g. variable-K loops
    // bypass `const_eval_iter_count`), the actual K may be too small to
    // beat the per-call dispatch overhead. Compute the estimated total
    // work and run the worker once in the caller's thread when it falls
    // below `pool_workers * DISPATCH_OVERHEAD_PER_CALL_UNITS_RT`. The
    // `per_iter_cost_units == 0` sentinel (caller didn't estimate)
    // bypasses the gate so behaviour stays at "always dispatch."
    let total_cost = desc.iter_total.saturating_mul(desc.per_iter_cost_units);
    let cost_threshold = (pool_workers as u64).saturating_mul(DISPATCH_OVERHEAD_PER_CALL_UNITS_RT);
    let gate_skip = desc.per_iter_cost_units != 0 && total_cost < cost_threshold;

    // Single-worker fast path: bypass the slot buffer + spawn machinery
    // and run the worker directly into `out_slot`. The serial combine
    // would be a no-op anyway (one slot folded into itself) so skipping
    // it preserves observable behavior. Also taken when the runtime
    // cost gate fires (slice 3b.8) — same shape (init_slot is already
    // seeded above; one worker_fn call covers the full range).
    if n_workers == 1 || gate_skip {
        let dummy = AtomicBool::new(false);
        (desc.worker_fn)(out_slot, 0, desc.iter_total, desc.ctx, &dummy);
        return;
    }

    // Allocate the per-worker slot buffer in one chunk so the worker
    // slots share locality and the dealloc on return is a single call.
    // Slot size/align are descriptor-level `u64` (see the field-width
    // note on `iter_total`) but describe an in-memory accumulator, so
    // they always fit `usize` — codegen stamps them from a Kara
    // primitive's size/alignment.
    let slot_size = desc.slot_size as usize;
    let slot_align = desc.slot_align as usize;
    let stride = align_up(slot_size, slot_align);
    let layout = std::alloc::Layout::from_size_align(stride * n_workers, slot_align)
        .expect("karac_par_reduce: slot_size * n_workers overflows or alignment is invalid");
    let slots: *mut u8 = std::alloc::alloc(layout);
    if slots.is_null() {
        std::alloc::handle_alloc_error(layout);
    }

    // Seed every worker's slot at identity. The worker_fn folds into the
    // slot from there; reading an uninitialized slot would surface as
    // miscompile-grade UB.
    for w in 0..n_workers {
        (desc.init_slot)(slots.add(w * stride));
    }

    // Slice 3b.7 (2026-05-20): build per-call coordination state
    // (cancel, remaining, notify Condvar) — mirrors `karac_par_run`'s
    // ParCall shape so the shared `dispatch_and_wait` helper handles
    // both call kinds uniformly. `parent_addr` + `track_frames` stay at
    // their disabled defaults: reductions don't surface in the slice-5
    // frame-tracking API today (the worker fn is a synthesized helper,
    // not a source-level par-branch); they fold in alongside whenever
    // the debugger contract grows a reduction-frame variant.
    // Range math stays in `u64` end-to-end — the per-worker `start`/`end`
    // feed `worker_fn`'s i64-width index parameters directly, no
    // narrowing on wasm32.
    let chunk = desc.iter_total.div_ceil(n_workers as u64);
    let ctx_addr = desc.ctx as usize;
    let slot_base = slots as usize;
    let worker_fn = desc.worker_fn;
    let stride_local = stride;
    let iter_total = desc.iter_total;

    let call = Arc::new(ParCall {
        cancel: AtomicBool::new(false),
        remaining: Mutex::new(n_workers as u32),
        notify: Condvar::new(),
        spawn_site_id: _spawn_site_id,
        parent_addr: 0,
        track_frames: false,
    });

    let tasks: Vec<Task> = (0..n_workers)
        .map(|w| {
            let start = (w as u64) * chunk;
            let end = ((w as u64) + 1).saturating_mul(chunk).min(iter_total);
            let slot_addr = slot_base + w * stride_local;
            Task {
                call: Arc::clone(&call),
                branch_idx: w as u32,
                run: Box::new(move |cancel: &AtomicBool| unsafe {
                    worker_fn(
                        slot_addr as *mut u8,
                        start,
                        end,
                        ctx_addr as *mut c_void,
                        cancel as *const AtomicBool,
                    );
                }),
            }
        })
        .collect();

    dispatch_and_wait(&call, tasks);

    // Serial combine: fold each worker's slot into `out_slot` in worker
    // order. The op is associative + commutative (recognizer's allow-list
    // requirement) so this order is one of many equally-valid orderings.
    for w in 0..n_workers {
        (desc.combine_fn)(out_slot, slots.add(w * stride));
    }

    std::alloc::dealloc(slots, layout);
}

/// Round `n` up to the nearest multiple of `align`. `align` must be a
/// power of two — the caller (`karac_par_reduce` above) gets `align` from
/// the FFI descriptor, where the codegen guarantees `align` is the
/// natural alignment of a Kara primitive type (1, 2, 4, or 8).
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
#[inline]
fn align_up(n: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two(), "align must be a power of two");
    (n + align - 1) & !(align - 1)
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

// ── Debugger Contract — `std.runtime` introspection (slice 5) ──────────────
//
// Item (4) of the four-piece contract per `design.md § Debugger Contract`.
// Materializes slice 3's `KARAC_SPAWN_SITES` LLVM globals + slice 4's
// `ACTIVE_FRAMES` registry as Kāra-callable APIs through the
// `Runtime.has_debug_metadata()` / `Runtime.list_par_blocks()` /
// `Runtime.list_tasks()` surface declared in `runtime/stdlib/runtime.kara`.
//
// **Linkage choice (cross-checked against `cat rust-toolchain.toml`).**
// The slice plan flagged a fork between `#[linkage = "extern_weak"]`
// (nightly-only via `#![feature(linkage)]`) and strong linkage on stable
// Rust. The project pins stable Rust (no `rust-toolchain.toml`; cargo
// 1.95.0 stable), so this section takes the **strong-linkage** path:
// slice 3's `emit_spawn_sites_metadata` always emits the globals (even
// the gate-off form ships `LEN = 0`, `ENABLED = false`, empty array), so
// extern declarations without `#[linkage]` resolve at link time on every
// karac binary. Hard-stop trigger 1 is satisfied: weak linkage is only
// needed when some build path skips the emission, which slice 3 never
// does.
//
// **Vec materialization (sub-step f, hard-stop trigger 3).** Slice 5 takes
// the runtime-side full Vec materialization path: `karac_runtime_list_par_blocks_into`
// allocates the `Vec[ParBlockInfo]` element buffer, populates each entry
// (including per-entry String allocation for the file-path field), and
// writes the final `{data, len, cap}` Vec descriptor into a slot the
// codegen alloca'd. Trade-off: the runtime carries Kāra Vec + String
// layout knowledge (already present from `clone.rs::karac_string_clone`)
// and the compiler-side ParBlockInfo struct layout (matched via `#[repr(C)]`
// with explicit padding). Codegen-side complexity drops from ~80 lines of
// inline-IR loop to a single call + load. The alternative (codegen emits
// the iteration + per-entry String clone in inline IR) is the
// plan-recommended path; slice 5 deviates because the Kāra-side `String`
// allocation surface for inline-IR construction (hard-stop trigger 4) is
// not directly exposed at the relevant abstraction level.

// ── JIT spawn-sites address override (W3.5, 2026-05-30) ─────────────
//
// Under AOT, codegen emits `KARAC_SPAWN_SITES*` globals into the user
// binary, the runtime's externs below resolve to those at link time,
// and reads of `KARAC_SPAWN_SITES_ENABLED` / etc. return the values
// the running program intends.
//
// Under JIT subprocess (`karac_jit_runner`), the runtime's externs
// resolve to the helper bin's stand-ins (necessarily `ENABLED = 0`,
// `LEN = 0`) because they're bound at link time of the helper bin —
// before any JIT module is loaded. The JIT module's emitted globals
// live inside the JITDylib's symbol table; reads in the helper bin's
// Rust code can't see them.
//
// Fix: the JIT helper, after adding the IR module to the engine,
// looks up the three symbols' addresses via `LLJITEngine::lookup_address`
// and calls `karac_runtime_init_jit_spawn_sites` to publish them.
// The runtime's two read sites (`karac_runtime_has_debug_metadata`,
// `karac_runtime_list_par_blocks_into`) check this override first
// and fall back to the externs when it's unset — so AOT pays nothing
// at the cost of one branch + load per call.
//
// Send/Sync wrapper is required because raw pointers aren't Send by
// default; the pointers we store are owned by the JIT engine (lives
// at least as long as the helper bin, which is the only process that
// ever sets these), so the unsafe Send/Sync impl is sound.
struct SpawnSitesAddrPtrs {
    enabled: *const u8,
    len: *const u32,
    base: *const KaracSpawnSiteEntry,
}

unsafe impl Send for SpawnSitesAddrPtrs {}
unsafe impl Sync for SpawnSitesAddrPtrs {}

static SPAWN_SITES_OVERRIDE: std::sync::OnceLock<SpawnSitesAddrPtrs> = std::sync::OnceLock::new();

/// Publish the JIT-resolved addresses of `KARAC_SPAWN_SITES_ENABLED`,
/// `KARAC_SPAWN_SITES_LEN`, and the `KARAC_SPAWN_SITES` array base
/// into the runtime so subsequent introspection reads see the
/// JIT-module values instead of the helper bin's stand-ins.
///
/// Called exactly once by the JIT helper between `add_ir_module` and
/// the JIT'd `main`. Idempotent at the OnceLock layer — subsequent
/// calls are no-ops (the first set wins). AOT callers never invoke
/// this; their reads fall through to the externs.
///
/// # Safety
/// Each non-null pointer must point to a live storage cell of the
/// correct type for the lifetime of the calling process. Passing null
/// for a slot means "keep falling back to the extern" for that field.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_init_jit_spawn_sites(
    enabled: *const u8,
    len: *const u32,
    base: *const u8,
) {
    let _ = SPAWN_SITES_OVERRIDE.set(SpawnSitesAddrPtrs {
        enabled,
        len,
        // The base is a `[N x SpawnSiteEntry]` array global per slice
        // 3's codegen; reinterpret the opaque `*const u8` as a typed
        // pointer for the indexing arithmetic in
        // `karac_runtime_list_par_blocks_into`. The setter's `*const u8`
        // signature is a caller-side convenience — JIT helpers can
        // hand us an `LLVMOrcExecutorAddress`-as-u64 cast to pointer
        // without pulling in `KaracSpawnSiteEntry`'s layout.
        base: base as *const KaracSpawnSiteEntry,
    });
}

// Strong-linkage extern declarations of slice 3's globals. Gated on
// `#[cfg(not(test))]` so the runtime crate's own unit tests can provide
// stand-in definitions (see the `#[cfg(test)]` block at the bottom of
// this file) — codegen-emitted globals only enter the link in real karac
// builds, never in the runtime crate's standalone test binary.
#[cfg(not(test))]
extern "C" {
    /// Slice 3 emits `KARAC_SPAWN_SITES_ENABLED` as an LLVM `i1`
    /// (booltype) global. On every supported target the `i1` lowers to
    /// a 1-byte storage cell (the LLVM data layout's `i1` alignment is
    /// 1, and the value-bit lives in the low bit), so reading it
    /// through a `u8` extern static is the stable way to recover the
    /// boolean: any non-zero low bit means `true`.
    static KARAC_SPAWN_SITES_ENABLED: u8;
    /// Slice 3 emits this as an `i32` global; row count of the
    /// `KARAC_SPAWN_SITES` array (`0` when the gate is off).
    static KARAC_SPAWN_SITES_LEN: u32;
    /// Slice 3 emits this as a `[N x SpawnSiteEntry]` array global.
    /// `KaracSpawnSiteEntry` below mirrors the LLVM struct layout
    /// `{ i32 id, ptr file_cstr, i32 line, i32 col, i32 worker_count, i32 reserved }`.
    static KARAC_SPAWN_SITES: KaracSpawnSiteEntry;
}

/// One row of slice 3's `KARAC_SPAWN_SITES` LLVM array. The layout must
/// match `Codegen::emit_spawn_sites_metadata`'s
/// `{ i32 id, ptr file_cstr, i32 line, i32 col, i32 worker_count, i32 reserved }`
/// struct exactly: `#[repr(C)]` + 8-byte alignment for the `file_cstr`
/// pointer puts a 4-byte gap after `id` and a 4-byte gap after
/// `_reserved`, total 32 bytes per entry. `mem::size_of` /
/// `mem::offset_of` are pinned in `tests::test_spawn_site_entry_layout_pinned`
/// so any future codegen-side rearrangement triggers a runtime-test
/// failure rather than a silent ABI break.
#[repr(C)]
struct KaracSpawnSiteEntry {
    id: u32,
    _pad0: u32, // alignment padding before pointer
    file_cstr: *const std::os::raw::c_char,
    line: u32,
    col: u32,
    worker_count: u32,
    _reserved: u32,
}

/// Layout-compatible view of a Kāra `String` value `{ ptr data, i64 len, i64 cap }`.
/// Mirrors `clone.rs::KaracString` — duplicated here rather than imported
/// because `clone.rs` defines it with crate-private visibility for the
/// `karac_string_clone` symbol; lifting it to a shared module is a
/// post-slice-5 refactor.
///
/// `pub` because it now names the out-pointer parameter of the
/// `karac_runtime_env_var` extern fn (L646 slice 3a) — same FFI-ABI rationale
/// as the public `KaracVec` above.
#[repr(C)]
pub struct RuntimeKaracString {
    pub data: *mut u8,
    pub len: i64,
    pub cap: i64,
}

/// Layout-compatible view of a Kāra `Vec[T]` value `{ ptr data, i64 len, i64 cap }`.
/// Element type is opaque at this level — the slice 5 `_into` writers
/// allocate `count * size_of::<KaracParBlockInfo>()` bytes and stride by
/// the same element size when filling.
///
/// Public so the `karac_runtime_list_par_blocks_into` extern fn can name
/// the type in its parameter list. Field semantics match Kāra's `Vec[T]`
/// codegen — `data` is heap-allocated (`std::alloc::alloc` here, freed at
/// scope exit by user-side codegen), `len` / `cap` are i64 element counts.
#[repr(C)]
pub struct KaracVec {
    pub data: *mut u8,
    pub len: i64,
    pub cap: i64,
}

/// Layout-compatible view of the Kāra `ParBlockInfo` struct declared in
/// `runtime/stdlib/runtime.kara`:
///
/// ```text
/// pub struct ParBlockInfo {
///     spawn_site_id: u32,
///     file: String,        // {ptr, i64 len, i64 cap}
///     line: u32,
///     col: u32,
///     worker_count: u32,
/// }
/// ```
///
/// LLVM's natural layout for `{ i32, {ptr, i64, i64}, i32, i32, i32 }`
/// on 64-bit targets:
///
///   - offset 0..4:   spawn_site_id (i32)
///   - offset 4..8:   padding (alignment to 8 for the inner String)
///   - offset 8..32:  file (24 bytes)
///   - offset 32..36: line (i32)
///   - offset 36..40: col (i32)
///   - offset 40..44: worker_count (i32)
///   - offset 44..48: trailing padding (struct alignment 8)
///   - total size:    48 bytes
///
/// Rust's `#[repr(C)]` produces the identical layout because the field
/// order, alignments, and trailing-padding rules match LLVM's
/// `target-data-layout`-driven defaults on every supported target. The
/// `_pad0` / `_pad1` fields are explicit so the layout reads identically
/// to the LLVM struct in source — `tests::test_par_block_info_layout_pinned`
/// asserts size and field offsets at runtime.
#[repr(C)]
struct KaracParBlockInfo {
    spawn_site_id: u32,
    _pad0: u32,
    file: RuntimeKaracString,
    line: u32,
    col: u32,
    worker_count: u32,
    _pad1: u32,
}

/// Slice 5 of the Debugger Contract — public extern reading
/// `KARAC_SPAWN_SITES_ENABLED` from the binary's LLVM globals.
/// `runtime/stdlib/runtime.kara`'s `Runtime.has_debug_metadata()`
/// `#[compiler_builtin]` shim dispatches to this through codegen.
///
/// Slice 3 always emits the symbol (gate-off form is `0`), so the read
/// is unconditionally safe under strong linkage.
#[no_mangle]
pub extern "C" fn karac_runtime_has_debug_metadata() -> bool {
    // JIT override path (W3.5): if a JIT helper published its
    // module's `KARAC_SPAWN_SITES_ENABLED` address before calling
    // `main`, read from that — otherwise fall through to the extern.
    if let Some(over) = SPAWN_SITES_OVERRIDE.get() {
        if !over.enabled.is_null() {
            // SAFETY: `enabled` points to a live `u8` storage cell
            // for the lifetime of the calling process (the JIT
            // helper guarantees this — see `karac_runtime_init_jit_spawn_sites`).
            return unsafe { *over.enabled } != 0;
        }
    }
    // SAFETY: KARAC_SPAWN_SITES_ENABLED is always emitted by codegen
    // (slice 3, `c6d8b44`) — even the gate-off form ships the symbol
    // with value 0. Strong linkage resolves the address at link time;
    // the load is a single byte read. The `i1` LLVM type lowers to
    // 1-byte storage with the boolean value in the low bit, so any
    // non-zero byte means `true`.
    //
    // The `unsafe` block is required only in non-test builds where the
    // symbol resolves through an `extern "C"` decl; in test builds the
    // stand-in is a regular Rust `static u8` and the `unsafe` would be
    // unnecessary, so we cfg-gate accordingly.
    #[cfg(not(test))]
    {
        unsafe { KARAC_SPAWN_SITES_ENABLED != 0 }
    }
    #[cfg(test)]
    {
        KARAC_SPAWN_SITES_ENABLED != 0
    }
}

/// Build a Kāra `Vec[ParBlockInfo]` snapshot of currently-active
/// `par {}` blocks across all OS threads. Writes the resulting
/// `{data, len, cap}` Vec descriptor into `*out`.
///
/// Joins slice 4's `ACTIVE_FRAMES` registry against slice 3's
/// `KARAC_SPAWN_SITES` table: each active `KaracFrame::spawn_site_id`
/// indexes into `KARAC_SPAWN_SITES[id]` to look up `(file, line, col,
/// worker_count)`. The lookup is bounds-checked — frames whose id is
/// out-of-range (which would indicate a metadata mismatch between
/// runtime and codegen) are skipped rather than panicking, on the
/// "introspection should never crash the program" principle.
///
/// **Iteration holds the registry lock.** `karac_runtime_for_each_active_frame`'s
/// callback API is reused so that frame-pointer dereferences happen
/// while the lock is held — slice-4-style soundness for the `*const
/// KaracFrame` reads. The two-call snapshot race the slice plan worried
/// about (`_count` then `_fill`) is avoided entirely because we go from
/// active-frames → final Vec in a single function call.
///
/// Allocates two heap regions: the element buffer
/// (`count * size_of::<KaracParBlockInfo>()` bytes via `std::alloc::alloc`,
/// the same allocator the rest of the runtime uses) and one
/// `RuntimeKaracString` heap copy per entry's file path (also via
/// `std::alloc::alloc`). Empty result (`count == 0` or
/// `runtime_debug_metadata_enabled()` is false) writes `{null, 0, 0}` —
/// no allocation, matching Kāra's `Vec.new()` convention so scope-exit
/// cleanup is a no-op.
///
/// # Safety
///
/// `out` must point to a writable `{ptr, i64, i64}` slot. Codegen
/// always allocas this on the caller's stack before invoking. The
/// returned Vec's `cap` matches `len`, so when scope-exit cleanup
/// `free`s the buffer it sees a complete Kāra-shape allocation.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_list_par_blocks_into(out: *mut KaracVec) {
    if out.is_null() {
        return;
    }
    // Empty fast path: gate off, or no active frames. Either way write
    // the canonical empty `{null, 0, 0}` Vec.
    if !runtime_debug_metadata_enabled() {
        (*out) = KaracVec {
            data: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        return;
    }

    // Snapshot active frames under the lock; copy out the (id, parent,
    // worker_index) triples so we can release the lock before doing
    // String allocations.
    struct FrameSnapshot {
        spawn_site_id: u32,
    }
    let frames: Vec<FrameSnapshot> = {
        let guard = ACTIVE_FRAMES.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .iter()
            .map(|fp| FrameSnapshot {
                // SAFETY: pointers in ACTIVE_FRAMES are valid while the
                // lock is held — `FrameGuard::drop` deregisters before
                // the stack frame deallocates, and we read the field
                // through the lock.
                spawn_site_id: (*fp.0).spawn_site_id,
            })
            .collect()
    };

    let count = frames.len();
    if count == 0 {
        (*out) = KaracVec {
            data: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        return;
    }

    // Slice 3's KARAC_SPAWN_SITES table — bounds-check each spawn_site_id
    // against KARAC_SPAWN_SITES_LEN before indexing. Address cast goes
    // through a `*const ()` intermediate so the test-mode stand-in type
    // (`SpawnSiteEntryStandIn`, a `#[repr(transparent)]` wrapper around
    // `KaracSpawnSiteEntry`) and the production extern type both lower
    // to a raw byte address.
    //
    // W3.5: prefer the JIT-published override addresses when set;
    // otherwise fall through to the extern (AOT path).
    let (sites_len, sites_base): (usize, *const KaracSpawnSiteEntry) =
        if let Some(over) = SPAWN_SITES_OVERRIDE.get() {
            if !over.len.is_null() && !over.base.is_null() {
                // SAFETY: pointers are live for the calling process's
                // lifetime per the JIT helper's contract.
                (unsafe { *over.len } as usize, over.base)
            } else {
                (
                    KARAC_SPAWN_SITES_LEN as usize,
                    &KARAC_SPAWN_SITES as *const _ as *const () as *const KaracSpawnSiteEntry,
                )
            }
        } else {
            (
                KARAC_SPAWN_SITES_LEN as usize,
                &KARAC_SPAWN_SITES as *const _ as *const () as *const KaracSpawnSiteEntry,
            )
        };

    let elem_size = std::mem::size_of::<KaracParBlockInfo>();
    let layout = std::alloc::Layout::from_size_align(elem_size * count, 8)
        .expect("ParBlockInfo array layout");
    let buf = std::alloc::alloc(layout) as *mut KaracParBlockInfo;
    if buf.is_null() {
        std::alloc::handle_alloc_error(layout);
    }

    let mut filled: usize = 0;
    for snap in &frames {
        let id = snap.spawn_site_id as usize;
        let (file_str, line, col, worker_count) = if id < sites_len {
            let entry = &*sites_base.add(id);
            let file = if entry.file_cstr.is_null() {
                RuntimeKaracString {
                    data: std::ptr::null_mut(),
                    len: 0,
                    cap: 0,
                }
            } else {
                let cstr = std::ffi::CStr::from_ptr(entry.file_cstr);
                let bytes = cstr.to_bytes();
                if bytes.is_empty() {
                    RuntimeKaracString {
                        data: std::ptr::null_mut(),
                        len: 0,
                        cap: 0,
                    }
                } else {
                    let str_layout = std::alloc::Layout::array::<u8>(bytes.len()).unwrap();
                    let str_buf = std::alloc::alloc(str_layout);
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), str_buf, bytes.len());
                    RuntimeKaracString {
                        data: str_buf,
                        len: bytes.len() as i64,
                        cap: bytes.len() as i64,
                    }
                }
            };
            (file, entry.line, entry.col, entry.worker_count)
        } else {
            // Spawn-site ID out of range — metadata mismatch (e.g. table
            // emitted with gate off). Skip rather than crash.
            continue;
        };

        let entry_ptr = buf.add(filled);
        std::ptr::write(
            entry_ptr,
            KaracParBlockInfo {
                spawn_site_id: snap.spawn_site_id,
                _pad0: 0,
                file: file_str,
                line,
                col,
                worker_count,
                _pad1: 0,
            },
        );
        filled += 1;
    }

    (*out) = KaracVec {
        data: buf as *mut u8,
        len: filled as i64,
        cap: count as i64,
    };
}

// ── Provider stack (`with_provider[R]` trait-method dispatch) ──────────────
//
// Per-task linked list of `(resource_id, provider_data, vtable)` cells that
// `R.method(args)` dispatch walks innermost-first. Mirrors the interpreter's
// `eval_resource_method` semantics (src/interpreter.rs:7146) and the
// `design.md § Provider-Rooted Resources` ("Resource call desugaring",
// "Runtime mechanics", "with_provider and parameterized resources")
// paragraphs.
//
// **TLS-backed head pointer.** The slice plan recommended carrying the head
// pointer in `KaracFrame` to avoid `thread_local!` overhead, but root tasks
// (no par-block) have no `KaracFrame` — `karac_par_run` is the only site
// that allocates one. A thread-local works uniformly for root and spawned
// tasks; the per-`R.method()` cost is one TLS read, well within the cost
// model `design.md` already names ("thin Arc deref + one vtable
// indirection"). Cross-task inheritance (par-block branches): the env-struct
// emitted by codegen carries a `provider_stack_head` snapshot from the
// calling thread; each worker calls `karac_provider_set_stack_head` from
// the branch fn prologue to seed its TLS.
//
// **Frame ownership.** `ProviderFrame` storage is alloca'd by codegen at
// each `with_provider[R](p, ||body)` site; `karac_provider_push` populates
// the frame in-place and links it as the new head. `karac_provider_pop`
// unlinks the head (without deallocating — codegen owns the alloca). This
// matches the structured-concurrency invariant: every push has a matching
// pop on the same thread, balanced across normal and unwind paths.

/// FFI-safe handle to a trait vtable. Opaque from the runtime's
/// perspective — the runtime walks `vtable_ptr` only as far as following
/// the indirection; codegen generates the vtable layout (array of fn
/// pointers in trait-method-declaration order) and emits the indirect
/// call inline.
#[repr(C)]
pub struct VTable {
    _private: [u8; 0],
}

/// One entry in the per-task provider stack. Codegen alloca's storage for
/// these at each `with_provider[R](...)` site; `karac_provider_push`
/// populates them in-place.
///
/// `prev` chains to the previous head (innermost-first lookup); `null` for
/// the bottom frame. `resource_id` is the codegen-assigned u32 for the
/// resource trait `R`. `provider_data_ptr` is an opaque pointer to the
/// provider value's payload (codegen knows the layout); `vtable_ptr` is
/// the static vtable for `Provider's-impl-of-R::Provider`.
#[repr(C)]
pub struct ProviderFrame {
    pub prev: *const ProviderFrame,
    pub resource_id: u32,
    pub provider_data_ptr: *const u8,
    pub vtable_ptr: *const VTable,
}

// SAFETY: ProviderFrame stores raw pointers but the per-thread invariant
// (push/pop balanced on the same thread, frame storage alloca'd in the
// caller's stack frame) means cross-thread sharing never happens through
// `PROVIDER_STACK_HEAD` directly. The env-struct snapshot mechanism
// (`karac_provider_set_stack_head`) is the only cross-thread transfer and
// it copies the head pointer at branch entry — not a shared cell.
unsafe impl Send for ProviderFrame {}
unsafe impl Sync for ProviderFrame {}

// Per-thread current-head pointer. `Cell` over `*const ProviderFrame` —
// see the slice-4 `CURRENT_FRAME` comment block for the TLS-during-atexit
// rationale; this surface is read only inside live Kāra code, never from
// `atexit`.
thread_local! {
    static PROVIDER_STACK_HEAD: Cell<*const ProviderFrame> = const { Cell::new(ptr::null()) };
}

/// FFI return type for `karac_provider_lookup`. Two-pointer struct so the
/// caller can branch on `data.is_null()` for the "no binding" panic path
/// without needing a separate boolean. `#[repr(C)]` pins the layout.
#[repr(C)]
pub struct ProviderLookupResult {
    pub data: *const u8,
    pub vtable: *const VTable,
}

/// Push `frame` onto the per-task provider stack. Caller (codegen) supplies
/// `frame` storage (typically an alloca'd `ProviderFrame`) so the runtime
/// doesn't allocate. Populates `frame` in-place with `prev = current_head,
/// resource_id, provider_data, vtable`, then sets the per-task head pointer
/// to `frame`.
///
/// # Safety
///
/// `frame` must point to writable `ProviderFrame` storage that outlives
/// the matching `karac_provider_pop()` call. Codegen alloca's the storage
/// inside the same function frame as the `with_provider` body, so this is
/// satisfied by construction. `provider_data` and `vtable` must remain
/// valid for the duration of the push/pop window (provider value alive,
/// vtable is a static global).
#[no_mangle]
pub unsafe extern "C" fn karac_provider_push(
    frame: *mut ProviderFrame,
    resource_id: u32,
    provider_data: *const u8,
    vtable: *const VTable,
) {
    let prev = PROVIDER_STACK_HEAD.with(|c| c.get());
    *frame = ProviderFrame {
        prev,
        resource_id,
        provider_data_ptr: provider_data,
        vtable_ptr: vtable,
    };
    PROVIDER_STACK_HEAD.with(|c| c.set(frame));
}

/// Pop the current head frame from the per-task provider stack, reverting
/// the head pointer to the `prev` link. The frame's storage is owned by
/// the caller (codegen alloca) — the runtime only updates the head pointer.
/// No-op if the stack is already empty (defensive against double-pop on
/// unwind paths, though codegen should never emit that shape).
#[no_mangle]
pub extern "C" fn karac_provider_pop() {
    PROVIDER_STACK_HEAD.with(|c| {
        let head = c.get();
        if !head.is_null() {
            // SAFETY: head is a valid ProviderFrame (alive until matching
            // pop, per the push contract); reading `.prev` is safe.
            let prev = unsafe { (*head).prev };
            c.set(prev);
        }
    });
}

/// Walk the per-task provider stack innermost-first, returning the first
/// frame whose `resource_id` matches the requested ID. Returns
/// `(null, null)` on miss; codegen emits the structured-panic call inline
/// per `design.md:7084-7095` ("Resource call: no provider bound...").
#[no_mangle]
pub extern "C" fn karac_provider_lookup(resource_id: u32) -> ProviderLookupResult {
    let mut cursor = PROVIDER_STACK_HEAD.with(|c| c.get());
    while !cursor.is_null() {
        // SAFETY: cursor was either the live head pointer or a `prev` link
        // from a live frame; both are valid for the duration of the lookup
        // because frames don't deallocate until matching pops on the same
        // thread.
        let frame = unsafe { &*cursor };
        if frame.resource_id == resource_id {
            return ProviderLookupResult {
                data: frame.provider_data_ptr,
                vtable: frame.vtable_ptr,
            };
        }
        cursor = frame.prev;
    }
    ProviderLookupResult {
        data: ptr::null(),
        vtable: ptr::null(),
    }
}

/// Set the per-task provider stack head to `head`. Used by par-block worker
/// branches at branch-fn prologue to inherit the parent thread's stack.
/// Codegen captures `karac_provider_get_stack_head()` into the env-struct
/// at par-block entry, then each worker calls this with the captured value
/// before executing the branch body.
///
/// # Safety
///
/// `head` must point to a `ProviderFrame` whose lifetime spans the entire
/// par-block (it's the parent's frame, which lives until `karac_par_run`
/// returns, which lives until all branches join — so the lifetime is
/// satisfied by `karac_par_run`'s per-call Condvar wait, which holds the
/// caller frame open until every dispatched task has decremented
/// `remaining`).
#[no_mangle]
pub unsafe extern "C" fn karac_provider_set_stack_head(head: *const ProviderFrame) {
    PROVIDER_STACK_HEAD.with(|c| c.set(head));
}

/// Snapshot the current per-task provider stack head. Used by codegen at
/// par-block entry to copy into the env-struct so each spawned worker can
/// seed its TLS via `karac_provider_set_stack_head`.
#[no_mangle]
pub extern "C" fn karac_provider_get_stack_head() -> *const ProviderFrame {
    PROVIDER_STACK_HEAD.with(|c| c.get())
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

/// Emit a structured `KARAC_TEST_FAILURE` JSONL line to stderr. Called by
/// codegen-lowered `assert` / `assert_eq` / `assert_ne` on the failure path.
/// Caller is responsible for terminating the process — this function only
/// writes the line and returns, so codegen pairs every call with a following
/// `exit(1)`.
///
/// Slice (c).1 — feeds the per-test outcome bridge in `karac test` JIT mode.
/// The `cmd_test` runner scans the subprocess's stderr for the
/// `KARAC_TEST_FAILURE ` prefix and parses the trailing JSON into a
/// `TestOutcome { passed: false, message, span, left, right }`.
///
/// `left_ptr` / `right_ptr` are null (with `left_len` / `right_len` = 0) for
/// plain `assert(cond)` failures, and non-null UTF-8 byte slices for the
/// formatted operands of `assert_eq` / `assert_ne` mismatches.
///
/// # Safety
///
/// Each `(ptr, len)` pair must either be `(null, 0)` or point to `len`
/// initialized, readable bytes. Codegen always satisfies this — the slices
/// come from either the program's read-only string table or fresh f-string
/// builds whose lifetime exceeds this call.
#[no_mangle]
pub unsafe extern "C" fn karac_test_record_failure(
    file_ptr: *const u8,
    file_len: usize,
    line: u32,
    col: u32,
    msg_ptr: *const u8,
    msg_len: usize,
    left_ptr: *const u8,
    left_len: usize,
    right_ptr: *const u8,
    right_len: usize,
) {
    let file = read_utf8_slice(file_ptr, file_len);
    let msg = read_utf8_slice(msg_ptr, msg_len);
    let left = read_optional_utf8_slice(left_ptr, left_len);
    let right = read_optional_utf8_slice(right_ptr, right_len);

    let mut out = String::from("KARAC_TEST_FAILURE {\"file\":");
    write_json_string(&mut out, &file);
    out.push_str(",\"line\":");
    push_u32(&mut out, line);
    out.push_str(",\"column\":");
    push_u32(&mut out, col);
    out.push_str(",\"message\":");
    write_json_string(&mut out, &msg);
    match left {
        Some(s) => {
            out.push_str(",\"left\":");
            write_json_string(&mut out, &s);
        }
        None => out.push_str(",\"left\":null"),
    }
    match right {
        Some(s) => {
            out.push_str(",\"right\":");
            write_json_string(&mut out, &s);
        }
        None => out.push_str(",\"right\":null"),
    }
    out.push('}');
    eprintln!("{out}");
}

fn read_utf8_slice(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: caller's contract — see `karac_test_record_failure`.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn read_optional_utf8_slice(ptr: *const u8, len: usize) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(read_utf8_slice(ptr, len))
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

// ── Slice F: `std.json` FFI surface ───────────────────────────────────────
//
// Backs `runtime/stdlib/json.kara`'s `Json.parse(s: String)` /
// `Json.stringify(self) -> String` through a pair of `extern "C"` exports
// keyed against `serde_json` (locked design choice (iii) — backing impl is
// `serde_json` via Rust FFI, no hand-rolled Kāra parser).
//
// **Variant-payload-struct shape, not `#[repr(C)] union`.** Sub-step (d)'s
// hard-stop trigger 1 explicitly recommends starting with the variant-
// struct alternative because `#[repr(C)]` unions are unsafe-fiddly and
// every node carrying the largest payload size is negligible at the demo's
// typical tree size (~20 nodes ≈ ~320 bytes overhead). `KaracJsonValue`
// below is therefore one tag byte plus six payload fields, only one of
// which is meaningful per the tag.
//
// **Memory ownership.** Both `karac_runtime_json_parse` and
// `karac_runtime_json_stringify` allocate Boxed trees / `CString`s through
// the standard Rust allocator and return raw pointers; the matching
// `karac_runtime_json_free_value` and `karac_runtime_json_free_string`
// exports return that ownership for cleanup. The Kāra-side bindings in
// `runtime/stdlib/json.kara` walk the tree once into native `Json` shape
// (and once for stringify back), then free immediately — no aliased
// references survive past the round-trip.
//
// **Codegen wiring is deferred to a sibling slice.** v1's interpreter
// dispatch in `src/interpreter.rs` calls `serde_json` directly without
// crossing the FFI boundary; the runtime exports below exist so that when
// codegen wires in JSON support (Slice B's `Response.json[T: ToJson]`
// builder, deferred), the ABI is already settled. They are invoked by the
// `tests::test_json_*` runtime-crate tests at the bottom of this file to
// keep the surface live.

/// Tag byte for `KaracJsonValue`. Values 0..=5 in source-spec order:
/// Null, Bool, Number, String, Array, Object. `#[repr(u8)]` for stable
/// FFI; layout pinned by `tests::test_karac_json_value_layout_pinned`.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KaracJsonTag {
    Null = 0,
    Bool = 1,
    Number = 2,
    String = 3,
    Array = 4,
    Object = 5,
}

/// FFI representation of a single Kāra `Json` enum node. The active
/// payload field is selected by the `tag` byte; all other fields read
/// as zero / null. See module-level comment for the variant-struct vs.
/// union tradeoff.
#[repr(C)]
pub struct KaracJsonValue {
    pub tag: u8,
    /// Active when `tag == KaracJsonTag::Bool`.
    pub bool_val: bool,
    /// Active when `tag == KaracJsonTag::Number`.
    pub num_val: f64,
    /// Active when `tag == KaracJsonTag::String`. UTF-8 bytes; `len` is
    /// the byte count (not character count). `str_ptr` is null when
    /// `len == 0`.
    pub str_ptr: *mut u8,
    pub str_len: usize,
    /// Active when `tag == KaracJsonTag::Array`. `arr_items` points at a
    /// heap-allocated array of `*mut KaracJsonValue`; freed by
    /// `karac_runtime_json_free_value` with the matching tag.
    pub arr_items: *mut *mut KaracJsonValue,
    pub arr_len: usize,
    /// Active when `tag == KaracJsonTag::Object`. `obj_keys` and
    /// `obj_vals` are parallel arrays of `obj_len` entries; key strings
    /// are null-terminated UTF-8 (CString-allocated). Insertion-order
    /// preservation is guaranteed because the parse path uses
    /// `serde_json::Map<_, _>` with the `preserve_order` feature off but
    /// the locked design (ii) reads ordering from the input via the
    /// `serde_json::de::from_str` document order.
    pub obj_keys: *mut *mut std::os::raw::c_char,
    pub obj_vals: *mut *mut KaracJsonValue,
    pub obj_len: usize,
}

/// FFI representation of a `serde_json::Error` location + message.
/// Populated by `karac_runtime_json_parse` only on error; the Kāra-side
/// `JsonError` struct mirrors this shape (see `runtime/stdlib/json.kara`).
#[repr(C)]
pub struct KaracJsonError {
    pub line: u32,
    pub column: u32,
    /// Null-terminated UTF-8 message owned by the runtime; freed via
    /// `karac_runtime_json_free_string`.
    pub message: *mut std::os::raw::c_char,
}

/// Allocate a fresh `KaracJsonValue` on the heap, populate it from the
/// provided `serde_json::Value` recursively, and return the leaked
/// raw pointer. Caller owns the tree and is responsible for invoking
/// `karac_runtime_json_free_value` on the root.
fn json_value_to_karac(value: &serde_json::Value) -> *mut KaracJsonValue {
    let node = match value {
        serde_json::Value::Null => KaracJsonValue {
            tag: KaracJsonTag::Null as u8,
            bool_val: false,
            num_val: 0.0,
            str_ptr: std::ptr::null_mut(),
            str_len: 0,
            arr_items: std::ptr::null_mut(),
            arr_len: 0,
            obj_keys: std::ptr::null_mut(),
            obj_vals: std::ptr::null_mut(),
            obj_len: 0,
        },
        serde_json::Value::Bool(b) => KaracJsonValue {
            tag: KaracJsonTag::Bool as u8,
            bool_val: *b,
            num_val: 0.0,
            str_ptr: std::ptr::null_mut(),
            str_len: 0,
            arr_items: std::ptr::null_mut(),
            arr_len: 0,
            obj_keys: std::ptr::null_mut(),
            obj_vals: std::ptr::null_mut(),
            obj_len: 0,
        },
        serde_json::Value::Number(n) => KaracJsonValue {
            tag: KaracJsonTag::Number as u8,
            bool_val: false,
            num_val: n.as_f64().unwrap_or(0.0),
            str_ptr: std::ptr::null_mut(),
            str_len: 0,
            arr_items: std::ptr::null_mut(),
            arr_len: 0,
            obj_keys: std::ptr::null_mut(),
            obj_vals: std::ptr::null_mut(),
            obj_len: 0,
        },
        serde_json::Value::String(s) => {
            let bytes = s.as_bytes();
            let (ptr, len) = if bytes.is_empty() {
                (std::ptr::null_mut(), 0usize)
            } else {
                let buf = bytes.to_vec().into_boxed_slice();
                let len = buf.len();
                (Box::into_raw(buf) as *mut u8, len)
            };
            KaracJsonValue {
                tag: KaracJsonTag::String as u8,
                bool_val: false,
                num_val: 0.0,
                str_ptr: ptr,
                str_len: len,
                arr_items: std::ptr::null_mut(),
                arr_len: 0,
                obj_keys: std::ptr::null_mut(),
                obj_vals: std::ptr::null_mut(),
                obj_len: 0,
            }
        }
        serde_json::Value::Array(items) => {
            let n = items.len();
            let (arr_ptr, arr_len) = if n == 0 {
                (std::ptr::null_mut(), 0usize)
            } else {
                let mut child_ptrs: Vec<*mut KaracJsonValue> =
                    items.iter().map(json_value_to_karac).collect();
                let ptr = child_ptrs.as_mut_ptr();
                std::mem::forget(child_ptrs);
                (ptr, n)
            };
            KaracJsonValue {
                tag: KaracJsonTag::Array as u8,
                bool_val: false,
                num_val: 0.0,
                str_ptr: std::ptr::null_mut(),
                str_len: 0,
                arr_items: arr_ptr,
                arr_len,
                obj_keys: std::ptr::null_mut(),
                obj_vals: std::ptr::null_mut(),
                obj_len: 0,
            }
        }
        serde_json::Value::Object(map) => {
            let n = map.len();
            let (keys_ptr, vals_ptr, obj_len) = if n == 0 {
                (std::ptr::null_mut(), std::ptr::null_mut(), 0usize)
            } else {
                let mut keys: Vec<*mut std::os::raw::c_char> = Vec::with_capacity(n);
                let mut vals: Vec<*mut KaracJsonValue> = Vec::with_capacity(n);
                for (k, v) in map.iter() {
                    let cstring = std::ffi::CString::new(k.as_str())
                        .unwrap_or_else(|_| std::ffi::CString::new("").unwrap());
                    keys.push(cstring.into_raw());
                    vals.push(json_value_to_karac(v));
                }
                let keys_ptr = keys.as_mut_ptr();
                let vals_ptr = vals.as_mut_ptr();
                std::mem::forget(keys);
                std::mem::forget(vals);
                (keys_ptr, vals_ptr, n)
            };
            KaracJsonValue {
                tag: KaracJsonTag::Object as u8,
                bool_val: false,
                num_val: 0.0,
                str_ptr: std::ptr::null_mut(),
                str_len: 0,
                arr_items: std::ptr::null_mut(),
                arr_len: 0,
                obj_keys: keys_ptr,
                obj_vals: vals_ptr,
                obj_len,
            }
        }
    };
    Box::into_raw(Box::new(node))
}

/// Inverse of `json_value_to_karac`: walk a `KaracJsonValue` tree (built
/// by Kāra-side codegen) and produce a `serde_json::Value` for
/// `serde_json::to_string`. Reads only — does not free.
///
/// # Safety
///
/// `node` must point at a valid `KaracJsonValue` whose payload pointers
/// describe initialized memory consistent with the tag byte.
unsafe fn karac_to_json_value(node: *const KaracJsonValue) -> serde_json::Value {
    if node.is_null() {
        return serde_json::Value::Null;
    }
    let n = &*node;
    match n.tag {
        x if x == KaracJsonTag::Null as u8 => serde_json::Value::Null,
        x if x == KaracJsonTag::Bool as u8 => serde_json::Value::Bool(n.bool_val),
        x if x == KaracJsonTag::Number as u8 => serde_json::Number::from_f64(n.num_val)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        x if x == KaracJsonTag::String as u8 => {
            if n.str_ptr.is_null() || n.str_len == 0 {
                serde_json::Value::String(String::new())
            } else {
                let slice = std::slice::from_raw_parts(n.str_ptr, n.str_len);
                serde_json::Value::String(String::from_utf8_lossy(slice).into_owned())
            }
        }
        x if x == KaracJsonTag::Array as u8 => {
            let mut out = Vec::with_capacity(n.arr_len);
            for i in 0..n.arr_len {
                let item = *n.arr_items.add(i);
                out.push(karac_to_json_value(item));
            }
            serde_json::Value::Array(out)
        }
        x if x == KaracJsonTag::Object as u8 => {
            let mut map = serde_json::Map::with_capacity(n.obj_len);
            for i in 0..n.obj_len {
                let key_ptr = *n.obj_keys.add(i);
                let val_ptr = *n.obj_vals.add(i);
                let key = if key_ptr.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(key_ptr)
                        .to_string_lossy()
                        .into_owned()
                };
                map.insert(key, karac_to_json_value(val_ptr));
            }
            serde_json::Value::Object(map)
        }
        _ => serde_json::Value::Null,
    }
}

/// Parse a null-terminated UTF-8 input string via `serde_json`, return a
/// freshly heap-allocated `KaracJsonValue` tree on success or null on
/// error (with `*error_out` populated).
///
/// # Safety
///
/// `input` must be a valid null-terminated C string. `error_out` must
/// point at writable storage for a `KaracJsonError`; on success the slot
/// is left untouched.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_json_parse(
    input: *const std::os::raw::c_char,
    error_out: *mut KaracJsonError,
) -> *mut KaracJsonValue {
    if input.is_null() {
        if !error_out.is_null() {
            let msg = std::ffi::CString::new("input pointer was null").unwrap();
            (*error_out) = KaracJsonError {
                line: 0,
                column: 0,
                message: msg.into_raw(),
            };
        }
        return std::ptr::null_mut();
    }
    let cstr = std::ffi::CStr::from_ptr(input);
    let s = match cstr.to_str() {
        Ok(s) => s,
        Err(e) => {
            if !error_out.is_null() {
                let msg = std::ffi::CString::new(format!("invalid UTF-8 in input: {}", e))
                    .unwrap_or_else(|_| std::ffi::CString::new("invalid UTF-8").unwrap());
                (*error_out) = KaracJsonError {
                    line: 0,
                    column: 0,
                    message: msg.into_raw(),
                };
            }
            return std::ptr::null_mut();
        }
    };
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(value) => json_value_to_karac(&value),
        Err(e) => {
            if !error_out.is_null() {
                let msg = std::ffi::CString::new(e.to_string())
                    .unwrap_or_else(|_| std::ffi::CString::new("parse error").unwrap());
                (*error_out) = KaracJsonError {
                    line: e.line() as u32,
                    column: e.column() as u32,
                    message: msg.into_raw(),
                };
            }
            std::ptr::null_mut()
        }
    }
}

/// Walk a Kāra-built `KaracJsonValue` tree, render it as a single-line
/// JSON string via `serde_json::to_string`, and return the resulting
/// null-terminated buffer. Caller is responsible for invoking
/// `karac_runtime_json_free_string` on the return value.
///
/// # Safety
///
/// `value` must point at a valid `KaracJsonValue` tree (or be null,
/// which renders as `"null"`).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_json_stringify(
    value: *const KaracJsonValue,
) -> *mut std::os::raw::c_char {
    let v = karac_to_json_value(value);
    let s = serde_json::to_string(&v).unwrap_or_else(|_| "null".to_string());
    std::ffi::CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Recursively free a `KaracJsonValue` tree allocated by
/// `karac_runtime_json_parse`. Walks the tag-keyed payload to free the
/// String payload buffer / Array element pointers / Object key+val
/// arrays before dropping the node itself.
///
/// # Safety
///
/// `value` must either be null or point at a `KaracJsonValue` tree
/// allocated by `karac_runtime_json_parse` (or `json_value_to_karac`)
/// that has not already been freed.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_json_free_value(value: *mut KaracJsonValue) {
    if value.is_null() {
        return;
    }
    let node = Box::from_raw(value);
    match node.tag {
        x if x == KaracJsonTag::String as u8 && !node.str_ptr.is_null() && node.str_len > 0 => {
            let slice = std::slice::from_raw_parts_mut(node.str_ptr, node.str_len);
            drop(Box::from_raw(slice as *mut [u8]));
        }
        x if x == KaracJsonTag::Array as u8 && !node.arr_items.is_null() && node.arr_len > 0 => {
            let items = Vec::from_raw_parts(node.arr_items, node.arr_len, node.arr_len);
            for child in items {
                karac_runtime_json_free_value(child);
            }
        }
        x if x == KaracJsonTag::Object as u8 && node.obj_len > 0 => {
            if !node.obj_keys.is_null() {
                let keys: Vec<*mut std::os::raw::c_char> =
                    Vec::from_raw_parts(node.obj_keys, node.obj_len, node.obj_len);
                for k in keys {
                    if !k.is_null() {
                        drop(std::ffi::CString::from_raw(k));
                    }
                }
            }
            if !node.obj_vals.is_null() {
                let vals: Vec<*mut KaracJsonValue> =
                    Vec::from_raw_parts(node.obj_vals, node.obj_len, node.obj_len);
                for v in vals {
                    karac_runtime_json_free_value(v);
                }
            }
        }
        _ => {}
    }
}

/// Free a `*mut c_char` returned from `karac_runtime_json_stringify` or
/// stored in a `KaracJsonError::message` slot.
///
/// # Safety
///
/// `s` must either be null or point at a CString allocated by the
/// runtime (`CString::into_raw`).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_json_free_string(s: *mut std::os::raw::c_char) {
    if s.is_null() {
        return;
    }
    drop(std::ffi::CString::from_raw(s));
}

// ── std.json codegen-side wiring: per-variant FFI constructors ───────────
//
// Slice (1) of the `Json.stringify()` codegen entry in phase-8-stdlib-floor.md.
// The compiled-binary path to `j.stringify()` requires walking a Kāra-side
// `Json` enum value (variant-tagged `{ tag i64, w0 i64, w1 i64, w2 i64 }`)
// into a runtime-side `*mut KaracJsonValue` tree before handing the tree to
// `karac_runtime_json_stringify`. The walker itself is emitted on the codegen
// side (one synthesized LLVM helper `__karac_json_kara_to_ffi` per module);
// the helpers below are the primitive constructors it calls.
//
// Memory ownership rule matches the rest of the std.json surface: each
// constructor returns a freshly Boxed `KaracJsonValue` (or, for the buffer
// helpers, a freshly allocated `Vec<_>::into_raw` triple), and the caller
// owns the resulting allocation through to either `karac_runtime_json_*` or
// `karac_runtime_json_free_value`. Buffers allocated by `_alloc_*_buf` MUST
// be handed to the matching `_make_array` / `_make_object` consumer — those
// constructors capture the buffer for later free via `Vec::from_raw_parts`
// in `karac_runtime_json_free_value`'s Array/Object arms.

/// Construct a `KaracJsonValue::Null` and return ownership.
#[no_mangle]
pub extern "C" fn karac_runtime_json_make_null() -> *mut KaracJsonValue {
    Box::into_raw(Box::new(KaracJsonValue {
        tag: KaracJsonTag::Null as u8,
        bool_val: false,
        num_val: 0.0,
        str_ptr: std::ptr::null_mut(),
        str_len: 0,
        arr_items: std::ptr::null_mut(),
        arr_len: 0,
        obj_keys: std::ptr::null_mut(),
        obj_vals: std::ptr::null_mut(),
        obj_len: 0,
    }))
}

/// Construct a `KaracJsonValue::Bool(b != 0)`. Pass `1` for `true`, `0` for
/// `false`; any non-zero value is treated as true.
#[no_mangle]
pub extern "C" fn karac_runtime_json_make_bool(b: u8) -> *mut KaracJsonValue {
    Box::into_raw(Box::new(KaracJsonValue {
        tag: KaracJsonTag::Bool as u8,
        bool_val: b != 0,
        num_val: 0.0,
        str_ptr: std::ptr::null_mut(),
        str_len: 0,
        arr_items: std::ptr::null_mut(),
        arr_len: 0,
        obj_keys: std::ptr::null_mut(),
        obj_vals: std::ptr::null_mut(),
        obj_len: 0,
    }))
}

/// Construct a `KaracJsonValue::Number(n)`.
#[no_mangle]
pub extern "C" fn karac_runtime_json_make_number(n: f64) -> *mut KaracJsonValue {
    Box::into_raw(Box::new(KaracJsonValue {
        tag: KaracJsonTag::Number as u8,
        bool_val: false,
        num_val: n,
        str_ptr: std::ptr::null_mut(),
        str_len: 0,
        arr_items: std::ptr::null_mut(),
        arr_len: 0,
        obj_keys: std::ptr::null_mut(),
        obj_vals: std::ptr::null_mut(),
        obj_len: 0,
    }))
}

/// Construct a `KaracJsonValue::String` by copying `len` UTF-8 bytes from
/// `ptr` into a freshly allocated runtime buffer. Empty strings (`len == 0`)
/// store a null pointer with `len == 0`.
///
/// # Safety
///
/// `ptr` must either be null with `len == 0`, or point at `len` initialized
/// UTF-8 bytes (the function does not enforce UTF-8 validity — invalid input
/// surfaces as lossy stringification later).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_json_make_string(
    ptr: *const u8,
    len: usize,
) -> *mut KaracJsonValue {
    let (out_ptr, out_len) = if ptr.is_null() || len == 0 {
        (std::ptr::null_mut(), 0usize)
    } else {
        let slice = std::slice::from_raw_parts(ptr, len);
        let buf = slice.to_vec().into_boxed_slice();
        let n = buf.len();
        (Box::into_raw(buf) as *mut u8, n)
    };
    Box::into_raw(Box::new(KaracJsonValue {
        tag: KaracJsonTag::String as u8,
        bool_val: false,
        num_val: 0.0,
        str_ptr: out_ptr,
        str_len: out_len,
        arr_items: std::ptr::null_mut(),
        arr_len: 0,
        obj_keys: std::ptr::null_mut(),
        obj_vals: std::ptr::null_mut(),
        obj_len: 0,
    }))
}

/// Allocate a length-`len` items buffer for use with
/// `karac_runtime_json_make_array`. Returns a `Vec`-allocated pointer suitable
/// for the matching `Vec::from_raw_parts` reclamation in
/// `karac_runtime_json_free_value`. Caller is responsible for populating each
/// slot with a child `*mut KaracJsonValue` before handing the buffer off.
///
/// `len == 0` returns null (matching `karac_runtime_json_free_value`'s null-
/// guard on `arr_items`).
#[no_mangle]
pub extern "C" fn karac_runtime_json_alloc_items_buf(len: usize) -> *mut *mut KaracJsonValue {
    if len == 0 {
        return std::ptr::null_mut();
    }
    let mut v: Vec<*mut KaracJsonValue> = vec![std::ptr::null_mut(); len];
    let ptr = v.as_mut_ptr();
    std::mem::forget(v);
    ptr
}

/// Allocate a length-`len` keys buffer for use with
/// `karac_runtime_json_make_object`. Same allocation contract as
/// `_alloc_items_buf`. Each slot is a `*mut c_char` — populate via
/// `karac_runtime_json_alloc_key`.
#[no_mangle]
pub extern "C" fn karac_runtime_json_alloc_keys_buf(len: usize) -> *mut *mut std::os::raw::c_char {
    if len == 0 {
        return std::ptr::null_mut();
    }
    let mut v: Vec<*mut std::os::raw::c_char> = vec![std::ptr::null_mut(); len];
    let ptr = v.as_mut_ptr();
    std::mem::forget(v);
    ptr
}

/// Copy `len` UTF-8 bytes from `ptr` into a CString-allocated buffer and
/// return the raw pointer. Pairs with `karac_runtime_json_free_value`'s
/// Object arm, which reclaims each key via `CString::from_raw`. Empty or
/// null input returns a CString containing the empty string (the runtime's
/// Object-key free path expects a valid CString).
///
/// # Safety
///
/// `ptr` must either be null with `len == 0`, or point at `len` initialized
/// bytes that are UTF-8 (interior NULs are stripped via `from_utf8_lossy`
/// fallback when CString construction fails).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_json_alloc_key(
    ptr: *const u8,
    len: usize,
) -> *mut std::os::raw::c_char {
    let bytes: &[u8] = if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len)
    };
    let s = String::from_utf8_lossy(bytes);
    std::ffi::CString::new(s.as_ref())
        .unwrap_or_else(|_| std::ffi::CString::new("").unwrap())
        .into_raw()
}

/// Construct a `KaracJsonValue::Array(items[0..len])`. Takes ownership of
/// the `items` buffer (allocated by `karac_runtime_json_alloc_items_buf`)
/// and of each child node; the caller must not free either after the
/// transfer. A subsequent `karac_runtime_json_free_value` on the result
/// reclaims both.
#[no_mangle]
pub extern "C" fn karac_runtime_json_make_array(
    items: *mut *mut KaracJsonValue,
    len: usize,
) -> *mut KaracJsonValue {
    Box::into_raw(Box::new(KaracJsonValue {
        tag: KaracJsonTag::Array as u8,
        bool_val: false,
        num_val: 0.0,
        str_ptr: std::ptr::null_mut(),
        str_len: 0,
        arr_items: items,
        arr_len: len,
        obj_keys: std::ptr::null_mut(),
        obj_vals: std::ptr::null_mut(),
        obj_len: 0,
    }))
}

/// Construct a `KaracJsonValue::Object(keys[0..len], vals[0..len])`.
/// Same ownership contract as `_make_array`: `keys`, `vals`, every CString
/// in `keys[*]`, and every child node in `vals[*]` are transferred to the
/// new value's free path.
#[no_mangle]
pub extern "C" fn karac_runtime_json_make_object(
    keys: *mut *mut std::os::raw::c_char,
    vals: *mut *mut KaracJsonValue,
    len: usize,
) -> *mut KaracJsonValue {
    Box::into_raw(Box::new(KaracJsonValue {
        tag: KaracJsonTag::Object as u8,
        bool_val: false,
        num_val: 0.0,
        str_ptr: std::ptr::null_mut(),
        str_len: 0,
        arr_items: std::ptr::null_mut(),
        arr_len: 0,
        obj_keys: keys,
        obj_vals: vals,
        obj_len: len,
    }))
}

// ── Slice B: HTTP server FFI surface (minimal `std.http`) ─────────────────
//
// Per locked design choices (2026-05-09):
//   (i)   B1 server-only minimal — `Server`, `Request`, `Response`, `serve()`.
//         No client (the existing client surface in `runtime/stdlib/http.kara`
//         is a separate Phase 8 entry); no middleware / TLS / HTTP/2 /
//         WebSocket.
//   (ii)  B2 hyper backing through this FFI boundary; mirrors Slice F's
//         `serde_json` shape (variant-payload structs, raw-pointer ownership
//         routed back through `karac_runtime_*_free` exports).
//   (iii) B3 fallback (b) — non-polymorphic `serve(handler: fn(Request) -> Response)`
//         with effect-erasure at the FFI boundary. Effect-set parameter syntax
//         on free fns isn't typechecker-supported yet (Theme 6 settled the
//         trait-method shape but free-fn shape is the open delta); polymorphic
//         `serve[E]` is additive once that lands.
//   (iv)  B4 two-layer concurrency — hyper's `tokio::runtime::Runtime` (multi-
//         thread flavor) is built inside `karac_runtime_serve_http`; per-request
//         hyper invokes the Kāra handler synchronously through
//         `tokio::task::block_in_place`, so the handler can sleep / do
//         compute work without breaking the executor.
//   (v)   B5 one bound-zero-port smoke test — see `tests/http_server.rs`.

/// FFI representation of an inbound hyper `Request<Body>` surfaced to the
/// Kāra-side handler. All pointers are owned by the runtime for the
/// duration of the handler call; the handler reads the values but must
/// not free them.
///
/// Strings are null-terminated UTF-8 (CString-allocated). The body buffer
/// is raw bytes (`body_ptr` may be null when `body_len == 0`).
///
/// Headers are conveyed as parallel arrays of `headers_len` entries; both
/// keys and values are CString-allocated. v1's smoke surface doesn't yet
/// exercise header round-trip on the response side — the request-side
/// arrays exist so the handler *could* read headers, but the v1 response
/// builder ignores per-call header insertion (locked design (i): minimal
/// surface; full header round-trip is a v1.5 follow-up).
#[repr(C)]
pub struct KaracHttpRequest {
    pub method: *const std::os::raw::c_char,
    pub path: *const std::os::raw::c_char,
    pub query: *const std::os::raw::c_char,
    pub headers_keys: *const *const std::os::raw::c_char,
    pub headers_vals: *const *const std::os::raw::c_char,
    pub headers_len: usize,
    // Parsed `application/x-www-form-urlencoded` query parameters, conveyed
    // as parallel arrays of `query_len` CString-allocated entries (same
    // shape as `headers_keys` / `headers_vals`). Both key and value are
    // percent-decoded (and `+` → space) at request-translation time, so
    // `Request.query()` iterates them directly without re-parsing the raw
    // `query` string above. Duplicate keys (`?a=1&a=2`) are preserved in
    // order — the Kāra-side return shape is `Vec[(String, String)]`, not a
    // Map, precisely so repeated keys survive.
    pub query_keys: *const *const std::os::raw::c_char,
    pub query_vals: *const *const std::os::raw::c_char,
    pub query_len: usize,
    pub body_ptr: *const u8,
    pub body_len: usize,
}

/// FFI representation of an outbound `Response` produced by the handler.
/// The runtime allocates the buffers; the handler writes them; the
/// runtime translates back to hyper's `Response<Full<Bytes>>` and frees
/// the buffers after the response is sent on the wire.
///
/// `status` is initialized to 200 by `karac_runtime_serve_http` before
/// the handler is invoked; the handler may overwrite it (e.g. 500 on
/// internal error). `body_ptr`/`body_len`/`body_cap` describe a
/// contiguous byte buffer the runtime takes ownership of after the
/// handler returns; `body_cap` is the allocation size (matches `body_len`
/// for v1's tightly-packed byte buffers, but the field exists for
/// future-compat with growable response builders).
///
/// `headers_*` are parallel arrays the handler can populate; v1's smoke
/// path leaves them at `(null, null, 0, 0)`.
#[repr(C)]
pub struct KaracHttpResponse {
    pub status: u16,
    pub body_ptr: *mut u8,
    pub body_len: usize,
    pub body_cap: usize,
    pub headers_keys: *mut *mut std::os::raw::c_char,
    pub headers_vals: *mut *mut std::os::raw::c_char,
    pub headers_len: usize,
    pub headers_cap: usize,
}

/// Allocate a fresh response-body buffer and write it into the response
/// slot. Called from Kāra-side handler bodies that have constructed a
/// `String`/`Bytes` body to emit; the runtime takes ownership of `bytes`
/// for the duration of the request-handling task and frees it after the
/// response is sent.
///
/// # Safety
///
/// `response` must point at a writable `KaracHttpResponse` slot. `bytes`
/// must point at an initialized buffer of `len` bytes (or be null with
/// `len == 0`). Caller must not alias the buffer after this call.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_response_set_body(
    response: *mut KaracHttpResponse,
    bytes: *const u8,
    len: usize,
) {
    if response.is_null() {
        return;
    }
    let resp = &mut *response;
    // Free any previously set body before overwriting.
    if !resp.body_ptr.is_null() && resp.body_cap > 0 {
        let slice = std::slice::from_raw_parts_mut(resp.body_ptr, resp.body_cap);
        drop(Box::from_raw(slice as *mut [u8]));
    }
    if bytes.is_null() || len == 0 {
        resp.body_ptr = std::ptr::null_mut();
        resp.body_len = 0;
        resp.body_cap = 0;
        return;
    }
    let src = std::slice::from_raw_parts(bytes, len);
    let buf: Box<[u8]> = src.to_vec().into_boxed_slice();
    let cap = buf.len();
    let raw = Box::into_raw(buf) as *mut u8;
    resp.body_ptr = raw;
    resp.body_len = len;
    resp.body_cap = cap;
}

/// Phase-8 line 14 — accumulate a `(key, value)` header pair into a
/// thread-local staging buffer that `serve_request` drains after the
/// handler returns. The codegen handler shim (`emit_http_handler_shim`)
/// calls this once per `(key, value)` entry it extracts from the
/// user's `Response.headers: Vec[(String, String)]` field (when
/// present). The `response` argument is unused at v1 (kept in the
/// signature for symmetry with `_set_status` / `_set_body`); v1 has at
/// most one response in flight per worker thread (handlers run inside
/// `block_in_place`), so the thread-local is unambiguous.
///
/// **Contract**: must be called only from within the handler invocation
/// path that `serve_request` drives. Calls outside that path push into
/// a stray buffer that the next request on the same worker thread will
/// drain — behavior is unspecified.
///
/// # Safety
///
/// `key_ptr` / `val_ptr` must point at `key_len` / `val_len`
/// initialized bytes (or be null with `_len == 0`).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_response_set_header(
    _response: *mut KaracHttpResponse,
    key_ptr: *const u8,
    key_len: usize,
    val_ptr: *const u8,
    val_len: usize,
) {
    let key_bytes: &[u8] = if key_ptr.is_null() || key_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(key_ptr, key_len)
    };
    let val_bytes: &[u8] = if val_ptr.is_null() || val_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(val_ptr, val_len)
    };
    // `CString::new` rejects interior NULs; on rejection swallow the
    // header rather than abort the handler (matches the lenient posture
    // the request-side header lookup takes for malformed cstrings).
    let key_cstr = match std::ffi::CString::new(key_bytes) {
        Ok(c) => c,
        Err(_) => return,
    };
    let val_cstr = match std::ffi::CString::new(val_bytes) {
        Ok(c) => c,
        Err(_) => return,
    };
    PENDING_RESPONSE_HEADERS.with(|cell| {
        cell.borrow_mut().push((key_cstr, val_cstr));
    });
}

thread_local! {
    /// Phase-8 line 14 — per-worker-thread staging buffer for
    /// `karac_runtime_http_response_set_header`. Drained by
    /// `serve_request` after each handler invocation and reset at the
    /// start of every `invoke` closure so headers from a prior handler
    /// (running on the same tokio worker thread for a different
    /// request) can't leak into the next response.
    static PENDING_RESPONSE_HEADERS: std::cell::RefCell<Vec<(std::ffi::CString, std::ffi::CString)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Set the HTTP status code on the response slot.
///
/// # Safety
///
/// `response` must point at a writable `KaracHttpResponse` slot.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_response_set_status(
    response: *mut KaracHttpResponse,
    status: u16,
) {
    if response.is_null() {
        return;
    }
    (*response).status = status;
}

/// Read the request path as a null-terminated UTF-8 string. Returned
/// pointer is owned by the runtime for the duration of the handler
/// call; caller must not free.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_path(
    request: *const KaracHttpRequest,
) -> *const std::os::raw::c_char {
    if request.is_null() {
        return std::ptr::null();
    }
    (*request).path
}

/// Read the request method as a null-terminated UTF-8 string. Returned
/// pointer is owned by the runtime; caller must not free.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_method(
    request: *const KaracHttpRequest,
) -> *const std::os::raw::c_char {
    if request.is_null() {
        return std::ptr::null();
    }
    (*request).method
}

/// Read the request body's raw byte pointer. The body is not
/// null-terminated; pair this with `karac_runtime_http_request_body_len`
/// to read the full buffer. Returned pointer is owned by the runtime
/// and valid only for the duration of the current handler invocation.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_body_ptr(
    request: *const KaracHttpRequest,
) -> *const u8 {
    if request.is_null() {
        return std::ptr::null();
    }
    (*request).body_ptr
}

/// Read the request body length in bytes. Returns `0` for the empty
/// body. Pair with `karac_runtime_http_request_body_ptr`.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_body_len(
    request: *const KaracHttpRequest,
) -> usize {
    if request.is_null() {
        return 0;
    }
    (*request).body_len
}

/// Look up a header value by name (case-insensitive per RFC 7230 §
/// 3.2). Returns a pointer to the value's null-terminated UTF-8 bytes
/// if a matching header exists, or null if no header with the given
/// name is present. The returned pointer is owned by the runtime for
/// the duration of the handler call; caller must not free.
///
/// The lookup walks `headers_keys` linearly — hyper preserves request-
/// order, and v1's typical handler reads at most a handful of headers
/// per request, so the simpler linear scan beats a per-request HashMap
/// build (which would amortize only past ~16 lookups). If a v1.x
/// workload pushes that envelope, the hot path can switch to a
/// `HeaderMap` view without breaking this FFI's contract.
///
/// An explicitly-empty header value (rare but legal — e.g.
/// `X-Trace-Id:`) returns a pointer to a zero-length C string, not
/// null. Null is reserved for "header not found." This lets the
/// Kāra-side `Request.header(name)` distinguish `Some("")` from
/// `None` without a second FFI call.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path. `name_ptr` must point at
/// `name_len` initialized UTF-8 bytes (or be null with `name_len ==
/// 0`).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_header(
    request: *const KaracHttpRequest,
    name_ptr: *const u8,
    name_len: usize,
) -> *const std::os::raw::c_char {
    if request.is_null() {
        return std::ptr::null();
    }
    let req = &*request;
    if req.headers_keys.is_null() || req.headers_vals.is_null() || req.headers_len == 0 {
        return std::ptr::null();
    }
    let name_bytes: &[u8] = if name_ptr.is_null() || name_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(name_ptr, name_len)
    };
    let name_str = match std::str::from_utf8(name_bytes) {
        Ok(s) => s,
        Err(_) => return std::ptr::null(),
    };

    let keys = std::slice::from_raw_parts(req.headers_keys, req.headers_len);
    let vals = std::slice::from_raw_parts(req.headers_vals, req.headers_len);
    for (idx, key_ptr) in keys.iter().enumerate() {
        if key_ptr.is_null() {
            continue;
        }
        let key_cstr = std::ffi::CStr::from_ptr(*key_ptr);
        let Ok(key_str) = key_cstr.to_str() else {
            continue;
        };
        if key_str.eq_ignore_ascii_case(name_str) {
            return vals[idx];
        }
    }
    std::ptr::null()
}

/// Number of request headers — the bound for the index passed to
/// `karac_runtime_http_request_header_key_at` /
/// `karac_runtime_http_request_header_val_at`. Backs the full-map
/// iteration surface of `Request.headers()` (each call to
/// `header(name)` walks the same array; `headers()` returns the whole
/// thing as `Vec[(String, String)]`).
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_headers_count(
    request: *const KaracHttpRequest,
) -> usize {
    if request.is_null() {
        return 0;
    }
    (*request).headers_len
}

/// Header name at `idx` (`0 <= idx < headers_count`) as a borrowed
/// null-terminated UTF-8 pointer. Returns null when `idx` is out of
/// range or the slot pointer is null. Caller must not free.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_header_key_at(
    request: *const KaracHttpRequest,
    idx: usize,
) -> *const std::os::raw::c_char {
    if request.is_null() {
        return std::ptr::null();
    }
    let req = &*request;
    if req.headers_keys.is_null() || idx >= req.headers_len {
        return std::ptr::null();
    }
    *req.headers_keys.add(idx)
}

/// Header value at `idx` (`0 <= idx < headers_count`) as a borrowed
/// null-terminated UTF-8 pointer. Pairs index-for-index with
/// `karac_runtime_http_request_header_key_at`. Caller must not free.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_header_val_at(
    request: *const KaracHttpRequest,
    idx: usize,
) -> *const std::os::raw::c_char {
    if request.is_null() {
        return std::ptr::null();
    }
    let req = &*request;
    if req.headers_vals.is_null() || idx >= req.headers_len {
        return std::ptr::null();
    }
    *req.headers_vals.add(idx)
}

/// Number of parsed query parameters — the bound for the index passed
/// to `karac_runtime_http_request_query_key_at` /
/// `karac_runtime_http_request_query_val_at`. Backs `Request.query()`.
/// Parameters are percent-decoded at request-translation time; see
/// `parse_query_pairs`.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_query_count(
    request: *const KaracHttpRequest,
) -> usize {
    if request.is_null() {
        return 0;
    }
    (*request).query_len
}

/// Query-parameter key at `idx` (`0 <= idx < query_count`) as a
/// borrowed null-terminated UTF-8 pointer (already percent-decoded).
/// Returns null when `idx` is out of range. Caller must not free.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_query_key_at(
    request: *const KaracHttpRequest,
    idx: usize,
) -> *const std::os::raw::c_char {
    if request.is_null() {
        return std::ptr::null();
    }
    let req = &*request;
    if req.query_keys.is_null() || idx >= req.query_len {
        return std::ptr::null();
    }
    *req.query_keys.add(idx)
}

/// Query-parameter value at `idx` (`0 <= idx < query_count`) as a
/// borrowed null-terminated UTF-8 pointer (already percent-decoded).
/// Pairs index-for-index with
/// `karac_runtime_http_request_query_key_at`. Caller must not free.
///
/// # Safety
///
/// `request` must point at a `KaracHttpRequest` populated by the
/// runtime's per-request translation path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_request_query_val_at(
    request: *const KaracHttpRequest,
    idx: usize,
) -> *const std::os::raw::c_char {
    if request.is_null() {
        return std::ptr::null();
    }
    let req = &*request;
    if req.query_vals.is_null() || idx >= req.query_len {
        return std::ptr::null();
    }
    *req.query_vals.add(idx)
}

/// Parse a UTF-8 byte slice as a base-10 signed 64-bit integer.
/// Returns `1` on success (with the parsed value written through
/// `out`) or `0` on failure. Trims leading/trailing whitespace
/// before parsing. On failure the contents of `*out` are unspecified;
/// the caller should not read them.
///
/// # Safety
///
/// `data` must point at `len` initialized UTF-8 bytes (or be null
/// with `len == 0`). `out` must be a valid `*mut i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_parse_i64(data: *const u8, len: usize, out: *mut i64) -> u8 {
    if data.is_null() || len == 0 || out.is_null() {
        return 0;
    }
    let slice = std::slice::from_raw_parts(data, len);
    let s = match std::str::from_utf8(slice) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    match s.trim().parse::<i64>() {
        Ok(n) => {
            *out = n;
            1
        }
        Err(_) => 0,
    }
}

/// Parse a UTF-8 byte slice as a signed 64-bit integer in the given
/// `radix` (2..=36). Returns `1` on success (parsed value written through
/// `out`) or `0` on failure. Trims leading/trailing whitespace. A radix
/// outside 2..=36 fails. Backs `i64.from_str_radix(s, radix)` — the
/// self-hosting lexer's hex/binary/octal literal path.
///
/// # Safety
///
/// `data` must point at `len` initialized UTF-8 bytes (or be null with
/// `len == 0`). `out` must be a valid `*mut i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_parse_i64_radix(
    data: *const u8,
    len: usize,
    radix: u32,
    out: *mut i64,
) -> u8 {
    if data.is_null() || len == 0 || out.is_null() || !(2..=36).contains(&radix) {
        return 0;
    }
    let slice = std::slice::from_raw_parts(data, len);
    let s = match std::str::from_utf8(slice) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    match i64::from_str_radix(s.trim(), radix) {
        Ok(n) => {
            *out = n;
            1
        }
        Err(_) => 0,
    }
}

// ── std.http client codegen path (phase-8 line 17 slice 1) ────────────
//
// `karac_runtime_http_client_get` / `_post` back compiled-mode
// `Client.get(url)` / `Client.post(url, body)` dispatch. Synchronous
// `ureq`-backed HTTP/1.1; rustls + `ring` provider via ureq's `tls`
// feature. Out-params convey either success (status > 0, body buffer)
// or transport error (status = 0, error-message buffer). Both buffers
// are libc::malloc-allocated so the Kāra-side `String` `{ data, len,
// cap }` value's `Drop` can free them via plain `free(data)` —
// matching the cap = len convention.
//
// Caller-side codegen treats the status discriminant: `status > 0`
// builds `Result.Ok(Response { status, body })` with body packed into
// the next three Result-payload words; `status == 0` builds
// `Result.Err(HttpError { message })` with the error message packed
// into the same three slots.

#[cfg(feature = "tls")]
extern "C" {
    /// libc `malloc`. Backs the body / error-message buffers handed
    /// back to Kāra-side `String { data, len, cap }` values where
    /// `cap = len` and the Kāra String's `Drop` calls plain
    /// `free(data)`. Used by the http-client FFI below.
    fn malloc(size: usize) -> *mut u8;
}

/// Copy `bytes` into a fresh libc::malloc-allocated buffer and write
/// the pointer + length to the supplied out-params. Returns silently
/// (zero-byte buffer) when `bytes` is empty — the Kāra-side String
/// `{ data: null, len: 0, cap: 0 }` is the empty-string representation
/// and is sound to `free(null)` on Drop (POSIX guarantees `free(null)`
/// is a no-op).
#[cfg(feature = "tls")]
unsafe fn write_owned_bytes_into_out_params(
    bytes: &[u8],
    out_ptr: *mut *mut u8,
    out_len: *mut i64,
) {
    if bytes.is_empty() {
        *out_ptr = std::ptr::null_mut();
        *out_len = 0;
        return;
    }
    let buf = malloc(bytes.len());
    if buf.is_null() {
        *out_ptr = std::ptr::null_mut();
        *out_len = 0;
        return;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
    *out_ptr = buf;
    *out_len = bytes.len() as i64;
}

/// Read a `ureq::Response` entity to raw bytes, bypassing the UTF-8
/// validation `ureq::Response::into_string` performs. This is what lets
/// `Response.bytes()` (phase-8 line 32) surface binary payloads (image
/// downloads, protobuf, file transfers) intact: the body buffer the
/// client FFI hands back holds the verbatim wire bytes rather than the
/// empty string `into_string().unwrap_or_default()` produces on invalid
/// UTF-8. `Response.text()` / `.body()` reinterpret the same buffer as a
/// Kāra `String` — valid UTF-8 for the common text response, raw bytes
/// otherwise (matching reqwest's lossy-`text` posture). A mid-stream
/// read error yields whatever was read so far, mirroring
/// `into_string().unwrap_or_default()`'s lenient stance.
#[cfg(feature = "tls")]
fn read_response_body_bytes(resp: ureq::Response) -> Vec<u8> {
    use std::io::Read;
    let mut buf = Vec::new();
    let _ = resp.into_reader().read_to_end(&mut buf);
    buf
}

// ── Shared ureq agent with explicit webpki-roots (phase-8 line 48) ────
//
// All 4 ureq client fetch sites (`_client_get` / `_post` / `_client_send`
// / `_builder_send`) route through this single `ureq::Agent` whose
// rustls `ClientConfig` explicitly trusts `webpki_roots::TLS_SERVER_ROOTS`
// (the bundled Mozilla Root program) — same `ring` provider + safe
// default protocol versions as `runtime/src/tls.rs::build_client_config`,
// the direct-TLS client-side path. ureq's `tls` feature already brings
// webpki-roots in transitively today, but pinning the choice in our own
// config builder defends against a future ureq default flip to
// `rustls-native-certs` and lets `Client.get("https://...")` work on
// stripped images (Alpine / scratch / distroless) without a system CA
// bundle reachable to the process.
#[cfg(feature = "tls")]
fn http_client_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        let root_store = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let tls_config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("rustls safe default protocol versions are always supported by ring")
            .with_root_certificates(root_store)
            .with_no_client_auth();
        ureq::AgentBuilder::new()
            .tls_config(std::sync::Arc::new(tls_config))
            .build()
    })
}

// ── Client response headers side-table (phase-8 line 39) ──────────────
//
// `Response.header(name)` needs the response's headers, but the
// Kāra-side `Response` is a fixed-width value packed into the
// `Result[Response, HttpError]` payload — there's no room to carry a
// `Vec` of header pairs inline. Instead, each successful client fetch
// stashes the response headers in this side-table keyed by a fresh
// positive handle, and the handle rides in the client FFI's
// `out_headers_handle` out-param into `Response`'s hidden `headers: i64`
// field. `Response.header(name)` reads the handle back and looks the
// header up via `karac_runtime_http_response_header`.
//
// Values are stored as `CString` so the accessor can hand back a
// stable, null-terminated `*const c_char` (the heap buffer a `CString`
// owns is fixed across `HashMap` rehashes, and entries are never
// removed). Entries leak until process exit — the same bounded v1
// trade-off as the `HTTP_BUILDERS` abandoned-handle leak, and resolved
// the same way once `impl Drop for Response` is wired through codegen.

/// One captured response's headers: `(name, value)` pairs, BOTH stored
/// as `CString` so `karac_runtime_http_response_header` (value) and the
/// `karac_runtime_http_response_header_{key,val}_at` iteration accessors
/// (phase-8 line 39 follow-up) can each return a stable null-terminated
/// pointer. Named to keep the `HTTP_RESPONSE_HEADERS` static's type
/// within clippy's `type_complexity` threshold.
type CapturedResponseHeaders = Vec<(std::ffi::CString, std::ffi::CString)>;

static HTTP_RESPONSE_HEADERS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<i64, CapturedResponseHeaders>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(feature = "tls")]
static HTTP_RESPONSE_HEADERS_NEXT_ID: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(1);

/// Capture every header on a live `ureq::Response` into the
/// `HTTP_RESPONSE_HEADERS` side-table and return a fresh positive
/// handle keying the stored entry (or `0` if the lock is poisoned).
/// Must be called BEFORE `read_response_body_bytes` consumes the
/// response — `ureq::Response::header` / `::headers_names` borrow the
/// response, while `into_reader()` moves it. Header values containing an
/// interior NUL (illegal per RFC 7230, but defended against) are
/// skipped. The handle is written into the client FFI's
/// `out_headers_handle` out-param on the Ok path; the Err path leaves it
/// `0` so `Response.header(...)` on a (non-existent) error Response
/// would resolve every lookup to `None`.
#[cfg(feature = "tls")]
fn capture_response_headers(resp: &ureq::Response) -> i64 {
    let mut pairs: CapturedResponseHeaders = Vec::new();
    for name in resp.headers_names() {
        if let Some(val) = resp.header(&name) {
            if let (Ok(cname), Ok(cval)) =
                (std::ffi::CString::new(name), std::ffi::CString::new(val))
            {
                pairs.push((cname, cval));
            }
        }
    }
    let handle = HTTP_RESPONSE_HEADERS_NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if let Ok(mut map) = HTTP_RESPONSE_HEADERS.lock() {
        map.insert(handle, pairs);
        handle
    } else {
        0
    }
}

/// Look up a response header by name (case-insensitive per RFC 7230 §
/// 3.2) in the `HTTP_RESPONSE_HEADERS` entry keyed by `handle`. Returns
/// null when the handle is unknown (an Err response carries handle `0`,
/// which is never inserted) or the header is absent; on hit returns a
/// runtime-owned, null-terminated UTF-8 pointer valid until process
/// exit (entries are never removed, and the `CString` heap buffer is
/// stable across map rehashes). `Response.header(name)` copies the bytes
/// into a fresh Kāra `String` per call, so the resulting
/// `Option[String]` owns its buffer. Response-side mirror of
/// `karac_runtime_http_request_header`.
///
/// # Safety
///
/// `name_ptr` must point at `name_len` initialized UTF-8 bytes (or be
/// null with `name_len == 0`).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_response_header(
    handle: i64,
    name_ptr: *const u8,
    name_len: usize,
) -> *const std::os::raw::c_char {
    let name_bytes: &[u8] = if name_ptr.is_null() || name_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(name_ptr, name_len)
    };
    let name_str = match std::str::from_utf8(name_bytes) {
        Ok(s) => s,
        Err(_) => return std::ptr::null(),
    };
    let Ok(map) = HTTP_RESPONSE_HEADERS.lock() else {
        return std::ptr::null();
    };
    let Some(pairs) = map.get(&handle) else {
        return std::ptr::null();
    };
    for (k, v) in pairs {
        if k.to_bytes().eq_ignore_ascii_case(name_str.as_bytes()) {
            return v.as_ptr();
        }
    }
    std::ptr::null()
}

/// Number of headers captured for `handle` — the loop bound for
/// `Response.headers()` (phase-8 line 39 follow-up). `0` for an unknown
/// handle (e.g. the Err-path sentinel `0`).
#[no_mangle]
pub extern "C" fn karac_runtime_http_response_headers_count(handle: i64) -> i64 {
    if let Ok(map) = HTTP_RESPONSE_HEADERS.lock() {
        if let Some(pairs) = map.get(&handle) {
            return pairs.len() as i64;
        }
    }
    0
}

/// The captured header name (`_key_at`) / value (`_val_at`) at `idx` for
/// `handle`, as a runtime-owned null-terminated pointer valid until
/// process exit (entries are never removed; the `CString` heap buffers
/// are stable across map rehashes), or null when the handle is unknown
/// or `idx` is out of range. Backs `Response.headers()`'s counted-loop
/// copy — each borrowed cstring is copied into a fresh owned Kāra String
/// per call, so the resulting `Vec[(String, String)]` outlives the
/// table. Names are returned in the order ureq surfaced them.
#[no_mangle]
pub extern "C" fn karac_runtime_http_response_header_key_at(
    handle: i64,
    idx: i64,
) -> *const std::os::raw::c_char {
    response_header_field_at(handle, idx, true)
}

#[no_mangle]
pub extern "C" fn karac_runtime_http_response_header_val_at(
    handle: i64,
    idx: i64,
) -> *const std::os::raw::c_char {
    response_header_field_at(handle, idx, false)
}

fn response_header_field_at(handle: i64, idx: i64, want_key: bool) -> *const std::os::raw::c_char {
    if idx < 0 {
        return std::ptr::null();
    }
    if let Ok(map) = HTTP_RESPONSE_HEADERS.lock() {
        if let Some(pairs) = map.get(&handle) {
            if let Some((k, v)) = pairs.get(idx as usize) {
                return if want_key { k.as_ptr() } else { v.as_ptr() };
            }
        }
    }
    std::ptr::null()
}

/// Release the `HTTP_RESPONSE_HEADERS` side-table entry keyed by
/// `handle` (phase-8 line 39 follow-up). Called from the synthesized
/// `Response` Drop at scope exit so response-header captures don't leak
/// until process exit. Idempotent and total: `handle == 0` (the
/// Err-path sentinel / a move-suppressed Response) and any unknown
/// handle are no-ops, so a double-free (e.g. a value-copied Response
/// whose move-suppression was missed) is harmless. After this returns,
/// any pointer previously handed out by `karac_runtime_http_response_header`
/// / `_header_{key,val}_at` for this handle is dangling — the
/// codegen-side accessors copy the bytes into owned Kāra Strings before
/// the Response drops, so a correctly-ordered program never observes
/// that.
#[no_mangle]
pub extern "C" fn karac_runtime_http_response_headers_free(handle: i64) {
    if handle == 0 {
        return;
    }
    if let Ok(mut map) = HTTP_RESPONSE_HEADERS.lock() {
        map.remove(&handle);
    }
}

/// Synchronously fetch `url` via HTTP GET and populate the success or
/// error out-params. The two out-paths are mutually exclusive — only
/// one of `(body_ptr, body_len)` / `(err_ptr, err_len)` carries a real
/// buffer; the other is `(null, 0)`. The success/error discriminant is
/// `*out_status`: `> 0` means HTTP transaction completed (the server
/// returned a status code); `0` means transport error (DNS, connect,
/// TLS, timeout — `err_ptr` carries `ureq::Error`'s display message).
/// Both buffers are libc::malloc-allocated; ownership transfers to
/// the caller.
///
/// # Safety
///
/// `url_ptr` must point at `url_len` initialized UTF-8 bytes (or be
/// null with `url_len == 0`). `out_status`, `out_body_ptr`,
/// `out_body_len`, `out_err_ptr`, `out_err_len` must each point at
/// writable storage of the indicated type.
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_client_get(
    url_ptr: *const u8,
    url_len: usize,
    out_status: *mut i64,
    out_body_ptr: *mut *mut u8,
    out_body_len: *mut i64,
    out_err_ptr: *mut *mut u8,
    out_err_len: *mut i64,
    out_headers_handle: *mut i64,
) {
    *out_status = 0;
    *out_body_ptr = std::ptr::null_mut();
    *out_body_len = 0;
    *out_err_ptr = std::ptr::null_mut();
    *out_err_len = 0;
    *out_headers_handle = 0;

    let url_bytes: &[u8] = if url_ptr.is_null() || url_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(url_ptr, url_len)
    };
    let url = match std::str::from_utf8(url_bytes) {
        Ok(s) => s,
        Err(e) => {
            write_owned_bytes_into_out_params(e.to_string().as_bytes(), out_err_ptr, out_err_len);
            return;
        }
    };

    match http_client_agent().get(url).call() {
        Ok(resp) => {
            *out_status = resp.status() as i64;
            *out_headers_handle = capture_response_headers(&resp);
            let body = read_response_body_bytes(resp);
            write_owned_bytes_into_out_params(&body, out_body_ptr, out_body_len);
        }
        Err(e) => {
            write_owned_bytes_into_out_params(e.to_string().as_bytes(), out_err_ptr, out_err_len);
        }
    }
}

/// Synchronously POST `body_ptr`/`body_len` to `url_ptr`/`url_len` and
/// populate the success or error out-params. Same discriminant + buffer
/// ownership convention as `karac_runtime_http_client_get`. The body is
/// sent verbatim as the request entity (no Content-Type defaulting at
/// this layer — that lives in the chained-builder follow-on); `ureq`
/// applies `Content-Length` automatically.
///
/// # Safety
///
/// Same caller obligations as `karac_runtime_http_client_get`, plus
/// `body_ptr` must point at `body_len` initialized bytes (or be null
/// with `body_len == 0`, which sends an empty entity).
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_client_post(
    url_ptr: *const u8,
    url_len: usize,
    body_ptr: *const u8,
    body_len: usize,
    out_status: *mut i64,
    out_body_ptr: *mut *mut u8,
    out_body_len: *mut i64,
    out_err_ptr: *mut *mut u8,
    out_err_len: *mut i64,
    out_headers_handle: *mut i64,
) {
    *out_status = 0;
    *out_body_ptr = std::ptr::null_mut();
    *out_body_len = 0;
    *out_err_ptr = std::ptr::null_mut();
    *out_err_len = 0;
    *out_headers_handle = 0;

    let url_bytes: &[u8] = if url_ptr.is_null() || url_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(url_ptr, url_len)
    };
    let url = match std::str::from_utf8(url_bytes) {
        Ok(s) => s,
        Err(e) => {
            write_owned_bytes_into_out_params(e.to_string().as_bytes(), out_err_ptr, out_err_len);
            return;
        }
    };
    let body_bytes: &[u8] = if body_ptr.is_null() || body_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(body_ptr, body_len)
    };

    match http_client_agent().post(url).send_bytes(body_bytes) {
        Ok(resp) => {
            *out_status = resp.status() as i64;
            *out_headers_handle = capture_response_headers(&resp);
            let body = read_response_body_bytes(resp);
            write_owned_bytes_into_out_params(&body, out_body_ptr, out_body_len);
        }
        Err(e) => {
            write_owned_bytes_into_out_params(e.to_string().as_bytes(), out_err_ptr, out_err_len);
        }
    }
}

// ── Chained-builder send path (phase-8 line 24) ───────────────────────
//
// `karac_runtime_http_client_send` backs `RequestBuilder.send()`. The
// eager `Client.get` / `Client.post` paths keep their dedicated FFI; the
// chained builder path needs method + headers + timeout, so it gets its
// own extern rather than overloading the simple eager surface.

/// FFI mirror of a Kāra `String { data, len, cap }` aggregate. Pairs
/// of these back the `headers: Vec[(String, String)]` field — the Vec's
/// `data` ptr points at a flat `[KaracHttpHeaderPair; N]` whose layout
/// matches `(String, String)` (two `KaracStr` halves, natural alignment,
/// no padding on 64-bit since pointer + i64 are both 8 bytes).
#[repr(C)]
pub struct KaracStr {
    pub data: *const u8,
    pub len: i64,
    pub cap: i64,
}

#[repr(C)]
pub struct KaracHttpHeaderPair {
    pub key: KaracStr,
    pub val: KaracStr,
}

#[cfg(feature = "tls")]
unsafe fn kara_str_to_str(s: &KaracStr) -> Option<&str> {
    if s.data.is_null() || s.len <= 0 {
        return Some("");
    }
    let bytes = std::slice::from_raw_parts(s.data, s.len as usize);
    std::str::from_utf8(bytes).ok()
}

/// Synchronously send an HTTP request via the chained-builder surface
/// (`Client.request(method, url).header(...).body(...).timeout(...).send()`).
/// Same out-param ownership convention as `karac_runtime_http_client_get`:
/// `*out_status > 0` means HTTP transaction completed; `0` means
/// transport error (`out_err_ptr` carries ureq's display message). Both
/// `body` and `err` buffers are libc::malloc-allocated and freed by the
/// Kāra-side `String { data, len, cap }`'s Drop.
///
/// `method`: ASCII verb (`"GET"`, `"POST"`, `"PUT"`, `"DELETE"`,
/// `"PATCH"`, etc.); forwarded verbatim to `ureq::request(method, url)`.
/// `body_ptr` / `body_len`: request entity bytes; `(null, 0)` sends an
/// empty entity (correct for GET / HEAD; benign for verbs that ignore
/// the body). `headers_ptr` / `headers_count`: array of `(key, val)`
/// String pairs; each non-empty (key, val) is applied via `req.set(k, v)`.
/// `timeout_ms`: `> 0` configures `ureq::Request::timeout`; `0` leaves
/// ureq's default (no timeout).
///
/// # Safety
///
/// Same as `karac_runtime_http_client_get` for url / out-params, plus:
/// `method_ptr` / `body_ptr` must point at the indicated number of bytes
/// (or be null with the matching length `0`); `headers_ptr` must point
/// at `headers_count` initialized `KaracHttpHeaderPair` entries (or be
/// null with `headers_count == 0`).
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_client_send(
    method_ptr: *const u8,
    method_len: usize,
    url_ptr: *const u8,
    url_len: usize,
    body_ptr: *const u8,
    body_len: usize,
    headers_ptr: *const KaracHttpHeaderPair,
    headers_count: usize,
    timeout_ms: i64,
    out_status: *mut i64,
    out_body_ptr: *mut *mut u8,
    out_body_len: *mut i64,
    out_err_ptr: *mut *mut u8,
    out_err_len: *mut i64,
    out_headers_handle: *mut i64,
) {
    *out_status = 0;
    *out_body_ptr = std::ptr::null_mut();
    *out_body_len = 0;
    *out_err_ptr = std::ptr::null_mut();
    *out_err_len = 0;
    *out_headers_handle = 0;

    let method_bytes: &[u8] = if method_ptr.is_null() || method_len == 0 {
        b"GET"
    } else {
        std::slice::from_raw_parts(method_ptr, method_len)
    };
    let method = match std::str::from_utf8(method_bytes) {
        Ok(s) => s,
        Err(e) => {
            write_owned_bytes_into_out_params(e.to_string().as_bytes(), out_err_ptr, out_err_len);
            return;
        }
    };
    let url_bytes: &[u8] = if url_ptr.is_null() || url_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(url_ptr, url_len)
    };
    let url = match std::str::from_utf8(url_bytes) {
        Ok(s) => s,
        Err(e) => {
            write_owned_bytes_into_out_params(e.to_string().as_bytes(), out_err_ptr, out_err_len);
            return;
        }
    };

    let mut req = http_client_agent().request(method, url);
    if headers_count > 0 && !headers_ptr.is_null() {
        let pairs = std::slice::from_raw_parts(headers_ptr, headers_count);
        for pair in pairs {
            let key = match kara_str_to_str(&pair.key) {
                Some(k) if !k.is_empty() => k,
                _ => continue,
            };
            let val = kara_str_to_str(&pair.val).unwrap_or("");
            req = req.set(key, val);
        }
    }
    if timeout_ms > 0 {
        req = req.timeout(std::time::Duration::from_millis(timeout_ms as u64));
    }

    let body_bytes: &[u8] = if body_ptr.is_null() || body_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(body_ptr, body_len)
    };

    let result = if body_bytes.is_empty() {
        req.call()
    } else {
        req.send_bytes(body_bytes)
    };

    match result {
        Ok(resp) => {
            *out_status = resp.status() as i64;
            *out_headers_handle = capture_response_headers(&resp);
            let body = read_response_body_bytes(resp);
            write_owned_bytes_into_out_params(&body, out_body_ptr, out_body_len);
        }
        Err(e) => {
            write_owned_bytes_into_out_params(e.to_string().as_bytes(), out_err_ptr, out_err_len);
        }
    }
}

// ── Handle-based request builder (phase-8 line 24) ────────────────────
//
// `RequestBuilder` on the Kāra side is a thin `{ handle: i64 }` wrapper;
// the actual config lives in `HTTP_BUILDERS` keyed by handle. Each
// chained Kāra method routes through a `_builder_*` extern that
// mutates the entry by handle. `_builder_send` performs the request
// and drops the entry. Handles abandoned without a `send` call leak
// their entry until process exit — acceptable v1 trade-off, resolved
// when `impl Drop for RequestBuilder` is wired through codegen.

#[cfg(feature = "tls")]
struct HttpBuilderState {
    method: String,
    url: String,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
    timeout_ms: i64,
}

#[cfg(feature = "tls")]
static HTTP_BUILDERS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<i64, HttpBuilderState>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(feature = "tls")]
static HTTP_BUILDER_NEXT_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

#[cfg(feature = "tls")]
unsafe fn slice_to_owned_string(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(feature = "tls")]
unsafe fn slice_to_owned_bytes(ptr: *const u8, len: usize) -> Vec<u8> {
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    std::slice::from_raw_parts(ptr, len).to_vec()
}

/// Allocate a new builder entry pre-populated with `method` + `url`,
/// returning a positive handle the chained methods can address. A
/// non-positive return signals allocation failure (treated as a no-op
/// builder by downstream calls, since their handle-lookup is a no-op
/// when the handle isn't present). Backs `Client.request(method, url)`.
///
/// # Safety
///
/// `method_ptr` and `url_ptr` must each point at the indicated number
/// of UTF-8 bytes (or be null with the matching length `0`).
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_builder_new(
    method_ptr: *const u8,
    method_len: usize,
    url_ptr: *const u8,
    url_len: usize,
) -> i64 {
    let method = slice_to_owned_string(method_ptr, method_len);
    let url = slice_to_owned_string(url_ptr, url_len);
    let state = HttpBuilderState {
        method: if method.is_empty() {
            "GET".to_string()
        } else {
            method
        },
        url,
        body: Vec::new(),
        headers: Vec::new(),
        timeout_ms: 0,
    };
    let handle = HTTP_BUILDER_NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if let Ok(mut map) = HTTP_BUILDERS.lock() {
        map.insert(handle, state);
        handle
    } else {
        0
    }
}

/// Append `(name, value)` to the builder's header list. Empty key is
/// silently dropped (mirrors `karac_runtime_http_client_send`'s
/// `skip-empty-key` behavior). Unknown handle is a no-op so an
/// allocation failure at `_builder_new` time degrades gracefully into
/// "send fires with whatever state was successfully set." Backs
/// `RequestBuilder.header(name, value)`.
///
/// # Safety
///
/// `key_ptr` / `val_ptr` must each point at the indicated number of
/// bytes (or be null with the matching length `0`).
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_builder_add_header(
    handle: i64,
    key_ptr: *const u8,
    key_len: usize,
    val_ptr: *const u8,
    val_len: usize,
) {
    let key = slice_to_owned_string(key_ptr, key_len);
    if key.is_empty() {
        return;
    }
    let val = slice_to_owned_string(val_ptr, val_len);
    if let Ok(mut map) = HTTP_BUILDERS.lock() {
        if let Some(state) = map.get_mut(&handle) {
            state.headers.push((key, val));
        }
    }
}

/// Replace the builder's request body bytes (`(null, 0)` clears).
/// Unknown handle is a no-op. Backs `RequestBuilder.body(b)`.
///
/// # Safety
///
/// `body_ptr` must point at `body_len` initialized bytes (or be null
/// with `body_len == 0`).
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_builder_set_body(
    handle: i64,
    body_ptr: *const u8,
    body_len: usize,
) {
    let body = slice_to_owned_bytes(body_ptr, body_len);
    if let Ok(mut map) = HTTP_BUILDERS.lock() {
        if let Some(state) = map.get_mut(&handle) {
            state.body = body;
        }
    }
}

/// Configure the builder's request deadline. `ms <= 0` disables the
/// timeout (ureq default). Unknown handle is a no-op. Backs
/// `RequestBuilder.timeout(ms)`.
///
/// # Safety
///
/// Marked `unsafe extern "C"` for ABI symmetry with the sibling
/// `_builder_*` externs; the body itself touches no raw pointers, so
/// callers have no additional obligation beyond providing a `handle`
/// that was minted by `_builder_new` (which is the only correctness
/// requirement — an unknown handle is a silent no-op).
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_builder_set_timeout(handle: i64, ms: i64) {
    if let Ok(mut map) = HTTP_BUILDERS.lock() {
        if let Some(state) = map.get_mut(&handle) {
            state.timeout_ms = ms;
        }
    }
}

/// Release the `HTTP_BUILDERS` entry keyed by `handle` (phase-8 line 39
/// follow-up). Called from the synthesized `RequestBuilder` Drop at
/// scope exit so an abandoned (never-`send()`-ed) builder doesn't leak
/// its runtime entry — `send()` already removes the entry on its own
/// paths, so for a sent builder this is a no-op. Idempotent and total:
/// `handle == 0` (a move-suppressed / alloc-failed builder) and any
/// unknown handle are no-ops, so a double-free is harmless.
#[cfg(feature = "tls")]
#[no_mangle]
pub extern "C" fn karac_runtime_http_builder_free(handle: i64) {
    if handle == 0 {
        return;
    }
    if let Ok(mut map) = HTTP_BUILDERS.lock() {
        map.remove(&handle);
    }
}

/// Drive the configured request and populate the success / error
/// out-params. Same discriminant + buffer ownership convention as
/// `karac_runtime_http_client_get`. The builder entry is removed from
/// `HTTP_BUILDERS` on every code path (Ok, Err, or unknown handle) so
/// the handle's storage is bounded. An unknown handle yields the
/// transport-error path with a descriptive message. Backs
/// `RequestBuilder.send()`.
///
/// # Safety
///
/// Same as `karac_runtime_http_client_get` for the out-params.
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_builder_send(
    handle: i64,
    out_status: *mut i64,
    out_body_ptr: *mut *mut u8,
    out_body_len: *mut i64,
    out_err_ptr: *mut *mut u8,
    out_err_len: *mut i64,
    out_headers_handle: *mut i64,
) {
    *out_status = 0;
    *out_body_ptr = std::ptr::null_mut();
    *out_body_len = 0;
    *out_err_ptr = std::ptr::null_mut();
    *out_err_len = 0;
    *out_headers_handle = 0;

    let state = if let Ok(mut map) = HTTP_BUILDERS.lock() {
        map.remove(&handle)
    } else {
        None
    };
    let state = match state {
        Some(s) => s,
        None => {
            let msg = "unknown request-builder handle";
            write_owned_bytes_into_out_params(msg.as_bytes(), out_err_ptr, out_err_len);
            return;
        }
    };

    let mut req = http_client_agent().request(&state.method, &state.url);
    for (k, v) in &state.headers {
        req = req.set(k, v);
    }
    if state.timeout_ms > 0 {
        req = req.timeout(std::time::Duration::from_millis(state.timeout_ms as u64));
    }

    let result = if state.body.is_empty() {
        req.call()
    } else {
        req.send_bytes(&state.body)
    };

    match result {
        Ok(resp) => {
            *out_status = resp.status() as i64;
            *out_headers_handle = capture_response_headers(&resp);
            let body = read_response_body_bytes(resp);
            write_owned_bytes_into_out_params(&body, out_body_ptr, out_body_len);
        }
        Err(e) => {
            write_owned_bytes_into_out_params(e.to_string().as_bytes(), out_err_ptr, out_err_len);
        }
    }
}

/// Default slow-header / slow-handshake window for the serve loops when
/// `KARAC_HTTP_HEADER_TIMEOUT_MS` is unset (phase-8 line 124). 10 s is
/// generous for a real client's first request line / TLS round-trip yet
/// short enough to reap a slowloris connection promptly.
#[cfg(feature = "net")]
const DEFAULT_HTTP_HEADER_TIMEOUT_MS: u64 = 10_000;

/// Connection-level resource bounds shared by all three `karac_runtime_serve_*`
/// accept loops — the phase-8 line 124 slowloris / unbounded-spawn fix.
///
/// - `header_timeout`: bounds the window before a connection produces its
///   first request headers (plain HTTP) or completes its TLS handshake
///   (HTTPS). **On by default** ([`DEFAULT_HTTP_HEADER_TIMEOUT_MS`]);
///   `KARAC_HTTP_HEADER_TIMEOUT_MS=<ms>` overrides, `=0` disables. It only
///   fires *before* the request line / handshake completes, so a long-idle
///   *established* connection (e.g. a Demo-1 idle keep-alive) is never
///   reaped by it.
/// - `conn_permits`: optional cap on concurrently in-flight connections.
///   **Off by default** (unbounded) so high-idle-connection workloads (the
///   1M-conn Demo-1 target) need no tuning; `KARAC_HTTP_MAX_CONNS=<n>`
///   bounds it for public-facing deployments. When set, the accept loop
///   acquires a permit *before* accepting, so reaching the cap applies
///   backpressure at the OS accept backlog rather than spawning unbounded
///   tasks.
#[cfg(feature = "net")]
struct ServeLimits {
    header_timeout: Option<std::time::Duration>,
    conn_permits: Option<Arc<tokio::sync::Semaphore>>,
}

#[cfg(feature = "net")]
impl ServeLimits {
    /// Read the serve-loop bounds from the environment once per
    /// `karac_runtime_serve_*` call. A thin reader over [`from_raw`], which
    /// holds the (env-independent, unit-tested) parse logic.
    ///
    /// [`from_raw`]: ServeLimits::from_raw
    fn from_env() -> ServeLimits {
        ServeLimits::from_raw(
            std::env::var("KARAC_HTTP_HEADER_TIMEOUT_MS")
                .ok()
                .as_deref(),
            std::env::var("KARAC_HTTP_MAX_CONNS").ok().as_deref(),
        )
    }

    /// Parse the two serve-loop tunables from their raw string values
    /// (`None` = env var unset). Pure — no environment access — so the
    /// parse matrix is unit-testable without racy `set_var`. Unparseable /
    /// out-of-range values fall back to the safe default (timeout: 10 s on;
    /// cap: off) rather than erroring — a malformed tunable must not take
    /// the server down.
    fn from_raw(timeout_ms: Option<&str>, max_conns: Option<&str>) -> ServeLimits {
        let header_timeout = match timeout_ms.map(|s| s.trim().parse::<u64>()) {
            // unset / non-numeric → default-on; explicit 0 → disabled.
            None | Some(Err(_)) => Some(std::time::Duration::from_millis(
                DEFAULT_HTTP_HEADER_TIMEOUT_MS,
            )),
            Some(Ok(0)) => None,
            Some(Ok(ms)) => Some(std::time::Duration::from_millis(ms)),
        };
        let conn_permits = match max_conns.map(|s| s.trim().parse::<usize>()) {
            // unset / non-numeric / 0 → no cap; n > 0 → bounded.
            Some(Ok(n)) if n > 0 => Some(Arc::new(tokio::sync::Semaphore::new(n))),
            _ => None,
        };
        ServeLimits {
            header_timeout,
            conn_permits,
        }
    }

    /// Acquire one connection permit if a cap is configured, awaiting (i.e.
    /// applying backpressure) when the cap is reached. Returns `None` when
    /// no cap is set. The returned permit is held for the connection's
    /// lifetime and releases its slot on drop.
    async fn acquire_permit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        match &self.conn_permits {
            // `acquire_owned` only errors if the semaphore is closed, which
            // we never do — the `ok()` collapses that impossible case to a
            // dropped permit (no cap enforced for this one connection).
            Some(sem) => Arc::clone(sem).acquire_owned().await.ok(),
            None => None,
        }
    }
}

/// Apply the configured serve-loop tuning to a hyper-util `auto::Builder`
/// (the protocol-negotiating connection builder used by all three serve
/// loops since phase-8 line 145).
///
/// The header-read timeout (phase-8 line 124) is the **HTTP/1**
/// slowloris guard — a peer that opens a connection then dribbles
/// request-header bytes. It is applied through the `auto::Builder`'s
/// `.http1()` sub-builder; hyper only enforces `header_read_timeout`
/// when a `Timer` is installed, so the two are set together. The
/// HTTP/2 path needs no equivalent: `h2`'s connection/stream flow
/// control plus its frame-size limits bound header buffering structurally
/// (there is no unbounded per-stream header accumulation to stall on),
/// so the line-124 vector does not apply. The h2 keep-alive knobs are
/// left at hyper defaults — a future tuning surface, not a v1 gate.
#[cfg(feature = "net")]
fn apply_serve_tuning(
    builder: &mut hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>,
    timeout: Option<std::time::Duration>,
) {
    if let Some(t) = timeout {
        builder.http1().timer(hyper_util::rt::TokioTimer::new());
        builder.http1().header_read_timeout(t);
    }
}

#[cfg(all(test, feature = "net"))]
mod serve_limits_tests {
    //! Phase-8 line 124 — serve-loop slowloris / resource-bound hardening:
    //! the env-driven config matrix, semaphore backpressure, and the
    //! header-timeout reaping a stalled (slowloris) connection while leaving
    //! a healthy request untouched.
    use super::ServeLimits;
    use std::time::Duration;

    // ── config parse matrix (pure, env-free via `from_raw`) ──────────────

    #[test]
    fn default_timeout_on_cap_off_when_unset() {
        let l = ServeLimits::from_raw(None, None);
        assert_eq!(l.header_timeout, Some(Duration::from_millis(10_000)));
        assert!(l.conn_permits.is_none(), "cap must be off by default");
    }

    #[test]
    fn explicit_timeout_is_honored_zero_disables() {
        assert_eq!(
            ServeLimits::from_raw(Some("500"), None).header_timeout,
            Some(Duration::from_millis(500))
        );
        assert!(
            ServeLimits::from_raw(Some("0"), None)
                .header_timeout
                .is_none(),
            "0 ms must disable the timeout"
        );
    }

    #[test]
    fn garbage_timeout_falls_back_to_default() {
        // A malformed tunable must not take the server down — it defaults on.
        assert_eq!(
            ServeLimits::from_raw(Some("not-a-number"), None).header_timeout,
            Some(Duration::from_millis(10_000))
        );
    }

    #[test]
    fn cap_set_only_when_positive() {
        let permits = |s| {
            ServeLimits::from_raw(None, s)
                .conn_permits
                .as_ref()
                .map(|sem| sem.available_permits())
        };
        assert_eq!(permits(Some("128")), Some(128));
        assert_eq!(permits(Some("0")), None, "0 means unbounded");
        assert_eq!(permits(Some("xyz")), None, "garbage means unbounded");
        assert_eq!(permits(None), None, "unset means unbounded");
    }

    #[test]
    fn whitespace_is_trimmed() {
        let l = ServeLimits::from_raw(Some("  250  "), Some("  4  "));
        assert_eq!(l.header_timeout, Some(Duration::from_millis(250)));
        assert_eq!(
            l.conn_permits.as_ref().map(|s| s.available_permits()),
            Some(4)
        );
    }

    // ── semaphore backpressure ───────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn permit_cap_applies_backpressure_until_released() {
        let l = ServeLimits::from_raw(None, Some("1"));
        let p1 = l.acquire_permit().await;
        assert!(p1.is_some());
        // At cap = 1 with the permit held, a second acquire must block.
        let blocked = tokio::time::timeout(Duration::from_millis(100), l.acquire_permit()).await;
        assert!(blocked.is_err(), "second permit should block at cap = 1");
        // Releasing the first frees the slot.
        drop(p1);
        let p2 = tokio::time::timeout(Duration::from_millis(500), l.acquire_permit()).await;
        assert!(
            matches!(p2, Ok(Some(_))),
            "permit should be available after release"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_cap_never_blocks() {
        let l = ServeLimits::from_raw(None, None);
        for _ in 0..1000 {
            // No semaphore → no permit, immediate resolve, no blocking.
            assert!(l.acquire_permit().await.is_none());
        }
    }

    // ── slowloris: the header timeout reaps a stalled connection ─────────

    /// The shared "always reply 200 / `ok`" hyper service used by the two
    /// behavioral tests below. Inlined as a macro rather than a fn to dodge
    /// the unnameable `service_fn` future type (stable Rust has no
    /// `Future = impl Future` associated-type bound).
    macro_rules! ok_service {
        () => {
            hyper::service::service_fn(|_req: hyper::Request<hyper::body::Incoming>| async {
                Ok::<_, std::convert::Infallible>(hyper::Response::new(http_body_util::Full::new(
                    bytes::Bytes::from_static(b"ok"),
                )))
            })
        };
    }

    /// A connection that opens, dribbles a partial request, and never sends
    /// the terminating blank line must be dropped once the header-read
    /// timeout elapses — not held open indefinitely. This is the core
    /// line-124 regression: before the fix the serve loop drove
    /// `serve_connection` with no timer, so hyper waited forever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_header_connection_is_reaped() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let io = hyper_util::rt::TokioIo::new(stream);
            let mut builder =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
            super::apply_serve_tuning(&mut builder, Some(Duration::from_millis(300)));
            let _ = builder.serve_connection(io, ok_service!()).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Partial request: headers started but never terminated (\r\n\r\n).
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n")
            .await
            .unwrap();

        // With the timeout wired, the server closes ~300 ms in, so
        // read_to_end returns (EOF). Without it this hangs to the 5 s
        // outer deadline and the test fails.
        let mut buf = Vec::new();
        let reaped =
            tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut buf)).await;
        assert!(
            reaped.is_ok(),
            "slow-header connection was not reaped within 5s — header timeout not enforced"
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    /// The header timeout must NOT break a well-behaved client: a complete
    /// request still gets its 200 response with the timeout configured.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn healthy_request_succeeds_under_header_timeout() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let io = hyper_util::rt::TokioIo::new(stream);
            let mut builder =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
            super::apply_serve_tuning(&mut builder, Some(Duration::from_millis(300)));
            let _ = builder.serve_connection(io, ok_service!()).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut buf)).await;
        assert!(read.is_ok(), "healthy request did not complete");
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("200"), "expected 200 response, got: {text}");
        assert!(text.ends_with("ok"), "expected body 'ok', got: {text}");
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }
}

/// Synchronously serve HTTP/1.1 traffic on `addr_cstr` until a fatal
/// error breaks the accept loop. The Kāra-side handler is invoked
/// through `tokio::task::block_in_place` per request so it can do
/// arbitrary compute / sleep without blocking other tokio tasks.
///
/// **Returned port shim.** `bound_port_out` (when non-null) receives
/// the actual port the OS bound — this lets `bind("127.0.0.1:0")` work
/// for tests that read the port from the binary's stdout. The port is
/// written before the accept loop starts; readers may read it as soon
/// as they observe a non-zero value.
///
/// Return code: 0 on graceful shutdown (currently never reached — the
/// accept loop runs forever until the process exits); non-zero on bind
/// failure / runtime construction failure.
///
/// # Safety
///
/// `addr_cstr` must be a valid null-terminated C string of the form
/// `"<ip>:<port>"`. `handler` must be a valid `extern "C"` function
/// pointer with the documented signature; the handler is invoked from
/// a tokio worker thread (potentially many threads concurrently) and
/// must be thread-safe. `bound_port_out` may be null; if non-null it
/// must point at writable `u16` storage that lives until at least the
/// accept loop has been entered.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_serve_http(
    addr_cstr: *const std::os::raw::c_char,
    handler: extern "C" fn(*const KaracHttpRequest, *mut KaracHttpResponse),
    bound_port_out: *mut u16,
) -> i32 {
    if addr_cstr.is_null() {
        return 1;
    }
    let cstr = std::ffi::CStr::from_ptr(addr_cstr);
    let addr_str = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => return 2,
    };
    let socket_addr: std::net::SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(_) => return 3,
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return 4,
    };

    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(socket_addr).await {
            Ok(l) => l,
            Err(_) => return 5,
        };
        if let Ok(local) = listener.local_addr() {
            if !bound_port_out.is_null() {
                *bound_port_out = local.port();
            }
            // Smoke-test convention (B5): print the bound port on a
            // dedicated `BOUND_PORT=<n>\n` stdout line so the test
            // harness can read it back when binding to `127.0.0.1:0`
            // (the OS picks an ephemeral port). Real-world apps
            // typically bind to a fixed port; this line is a
            // debug-friendly side-channel rather than a contract surface.
            // Flushed explicitly so the parent process can sync against
            // it without waiting on stdout's stdio buffer.
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "BOUND_PORT={}", local.port());
            let _ = stdout.flush();
        }
        let limits = ServeLimits::from_env();
        loop {
            // Reserve a connection slot before accepting so reaching the
            // optional cap applies backpressure at the OS accept backlog
            // rather than spawning unbounded tasks (phase-8 line 124).
            let permit = limits.acquire_permit().await;
            let (stream, _peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let io = hyper_util::rt::TokioIo::new(stream);
            let header_timeout = limits.header_timeout;
            tokio::spawn(async move {
                let _permit = permit;
                let svc = hyper::service::service_fn(
                    move |req: hyper::Request<hyper::body::Incoming>| async move {
                        serve_request(req, handler).await
                    },
                );
                // phase-8 line 145: `auto::Builder` negotiates the protocol
                // per-connection — an h2 preface (h2c prior-knowledge over
                // this plain TCP socket) drives HTTP/2; anything else is
                // served as HTTP/1.1. The Kāra handler bridge is identical
                // for both (see `serve_request`).
                let mut builder = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                );
                apply_serve_tuning(&mut builder, header_timeout);
                let _ = builder.serve_connection(io, svc).await;
            });
        }
    })
}

/// Synchronously serve HTTPS/1.1 traffic on `addr_cstr` until a fatal
/// error breaks the accept loop. Parallels `karac_runtime_serve_http`
/// — same per-request `block_in_place` dispatch via `serve_request`
/// and same `BOUND_PORT=<n>` stdout convention — but terminates TLS
/// in front of hyper via `tokio_rustls::TlsAcceptor`. The cert + key
/// are supplied as inline PEM byte slices; PEM parsing reuses
/// `tls::build_server_config` (rustls 0.23 + `ring` provider,
/// server-only / no client auth / safe-default protocol versions).
///
/// Per-connection handshake failures are swallowed — a single bad
/// client must not break the accept loop. Successful TLS streams
/// route through the same `serve_request` body the plain-HTTP path
/// uses, so request parsing + response assembly are shared.
///
/// Return codes: same shared classes as `karac_runtime_serve_http`
/// (1 = null addr, 2 = invalid utf-8, 3 = parse fail, 4 = runtime
/// build fail, 5 = bind fail); plus 6 = cert/key PEM invalid or
/// rustls config-build failure.
///
/// # Safety
///
/// Same caller obligations as `karac_runtime_serve_http`, plus
/// `cert_pem` / `key_pem` must each point at `cert_len` / `key_len`
/// initialized bytes (or be null with the matching length `<= 0`,
/// in which case the rustls config build fails with return code 6).
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_serve_https(
    addr_cstr: *const std::os::raw::c_char,
    cert_pem: *const u8,
    cert_len: i64,
    key_pem: *const u8,
    key_len: i64,
    handler: extern "C" fn(*const KaracHttpRequest, *mut KaracHttpResponse),
    bound_port_out: *mut u16,
) -> i32 {
    if addr_cstr.is_null() {
        return 1;
    }
    let cstr = std::ffi::CStr::from_ptr(addr_cstr);
    let addr_str = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => return 2,
    };
    let socket_addr: std::net::SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(_) => return 3,
    };

    // Build the rustls `ServerConfig` from the supplied PEM bytes via
    // the shared `tls::build_server_config` helper (same path used by
    // `karac_runtime_tls_config_new`). Any malformed cert / key
    // collapses into return code 6 — callers see "TLS config failed"
    // without leaking rustls's internal error enum across the FFI.
    let cert_bytes: &[u8] = if cert_pem.is_null() || cert_len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(cert_pem, cert_len as usize)
    };
    let key_bytes: &[u8] = if key_pem.is_null() || key_len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(key_pem, key_len as usize)
    };
    let mut server_config = match crate::tls::build_server_config(cert_bytes, key_bytes) {
        Ok(c) => c,
        Err(_) => return 6,
    };
    // phase-8 line 145: advertise HTTP/2 (then HTTP/1.1) via ALPN so a
    // capable client negotiates `h2` during the TLS handshake; the
    // `auto::Builder` below then drives the matching protocol. ALPN is
    // set *here*, at the HTTPS serve site, rather than in the shared
    // `tls::build_server_config` — that helper also backs the raw
    // `std.tls` listener (`karac_runtime_tls_config_new`), a generic
    // TLS socket that is not an HTTP server, so it must stay ALPN-free.
    // Order is preference order: `h2` first, `http/1.1` as fallback.
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return 4,
    };

    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(socket_addr).await {
            Ok(l) => l,
            Err(_) => return 5,
        };
        if let Ok(local) = listener.local_addr() {
            if !bound_port_out.is_null() {
                *bound_port_out = local.port();
            }
            // Same `BOUND_PORT=<n>\n` stdout convention as
            // `karac_runtime_serve_http` so the smoke-test harness can
            // discover the ephemeral port when binding to
            // `"127.0.0.1:0"`.
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "BOUND_PORT={}", local.port());
            let _ = stdout.flush();
        }
        let limits = ServeLimits::from_env();
        loop {
            // Backpressure before accept (phase-8 line 124) — see the
            // plain-HTTP loop for the rationale.
            let permit = limits.acquire_permit().await;
            let (tcp_stream, _peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let acceptor = acceptor.clone();
            let header_timeout = limits.header_timeout;
            tokio::spawn(async move {
                let _permit = permit;
                // Bound the TLS handshake: a peer that completes the TCP
                // connect but never finishes (or stalls) the ClientHello
                // must not park this task indefinitely (phase-8 line 124,
                // the primary HTTPS slowloris vector). On timeout or
                // handshake error the connection is dropped.
                let tls_stream = match header_timeout {
                    Some(t) => match tokio::time::timeout(t, acceptor.accept(tcp_stream)).await {
                        Ok(Ok(s)) => s,
                        _ => return,
                    },
                    None => match acceptor.accept(tcp_stream).await {
                        Ok(s) => s,
                        Err(_) => return,
                    },
                };
                let io = hyper_util::rt::TokioIo::new(tls_stream);
                let svc = hyper::service::service_fn(
                    move |req: hyper::Request<hyper::body::Incoming>| async move {
                        serve_request(req, handler).await
                    },
                );
                // phase-8 line 145: `auto::Builder` serves HTTP/2 when the
                // TLS handshake negotiated `h2` via ALPN (advertised on the
                // `ServerConfig` below), falling back to HTTP/1.1 otherwise.
                let mut builder = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                );
                apply_serve_tuning(&mut builder, header_timeout);
                let _ = builder.serve_connection(io, svc).await;
            });
        }
    })
}

/// Serve a single hardcoded JSON body for every incoming GET request on
/// `addr_cstr`. This is the **minimal smoke surface** Slice B's B5 test
/// exercises: it bypasses the full handler-fn-ptr codegen path while
/// still proving the FFI + hyper + tokio integration end-to-end.
/// Real handler dispatch flows through `karac_runtime_serve_http`
/// (above); the static-body variant exists so v1's smoke test can pin
/// the bind / serve / respond contract before fn-pointer-as-arg codegen
/// for free fns lands (a follow-up — see Slice B's close-out).
///
/// Behavior: every incoming request returns `200 OK` with the supplied
/// body bytes and `content-type: application/json`. A `BOUND_PORT=<n>\n`
/// line is emitted to stdout before the accept loop starts so test
/// harnesses can read the bound port from a `127.0.0.1:0` bind.
///
/// Return code: 0 on graceful shutdown (currently never reached); non-
/// zero on bind failure / runtime construction failure.
///
/// # Safety
///
/// `addr_cstr` must be a valid null-terminated C string of the form
/// `"<ip>:<port>"`. `body_ptr` must point at `body_len` initialized
/// bytes (or be null with `body_len == 0`). The runtime copies the body
/// before returning so the caller's buffer can be freed immediately.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_serve_http_static(
    addr_cstr: *const std::os::raw::c_char,
    body_ptr: *const u8,
    body_len: usize,
) -> i32 {
    if addr_cstr.is_null() {
        return 1;
    }
    let cstr = std::ffi::CStr::from_ptr(addr_cstr);
    let addr_str = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => return 2,
    };
    let socket_addr: std::net::SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(_) => return 3,
    };
    let body_owned: bytes::Bytes = if body_ptr.is_null() || body_len == 0 {
        bytes::Bytes::new()
    } else {
        let slice = std::slice::from_raw_parts(body_ptr, body_len);
        bytes::Bytes::copy_from_slice(slice)
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return 4,
    };

    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(socket_addr).await {
            Ok(l) => l,
            Err(_) => return 5,
        };
        if let Ok(local) = listener.local_addr() {
            // Smoke-test convention: emit `BOUND_PORT=<n>\n` so the
            // test harness can sync the GET against the actual bound
            // port.
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "BOUND_PORT={}", local.port());
            let _ = stdout.flush();
        }
        let limits = ServeLimits::from_env();
        loop {
            // Backpressure before accept (phase-8 line 124) — see the
            // plain-HTTP loop for the rationale.
            let permit = limits.acquire_permit().await;
            let (stream, _peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let body_clone = body_owned.clone();
            let io = hyper_util::rt::TokioIo::new(stream);
            let header_timeout = limits.header_timeout;
            tokio::spawn(async move {
                let _permit = permit;
                let svc = hyper::service::service_fn(
                    move |_req: hyper::Request<hyper::body::Incoming>| {
                        let body = body_clone.clone();
                        async move {
                            let resp = hyper::Response::builder()
                                .status(200)
                                .header("content-type", "application/json")
                                .body(http_body_util::Full::new(body))
                                .unwrap();
                            Ok::<_, std::convert::Infallible>(resp)
                        }
                    },
                );
                // phase-8 line 145: same protocol-negotiating `auto::Builder`
                // as the handler path — the static smoke surface serves h2c
                // (plain HTTP/2 prior-knowledge) as well as HTTP/1.1.
                let mut builder = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                );
                apply_serve_tuning(&mut builder, header_timeout);
                let _ = builder.serve_connection(io, svc).await;
            });
        }
    })
}

/// Decode a single `application/x-www-form-urlencoded` component:
/// `+` → space, `%XX` → the byte the two hex digits encode, everything
/// else verbatim. Percent-encoded multi-byte UTF-8 sequences decode to
/// their constituent bytes and are reassembled by `from_utf8_lossy`.
/// Malformed escapes (`%` not followed by two hex digits, or a trailing
/// `%`) are kept literally rather than erroring — query strings reach
/// the server from arbitrary clients, so lenient decode beats rejecting
/// the whole request.
// Only the `net`-gated `serve_request` calls this, but it is a pure
// function with unit tests that should run in every feature combo —
// keep it compiled and let the linker dead-strip it from no-net archives.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
fn decode_form_component(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a raw URL query string (the part after `?`, without the `?`)
/// into percent-decoded `(key, value)` pairs. Splits on `&`; each
/// segment splits on its first `=` (`foo` with no `=` yields
/// `("foo", "")`). Empty segments (e.g. a trailing `&`) are skipped.
/// Order is preserved and duplicate keys are kept — backing the
/// `Vec[(String, String)]` (not Map) return shape of `Request.query()`.
// Same all-combo treatment as `decode_form_component` above.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
fn parse_query_pairs(query: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    if query.is_empty() {
        return pairs;
    }
    for segment in query.split('&') {
        if segment.is_empty() {
            continue;
        }
        let (raw_key, raw_val) = match segment.split_once('=') {
            Some((k, v)) => (k, v),
            None => (segment, ""),
        };
        pairs.push((
            decode_form_component(raw_key),
            decode_form_component(raw_val),
        ));
    }
    pairs
}

/// Translate a single hyper `Request<Incoming>` into our `KaracHttpRequest`
/// FFI struct, invoke the Kāra handler synchronously through
/// `block_in_place`, then translate the populated `KaracHttpResponse`
/// back into a hyper `Response<Full<Bytes>>`.
///
/// **Send safety.** The FFI structs hold raw pointers, which are not
/// `Send`. We avoid that hazard by performing all of body-collection +
/// CString building + the synchronous handler call + buffer reclaim
/// *inside* a single `block_in_place` body — no raw pointer ever crosses
/// an `.await` point, so the surrounding async future stays `Send` for
/// `tokio::spawn`. The body is drained synchronously inside
/// `block_in_place` via `Handle::current().block_on(...)` on the body
/// stream's `collect().await`.
///
/// **HTTP/2 multiplexing note (phase-8 line 145).** Under HTTP/2 a
/// single connection carries many concurrent streams, each driving one
/// `serve_request` call. Because the handler runs under
/// `block_in_place`, sibling streams on the *same* connection cannot be
/// polled while one handler is executing — streams within a connection
/// are effectively serialized (tokio's worker-replacement keeps *other*
/// connections progressing in parallel). This matches the per-connection
/// behavior HTTP/1.1 keep-alive already had and is correct for v1; a
/// fully-async handler ABI that would let streams on one connection run
/// concurrently is a separate, larger slice.
#[cfg(feature = "net")]
async fn serve_request(
    req: hyper::Request<hyper::body::Incoming>,
    handler: extern "C" fn(*const KaracHttpRequest, *mut KaracHttpResponse),
) -> Result<hyper::Response<http_body_util::Full<bytes::Bytes>>, std::convert::Infallible> {
    use http_body_util::BodyExt;

    let (parts, body) = req.into_parts();

    // Drain the body before entering the FFI call. This is the only
    // `.await` point in this function; the resulting `bytes::Bytes` is
    // `Send` and the rest of the work runs inside `block_in_place`.
    let body_bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => bytes::Bytes::new(),
    };

    // H1 probe surface (`docs/investigations/http_layer_perf.md`):
    // `KARAC_HTTP_BLOCK_IN_PLACE=0` skips the `block_in_place`
    // wrapper and runs the handler closure directly on the tokio
    // worker. The wrapper's documented purpose is to let other
    // concurrent async work progress while a CPU-bound handler is
    // running (worker-replacement dance). Skipping it removes the
    // per-request handoff cost but blocks the worker for the full
    // handler duration. A/B-able via env var so the impact can be
    // measured against the bench without rebuilding.
    let skip_block_in_place = matches!(
        std::env::var("KARAC_HTTP_BLOCK_IN_PLACE").as_deref(),
        Ok("0")
    );
    // H2 step 1 (cheap part) — eliminate intermediate `String` allocs.
    // `parts` is moved into the closure and `&str` views are taken
    // *inside* it, then handed straight to `CString::new` (which
    // accepts `Into<Vec<u8>>` and so takes `&str` directly). Saves
    // ~3 String allocs (method/path/query) + 2N for headers per
    // request. The CString allocs themselves remain — killing those
    // requires a length-prefixed FFI shape (next step). See
    // `docs/investigations/http_layer_perf.md § H2`.
    let invoke = move || {
        // Phase-8 line 14 — clear any stray header state from a prior
        // handler that ran on this worker thread (block_in_place is
        // synchronous so concurrent handlers on the same thread can't
        // race, but sequential ones can if a prior handler didn't drain
        // because of an early panic).
        PENDING_RESPONSE_HEADERS.with(|cell| cell.borrow_mut().clear());

        let method_str: &str = parts.method.as_str();
        let path_str: &str = parts.uri.path();
        let query_str: &str = parts.uri.query().unwrap_or("");
        let method_cstr = std::ffi::CString::new(method_str).unwrap_or_default();
        let path_cstr = std::ffi::CString::new(path_str).unwrap_or_default();
        let query_cstr = std::ffi::CString::new(query_str).unwrap_or_default();
        let header_count = parts.headers.len();
        let mut hdr_keys: Vec<std::ffi::CString> = Vec::with_capacity(header_count);
        let mut hdr_vals: Vec<std::ffi::CString> = Vec::with_capacity(header_count);
        for (k, v) in parts.headers.iter() {
            hdr_keys.push(std::ffi::CString::new(k.as_str()).unwrap_or_default());
            hdr_vals.push(std::ffi::CString::new(v.to_str().unwrap_or("")).unwrap_or_default());
        }
        let hdr_keys_ptrs: Vec<*const std::os::raw::c_char> =
            hdr_keys.iter().map(|c| c.as_ptr()).collect();
        let hdr_vals_ptrs: Vec<*const std::os::raw::c_char> =
            hdr_vals.iter().map(|c| c.as_ptr()).collect();

        // Parse the raw query string into percent-decoded (key, value)
        // pairs once, here, so `Request.query()` is a flat array walk on
        // the codegen side (parallel to headers). CStrings own the
        // decoded bytes; the ptr vecs stay live until the closure
        // returns (same lifetime contract as the header arrays).
        let query_pairs = parse_query_pairs(query_str);
        let mut qry_keys: Vec<std::ffi::CString> = Vec::with_capacity(query_pairs.len());
        let mut qry_vals: Vec<std::ffi::CString> = Vec::with_capacity(query_pairs.len());
        for (k, v) in &query_pairs {
            qry_keys.push(std::ffi::CString::new(k.as_str()).unwrap_or_default());
            qry_vals.push(std::ffi::CString::new(v.as_str()).unwrap_or_default());
        }
        let qry_keys_ptrs: Vec<*const std::os::raw::c_char> =
            qry_keys.iter().map(|c| c.as_ptr()).collect();
        let qry_vals_ptrs: Vec<*const std::os::raw::c_char> =
            qry_vals.iter().map(|c| c.as_ptr()).collect();

        let req_struct = KaracHttpRequest {
            method: method_cstr.as_ptr(),
            path: path_cstr.as_ptr(),
            query: query_cstr.as_ptr(),
            headers_keys: hdr_keys_ptrs.as_ptr(),
            headers_vals: hdr_vals_ptrs.as_ptr(),
            headers_len: hdr_keys_ptrs.len(),
            query_keys: qry_keys_ptrs.as_ptr(),
            query_vals: qry_vals_ptrs.as_ptr(),
            query_len: qry_keys_ptrs.len(),
            body_ptr: if body_bytes.is_empty() {
                std::ptr::null()
            } else {
                body_bytes.as_ptr()
            },
            body_len: body_bytes.len(),
        };

        let mut resp_struct = KaracHttpResponse {
            status: 200,
            body_ptr: std::ptr::null_mut(),
            body_len: 0,
            body_cap: 0,
            headers_keys: std::ptr::null_mut(),
            headers_vals: std::ptr::null_mut(),
            headers_len: 0,
            headers_cap: 0,
        };

        let req_ptr: *const KaracHttpRequest = &req_struct;
        let resp_ptr: *mut KaracHttpResponse = &mut resp_struct;
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handler(req_ptr, resp_ptr);
        }))
        .is_err();

        // Reclaim the response body buffer (if any) and copy out the
        // live bytes. The `Box<[u8]>` drops at the end of this scope,
        // freeing the runtime-allocated buffer.
        let body_out = if resp_struct.body_ptr.is_null() || resp_struct.body_len == 0 {
            bytes::Bytes::new()
        } else {
            let cap = resp_struct.body_cap.max(resp_struct.body_len);
            // SAFETY: by the FFI contract on
            // `karac_runtime_http_response_set_body`, `body_ptr` /
            // `body_cap` are paired Box-into-raw values whose ownership
            // returns to us at the end of the request-handling task.
            let owned: Box<[u8]> = unsafe {
                let raw_slice = std::slice::from_raw_parts_mut(resp_struct.body_ptr, cap);
                Box::from_raw(raw_slice as *mut [u8])
            };
            bytes::Bytes::copy_from_slice(&owned[..resp_struct.body_len])
        };

        // The cstrings + ptr vecs (and `req_struct`) drop at end of
        // scope here; `method_cstr` etc. are still bound so the
        // borrowed `*const c_char` pointers stay live across the
        // handler call. We don't need explicit `drop` calls — the
        // pointer-bearing locals stay live until the closure returns,
        // which is exactly what we want, and clippy flags explicit
        // drops on non-Drop types.
        let _keep = (
            &method_cstr,
            &path_cstr,
            &query_cstr,
            &hdr_keys,
            &hdr_vals,
            &hdr_keys_ptrs,
            &hdr_vals_ptrs,
        );

        // Phase-8 line 14 — drain the handler's staged response
        // headers into an owned Vec. Done inside the closure so the
        // thread-local is consumed before the worker thread becomes
        // available for the next request (the next request's
        // `invoke` start clears as a safety net, but this drain is
        // the primary cleanup).
        let user_headers: Vec<(String, String)> = PENDING_RESPONSE_HEADERS.with(|cell| {
            cell.borrow_mut()
                .drain(..)
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect()
        });

        (resp_struct.status, body_out, user_headers, panicked)
    };
    let (status, body_out, user_headers, panicked) = if skip_block_in_place {
        invoke()
    } else {
        tokio::task::block_in_place(invoke)
    };

    if panicked {
        let msg = b"Internal Server Error\n";
        let body = http_body_util::Full::new(bytes::Bytes::copy_from_slice(msg));
        let resp = hyper::Response::builder()
            .status(500)
            .header("content-type", "text/plain")
            .body(body)
            .unwrap();
        return Ok(resp);
    }

    // Phase-8 line 14 — apply user-set response headers (via
    // `karac_runtime_http_response_set_header`). If the user set
    // their own `content-type` it overrides the smoke-path default;
    // otherwise the default keeps the existing JSON-friendly behavior
    // for `serve(handler)` cases that don't touch headers.
    let user_set_content_type = user_headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
    let mut builder = hyper::Response::builder().status(status);
    for (k, v) in &user_headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    if !user_set_content_type {
        builder = builder.header("content-type", "application/json");
    }
    let response = builder
        .body(http_body_util::Full::new(body_out))
        .unwrap_or_else(|_| {
            hyper::Response::new(http_body_util::Full::new(bytes::Bytes::from_static(
                b"response build failed",
            )))
        });
    Ok(response)
}

/// In-place sort of a raw byte buffer (`len` elements of `elem_size` bytes).
/// The compiler emits a per-call-site bridge thunk that loads two elements
/// through their pointers, invokes the user closure, and reports the
/// resulting `Ordering` tag as `-1` / `0` / `+1`.
///
/// Fast paths for `elem_size == 8` and `elem_size == 16` reinterpret the
/// buffer as `&mut [[u8; N]]` and call Rust's `sort_by` directly — the most
/// common element layouts (`Vec[i64]`, `Vec[(i64, i64)]`) skip the indirect
/// permute. The general fallback sorts a Vec of indices and permutes through
/// a single uninitialised scratch buffer; this stays correct for any element
/// size without needing a typed Rust view.
///
/// Backs `Vec.sort_by` codegen. See `src/codegen.rs` `compile_vec_method`
/// `"sort_by"` arm and the matching interpreter arm in `src/interpreter.rs`.
///
/// # Safety
///
/// `data` must point to `len * elem_size` initialized, contiguous bytes that
/// the caller exclusively owns for the duration of the call. `cmp` must be a
/// valid function pointer whose only side effect is reading through the two
/// element pointers it is given, returning a `-1 / 0 / +1` tag; spurious
/// orderings (returning the same sign for both `(a, b)` and `(b, a)` calls)
/// produce an arbitrary permutation but never undefined behavior. `ctx` is
/// passed back to `cmp` opaquely and may be null only if `cmp` does not
/// dereference it. `len < 2` or `elem_size <= 0` are accepted and produce a
/// no-op.
#[no_mangle]
pub unsafe extern "C" fn karac_vec_sort_by(
    data: *mut u8,
    len: i64,
    elem_size: i64,
    cmp: extern "C" fn(*mut u8, *const u8, *const u8) -> i64,
    ctx: *mut u8,
) {
    if data.is_null() || len < 2 || elem_size <= 0 {
        return;
    }
    let n = len as usize;
    let sz = elem_size as usize;

    // Stable bottom-up merge sort over an index permutation, built from raw
    // `std::alloc` and pointer arithmetic. The element representation is
    // opaque bytes, so we sort `usize` indices (comparing the elements they
    // point at via `cmp`) and permute the buffer once at the end — cheap
    // index moves, one element pass, stable on equal keys (the merge takes
    // the left run on a tie).
    //
    // Why hand-rolled rather than `slice::sort_by` / `Vec`: the whole point
    // of this entry is to keep a lean seq binary that *sorts* from paying
    // the ~262 KiB DWARF backtrace-symbolizer floor. That floor survives
    // `-dead_strip` whenever the default panic hook stays reachable, and
    // `slice::sort_by` (total-order violation `panic!`) and `Vec` /
    // `(0..n).collect()` (capacity-overflow `panic!`) each keep it
    // reachable. This body has *no reachable panic*: every allocation is a
    // null-checked raw `alloc` (no-op return on failure), indexing is
    // unchecked pointer arithmetic, and there is no `unwrap`/`assert`. So
    // the symbolizer dead-strips and the binary returns to its no-sort
    // floor. See docs/implementation_checklist/phase-7-codegen.md
    // "Lean large-N sort entry". Stability is required by design.md
    // ("In-place stable sort"), which also rules out heapsort/introsort.

    // Total permute-buffer size, guarded against usize overflow.
    let total_bytes = match n.checked_mul(sz) {
        Some(v) => v,
        None => return,
    };
    let idx_layout = match std::alloc::Layout::array::<usize>(n) {
        Ok(l) => l,
        Err(_) => return,
    };
    let tmp_layout = match std::alloc::Layout::from_size_align(total_bytes, 1) {
        Ok(l) => l,
        Err(_) => return,
    };
    let idx = std::alloc::alloc(idx_layout) as *mut usize;
    if idx.is_null() {
        return;
    }
    let buf = std::alloc::alloc(idx_layout) as *mut usize;
    if buf.is_null() {
        std::alloc::dealloc(idx as *mut u8, idx_layout);
        return;
    }
    let tmp = std::alloc::alloc(tmp_layout);
    if tmp.is_null() {
        std::alloc::dealloc(idx as *mut u8, idx_layout);
        std::alloc::dealloc(buf as *mut u8, idx_layout);
        return;
    }

    // Identity permutation.
    let mut i = 0usize;
    while i < n {
        *idx.add(i) = i;
        i += 1;
    }

    // Bottom-up merge: double the run width each pass, merging adjacent runs
    // from `src` into `dst` and ping-ponging the two index buffers. `src`
    // always points at the buffer holding the live permutation.
    let mut src = idx;
    let mut dst = buf;
    let mut width = 1usize;
    while width < n {
        let mut lo = 0usize;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let mut a = lo;
            let mut b = mid;
            let mut k = lo;
            while a < mid && b < hi {
                let ia = *src.add(a);
                let ib = *src.add(b);
                // `<= 0` keeps the left (earlier) element first on a tie —
                // the stability guarantee.
                if cmp(ctx, data.add(ia * sz), data.add(ib * sz)) <= 0 {
                    *dst.add(k) = ia;
                    a += 1;
                } else {
                    *dst.add(k) = ib;
                    b += 1;
                }
                k += 1;
            }
            while a < mid {
                *dst.add(k) = *src.add(a);
                a += 1;
                k += 1;
            }
            while b < hi {
                *dst.add(k) = *src.add(b);
                b += 1;
                k += 1;
            }
            lo += 2 * width;
        }
        std::mem::swap(&mut src, &mut dst);
        width *= 2;
    }

    // Permute the elements into sorted order via `tmp`, then copy back.
    let mut k = 0usize;
    while k < n {
        let old_i = *src.add(k);
        ptr::copy_nonoverlapping(data.add(old_i * sz), tmp.add(k * sz), sz);
        k += 1;
    }
    ptr::copy_nonoverlapping(tmp, data, total_bytes);

    std::alloc::dealloc(idx as *mut u8, idx_layout);
    std::alloc::dealloc(buf as *mut u8, idx_layout);
    std::alloc::dealloc(tmp, tmp_layout);
}

/// In-place reverse of a raw byte buffer (`len` elements of `elem_size` bytes).
/// Backs `Vec.reverse` codegen. See `src/codegen/vec_method.rs` `"reverse"`
/// arm and the matching interpreter arm.
///
/// # Safety
///
/// `data` must point to `len * elem_size` initialized, contiguous bytes that
/// the caller exclusively owns for the duration of the call. `len < 2` or
/// `elem_size <= 0` are accepted and produce a no-op.
#[no_mangle]
pub unsafe extern "C" fn karac_vec_reverse(data: *mut u8, len: i64, elem_size: i64) {
    if data.is_null() || len < 2 || elem_size <= 0 {
        return;
    }
    let n = len as usize;
    let sz = elem_size as usize;
    let mut tmp: Vec<u8> = vec![0u8; sz];
    let mut lo = 0usize;
    let mut hi = n - 1;
    while lo < hi {
        let lp = data.add(lo * sz);
        let hp = data.add(hi * sz);
        ptr::copy_nonoverlapping(lp, tmp.as_mut_ptr(), sz);
        ptr::copy_nonoverlapping(hp, lp, sz);
        ptr::copy_nonoverlapping(tmp.as_ptr(), hp, sz);
        lo += 1;
        hi -= 1;
    }
}

/// Lexicographic byte compare of two strings, with length as the tie-break.
/// Returns `-1` if `a < b`, `0` if equal, `+1` if `a > b`. Backs the
/// String-key arm of `Vec.sort_by_key` codegen (see
/// `emit_sort_by_key_inline_thunk` in `src/codegen/vec_method.rs`).
/// Byte-wise compare matches Rust's `Ord` on `String` for valid UTF-8
/// (Unicode code-point order coincides with byte order for UTF-8).
///
/// # Safety
///
/// `a_ptr` must point to `a_len` initialized, contiguous bytes the caller
/// exclusively owns for the duration of the call; same for `b_ptr` /
/// `b_len`. A null pointer paired with `len == 0` is accepted (empty
/// string); a null pointer with `len > 0` is undefined. Negative lengths
/// are accepted and treated as zero.
#[no_mangle]
pub unsafe extern "C" fn karac_string_cmp(
    a_ptr: *const u8,
    a_len: i64,
    b_ptr: *const u8,
    b_len: i64,
) -> i64 {
    let an = if a_len < 0 { 0 } else { a_len as usize };
    let bn = if b_len < 0 { 0 } else { b_len as usize };
    let prefix = an.min(bn);
    if prefix > 0 {
        let a_slice = std::slice::from_raw_parts(a_ptr, prefix);
        let b_slice = std::slice::from_raw_parts(b_ptr, prefix);
        match a_slice.cmp(b_slice) {
            std::cmp::Ordering::Less => return -1,
            std::cmp::Ordering::Greater => return 1,
            std::cmp::Ordering::Equal => {}
        }
    }
    // Prefix equal — shorter string sorts first.
    match an.cmp(&bn) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Total-order compare on two `f64` values, returning `-1` / `0` / `+1`.
/// Implements Rust's `f64::total_cmp` semantics: sign-flip the bit pattern
/// of negative values so the integer compare matches IEEE 754 numeric order
/// for finite values, with the side effect of giving every `f64` (including
/// every NaN payload, ±0, ±∞) a well-defined position in a total order.
/// Negative NaNs sort smallest, then -∞, then negative finites, then -0,
/// +0, positive finites, +∞, then positive NaNs (largest). Backs the
/// float-key arm of `Vec.sort_by_key` codegen.
///
/// # Safety
///
/// Trivially safe — the inputs are by-value `f64`s with no aliasing.
/// `#[no_mangle]` is required for codegen to link by symbol; the
/// `extern "C"` ABI is chosen to keep the calling convention stable
/// across optimisation levels.
#[no_mangle]
pub extern "C" fn karac_float_cmp(a: f64, b: f64) -> i64 {
    // Convert to a sortable i64 via Rust's standard total_cmp algorithm:
    // (left ^ ((left >> 63) >> 1)) gives the same ordering as IEEE 754 for
    // finite values and a well-defined ordering for NaNs (largest by
    // absolute bit pattern after the sign-flip).
    let mut left = a.to_bits() as i64;
    let mut right = b.to_bits() as i64;
    left ^= (((left >> 63) as u64) >> 1) as i64;
    right ^= (((right >> 63) as u64) >> 1) as i64;
    match left.cmp(&right) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Format `val` the way Rust's `{}` does — the *shortest* decimal string that
/// round-trips — into `buf` (up to `buf_len` bytes), returning the number of
/// bytes written. This is exactly the tree-walk interpreter's float rendering
/// (`Value::Float(v) => write!(f, "{}", v)` in `src/interpreter/value.rs`), so
/// AOT-compiled `println` / interpolation / `Display` print floats identically
/// to `karac run`. C's `printf("%g")` (6 significant figures, lowercase `nan`)
/// is what this replaces and the source of the prior interp-vs-AOT mismatch.
///
/// # Safety
/// `buf` must point to at least `buf_len` writable bytes (a stack buffer at
/// the call site). A null `buf` or non-positive `buf_len` writes nothing and
/// returns 0. The output is NOT NUL-terminated; the caller uses the returned
/// length (the `%.*s` / append-raw convention).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_f64_to_str(val: f64, buf: *mut u8, buf_len: i64) -> i64 {
    let s = format!("{val}");
    let bytes = s.as_bytes();
    let n = (bytes.len() as i64).min(buf_len.max(0));
    if !buf.is_null() && n > 0 {
        // SAFETY: caller guarantees `buf_len` writable bytes; `n <= buf_len`
        // and `n <= bytes.len()`, and the regions don't overlap (distinct
        // allocations — a fresh `String` vs the caller's buffer). `unsafe fn`
        // body is itself an unsafe context (edition 2021).
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n as usize);
    }
    n
}

// ── Slice 5 test stand-ins for slice 3 globals ─────────────────────────────
//
// The runtime crate's `cargo test -p karac-runtime` binary has its own
// (test-only) symbol space — the LLVM globals `KARAC_SPAWN_SITES`,
// `KARAC_SPAWN_SITES_LEN`, `KARAC_SPAWN_SITES_ENABLED` emitted by codegen
// never enter the link. The `#[cfg(not(test))]` gate on the `extern "C"`
// block above means the runtime test binary has no extern decl to resolve
// — it instead reads the stand-in `static` definitions below directly.
//
// In real karac-build pipelines (compiler emits + runtime statically
// links), codegen's `emit_spawn_sites_metadata` provides the symbols with
// `External` linkage and the runtime's `extern "C"` block resolves to
// them. The two paths never collide because they're cfg-gated apart.
//
// `KARAC_SPAWN_SITES_ENABLED = 1` flips
// `karac_runtime_has_debug_metadata()` to `true` for the corresponding
// runtime test (`test_has_debug_metadata_reads_through_global`). `_LEN = 0`
// makes the `list_par_blocks_into` snapshot-from-table loop a no-op for
// tests that don't bind a real frame.
//
// `SpawnSiteEntryStandIn` wraps `KaracSpawnSiteEntry` so we can express
// `unsafe impl Sync` for the const-static stand-in (raw pointers are
// `!Sync` by default; the wrapper is sound because the entry is read-only
// and the pointer is the null sentinel).
#[cfg(test)]
#[repr(transparent)]
struct SpawnSiteEntryStandIn(KaracSpawnSiteEntry);

#[cfg(test)]
unsafe impl Sync for SpawnSiteEntryStandIn {}

#[cfg(test)]
#[no_mangle]
static KARAC_SPAWN_SITES_ENABLED: u8 = 1;

#[cfg(test)]
#[no_mangle]
static KARAC_SPAWN_SITES_LEN: u32 = 0;

#[cfg(test)]
#[no_mangle]
static KARAC_SPAWN_SITES: SpawnSiteEntryStandIn = SpawnSiteEntryStandIn(KaracSpawnSiteEntry {
    id: 0,
    _pad0: 0,
    file_cstr: std::ptr::null(),
    line: 0,
    col: 0,
    worker_count: 0,
    _reserved: 0,
});

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Runtime unit tests for the Debugger Contract slice 4 surface
    //! (parent-frame ref + `KaracWaitTarget`).
    //!
    //! **Frame-tracking test isolation.** Two distinct hazards force
    //! these tests to serialize on `FRAME_TRACKING_ENV_LOCK`:
    //!
    //! 1. **Env-var races on `KARAC_RUNTIME_DEBUG_METADATA`.** Cargo runs
    //!    tests in parallel, so any test that mutates the var races peers
    //!    reading it. Compounding this, `runtime_debug_metadata_enabled`
    //!    caches its result in a `OnceLock<bool>` — once initialized the
    //!    env read never repeats, so a test mutating the var after another
    //!    test has triggered initialization observes nothing.
    //! 2. **Shared-state races on `ACTIVE_FRAMES`.** The registry is a
    //!    process-global `static Mutex<Vec<FramePtr>>`, not thread-local.
    //!    Any test that pushes frames into it (directly via `FrameGuard`
    //!    or transitively by calling `karac_par_run`) or that reads it
    //!    (directly or via `karac_runtime_list_par_blocks_into` /
    //!    `karac_runtime_for_each_active_frame`) must hold the lock.
    //!    Without this, a reader test can run during another test's
    //!    barrier window and observe frames it shouldn't.
    //!
    //! Resolution: every frame-tracking test acquires
    //! `FRAME_TRACKING_ENV_LOCK` at entry, and the disabled-path test
    //! goes through `runtime_debug_metadata_enabled_uncached` (test-only
    //! re-read that bypasses the cache). This mirrors slice 3's
    //! `SPAWN_SITE_ENV_LOCK` pattern in `tests/codegen.rs`.
    //!
    //! Frame-pointer cross-thread shuttling uses `usize` casts so the
    //! `*const KaracFrame` (which is `!Send`) crosses the thread boundary
    //! as a plain integer; the runtime never relies on Rust's auto-Send
    //! inference for these pointers. Soundness now comes from
    //! `karac_par_run`'s per-call Condvar wait, which keeps the calling
    //! thread's frame alive until every dispatched task has finished —
    //! see `ParCall` doc-comments above.
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Barrier, Mutex};

    /// Serializes tests that touch the `KARAC_RUNTIME_DEBUG_METADATA`
    /// env var or the process-global `ACTIVE_FRAMES` registry (read or
    /// write). See the module-level comment for the two hazards.
    static FRAME_TRACKING_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// `KaracWaitTarget` v1 layout pin. Single-variant `None` under
    /// `#[repr(C, u8)]` is one byte total; future variants must be
    /// additive (non-breaking). If this assertion fails, slice 5 / FFI
    /// consumers built against the current layout would mis-read frames.
    #[test]
    fn test_wait_target_size_pinned() {
        assert_eq!(std::mem::size_of::<KaracWaitTarget>(), 1);
    }

    // ── karac_vec_sort_by (lean large-N sort) ──────────────────────────
    //
    // The body is a hand-rolled stable, panic-free merge sort (replaced
    // `slice::sort_by` to drop the ~262 KiB DWARF symbolizer floor — see
    // phase-7-codegen.md "Lean large-N sort entry"). These pin the two
    // properties that matter and that the E2E codegen tests can't isolate
    // from the comparator-thunk layer: stability, and correctness across
    // element sizes (the 8/16-byte special-cases were removed, so every
    // size now flows through the one merge path).

    /// Comparator over the first `i64` of each element (the sort key);
    /// ignores trailing bytes, so equal-key records exercise stability.
    extern "C" fn cmp_first_i64(_ctx: *mut u8, a: *const u8, b: *const u8) -> i64 {
        // SAFETY: the sort hands back pointers to whole elements whose
        // first 8 bytes are an i64 key by construction in these tests.
        unsafe {
            let ka = (a as *const i64).read_unaligned();
            let kb = (b as *const i64).read_unaligned();
            (ka > kb) as i64 - (ka < kb) as i64
        }
    }

    #[test]
    fn karac_vec_sort_by_is_stable() {
        // 16-byte records (key, ord): duplicate keys (`i % 8`) with a
        // strictly increasing `ord`. A stable sort by key alone must leave
        // equal-key records in original (`ord`-ascending) order.
        let n = 200usize;
        let mut data: Vec<i64> = Vec::with_capacity(n * 2);
        for i in 0..n {
            data.push((i % 8) as i64);
            data.push(i as i64);
        }
        unsafe {
            karac_vec_sort_by(
                data.as_mut_ptr() as *mut u8,
                n as i64,
                16,
                cmp_first_i64,
                std::ptr::null_mut(),
            );
        }
        let (mut pk, mut po) = (i64::MIN, i64::MIN);
        for r in 0..n {
            let (k, o) = (data[r * 2], data[r * 2 + 1]);
            assert!(k >= pk, "not sorted by key at record {r}");
            if k == pk {
                assert!(o > po, "stability violated at record {r}");
            }
            pk = k;
            po = o;
        }
    }

    #[test]
    fn karac_vec_sort_by_sorts_various_elem_sizes() {
        // Correctness across 8 / 24 / 12-byte elements (the last is a
        // non-word-multiple, exercising the generic byte permute). Each
        // record's key is the first i64; trailing bytes are padding.
        for &words in &[1usize, 3, 2] {
            let elem_size = if words == 2 {
                12i64
            } else {
                (words * 8) as i64
            };
            let n = 150usize;
            // Build n records of `words` i64 each (12-byte case overlays the
            // key into the first 8 of 12 bytes via a byte buffer).
            let bytes_per = elem_size as usize;
            let mut buf = vec![0u8; n * bytes_per];
            for i in 0..n {
                let key = ((i * 31 + 7) % n) as i64;
                let off = i * bytes_per;
                buf[off..off + 8].copy_from_slice(&key.to_ne_bytes());
            }
            unsafe {
                karac_vec_sort_by(
                    buf.as_mut_ptr(),
                    n as i64,
                    elem_size,
                    cmp_first_i64,
                    std::ptr::null_mut(),
                );
            }
            let mut prev = i64::MIN;
            for i in 0..n {
                let off = i * bytes_per;
                let mut k8 = [0u8; 8];
                k8.copy_from_slice(&buf[off..off + 8]);
                let k = i64::from_ne_bytes(k8);
                assert!(k >= prev, "elem_size {elem_size}: unsorted at {i}");
                prev = k;
            }
        }
    }

    #[test]
    fn karac_vec_sort_by_edge_cases() {
        // n < 2 and elem_size <= 0 are no-ops (never crash).
        let mut one = [5i64];
        unsafe {
            karac_vec_sort_by(
                one.as_mut_ptr() as *mut u8,
                1,
                8,
                cmp_first_i64,
                std::ptr::null_mut(),
            );
            karac_vec_sort_by(
                std::ptr::null_mut(),
                0,
                8,
                cmp_first_i64,
                std::ptr::null_mut(),
            );
        }
        assert_eq!(one, [5]);
        // Already-sorted and reverse-sorted i64 both end ascending.
        let mut asc: Vec<i64> = (0..100).collect();
        let mut desc: Vec<i64> = (0..100).rev().collect();
        unsafe {
            karac_vec_sort_by(
                asc.as_mut_ptr() as *mut u8,
                100,
                8,
                cmp_first_i64,
                std::ptr::null_mut(),
            );
            karac_vec_sort_by(
                desc.as_mut_ptr() as *mut u8,
                100,
                8,
                cmp_first_i64,
                std::ptr::null_mut(),
            );
        }
        let want: Vec<i64> = (0..100).collect();
        assert_eq!(asc, want);
        assert_eq!(desc, want);
    }

    // ── seq_par_run (phase-10 WASM sequential default) ─────────────────
    //
    // The wasm archive's `karac_par_run` body. Compiled on native under
    // cfg(test) precisely so these tests can pin its semantics without a
    // wasm host: source-order execution, parent-cancel cascade, and
    // branch-to-later-branch cancel visibility.

    /// Branch fn: appends its id to a shared order log.
    /// ctx points at `(id: i64, log: *mut Vec<i64>)`.
    unsafe extern "C" fn seq_branch_log(ctx: *mut c_void, _cancel: *const AtomicBool) {
        let (id, log) = *(ctx as *const (i64, *mut Vec<i64>));
        (*log).push(id);
    }

    /// Branch fn: records whether the per-call cancel flag was set when
    /// it ran. ctx points at `*mut bool`.
    unsafe extern "C" fn seq_branch_observe_cancel(ctx: *mut c_void, cancel: *const AtomicBool) {
        let slot = *(ctx as *const *mut bool);
        *slot = (*cancel).load(Ordering::Relaxed);
    }

    /// Branch fn: stores `true` through the per-call cancel flag —
    /// stands in for codegen's fail-fast store on a branch Err.
    unsafe extern "C" fn seq_branch_set_cancel(_ctx: *mut c_void, cancel: *const AtomicBool) {
        (*cancel).store(true, Ordering::Relaxed);
    }

    #[test]
    fn test_seq_par_run_executes_branches_in_source_order() {
        let mut log: Vec<i64> = Vec::new();
        let log_ptr = &mut log as *mut Vec<i64>;
        let ctxs: Vec<(i64, *mut Vec<i64>)> = (1..=4).map(|i| (i, log_ptr)).collect();
        let branches: Vec<KaracBranch> = ctxs
            .iter()
            .map(|c| KaracBranch {
                func: seq_branch_log,
                ctx: c as *const (i64, *mut Vec<i64>) as *mut c_void,
            })
            .collect();
        unsafe {
            seq_par_run(branches.as_ptr(), branches.len(), 0, std::ptr::null());
        }
        assert_eq!(log, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_seq_par_run_cascades_parent_cancel_into_branch_flag() {
        let mut observed = false;
        let observed_ptr: *mut bool = &mut observed;
        let branch = KaracBranch {
            func: seq_branch_observe_cancel,
            ctx: &observed_ptr as *const *mut bool as *mut c_void,
        };
        let parent_cancel = AtomicBool::new(true);
        unsafe {
            seq_par_run(&branch, 1, 0, &parent_cancel as *const AtomicBool);
        }
        assert!(
            observed,
            "an enclosing cancellation must cascade into this region's flag"
        );
    }

    #[test]
    fn test_seq_par_run_branch_cancel_visible_to_later_branches() {
        let mut observed = false;
        let observed_ptr: *mut bool = &mut observed;
        let branches = [
            KaracBranch {
                func: seq_branch_set_cancel,
                ctx: std::ptr::null_mut(),
            },
            KaracBranch {
                func: seq_branch_observe_cancel,
                ctx: &observed_ptr as *const *mut bool as *mut c_void,
            },
        ];
        unsafe {
            seq_par_run(branches.as_ptr(), branches.len(), 0, std::ptr::null());
        }
        assert!(
            observed,
            "a branch's fail-fast cancel store must be visible to later branches"
        );
    }

    #[test]
    fn test_parse_query_pairs_basic() {
        assert_eq!(
            parse_query_pairs("q=hello&lang=en"),
            vec![
                ("q".to_string(), "hello".to_string()),
                ("lang".to_string(), "en".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_query_pairs_empty_is_empty() {
        assert!(parse_query_pairs("").is_empty());
    }

    #[test]
    fn test_parse_query_pairs_percent_and_plus_decode() {
        // `+` → space and `%XX` both decode; UTF-8 (`%E2%9C%93` = ✓)
        // round-trips through the byte buffer.
        assert_eq!(
            parse_query_pairs("greeting=hello+world&mark=%E2%9C%93"),
            vec![
                ("greeting".to_string(), "hello world".to_string()),
                ("mark".to_string(), "✓".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_query_pairs_preserves_duplicate_keys_and_order() {
        assert_eq!(
            parse_query_pairs("a=1&a=2&b=3"),
            vec![
                ("a".to_string(), "1".to_string()),
                ("a".to_string(), "2".to_string()),
                ("b".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_query_pairs_keyless_and_empty_segments() {
        // `flag` with no `=` → empty value; the trailing `&` segment is
        // skipped rather than producing an empty pair.
        assert_eq!(
            parse_query_pairs("flag&x=1&"),
            vec![
                ("flag".to_string(), "".to_string()),
                ("x".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn test_decode_form_component_malformed_percent_kept_literal() {
        // A `%` not followed by two hex digits stays literal.
        assert_eq!(decode_form_component("100%done"), "100%done");
        assert_eq!(decode_form_component("trailing%"), "trailing%");
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
            karac_par_run(
                branches.as_ptr(),
                branches.len() as u64,
                42,
                std::ptr::null(),
            );
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
                karac_par_run(
                    inner_branches.as_ptr(),
                    inner_branches.len() as u64,
                    99,
                    std::ptr::null(),
                );
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
            karac_par_run(
                outer_branches.as_ptr(),
                outer_branches.len() as u64,
                7,
                std::ptr::null(),
            );
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

    /// Phase-6 line 475 — nested cancellation cascade. `par_join_wait`, when
    /// given a non-null `parent_cancel`, must flip the region's own `cancel`
    /// once the parent cancels, so nested branches observe it cooperatively.
    /// Tested directly (no spinning branch / pool-grab race): a side thread
    /// sets the parent flag, confirms the join propagated it to the region's
    /// `cancel`, then releases the join by completing the lone outstanding
    /// task.
    #[test]
    fn par_join_wait_propagates_parent_cancel_into_region() {
        let call = Arc::new(ParCall {
            cancel: AtomicBool::new(false),
            remaining: Mutex::new(1), // one outstanding "task"
            notify: Condvar::new(),
            spawn_site_id: 0,
            parent_addr: 0,
            track_frames: false,
        });
        let parent = Arc::new(AtomicBool::new(false));
        let p = pool();

        let side = {
            let call = Arc::clone(&call);
            let parent = Arc::clone(&parent);
            thread::spawn(move || {
                // Let the join settle into its poll loop, then fire the
                // outer cancel.
                thread::sleep(std::time::Duration::from_millis(20));
                parent.store(true, Ordering::Release);
                // Give the ≤1ms poll time to observe + propagate.
                thread::sleep(std::time::Duration::from_millis(50));
                assert!(
                    call.cancel.load(Ordering::Acquire),
                    "par_join_wait did not propagate the parent cancel into the region",
                );
                // Release the join.
                {
                    let mut r = call.remaining.lock().unwrap_or_else(|e| e.into_inner());
                    *r = 0;
                }
                call.notify.notify_all();
            })
        };

        // Blocks until the side thread zeroes `remaining`; meanwhile it polls
        // `parent` and flips `call.cancel`.
        unsafe {
            par_join_wait(&call, p, Arc::as_ptr(&parent));
        }
        side.join().unwrap();
        assert!(call.cancel.load(Ordering::Acquire));
    }

    /// Phase-6 line 473 — completion wins cancellation. A task already
    /// running is never aborted by the cancel flag: `execute_task` checks
    /// the flag only at pickup, so a branch that sets the flag mid-body
    /// (modeling a sibling fail-fast firing while this branch is past its
    /// last effect-boundary check) still runs to its end. Real work is not
    /// retroactively converted to `Cancelled`.
    #[test]
    fn par_run_running_branch_completes_despite_cancel() {
        struct Ctx {
            ran_to_end: AtomicBool,
        }
        unsafe extern "C" fn branch(ctx: *mut c_void, cancel: *const AtomicBool) {
            let c = &*(ctx as *const Ctx);
            // Fire the region's cancel mid-run (as a sibling's fail-fast
            // would). A running task must NOT be torn down by this.
            (*cancel).store(true, Ordering::Relaxed);
            c.ran_to_end.store(true, Ordering::Release);
        }
        let ctx = Ctx {
            ran_to_end: AtomicBool::new(false),
        };
        let branches = [KaracBranch {
            func: branch,
            ctx: &ctx as *const _ as *mut c_void,
        }];
        unsafe {
            karac_par_run(
                branches.as_ptr(),
                branches.len() as u64,
                0,
                std::ptr::null(),
            );
        }
        assert!(
            ctx.ran_to_end.load(Ordering::Acquire),
            "a running branch was aborted by a mid-run cancel — completion-wins violated",
        );
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
                karac_par_run(
                    branches_addr as *const KaracBranch,
                    count as u64,
                    11,
                    std::ptr::null(),
                );
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
            karac_par_run(
                branches.as_ptr(),
                branches.len() as u64,
                0,
                std::ptr::null(),
            );
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

    // ── Slice 5 layout pins ────────────────────────────────────────
    //
    // The `KaracParBlockInfo` `#[repr(C)]` layout must match what
    // user-side codegen would emit for the baked-stdlib `ParBlockInfo`
    // struct (`runtime/stdlib/runtime.kara`). LLVM lays out
    // `{ i32, {ptr, i64, i64}, i32, i32, i32 }` with explicit alignment
    // padding; if Rust's `#[repr(C)]` rules ever diverge from LLVM's
    // `target-data-layout` defaults on a supported target, the runtime
    // would silently mis-write entries and slice 5's `list_par_blocks()`
    // would return garbage. These two tests are the canary.

    #[test]
    fn test_par_block_info_size_pinned() {
        // Expected: { i32 (4) + 4 pad + KaracString (24) + 3*i32 (12) + 4 pad } = 48
        assert_eq!(
            std::mem::size_of::<KaracParBlockInfo>(),
            48,
            "KaracParBlockInfo size drift — codegen would mis-stride; \
             check field order vs `runtime/stdlib/runtime.kara`'s ParBlockInfo"
        );
    }

    #[test]
    fn test_par_block_info_field_offsets_pinned() {
        // Field offsets the LLVM layout produces:
        //   spawn_site_id: 0
        //   file:          8 (after 4 bytes of alignment padding)
        //   line:         32
        //   col:          36
        //   worker_count: 40
        assert_eq!(std::mem::offset_of!(KaracParBlockInfo, spawn_site_id), 0);
        assert_eq!(std::mem::offset_of!(KaracParBlockInfo, file), 8);
        assert_eq!(std::mem::offset_of!(KaracParBlockInfo, line), 32);
        assert_eq!(std::mem::offset_of!(KaracParBlockInfo, col), 36);
        assert_eq!(std::mem::offset_of!(KaracParBlockInfo, worker_count), 40);
    }

    #[test]
    fn test_karac_json_value_layout_pinned() {
        // `KaracJsonValue` is referenced by hand-coded byte-offset GEPs
        // in `src/codegen/json.rs`'s `__karac_json_ffi_to_kara` walker.
        // Lock the offsets here so a reorder of the source struct
        // surfaces as a runtime-crate test failure rather than as
        // wrong-payload values emerging from `Json.parse` calls.
        //
        // Layout (8-byte aligned, total 72 bytes):
        //   tag:       u8           offset  0  (size 1)
        //   bool_val:  bool         offset  1  (size 1, +6 padding)
        //   num_val:   f64          offset  8  (size 8)
        //   str_ptr:   *mut u8      offset 16  (size 8)
        //   str_len:   usize        offset 24  (size 8)
        //   arr_items: ptr-of-ptr   offset 32  (size 8)
        //   arr_len:   usize        offset 40  (size 8)
        //   obj_keys:  ptr-of-ptr   offset 48  (size 8)
        //   obj_vals:  ptr-of-ptr   offset 56  (size 8)
        //   obj_len:   usize        offset 64  (size 8)
        assert_eq!(std::mem::size_of::<KaracJsonValue>(), 72);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, tag), 0);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, bool_val), 1);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, num_val), 8);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, str_ptr), 16);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, str_len), 24);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, arr_items), 32);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, arr_len), 40);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, obj_keys), 48);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, obj_vals), 56);
        assert_eq!(std::mem::offset_of!(KaracJsonValue, obj_len), 64);
    }

    #[test]
    fn test_karac_json_error_layout_pinned() {
        // `KaracJsonError` is allocated on the codegen-emitted caller's
        // stack and read field-by-field after a failed
        // `karac_runtime_json_parse`. Pin the offsets so the codegen-side
        // GEPs stay in sync with the struct shape.
        //
        // Layout (8-byte aligned, total 16 bytes):
        //   line:    u32           offset  0  (size 4)
        //   column:  u32           offset  4  (size 4)
        //   message: *mut c_char   offset  8  (size 8)
        assert_eq!(std::mem::size_of::<KaracJsonError>(), 16);
        assert_eq!(std::mem::offset_of!(KaracJsonError, line), 0);
        assert_eq!(std::mem::offset_of!(KaracJsonError, column), 4);
        assert_eq!(std::mem::offset_of!(KaracJsonError, message), 8);
    }

    #[test]
    fn test_spawn_site_entry_layout_pinned() {
        // Mirrors the LLVM struct layout in `Codegen::emit_spawn_sites_metadata`:
        //   { i32 id, ptr file_cstr, i32 line, i32 col, i32 worker_count, i32 reserved }
        // Expected total size 32 bytes (8-byte alignment from the pointer).
        assert_eq!(std::mem::size_of::<KaracSpawnSiteEntry>(), 32);
        assert_eq!(std::mem::offset_of!(KaracSpawnSiteEntry, id), 0);
        assert_eq!(std::mem::offset_of!(KaracSpawnSiteEntry, file_cstr), 8);
        assert_eq!(std::mem::offset_of!(KaracSpawnSiteEntry, line), 16);
        assert_eq!(std::mem::offset_of!(KaracSpawnSiteEntry, col), 20);
        assert_eq!(std::mem::offset_of!(KaracSpawnSiteEntry, worker_count), 24);
        assert_eq!(std::mem::offset_of!(KaracSpawnSiteEntry, _reserved), 28);
    }

    #[test]
    fn test_has_debug_metadata_reads_through_global() {
        // The runtime crate's `karac_runtime_has_debug_metadata` reads
        // `KARAC_SPAWN_SITES_ENABLED` directly. In the runtime test
        // binary we provide a strong-linkage definition of the slice-3
        // globals (see the `#[no_mangle]` block at the top of this
        // test module) so the reader resolves cleanly under
        // `cargo test -p karac-runtime`. The test confirms the value
        // we set flows through: 1 → true.
        let value = karac_runtime_has_debug_metadata();
        // The test-side stand-in below sets ENABLED to 1.
        assert!(
            value,
            "expected has_debug_metadata to read true via stand-in"
        );
    }

    #[test]
    fn test_list_par_blocks_into_empty_outside_par() {
        // Slice 5: `karac_runtime_list_par_blocks_into` writes
        // `{null, 0, 0}` when `ACTIVE_FRAMES` is empty. Validates the
        // empty-fast-path branch.
        //
        // Holds `FRAME_TRACKING_ENV_LOCK` because peer tests
        // (e.g. `test_active_frames_register_during_par`) push worker
        // frames into the process-global `ACTIVE_FRAMES` and park on a
        // barrier — without the lock this test races them and observes
        // a non-empty registry, taking the allocation path instead of
        // the fast path.
        let _guard = FRAME_TRACKING_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let mut out = KaracVec {
            data: std::ptr::null_mut(),
            len: -1,
            cap: -1,
        };
        unsafe {
            karac_runtime_list_par_blocks_into(&mut out as *mut _);
        }
        assert!(out.data.is_null(), "expected null data on empty");
        assert_eq!(out.len, 0, "expected len=0 on empty");
        assert_eq!(out.cap, 0, "expected cap=0 on empty");
    }

    #[test]
    fn test_list_par_blocks_into_null_out_safe() {
        // Defensive: passing `null` as the out-pointer is a no-op
        // rather than UB. The compiler always allocates the slot, so
        // this should never happen in practice — but the runtime
        // explicitly returns early to avoid a deref crash if a
        // future codegen bug regresses the alloca path.
        unsafe {
            karac_runtime_list_par_blocks_into(std::ptr::null_mut());
        }
        // No assertion — the test passes by not crashing.
    }

    // ── Provider stack tests (Theme 6 sub-step 1) ──────────────────────────

    /// `karac_provider_lookup` returns null + null when the per-task stack
    /// is empty — codegen branches on this for the structured-panic call.
    #[test]
    fn test_provider_lookup_returns_null_on_empty_stack() {
        // Defensive: any earlier test on this thread might have left the
        // stack non-empty. Pop until empty before asserting.
        while !PROVIDER_STACK_HEAD.with(|c| c.get()).is_null() {
            karac_provider_pop();
        }
        let result = karac_provider_lookup(42);
        assert!(result.data.is_null());
        assert!(result.vtable.is_null());
    }

    /// `push` / `lookup` / `pop` round-trip on a single frame: lookup
    /// finds the just-pushed frame; pop unlinks it; subsequent lookup
    /// misses.
    #[test]
    fn test_provider_push_lookup_pop_roundtrip() {
        while !PROVIDER_STACK_HEAD.with(|c| c.get()).is_null() {
            karac_provider_pop();
        }
        let mut frame = ProviderFrame {
            prev: std::ptr::null(),
            resource_id: 0,
            provider_data_ptr: std::ptr::null(),
            vtable_ptr: std::ptr::null(),
        };
        let data: u64 = 0xCAFE_BABE;
        unsafe {
            karac_provider_push(
                &mut frame as *mut ProviderFrame,
                7,
                &data as *const u64 as *const u8,
                std::ptr::null::<VTable>(),
            );
        }
        let hit = karac_provider_lookup(7);
        assert!(!hit.data.is_null());
        assert_eq!(hit.data as *const u64, &data as *const u64);

        karac_provider_pop();
        let miss = karac_provider_lookup(7);
        assert!(miss.data.is_null());
    }

    /// Nested pushes: lookup returns the innermost (most-recently-pushed)
    /// binding. Pop unwinds to the outer binding.
    #[test]
    fn test_provider_stack_innermost_wins() {
        while !PROVIDER_STACK_HEAD.with(|c| c.get()).is_null() {
            karac_provider_pop();
        }
        let outer_data: u64 = 100;
        let inner_data: u64 = 200;
        let mut outer = ProviderFrame {
            prev: std::ptr::null(),
            resource_id: 0,
            provider_data_ptr: std::ptr::null(),
            vtable_ptr: std::ptr::null(),
        };
        let mut inner = ProviderFrame {
            prev: std::ptr::null(),
            resource_id: 0,
            provider_data_ptr: std::ptr::null(),
            vtable_ptr: std::ptr::null(),
        };
        unsafe {
            karac_provider_push(
                &mut outer,
                3,
                &outer_data as *const u64 as *const u8,
                std::ptr::null::<VTable>(),
            );
            karac_provider_push(
                &mut inner,
                3,
                &inner_data as *const u64 as *const u8,
                std::ptr::null::<VTable>(),
            );
        }
        let hit = karac_provider_lookup(3);
        assert_eq!(hit.data as *const u64, &inner_data as *const u64);

        karac_provider_pop();
        let outer_hit = karac_provider_lookup(3);
        assert_eq!(outer_hit.data as *const u64, &outer_data as *const u64);

        karac_provider_pop();
        let miss = karac_provider_lookup(3);
        assert!(miss.data.is_null());
    }

    /// `set_stack_head` + `get_stack_head` round-trip the per-task head
    /// pointer — used by par-block worker branches to inherit the parent
    /// thread's stack.
    #[test]
    fn test_provider_set_and_get_stack_head() {
        while !PROVIDER_STACK_HEAD.with(|c| c.get()).is_null() {
            karac_provider_pop();
        }
        assert!(karac_provider_get_stack_head().is_null());

        let mut frame = ProviderFrame {
            prev: std::ptr::null(),
            resource_id: 0,
            provider_data_ptr: std::ptr::null(),
            vtable_ptr: std::ptr::null(),
        };
        unsafe {
            karac_provider_push(&mut frame, 1, std::ptr::null(), std::ptr::null::<VTable>());
        }
        let head = karac_provider_get_stack_head();
        assert!(!head.is_null());
        assert_eq!(head, &frame as *const ProviderFrame);

        unsafe {
            karac_provider_set_stack_head(std::ptr::null());
        }
        assert!(karac_provider_get_stack_head().is_null());

        // Restore for cleanup
        unsafe {
            karac_provider_set_stack_head(head);
        }
        karac_provider_pop();
    }

    // ── Slice F: `std.json` FFI surface tests ─────────────────────────
    //
    // The interpreter dispatches `Json.parse` / `Json.stringify` directly
    // through `serde_json` (no FFI cross-over); these tests exist to keep
    // the `karac_runtime_json_*` exports live so codegen wiring (deferred
    // to Slice B) inherits a settled ABI. Both round-trip the FFI shape
    // and exercise the error-line/col surface.

    #[test]
    fn test_karac_runtime_json_parse_roundtrip() {
        let input = std::ffi::CString::new("{\"a\": 1, \"b\": [true, null]}").unwrap();
        let mut err = KaracJsonError {
            line: 0,
            column: 0,
            message: std::ptr::null_mut(),
        };
        let tree = unsafe { karac_runtime_json_parse(input.as_ptr(), &mut err) };
        assert!(!tree.is_null(), "parse should succeed");
        let s_ptr = unsafe { karac_runtime_json_stringify(tree) };
        assert!(!s_ptr.is_null());
        let stringified = unsafe {
            std::ffi::CStr::from_ptr(s_ptr)
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(stringified, r#"{"a":1.0,"b":[true,null]}"#);
        unsafe {
            karac_runtime_json_free_string(s_ptr);
            karac_runtime_json_free_value(tree);
        }
    }

    #[test]
    fn test_karac_runtime_json_parse_error_surfaces_line_col() {
        // Malformed: `{"a": }` — the line is 1, column is the position
        // of `}` (column 7, 1-indexed). serde_json reports column at the
        // offending byte; we just sanity-check that line and column are
        // populated and the message is non-empty.
        let input = std::ffi::CString::new("{\"a\": }").unwrap();
        let mut err = KaracJsonError {
            line: 0,
            column: 0,
            message: std::ptr::null_mut(),
        };
        let tree = unsafe { karac_runtime_json_parse(input.as_ptr(), &mut err) };
        assert!(tree.is_null(), "parse should fail");
        assert_eq!(err.line, 1);
        assert!(err.column >= 1);
        assert!(!err.message.is_null());
        unsafe {
            karac_runtime_json_free_string(err.message);
        }
    }

    #[test]
    fn test_karac_runtime_json_object_preserves_insertion_order() {
        let input = std::ffi::CString::new("{\"z\":1,\"a\":2,\"m\":3}").unwrap();
        let mut err = KaracJsonError {
            line: 0,
            column: 0,
            message: std::ptr::null_mut(),
        };
        let tree = unsafe { karac_runtime_json_parse(input.as_ptr(), &mut err) };
        assert!(!tree.is_null());
        // The `preserve_order` feature on the runtime crate's
        // `serde_json` dep keeps the input ordering across the
        // `serde_json::Map` round-trip, satisfying locked design (ii).
        unsafe {
            let n = &*tree;
            assert_eq!(n.tag, KaracJsonTag::Object as u8);
            assert_eq!(n.obj_len, 3);
            let keys: Vec<String> = (0..n.obj_len)
                .map(|i| {
                    let k = *n.obj_keys.add(i);
                    std::ffi::CStr::from_ptr(k).to_string_lossy().into_owned()
                })
                .collect();
            assert_eq!(keys, vec!["z", "a", "m"]);
            karac_runtime_json_free_value(tree);
        }
    }

    // ── karac_par_reduce tests (slice 2, 2026-05-19) ──────────────────────
    //
    // These tests stub the codegen-emitted `init_slot` / `worker_fn` /
    // `combine_fn` directly in Rust so the runtime's dispatch path can be
    // exercised without standing up the full compiler pipeline. Each helper
    // mirrors what slice 3's codegen will emit per (op, type) pair.

    /// Identity-element init: write 0 into an i64 slot. Mirrors what
    /// codegen will emit for the `+` reduction on i64.
    unsafe extern "C" fn init_i64_zero(slot: *mut u8) {
        *(slot as *mut i64) = 0;
    }

    /// Identity-element init: write 1 into an i64 slot. For `*` reductions.
    unsafe extern "C" fn init_i64_one(slot: *mut u8) {
        *(slot as *mut i64) = 1;
    }

    /// Combine two i64 slots: `*dst += *src`. Mirrors the `+` op's combine.
    unsafe extern "C" fn combine_i64_add(dst: *mut u8, src: *const u8) {
        *(dst as *mut i64) += *(src as *const i64);
    }

    /// Combine two i64 slots: `*dst *= *src`. Mirrors the `*` op's combine.
    unsafe extern "C" fn combine_i64_mul(dst: *mut u8, src: *const u8) {
        *(dst as *mut i64) *= *(src as *const i64);
    }

    /// Worker function for the canonical "sum k for k in [start, end)"
    /// reduction. `ctx` is unused here (no captures); the kata-7 codegen
    /// will thread `inputs` and `reverse` through ctx in slice 3.
    unsafe extern "C" fn worker_sum_range(
        slot: *mut u8,
        start: u64,
        end: u64,
        _ctx: *mut c_void,
        _cancel: *const AtomicBool,
    ) {
        let mut acc: i64 = *(slot as *const i64);
        for k in start..end {
            acc += k as i64;
        }
        *(slot as *mut i64) = acc;
    }

    /// Worker function for "product (k+1) for k in [start, end)" — the
    /// `+1` keeps the seed value out of zero (otherwise a 0 in the range
    /// zeroes the entire product). Multiplicative reduction sanity check.
    unsafe extern "C" fn worker_product_range_plus_one(
        slot: *mut u8,
        start: u64,
        end: u64,
        _ctx: *mut c_void,
        _cancel: *const AtomicBool,
    ) {
        let mut acc: i64 = *(slot as *const i64);
        for k in start..end {
            acc *= (k as i64) + 1;
        }
        *(slot as *mut i64) = acc;
    }

    fn run_reduce(
        iter_total: u64,
        init: unsafe extern "C" fn(*mut u8),
        worker: unsafe extern "C" fn(*mut u8, u64, u64, *mut c_void, *const AtomicBool),
        combine: unsafe extern "C" fn(*mut u8, *const u8),
    ) -> i64 {
        let desc = KaracReduceDescriptor {
            iter_total,
            slot_size: std::mem::size_of::<i64>() as u64,
            slot_align: std::mem::align_of::<i64>() as u64,
            init_slot: init,
            worker_fn: worker,
            combine_fn: combine,
            ctx: std::ptr::null_mut(),
            // 0 sentinel: "no estimate" — bypasses the slice 3b.8 gate
            // so these dispatch-correctness tests cover the multi-worker
            // path regardless of N's nominal cost. The gate-behavior
            // tests below construct their own descriptors with real
            // per_iter values.
            per_iter_cost_units: 0,
        };
        let mut out: i64 = 0xDEAD_BEEF; // arbitrary sentinel — init must overwrite
        unsafe {
            karac_par_reduce(&desc, &mut out as *mut i64 as *mut u8, 0);
        }
        out
    }

    /// Layout pin for the wasm32 ABI contract: every integer field of
    /// `KaracReduceDescriptor` is `u64` (codegen stamps the struct with
    /// i64 fields for every target — `src/codegen/reduce.rs`'s
    /// `desc_ty`), so the `#[repr(C)]` layout is identical on 64-bit
    /// native and wasm32. A regression back to `usize` would shrink the
    /// wasm32 layout (i32-width fields) and silently misalign every
    /// field after the first against codegen's stores — this pin makes
    /// that a loud native test failure instead. Offsets assume the
    /// 64-bit host this test suite runs on: 4 × u64 + 3 fn-ptrs + 1
    /// ctx-ptr, all 8-byte slots.
    #[test]
    fn test_reduce_descriptor_layout_is_u64_pinned() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<KaracReduceDescriptor>(), 8 * 8);
        assert_eq!(offset_of!(KaracReduceDescriptor, iter_total), 0);
        assert_eq!(offset_of!(KaracReduceDescriptor, slot_size), 8);
        assert_eq!(offset_of!(KaracReduceDescriptor, slot_align), 16);
        assert_eq!(offset_of!(KaracReduceDescriptor, init_slot), 24);
        assert_eq!(offset_of!(KaracReduceDescriptor, worker_fn), 32);
        assert_eq!(offset_of!(KaracReduceDescriptor, combine_fn), 40);
        assert_eq!(offset_of!(KaracReduceDescriptor, ctx), 48);
        assert_eq!(offset_of!(KaracReduceDescriptor, per_iter_cost_units), 56);
    }

    /// 0-iter reduction returns the identity element (init_slot output).
    /// Catches the "skip dispatch but forget to seed out_slot" bug.
    #[test]
    fn test_par_reduce_zero_iter_returns_identity_add() {
        assert_eq!(
            run_reduce(0, init_i64_zero, worker_sum_range, combine_i64_add),
            0
        );
    }

    #[test]
    fn test_par_reduce_zero_iter_returns_identity_mul() {
        assert_eq!(
            run_reduce(
                0,
                init_i64_one,
                worker_product_range_plus_one,
                combine_i64_mul
            ),
            1
        );
    }

    /// 1-iter exercises the single-worker fast path (skip slot buffer +
    /// combine), which is a distinct code path from the multi-worker
    /// dispatch — regression locks it in.
    #[test]
    fn test_par_reduce_single_iter_add() {
        // Σ k for k in [0, 1) = 0
        assert_eq!(
            run_reduce(1, init_i64_zero, worker_sum_range, combine_i64_add),
            0
        );
    }

    /// Small iter count below pool size — runtime caps n_workers at
    /// `iter_total`, so each worker handles exactly one iteration.
    /// Same expected total as the serial sum.
    #[test]
    fn test_par_reduce_below_pool_size_add() {
        // Σ k for k in [0, 4) = 0 + 1 + 2 + 3 = 6
        let total = run_reduce(4, init_i64_zero, worker_sum_range, combine_i64_add);
        assert_eq!(total, 6);
    }

    /// Multi-worker dispatch over a range large enough that each worker
    /// owns a real chunk. Checks the chunking math + combine ordering
    /// against the closed-form serial sum.
    #[test]
    fn test_par_reduce_multi_worker_add_matches_serial() {
        let n = 100_000;
        let parallel_total = run_reduce(n, init_i64_zero, worker_sum_range, combine_i64_add);
        let serial_total: i64 = (0..n as i64).sum();
        assert_eq!(parallel_total, serial_total);
    }

    /// Multi-worker `*` reduction over a range. Multiplication is
    /// associative + commutative but order-sensitive enough that a
    /// mis-combined chunk would land a wildly wrong answer — small N
    /// keeps the product in i64 range. `(k+1) for k in [0, 12)` = 12! =
    /// 479_001_600.
    #[test]
    fn test_par_reduce_multi_worker_mul_matches_serial() {
        let n = 12;
        let parallel_total = run_reduce(
            n,
            init_i64_one,
            worker_product_range_plus_one,
            combine_i64_mul,
        );
        let serial_total: i64 = (1..=n as i64).product();
        assert_eq!(parallel_total, serial_total);
    }

    /// Stride math sanity check: a slot with non-default alignment still
    /// gets correctly aligned per-worker slots from the buffer. Pad to 16
    /// bytes — wider than i64's natural alignment — and verify the
    /// runtime respects it. (LLVM SIMD types ride in on this path
    /// eventually, and they're 16/32-byte aligned.)
    #[test]
    fn test_par_reduce_respects_oversized_alignment() {
        // We use 8-byte slot_size but request 16-byte alignment. Each
        // worker slot is stride=16 bytes in the buffer, but only the
        // first 8 bytes hold meaningful data.
        let desc = KaracReduceDescriptor {
            iter_total: 1000,
            slot_size: std::mem::size_of::<i64>() as u64,
            slot_align: 16,
            init_slot: init_i64_zero,
            worker_fn: worker_sum_range,
            combine_fn: combine_i64_add,
            ctx: std::ptr::null_mut(),
            per_iter_cost_units: 0,
        };
        // out_slot must be 16-byte aligned to match the descriptor;
        // allocate a 16-byte-aligned scratch slot via a wrapper struct.
        #[repr(C, align(16))]
        struct AlignedSlot([u8; 8]);
        let mut out = AlignedSlot([0u8; 8]);
        unsafe {
            karac_par_reduce(&desc, out.0.as_mut_ptr(), 0);
            let val = *(out.0.as_ptr() as *const i64);
            let expected: i64 = (0..1000i64).sum();
            assert_eq!(val, expected);
        }
    }

    /// A reduction whose iter range is exactly the pool worker count: one
    /// iteration per worker, the closing edge of the "below pool size"
    /// fast path becoming the multi-worker general path. Reads
    /// `resolve_pool_workers()` so the test tracks any `KARAC_PAR_WORKERS`
    /// override the harness sets — without that, the test would compute
    /// `n` from auto-detect and `karac_par_reduce` would use the env value,
    /// and the assertion `expected == sum(0..n)` would diverge from the
    /// runtime's actual chunk count.
    #[test]
    fn test_par_reduce_iter_equals_pool_size_add() {
        let n = super::resolve_pool_workers();
        let total = run_reduce(n as u64, init_i64_zero, worker_sum_range, combine_i64_add);
        let expected: i64 = (0..n as i64).sum();
        assert_eq!(total, expected);
    }

    // ── karac_par_reduce slice 3b.8: runtime-side cost gate ───────────
    //
    // The codegen-time gate (slice 3b.5) catches small-K loops when both
    // bounds are literals. Variable-K loops bypass that gate; the runtime-
    // side gate here catches them at call time using the per_iter cost
    // estimate threaded through the descriptor. These tests use an
    // AtomicUsize-counter worker to verify the gate path: 1 invocation
    // (sequential, init_slot + one worker_fn call) when gated, N
    // invocations (one per worker) when dispatched.

    use std::sync::atomic::AtomicUsize;

    /// Worker that increments the test-supplied AtomicUsize counter
    /// (passed via the descriptor's `ctx` pointer) and folds k as in
    /// `worker_sum_range`. Per-test counter ownership keeps the
    /// invocation count free of cargo-parallel-test interference.
    unsafe extern "C" fn worker_sum_range_counting(
        slot: *mut u8,
        start: u64,
        end: u64,
        ctx: *mut c_void,
        _cancel: *const AtomicBool,
    ) {
        let counter = &*(ctx as *const AtomicUsize);
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut acc: i64 = *(slot as *const i64);
        for k in start..end {
            acc += k as i64;
        }
        *(slot as *mut i64) = acc;
    }

    /// Build a descriptor with an explicit per_iter cost. Mirrors
    /// `run_reduce` but plumbs the new field and supplies a per-test
    /// counter via `ctx` so the gate path can be inspected by call-
    /// count without static state racing across parallel tests.
    fn run_reduce_with_per_iter(iter_total: u64, per_iter_cost_units: u64) -> (i64, usize) {
        let counter = AtomicUsize::new(0);
        let desc = KaracReduceDescriptor {
            iter_total,
            slot_size: std::mem::size_of::<i64>() as u64,
            slot_align: std::mem::align_of::<i64>() as u64,
            init_slot: init_i64_zero,
            worker_fn: worker_sum_range_counting,
            combine_fn: combine_i64_add,
            ctx: &counter as *const AtomicUsize as *mut c_void,
            per_iter_cost_units,
        };
        let mut out: i64 = 0xDEAD_BEEF;
        unsafe {
            karac_par_reduce(&desc, &mut out as *mut i64 as *mut u8, 0);
        }
        let calls = counter.load(std::sync::atomic::Ordering::SeqCst);
        (out, calls)
    }

    /// Gate fires: a small loop with a real per_iter estimate that
    /// puts total work below the runtime threshold runs the worker once
    /// on the caller's thread (sequential), not the multi-worker pool.
    /// K=100, per_iter=1 → 100 unit-iters << pool_workers × 10_000.
    #[test]
    fn test_par_reduce_runtime_gate_skips_dispatch_for_small_loop() {
        let (sink, calls) = run_reduce_with_per_iter(100, 1);
        assert_eq!(sink, (0..100i64).sum::<i64>());
        assert_eq!(
            calls, 1,
            "expected sequential single worker_fn call when total work < threshold; got {calls}"
        );
    }

    /// Gate skipped: same K but a fat per_iter cost (1_000_000 units —
    /// equivalent to a very expensive body) pushes total work above the
    /// threshold so the runtime dispatches normally. Calls = n_workers
    /// (capped at K, but K=100 > pool size on every dev machine).
    #[test]
    fn test_par_reduce_runtime_gate_dispatches_when_above_threshold() {
        let (sink, calls) = run_reduce_with_per_iter(100, 1_000_000);
        assert_eq!(sink, (0..100i64).sum::<i64>());
        let pool_workers = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .max(2);
        let expected_calls = pool_workers.min(100);
        assert_eq!(
            calls, expected_calls,
            "expected dispatch across pool workers when total work >= threshold; got {calls}, \
             expected {expected_calls}"
        );
    }

    /// Sentinel `per_iter_cost_units == 0` means "no estimate" — the
    /// runtime treats this as "always dispatch" so legacy / hand-built
    /// descriptors keep the current behavior. K=100, sentinel=0 →
    /// dispatched (calls > 1) even though the implied cost is zero.
    #[test]
    fn test_par_reduce_runtime_gate_sentinel_zero_bypasses_gate() {
        let (sink, calls) = run_reduce_with_per_iter(100, 0);
        assert_eq!(sink, (0..100i64).sum::<i64>());
        // Reads `resolve_pool_workers()` (not direct auto-detect) so the
        // test tracks the env var when it's set in the harness; otherwise
        // the assertion's `expected_calls` would diverge from the actual
        // dispatch count under `KARAC_PAR_WORKERS=N`.
        let pool_workers = super::resolve_pool_workers();
        let expected_calls = pool_workers.min(100);
        assert_eq!(
            calls, expected_calls,
            "per_iter=0 sentinel should bypass the gate; got {calls} calls, expected dispatch \
             across {expected_calls} workers"
        );
    }

    // ─── KARAC_PAR_WORKERS env override ──────────────────────────────
    //
    // Tests serialize on `PAR_WORKERS_ENV_LOCK` (peer of
    // `FRAME_TRACKING_ENV_LOCK` above) so cargo-parallel runs don't
    // race on the env var. Each test snapshots the prior value at
    // entry, mutates, runs the assertion, and restores — a panicking
    // assert leaves the mutex poisoned, which subsequent tests handle
    // via `.lock().unwrap_or_else(|p| p.into_inner())`.
    //
    // The pool is initialised lazily via `OnceLock`, so testing the
    // pool-construction path here would only fire once per process
    // and the env mutation would silently lose afterward. Tests
    // exercise `resolve_pool_workers()` directly (the helper both
    // `pool()` and `karac_par_reduce` call) — this verifies the
    // env-aware shape without coupling to the pool's lazy-init
    // lifecycle.

    static PAR_WORKERS_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_par_workers_env<F: FnOnce() -> R, R>(value: Option<&str>, body: F) -> R {
        let _guard = PAR_WORKERS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var("KARAC_PAR_WORKERS").ok();
        match value {
            Some(v) => std::env::set_var("KARAC_PAR_WORKERS", v),
            None => std::env::remove_var("KARAC_PAR_WORKERS"),
        }
        let result = body();
        match prior {
            Some(v) => std::env::set_var("KARAC_PAR_WORKERS", v),
            None => std::env::remove_var("KARAC_PAR_WORKERS"),
        }
        result
    }

    /// `KARAC_PAR_WORKERS=N` for a valid positive integer is honored
    /// exactly. Tested at N=4 (typical container-quota override) and
    /// N=1 (the lowest legal value — `pool()` uses this to drive
    /// `karac_par_reduce`'s single-worker fast path).
    #[test]
    fn test_resolve_pool_workers_honors_explicit_count() {
        with_par_workers_env(Some("4"), || {
            assert_eq!(super::resolve_pool_workers(), 4);
        });
        with_par_workers_env(Some("1"), || {
            assert_eq!(super::resolve_pool_workers(), 1);
        });
    }

    /// `KARAC_PAR_WORKERS=0` is invalid (the `n >= 1` guard rejects
    /// it) and falls back to the auto-detect default. Same shape as
    /// passing an unparseable value.
    #[test]
    fn test_resolve_pool_workers_invalid_value_falls_back_to_auto() {
        let auto = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(2);

        with_par_workers_env(Some("0"), || {
            assert_eq!(super::resolve_pool_workers(), auto);
        });
        with_par_workers_env(Some("bogus"), || {
            assert_eq!(super::resolve_pool_workers(), auto);
        });
        with_par_workers_env(Some(""), || {
            assert_eq!(super::resolve_pool_workers(), auto);
        });
    }

    /// With the env var unset, the resolver returns the auto-detect
    /// value floored at 2 — same shape as the pre-`KARAC_PAR_WORKERS`
    /// behaviour. Pins back-compat.
    #[test]
    fn test_resolve_pool_workers_unset_returns_auto_floored() {
        let auto = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(2);
        with_par_workers_env(None, || {
            assert_eq!(super::resolve_pool_workers(), auto);
            assert!(super::resolve_pool_workers() >= 2);
        });
    }

    // ── std.http client FFI tests (phase-8 line 17 slice 1) ────────────
    //
    // Stand up a 1-shot HTTP/1.1 origin in a background thread, serve
    // one canned response, then drive the client FFI against it. The
    // origin reads-until-blank-line-pair to demarcate end-of-request
    // headers (good enough for the GET path) or until Content-Length
    // bytes have been consumed (good enough for the POST path); no real
    // parser. Each test allocates a fresh ephemeral port so the tests
    // can run in parallel without colliding.
    //
    // libc::free for the malloc'd out-buffers comes via the local
    // `extern "C" { fn free(...); }` decl below so test cleanup
    // matches the production caller-side ownership contract.

    #[cfg(feature = "tls")]
    extern "C" {
        fn free(ptr: *mut std::os::raw::c_void);
    }

    /// Bind an ephemeral-port server that accepts exactly one connection,
    /// reads the request, and writes `canned_response` back. Returns the
    /// bound port. Spawns a detached thread; the thread terminates after
    /// the single response is written. POST requests are recognized via
    /// the `Content-Length` header so the origin reads the full entity
    /// before responding.
    #[cfg(feature = "tls")]
    fn spawn_oneshot_origin(canned_response: &'static [u8]) -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let mut headers_end = None;
                let mut total = 0usize;
                while headers_end.is_none() && total < buf.len() {
                    let n = match stream.read(&mut buf[total..]) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    total += n;
                    if let Some(pos) = buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
                        headers_end = Some(pos + 4);
                    }
                }
                if let Some(end) = headers_end {
                    let headers = &buf[..end];
                    let header_text = std::str::from_utf8(headers).unwrap_or("");
                    let mut content_length = 0usize;
                    for line in header_text.split("\r\n") {
                        let lower = line.to_ascii_lowercase();
                        if let Some(rest) = lower.strip_prefix("content-length:") {
                            content_length = rest.trim().parse().unwrap_or(0);
                            break;
                        }
                    }
                    let already_have = total.saturating_sub(end);
                    let mut remaining = content_length.saturating_sub(already_have);
                    while remaining > 0 {
                        let cap = remaining.min(buf.len());
                        match stream.read(&mut buf[..cap]) {
                            Ok(0) => break,
                            Ok(n) => remaining -= n,
                            Err(_) => return,
                        }
                    }
                }
                let _ = stream.write_all(canned_response);
                let _ = stream.flush();
            }
        });
        port
    }

    /// Helper to read an out-param byte buffer (malloc'd by the runtime)
    /// into an owned Vec for assertions, then libc::free it to match the
    /// production caller-side ownership contract.
    #[cfg(feature = "tls")]
    unsafe fn take_owned_buffer(ptr: *mut u8, len: i64) -> Vec<u8> {
        if ptr.is_null() || len <= 0 {
            return Vec::new();
        }
        let slice = std::slice::from_raw_parts(ptr, len as usize);
        let v = slice.to_vec();
        free(ptr as *mut std::os::raw::c_void);
        v
    }

    /// Phase-8 line 48 — real HTTPS round-trip against a public
    /// CA-signed host (`https://example.com/`), gated `#[ignore]` so
    /// stock `cargo test` stays hermetic. The CI job
    /// `stripped-image-https` invokes this with `--include-ignored`
    /// inside an Alpine container whose `ca-certificates` package has
    /// been removed AFTER build — proving the runtime's embedded
    /// `webpki-roots` (not any system CA bundle) is what validated the
    /// peer cert. A non-200 response is acceptable (example.com may
    /// redirect or change body); a `status == 0` is a hard fail because
    /// it means the TLS handshake or cert verification did not complete.
    #[test]
    #[ignore = "network HTTPS round-trip; runs in the stripped-image-https CI job via --include-ignored"]
    #[cfg(feature = "tls")]
    fn test_https_round_trip_against_public_origin() {
        let url = "https://example.com/";
        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_get(
                url.as_ptr(),
                url.len(),
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        if status == 0 {
            let msg = if err_ptr.is_null() {
                String::new()
            } else {
                String::from_utf8_lossy(&unsafe { take_owned_buffer(err_ptr, err_len) })
                    .into_owned()
            };
            panic!(
                "HTTPS round-trip failed: status=0, err={msg:?}; \
                 expected any HTTP status (cert verification via embedded webpki-roots must succeed)"
            );
        }
        assert!(
            (200..400).contains(&status),
            "expected 2xx/3xx from example.com, got status {status}"
        );
        if !body_ptr.is_null() {
            unsafe {
                take_owned_buffer(body_ptr, body_len);
            }
        }
    }

    /// Phase-8 line 48 — pin the shared `ureq::Agent`'s explicit
    /// webpki-roots config. ureq's `tls` feature transitively brings
    /// webpki-roots in today, but we build our own `ClientConfig` (per
    /// `http_client_agent`) so a future ureq default flip to
    /// `rustls-native-certs` can't silently move us off the bundled
    /// Mozilla Root program. Asserts: (1) the Mozilla root bundle is
    /// non-empty (proves the `webpki-roots` dep is reachable, not a
    /// dead crate), and (2) `http_client_agent()` returns the same
    /// `&'static` agent across calls (proves the `OnceLock` cache is
    /// what's serving every HTTPS request — single shared TLS config,
    /// not a fresh build per fetch).
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_agent_uses_explicit_webpki_roots_and_is_shared() {
        assert!(
            !webpki_roots::TLS_SERVER_ROOTS.is_empty(),
            "webpki-roots Mozilla bundle must be reachable; an empty bundle would mean every HTTPS request rejects every cert"
        );
        let a = super::http_client_agent();
        let b = super::http_client_agent();
        assert!(
            std::ptr::eq(a, b),
            "http_client_agent must return the OnceLock-cached agent; a fresh build per call would mean per-request TLS handshake state setup"
        );
    }

    /// GET happy path — origin returns 200 OK with `hello` body; FFI
    /// reports status = 200, body buffer carries `hello`, no error.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_get_success_populates_status_and_body() {
        let canned = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
        let port = spawn_oneshot_origin(canned);
        let url = format!("http://127.0.0.1:{port}/");

        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_get(
                url.as_ptr(),
                url.len(),
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 200);
        assert_eq!(body_len, 5);
        assert!(err_ptr.is_null());
        assert_eq!(err_len, 0);
        let body = unsafe { take_owned_buffer(body_ptr, body_len) };
        assert_eq!(body, b"hello");
    }

    /// Phase-8 line 32 — `Response.bytes()` raw-byte path. The origin
    /// returns a body containing invalid-UTF-8 bytes (`0xFF 0xFE 0x00
    /// 0x41`). Pre-fix, the FFI ran the body through
    /// `into_string().unwrap_or_default()`, so a non-UTF-8 body collapsed
    /// to an empty buffer (`body_len == 0`) and `Response.bytes()` would
    /// surface nothing. With `read_response_body_bytes`, the body buffer
    /// carries the four bytes verbatim — which is what lets binary
    /// downloads (images / protobuf / file transfers) round-trip.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_get_returns_raw_bytes_for_non_utf8_body() {
        let canned: &[u8] =
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n\xff\xfe\x00\x41";
        let port = spawn_oneshot_origin(canned);
        let url = format!("http://127.0.0.1:{port}/");

        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_get(
                url.as_ptr(),
                url.len(),
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 200);
        assert_eq!(
            body_len, 4,
            "non-UTF-8 body must survive verbatim, not collapse to empty"
        );
        assert!(err_ptr.is_null());
        let body = unsafe { take_owned_buffer(body_ptr, body_len) };
        assert_eq!(body, vec![0xFFu8, 0xFE, 0x00, 0x41]);
    }

    /// Phase-8 line 39 — `Response.header(name)` capture + lookup. The
    /// origin returns a custom response header; the GET FFI stashes the
    /// response headers in the side-table and reports the keying handle
    /// through `out_headers_handle`. `karac_runtime_http_response_header`
    /// resolves it case-insensitively (RFC 7230 §3.2), returns null for
    /// an absent name, and returns null for the Err-path sentinel
    /// handle `0`.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_get_captures_response_headers_for_lookup() {
        let canned = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Custom: custom-value\r\nConnection: close\r\n\r\nok";
        let port = spawn_oneshot_origin(canned);
        let url = format!("http://127.0.0.1:{port}/");

        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_get(
                url.as_ptr(),
                url.len(),
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 200);
        assert!(headers_handle > 0, "Ok response must mint a headers handle");
        let _ = unsafe { take_owned_buffer(body_ptr, body_len) };

        // Case-insensitive hit: query with different case than the wire.
        let name = b"x-CUSTOM";
        let val_ptr = unsafe {
            super::karac_runtime_http_response_header(headers_handle, name.as_ptr(), name.len())
        };
        assert!(!val_ptr.is_null(), "present header must resolve");
        let val = unsafe { std::ffi::CStr::from_ptr(val_ptr) }
            .to_str()
            .unwrap();
        assert_eq!(val, "custom-value");

        // Absent header → null.
        let absent = b"x-missing";
        let miss_ptr = unsafe {
            super::karac_runtime_http_response_header(headers_handle, absent.as_ptr(), absent.len())
        };
        assert!(miss_ptr.is_null(), "absent header must resolve to null");

        // Unknown handle (the Err-path sentinel `0`) → null.
        let zero_ptr =
            unsafe { super::karac_runtime_http_response_header(0, name.as_ptr(), name.len()) };
        assert!(
            zero_ptr.is_null(),
            "handle 0 (error sentinel) must resolve to null"
        );
    }

    /// Phase-8 line 39 follow-up — `Response.headers()` iteration
    /// accessors. After a GET, `_headers_count` reports the captured
    /// header count and `_header_key_at` / `_val_at` walk the `(name,
    /// value)` pairs in order. Pins that the custom header round-trips
    /// through the indexed surface and that an out-of-range index / the
    /// Err-sentinel handle `0` resolve to null.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_get_response_headers_iteration() {
        let canned = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Custom: custom-value\r\nConnection: close\r\n\r\nok";
        let port = spawn_oneshot_origin(canned);
        let url = format!("http://127.0.0.1:{port}/");

        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_get(
                url.as_ptr(),
                url.len(),
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 200);
        assert!(headers_handle > 0);
        let _ = unsafe { take_owned_buffer(body_ptr, body_len) };

        let count = super::karac_runtime_http_response_headers_count(headers_handle);
        assert!(
            count >= 1,
            "expected at least the custom header; got {count}"
        );

        // Walk the pairs and confirm the custom header is present.
        let mut found = false;
        for i in 0..count {
            let k_ptr = super::karac_runtime_http_response_header_key_at(headers_handle, i);
            let v_ptr = super::karac_runtime_http_response_header_val_at(headers_handle, i);
            assert!(
                !k_ptr.is_null() && !v_ptr.is_null(),
                "in-range pair non-null"
            );
            let k = unsafe { std::ffi::CStr::from_ptr(k_ptr) }.to_str().unwrap();
            let v = unsafe { std::ffi::CStr::from_ptr(v_ptr) }.to_str().unwrap();
            if k.eq_ignore_ascii_case("x-custom") {
                assert_eq!(v, "custom-value");
                found = true;
            }
        }
        assert!(found, "x-custom should appear in the iterated headers");

        // Out-of-range index → null.
        assert!(
            super::karac_runtime_http_response_header_key_at(headers_handle, count).is_null(),
            "out-of-range key index must be null"
        );
        // Unknown handle → count 0, null fields.
        assert_eq!(super::karac_runtime_http_response_headers_count(0), 0);
        assert!(super::karac_runtime_http_response_header_val_at(0, 0).is_null());
    }

    /// Phase-8 line 39 follow-up — `karac_runtime_http_response_headers_free`
    /// releases the side-table entry (what the synthesized `Response` Drop
    /// calls at scope exit), and is idempotent/total: 0, unknown, and
    /// double-free are no-ops.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_response_headers_free_removes_entry() {
        let handle = 990_001_i64;
        {
            let mut map = super::HTTP_RESPONSE_HEADERS.lock().unwrap();
            map.insert(
                handle,
                vec![(
                    std::ffi::CString::new("x-test").unwrap(),
                    std::ffi::CString::new("v").unwrap(),
                )],
            );
        }
        assert!(super::HTTP_RESPONSE_HEADERS
            .lock()
            .unwrap()
            .contains_key(&handle));
        super::karac_runtime_http_response_headers_free(handle);
        assert!(
            !super::HTTP_RESPONSE_HEADERS
                .lock()
                .unwrap()
                .contains_key(&handle),
            "free must remove the entry"
        );
        // Idempotent / total — none of these panic or corrupt.
        super::karac_runtime_http_response_headers_free(handle); // double-free
        super::karac_runtime_http_response_headers_free(0); // Err-path sentinel
        super::karac_runtime_http_response_headers_free(123_456_789); // unknown
    }

    /// Phase-8 line 39 follow-up — `karac_runtime_http_builder_free`
    /// releases an abandoned (never-sent) `HTTP_BUILDERS` entry, mirroring
    /// the response-headers free. Idempotent on 0 / unknown / already-sent.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_builder_free_removes_entry() {
        let method = b"GET";
        let url = b"http://127.0.0.1:1/";
        let handle = unsafe {
            super::karac_runtime_http_builder_new(
                method.as_ptr(),
                method.len(),
                url.as_ptr(),
                url.len(),
            )
        };
        assert!(handle > 0, "builder_new should mint a positive handle");
        assert!(super::HTTP_BUILDERS.lock().unwrap().contains_key(&handle));
        super::karac_runtime_http_builder_free(handle);
        assert!(
            !super::HTTP_BUILDERS.lock().unwrap().contains_key(&handle),
            "builder_free must remove the entry"
        );
        // Idempotent / total.
        super::karac_runtime_http_builder_free(handle);
        super::karac_runtime_http_builder_free(0);
        super::karac_runtime_http_builder_free(987_654_321);
    }

    /// Transport-failure path — connect to a port nothing's listening
    /// on. FFI reports status = 0 (the discriminant the caller-side
    /// codegen uses to build `Result.Err`), no body buffer, and an
    /// error-message buffer carrying ureq's display text.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_get_transport_error_populates_err() {
        // Bind a port and immediately drop the listener; the OS keeps
        // the port unbound long enough for the connect attempt below to
        // fail with `ECONNREFUSED` (the exact wording varies — we only
        // assert the message is non-empty).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}/");

        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_get(
                url.as_ptr(),
                url.len(),
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 0);
        assert!(body_ptr.is_null());
        assert_eq!(body_len, 0);
        assert!(err_len > 0);
        let err = unsafe { take_owned_buffer(err_ptr, err_len) };
        assert!(!err.is_empty());
    }

    /// POST round-trip — entity body is forwarded to the origin, which
    /// echoes a fixed response. Pins the body-send code path.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_post_sends_body_and_returns_response() {
        let canned =
            b"HTTP/1.1 201 Created\r\nContent-Length: 7\r\nConnection: close\r\n\r\ncreated";
        let port = spawn_oneshot_origin(canned);
        let url = format!("http://127.0.0.1:{port}/items");
        let body = b"name=widget";

        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_post(
                url.as_ptr(),
                url.len(),
                body.as_ptr(),
                body.len(),
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 201);
        assert_eq!(body_len, 7);
        assert!(err_ptr.is_null());
        let resp_body = unsafe { take_owned_buffer(body_ptr, body_len) };
        assert_eq!(resp_body, b"created");
    }

    // ── Chained-builder FFI tests (phase-8 line 24) ───────────────────
    //
    // `karac_runtime_http_client_send` is the chained-builder backend —
    // accepts method + headers + timeout where `_get` / `_post` don't.
    // Tests use a capture-aware origin helper so the assertion can pin
    // the bytes the client actually sent (method, header lines, body).

    /// Bind an ephemeral-port server that accepts exactly one connection,
    /// captures the request bytes into the supplied `Arc<Mutex<Vec<u8>>>`,
    /// then writes `canned_response` back. Same end-of-headers detection
    /// as `spawn_oneshot_origin` plus a Content-Length-bounded body read,
    /// so the captured buffer contains the full request line + headers +
    /// body when the caller used either GET (no body) or a Content-Length-
    /// declaring verb.
    #[cfg(feature = "tls")]
    fn spawn_capturing_origin(
        canned_response: &'static [u8],
        captured: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ) -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let mut headers_end = None;
                let mut total = 0usize;
                while headers_end.is_none() && total < buf.len() {
                    let n = match stream.read(&mut buf[total..]) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    total += n;
                    if let Some(pos) = buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
                        headers_end = Some(pos + 4);
                    }
                }
                if let Some(end) = headers_end {
                    let headers = &buf[..end];
                    let header_text = std::str::from_utf8(headers).unwrap_or("");
                    let mut content_length = 0usize;
                    for line in header_text.split("\r\n") {
                        let lower = line.to_ascii_lowercase();
                        if let Some(rest) = lower.strip_prefix("content-length:") {
                            content_length = rest.trim().parse().unwrap_or(0);
                            break;
                        }
                    }
                    let already_have = total.saturating_sub(end);
                    let mut remaining = content_length.saturating_sub(already_have);
                    while remaining > 0 {
                        let cap = remaining.min(buf.len() - total);
                        if cap == 0 {
                            break;
                        }
                        match stream.read(&mut buf[total..total + cap]) {
                            Ok(0) => break,
                            Ok(n) => {
                                total += n;
                                remaining -= n;
                            }
                            Err(_) => return,
                        }
                    }
                }
                if let Ok(mut guard) = captured.lock() {
                    guard.extend_from_slice(&buf[..total]);
                }
                let _ = stream.write_all(canned_response);
                let _ = stream.flush();
            }
        });
        port
    }

    /// Build a `KaracStr` borrowing the bytes of `s`. The lifetime of the
    /// returned `KaracStr` is bound to `s` — callers must keep the source
    /// `String` alive for the duration of the FFI call. `cap` matches
    /// `len` since the runtime doesn't read it for input strings.
    #[cfg(feature = "tls")]
    fn kara_str_for(s: &str) -> super::KaracStr {
        super::KaracStr {
            data: s.as_ptr(),
            len: s.len() as i64,
            cap: s.len() as i64,
        }
    }

    /// send() happy path with a custom request header: the chained
    /// builder's `.header("X-Custom", "abc")` is reflected in the wire
    /// bytes the origin sees, and the response status / body propagate
    /// back through the out-params identically to `_get`.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_send_forwards_custom_headers_to_origin() {
        let canned = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let port = spawn_capturing_origin(canned, std::sync::Arc::clone(&captured));
        let url = format!("http://127.0.0.1:{port}/test");

        let key = String::from("x-custom");
        let val = String::from("abc");
        let pair = super::KaracHttpHeaderPair {
            key: kara_str_for(&key),
            val: kara_str_for(&val),
        };
        let headers = [pair];

        let method = "GET";
        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_send(
                method.as_ptr(),
                method.len(),
                url.as_ptr(),
                url.len(),
                std::ptr::null(),
                0,
                headers.as_ptr(),
                headers.len(),
                0,
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 200);
        assert_eq!(body_len, 5);
        let body = unsafe { take_owned_buffer(body_ptr, body_len) };
        assert_eq!(body, b"hello");

        let wire = captured.lock().unwrap().clone();
        let wire_text = String::from_utf8_lossy(&wire).to_lowercase();
        assert!(
            wire_text.contains("get /test"),
            "expected GET /test in captured wire bytes, got: {wire_text}"
        );
        assert!(
            wire_text.contains("x-custom: abc"),
            "expected x-custom header in captured wire bytes, got: {wire_text}"
        );
    }

    /// send() POST path: the chained builder routes a `body("...")` call
    /// through the request entity even though headers may also be set.
    /// Pins both knobs at once: the origin sees `POST /api` with the
    /// custom header AND the body in the request entity.
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_send_post_carries_body_and_headers() {
        let canned = b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let port = spawn_capturing_origin(canned, std::sync::Arc::clone(&captured));
        let url = format!("http://127.0.0.1:{port}/api");

        let key = String::from("x-trace-id");
        let val = String::from("trace-42");
        let pair = super::KaracHttpHeaderPair {
            key: kara_str_for(&key),
            val: kara_str_for(&val),
        };
        let headers = [pair];
        let body = b"hello world";

        let method = "POST";
        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_send(
                method.as_ptr(),
                method.len(),
                url.as_ptr(),
                url.len(),
                body.as_ptr(),
                body.len(),
                headers.as_ptr(),
                headers.len(),
                0,
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 201);
        assert_eq!(body_len, 2);
        let resp = unsafe { take_owned_buffer(body_ptr, body_len) };
        assert_eq!(resp, b"ok");

        let wire = captured.lock().unwrap().clone();
        let wire_text = String::from_utf8_lossy(&wire).to_lowercase();
        assert!(
            wire_text.contains("post /api"),
            "expected POST /api in captured wire bytes, got: {wire_text}"
        );
        assert!(
            wire_text.contains("x-trace-id: trace-42"),
            "expected x-trace-id header in captured wire bytes, got: {wire_text}"
        );
        assert!(
            wire_text.contains("hello world"),
            "expected body in captured wire bytes, got: {wire_text}"
        );
    }

    /// Handle-based builder happy path: new + add_header + set_body +
    /// set_timeout + send round-trips a POST through the capturing
    /// origin. Pins both the wire-side state (origin sees the verb +
    /// custom header + body) and the entry-lifecycle invariant
    /// (HTTP_BUILDERS no longer contains the handle after `_send`).
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_builder_send_consumes_handle_and_round_trips_state() {
        let canned = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhi-fu";
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let port = spawn_capturing_origin(canned, std::sync::Arc::clone(&captured));
        let url = format!("http://127.0.0.1:{port}/api/items");

        let method = "POST";
        let handle = unsafe {
            super::karac_runtime_http_builder_new(
                method.as_ptr(),
                method.len(),
                url.as_ptr(),
                url.len(),
            )
        };
        assert!(handle > 0, "expected positive handle, got {handle}");

        let key = b"x-trace-id";
        let val = b"trace-99";
        unsafe {
            super::karac_runtime_http_builder_add_header(
                handle,
                key.as_ptr(),
                key.len(),
                val.as_ptr(),
                val.len(),
            );
        }
        let body = b"hi-server";
        unsafe {
            super::karac_runtime_http_builder_set_body(handle, body.as_ptr(), body.len());
        }
        unsafe {
            super::karac_runtime_http_builder_set_timeout(handle, 5000);
        }

        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_builder_send(
                handle,
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 200);
        assert_eq!(body_len, 5);
        let response_body = unsafe { take_owned_buffer(body_ptr, body_len) };
        assert_eq!(response_body, b"hi-fu");

        let wire = captured.lock().unwrap().clone();
        let wire_text = String::from_utf8_lossy(&wire).to_lowercase();
        assert!(
            wire_text.contains("post /api/items"),
            "expected POST in wire bytes: {wire_text}"
        );
        assert!(
            wire_text.contains("x-trace-id: trace-99"),
            "expected x-trace-id header in wire bytes: {wire_text}"
        );
        assert!(
            wire_text.contains("hi-server"),
            "expected body in wire bytes: {wire_text}"
        );

        // Handle is consumed by `_send` — calling `_send` again with the
        // same handle yields the unknown-handle error path, not a
        // duplicate request.
        let mut status2: i64 = -1;
        let mut body_ptr2: *mut u8 = std::ptr::null_mut();
        let mut body_len2: i64 = -1;
        let mut err_ptr2: *mut u8 = std::ptr::null_mut();
        let mut err_len2: i64 = -1;
        let mut headers_handle2: i64 = -1;
        unsafe {
            super::karac_runtime_http_builder_send(
                handle,
                &mut status2,
                &mut body_ptr2,
                &mut body_len2,
                &mut err_ptr2,
                &mut err_len2,
                &mut headers_handle2,
            );
        }
        assert_eq!(status2, 0);
        assert!(err_len2 > 0);
        let err = unsafe { take_owned_buffer(err_ptr2, err_len2) };
        let err_text = String::from_utf8_lossy(&err);
        assert!(
            err_text.contains("unknown"),
            "expected unknown-handle err text: {err_text}"
        );
    }

    /// send() timeout path: pointing at a host that accepts the TCP
    /// connection but never responds. The runtime's ureq timeout fires
    /// at the configured deadline and yields the transport-error
    /// out-params (status = 0, error-message buffer populated).
    #[test]
    #[cfg(feature = "tls")]
    fn test_http_client_send_timeout_returns_transport_error() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accept the connection but block reading without ever responding;
        // ureq's read timeout fires before the test thread joins.
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 64];
                let _ = stream.read(&mut buf);
                std::thread::sleep(std::time::Duration::from_millis(1500));
            }
        });
        let url = format!("http://127.0.0.1:{port}/");

        let method = "GET";
        let mut status: i64 = -1;
        let mut body_ptr: *mut u8 = std::ptr::null_mut();
        let mut body_len: i64 = -1;
        let mut err_ptr: *mut u8 = std::ptr::null_mut();
        let mut err_len: i64 = -1;
        let mut headers_handle: i64 = -1;
        unsafe {
            super::karac_runtime_http_client_send(
                method.as_ptr(),
                method.len(),
                url.as_ptr(),
                url.len(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                200,
                &mut status,
                &mut body_ptr,
                &mut body_len,
                &mut err_ptr,
                &mut err_len,
                &mut headers_handle,
            );
        }
        assert_eq!(status, 0, "timeout should yield status = 0");
        assert!(body_ptr.is_null());
        assert!(err_len > 0, "expected error-message buffer on timeout");
        let err = unsafe { take_owned_buffer(err_ptr, err_len) };
        assert!(!err.is_empty());
    }
}
