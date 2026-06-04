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
}
