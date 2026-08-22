//! Cached runtime-function declarations.
//!
//! First slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! 66 `FunctionValue` handles for the C and `karac_*` runtime entry points
//! a compiled module may call — `printf`, `malloc`, the map/string/tracing
//! /provider families, and so on.
//!
//! This is the lowest-risk cluster in the whole refactor and deliberately
//! the first: every field is **declared once** by `Codegen::new` and only
//! ever read afterwards (verified: no assignment to any of them exists
//! outside the constructor), so the group carries no subsystem semantics
//! and no ordering constraints. It is a pure lookup cache that happened to
//! be parked in the god struct.
//!
//! Accessed as `self.runtime_fns.<name>` from the sibling
//! `impl Codegen` modules.

use inkwell::values::FunctionValue;

/// Declare-once handles for the runtime functions a module may call.
pub(crate) struct RuntimeFns<'ctx> {
    /// Runtime contract-predicate-context FFI (design.md § Contracts rule 2).
    /// `emit_contract_assert` brackets a predicate's *runtime* evaluation with
    /// `karac_runtime_enter_predicate()` / `karac_runtime_exit_predicate()` (a
    /// thread-local depth counter in the runtime), and `emit_panic` reads
    /// `karac_runtime_panic_prefix()` to choose its fault category. A panic that
    /// fires while the depth is non-zero — whether an inline bounds/div/unwrap
    /// check lexically inside the predicate (`requires v[i] >= 0`) OR a panic
    /// inside a function the predicate transitively *calls* — is the distinct
    /// `contract predicate panicked: <msg>` fault, not `contract violated`
    /// (reserved for the predicate evaluating to `false`, where the depth is
    /// back to 0). The runtime flag subsumes the prior compile-time flag: it
    /// sees cross-call panics a lexical flag cannot, matching the interpreter's
    /// global `pending_cf` behavior. The depth is a counter, not a bool, so a
    /// predicate that calls a function with its own contract nests correctly.
    pub(crate) karac_runtime_enter_predicate_fn: FunctionValue<'ctx>,
    pub(crate) karac_runtime_exit_predicate_fn: FunctionValue<'ctx>,
    pub(crate) karac_runtime_panic_prefix_fn: FunctionValue<'ctx>,
    pub(crate) printf_fn: FunctionValue<'ctx>,
    /// `int snprintf(char* buf, size_t n, const char* fmt, ...)` — used by f-string
    /// codegen to convert integers and floats to their decimal string forms.
    pub(crate) snprintf_fn: FunctionValue<'ctx>,
    /// `size_t fwrite(const void* ptr, size_t size, size_t nmemb, FILE* stream)` —
    /// the NUL-safe string-print primitive (L5). Unlike `printf("%.*s")`, which
    /// stops at the first interior NUL even with a precision, `fwrite` writes
    /// exactly `len` bytes. It shares libc's stdio buffer with the `printf`
    /// int/bool paths, so output ordering across mixed prints is preserved.
    /// `void karac_runtime_write_console(ptr data, size_t len, ptr stream)` —
    /// the runtime console-write chokepoint every print path funnels through
    /// (auto-par ordered-output). At the top level it `fwrite`s to `stream`
    /// (byte-for-byte the old inline path); inside a parallel branch it records
    /// the bytes for ordered replay at the join so parallelized logging-bearing
    /// work keeps sequential output order. `size_t`-width `len` (i32 wasm32 /
    /// i64 native) matches the runtime's `usize` parameter.
    pub(crate) write_console_fn: FunctionValue<'ctx>,
    /// B-2026-07-30-9 — `__karac_write_console_line(data, len, nl, nl_len,
    /// stream)`: stage a payload and its trailing newline into ONE buffer and
    /// hand them to [`Self::write_console_fn`] as a single write, so a `println`
    /// is line-atomic.
    ///
    /// `println` used to emit two `write_console` calls, and the lock that keeps
    /// a write intact — glibc's per-`FILE` lock inside `fwrite` — is released
    /// between them. Two `spawn`ed tasks printing concurrently therefore
    /// interleaved as payload-A, payload-B, newline-A, newline-B, i.e. `12\n\n`
    /// instead of `1\n2\n`.
    pub(crate) write_console_line_fn: FunctionValue<'ctx>,
    /// malloc function for heap allocation.
    pub(crate) malloc_fn: FunctionValue<'ctx>,
    /// `karac_alloc_fallible(size) -> ptr` — non-null on success, null on OOM
    /// (phase-8-stdlib-floor item 8). The `try_*` collection companions call
    /// this and branch on null to build `Result.Err(AllocError)`.
    pub(crate) alloc_fallible_fn: FunctionValue<'ctx>,
    /// `karac_alloc_or_panic(size) -> ptr` — the panicking counterpart that
    /// aborts on OOM instead of returning null. The panicking collection
    /// methods (`Vec.with_capacity`, `Vec.from_slice`, grow paths) route
    /// through it so OOM is a clean abort, not a null-deref segfault.
    pub(crate) alloc_or_panic_fn: FunctionValue<'ctx>,
    /// free function for heap deallocation.
    pub(crate) free_fn: FunctionValue<'ctx>,
    /// `karac_free_buf(ptr, bytes_hint)` — recycling-aware release for
    /// Vec/String DATA buffers (`runtime/src/alloc.rs` large-buffer cache).
    /// Emitted at the buffer-release sites that own a `{data, len, cap}`
    /// heap buffer (scope-exit `FreeVecBuffer` drain, overwrite-free,
    /// synthesized Vec/String drop fns); everything else stays on `free_fn`.
    /// `bytes_hint` is `cap * elem_size` when the site knows the element
    /// size, else `0` = "unknown, runtime asks the allocator" — a wrong
    /// hint can only cost a recycling opportunity, never correctness.
    pub(crate) free_buf_fn: FunctionValue<'ctx>,
    /// exit function for runtime panics.
    pub(crate) exit_fn: FunctionValue<'ctx>,
    /// memcmp for string comparison.
    pub(crate) memcmp_fn: FunctionValue<'ctx>,
    /// `int sched_yield(void)` — POSIX thread-yield primitive. Phase 6
    /// line 26 slice 8e wires this into the caller-side network-boundary
    /// intercept's Pending path so the parent thread cooperatively
    /// yields to the OS scheduler / dispatcher between poll-fn
    /// invocations instead of busy-looping. Linked from libc (same
    /// path as malloc / free). Windows IOCP support (line 17 sub-item 7)
    /// will need a `SwitchToThread` analog; v1 targets Linux / macOS
    /// where sched_yield is available.
    pub(crate) sched_yield_fn: FunctionValue<'ctx>,
    /// Runtime entry point `void karac_par_run(const KaracBranch*, usize)`.
    pub(crate) karac_par_run_fn: FunctionValue<'ctx>,
    /// `karac_par_run_auto` — same ABI as `karac_par_run`, routed to by
    /// compiler-DERIVED parallel regions (auto-par statement groups). Runs
    /// branches inline when the calling thread is already inside a par
    /// worker at the fork-depth cap (B-2026-08-17-14).
    pub(crate) karac_par_run_auto_fn: FunctionValue<'ctx>,
    /// Runtime entry point `void karac_par_reduce(*const KaracReduceDescriptor,
    /// *mut u8 out_slot, u32 spawn_site_id)`. Declared in slice 3a, called
    /// from slice 3b's `src/codegen/reduce.rs::emit_reduce_call`. See
    /// `runtime/src/lib.rs`'s `karac_par_reduce` for the ABI.
    pub(crate) karac_par_reduce_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_provider_push(frame_ptr, resource_id, data_ptr, vtable_ptr)`.
    /// Consumed by `with_provider[R]` lowering (sub-step 3).
    pub(crate) karac_provider_push_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_provider_pop()`. Consumed by `with_provider[R]`
    /// lowering (sub-step 3) for the matching pop on body exit.
    pub(crate) karac_provider_pop_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_provider_lookup(resource_id) -> ProviderLookupResult`.
    /// Consumed by `R.method(...)` dispatch (sub-step 4) to find the
    /// active provider's data pointer and vtable.
    pub(crate) karac_provider_lookup_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_provider_get_stack_head() -> *const ProviderFrame`.
    /// Consumed by par-block lowering (sub-step 5) to snapshot the
    /// calling thread's stack head into the par-block env-struct.
    pub(crate) karac_provider_get_stack_head_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_provider_set_stack_head(head)`. Consumed
    /// by par-branch fn prologues (sub-step 5) to seed each worker
    /// thread's TLS from the env-struct snapshot, so providers in
    /// scope at the par-block site stay visible inside spawned branches.
    pub(crate) karac_provider_set_stack_head_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_get_active_span() -> i64` (phase-8
    /// line 153). Consumed by the `tracing_active_span()` builtin (which
    /// `Log.*` / `LogEvent` use to auto-stamp the ambient span) and by
    /// the `with_span` lowering to snapshot the prior active span.
    pub(crate) karac_tracing_get_active_span_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_set_active_span(i64)` (phase-8 line
    /// 153). Consumed by the `with_span(span, ||body)` lowering to install
    /// the body's active span and restore the prior one on exit, and by
    /// par-branch prologues to inherit the parent's active span.
    pub(crate) karac_tracing_set_active_span_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_get_min_level() -> i64` (phase-8 line
    /// 156, codegen half). The `tracing_level_enabled(rank)` builtin lowers
    /// to `rank >= this`, so a compiled `Log.*` honors `Log.set_min_level`.
    pub(crate) karac_tracing_get_min_level_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_set_min_level(i64)` (phase-8 line
    /// 156). The `tracing_set_min_level(rank)` builtin (called from
    /// `Log.set_min_level`'s lowered body) writes the process-global level.
    pub(crate) karac_tracing_set_min_level_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_set_exporter(*const u8, *const u8)`
    /// (phase-8 line 156). The `tracing_set_exporter(e)` builtin registers
    /// the heap-leaked exporter value + its `export_event` fn-ptr here.
    pub(crate) karac_tracing_set_exporter_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_get_exporter_data() -> *const u8`
    /// (phase-8 line 156). The `tracing_emit_event` lowering branches on
    /// this (null → default `StdoutExporter`, else indirect-dispatch).
    pub(crate) karac_tracing_get_exporter_data_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_get_exporter_fn() -> *const u8`
    /// (phase-8 line 156). The registered sink's `export_event` fn-ptr, used
    /// by the `tracing_emit_event` lowering for the indirect call.
    pub(crate) karac_tracing_get_exporter_fn_fn: FunctionValue<'ctx>,
    /// Runtime extern: `karac_tracing_reset()` (phase-8 line 156). Clears
    /// the min level and registered sink; `Log.reset`'s body lowers to it.
    pub(crate) karac_tracing_reset_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_new_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_free_fn: FunctionValue<'ctx>,
    /// `karac_map_free_with_drop_vec(map: ptr, drop_key: i32, drop_val: i32)`
    /// — `karac_map_free` variant that recursively drops per-entry
    /// Vec/String content before deallocating the bucket storage.
    /// `drop_key != 0` releases each live entry's key data buffer when
    /// the key follows the `{ptr, len, cap}` layout; `drop_val != 0`
    /// does the same for the value. Selected by the `FreeMapHandle`
    /// cleanup arm whenever either flag is set. Replaces the narrower
    /// `karac_map_free_with_val_drop_vec` (val-only) helper that
    /// shipped 2026-05-13.
    ///
    /// Closes leaks for `Set[Vec[T]]` / `Set[String]` (key drop only),
    /// `Map[String, V]` / `Map[Vec[T], V]` (key drop only),
    /// `Map[String, Vec[U]]` / `Map[Vec[T], Vec[U]]` (both flags). The
    /// primitive-only `Map[i64, i64]` case stays on plain
    /// `karac_map_free` for zero overhead.
    pub(crate) karac_map_free_with_drop_vec_fn: FunctionValue<'ctx>,
    /// `karac_map_free_with_val_drop_fn(map: ptr, drop_key: i32,
    /// val_drop_fn: ptr)` — slice 3r (deferred gap (d)): frees each live
    /// entry's VALUE via a synthesized `karac_drop_<T>(ptr)` (values that
    /// aren't the one-level Vec/String overlay: user structs/enums, inner
    /// Maps/Sets, Option/Result, Vec-with-heap-elements). Key side keeps
    /// the flag contract of `karac_map_free_with_drop_vec`.
    pub(crate) karac_map_free_with_val_drop_fn_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_insert_old_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_try_insert_fn: FunctionValue<'ctx>,
    /// Borrowed-String-key insert: deep-copies the key only on a fresh
    /// insertion, so a slice-into-source key (`m.insert(s[a..b], v)`)
    /// allocates once per distinct key instead of once per call.
    pub(crate) karac_map_insert_borrowed_str_old_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_get_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_remove_old_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_contains_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_len_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_clear_fn: FunctionValue<'ctx>,
    /// `karac_map_clear_with_drop_vec(map, drop_key, drop_val)` — clear that
    /// frees heap key/value buffers first (peer of
    /// `karac_map_free_with_drop_vec`); selected for heap-keyed/valued maps.
    pub(crate) karac_map_clear_with_drop_vec_fn: FunctionValue<'ctx>,
    /// `karac_map_clear_with_val_drop_fn(map, drop_key, val_drop_fn)` — the
    /// clear sibling of `karac_map_free_with_val_drop_fn` (slice 3r).
    pub(crate) karac_map_clear_with_val_drop_fn_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_iter_new_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_sorted_keys_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_iter_next_fn: FunctionValue<'ctx>,
    pub(crate) karac_map_iter_free_fn: FunctionValue<'ctx>,
    /// `i64 karac_string_decode_char(*const u8 data, i64 len, i64 byte_offset, *mut u32 out_cp)`.
    /// Returns the byte offset after the decoded char and writes the
    /// codepoint through the out-param. Drives `for c in s` / `for c in
    /// s.chars()` lowering — see `compile_for_string_chars`.
    pub(crate) karac_string_decode_char_fn: FunctionValue<'ctx>,
    /// `i64 karac_string_encode_char(u32 cp, *mut u8 out)`. Writes 1–4
    /// UTF-8 bytes for the codepoint through `out`, returns the byte
    /// count. Peer of `karac_string_decode_char_fn`; used by the print
    /// and f-string char arms to render the glyph rather than the
    /// integer codepoint. See `emit_codepoint_to_utf8`.
    pub(crate) karac_string_encode_char_fn: FunctionValue<'ctx>,
    /// `karac_map_entry(map: ptr, key: ptr, out_slot_ptr: ptr) -> i1` —
    /// probe-and-insert-on-vacant. Used by entry chains whose terminal is
    /// `or_insert` / `or_insert_with` — codegen will write a default through
    /// the slot when occupied=false, so the runtime claims the bucket up
    /// front.
    pub(crate) karac_map_entry_fn: FunctionValue<'ctx>,
    /// `karac_map_lookup_slot(map: ptr, key: ptr, out_slot_ptr: ptr) -> i1`
    /// — read-only variant used by entry chains whose terminal is
    /// `and_modify`. The closure runs only when occupied=true; nothing is
    /// inserted on the Vacant path.
    pub(crate) karac_map_lookup_slot_fn: FunctionValue<'ctx>,
    /// `karac_string_clone(src: ptr, dst: ptr) -> void` — runtime helper
    /// for the codegen-emitted String case in `emit_clone_fn_for_type_expr`.
    /// Allocates a fresh buffer, copies len bytes, writes the new
    /// `{data, len, cap}` to `dst`. Static-literal sources (cap = 0) get
    /// a heap-owned copy so scope-exit cleanup fires; source untouched.
    pub(crate) karac_string_clone_fn: FunctionValue<'ctx>,
    pub(crate) karac_string_slice_fn: FunctionValue<'ctx>,
    /// `karac_string_slice_borrow(data, len, start, end) -> ptr` — validating,
    /// non-allocating slice; returns `data + start`. Backs borrowed
    /// `{ptr, len, cap=0}` String views used as non-retained map keys.
    pub(crate) karac_string_slice_borrow_fn: FunctionValue<'ctx>,
    /// Allocating String→String transforms (full Unicode, matching the
    /// interpreter's Rust stdlib). Each `(data, len, *mut out_len) -> ptr`
    /// returns a fresh NUL-terminated buffer and writes the result byte length
    /// to `out_len` (null + 0 for an empty result). See `runtime/src/clone.rs`.
    /// `karac_unicode_normalize(data, len, form, out_len) -> ptr` —
    /// `String.normalize(form)`. Opt-in `libkarac_runtime_unicode.a` only;
    /// see `driver.rs § SpecialArchive::Unicode`.
    pub(crate) karac_unicode_normalize_fn: FunctionValue<'ctx>,
    pub(crate) karac_string_to_lowercase_fn: FunctionValue<'ctx>,
    pub(crate) karac_string_to_uppercase_fn: FunctionValue<'ctx>,
    pub(crate) karac_string_trim_fn: FunctionValue<'ctx>,
    /// `String.trim_start()` / `.trim_end()` — strip only leading / trailing
    /// Unicode whitespace. Same `(data, len, *mut out_len) -> ptr` xform shape
    /// as `trim`.
    pub(crate) karac_string_trim_start_fn: FunctionValue<'ctx>,
    pub(crate) karac_string_trim_end_fn: FunctionValue<'ctx>,
    /// `String.sorted()` — chars sorted ascending into a fresh String (the
    /// anagram key). Same `(data, len, *mut out_len) -> ptr` xform shape.
    pub(crate) karac_string_sorted_fn: FunctionValue<'ctx>,
    /// `karac_string_replace(data, len, from, from_len, to, to_len, *mut out_len)
    /// -> ptr` — every `from` replaced with `to` (Rust `str::replace`).
    pub(crate) karac_string_replace_fn: FunctionValue<'ctx>,
    /// `karac_string_replacen(data, len, from, from_len, to, to_len, n, *mut out_len)
    /// -> ptr` — first `n` `from` replaced with `to` (Rust `str::replacen`).
    pub(crate) karac_string_replacen_fn: FunctionValue<'ctx>,
    // ── Error return trace runtime ────────────────────────────────
    /// `void karac_error_trace_push(ptr file, i64 file_len, i32 line, i32 col)`.
    /// Called by `compile_question` at each `?` failure block before
    /// `emit_scope_cleanup`. The runtime maintains a thread-local depth-64
    /// ring buffer; an atexit handler prints it to stderr at program exit.
    pub(crate) karac_error_trace_push_fn: FunctionValue<'ctx>,
    /// `void karac_error_trace_clear()`. Emitted at every `?` success site
    /// so a recovered earlier propagation doesn't leak frames into a later
    /// failure.
    pub(crate) karac_error_trace_clear_fn: FunctionValue<'ctx>,
    /// `void karac_test_record_failure(ptr file, i64 file_len, i32 line, i32 col,
    /// ptr msg, i64 msg_len, ptr left, i64 left_len, ptr right, i64 right_len)`.
    /// Lowered `assert` / `assert_eq` / `assert_ne` failure path calls this then
    /// `exit(1)`. The runtime writes a `KARAC_TEST_FAILURE {...JSON...}` line to
    /// stderr; `cmd_test` (Slice c.3) parses the line into a `TestOutcome`.
    pub(crate) karac_test_record_failure_fn: FunctionValue<'ctx>,
}
