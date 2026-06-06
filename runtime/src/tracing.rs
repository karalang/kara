//! Ambient active-span register for `std.tracing` (phase-8 line 153,
//! Phase 1).
//!
//! The active span is the span that log events attach to without the
//! programmer threading a `Span` value by hand. It is a per-task
//! ambient value — `design.md:6143` ("the runtime maintains a span
//! context per logical task"). Phase 1 represents it as the active
//! span's `span_id` (`i64`, `0` = no active span); the fuller
//! `(trace_id, span_id, task_id)` triple is an additive follow-on once
//! `Span` grows those fields.
//!
//! Storage mirrors the per-task provider stack head
//! (`PROVIDER_STACK_HEAD`): a `thread_local!` `Cell<i64>`. The
//! `with_span(span, || body)` construct snapshots the current value,
//! installs `span.span_id` for the dynamic extent of the body, and
//! restores the snapshot on exit (codegen + interpreter lower this).
//! Propagation across concurrency boundaries reuses the same machinery
//! the provider head uses: par-block workers inherit the parent's value
//! via an env-struct snapshot (Phase 1); a coroutine preserves it across
//! a suspend/resume via a frame slot (Phase 2).
//!
//! A `thread_local!` is correct here for the same reason it is for the
//! provider head: the active span is read on the worker thread that runs
//! the task, and the propagation sites explicitly carry the value to
//! wherever the task next runs.

use std::cell::Cell;

thread_local! {
    /// The active span id on this worker thread (`0` = no active span).
    static ACTIVE_SPAN: Cell<i64> = const { Cell::new(0) };
}

/// Read the active span id for the current thread (`0` if none). Codegen
/// lowers the `tracing_active_span()` builtin to this; `Log.*` /
/// `LogEvent` stamp it onto events that carry no explicit `in_span`.
#[no_mangle]
pub extern "C" fn karac_tracing_get_active_span() -> i64 {
    ACTIVE_SPAN.with(|c| c.get())
}

/// Set the active span id for the current thread. `with_span` snapshots
/// the prior value via [`karac_tracing_get_active_span`], installs the
/// new one here for the body, then restores the snapshot — and par-block
/// workers call this at branch entry to inherit the parent's active span.
#[no_mangle]
pub extern "C" fn karac_tracing_set_active_span(span_id: i64) {
    ACTIVE_SPAN.with(|c| c.set(span_id));
}

// ── Configurable ambient logging (phase-8 line 156, codegen half) ─────
//
// Two pieces of *process-global* config back the compiled `Log.*` surface
// (`Log.set_min_level` / `set_exporter` / `reset`), mirroring what the
// interpreter half holds on the `Interpreter` struct:
//
//   * a minimum level (rank `trace`=0 .. `error`=4) — `Log.*` below it are
//     dropped before emitting;
//   * a registered sink — an opaque `(data, export_event_fn)` pair. `data`
//     is a pointer to the heap-leaked exporter value; `export_event_fn` is
//     a pointer to that exporter type's lowered `export_event(ref self,
//     LogEvent)`. The runtime only *stores* these — it never dereferences
//     `data` nor calls the fn-ptr. The compiled `tracing_emit_event`
//     lowering emits the indirect call (it owns the LLVM call signature),
//     falling back to the default `StdoutExporter` when `data` is null.
//
// These are genuine process globals (atomics), not thread-locals: the spec
// calls the minimum level "process-global", and a registered sink set on
// `main`'s thread must be observed by tasks running on any worker thread —
// so unlike the active span (which is per-task and explicitly propagated
// across concurrency boundaries) there is nothing to snapshot/restore at a
// par/spawn or suspend boundary. The registered exporter value is leaked
// for the process lifetime, so the stored `data` pointer never dangles; a
// custom exporter dispatched from multiple worker threads is the user's
// concern to make sound, exactly as a global subscriber is in Rust's
// `tracing` (v1 contract).

use std::sync::atomic::{AtomicI64, AtomicPtr, Ordering};

/// Process-global minimum log level (rank `trace`=0 .. `error`=4; default
/// 0 = emit everything). `Log.set_min_level` writes it via
/// [`karac_tracing_set_min_level`]; each compiled `Log.*` reads it.
static MIN_LEVEL: AtomicI64 = AtomicI64::new(0);

/// Pointer to the heap-leaked registered exporter value (`null` = no sink
/// registered → compiled `Log.*` uses the default `StdoutExporter`).
static EXPORTER_DATA: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

