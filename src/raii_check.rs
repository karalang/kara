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
//! ## Slice 2 — user-extensible `CancelSafe` opt-in
//!
//! [`collect_cancel_safe_opt_ins`] walks `program.items` once per check
//! call, collecting target type names from every `impl CancelSafe for T`
//! into a `HashSet<String>`. [`is_not_cancel_safe`] consults the set
//! before the closed-enumeration check — opted-in types fall through to
//! "cancel-safe" regardless of their `shared`-ness. v1 keeps the
//! `marker trait CancelSafe;` declaration user-owned (no implicit
//! stdlib seeding); user programs (and tests) declare the marker
//! alongside their opt-in impl. Stdlib seeding lands when stdlib
//! infrastructure does.
//!
//! ## Slice 3a — flow-sensitive detection (File seeding)
//!
//! [`collect_cancel_unsafe_annotations`] walks every impl method for
//! `#[cancel_unsafe_until(method = "<clear>")]` attributes, building a
//! `type_name → (soiling_method → clearing_method)` table.
//! [`check_state_flow_for_program`] then walks every network-boundary
//! function body and threads a per-binding state map through the walk:
//! a soiling `MethodCall` (e.g. `f.write(buf)` on a `File` binding)
//! flips the binding's state to `Soiled`, the matching clearing call
//! (`f.flush()`) flips it back to `Clean`, and any yield point reached
//! while a binding is `Soiled` emits a `RaiiAcrossYieldError` with the
//! [`StateViolation`] payload describing which method soiled and what
//! clear method to call before the yield.
//!
//! ## Slice 3 — branch-precise flow merging
//!
//! The flow is **branch-precise**: each `if` / `if let` arm and each
//! `match` arm is walked from a clone of the pre-branch state, and the
//! post-branch state is the may-soiled union across arms (a binding is
//! Soiled-after iff it is Soiled on *some* arm — see [`merge_states`]).
//! This is sound where the prior linear single-state walk was not:
//! `if c { f.write(); } else { f.flush(); } yield` is now correctly
//! **rejected** (the `c == true` path holds a soiled `File` across the
//! yield), and mutually-exclusive shapes like `if c { f.write(); } else {
//! yield; }` are correctly **accepted** (the soil and the yield are on
//! disjoint paths). Loops run a bounded 2-pass fixpoint
//! ([`walk_loop_body`]) so a soil carried across iterations is observed at
//! a body-top yield. Remaining imprecision biases toward false positives
//! (the sound direction): a per-iteration loop condition is walked once,
//! and a `break`-mid-body exit state is approximated.
//!
//! ## What this module does NOT do (yet)
//!
//! - Flow-sensitive detection beyond File — `BufReader[R]` while buffer
//!   non-empty, database transaction handles pre-commit. The
//!   infrastructure shipped here is type-agnostic (any
//!   `#[cancel_unsafe_until(method = ...)]` annotated method
//!   participates), but the stdlib types themselves don't exist yet.
//!   Tracker: phase 6 line 155 slice 3b (BufReader sub-slice).
//! - Guard-soil fall-through — a soiling side effect inside a `match`
//!   arm guard that then *fails* its match (falling through to a later
//!   arm) is not threaded into the later arm's entry state. Exotic; left
//!   to a future slice.
//! - Soiling via methods on object subexpressions — only
//!   `Identifier(name).M(...)` and `self.M(...)` are tracked; calls
//!   through field access (`record.handle.write(...)`), index
//!   (`files[i].write(...)`), or other complex receiver shapes fall
//!   through unmodified.
//! - Soiling propagation across function calls — a helper function
//!   that takes a `File` by `mut ref self` and writes to it is not
//!   re-walked from the caller's flow state. Effect-typed propagation
//!   ("any fn whose receiver is `cancel_unsafe`-annotated returns
//!   with the soiling-state set") is a separate slice.

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
    /// the suspension boundary the binding cannot live across. The
    /// primary anchor for the diagnostic.
    pub yield_span: Span,
    /// Span of the binding's introducing pattern (parameter, `let`,
    /// match-arm). Threaded through `StateStructField.binding_span`
    /// from `StateStructLayoutWalker`; emitted as a secondary
    /// "binding declared here" highlight by the cli.rs diagnostic
    /// formatter (plain / JSON / JSONL). `None` when the binding has
    /// no source-level pattern — at v1 this is only `self` (whose
    /// `ScopeEntry.span_key` is `None`); future synthetic bindings
    /// follow the same convention.
    pub binding_span: Option<Span>,
    /// `Some` for slice-3 flow-sensitive violations — the binding's
    /// surface type itself is cancel-safe (so slices 1/2/4 don't fire),
    /// but a [`#[cancel_unsafe_until]`](collect_cancel_unsafe_annotations)
    /// method was called on it without the matching clearing method
    /// before the yield. `None` for slice-1 / slice-2 / slice-4
    /// rejections, where the surface type itself is unconditionally
    /// non-cancel-safe.
    pub state_violation: Option<StateViolation>,
}

