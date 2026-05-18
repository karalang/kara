//! RAII-across-yield typechecker pass.
//!
//! Implements the v1 rule from design.md § Network Event Loop and
//! State-Machine Transform > RAII Across Yield Points: a network-boundary
//! function cannot hold a non-cancel-safe binding live across any yield
//! point. The rule fires per yield-point reusing slice-4's
//! `state_struct_layouts` data (which already records the v1
//! over-approximation of bindings live across each function's yield set).
//!
//! ## v1 scope — closed enumeration
//!
//! Slice 1 of phase 6 line 31. Detects the unambiguous "definitely not
//! cancel-safe" cases the typechecker can determine without flow analysis:
//!
//! - **Shared structs (`shared struct N { ... }`)** held across a yield.
//!   The design.md v1 NOT-CancelSafe set lists `shared struct` / `shared
//!   enum` with `Rc`-rooted reachability — drop-under-cancellation can
//!   leave referenced heap state in an undefined intermediate cleanup
//!   state, so users must release the binding before yielding or opt the
//!   type into a `CancelSafe` impl with a manually-audited Drop.
//! - **Shared enums (`shared enum N { ... }`)** symmetric to shared
//!   structs.
//!
//! ## What slice 1 does NOT do
//!
//! - The `CancelSafe` marker trait — slice 2 will add the user-extensible
//!   `pub marker trait CancelSafe;` per the design spec, alongside the
//!   stdlib `CancelSafe` set (primitives; String/collection types;
//!   Box/Arc/Rc when wrapped-type-CancelSafe; lock guards; network
//!   connection types).
//! - Flow-sensitive detection (`File` before fsync; `BufReader` while
//!   buffer non-empty; database transaction handles pre-commit) — these
//!   need the stdlib types to exist first plus a per-binding live-range
//!   pass that tracks observed state changes.
//! - Raw pointer detection (`*const T` / `*mut T`) — these are part of
//!   the v1 NOT-CancelSafe set per the spec but don't currently appear
//!   in `pattern_binding_types`, so detecting them needs a richer type
//!   threading slice that lands alongside the marker trait.
//! - Precise binding-construction span anchoring — slice 1 uses the
//!   yield-point span as the primary anchor + the function name in the
//!   message; slice 3 will thread the binding pattern's introducing
//!   span through to the diagnostic.

use crate::ast::*;
use crate::token::Span;
use crate::typechecker::TypeCheckResult;

/// Diagnostic emitted by [`check_raii_across_yield`]. One per
/// (binding × function) pair that holds a non-cancel-safe binding
/// across a yield point.
#[derive(Debug, Clone)]
pub struct RaiiAcrossYieldError {
    /// Function key (free fn `name` or `Type.method`) carrying the
    /// violation. Same shape as `Program.state_struct_layouts` keys.
    pub fn_key: String,
    /// Source-level name of the captured binding (parameter, `let`,
    /// pattern binding) that the state-machine transform would need to
    /// preserve across at least one yield point in the function body.
    pub binding_name: String,
    /// Surface type name as recorded by the typechecker
    /// (`TypeCheckResult.pattern_binding_types`), used in the diagnostic
    /// message ("holding 'binding' (type 'TypeName')").
    pub type_name: String,
    /// Span of the first yield-point call site in the function body —
    /// the suspension boundary the binding cannot live across. Slice 1
    /// anchors the diagnostic here; slice 3 will additionally surface
    /// the binding's introducing pattern span as a secondary highlight.
    pub yield_span: Span,
}

impl RaiiAcrossYieldError {
    /// Human-readable diagnostic body. The code prefix
    /// `error[E_RAII_ACROSS_YIELD]` is added by the diagnostic formatter
    /// in `src/cli.rs` (mirrors the other phase error types — they each
    /// expose the body, the formatter prepends the namespaced code).
    pub fn message(&self) -> String {
        format!(
            "holding `{}` (type `{}`) across a suspension point in `{}` is not cancel-safe",
            self.binding_name, self.type_name, self.fn_key,
        )
    }

    /// Trailing diagnostic note explaining the cancel-leak hazard.
    /// Matches the wording from design.md § RAII Across Yield Points.
    pub fn note(&self) -> &'static str {
        "dropping `{}` while the task is parked at a yield point would run its destructor under \
         cancellation, leaving owned resources in an undefined intermediate state"
    }

    /// Fix-it hint pointing at the two remediation paths from the design
    /// spec: release the binding before the yield, or opt the type into
    /// the `CancelSafe` marker (slice 2 adds the marker itself; slice 1
    /// surfaces the suggestion text up front so users see the migration
    /// shape even before the marker lands).
    pub fn help(&self) -> String {
        format!(
            "release `{}` before the yield, or `impl CancelSafe for {}` once the type's `Drop` \
             impl is audited to be safe under cancellation",
            self.binding_name, self.type_name,
        )
    }
}

/// Run the RAII-across-yield check over `program`. Returns a flat list
/// of `RaiiAcrossYieldError`s — one per (binding × function) pair that
/// holds a non-cancel-safe binding across at least one yield point.
///
/// Reads `program.state_struct_layouts` (populated by slice 4 in
/// `Pipeline::effectcheck`) for the per-function captured-locals
/// union, and `program.yield_points` (populated by slice 2) for the
/// suspension-site anchor span. The `types` argument carries the
/// typechecker's `struct_info` / `enum_info` which classify each
/// surface type name as `is_shared` vs not.
///
/// When `types` is `None` (parse-only pipeline, no typecheck run),
/// returns an empty error list — the check can't classify types
/// without the typechecker's index, and the pipeline shape today
/// only invokes this pass when typecheck succeeded.
pub fn check_raii_across_yield(
    program: &Program,
    types: Option<&TypeCheckResult>,
) -> Vec<RaiiAcrossYieldError> {
    let Some(types) = types else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    for (fn_key, layout) in &program.state_struct_layouts {
        // Need at least one yield point in the function for the
        // diagnostic to point at a suspension site. Slice 4's presence
        // rule guarantees this — a function only gets a layout entry
        // when it has at least one yield-point call — but defensively
        // skip if the yield_points table is missing the entry (e.g.
        // tests that build only one side-table without the other).
        let Some(yps) = program.yield_points.get(fn_key) else {
            continue;
        };
        let Some(first_yp) = yps.first() else {
            continue;
        };
        for field in &layout.fields {
            let Some(ref type_name) = field.type_name else {
                continue;
            };
            if is_not_cancel_safe_v1(type_name, types) {
                errors.push(RaiiAcrossYieldError {
                    fn_key: fn_key.clone(),
                    binding_name: field.name.clone(),
                    type_name: type_name.clone(),
                    yield_span: first_yp.span.clone(),
                });
            }
        }
    }
    errors
}

/// v1 closed enumeration of NOT-cancel-safe surface type names.
///
/// Slice 1 detects: `shared struct N` / `shared enum N` (via the
/// typechecker's `is_shared` flag) — the unambiguous Rc-rooted
/// reachability case from the design spec's v1 NOT-CancelSafe set.
/// All other surface types fall through to "cancel-safe by default"
/// at v1; the marker-trait slice will flip the default to "opt-in
/// cancel-safe" so the v1 stdlib set lives in code rather than as
/// negative space.
fn is_not_cancel_safe_v1(type_name: &str, types: &TypeCheckResult) -> bool {
    if let Some(info) = types.struct_info.get(type_name) {
        if info.is_shared {
            return true;
        }
    }
    if let Some(info) = types.enum_info.get(type_name) {
        if info.is_shared {
            return true;
        }
    }
    false
}