/// Pointer to the registered exporter type's lowered `export_event`
/// (`null` when no sink is registered). Stored opaquely; the compiled
/// `tracing_emit_event` lowering performs the indirect call.
static EXPORTER_FN: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

/// Read the process-global minimum log level. The compiled
/// `tracing_level_enabled(rank)` builtin lowers to `rank >= this`.
#[no_mangle]
pub extern "C" fn karac_tracing_get_min_level() -> i64 {
    MIN_LEVEL.load(Ordering::Relaxed)
}

/// Set the process-global minimum log level. `Log.set_min_level` lowers to
/// this after mapping the level name to its rank (an unrecognized name
/// never reaches here, so the threshold is left unchanged — matching the
/// interpreter half).
#[no_mangle]
pub extern "C" fn karac_tracing_set_min_level(level: i64) {
    MIN_LEVEL.store(level, Ordering::Relaxed);
}

/// Register the ambient sink: `data` points at the heap-leaked exporter
/// value, `export_fn` at its lowered `export_event`. Both opaque here.
/// `Log.set_exporter` lowers to this.
#[no_mangle]
pub extern "C" fn karac_tracing_set_exporter(data: *const u8, export_fn: *const u8) {
    EXPORTER_DATA.store(data as *mut u8, Ordering::Relaxed);
    EXPORTER_FN.store(export_fn as *mut u8, Ordering::Relaxed);
}

/// Read the registered exporter's data pointer (`null` = none). The
/// compiled `tracing_emit_event` branches on this: null → default
/// `StdoutExporter`, non-null → indirect-call [`karac_tracing_get_exporter_fn`].
#[no_mangle]
pub extern "C" fn karac_tracing_get_exporter_data() -> *const u8 {
    EXPORTER_DATA.load(Ordering::Relaxed)
}

/// Read the registered exporter's `export_event` fn pointer (`null` =
/// none). Only dereferenced (as an indirect call) by compiled code when
/// [`karac_tracing_get_exporter_data`] is non-null.
#[no_mangle]
pub extern "C" fn karac_tracing_get_exporter_fn() -> *const u8 {
    EXPORTER_FN.load(Ordering::Relaxed)
}

/// Restore defaults: minimum level `trace` (rank 0) and no registered sink
/// (the leaked exporter value, if any, stays leaked). `Log.reset` lowers
/// to this.
#[no_mangle]
pub extern "C" fn karac_tracing_reset() {
    MIN_LEVEL.store(0, Ordering::Relaxed);
    EXPORTER_DATA.store(std::ptr::null_mut(), Ordering::Relaxed);
    EXPORTER_FN.store(std::ptr::null_mut(), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_active_span_is_zero() {
        // Fresh thread (cargo runs each test on its own thread) → 0.
        assert_eq!(karac_tracing_get_active_span(), 0);
    }

    #[test]
    fn set_then_get_round_trips() {
        karac_tracing_set_active_span(42);
        assert_eq!(karac_tracing_get_active_span(), 42);
        // Restore-to-prior dance, as `with_span` performs it.
        let prev = karac_tracing_get_active_span();
        karac_tracing_set_active_span(7);
        assert_eq!(karac_tracing_get_active_span(), 7);
        karac_tracing_set_active_span(prev);
        assert_eq!(karac_tracing_get_active_span(), 42);
    }

    // The config globals are process-wide, so the whole set/get/reset
    // cycle lives in one test to avoid racing a sibling test on the shared
    // atomics (no other test touches them). Reset up-front so the assertion
    // on defaults holds regardless of run order.
    #[test]
    fn config_set_get_reset_round_trips() {
        karac_tracing_reset();
        assert_eq!(karac_tracing_get_min_level(), 0);
        assert!(karac_tracing_get_exporter_data().is_null());
        assert!(karac_tracing_get_exporter_fn().is_null());

        karac_tracing_set_min_level(3); // warn
        assert_eq!(karac_tracing_get_min_level(), 3);

        let data = 0xdead_beef_usize as *const u8;
        let func = 0x1234_5678_usize as *const u8;
        karac_tracing_set_exporter(data, func);
        assert_eq!(karac_tracing_get_exporter_data(), data);
        assert_eq!(karac_tracing_get_exporter_fn(), func);

        karac_tracing_reset();
        assert_eq!(karac_tracing_get_min_level(), 0);
        assert!(karac_tracing_get_exporter_data().is_null());
        assert!(karac_tracing_get_exporter_fn().is_null());
    }
}