/// Companion payload for slice-3 flow-sensitive [`RaiiAcrossYieldError`]s.
/// Names the soiling method that put the binding into the cancel-unsafe
/// state and the clearing method the user must call before yielding to
/// restore cancel-safety.
#[derive(Debug, Clone)]
pub struct StateViolation {
    /// Surface name of the method whose call soiled the binding —
    /// the method name (right of the dot), not the full `Type.method`
    /// key. E.g. `"write"` for `f.write(buf)`.
    pub soiling_method: String,
    /// Span of the soiling `MethodCall` expression — the call site
    /// the user can act on. Emitted as a "soiled by call here"
    /// secondary highlight.
    pub soil_span: Span,
    /// Method name (right of the dot) that, when called on the same
    /// binding, restores it to a cancel-safe state. E.g. `"flush"`.
    /// Threaded into the `help:` text so the user sees the literal
    /// method to call.
    pub clear_method_name: String,
}

impl RaiiAcrossYieldError {
    /// Human-readable diagnostic body. The code prefix
    /// `error[E_RAII_ACROSS_YIELD]` is added by the diagnostic formatter
    /// in `src/cli.rs` (mirrors the other phase error types — they each
    /// expose the body, the formatter prepends the namespaced code).
    ///
    /// For slice-3 flow-sensitive violations ([`state_violation`]
    /// is `Some`), the body names the soiling method and the missing
    /// clearing call rather than the surface-type identity.
    ///
    /// [`state_violation`]: RaiiAcrossYieldError::state_violation
    pub fn message(&self) -> String {
        if let Some(ref sv) = self.state_violation {
            format!(
                "holding `{}` (type `{}`) with pending `{}` across a suspension point in `{}` — \
                 call `{}.{}` before yielding to restore cancel-safety",
                self.binding_name,
                self.type_name,
                sv.soiling_method,
                self.fn_key,
                self.binding_name,
                sv.clear_method_name,
            )
        } else {
            format!(
                "holding `{}` (type `{}`) across a suspension point in `{}` is not cancel-safe",
                self.binding_name, self.type_name, self.fn_key,
            )
        }
    }

    /// Trailing diagnostic note explaining the cancel-leak hazard.
    /// Matches the wording from design.md § RAII Across Yield Points.
    pub fn note(&self) -> &'static str {
        "dropping `{}` while the task is parked at a yield point would run its destructor under \
         cancellation, leaving owned resources in an undefined intermediate state"
    }

    /// Fix-it hint pointing at the remediation paths from the design
    /// spec. The shape varies by NOT-CancelSafe class:
    ///
    /// - **Shared structs / enums:** "release X before the yield, or
    ///   `impl CancelSafe for T` once the type's `Drop` impl is
    ///   audited" — the slice-2 user-extensible opt-in surface.
    /// - **Raw pointers (`*const T` / `*mut T`):** "release X before
    ///   the yield, or convert the pointer to a safe handle". The
    ///   `impl CancelSafe` suggestion is omitted — raw pointers have
    ///   no `Drop` to audit, and slice 2's opt-in walker only matches
    ///   single-segment `TypeKind::Path` targets (so
    ///   `impl CancelSafe for *const T` wouldn't apply anyway).
    pub fn help(&self) -> String {
        if let Some(ref sv) = self.state_violation {
            return format!(
                "call `{}.{}()` before the suspension point to clear the pending `{}` state, \
                 or release `{}` entirely before yielding",
                self.binding_name, sv.clear_method_name, sv.soiling_method, self.binding_name,
            );
        }
        if is_raw_pointer_surface_name(&self.type_name) {
            format!(
                "release `{}` before the yield, or convert the pointer to a safe handle \
                 (a `ref`/`mut ref` borrow, a `Box[T]`, or a Kāra-side wrapper) before yielding — \
                 raw pointers have no `Drop` so they cannot opt into `CancelSafe`",
                self.binding_name,
            )
        } else {
            format!(
                "release `{}` before the yield, or `impl CancelSafe for {}` once the type's `Drop` \
                 impl is audited to be safe under cancellation",
                self.binding_name, self.type_name,
            )
        }
    }
}

