//! Closure-lowering state: fat-pointer types, heap-env ownership tracking,
//! and the compile-time staging cells.
//!
//! Everything `Codegen` keeps about closures as VALUES: the per-binding fat
//! pointer function types, the shared escape analysis (`escape` — producer
//! sets + per-function owner tables, computed in `crate::closure_escape`,
//! the plain-AST module the `escaping_closure` check lint shares so the
//! build gate and the check diagnostic can never drift, B-2026-08-16-13),
//! the per-function set of locals that own a heap env, the emission-side
//! owner-field map `neutralize_moved_aggregate_env_slots` reads, and the
//! two `pending_closure_*` staging cells `compile_closure` threads to the
//! surrounding `let`. Extracted from `Codegen` as a cluster-15 sub-slice of
//! the state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.

use rustc_hash::FxHashMap;

use inkwell::types::{BasicTypeEnum, FunctionType};

use crate::ownership::{CapturePath, OwnershipMode};
use crate::resolver::SpanKey;

pub(crate) struct ClosureState<'ctx> {
    // ── Closure compilation ────────────────────────────────────────
    /// Monotonic counter used to generate unique closure function names.
    pub(crate) closure_counter: u32,
    /// Maps local variable names that hold closure fat-pointers to their LLVM function type.
    /// Required for indirect calls: `build_indirect_call` needs the callee's function type.
    pub(crate) closure_fn_types: std::collections::HashMap<String, FunctionType<'ctx>>,
    /// The shared escape analysis (B-2026-08-16-13): the five heap-env
    /// producer sets (`fns_returning_heap_env*` — functions whose return
    /// value IS or OWNS a reference-counted heap closure environment,
    /// directly or inside a struct / tuple / array / `Vec[Fn]`), plus the
    /// CURRENT function's owner tables (`curry_closure_vars`,
    /// `heap_env_{aggregate,tuple,array,vec}_owners`), rebuilt by
    /// `EscapeAnalysis::check_function` at the top of `compile_function`.
    /// Emission reads these to wire `FreeClosureEnv` drops and to sanction
    /// owner-scoped uses exactly as when the fields lived flat on this
    /// struct; the analysis itself (validators + fixpoints) lives in
    /// `crate::closure_escape` so `karac check` runs the same predicate.
    pub(crate) escape: crate::closure_escape::EscapeAnalysis,
    /// Local bindings in the current function that own a heap-env closure (a
    /// `FreeClosureEnv` was registered). Used to reject not-yet-supported
    /// escapes of such a binding (return / copy / store / pass — Slice 1 is
    /// call-only) and reset per function.
    pub(crate) heap_env_closure_vars: std::collections::HashSet<String>,
    /// Per-function map (reset each function): an aggregate-owner local `h` → the
    /// `(struct type name, field index)` of each heap-env field it owns. Recorded
    /// when the field's `FreeClosureEnv` is registered (struct-literal store OR an
    /// aggregate-returning call result). Used by `neutralize_moved_aggregate_env_slots`
    /// to runtime-null those field env slots when `h` is moved out via a return, so
    /// the callee's field drop no-ops and the box flows to the caller at the same
    /// refcount (the aggregate analog of `neutralize_moved_closure_env_slot`).
    pub(crate) heap_env_owner_fields: std::collections::HashMap<String, Vec<(String, u32)>>,
    /// Staging slot — set by `compile_closure` so the surrounding `let` binding can record
    /// the function type under the newly bound name.
    pub(crate) pending_closure_fn_type: Option<FunctionType<'ctx>>,
    /// Staging slot — caller-supplied LLVM types for a closure's parameters,
    /// consulted by `compile_closure` when the source has no type annotation
    /// to refine. Used by `Vec.sort_by` to push the element type into
    /// `|a, b|` closures so tuple receivers don't collapse to bare `i64`.
    /// Taken once and cleared on entry to `compile_closure`.
    pub(crate) pending_closure_param_hints: Option<Vec<BasicTypeEnum<'ctx>>>,
    /// Per-closure capture path modes sourced from
    /// `OwnershipCheckResult::closure_capture_path_modes` — line 353
    /// phase-5 checklist disjoint-capture slice 4. When a closure
    /// expression's `SpanKey` lives in this map, `compile_closure` lays
    /// the env struct out with one slot per captured `CapturePath` (each
    /// sized to the leaf type at the projection chain end) and stitches
    /// the leaf values back into a fresh root alloca in the synthesized
    /// closure body. Absent → fall back to the per-name
    /// `collect_closure_free_vars` layout (preserves the `compile_to_ir`
    /// path and any codegen-only tests that don't run the ownership
    /// pass).
    pub(crate) closure_capture_paths: FxHashMap<SpanKey, Vec<(CapturePath, OwnershipMode)>>,
}
