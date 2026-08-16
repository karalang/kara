//! Diagnostic-instrumentation state — panic sites, spans, error traces.
//!
//! Fifth slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Small and
//! isolated: the state behind the crash- and error-diagnostic surfaces.
//!
//! - `current_span` — the source span of the expression being compiled,
//!   set at the top of `compile_expr` and read by `emit_panic` for Level 2
//!   crash diagnostics (design.md § Crash diagnostics);
//! - `panic_site_counter` — names the per-site outlined panic bodies
//!   `emit_panic` creates. A `Cell` because `emit_panic` takes `&self`;
//! - `strip_error_trace` — elides the `?`-error-return-trace
//!   instrumentation (peer to `strip_contracts`), so a release build pays
//!   zero `?`-site cost;
//! - `runtime_panic_prefix_needed` — whether the module must fetch the
//!   runtime's panic-category prefix.
//!
//! Accessed as `self.tracing.<name>` from the sibling `impl Codegen`
//! modules.

/// Panic-site, span and error-trace instrumentation state.
pub(crate) struct Tracing {
    /// When `true`, the `?`-error-return-trace instrumentation is elided: no
    /// `karac_error_trace_push` at `?` failure sites, no `karac_error_trace_clear`
    /// on the success path. The trace is a debug-only diagnostic, so a release
    /// build pays zero `?`-site cost (peer to `strip_contracts`). Defaults from
    /// `read_strip_error_trace_env` (`KARAC_STRIP_ERROR_TRACE`) at construction;
    /// `set_strip_error_trace` overrides it (the `release` build path forces it
    /// on alongside contract stripping). The gate lives at the two emission
    /// sites in `compile_expr`'s `?` lowering.
    pub(crate) strip_error_trace: bool,
    /// Whether `emit_panic` must read the fault-category prefix from the
    /// runtime (`karac_runtime_panic_prefix()`) rather than folding it to the
    /// static `""`. Set at the top of `compile_program`: `true` when the
    /// program declares any contract (`requires` / `ensures` / `invariant`,
    /// scanned across free fns, impl methods, trait methods, and struct
    /// invariants by `program_declares_contracts`) and contracts aren't
    /// stripped, or when compiling a REPL cell module (`main_symbol_override`
    /// set — a cell can call contracted functions JIT'd from earlier cells,
    /// which this module's item scan can't see; per-test `main` modules ride
    /// the same entry point and signal). When `false`, no predicate bracket
    /// can ever run in-process, the depth counter is statically 0, and the
    /// prefix is always `""` — `emit_panic` skips the runtime call, so (a)
    /// the `karac_runtime_panic_prefix` symbol and the writable thread-local
    /// `__DATA` page it drags into the link dead-strip from contract-free
    /// binaries (+16 KiB per binary), and (b) panic landing pads stay
    /// static-string leaves instead of blocks with a live call (the
    /// unconditional call regressed a bounds-check-hot loop 1.34× —
    /// kata-5 longest-palindromic-substring, 2026-06-05). Defaults `true`
    /// (conservative: any path that bypasses `compile_program` keeps the
    /// always-correct runtime read).
    pub(crate) runtime_panic_prefix_needed: bool,
    /// Monotonic counter naming the per-site outlined panic bodies
    /// (`__karac_panic_site_<n>`) `emit_panic` creates — see its doc for why
    /// panic bodies are outlined. `Cell` because `emit_panic` is `&self`.
    pub(crate) panic_site_counter: std::cell::Cell<u32>,
    /// Source span of the expression currently being compiled. Set at the top
    /// of `compile_expr`; read by `emit_panic` for Level 2 crash diagnostics
    /// (design.md § Crash diagnostics) — `panic at <file>:<line>:<col> in
    /// <fn>: <msg>`. `Span` already carries 1-indexed `line`/`column`, so no
    /// byte-offset resolution is needed. `None` until the first expression is
    /// compiled (synthetic panics with no originating expression fall back to
    /// the bare `panic: <msg>` form).
    pub(crate) current_span: Option<crate::token::Span>,
}