/// True if `type_name` is a raw-pointer surface name as recorded by
/// `bind_pattern_types` / `check_pattern_against` (slice 4). The
/// recorder writes `type_display(Type::Pointer)` which yields
/// `*mut T` or `*const T` (note the trailing space after `mut` /
/// `const` before the pointee). Nominal type names cannot otherwise
/// begin with `*` per the lexer, so the prefix check is unambiguous.
fn is_raw_pointer_surface_name(type_name: &str) -> bool {
    type_name.starts_with("*const ") || type_name.starts_with("*mut ")
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
    let cancel_safe_opt_ins = collect_cancel_safe_opt_ins(program);
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
            if is_not_cancel_safe(type_name, types, &cancel_safe_opt_ins) {
                errors.push(RaiiAcrossYieldError {
                    fn_key: fn_key.clone(),
                    binding_name: field.name.clone(),
                    type_name: type_name.clone(),
                    yield_span: first_yp.span.clone(),
                    binding_span: field.binding_span.clone(),
                    state_violation: None,
                });
            }
        }
    }
    let annotations = collect_cancel_unsafe_annotations(program);
    if !annotations.is_empty() {
        errors.extend(check_state_flow_for_program(program, types, &annotations));
    }
    errors
}

/// Walk `program.items` and return the set of single-segment target
/// type names that have an `impl CancelSafe for T` block in this
/// program. Slice 2: the user-extensible opt-in surface for the v1
/// NOT-CancelSafe closed enumeration in [`is_not_cancel_safe`].
///
/// v1 contract:
/// - Trait match is by `path.segments.last() == "CancelSafe"` —
///   single-segment match, consistent with how the typechecker
///   matches trait names elsewhere.
/// - Target match is restricted to `TypeKind::Path` with exactly one
///   segment (e.g. `impl CancelSafe for Hub`). Generic-bound forms
///   (`impl[T: CancelSafe] CancelSafe for Vec[T]`) and multi-segment
///   target paths are out of scope at v1 — they need bound-resolution
///   wiring that lives elsewhere; this walker silently skips them.
/// - No validation that `CancelSafe` was declared as a `marker trait`
///   in this program — the resolver / typechecker emits the "trait
///   not found" / `E_MARKER_IMPL_HAS_METHOD` diagnostics if the impl
///   is ill-formed; this walker just records the syntactic opt-in.
fn collect_cancel_safe_opt_ins(program: &Program) -> std::collections::HashSet<String> {
    let mut opt_ins = std::collections::HashSet::new();
    for item in &program.items {
        let Item::ImplBlock(imp) = item else { continue };
        let Some(ref trait_path) = imp.trait_name else {
            continue;
        };
        if trait_path.segments.last().map(String::as_str) != Some("CancelSafe") {
            continue;
        }
        let TypeKind::Path(ref target_path) = imp.target_type.kind else {
            continue;
        };
        if target_path.segments.len() != 1 {
            continue;
        }
        opt_ins.insert(target_path.segments[0].clone());
    }
    opt_ins
}

/// Surface-name predicate that drives the diagnostic. Returns `true`
/// when `type_name` is in the v1 NOT-cancel-safe set:
///
/// - **Shared structs / enums** (slice 1 — Rc-rooted reachability)
///   with no `impl CancelSafe for T` opt-in (slice 2's user-extensible
///   override surface).
/// - **Raw pointers** `*const T` / `*mut T` (slice 4 — no `Drop`
///   hook, no way to release the pointee under cancellation). The
///   slice-2 opt-in does NOT apply: its walker matches single-segment
///   `TypeKind::Path` targets only, so `impl CancelSafe for *const T`
///   wouldn't parse into a recognised opt-in even if a user wrote
///   one — raw-pointer rejection is unconditional.
///
/// All other surface types fall through to "cancel-safe by default"
/// at v1; once stdlib + the marker-trait stdlib seeding lands, the
/// default flips to "opt-in cancel-safe" so the v1 stdlib set lives
/// in code rather than as negative space.
fn is_not_cancel_safe(
    type_name: &str,
    types: &TypeCheckResult,
    cancel_safe_opt_ins: &std::collections::HashSet<String>,
) -> bool {
    if is_raw_pointer_surface_name(type_name) {
        return true;
    }
    if cancel_safe_opt_ins.contains(type_name) {
        return false;
    }
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

// ── Slice 3 — flow-sensitive `#[cancel_unsafe_until]` detection ──────

/// Map of `type_name → (soiling_method → clearing_method)` collected
/// from every `impl <Type> { #[cancel_unsafe_until(method = "<clear>")]
/// fn <soil>(...) ... }` annotation in the program. The inner map's
/// keys are the soiling method names (left of "→ Soiled") and the
/// values are the clearing method names (the call that flips
/// "→ Clean"). v1 stdlib seeding: `{"File": {"write": "flush"}}`.
///
/// Outer map is keyed by the impl block's single-segment target type
/// name — matches the slice-2 opt-in walker's scope. Multi-segment
/// target paths and generic-impl forms are out of scope at v1; the
/// walker silently skips them.
type CancelUnsafeAnnotations =
    std::collections::HashMap<String, std::collections::HashMap<String, String>>;

/// Walk `program.items` for `#[cancel_unsafe_until(method = "<name>")]`
/// attributes on impl methods. Returns the collected
/// [`CancelUnsafeAnnotations`] table. The attribute must carry exactly
/// one named arg, `method = "<string>"`; malformed shapes (missing
/// arg, non-string value, wrong arg name) are silently ignored — the
/// attribute-validation pass already rejects unknown attribute names,
/// and the bare-name registry accepts `cancel_unsafe_until` so the
/// parse-side is well-formed; we treat slice-3 as best-effort over
/// the well-formed cases and let any future shape-validator
/// (slice-3b?) escalate malformed shapes to errors.
fn collect_cancel_unsafe_annotations(program: &Program) -> CancelUnsafeAnnotations {
    let mut out: CancelUnsafeAnnotations = std::collections::HashMap::new();
    accumulate_annotations_from_items(&program.items, &mut out);
    // Stdlib bake (`runtime/stdlib/*.kara`) declares impls outside of
    // `program.items` — `register_baked_stdlib` registers methods into
    // the typechecker's env directly, and `synthetic_prelude_items`
    // splices StructDefs but not their impl blocks. Walk the baked
    // programs explicitly so v1 stdlib annotations (`File.write`)
    // participate without the caller having to splice impl blocks in.
    for (_, stdlib_program) in crate::prelude::STDLIB_PROGRAMS.iter() {
        accumulate_annotations_from_items(&stdlib_program.items, &mut out);
    }
    out
}

fn accumulate_annotations_from_items(items: &[Item], out: &mut CancelUnsafeAnnotations) {
    for item in items {
        let Item::ImplBlock(imp) = item else { continue };
        let TypeKind::Path(ref target_path) = imp.target_type.kind else {
            continue;
        };
        if target_path.segments.len() != 1 {
            continue;
        }
        let type_name = &target_path.segments[0];
        for impl_item in &imp.items {
            let ImplItem::Method(m) = impl_item else {
                continue;
            };
            for attr in &m.attributes {
                if !attr.is_bare("cancel_unsafe_until") {
                    continue;
                }
                let mut clear: Option<String> = None;
                for arg in &attr.args {
                    if arg.name.as_deref() != Some("method") {
                        continue;
                    }
                    let Some(ref v) = arg.value else { continue };
                    if let ExprKind::StringLit(s) = &v.kind {
                        clear = Some(s.clone());
                        break;
                    }
                }
                if let Some(c) = clear {
                    out.entry(type_name.clone())
                        .or_default()
                        .insert(m.name.clone(), c);
                }
            }
        }
    }
}

/// Per-binding tracked state during the flow walk. `Clean` is the
/// default for every binding the walker pushes onto its scope; once
/// a `#[cancel_unsafe_until]`-annotated method is called on the
/// binding, state flips to `Soiled` with the soiling-call's span
/// and the clearing method name pinned. A subsequent call to the
/// clearing method on the same binding flips back to `Clean`.
#[derive(Debug, Clone)]
enum BindingState {
    Clean,
    Soiled {
        soiling_method: String,
        soil_span: Span,
        clear_method_name: String,
    },
}

/// Walker state for one function body's flow-sensitive cancel-unsafe
/// state tracking. Mirrors `cli::YieldPointWalker`'s scope-discipline
/// (push on binding introduction, truncate on block exit) and enriches
/// each scope slot with the binding's surface type name — used to
/// resolve which `CancelUnsafeAnnotations` table to consult at each
/// `MethodCall`.
///
/// State map is keyed by binding name and threaded linearly through
/// the walk. v1 does no branch merging: a soil seen anywhere in
/// source-traversal order before a yield triggers the error,
/// regardless of whether the soil + yield were in mutually-exclusive
/// branches. See module doc comment for the v1 fidelity statement.
struct StateFlowWalker<'a> {
    annotations: &'a CancelUnsafeAnnotations,
    method_callee_types: &'a std::collections::HashMap<crate::resolver::SpanKey, String>,
    pattern_binding_types: &'a std::collections::HashMap<crate::resolver::SpanKey, String>,
    /// In-scope bindings with their resolved surface type name (looked
    /// up at push time from `pattern_binding_types`, or threaded from
    /// the impl's target type for `self`). `None` for bindings whose
    /// type the typechecker didn't record (primitives, untyped patterns)
    /// — those can never participate in cancel-unsafe annotations so
    /// the walker skips them at `MethodCall` resolution.
    scope: Vec<(String, Option<String>, Option<Span>)>,
    /// Per-binding state map. Bindings missing from the map are
    /// implicitly `Clean` — only Soiled bindings carry an entry.
    /// Keyed by the source-level binding name (same shape as
    /// `state_struct_layouts` fields and yield-points captured-locals).
    state: std::collections::HashMap<String, BindingState>,
    /// Function key (free fn name or `Type.method`) currently being
    /// walked — threaded into emitted errors' `fn_key`.
    fn_key: String,
    /// Set of network-yield callee keys: when a `Call` or `MethodCall`
    /// matches one of these, the current Soiled bindings get errors.
    network_yield: &'a std::collections::HashMap<String, bool>,
    /// Per-(binding_name, fn_key) dedup — only one error per binding
    /// per fn even if the binding is held Soiled across multiple
    /// yield points. Matches the slice-1 walk's per-binding-once
    /// emission contract.
    emitted: std::collections::HashSet<String>,
    /// Output bucket; collected errors flushed back to the caller.
    errors: Vec<RaiiAcrossYieldError>,
}

/// Run the slice-3 flow-sensitive walker over every network-boundary
/// function in `program` (every fn with at least one entry in
/// `state_struct_layouts`). Returns a flat list of
/// `RaiiAcrossYieldError`s with `state_violation: Some(_)`.
///
/// `annotations` is the program's collected `#[cancel_unsafe_until]`
/// table from [`collect_cancel_unsafe_annotations`]. The caller
/// (`check_raii_across_yield`) shortcircuits this whole pass when
/// `annotations.is_empty()` — no soiling rules means no possible
/// state violations.
fn check_state_flow_for_program(
    program: &Program,
    types: &TypeCheckResult,
    annotations: &CancelUnsafeAnnotations,
) -> Vec<RaiiAcrossYieldError> {
    let mut out = Vec::new();
    for item in &program.items {
        match item {
            Item::Function(func) => {
                let key = func.name.clone();
                if !program.state_struct_layouts.contains_key(&key) {
                    continue;
                }
                check_state_flow_for_fn(program, types, annotations, &key, func, None, &mut out);
            }
            Item::ImplBlock(imp) => {
                let target = match &imp.target_type.kind {
                    TypeKind::Path(p) => match p.segments.last() {
                        Some(s) => s.clone(),
                        None => continue,
                    },
                    _ => continue,
                };
                for impl_item in &imp.items {
                    let ImplItem::Method(m) = impl_item else {
                        continue;
                    };
                    let key = format!("{}.{}", target, m.name);
                    if !program.state_struct_layouts.contains_key(&key) {
                        continue;
                    }
                    check_state_flow_for_fn(
                        program,
                        types,
                        annotations,
                        &key,
                        m,
                        Some(&target),
                        &mut out,
                    );
                }
            }
            _ => {}
        }
    }
    out
}

fn check_state_flow_for_fn(
    program: &Program,
    types: &TypeCheckResult,
    annotations: &CancelUnsafeAnnotations,
    fn_key: &str,
    func: &Function,
    impl_target_type: Option<&str>,
    out: &mut Vec<RaiiAcrossYieldError>,
) {
    let mut walker = StateFlowWalker {
        annotations,
        method_callee_types: &types.method_callee_types,
        pattern_binding_types: &types.pattern_binding_types,
        scope: Vec::new(),
        state: std::collections::HashMap::new(),
        fn_key: fn_key.to_string(),
        network_yield: &program.callee_network_yield_effect,
        emitted: std::collections::HashSet::new(),
        errors: Vec::new(),
    };
    if func.self_param.is_some() {
        walker.scope.push((
            "self".to_string(),
            impl_target_type.map(|s| s.to_string()),
            None,
        ));
    }
    for p in &func.params {
        for (name, span) in p.pattern.binding_name_spans() {
            let span_key = crate::resolver::SpanKey::from_span(&span);
            let ty = walker.pattern_binding_types.get(&span_key).cloned();
            walker.scope.push((name, ty, Some(span)));
        }
    }
    walker.walk_block(&func.body);
    out.extend(walker.errors);
}

impl StateFlowWalker<'_> {
    /// Look up the surface type recorded for the given binding name.
    /// Returns `None` for synthetic / untyped bindings — those can't
    /// participate in cancel-unsafe state tracking.
    fn type_of(&self, binding_name: &str) -> Option<&str> {
        for (name, ty, _) in self.scope.iter().rev() {
            if name == binding_name {
                return ty.as_deref();
            }
        }
        None
    }

    fn binding_span_of(&self, binding_name: &str) -> Option<Span> {
        for (name, _, span) in self.scope.iter().rev() {
            if name == binding_name {
                return span.clone();
            }
        }
        None
    }

    fn emit_for_soiled(&mut self, binding_name: &str, yield_span: &Span) {
        let Some(state) = self.state.get(binding_name) else {
            return;
        };
        let BindingState::Soiled {
            soiling_method,
            soil_span,
            clear_method_name,
        } = state.clone()
        else {
            return;
        };
        if !self.emitted.insert(binding_name.to_string()) {
            return;
        }
        let type_name = self.type_of(binding_name).unwrap_or("?").to_string();
        let binding_span = self.binding_span_of(binding_name);
        self.errors.push(RaiiAcrossYieldError {
            fn_key: self.fn_key.clone(),
            binding_name: binding_name.to_string(),
            type_name,
            yield_span: yield_span.clone(),
            binding_span,
            state_violation: Some(StateViolation {
                soiling_method,
                soil_span,
                clear_method_name,
            }),
        });
    }

    fn record_yield(&mut self, yield_span: &Span) {
        let soiled: Vec<String> = self
            .state
            .iter()
            .filter_map(|(name, s)| match s {
                BindingState::Soiled { .. } => Some(name.clone()),
                BindingState::Clean => None,
            })
            .collect();
        for name in soiled {
            self.emit_for_soiled(&name, yield_span);
        }
    }

    /// May-soiled union of two post-branch state maps: a binding is Soiled
    /// in the result iff it is Soiled in EITHER input. When both arms soil
    /// the same binding, `a`'s soil metadata (method / span / clear name)
    /// wins — `emit_for_soiled` dedups per (binding, fn), so the choice is
    /// immaterial to the diagnostic. Absent ≡ Clean, so a binding Soiled in
    /// one arm and untouched in the other ends up Soiled (the soiling path
    /// survives the merge). This is what makes branch handling sound: the
    /// prior linear walk threaded one mutable map through both arms, so a
    /// clear in one arm masked a soil in the sibling (false negative) and a
    /// soil leaked into the sibling's yield checks (false positive).
    fn merge_states(
        a: std::collections::HashMap<String, BindingState>,
        b: std::collections::HashMap<String, BindingState>,
    ) -> std::collections::HashMap<String, BindingState> {
        let mut out = a;
        for (name, sb) in b {
            if matches!(out.get(&name), Some(BindingState::Soiled { .. })) {
                continue; // `a` already soils it — keep a's metadata
            }
            if matches!(sb, BindingState::Soiled { .. }) {
                out.insert(name, sb);
            }
        }
        out
    }

    fn walk_body_opt_pattern(&mut self, pattern: Option<&Pattern>, body: &Block) {
        match pattern {
            Some(p) => self.walk_block_with_pattern(p, body),
            None => self.walk_block(body),
        }
    }

    /// Walk a loop body with a bounded 2-pass fixpoint so a soil carried
    /// across iterations is observed at body-top yields and after the loop.
    /// Pass 1 from the entry state is the first-iteration view and discovers
    /// body-end soils; fold those into the loop-head state (`head = entry ⊔
    /// pass1`); pass 2 re-walks from `head` so a yield near the body top now
    /// sees a soil left by a prior iteration's tail. The state's per-binding
    /// lattice has height 1 (Clean → Soiled) and soils are syntactic (a
    /// soiling call soils regardless of entry), so two passes reach the
    /// fixpoint. The post-loop state merges `head` (the loop may run zero
    /// times / exit at the top) with pass 2's body result. Per-binding error
    /// dedup makes the repeated walk emit at most one diagnostic per binding.
    ///
    /// `break`-mid-body exit state and per-iteration condition re-evaluation
    /// are approximated (the condition is walked once by the caller); both
    /// are exotic for cancel-unsafe state and left to a future slice.
    fn walk_loop_body(&mut self, pattern: Option<&Pattern>, body: &Block) {
        let entry = self.state.clone();
        self.walk_body_opt_pattern(pattern, body);
        let pass1 = std::mem::take(&mut self.state);
        let head = Self::merge_states(entry, pass1);
        self.state = head.clone();
        self.walk_body_opt_pattern(pattern, body);
        let pass2 = std::mem::take(&mut self.state);
        self.state = Self::merge_states(head, pass2);
    }

    /// Source-level binding name targeted by `expr`, if `expr` is a
    /// shape the slice-3 walker tracks. Returns `Some("self")` for
    /// `SelfValue` receivers, `Some(name)` for identifier receivers,
    /// and `None` for everything else (field access, index, complex
    /// subexpressions — out of scope at v1).
    fn receiver_binding(expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Identifier(n) => Some(n.clone()),
            ExprKind::SelfValue => Some("self".to_string()),
            _ => None,
        }
    }

    fn apply_method_call(&mut self, method_name: &str, object: &Expr, call_span: &Span) {
        let Some(binding_name) = Self::receiver_binding(object) else {
            return;
        };
        let Some(type_name) = self.type_of(&binding_name).map(str::to_string) else {
            return;
        };
        let Some(methods) = self.annotations.get(&type_name) else {
            return;
        };
        if let Some(clear_name) = methods.get(method_name) {
            self.state.insert(
                binding_name,
                BindingState::Soiled {
                    soiling_method: method_name.to_string(),
                    soil_span: call_span.clone(),
                    clear_method_name: clear_name.clone(),
                },
            );
            return;
        }
        // Clearing: if the current state names this method as the clear method, flip to Clean.
        let should_clear = matches!(
            self.state.get(&binding_name),
            Some(BindingState::Soiled { clear_method_name, .. }) if clear_method_name == method_name,
        );
        if should_clear {
            self.state.insert(binding_name, BindingState::Clean);
        }
    }

    fn callee_key(&self, callee: &Expr, expr_span: &Span) -> Option<String> {
        match &callee.kind {
            ExprKind::Identifier(name) => Some(name.clone()),
            ExprKind::Path { segments, .. } => Some(segments.join(".")),
            ExprKind::FieldAccess { .. } | ExprKind::MethodCall { .. } => None,
            _ => self
                .method_callee_types
                .get(&crate::resolver::SpanKey::from_span(expr_span))
                .cloned(),
        }
    }

    fn walk_block(&mut self, block: &Block) {
        let scope_mark = self.scope.len();
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    fn walk_block_with_pattern(&mut self, pat: &Pattern, block: &Block) {
        let scope_mark = self.scope.len();
        for (name, span) in pat.binding_name_spans() {
            let span_key = crate::resolver::SpanKey::from_span(&span);
            let ty = self.pattern_binding_types.get(&span_key).cloned();
            self.scope.push((name, ty, Some(span)));
        }
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    fn walk_expr_with_pattern(&mut self, pat: &Pattern, expr: &Expr) {
        let scope_mark = self.scope.len();
        for (name, span) in pat.binding_name_spans() {
            let span_key = crate::resolver::SpanKey::from_span(&span);
            let ty = self.pattern_binding_types.get(&span_key).cloned();
            self.scope.push((name, ty, Some(span)));
        }
        self.walk_expr(expr);
        self.scope.truncate(scope_mark);
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let { value, pattern, .. } => {
                self.walk_expr(value);
                for (name, span) in pattern.binding_name_spans() {
                    let span_key = crate::resolver::SpanKey::from_span(&span);
                    let ty = self.pattern_binding_types.get(&span_key).cloned();
                    self.scope.push((name, ty, Some(span)));
                }
            }
            StmtKind::LetUninit {
                name, name_span, ..
            } => {
                self.scope
                    .push((name.clone(), None, Some(name_span.clone())));
            }
            StmtKind::LetElse {
                value,
                pattern,
                else_block,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block(else_block);
                for (name, span) in pattern.binding_name_spans() {
                    let span_key = crate::resolver::SpanKey::from_span(&span);
                    let ty = self.pattern_binding_types.get(&span_key).cloned();
                    self.scope.push((name, ty, Some(span)));
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Expr(expr) => self.walk_expr(expr),
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
                if let Some(key) = self.callee_key(callee, &expr.span) {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        self.record_yield(&expr.span);
                    }
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.walk_expr(object);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
                // Resolve callee key first — if this MethodCall is itself
                // a yield point, the soiled-state snapshot must be taken
                // BEFORE the soil/clear flip below (otherwise a method
                // call that is both a yield AND soiling would miss its
                // own pre-call snapshot — not a current shape but
                // defensible against future stdlib annotations).
                if let Some(key) = self
                    .method_callee_types
                    .get(&crate::resolver::SpanKey::from_span(&expr.span))
                    .cloned()
                {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        self.record_yield(&expr.span);
                    }
                }
                self.apply_method_call(method, object, &expr.span);
            }
            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Question(inner) => self.walk_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object);
                if let Some(arglist) = args {
                    for arg in arglist {
                        self.walk_expr(&arg.value);
                    }
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object)
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                // The condition runs unconditionally before either arm.
                self.walk_expr(condition);
                // Walk each arm from a clone of the pre-branch state, then
                // merge by union: a binding is Soiled-after-if if it is
                // Soiled on EITHER arm (slice 3 branch-precise flow). The
                // `else`-less form's implicit empty path is the unchanged
                // entry, so a then-only soil still survives the merge.
                let entry = self.state.clone();
                self.walk_block(then_block);
                let then_state = std::mem::replace(&mut self.state, entry);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
                let else_state = std::mem::take(&mut self.state);
                self.state = Self::merge_states(then_state, else_state);
            }
            ExprKind::IfLet {
                value,
                pattern,
                then_block,
                else_branch,
            } => {
                self.walk_expr(value);
                let entry = self.state.clone();
                self.walk_block_with_pattern(pattern, then_block);
                let then_state = std::mem::replace(&mut self.state, entry);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
                let else_state = std::mem::take(&mut self.state);
                self.state = Self::merge_states(then_state, else_state);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                // Each arm runs from the same pre-match state; the post-match
                // state is the union across all arms (a match is exhaustive,
                // so the arms are the only paths out). A guard's soiling side
                // effects on a *failed* arm that falls through to a later arm
                // are not threaded forward — an exotic shape left to a future
                // slice (see module doc).
                let entry = self.state.clone();
                let mut merged: Option<std::collections::HashMap<String, BindingState>> = None;
                for arm in arms {
                    self.state = entry.clone();
                    if let Some(ref g) = arm.guard {
                        let scope_mark = self.scope.len();
                        for (name, span) in arm.pattern.binding_name_spans() {
                            let span_key = crate::resolver::SpanKey::from_span(&span);
                            let ty = self.pattern_binding_types.get(&span_key).cloned();
                            self.scope.push((name, ty, Some(span)));
                        }
                        self.walk_expr(g);
                        self.scope.truncate(scope_mark);
                    }
                    self.walk_expr_with_pattern(&arm.pattern, &arm.body);
                    let arm_state = std::mem::take(&mut self.state);
                    merged = Some(match merged {
                        None => arm_state,
                        Some(m) => Self::merge_states(m, arm_state),
                    });
                }
                self.state = merged.unwrap_or(entry);
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition);
                self.walk_loop_body(None, body);
            }
            ExprKind::WhileLet {
                value,
                pattern,
                body,
                ..
            } => {
                self.walk_expr(value);
                self.walk_loop_body(Some(pattern), body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.walk_expr(iterable);
                self.walk_loop_body(Some(pattern), body);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                self.walk_loop_body(None, body)
            }
            ExprKind::Closure { .. } => {}
            ExprKind::Return(Some(e)) => self.walk_expr(e),
            ExprKind::Return(None) => {}
            ExprKind::Break { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr(v);
                }
            }
            ExprKind::Continue { .. } => {}
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for e in items {
                    self.walk_expr(e);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.walk_expr(e);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.walk_expr(k);
                    self.walk_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_expr(&f.value);
                }
                if let Some(s) = spread {
                    self.walk_expr(s);
                }
            }
            ExprKind::Pipe { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Cast { expr, .. } => self.walk_expr(expr),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(e) = end {
                    self.walk_expr(e);
                }
            }
            ExprKind::Lock { body, .. } => self.walk_block(body),
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.walk_expr(&b.value);
                }
                self.walk_block(body);
            }
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e) = part {
                        self.walk_expr(e);
                    }
                }
            }
        }
    }
}
