//! Closure compilation: literal capture, env-struct emission, indirect
//! calls, and the free-variable scan helpers.
//!
//! Houses `closure_value_type` (the `{fn_ptr, env_ptr}` fat-pointer
//! struct), `compile_closure` (the synthesized closure-body fn +
//! caller-side env capture), `compile_closure_call` (indirect call
//! through a closure binding), `infer_closure_return_type`, and the
//! `collect_closure_free_vars` / `refs_in_expr` / `refs_in_block`
//! free-variable scan helpers consumed by both closure capture and
//! par-block capture sets.

use crate::ast::*;
use crate::ownership::CapturePath;
use crate::resolver::SpanKey;
use crate::token::Span;
use std::collections::{HashMap, HashSet};

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, FunctionType, StructType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
use inkwell::AddressSpace;

use super::state::VarSlot;

/// Per-root unpack plan for the disjoint-capture slice-4 per-path env
/// layout. Records how a captured root binding is rebuilt inside the
/// synthesized closure body from one or more env-struct slots.
///
/// `whole_root_slot = Some(idx)` means the env's slot at `idx` holds the
/// entire root value (matches today's per-name layout); the body unpack
/// loads the slot and stores into a root-named alloca, and field accesses
/// in the body walk it normally.
///
/// `whole_root_slot = None` means the root was captured *path-precisely*:
/// `sub_slots` lists the env slots that hold leaf values at non-empty
/// projection chains under this root. The body unpack allocates a fresh
/// root-typed alloca (uninit'd in the unread fields — the ownership pass
/// guarantees the body never reads them) and writes each sub-slot leaf
/// into its GEP chain. The body's field accesses then walk the stitched
/// root as if it were a whole-root capture.
struct RootUnpackPlan<'ctx> {
    /// LLVM type of the root in the outer scope (matches `VarSlot.ty`).
    root_ty: BasicTypeEnum<'ctx>,
    /// Source type-name of the root, if `var_type_names` has an entry.
    /// Propagated into the closure body's `var_type_names` so method
    /// dispatch on the captured root resolves through the user impl-block.
    type_name: Option<String>,
    /// `Some(env_slot_idx)` → whole-root capture; `None` → per-path.
    whole_root_slot: Option<usize>,
    /// Per-sub-path entries when `whole_root_slot` is None. Each tuple
    /// is `(env_slot_idx, gep_chain, leaf_ty)` — load env[idx] of type
    /// `leaf_ty`, then GEP into the root alloca via `gep_chain` and store.
    sub_slots: Vec<(usize, Vec<u32>, BasicTypeEnum<'ctx>)>,
}

/// Full per-closure capture layout — slot list (env struct field order)
/// plus the per-root unpack plans. Produced by
/// `Codegen::build_capture_path_layout` when ownership data is available
/// for the closure's `SpanKey` and every captured root resolves cleanly
/// through `struct_field_names` / `struct_field_type_names`. `None` →
/// fall back to the legacy `collect_closure_free_vars` per-name layout.
struct CapturePathLayout<'ctx> {
    /// Env-struct field types in slot order. Empty when no captures.
    slot_tys: Vec<BasicTypeEnum<'ctx>>,
    /// `slot_idx → (root_name, gep_chain)` — drives capture-site loads:
    /// for slot i, load `outer.variables[root]` via the gep chain and
    /// store into env field i. Empty `gep_chain` → store the whole-root
    /// value.
    slot_sources: Vec<(String, Vec<u32>)>,
    /// Per-root unpack plans, in deterministic root-name order. Drives
    /// the closure body's prelude.
    root_plans: Vec<(String, RootUnpackPlan<'ctx>)>,
}

impl<'ctx> super::Codegen<'ctx> {
    // ── Closure compilation ────────────────────────────────────────

    /// The LLVM struct type used to represent a closure fat-pointer: `{ ptr fn_ptr, ptr env_ptr }`.
    pub(super) fn closure_value_type(&self) -> StructType<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        self.context.struct_type(&[ptr.into(), ptr.into()], false)
    }

    // ── Escaping-capturing-closure guard (B-2026-06-22-2, heap-env epic Slice 0) ──

    /// Reject a closure that captures one of this function's locals/params and
    /// then ESCAPES via the function's return value. A closure's captured
    /// environment is a stack alloca in the defining frame (the
    /// heap-closure-environment feature is not yet implemented), so a returned
    /// capturing closure reads freed memory after the frame exits — a silent
    /// wrong-output miscompile (`fn make(k){ |x| x+k }` returns garbage, not
    /// `x+k`). This guard turns that into an honest compile error.
    ///
    /// Covers every return point: the body tail AND every explicit `return e`
    /// (not inside a nested closure), where the returned value is — directly,
    /// through an identifier bound to one, through a block/`if`/`match` tail, or
    /// through an aggregate literal (`return H { f: |x| x+k }`) — a capturing
    /// closure.
    ///
    /// Deliberately one-sided so it never rejects a SOUND program: it fires only
    /// on a *capturing* closure in *return* position; a non-capturing closure
    /// (null env), same-frame use, and pass-down-by-`Fn(..)`-param are all
    /// unaffected. Pure-AST, run once per function before codegen. Covers the
    /// local-then-return form too — a capturing closure stored into a LOCAL
    /// aggregate (`let h = H { f: |x| x+k }; h`) or chained through identifiers
    /// (`let g = |x| x+k; let h = H { f: g }; h`) — via the source-ordered
    /// `capturing_vars` builder below. The FIELD-PROJECTION form is also covered
    /// (`let h = H { f: |x| x+k }; return h.f`): the parallel `capturing_fields`
    /// builder records which fields of a local struct binding hold a capturing
    /// closure, so the `FieldAccess` arm rejects exactly those projections while
    /// leaving a sound `return h.other_field` to compile.
    ///
    /// The STORE-escape forms are covered too — a capturing closure moved into a
    /// place that then escapes: a collection (`v.push(clo)` / `v.insert(.., clo)`
    /// on a stdlib-collection local), an index slot (`v[i] = clo`), or a struct
    /// field (`h.f = clo`). The source-ordered builder marks the rooted local
    /// (and, for a field-store, the field) so the existing return-position check
    /// fires on `return v` / `return h` / `return h.f`. Soundness is preserved:
    /// the container-store marking is gated on a *known* collection (so a
    /// like-named `push` on a user type, which might invoke rather than store, is
    /// never marked), the marking only triggers rejection at a return (a stored
    /// closure whose container is used same-frame and dropped is untouched), and
    /// call-arg passing (`apply(clo, x)`) is never touched (pass-down stays
    /// supported). Residuals deferred to the escape-analysis / heap-env slices:
    /// a store into a collection bound by a non-recognized initializer (e.g.
    /// `let v = make_vec()`), a store nested inside a branch/loop, a deeper
    /// projection (`a.b.f`), and assignment to a global / `mut ref` param that
    /// escapes without a `return`.
    pub(super) fn reject_escaping_capturing_closure(&self, func: &Function) -> Result<(), String> {
        // Names whose capture would dangle on escape: the function's params +
        // its top-level `let` bindings (the scope visible at a return).
        let mut outer: HashSet<String> = func
            .params
            .iter()
            .flat_map(|p| p.pattern.binding_names())
            .collect();
        for stmt in &func.body.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    outer.extend(pattern.binding_names());
                }
                StmtKind::LetUninit { name, .. } => {
                    outer.insert(name.clone());
                }
                _ => {}
            }
        }
        // Top-level locals whose initializer resolves to a capturing closure —
        // directly (`let f = |x| x+k`), through an aggregate literal
        // (`let h = H { f: |x| x+k }`), or through an identifier chain
        // (`let g = |x| x+k; let h = H { f: g }`). Processed in SOURCE ORDER so
        // an identifier on a later RHS resolves against the set built from the
        // earlier `let`s. Reuses `tail_escapes_capturing_closure` as the
        // "does this expr produce a capturing closure" predicate — the shapes
        // that count as an escape in return position are exactly those that make
        // a binding capture one. Mirrors the ownership pass's
        // `collect_closure_let_bindings` (`closure_escape.rs`); that pass fires
        // only on REF captures (a dangling borrow), so the OWN-capture escape it
        // soundly admits — but codegen's stack env cannot yet support — must be
        // caught here.
        let mut capturing_vars: HashSet<String> = HashSet::new();
        // Per-binding field map: a local struct binding name → the set of its
        // field names that hold a capturing closure — populated by a
        // struct-literal initializer (`let h = H { f: |x| x+k }`) OR a later
        // field-store (`h.f = |x| x+k`). Lets a `return h.f` be rejected
        // precisely — only the capturing field projects a dangling stack env;
        // `return h.other_field` stays sound. Built in the SAME source-order
        // pass so a field initialized from an earlier-bound capturing local
        // (`H { f: g }`) resolves.
        let mut capturing_fields: HashMap<String, HashSet<String>> = HashMap::new();
        // Local bindings whose declared / inferred type is a stdlib collection
        // (Vec / Map / Set / VecDeque / …). ONLY these receive the container-
        // store marking below: `v.push(clo)` / `v.insert(.., clo)` / `v[i] = clo`
        // move the element INTO the receiver, so it then carries a dangling
        // stack env on escape. Gating on a *known* collection keeps the guard
        // one-sided — a same-named `push` on a USER type (which might invoke
        // rather than store) never marks.
        let mut collection_locals: HashSet<String> = HashSet::new();
        for stmt in &func.body.stmts {
            match &stmt.kind {
                StmtKind::Let {
                    pattern, ty, value, ..
                }
                | StmtKind::LetElse {
                    pattern, ty, value, ..
                } => {
                    // (a) initializer resolves to a capturing closure → the
                    // binding holds one (direct / aggregate literal / id chain).
                    if self.tail_escapes_capturing_closure(
                        value,
                        &outer,
                        &capturing_vars,
                        &capturing_fields,
                    ) {
                        for n in pattern.binding_names() {
                            capturing_vars.insert(n);
                        }
                    }
                    // (b) per-field capture for a direct struct-literal
                    // initializer bound to a single name (`let h = H { f: |x|
                    // x+k, g: 1 }`): mark only the closure-bearing fields. Other
                    // initializer shapes (a block tail, a call returning a
                    // struct, a multi-name pattern) are left untracked — a sound
                    // under-approximation that defers, never falsely rejects.
                    if let ExprKind::StructLiteral { fields, .. } = &value.kind {
                        if let [binding] = pattern.binding_names().as_slice() {
                            for fi in fields {
                                if self.tail_escapes_capturing_closure(
                                    &fi.value,
                                    &outer,
                                    &capturing_vars,
                                    &capturing_fields,
                                ) {
                                    capturing_fields
                                        .entry(binding.clone())
                                        .or_default()
                                        .insert(fi.name.clone());
                                }
                            }
                        }
                    }
                    // (c) remember single-name collection-typed bindings for the
                    // container-store marking in (d) / (e).
                    if Self::let_binds_collection(ty.as_ref(), value) {
                        if let [binding] = pattern.binding_names().as_slice() {
                            collection_locals.insert(binding.clone());
                        }
                    }
                }
                // (d) `v.push(|x| x+k)` / `v.insert(.., clo)` — a capturing
                // closure STORED into a collection local makes that local carry
                // a dangling stack env; mark it so a later `return v` (the
                // Identifier arm) is rejected.
                StmtKind::Expr(e) => {
                    if let ExprKind::MethodCall {
                        object,
                        method,
                        args,
                        ..
                    } = &e.kind
                    {
                        if Self::is_element_storing_method(method) {
                            if let ExprKind::Identifier(recv) = &object.kind {
                                if collection_locals.contains(recv)
                                    && args.iter().any(|a| {
                                        self.tail_escapes_capturing_closure(
                                            &a.value,
                                            &outer,
                                            &capturing_vars,
                                            &capturing_fields,
                                        )
                                    })
                                {
                                    capturing_vars.insert(recv.clone());
                                }
                            }
                        }
                    }
                }
                // (e) `v[i] = clo` (index-store into a collection local) and
                // `h.f = clo` (field-store into a struct local): a capturing
                // closure stored into the place makes the rooted local carry it.
                // Index-store is gated on a known collection (no index-set
                // overload surprises); field-store is unconditional (a struct
                // field-set always stores). Both also record the projected
                // field so `return h.f` / `return v` are caught.
                StmtKind::Assign { target, value }
                    if self.tail_escapes_capturing_closure(
                        value,
                        &outer,
                        &capturing_vars,
                        &capturing_fields,
                    ) =>
                {
                    match &target.kind {
                        ExprKind::Index { object, .. } => {
                            if let ExprKind::Identifier(recv) = &object.kind {
                                if collection_locals.contains(recv) {
                                    capturing_vars.insert(recv.clone());
                                }
                            }
                        }
                        ExprKind::FieldAccess { object, field } => {
                            if let ExprKind::Identifier(base) = &object.kind {
                                capturing_vars.insert(base.clone());
                                capturing_fields
                                    .entry(base.clone())
                                    .or_default()
                                    .insert(field.clone());
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        // Every return point: the body tail + every explicit `return e` not
        // inside a nested closure.
        let mut return_values: Vec<&Expr> = Vec::new();
        if let Some(tail) = func.body.final_expr.as_deref() {
            // Slice 1 (B-2026-06-22-2): a capturing-closure literal that IS the
            // function's direct tail now gets a reference-counted HEAP env and
            // is RETURNABLE — so don't reject it (compile_closure builds it,
            // the caller's binding frees it). Every OTHER escape shape (a
            // closure bound to a local then returned, an aggregate-literal
            // return, an explicit mid-body `return`, …) still needs later
            // slices and stays rejected.
            let supported = matches!(&tail.kind, ExprKind::Closure { params, body, .. }
                if self.closure_literal_captures(params, body, &outer));
            if !supported {
                return_values.push(tail);
            }
        }
        self.collect_outer_return_values(&func.body, &mut return_values);
        if return_values.iter().any(|e| {
            self.tail_escapes_capturing_closure(e, &outer, &capturing_vars, &capturing_fields)
        }) {
            return Err(
                "error[E_ESCAPING_CLOSURE_NOT_YET]: returning a closure that captures a \
                 local variable is not yet supported — the closure's captured environment lives \
                 on the returning function's stack frame, which is freed when it returns (it \
                 would read garbage). Tracked as the heap-closure-environment epic \
                 (B-2026-06-22-2). Workaround: return a non-capturing closure or a named `fn`, or \
                 pass the closure down by a `Fn(..)` parameter instead of returning it."
                    .to_string(),
            );
        }
        Ok(())
    }

    /// `true` when `e` is a call to a free function that RETURNS a heap-env
    /// closure (`make(..)` with `make` ∈ `fns_returning_heap_env`). Such a call
    /// mints a reference-counted heap environment that an owner must free; only
    /// a `let f = <call>` binding is wired to free it (a `FreeClosureEnv`
    /// cleanup), so any other occurrence of the call leaks or escapes the env.
    pub(super) fn is_heap_env_producing_call(&self, e: &Expr) -> bool {
        // A call to a NAMED heap-env fn, OR (currying, B-2026-07-12-12) a call
        // through a local closure-VALUE binding whose value returns a heap-env
        // closure (`make` in `let make = |n| |x| x + n; make(5)`). Both mint an
        // RC heap env the caller binding must free / own — routing the curry
        // call through this predicate reuses the whole free / owner / misuse
        // machinery unchanged.
        self.is_heap_env_producing_call_in(e, &self.fns_returning_heap_env)
            || self.is_heap_env_producing_call_in(e, &self.curry_closure_vars)
    }

    /// As [`Self::is_heap_env_producing_call`] but against an EXPLICIT set —
    /// used inside `compute_fns_returning_heap_env`'s fixpoint, where
    /// `self.fns_returning_heap_env` is not yet populated (the set is being
    /// built up iteration by iteration).
    fn is_heap_env_producing_call_in(&self, e: &Expr, set: &HashSet<String>) -> bool {
        let ExprKind::Call { callee, .. } = &e.kind else {
            return false;
        };
        match &callee.kind {
            ExprKind::Identifier(n) => set.contains(n),
            ExprKind::Path { segments, .. } => segments.len() == 1 && set.contains(&segments[0]),
            _ => false,
        }
    }

    /// If `e` is a struct literal with one or more fields whose value is a
    /// sanctioned heap-env closure STORE, return those field names — the struct
    /// local being bound OWNS each such RC env box (codegen registers an
    /// instance-specific `FreeClosureEnv` on the field). Two store shapes are
    /// collected: a FRESH heap-env-producing call (`H { f: make(..) }`, the field
    /// is the sole owner at refcount 1) and a heap-env BINDING source
    /// (`H { f: f }`, `f` in `binds` — the field co-owns the box with the source
    /// binding via inc-on-store). Empty otherwise.
    fn struct_literal_heap_env_store_fields(
        &self,
        e: &Expr,
        binds: &HashSet<String>,
    ) -> Vec<String> {
        let ExprKind::StructLiteral { fields, .. } = &e.kind else {
            return Vec::new();
        };
        fields
            .iter()
            .filter(|f| {
                self.is_heap_env_producing_call(&f.value)
                    || matches!(&f.value.kind, ExprKind::Identifier(n) if binds.contains(n))
            })
            .map(|f| f.name.clone())
            .collect()
    }

    /// Heap-closure-env epic (B-2026-06-22-2) — the misuse guard that keeps the
    /// heap-env feature SOUND. A heap-env closure binding (`let f = make(..)`)
    /// may now be CALLED in the function that binds it (`f(x)`, possibly many
    /// times), COPIED to another binding (`let g = f`; a copy increments the
    /// shared RC env's refcount and both owners free it via `FreeClosureEnv` at
    /// scope exit — inc-on-copy RC slice), and RETURNED as a bare-identifier tail
    /// or a top-level `return f;` (move-out: codegen neutralizes the source's
    /// `FreeClosureEnv` so the box flows to the caller at the same refcount, and
    /// the function is registered in `fns_returning_heap_env` so the caller's
    /// binding frees it — return-again slice). Every OTHER use would let the env
    /// outlive — or be double-freed by — its owner set: storing it (into a struct
    /// / collection / index / field), passing it as a call argument, capturing it
    /// in a nested closure, or a BRANCH-BURIED return. An UNBOUND `make(..)` (a
    /// non-`let`-RHS occurrence of a heap-env-producing call — `make(..);`,
    /// `make(..)(x)`, `return make(..)`, `[make(..)]`, …) leaks the env. All of
    /// those are not-yet-supported and are rejected here with an honest
    /// `E_ESCAPING_CLOSURE_NOT_YET` rather than miscompiled.
    ///
    /// Inert unless some function returns a heap-env closure. Otherwise: pass 1
    /// collects the top-level heap-env bindings — single-name `let`s whose RHS
    /// is a heap-env-producing call OR a bare copy of an already-collected
    /// binding (a forward scan makes the copy collection transitive, e.g. `let g
    /// = f; let h = g`); pass 2 walks every statement/expression position —
    /// EXCLUDING those sanctioned RHS calls and copies — flagging (a) any
    /// non-sanctioned heap-env-producing call and (b) any bare reference to a
    /// binding that is not the callee of a direct call and is not a sanctioned
    /// copy RHS. The walk ([`expr_has_heap_env_misuse`]) is exhaustive over
    /// `ExprKind` (no silent wildcard) so no escaping occurrence is missed.
    pub(super) fn reject_heap_env_misuse(&mut self, func: &Function) -> Result<(), String> {
        // Inert unless SOME heap-env source is in play — a named fn returning a
        // heap env, or (currying, B-2026-07-12-12) a local closure-value
        // binding whose call returns one. Either populates the misuse walk's
        // `is_heap_env_producing_call` recognition.
        if self.fns_returning_heap_env.is_empty() && self.curry_closure_vars.is_empty() {
            return Ok(());
        }
        // Pass 1 — sanctioned top-level heap-env bindings and aggregate owners
        // (factored into `collect_heap_env_binds_and_owners`, shared with the
        // aggregate-return detection fixpoint). `binds`: heap-env closure bindings
        // (call sources + transitive copies). `owners`: struct locals owning one
        // or more heap-env fields (struct-literal stores OR an aggregate-returning
        // call result).
        let (binds, owners) =
            self.collect_heap_env_binds_and_owners(func, &self.fns_returning_heap_env_aggregate);
        // Stash for the exhaustive walk (read via `&self` from the arms below).
        self.heap_env_aggregate_owners = owners;
        // Tuple / array owners (tuple/array-store + container-escape slices):
        // `let t = (make(..), ..)` / `(f, ..)`, `let a: Array[Fn,N] = [..]`, OR a
        // relay `let r = build(k)` where `build` returns a closure-owning tuple /
        // array (container-escape). Factored into `collect_tuple_array_owners`,
        // shared with the container-return detection fixpoint.
        let (tuple_owners, array_owners) = self.collect_tuple_array_owners(func, &binds);
        self.heap_env_tuple_owners = tuple_owners;
        self.heap_env_array_owners = array_owners;
        // Vec owners (Vec-store + Vec-escape slices): a `Vec[Fn]` local bound
        // `let v: Vec[Fn] = Vec.new()`/`Vec.with_capacity(..)` that receives >=1
        // heap-env push, OR a relay `let r = build(k)` where `build` returns a
        // closure-owning Vec (Vec-escape caller-adopt). Factored into
        // `collect_vec_owners`, shared with the Vec-return detection fixpoint.
        self.heap_env_vec_owners = self.collect_vec_owners(func, &binds);
        // Pass 2 — walk for misuse, skipping the sanctioned `let f = <call>` RHS.
        let mut bad = false;
        for stmt in &func.body.stmts {
            bad |= match &stmt.kind {
                StmtKind::Let { pattern, value, .. } => {
                    // A single-name `let` whose RHS is a heap-env-producing call
                    // OR a bare copy of a binding is sanctioned — its RHS
                    // occurrence is the supported shape, so it is not walked.
                    let names = pattern.binding_names();
                    let single = matches!(names.as_slice(), [_]);
                    let sanctioned = single
                        && (self.is_heap_env_producing_call(value)
                            || matches!(&value.kind,
                                ExprKind::Identifier(n) if binds.contains(n)));
                    let is_owner = single && self.heap_env_aggregate_owners.contains_key(&names[0]);
                    let is_tuple_owner =
                        single && self.heap_env_tuple_owners.contains_key(&names[0]);
                    let is_array_owner =
                        single && self.heap_env_array_owners.contains_key(&names[0]);
                    let is_vec_owner = single && self.heap_env_vec_owners.contains(&names[0]);
                    if sanctioned {
                        false
                    } else if is_vec_owner {
                        // A `Vec[Fn]` owner is bound three ways:
                        //   * construction `let v: Vec[Fn] = Vec.new()` /
                        //     `Vec.with_capacity(n)`: the constructor is innocuous —
                        //     the heap-env stores are separate `v.push(..)` statements,
                        //     sanctioned in the `MethodCall` arm; walk it so a misuse in
                        //     a capacity arg still flags.
                        //   * a Vec-returning CALL relay `let r = build(k)` (Vec-escape
                        //     caller-adopt): walk the call so the by-value arg-pass
                        //     sanction in the `Call` arm applies (same as `is_owner`).
                        //   * owner MOVE `let w = v` (Identifier RHS): the buffer + its
                        //     dynamic env-drop loop transfer to `w` (codegen zeroes
                        //     `v`'s cap, suppressing v's whole cleanup; `w` registers its
                        //     own loop) — a sanctioned move, not walked. The Identifier
                        //     RHS is exactly the move; everything else is construction /
                        //     relay and is walked as before.
                        match &value.kind {
                            ExprKind::Identifier(_) => false,
                            _ => self.expr_has_heap_env_misuse(value, &binds),
                        }
                    } else if is_array_owner {
                        // Array construction `let a: Array[Fn,N] = [<src>, ..]`: each
                        // sanctioned heap-env store element (a FRESH call or a heap-env
                        // BINDING source) is allowed; walk only the OTHER elements.
                        // Mirrors the tuple-owner construction walk, by element index.
                        if let ExprKind::ArrayLiteral(elems) = &value.kind {
                            elems
                                .iter()
                                .filter(|e| {
                                    !self.is_heap_env_producing_call(e)
                                        && !matches!(&e.kind,
                                            ExprKind::Identifier(n) if binds.contains(n))
                                })
                                .any(|e| self.expr_has_heap_env_misuse(e, &binds))
                        } else {
                            false
                        }
                    } else if is_tuple_owner {
                        // Tuple construction `let t = (<src>, ..)`: each sanctioned
                        // heap-env store element (a FRESH call or a heap-env BINDING
                        // source) is allowed; walk only the OTHER elements. Mirrors
                        // the struct-owner construction walk, by element index.
                        if let ExprKind::Tuple(elems) = &value.kind {
                            elems
                                .iter()
                                .filter(|e| {
                                    !self.is_heap_env_producing_call(e)
                                        && !matches!(&e.kind,
                                            ExprKind::Identifier(n) if binds.contains(n))
                                })
                                .any(|e| self.expr_has_heap_env_misuse(e, &binds))
                        } else {
                            false
                        }
                    } else if is_owner {
                        // An aggregate owner is bound two ways:
                        //   * construction `let h = H { f: <src>, .. }`: each
                        //     sanctioned heap-env store field (a FRESH call or a
                        //     heap-env BINDING source) is allowed; walk only the
                        //     OTHER fields (and any spread). The binding-field skip
                        //     mirrors the store-field collection — without it,
                        //     `H { f: f }`'s bare `f` would be (wrongly) flagged.
                        //   * an aggregate-returning CALL `let r = build(k)`
                        //     (aggregate-escape slice): the call result is the
                        //     sanctioned owner source, but the args may still misuse
                        //     a binding/owner, so walk them.
                        match &value.kind {
                            ExprKind::StructLiteral { fields, spread, .. } => {
                                fields
                                    .iter()
                                    .filter(|f| {
                                        !self.is_heap_env_producing_call(&f.value)
                                            && !matches!(&f.value.kind,
                                                ExprKind::Identifier(n) if binds.contains(n))
                                    })
                                    .any(|f| self.expr_has_heap_env_misuse(&f.value, &binds))
                                    || spread
                                        .as_deref()
                                        .is_some_and(|s| self.expr_has_heap_env_misuse(s, &binds))
                            }
                            // The aggregate-returning CALL itself is the sanctioned
                            // owner source (not heap-env-PRODUCING, so it is not a
                            // leak); walk the whole call so the by-value arg-pass
                            // sanction in the `Call` arm applies uniformly — a
                            // builder that BORROWS-only its closure arg accepts a
                            // heap-env binding, one that retains it is still
                            // rejected (the arg then re-flags).
                            ExprKind::Call { .. } => self.expr_has_heap_env_misuse(value, &binds),
                            _ => false,
                        }
                    } else {
                        self.expr_has_heap_env_misuse(value, &binds)
                    }
                }
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    self.expr_has_heap_env_misuse(value, &binds)
                        || self.block_has_heap_env_misuse(else_block, &binds)
                }
                StmtKind::LetUninit { .. } => false,
                StmtKind::Expr(e) => {
                    // A top-level `return <bare binding>;` is the sanctioned
                    // return-of-a-heap-env-binding shape, and `return <bare owner>;`
                    // the sanctioned aggregate-escape shape (move-out codegen
                    // neutralizes the source / the owner's field env slots) — not a
                    // misuse. Any other expr statement is walked as usual.
                    if let ExprKind::Return(Some(inner)) = &e.kind {
                        if matches!(&inner.kind, ExprKind::Identifier(n)
                            if binds.contains(n)
                                || self.heap_env_aggregate_owners.contains_key(n)
                                || self.heap_env_tuple_owners.contains_key(n)
                                || self.heap_env_array_owners.contains_key(n)
                                || self.heap_env_vec_owners.contains(n))
                        {
                            false
                        } else {
                            self.expr_has_heap_env_misuse(e, &binds)
                        }
                    } else {
                        self.expr_has_heap_env_misuse(e, &binds)
                    }
                }
                StmtKind::Assign { target, value } => {
                    // Sanctioned heap-env reassignment (`g = make(j)` / `g = f`
                    // or `r.f = make(j)` / `r.f = g`) is not walked — the bare
                    // `g` / `f` / `r.f` place would otherwise self-flag; codegen
                    // drops the old env + incs the new on a copy, freed once.
                    if self.is_heap_env_reassign(target, value, &binds) {
                        false
                    } else {
                        self.expr_has_heap_env_misuse(target, &binds)
                            || self.expr_has_heap_env_misuse(value, &binds)
                    }
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.expr_has_heap_env_misuse(target, &binds)
                        || self.expr_has_heap_env_misuse(value, &binds)
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.block_has_heap_env_misuse(body, &binds)
                }
                // Desugared away before codegen; under-approximate if it survives.
                StmtKind::MultiAssign { values, .. } => values
                    .iter()
                    .any(|v| self.expr_has_heap_env_misuse(v, &binds)),
            };
            if bad {
                break;
            }
        }
        if !bad {
            if let Some(tail) = &func.body.final_expr {
                // A bare heap-env-binding TAIL is the sanctioned
                // return-of-a-binding shape, and a bare AGGREGATE-OWNER tail the
                // sanctioned aggregate-escape shape (move-out codegen neutralizes
                // the source / the owner's field env slots); anything else in tail
                // position is walked as usual.
                let returnable = matches!(&tail.kind, ExprKind::Identifier(n)
                    if binds.contains(n)
                        || self.heap_env_aggregate_owners.contains_key(n)
                        || self.heap_env_tuple_owners.contains_key(n)
                        || self.heap_env_array_owners.contains_key(n)
                        || self.heap_env_vec_owners.contains(n));
                bad = !returnable && self.expr_has_heap_env_misuse(tail, &binds);
            }
        }
        if bad {
            return Err(
                "error[E_ESCAPING_CLOSURE_NOT_YET]: a returned capturing closure can currently \
                 be CALLED in the function that binds it (`let f = make(..); f(x)`), COPIED to \
                 another binding (`let g = f`), or RETURNED as a bare tail / top-level `return f`. \
                 Storing it (struct / collection / index / field), passing it as a call argument, \
                 capturing it in a nested closure, returning it from inside a branch, or leaving a \
                 `make(..)` result unbound is not yet supported — the reference-counted closure \
                 environment would outlive or be double-freed by its owner set \
                 (heap-closure-environment epic B-2026-06-22-2). Workaround: call, copy, or \
                 directly return the closure where it is bound, or pass it down by a `Fn(..)` \
                 parameter."
                    .to_string(),
            );
        }
        Ok(())
    }

    /// Vec-store slice (B-2026-06-22-2): recursively scan `block` (and nested
    /// loop / branch / block bodies) for `v.push(<heap-env source>)` where `v` is a
    /// `Vec[Fn]` candidate, promoting it to a heap-env Vec OWNER. A heap-env source
    /// is a fresh heap-env-producing call (`make(k)`) or a heap-env closure binding
    /// (`f` in `binds`). Descending into loops/branches is what makes the canonical
    /// `for .. { v.push(make(i)) }` shape usable; a push in an exotic position the
    /// scan misses is SOUND — `v` then isn't an owner and the guard rejects that
    /// push via the generic heap-env-call rule (over-restrict, never miscompile).
    fn collect_heap_env_vec_owners(
        &self,
        block: &Block,
        binds: &HashSet<String>,
        candidates: &HashSet<String>,
        owners: &mut HashSet<String>,
    ) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Expr(e)
                | StmtKind::Let { value: e, .. }
                | StmtKind::LetElse { value: e, .. } => {
                    self.collect_vec_owner_pushes_in_expr(e, binds, candidates, owners)
                }
                StmtKind::Assign { value, .. } | StmtKind::CompoundAssign { value, .. } => {
                    self.collect_vec_owner_pushes_in_expr(value, binds, candidates, owners)
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.collect_heap_env_vec_owners(body, binds, candidates, owners)
                }
                StmtKind::MultiAssign { values, .. } => {
                    for v in values {
                        self.collect_vec_owner_pushes_in_expr(v, binds, candidates, owners);
                    }
                }
                StmtKind::LetUninit { .. } => {}
            }
        }
        if let Some(t) = &block.final_expr {
            self.collect_vec_owner_pushes_in_expr(t, binds, candidates, owners);
        }
    }

    /// Expression companion to [`collect_heap_env_vec_owners`]: flag the push when
    /// `e` IS one, then descend into every block-bearing sub-expression so pushes
    /// nested in loops / branches / blocks are found.
    fn collect_vec_owner_pushes_in_expr(
        &self,
        e: &Expr,
        binds: &HashSet<String>,
        candidates: &HashSet<String>,
        owners: &mut HashSet<String>,
    ) {
        if let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &e.kind
        {
            if matches!(method.as_str(), "push" | "push_back") && args.len() == 1 {
                if let ExprKind::Identifier(v) = &object.kind {
                    if candidates.contains(v) {
                        let a = &args[0].value;
                        let heap_env = self.is_heap_env_producing_call(a)
                            || matches!(&a.kind, ExprKind::Identifier(n) if binds.contains(n));
                        if heap_env {
                            owners.insert(v.clone());
                        }
                    }
                }
            }
        }
        match &e.kind {
            ExprKind::For { body, .. }
            | ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. }
            | ExprKind::Lock { body, .. }
            | ExprKind::Providers { body, .. } => {
                self.collect_heap_env_vec_owners(body, binds, candidates, owners)
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Par(b) => self.collect_heap_env_vec_owners(b, binds, candidates, owners),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            }
            | ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                self.collect_heap_env_vec_owners(then_block, binds, candidates, owners);
                if let Some(eb) = else_branch {
                    self.collect_vec_owner_pushes_in_expr(eb, binds, candidates, owners);
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.collect_vec_owner_pushes_in_expr(&arm.body, binds, candidates, owners);
                }
            }
            _ => {}
        }
    }

    /// Reassignment slice (B-2026-06-22-2). A sanctioned heap-env closure
    /// reassignment is `<place> = make(j)` (fresh env, a MOVE) or
    /// `<place> = f` (binding source, the SHARED env, a COPY), where `<place>`
    /// is one of: a heap-env closure BINDING `g` (`g = ..`); a closure FIELD of
    /// a heap-env struct owner (`r.f = ..` — `r` in `heap_env_aggregate_owners`,
    /// `f` one of its closure fields); or an ELEMENT of a `Vec[Fn]` owner
    /// (`v[i] = ..` — `v` in `heap_env_vec_owners`).
    /// Codegen drops the place's CURRENT env, stores the new fat pointer, and
    /// incs the new env on a binding copy (the source `f` stays a live
    /// co-owner), so each env is freed EXACTLY once. Position-agnostic: works at
    /// the top level of the function body and nested in a branch / loop (the
    /// drop-old fires per execution), since the codegen Assign hooks key only off
    /// the target being a heap-env binding / owner field / Vec element.
    /// `CompoundAssign` (`g += ..`) is never a closure reassignment and is not
    /// sanctioned here. Any other target / value shape returns false (walked /
    /// rejected as before).
    fn is_heap_env_reassign(&self, target: &Expr, value: &Expr, binds: &HashSet<String>) -> bool {
        // The RHS must be a sanctioned reassignment SOURCE: a fresh heap-env
        // call (`make(j)`) or a heap-env closure binding (`f`, a copy).
        let value_ok = self.is_heap_env_producing_call(value)
            || matches!(&value.kind, ExprKind::Identifier(f) if binds.contains(f));
        if !value_ok {
            return false;
        }
        match &target.kind {
            ExprKind::Identifier(g) => binds.contains(g),
            ExprKind::FieldAccess { object, field } => {
                matches!(&object.kind, ExprKind::Identifier(r)
                    if self.heap_env_aggregate_owners
                        .get(r)
                        .is_some_and(|fs| fs.contains(field)))
            }
            ExprKind::Index { object, .. } => {
                matches!(&object.kind, ExprKind::Identifier(v)
                    if self.heap_env_vec_owners.contains(v))
            }
            _ => false,
        }
    }

    /// Statement-level companion to [`expr_has_heap_env_misuse`] for a nested
    /// block (an `if`/`for`/`while` body, a `defer`, …). Exhaustive over
    /// `StmtKind`. NOTE: a nested `let g = make(..)` is NOT a sanctioned binding
    /// (only top-level lets are tracked), so its RHS is walked and rejected as a
    /// non-sanctioned heap-env call — nested heap-env bindings are a deferred,
    /// over-rejected shape, never a silent miss.
    fn block_has_heap_env_misuse(&self, b: &Block, binds: &HashSet<String>) -> bool {
        for stmt in &b.stmts {
            let bad = match &stmt.kind {
                StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                    self.expr_has_heap_env_misuse(value, binds)
                }
                StmtKind::LetUninit { .. } => false,
                StmtKind::Expr(e) => self.expr_has_heap_env_misuse(e, binds),
                StmtKind::Assign { target, value } => {
                    // A heap-env reassignment nested in a branch / loop
                    // (`if c { g = f }`, `for .. { r.f = make(i) }`) is sanctioned
                    // too — the binding / owner is top-level and still in scope,
                    // and the codegen drop-old fires once per execution.
                    if self.is_heap_env_reassign(target, value, binds) {
                        false
                    } else {
                        self.expr_has_heap_env_misuse(target, binds)
                            || self.expr_has_heap_env_misuse(value, binds)
                    }
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.expr_has_heap_env_misuse(target, binds)
                        || self.expr_has_heap_env_misuse(value, binds)
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.block_has_heap_env_misuse(body, binds)
                }
                StmtKind::MultiAssign { values, .. } => values
                    .iter()
                    .any(|v| self.expr_has_heap_env_misuse(v, binds)),
            };
            if bad {
                return true;
            }
        }
        b.final_expr
            .as_deref()
            .is_some_and(|t| self.expr_has_heap_env_misuse(t, binds))
    }

    /// Exhaustive (no silent `_ => false` for any sub-expression-bearing
    /// variant) walk for a heap-env-binding misuse or a non-sanctioned heap-env
    /// call inside `e`. Allows exactly `f(args)` for a binding `f` (recursing
    /// the args, not the callee); a bare reference to a binding anywhere else is
    /// a misuse, and a heap-env-producing call in any non-sanctioned position is
    /// a leak. Leaves (literals, paths, `self`, …) hold no sub-expression and
    /// return `false`.
    fn expr_has_heap_env_misuse(&self, e: &Expr, binds: &HashSet<String>) -> bool {
        let mis = |x: &Expr| self.expr_has_heap_env_misuse(x, binds);
        let any = |xs: &[Expr]| xs.iter().any(mis);
        let any_args = |xs: &[CallArg]| xs.iter().any(|a| mis(&a.value));
        match &e.kind {
            // A bare reference to a heap-env binding (NOT in callee position —
            // that is handled in `Call`) escapes / aliases the single owner. A
            // bare reference to an aggregate OWNER `h` likewise escapes the
            // struct (and its embedded env) — only `(h.f)(x)` / `h.non_closure`
            // are allowed, handled in `Call` / `FieldAccess`.
            ExprKind::Identifier(n) => {
                binds.contains(n)
                    || self.heap_env_aggregate_owners.contains_key(n)
                    || self.heap_env_tuple_owners.contains_key(n)
                    || self.heap_env_array_owners.contains_key(n)
                    || self.heap_env_vec_owners.contains(n)
            }
            ExprKind::Call { callee, args } => {
                // The one supported use: `f(args)` for a binding `f`. The callee
                // occurrence is sanctioned; the args may still misuse a binding.
                if let ExprKind::Identifier(n) = &callee.kind {
                    if binds.contains(n) {
                        return any_args(args);
                    }
                }
                // Sanctioned field-call on an aggregate owner: `(h.f)(args)`. The
                // `h.f` callee occurrence is allowed (it invokes, doesn't move the
                // env); only the args can still misuse.
                if let ExprKind::FieldAccess { object, .. } = &callee.kind {
                    if let ExprKind::Identifier(n) = &object.kind {
                        if self.heap_env_aggregate_owners.contains_key(n) {
                            return any_args(args);
                        }
                    }
                }
                // Sanctioned tuple-index call on a tuple owner: `(t.0)(args)`. Like
                // the struct field-call, invoking through the element doesn't move
                // the env; only the args can still misuse.
                if let ExprKind::TupleIndex { object, .. } = &callee.kind {
                    if let ExprKind::Identifier(n) = &object.kind {
                        if self.heap_env_tuple_owners.contains_key(n) {
                            return any_args(args);
                        }
                    }
                }
                // Sanctioned index call on an array OR Vec owner: `(a[i])(args)` /
                // `(v[i])(args)`. As with the tuple-index call, invoking through the
                // element doesn't move the env; only the args can still misuse. The
                // index `i` may be any expression — walked via `any_args` only if it
                // appears in the args; the callee index occurrence itself is allowed.
                if let ExprKind::Index { object, .. } = &callee.kind {
                    if let ExprKind::Identifier(n) = &object.kind {
                        if self.heap_env_array_owners.contains_key(n)
                            || self.heap_env_vec_owners.contains(n)
                        {
                            return any_args(args);
                        }
                    }
                }
                // RULE A: a non-sanctioned heap-env-producing call leaks (the
                // sanctioned `let`-RHS calls never reach this walk).
                if self.is_heap_env_producing_call(e) {
                    return true;
                }
                // A callee EXPRESSION that is itself a misuse (e.g. a bare owner
                // name shadowing a free fn, or a computed callee referencing a
                // binding) is rejected before the arg sanction — preserving the
                // pre-slice `mis(callee)` semantics.
                if mis(callee) {
                    return true;
                }
                // By-value arg-pass (borrow): a heap-env binding passed BY VALUE
                // to a known free function whose matching parameter is
                // borrows-only (the callee only CALLS it — `fn_param_is_borrows_only`)
                // is sanctioned. The callee borrows the shared RC env and never
                // frees it; the caller retains sole ownership and RC-drops it once
                // at scope exit — so no inc and no move-out are needed, and the
                // fat pointer is simply passed by value (existing fn-value arg
                // codegen). Only PLAIN positional args map index→param soundly, so
                // bail the whole sanction if any arg is labeled; a `mut`-marked arg
                // (pass-by-mut-ref) is never treated as a borrow. Other args, and
                // the callee, are still walked.
                let all_positional = args.iter().all(|a| a.label.is_none());
                if all_positional {
                    if let ExprKind::Identifier(callee_name) = &callee.kind {
                        if let Some(callee_fn) = self.fn_asts.get(callee_name) {
                            return args.iter().enumerate().any(|(i, a)| {
                                // The arg is borrowed when it is a heap-env BINDING
                                // (`let f = make()`) or a heap-env CONTAINER OWNER —
                                // a struct (`let h = H { f: make() }`), tuple / array
                                // (`let t = (make(), 0)`), or `Vec[Fn]`
                                // (`let v = [make()]vec`) — passed by value to a
                                // borrows-only param. The callee only CALLS the
                                // closure(s) (`f(x)` / `(h.f)(x)` / `(t.0)(x)` /
                                // `(v[i])(x)`), so it borrows the shared RC env(s) and
                                // never frees them; the caller retains sole ownership
                                // and RC-drops each env once at scope exit (no inc, no
                                // move-out — a call arg is not a return move-out, so
                                // the owner's env slot is never neutralized).
                                let borrowed = !a.mut_marker
                                    && matches!(&a.value.kind,
                                        ExprKind::Identifier(n)
                                            if binds.contains(n)
                                                || self.heap_env_aggregate_owners.contains_key(n)
                                                || self.heap_env_tuple_owners.contains_key(n)
                                                || self.heap_env_array_owners.contains_key(n)
                                                || self.heap_env_vec_owners.contains(n))
                                    && self.fn_param_is_borrows_only(callee_fn, i);
                                !borrowed && mis(&a.value)
                            });
                        }
                    }
                }
                // `mis(callee)` was already checked above; only the args remain.
                any_args(args)
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                // Sanctioned methods on a Vec owner `v` (Vec-store slice): a
                // heap-env PUSH (`v.push(make(k))` / `v.push(f)`) — the supported
                // store, whose env the Vec's dynamic drop loop will free — and the
                // read-only `len`/`is_empty`/`capacity`. Any OTHER method on a Vec
                // owner (`pop`, `remove`, `get`, `clear`, `clone`, iteration, or a
                // NON-heap-env push) escapes / aliases / drops an element without
                // env accounting, OR would mix a stack-env element into a Vec the
                // drop loop frees unconditionally — rejected (the env would leak,
                // double-free, or free a stack address).
                if let ExprKind::Identifier(n) = &object.kind {
                    if self.heap_env_vec_owners.contains(n) {
                        let push_heap_env = matches!(method.as_str(), "push" | "push_back")
                            && args.len() == 1
                            && (self.is_heap_env_producing_call(&args[0].value)
                                || matches!(&args[0].value.kind,
                                    ExprKind::Identifier(a) if binds.contains(a)));
                        let readonly = args.is_empty()
                            && matches!(method.as_str(), "len" | "is_empty" | "capacity");
                        return !(push_heap_env || readonly);
                    }
                }
                mis(object) || any_args(args)
            }
            ExprKind::Binary { left, right, .. }
            | ExprKind::Pipe { left, right }
            | ExprKind::NilCoalesce { left, right } => mis(left) || mis(right),
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => mis(operand),
            ExprKind::Cast { expr, .. } => mis(expr),
            ExprKind::OptionalChain { object, args, .. } => {
                mis(object) || args.as_deref().is_some_and(any_args)
            }
            ExprKind::FieldAccess { object, field } => {
                // A non-call projection of an aggregate owner's CLOSURE field
                // escapes the env (`return h.f`, `let g = h.f`, `[h.f]`, …) →
                // misuse; a non-closure field read (`h.count`) is fine; otherwise
                // recurse into the object. A call form `(h.f)(x)` is sanctioned in
                // the `Call` arm before reaching here.
                if let ExprKind::Identifier(n) = &object.kind {
                    if let Some(closure_fields) = self.heap_env_aggregate_owners.get(n) {
                        return closure_fields.contains(field);
                    }
                }
                mis(object)
            }
            ExprKind::TupleIndex { object, index } => {
                // A non-call projection of a tuple owner's CLOSURE element
                // (`let g = t.0`, `return t.0`, …) escapes the env → misuse; a
                // non-closure element read (`t.1`) is fine; otherwise recurse. A
                // call form `(t.0)(x)` is sanctioned in the `Call` arm before here.
                if let ExprKind::Identifier(n) = &object.kind {
                    if let Some(elem_idxs) = self.heap_env_tuple_owners.get(n) {
                        return elem_idxs.contains(&(*index as usize));
                    }
                }
                mis(object)
            }
            ExprKind::Index { object, index } => {
                // A non-call projection of an array owner's CLOSURE element
                // (`let g = a[0]`, `return a[0]`, …) escapes the env → misuse; a
                // call form `(a[i])(x)` is sanctioned in the `Call` arm before here.
                // A constant index picks a specific element (reject iff that element
                // is a heap-env closure); a dynamic index can't be proven to land on
                // a non-closure element, so it is conservatively rejected. The index
                // sub-expression is still walked for its own misuse.
                if let ExprKind::Identifier(n) = &object.kind {
                    if let Some(elem_idxs) = self.heap_env_array_owners.get(n) {
                        let elem_escapes = match &index.kind {
                            ExprKind::Integer(c, _) => elem_idxs.contains(&(*c as usize)),
                            _ => true,
                        };
                        return elem_escapes || mis(index);
                    }
                }
                mis(object) || mis(index)
            }
            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => any(es),
            ExprKind::PrefixCollectionLiteral { items, .. } => any(items),
            ExprKind::RepeatLiteral { value, count, .. } => mis(value) || mis(count),
            ExprKind::MapLiteral(pairs) => pairs.iter().any(|(k, v)| mis(k) || mis(v)),
            ExprKind::StructLiteral { fields, spread, .. } => {
                fields.iter().any(|f| mis(&f.value)) || spread.as_deref().is_some_and(mis)
            }
            ExprKind::Range { start, end, .. } => {
                start.as_deref().is_some_and(mis) || end.as_deref().is_some_and(mis)
            }
            ExprKind::InterpolatedStringLit(parts) => parts
                .iter()
                .any(|p| matches!(p, ParsedInterpolationPart::Expr(inner) if mis(inner))),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                mis(condition)
                    || self.block_has_heap_env_misuse(then_block, binds)
                    || else_branch.as_deref().is_some_and(mis)
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                mis(value)
                    || self.block_has_heap_env_misuse(then_block, binds)
                    || else_branch.as_deref().is_some_and(mis)
            }
            ExprKind::Match { scrutinee, arms } => {
                mis(scrutinee) || arms.iter().any(|a| mis(&a.body))
            }
            ExprKind::While {
                condition, body, ..
            } => mis(condition) || self.block_has_heap_env_misuse(body, binds),
            ExprKind::WhileLet { value, body, .. } => {
                mis(value) || self.block_has_heap_env_misuse(body, binds)
            }
            ExprKind::For { iterable, body, .. } => {
                mis(iterable) || self.block_has_heap_env_misuse(body, binds)
            }
            ExprKind::Loop { body, .. } => self.block_has_heap_env_misuse(body, binds),
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Par(b) => self.block_has_heap_env_misuse(b, binds),
            ExprKind::LabeledBlock { body, .. } => self.block_has_heap_env_misuse(body, binds),
            ExprKind::Lock { mutex, body, .. } => {
                mis(mutex) || self.block_has_heap_env_misuse(body, binds)
            }
            ExprKind::Providers { body, .. } => self.block_has_heap_env_misuse(body, binds),
            ExprKind::Return(inner) | ExprKind::Break { value: inner, .. } => {
                inner.as_deref().is_some_and(mis)
            }
            // A nested closure capturing a heap-env binding `f` lets `f`'s env
            // escape into the (possibly escaping) closure env — not supported.
            // A param that shadows a binding name drops it from the live set.
            ExprKind::Closure { params, body, .. } => {
                let shadowed: HashSet<String> = params
                    .iter()
                    .flat_map(|p| p.pattern.binding_names())
                    .collect();
                if shadowed.is_empty() {
                    mis(body)
                } else {
                    let live: HashSet<String> = binds.difference(&shadowed).cloned().collect();
                    self.expr_has_heap_env_misuse(body, &live)
                }
            }
            // Leaves — no sub-expression can reference a binding.
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(..)
            | ExprKind::ByteLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::MultiStringLit(..)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(..)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Continue { .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => false,
        }
    }

    /// By-value arg-pass slice (B-2026-06-22-2): `true` when the `Fn`-value
    /// parameter named `pname` ESCAPES (is used as anything other than the
    /// callee of a direct call `pname(args)`) anywhere in `body`. A borrows-only
    /// callee — one for which this returns `false` — merely CALLS the closure and
    /// never returns / stores / re-binds / captures it, so a heap-env closure
    /// passed into that parameter is a pure BORROW: the callee touches the shared
    /// RC env but never frees it, and the CALLER retains sole ownership and
    /// RC-drops it once at scope exit (no inc, no move-out needed at the call).
    ///
    /// Deliberately self-contained — it consults NO owner sets (those are the
    /// CALLER's state), so a function's borrows-only-ness is a property of its own
    /// body alone and does not vary by call site. The walk is the exhaustive,
    /// single-name dual of [`Self::expr_has_heap_env_misuse`]: only a TOP-LEVEL
    /// `pname(args)` in callee position is sanctioned; every other occurrence
    /// escapes. The `in_closure` flag, set once the walk descends into a nested
    /// closure body, DISABLES even that sanction — inside a (possibly escaping)
    /// closure ANY mention of `pname` is a capture, so `|y| pname(y)` retains the
    /// env and is correctly an escape. Any over-approximation (treating a shadow
    /// or an exotic-but-safe use as an escape) only REJECTS a valid arg-pass —
    /// never admits an unsound one.
    fn fn_value_escapes_block(&self, body: &Block, pname: &str, in_closure: bool) -> bool {
        body.stmts.iter().any(|s| match &s.kind {
            StmtKind::Let { value, .. }
            | StmtKind::LetElse { value, .. }
            | StmtKind::Expr(value) => self.fn_value_escapes_expr(value, pname, in_closure),
            StmtKind::LetUninit { .. } => false,
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.fn_value_escapes_expr(target, pname, in_closure)
                    || self.fn_value_escapes_expr(value, pname, in_closure)
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.fn_value_escapes_block(body, pname, in_closure)
            }
            StmtKind::MultiAssign { values, .. } => values
                .iter()
                .any(|v| self.fn_value_escapes_expr(v, pname, in_closure)),
        }) || body
            .final_expr
            .as_deref()
            .is_some_and(|t| self.fn_value_escapes_expr(t, pname, in_closure))
    }

    /// Expression companion to [`Self::fn_value_escapes_block`]. Exhaustive (no
    /// silent `_ => false` for any sub-expression-bearing variant) so a new AST
    /// shape can never silently admit an unsound escape.
    fn fn_value_escapes_expr(&self, e: &Expr, pname: &str, in_closure: bool) -> bool {
        let esc = |x: &Expr| self.fn_value_escapes_expr(x, pname, in_closure);
        let any = |xs: &[Expr]| xs.iter().any(esc);
        let any_args = |xs: &[CallArg]| xs.iter().any(|a| esc(&a.value));
        match &e.kind {
            // A bare reference to the param escapes; only a top-level `pname(args)`
            // callee position (handled in `Call`) is a non-escaping borrow-call.
            ExprKind::Identifier(n) => n == pname,
            ExprKind::Call { callee, args } => {
                if !in_closure {
                    if let ExprKind::Identifier(n) = &callee.kind {
                        if n == pname {
                            // `pname(args)` — the sanctioned borrow-call. The callee
                            // occurrence does not escape; the args still might.
                            return any_args(args);
                        }
                    }
                    // Owner field-call `(pname.field)(args)`: invokes a closure
                    // stored in the owner param's field WITHOUT moving the env out of
                    // the owner, so the param is BORROWED — the caller still owns the
                    // owner + its env. Only the CALL form is sanctioned; `pname.field`
                    // in value position (a closure projection) stays an escape via the
                    // `FieldAccess` arm. Self-contained, so a binding param (a closure
                    // value, which has no fields) never reaches this — `pname.x` on a
                    // closure is a type error. Disabled inside a nested closure
                    // (`in_closure`), where any mention of `pname` is a capture.
                    if let ExprKind::FieldAccess { object, .. } = &callee.kind {
                        if matches!(&object.kind, ExprKind::Identifier(n) if n == pname) {
                            return any_args(args);
                        }
                    }
                    // Owner tuple-index call `(pname.N)(args)`: invokes a closure
                    // ELEMENT of a tuple param without moving the env out — the param
                    // is BORROWED, exactly like the struct field-call. Only the CALL
                    // form; `pname.N` in value position stays an escape via the
                    // `TupleIndex` arm. `index` is a literal (no sub-expression to
                    // walk). Disabled inside a nested closure (`in_closure`).
                    if let ExprKind::TupleIndex { object, .. } = &callee.kind {
                        if matches!(&object.kind, ExprKind::Identifier(n) if n == pname) {
                            return any_args(args);
                        }
                    }
                    // Owner index call `(pname[i])(args)`: invokes a closure ELEMENT
                    // of an array / `Vec[Fn]` param without moving the env out — the
                    // param is BORROWED. Only the CALL form; `pname[i]` in value
                    // position stays an escape via the `Index` arm. The index
                    // sub-expression is still walked (it could itself reference
                    // `pname`); the callee element occurrence is the borrow-call.
                    if let ExprKind::Index { object, index } = &callee.kind {
                        if matches!(&object.kind, ExprKind::Identifier(n) if n == pname) {
                            return esc(index) || any_args(args);
                        }
                    }
                }
                esc(callee) || any_args(args)
            }
            ExprKind::MethodCall { object, args, .. } => esc(object) || any_args(args),
            ExprKind::Binary { left, right, .. }
            | ExprKind::Pipe { left, right }
            | ExprKind::NilCoalesce { left, right } => esc(left) || esc(right),
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => esc(operand),
            ExprKind::Cast { expr, .. } => esc(expr),
            ExprKind::OptionalChain { object, args, .. } => {
                esc(object) || args.as_deref().is_some_and(any_args)
            }
            ExprKind::FieldAccess { object, .. } => esc(object),
            ExprKind::TupleIndex { object, .. } => esc(object),
            ExprKind::Index { object, index } => esc(object) || esc(index),
            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => any(es),
            ExprKind::PrefixCollectionLiteral { items, .. } => any(items),
            ExprKind::RepeatLiteral { value, count, .. } => esc(value) || esc(count),
            ExprKind::MapLiteral(pairs) => pairs.iter().any(|(k, v)| esc(k) || esc(v)),
            ExprKind::StructLiteral { fields, spread, .. } => {
                fields.iter().any(|f| esc(&f.value)) || spread.as_deref().is_some_and(esc)
            }
            ExprKind::Range { start, end, .. } => {
                start.as_deref().is_some_and(esc) || end.as_deref().is_some_and(esc)
            }
            ExprKind::InterpolatedStringLit(parts) => parts
                .iter()
                .any(|p| matches!(p, ParsedInterpolationPart::Expr(inner) if esc(inner))),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                esc(condition)
                    || self.fn_value_escapes_block(then_block, pname, in_closure)
                    || else_branch.as_deref().is_some_and(esc)
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                esc(value)
                    || self.fn_value_escapes_block(then_block, pname, in_closure)
                    || else_branch.as_deref().is_some_and(esc)
            }
            ExprKind::Match { scrutinee, arms } => {
                esc(scrutinee) || arms.iter().any(|a| esc(&a.body))
            }
            ExprKind::While {
                condition, body, ..
            } => esc(condition) || self.fn_value_escapes_block(body, pname, in_closure),
            ExprKind::WhileLet { value, body, .. } => {
                esc(value) || self.fn_value_escapes_block(body, pname, in_closure)
            }
            ExprKind::For { iterable, body, .. } => {
                esc(iterable) || self.fn_value_escapes_block(body, pname, in_closure)
            }
            ExprKind::Loop { body, .. } => self.fn_value_escapes_block(body, pname, in_closure),
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Seq(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Par(b) => self.fn_value_escapes_block(b, pname, in_closure),
            ExprKind::LabeledBlock { body, .. } => {
                self.fn_value_escapes_block(body, pname, in_closure)
            }
            ExprKind::Lock { mutex, body, .. } => {
                esc(mutex) || self.fn_value_escapes_block(body, pname, in_closure)
            }
            ExprKind::Providers { body, .. } => {
                self.fn_value_escapes_block(body, pname, in_closure)
            }
            ExprKind::Return(inner) | ExprKind::Break { value: inner, .. } => {
                inner.as_deref().is_some_and(esc)
            }
            // A nested closure that mentions `pname` CAPTURES it — the env escapes
            // into a (possibly escaping) closure env, so even `|y| pname(y)` is an
            // escape (walked with `in_closure = true`, which disables the
            // borrow-call sanction). A closure param that shadows `pname` rebinds
            // the name — inner uses are the shadow, not the param.
            ExprKind::Closure { params, body, .. } => {
                let shadowed = params
                    .iter()
                    .flat_map(|p| p.pattern.binding_names())
                    .any(|n| n == pname);
                !shadowed && self.fn_value_escapes_expr(body, pname, true)
            }
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(..)
            | ExprKind::ByteLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::MultiStringLit(..)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(..)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Continue { .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => false,
        }
    }

    /// By-value arg-pass slice (B-2026-06-22-2): `true` when parameter `idx` of
    /// `f` is a plain (non-`self`) value parameter that the body only ever CALLS
    /// — so a heap-env closure passed into it is borrowed, not owned (see
    /// [`Self::fn_value_escapes_block`]). A destructuring-pattern param, an
    /// out-of-range index, or a param the body lets escape all return `false`
    /// (conservatively NOT borrows-only → the arg-pass stays rejected).
    fn fn_param_is_borrows_only(&self, f: &Function, idx: usize) -> bool {
        let Some(param) = f.params.get(idx) else {
            return false;
        };
        let Some(pname) = param.name() else {
            return false;
        };
        !self.fn_value_escapes_block(&f.body, pname, false)
    }

    /// Stdlib collection type heads whose element-store methods (`push` /
    /// `insert` / `push_back` / `push_front`) move the argument INTO the
    /// receiver — so a capturing closure stored there outlives its stack env if
    /// the receiver escapes. Gating the container-store marking on a *known*
    /// collection is what keeps that marking one-sided (a like-named method on a
    /// user type, which might invoke rather than store, is never marked).
    fn is_collection_type_head(name: &str) -> bool {
        matches!(
            name,
            "Vec"
                | "VecDeque"
                | "Deque"
                | "Map"
                | "HashMap"
                | "BTreeMap"
                | "Set"
                | "HashSet"
                | "BTreeSet"
        )
    }

    /// Collection methods that STORE their element argument into the receiver,
    /// as opposed to `sort_by` / `map` / `retain` / `each`, which invoke a
    /// closure argument synchronously within the call and do not retain it (so
    /// passing a capturing closure to those is sound and must NOT mark).
    fn is_element_storing_method(name: &str) -> bool {
        matches!(name, "push" | "insert" | "push_back" | "push_front")
    }

    /// `true` when a `let` binds a stdlib-collection local — by type annotation
    /// (`let v: Vec[..] = …`), by a collection literal RHS (`[..]`, `Vec[..]`, a
    /// map / repeat literal), or by a collection constructor RHS (`Vec.new()` /
    /// `Map.with_capacity(..)`). Only such bindings are eligible for the
    /// container-store marking; every other shape under-approximates (sound: a
    /// missed collection leaves a residual, it never falsely rejects).
    fn let_binds_collection(ty: Option<&TypeExpr>, value: &Expr) -> bool {
        if let Some(TypeKind::Path(p)) = ty.map(|t| &t.kind) {
            if p.segments
                .last()
                .is_some_and(|h| Self::is_collection_type_head(h))
            {
                return true;
            }
        }
        match &value.kind {
            ExprKind::ArrayLiteral(_)
            | ExprKind::MapLiteral(_)
            | ExprKind::RepeatLiteral { .. } => true,
            ExprKind::PrefixCollectionLiteral { type_name, .. } => {
                Self::is_collection_type_head(type_name)
            }
            // `Vec.new()` / `Map.with_capacity(..)` — a 2-segment associated
            // call `Collection.method(..)`, which lowers to a `Call` whose
            // callee is a `Path` (`["Vec", "new"]`); the head segment is the
            // collection type. (A `MethodCall` on a bare collection identifier
            // is matched too, in case a form ever reaches here un-pathed.)
            ExprKind::Call { callee, .. } => matches!(
                &callee.kind,
                ExprKind::Path { segments, .. }
                    if segments.first().is_some_and(|h| Self::is_collection_type_head(h))
            ),
            ExprKind::MethodCall { object, .. } => {
                matches!(&object.kind, ExprKind::Identifier(h) if Self::is_collection_type_head(h))
            }
            _ => false,
        }
    }

    /// The set of names a closure in `func`'s body could capture from the
    /// enclosing frame: `func`'s params + its top-level `let` bindings.
    fn outer_capturable_names(func: &Function) -> HashSet<String> {
        let mut outer: HashSet<String> = func
            .params
            .iter()
            .flat_map(|p| p.pattern.binding_names())
            .collect();
        for stmt in &func.body.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    outer.extend(pattern.binding_names());
                }
                StmtKind::LetUninit { name, .. } => {
                    outer.insert(name.clone());
                }
                _ => {}
            }
        }
        outer
    }

    /// Slice 1 (B-2026-06-22-2): if `func`'s direct tail is a *capturing*
    /// closure literal, return its span — the closure escapes via the return,
    /// so it gets a reference-counted HEAP environment. `None` otherwise (a
    /// non-capturing tail closure needs no heap env; other escape shapes are
    /// still guarded). This is the one return shape Slice 1 supports.
    pub(super) fn func_tail_heap_closure_span(&self, func: &Function) -> Option<(usize, usize)> {
        let tail = func.body.final_expr.as_deref()?;
        if let ExprKind::Closure { params, body, .. } = &tail.kind {
            if self.closure_literal_captures(params, body, &Self::outer_capturable_names(func)) {
                return Some((tail.span.offset, tail.span.length));
            }
        }
        None
    }

    /// Currying sibling of [`Self::func_tail_heap_closure_span`]
    /// (B-2026-07-12-12): if the tail of a CLOSURE body (`|n| |x| x + n`, or
    /// `|n| { … |x| x + n }`) is itself a *capturing* closure literal, that
    /// inner closure escapes via the outer closure's return, so its
    /// environment must be a per-call reference-counted HEAP box — not a stack
    /// alloca that every `make(n)` instance would alias. Returns the inner
    /// closure's span. The outer's capturable names are its own params plus its
    /// body-block's top-level `let` bindings (the analog of
    /// `outer_capturable_names` for a closure rather than a named fn).
    pub(super) fn closure_tail_heap_closure_span(
        &self,
        outer_params: &[ClosureParam],
        outer_body: &Expr,
    ) -> Option<(usize, usize)> {
        // Unwrap a block body to its tail expression.
        let tail = match &outer_body.kind {
            ExprKind::Block(block) | ExprKind::Seq(block) => block.final_expr.as_deref()?,
            _ => outer_body,
        };
        let ExprKind::Closure { params, body, .. } = &tail.kind else {
            return None;
        };
        // Outer capturable names: the outer closure's params + its body-block's
        // top-level lets (a bare-expression body contributes only params).
        let mut outer: HashSet<String> = outer_params
            .iter()
            .flat_map(|p| p.pattern.binding_names())
            .collect();
        if let ExprKind::Block(block) | ExprKind::Seq(block) = &outer_body.kind {
            for stmt in &block.stmts {
                match &stmt.kind {
                    StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                        outer.extend(pattern.binding_names());
                    }
                    StmtKind::LetUninit { name, .. } => {
                        outer.insert(name.clone());
                    }
                    _ => {}
                }
            }
        }
        if self.closure_literal_captures(params, body, &outer) {
            return Some((tail.span.offset, tail.span.length));
        }
        None
    }

    /// Currying (B-2026-07-12-12): the local closure-VALUE bindings in `func`
    /// whose CALL returns a heap-env closure — `let make = |n| |x| x + n;`
    /// binds `make`, whose call `make(5)` yields the inner closure's RC heap
    /// env. A forward scan collects both the direct form (a `let` whose RHS is a
    /// closure literal whose tail is a *capturing* closure) and transitive
    /// value copies (`let g = make`, `make` already collected). Populated per
    /// function before the misuse guard runs; consulted via
    /// `is_heap_env_producing_call` so a `make(..)` call reuses the same
    /// free/owner/misuse machinery as a call to a named `fns_returning_heap_env`
    /// function. Top-level `let`s only — mirrors the named-fn machinery's
    /// top-level scan discipline.
    pub(super) fn compute_curry_closure_vars(&self, func: &Function) -> HashSet<String> {
        let mut set: HashSet<String> = HashSet::new();
        for stmt in &func.body.stmts {
            if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
                let is_curry = match &value.kind {
                    ExprKind::Closure { params, body, .. } => {
                        self.closure_tail_heap_closure_span(params, body).is_some()
                    }
                    ExprKind::Identifier(n) => set.contains(n),
                    _ => false,
                };
                if is_curry {
                    if let [b] = pattern.binding_names().as_slice() {
                        set.insert(b.clone());
                    }
                }
            }
        }
        set
    }

    /// Populate `fns_returning_heap_env` (functions whose return value is a
    /// heap-env closure) from `self.fn_asts`, before any body compiles. A
    /// `let f = <call to such a fn>` therefore owns a heap env and is given a
    /// `FreeClosureEnv` cleanup.
    pub(super) fn compute_fns_returning_heap_env(&mut self) {
        let funcs: Vec<Function> = self.fn_asts.values().cloned().collect();
        let mut set = std::collections::HashSet::new();
        // Seed: a direct capturing-closure-literal tail mints a heap env here.
        for func in &funcs {
            if self.func_tail_heap_closure_span(func).is_some() {
                set.insert(func.name.clone());
            }
        }
        // Fixpoint (return-again slice): a function that RETURNS a heap-env
        // BINDING — a local bound from a call to a fn already in the set
        // (transitively through copies `let g = f`), returned as a
        // bare-identifier tail or a top-level `return <binding>` — also yields a
        // heap env to ITS caller; codegen moves the env box out (neutralizes the
        // source's `FreeClosureEnv`), so the same box flows on at the same
        // refcount. Repeat until stable so a relay-of-a-relay is recognized once
        // its inner relay is.
        loop {
            let mut changed = false;
            for func in &funcs {
                if set.contains(&func.name) {
                    continue;
                }
                if self.func_returns_heap_env_binding(func, &set) {
                    set.insert(func.name.clone());
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.fns_returning_heap_env = set;
    }

    /// `true` when `func` returns — as a bare-identifier TAIL or a top-level
    /// `return <bare identifier>;` — a local that is a heap-env binding (bound
    /// from a call to a fn in `set`, transitively through copies). Branch-buried
    /// returns are intentionally NOT detected: a sound under-approximation that
    /// keeps detection in lockstep with the misuse guard (which only sanctions
    /// these same two top-level return shapes) and the move-out codegen (which
    /// neutralizes the source on the executed path) — never a silent miscompile.
    fn func_returns_heap_env_binding(&self, func: &Function, set: &HashSet<String>) -> bool {
        // Heap-env bindings local to `func` (forward scan; transitive copies).
        let mut binds: HashSet<String> = HashSet::new();
        for stmt in &func.body.stmts {
            if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
                let is_src = self.is_heap_env_producing_call_in(value, set)
                    || matches!(&value.kind, ExprKind::Identifier(n) if binds.contains(n));
                if is_src {
                    if let [b] = pattern.binding_names().as_slice() {
                        binds.insert(b.clone());
                    }
                }
            }
        }
        if binds.is_empty() {
            return false;
        }
        let is_bound = |e: &Expr| matches!(&e.kind, ExprKind::Identifier(n) if binds.contains(n));
        if func.body.final_expr.as_deref().is_some_and(&is_bound) {
            return true;
        }
        func.body.stmts.iter().any(|s| match &s.kind {
            StmtKind::Expr(e) => {
                matches!(&e.kind, ExprKind::Return(Some(inner)) if is_bound(inner))
            }
            _ => false,
        })
    }

    /// If `e` is a call to a function that returns a heap-env-OWNING aggregate
    /// (`build(..)` with `build` ∈ `agg_map`), return that function's owned-field
    /// set — the binding `let r = build(..)` then OWNS those env boxes (the caller
    /// registers an instance `FreeClosureEnv` on each named field; the callee moved
    /// them out at the same refcount). `None` otherwise. `agg_map` is passed
    /// explicitly so the detection fixpoint can query the in-progress map.
    fn aggregate_call_owner_fields(
        &self,
        e: &Expr,
        agg_map: &HashMap<String, HashSet<String>>,
    ) -> Option<HashSet<String>> {
        let ExprKind::Call { callee, .. } = &e.kind else {
            return None;
        };
        let name = match &callee.kind {
            ExprKind::Identifier(n) => n,
            ExprKind::Path { segments, .. } if segments.len() == 1 => &segments[0],
            _ => return None,
        };
        agg_map.get(name).cloned()
    }

    /// Collect, for `func`, the top-level heap-env closure BINDINGS and the
    /// aggregate OWNERS (struct locals owning one or more heap-env fields). Forward
    /// scan so a copy `let g = f` is collected once `f` is a binding (transitive
    /// `let g = f; let h = g`). An owner is bound from a struct literal with a
    /// sanctioned heap-env store field (`let h = H { f: <fresh-call|binding> }`) OR
    /// from a call to an aggregate-returning function (`let r = build(k)`, using
    /// `agg_map`). Shared by the misuse guard (pass 1) and the aggregate-return
    /// detection fixpoint — keeping owner reasoning in exactly one place.
    fn collect_heap_env_binds_and_owners(
        &self,
        func: &Function,
        agg_map: &HashMap<String, HashSet<String>>,
    ) -> (HashSet<String>, HashMap<String, HashSet<String>>) {
        let mut binds: HashSet<String> = HashSet::new();
        let mut owners: HashMap<String, HashSet<String>> = HashMap::new();
        for stmt in &func.body.stmts {
            if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
                let is_source = self.is_heap_env_producing_call(value)
                    || matches!(&value.kind, ExprKind::Identifier(n) if binds.contains(n));
                if is_source {
                    if let [b] = pattern.binding_names().as_slice() {
                        binds.insert(b.clone());
                    }
                } else if let ExprKind::Identifier(src) = &value.kind {
                    // Owner COPY (`let s = a`, `a` an aggregate owner): forward scan,
                    // so `s` adopts `a`'s owned fields and a copy-of-a-copy
                    // (`let t = s`) chains. COPY semantics — `a` stays a live owner,
                    // and codegen INCs the shared RC env so each owner RC-drops once
                    // (mirrors the `let g = f` binding copy). Sits after the
                    // binding-source check (a heap-env binding copy is `binds`, not
                    // an owner) and before the literal/call owner-construction arms.
                    if let Some(fields) = owners.get(src).cloned() {
                        if let [b] = pattern.binding_names().as_slice() {
                            owners.insert(b.clone(), fields);
                        }
                    }
                } else if let Some(fields) = self.aggregate_call_owner_fields(value, agg_map) {
                    if let [b] = pattern.binding_names().as_slice() {
                        owners.insert(b.clone(), fields);
                    }
                } else {
                    let fields = self.struct_literal_heap_env_store_fields(value, &binds);
                    if !fields.is_empty() {
                        if let [b] = pattern.binding_names().as_slice() {
                            owners.insert(b.clone(), fields.into_iter().collect());
                        }
                    }
                }
            }
        }
        (binds, owners)
    }

    /// Populate `fns_returning_heap_env_aggregate` (functions that RETURN a struct
    /// local owning one or more heap-env closure fields, as a bare tail / top-level
    /// `return h`). Maps fn name → the returned struct's owned-field names. Runs
    /// after `compute_fns_returning_heap_env` (an owner can be built from a fresh
    /// heap-env call). A FIXPOINT so a relay-of-aggregate (`let r = build(k); r`)
    /// is recognized once its inner builder is.
    pub(super) fn compute_fns_returning_heap_env_aggregate(&mut self) {
        let funcs: Vec<Function> = self.fn_asts.values().cloned().collect();
        let mut map: HashMap<String, HashSet<String>> = HashMap::new();
        loop {
            let mut changed = false;
            for func in &funcs {
                if map.contains_key(&func.name) {
                    continue;
                }
                if let Some(fields) = self.func_returns_heap_env_aggregate(func, &map) {
                    map.insert(func.name.clone(), fields);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.fns_returning_heap_env_aggregate = map;
    }

    /// The owned-field set if `func` returns — as a bare-identifier TAIL or a
    /// top-level `return <bare identifier>;` — a local that is an aggregate owner
    /// (per `collect_heap_env_binds_and_owners` against `map`). `None` otherwise.
    /// Branch-buried returns are intentionally NOT detected — the sound
    /// under-approximation that keeps detection in lockstep with the misuse guard
    /// and the move-out codegen (both only handle these top-level shapes).
    fn func_returns_heap_env_aggregate(
        &self,
        func: &Function,
        map: &HashMap<String, HashSet<String>>,
    ) -> Option<HashSet<String>> {
        let (_binds, owners) = self.collect_heap_env_binds_and_owners(func, map);
        if owners.is_empty() {
            return None;
        }
        let returned = |e: &Expr| match &e.kind {
            ExprKind::Identifier(n) => owners.get(n).cloned(),
            _ => None,
        };
        if let Some(fields) = func.body.final_expr.as_deref().and_then(&returned) {
            return Some(fields);
        }
        func.body.stmts.iter().find_map(|s| match &s.kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Return(Some(inner)) => returned(inner),
                _ => None,
            },
            _ => None,
        })
    }

    /// The heap-env element INDICES of `elems` — a tuple / array literal's element
    /// list — that hold a heap-env closure: a FRESH heap-env-producing call
    /// (`make(k)`) or a heap-env BINDING source (`f` in `binds`).
    fn heap_env_elem_indices(&self, elems: &[Expr], binds: &HashSet<String>) -> HashSet<usize> {
        elems
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                self.is_heap_env_producing_call(e)
                    || matches!(&e.kind, ExprKind::Identifier(n) if binds.contains(n))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The returned element-index set if `e` is a CALL to a fn in `map` (a
    /// container-returning fn). The tuple / array twin of `aggregate_call_owner_fields`.
    fn container_call_owner_elems(
        &self,
        e: &Expr,
        map: &HashMap<String, HashSet<usize>>,
    ) -> Option<HashSet<usize>> {
        let ExprKind::Call { callee, .. } = &e.kind else {
            return None;
        };
        let name = match &callee.kind {
            ExprKind::Identifier(n) => n,
            ExprKind::Path { segments, .. } if segments.len() == 1 => &segments[0],
            _ => return None,
        };
        map.get(name).cloned()
    }

    /// Collect, for `func`, the TUPLE and ARRAY owners → their heap-env element
    /// indices. An owner is bound from (a) a tuple / array LITERAL with a sanctioned
    /// heap-env store element (tuple/array-store slices) OR (b) a relay
    /// `let r = build(k)` where `build` returns a closure-owning tuple / array
    /// (container-escape — uses `fns_returning_heap_env_tuple` / `_array`). Shared by
    /// the misuse guard (pass 1) and the container-return fixpoint, keeping owner
    /// reasoning in one place (the tuple/array twin of
    /// `collect_heap_env_binds_and_owners`). `binds` must already be complete.
    fn collect_tuple_array_owners(
        &self,
        func: &Function,
        binds: &HashSet<String>,
    ) -> (
        HashMap<String, HashSet<usize>>,
        HashMap<String, HashSet<usize>>,
    ) {
        let mut tuple_owners: HashMap<String, HashSet<usize>> = HashMap::new();
        let mut array_owners: HashMap<String, HashSet<usize>> = HashMap::new();
        for stmt in &func.body.stmts {
            let StmtKind::Let { pattern, value, .. } = &stmt.kind else {
                continue;
            };
            let names = pattern.binding_names();
            let [b] = names.as_slice() else {
                continue;
            };
            let b = b.clone();
            match &value.kind {
                ExprKind::Tuple(elems) => {
                    let idxs = self.heap_env_elem_indices(elems, binds);
                    if !idxs.is_empty() {
                        tuple_owners.insert(b, idxs);
                    }
                }
                ExprKind::ArrayLiteral(elems) => {
                    let idxs = self.heap_env_elem_indices(elems, binds);
                    if !idxs.is_empty() {
                        array_owners.insert(b, idxs);
                    }
                }
                // Owner COPY (`let s = t`, `t` a tuple / array owner): forward
                // scan, so `s` adopts `t`'s owned element idxs and a copy-of-a-copy
                // (`let u = s`) chains. COPY semantics — `t` stays a live owner, and
                // codegen INCs the shared RC env per owned element so each owner
                // RC-drops once (the tuple/array twin of the struct owner-copy arm
                // in `collect_heap_env_binds_and_owners`). Sits before the
                // call-relay `_` arm (an Identifier is never a Call).
                ExprKind::Identifier(src) => {
                    if let Some(idxs) = tuple_owners.get(src).cloned() {
                        tuple_owners.insert(b, idxs);
                    } else if let Some(idxs) = array_owners.get(src).cloned() {
                        array_owners.insert(b, idxs);
                    }
                }
                _ => {
                    if let Some(idxs) =
                        self.container_call_owner_elems(value, &self.fns_returning_heap_env_tuple)
                    {
                        tuple_owners.insert(b, idxs);
                    } else if let Some(idxs) =
                        self.container_call_owner_elems(value, &self.fns_returning_heap_env_array)
                    {
                        array_owners.insert(b, idxs);
                    }
                }
            }
        }
        (tuple_owners, array_owners)
    }

    /// The owned element-index set if `func` returns — as a bare-identifier TAIL or
    /// top-level `return <bare identifier>;` — a local in `owners`. `None` otherwise.
    /// Branch-buried returns are intentionally NOT detected (the sound under-
    /// approximation that keeps detection in lockstep with the guard + move-out
    /// codegen). The tuple/array twin of `func_returns_heap_env_aggregate`.
    fn func_returns_container_owner(
        &self,
        func: &Function,
        owners: &HashMap<String, HashSet<usize>>,
    ) -> Option<HashSet<usize>> {
        if owners.is_empty() {
            return None;
        }
        let returned = |e: &Expr| match &e.kind {
            ExprKind::Identifier(n) => owners.get(n).cloned(),
            _ => None,
        };
        if let Some(idxs) = func.body.final_expr.as_deref().and_then(&returned) {
            return Some(idxs);
        }
        func.body.stmts.iter().find_map(|s| match &s.kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Return(Some(inner)) => returned(inner),
                _ => None,
            },
            _ => None,
        })
    }

    /// Populate `fns_returning_heap_env_tuple` / `_array` (functions that RETURN a
    /// tuple / array local owning one or more heap-env closure elements, as a bare
    /// tail / top-level `return t`). A FIXPOINT so a relay-of-container
    /// (`let r = build(k); r`) is recognized once its inner builder is. Runs after
    /// `compute_fns_returning_heap_env_aggregate` (an owner's binds come from a fresh
    /// heap-env call, and a tuple/array element can be a heap-env binding). The
    /// tuple/array twin of `compute_fns_returning_heap_env_aggregate`.
    pub(super) fn compute_fns_returning_heap_env_tuple_array(&mut self) {
        let funcs: Vec<Function> = self.fn_asts.values().cloned().collect();
        loop {
            let mut changed = false;
            for func in &funcs {
                let have_tuple = self.fns_returning_heap_env_tuple.contains_key(&func.name);
                let have_array = self.fns_returning_heap_env_array.contains_key(&func.name);
                if have_tuple && have_array {
                    continue;
                }
                // `binds` for this func (a tuple/array element may be a heap-env
                // binding); the aggregate map seeds owner reasoning shared with the
                // struct path.
                let (binds, _) = self.collect_heap_env_binds_and_owners(
                    func,
                    &self.fns_returning_heap_env_aggregate,
                );
                let (tuple_owners, array_owners) = self.collect_tuple_array_owners(func, &binds);
                if !have_tuple {
                    if let Some(idxs) = self.func_returns_container_owner(func, &tuple_owners) {
                        self.fns_returning_heap_env_tuple
                            .insert(func.name.clone(), idxs);
                        changed = true;
                    }
                }
                if !have_array {
                    if let Some(idxs) = self.func_returns_container_owner(func, &array_owners) {
                        self.fns_returning_heap_env_array
                            .insert(func.name.clone(), idxs);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// `true` when `e` is a CALL to a fn that returns a closure-owning `Vec[Fn]`
    /// (in `fns_returning_heap_env_vec`). The Vec twin of `container_call_owner_elems`
    /// (a Vec carries no per-element indices, so this returns a bool).
    fn call_returns_heap_env_vec(&self, e: &Expr) -> bool {
        let ExprKind::Call { callee, .. } = &e.kind else {
            return false;
        };
        let name = match &callee.kind {
            ExprKind::Identifier(n) => n,
            ExprKind::Path { segments, .. } if segments.len() == 1 => &segments[0],
            _ => return false,
        };
        self.fns_returning_heap_env_vec.contains(name)
    }

    /// Collect, for `func`, the `Vec[Fn]` OWNERS. An owner is (a) a fresh-ctor
    /// `let v: Vec[Fn] = Vec.new()`/`Vec.with_capacity(..)` binding that receives at
    /// least one heap-env push (Vec-store), OR (b) a relay `let r = build(k)` where
    /// `build` returns a closure-owning Vec (Vec-escape caller-adopt — uses
    /// `fns_returning_heap_env_vec`). Shared by the misuse guard (pass 1) and the
    /// Vec-return fixpoint, keeping owner reasoning in one place. `binds` must
    /// already be complete.
    fn collect_vec_owners(&self, func: &Function, binds: &HashSet<String>) -> HashSet<String> {
        let mut candidates: HashSet<String> = HashSet::new();
        for stmt in &func.body.stmts {
            if let StmtKind::Let {
                pattern,
                value,
                ty: Some(te),
                ..
            } = &stmt.kind
            {
                let is_fn_vec = super::helpers::vec_inner_type_expr(te)
                    .is_some_and(|inner| matches!(inner.kind, TypeKind::FnType { .. }));
                let fresh_vec =
                    self.is_vec_new_call(value) || self.is_vec_with_capacity_call(value);
                if is_fn_vec && fresh_vec {
                    if let [b] = pattern.binding_names().as_slice() {
                        candidates.insert(b.clone());
                    }
                }
            }
        }
        let mut owners: HashSet<String> = HashSet::new();
        if !candidates.is_empty() {
            self.collect_heap_env_vec_owners(&func.body, binds, &candidates, &mut owners);
        }
        // Vec-escape caller-adopt relay: `let r = build(k)` where `build` returns a
        // closure-owning Vec — `r` adopts the dynamic drop loop (no push needed).
        for stmt in &func.body.stmts {
            if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
                if self.call_returns_heap_env_vec(value) {
                    if let [b] = pattern.binding_names().as_slice() {
                        owners.insert(b.clone());
                    }
                }
            }
        }
        // Owner MOVE `let w = v` (`v` already a Vec owner): forward scan, so the
        // move-dest `w` becomes an owner and a chain `let w = v; let x = w`
        // propagates. Unlike the struct/tuple/array owner COPY (inc-on-copy),
        // `let w = v` for a Vec is a MOVE — codegen zeroes `v`'s cap, which the
        // `cap > 0` guard in the `FreeVecBuffer` cleanup uses to skip v's WHOLE
        // cleanup (the per-element env-drop loop AND the buffer free), while `w`
        // registers its own dynamic env-drop loop (no inc). Runs last so push-based
        // and relay owners are already in `owners` before a move references them.
        for stmt in &func.body.stmts {
            if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
                if let ExprKind::Identifier(src) = &value.kind {
                    if owners.contains(src) {
                        if let [b] = pattern.binding_names().as_slice() {
                            owners.insert(b.clone());
                        }
                    }
                }
            }
        }
        owners
    }

    /// `true` if `func` RETURNS a `Vec[Fn]` owner — a bare-identifier TAIL or
    /// top-level `return v;` of a Vec owner. The Vec twin of
    /// `func_returns_container_owner` (bool, no per-element indices).
    fn func_returns_vec_owner(&self, func: &Function, owners: &HashSet<String>) -> bool {
        if owners.is_empty() {
            return false;
        }
        let is_owner_id =
            |e: &Expr| matches!(&e.kind, ExprKind::Identifier(n) if owners.contains(n));
        if func.body.final_expr.as_deref().is_some_and(is_owner_id) {
            return true;
        }
        func.body.stmts.iter().any(|s| match &s.kind {
            StmtKind::Expr(e) => {
                matches!(&e.kind, ExprKind::Return(Some(inner)) if is_owner_id(inner))
            }
            _ => false,
        })
    }

    /// Populate `fns_returning_heap_env_vec` (functions that RETURN a `Vec[Fn]`
    /// owner, as a bare tail / `return v`). A FIXPOINT so a relay-of-Vec
    /// (`let r = build(k); r`) is recognized once its inner builder is. Runs after
    /// the tuple/array fixpoint (a relay can chain through any container builder).
    pub(super) fn compute_fns_returning_heap_env_vec(&mut self) {
        let funcs: Vec<Function> = self.fn_asts.values().cloned().collect();
        loop {
            let mut changed = false;
            for func in &funcs {
                if self.fns_returning_heap_env_vec.contains(&func.name) {
                    continue;
                }
                let (binds, _) = self.collect_heap_env_binds_and_owners(
                    func,
                    &self.fns_returning_heap_env_aggregate,
                );
                let owners = self.collect_vec_owners(func, &binds);
                if self.func_returns_vec_owner(func, &owners) {
                    self.fns_returning_heap_env_vec.insert(func.name.clone());
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// `true` when closure-literal `|params| body` captures at least one name in
    /// `outer` (a local/param of the enclosing function). Syntactic: the body's
    /// referenced names, minus the closure's own params and its inner `let`
    /// bindings, intersected with `outer`.
    fn closure_literal_captures(
        &self,
        params: &[ClosureParam],
        body: &Expr,
        outer: &HashSet<String>,
    ) -> bool {
        let param_names: HashSet<String> = params
            .iter()
            .flat_map(|p| p.pattern.binding_names())
            .collect();
        let mut refs = HashSet::new();
        let mut inner = HashSet::new();
        self.refs_in_expr(body, &mut refs, &mut inner);
        refs.iter()
            .any(|n| !param_names.contains(n) && !inner.contains(n) && outer.contains(n))
    }

    /// `true` when the tail expression `expr` (a function's return value)
    /// evaluates to a capturing closure — directly, through an identifier bound
    /// to one, or through the tail of a nested block / `if` / `match` /
    /// labeled block. Does NOT recurse into nested closure bodies (their tail
    /// is the inner closure's return, not this function's).
    fn tail_escapes_capturing_closure(
        &self,
        expr: &Expr,
        outer: &HashSet<String>,
        capturing_vars: &HashSet<String>,
        capturing_fields: &HashMap<String, HashSet<String>>,
    ) -> bool {
        match &expr.kind {
            ExprKind::Closure { params, body, .. } => {
                self.closure_literal_captures(params, body, outer)
            }
            ExprKind::Identifier(n) => capturing_vars.contains(n),
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::LabeledBlock { body: b, .. } => {
                b.final_expr.as_deref().is_some_and(|t| {
                    self.tail_escapes_capturing_closure(t, outer, capturing_vars, capturing_fields)
                })
            }
            ExprKind::If {
                then_block,
                else_branch,
                ..
            }
            | ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                let then_bad = then_block.final_expr.as_deref().is_some_and(|t| {
                    self.tail_escapes_capturing_closure(t, outer, capturing_vars, capturing_fields)
                });
                let else_bad = else_branch.as_deref().is_some_and(|e| {
                    self.tail_escapes_capturing_closure(e, outer, capturing_vars, capturing_fields)
                });
                then_bad || else_bad
            }
            ExprKind::Match { arms, .. } => arms.iter().any(|a| {
                self.tail_escapes_capturing_closure(
                    &a.body,
                    outer,
                    capturing_vars,
                    capturing_fields,
                )
            }),
            // An aggregate LITERAL that holds a capturing closure escapes it
            // (`return H { f: |x| x+k }`, `return (clo, 1)`, `return [clo]`,
            // `return Vec[clo]`, map / repeat literals, a struct `..spread`).
            // The local-then-return form (`let h = H { f: clo }; return h`) is
            // ALSO covered: the `capturing_vars` builder runs this same
            // predicate over each `let` RHS, so `h` is marked and the
            // `Identifier` arm fires on the returned `h`. Mirrors the ownership
            // pass's `collect_escape_target` (`closure_escape.rs`) literal set.
            ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => elems.iter().any(|e| {
                self.tail_escapes_capturing_closure(e, outer, capturing_vars, capturing_fields)
            }),
            ExprKind::PrefixCollectionLiteral { items, .. } => items.iter().any(|e| {
                self.tail_escapes_capturing_closure(e, outer, capturing_vars, capturing_fields)
            }),
            // Only `value` can hold a closure — `count` is a compile-time int.
            ExprKind::RepeatLiteral { value, .. } => {
                self.tail_escapes_capturing_closure(value, outer, capturing_vars, capturing_fields)
            }
            ExprKind::MapLiteral(pairs) => pairs.iter().any(|(k, v)| {
                self.tail_escapes_capturing_closure(k, outer, capturing_vars, capturing_fields)
                    || self.tail_escapes_capturing_closure(
                        v,
                        outer,
                        capturing_vars,
                        capturing_fields,
                    )
            }),
            ExprKind::StructLiteral { fields, spread, .. } => {
                fields.iter().any(|f| {
                    self.tail_escapes_capturing_closure(
                        &f.value,
                        outer,
                        capturing_vars,
                        capturing_fields,
                    )
                }) || spread.as_deref().is_some_and(|s| {
                    self.tail_escapes_capturing_closure(s, outer, capturing_vars, capturing_fields)
                })
            }
            // A field PROJECTION off a local struct binding whose initializer
            // stored a capturing closure in *that* field — `return h.f` /
            // `h.f` as the tail, after `let h = H { f: |x| x+k };`. The
            // closure's env lives on this frame's stack, so projecting it out
            // and returning it dangles exactly like returning the whole struct
            // (`return h`, already caught by the `Identifier` arm). Precise by
            // construction: `capturing_fields[base]` holds ONLY the fields whose
            // initializer was a capturing closure, so a sound `return
            // h.other_field` (a non-closure field, or a non-capturing closure
            // field) is left to compile. The base must be a plain local
            // identifier; a deeper projection (`a.b.f`) or a projection off a
            // by-value param is a narrower residual the escape-analysis slice
            // still owns — under-approximating here is sound (it never falsely
            // rejects), it just defers those shapes.
            ExprKind::FieldAccess { object, field } => {
                matches!(&object.kind, ExprKind::Identifier(base)
                    if capturing_fields
                        .get(base)
                        .is_some_and(|fs| fs.contains(field)))
            }
            _ => false,
        }
    }

    /// Collect every `return <value>` reachable from `block` WITHOUT entering a
    /// nested closure body (a `return` inside a closure returns from the
    /// closure, not the enclosing function). Best-effort over the common
    /// control-flow containers; a container this doesn't recurse into only
    /// UNDER-collects (a narrower residual) — it can never make the guard
    /// falsely reject a sound program.
    fn collect_outer_return_values<'a>(&self, block: &'a Block, out: &mut Vec<&'a Expr>) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Expr(e) | StmtKind::Let { value: e, .. } => {
                    self.collect_returns_in_expr(e, out)
                }
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    self.collect_returns_in_expr(value, out);
                    self.collect_outer_return_values(else_block, out);
                }
                StmtKind::Assign { value, .. } | StmtKind::CompoundAssign { value, .. } => {
                    self.collect_returns_in_expr(value, out)
                }
                StmtKind::MultiAssign { values, .. } => {
                    for v in values {
                        self.collect_returns_in_expr(v, out);
                    }
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.collect_outer_return_values(body, out)
                }
                StmtKind::LetUninit { .. } => {}
            }
        }
        if let Some(t) = &block.final_expr {
            self.collect_returns_in_expr(t, out);
        }
    }

    /// Recursive companion to [`collect_outer_return_values`] over an `Expr`.
    /// Stops at `Closure` bodies. Non-exhaustive on purpose (see that method).
    fn collect_returns_in_expr<'a>(&self, expr: &'a Expr, out: &mut Vec<&'a Expr>) {
        match &expr.kind {
            ExprKind::Return(Some(e)) => {
                out.push(e);
                self.collect_returns_in_expr(e, out);
            }
            ExprKind::Block(b)
            | ExprKind::Seq(b)
            | ExprKind::Comptime(b)
            | ExprKind::LabeledBlock { body: b, .. } => self.collect_outer_return_values(b, out),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            }
            | ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                self.collect_outer_return_values(then_block, out);
                if let Some(e) = else_branch {
                    self.collect_returns_in_expr(e, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    self.collect_returns_in_expr(&a.body, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. } => self.collect_outer_return_values(body, out),
            // Do NOT recurse into `Closure` — its `return` is the closure's.
            _ => {}
        }
    }

    /// Build the LLVM function type for the env-first closure-call ABI of a
    /// surface `Fn(P0, P1, …) -> R` annotation: `R (ptr env, P0, P1, …)`. The
    /// leading `ptr` is the captured-environment pointer every closure body
    /// (and every reified-fn trampoline) receives as its first parameter; a
    /// missing / `unit` return lowers to `void` (mirroring `declare_function`
    /// for a no-return fn, and matched by `compile_closure_call`'s void arm).
    ///
    /// Used both to register a `Fn`-typed parameter in `closure_fn_types` (so a
    /// body call `f(x)` becomes an indirect call) and to type the synthesized
    /// trampoline in `reify_named_fn_as_fn_value` — building both from the same
    /// annotation guarantees the indirect-call signature and the trampoline
    /// signature agree (B-2026-06-20-1).
    pub(super) fn closure_abi_fn_type(
        &self,
        params: &[TypeExpr],
        return_type: Option<&TypeExpr>,
    ) -> FunctionType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let mut param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr_ty.into()];
        for t in params {
            param_tys.push(BasicMetadataTypeEnum::from(self.llvm_type_for_type_expr(t)));
        }
        // A `unit` return (`Fn(..) -> ()`, or the typechecker's `Type::Unit`
        // round-tripped to `TypeKind::Unit`) is `void`, matching how a
        // no-return target lowers and how `compile_closure_call` treats a void
        // result — without this it would lower to `i64` and mismatch.
        if return_type.is_none() || matches!(return_type.map(|t| &t.kind), Some(TypeKind::Unit)) {
            return self.context.void_type().fn_type(&param_tys, false);
        }
        match return_type.map(|t| self.llvm_type_for_type_expr(t)) {
            Some(BasicTypeEnum::IntType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::FloatType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::PointerType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::StructType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::ArrayType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::VectorType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::ScalableVectorType(_)) | None => {
                self.context.void_type().fn_type(&param_tys, false)
            }
        }
    }

    /// B-2026-06-20-1: reify a bare named `fn` passed in `Fn(...)`-typed
    /// argument position into a closure fat-pointer value `{ trampoline, null
    /// env }`, so it dispatches through the same env-first indirect-call ABI as
    /// a closure literal. Returns `None` (caller compiles the arg normally)
    /// unless `arg` is a bare identifier that names a free fn AND the callee's
    /// parameter `idx` is `Fn(...)`-typed.
    ///
    /// A bare fn name otherwise lowers to a raw `ptr` (`@doubler`), which fails
    /// LLVM module verification against the fat-pointer parameter slot.
    pub(super) fn reify_named_fn_as_fn_value(
        &mut self,
        callee: &str,
        idx: usize,
        arg: &Expr,
    ) -> Option<BasicValueEnum<'ctx>> {
        let ExprKind::Identifier(fn_name) = &arg.kind else {
            return None;
        };
        // Gate: the callee's parameter `idx` must be `Fn(...)`-typed. (The
        // shared `reify_named_fn_value` separately rejects a name shadowed by a
        // higher-precedence binding.)
        let is_fn_param = self
            .fn_asts
            .get(callee)
            .and_then(|f| f.params.get(idx))
            .is_some_and(|p| matches!(p.ty.kind, TypeKind::FnType { .. }));
        if !is_fn_param {
            return None;
        }
        self.reify_named_fn_value(fn_name).map(|(fat, _)| fat)
    }

    /// B-2026-06-21-1: reify a bare identifier that names a free `fn` into a
    /// closure fat-pointer value `{ trampoline, null env }` plus the env-first
    /// `FunctionType` of that trampoline (for `closure_fn_types`). Shared by the
    /// `Fn`-typed argument-site reify above and the `let f = some_fn` binding
    /// path (`compile_stmt`), so a fn value works whether passed directly,
    /// bound to a local first, or called through that local.
    ///
    /// Returns `None` unless `name` resolves to a module function and is not
    /// shadowed by a higher-precedence binding — mirroring the resolution order
    /// of `compile_expr`'s `Identifier` arm (const-subst / local / module `let`
    /// / unit enum variant / top-level const all win over a free fn).
    pub(super) fn reify_named_fn_value(
        &mut self,
        name: &str,
    ) -> Option<(BasicValueEnum<'ctx>, FunctionType<'ctx>)> {
        if !self.name_resolves_to_free_fn(name) {
            return None;
        }
        let target = self.module.get_function(name)?;
        let tramp_name = format!("__karac_fnval_{}", name);
        let tramp = match self.module.get_function(&tramp_name) {
            Some(t) => t,
            None => self.emit_fn_value_trampoline(&tramp_name, target),
        };

        // Build the fat pointer `{ trampoline_ptr, null }` — null env because a
        // free fn captures nothing.
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fat_ty = self.closure_value_type();
        let mut fat = fat_ty.get_undef();
        fat = self
            .builder
            .build_insert_value(
                fat,
                tramp.as_global_value().as_pointer_value(),
                0,
                "fnval_fn",
            )
            .unwrap()
            .into_struct_value();
        fat = self
            .builder
            .build_insert_value(fat, ptr_ty.const_null(), 1, "fnval_env")
            .unwrap()
            .into_struct_value();
        Some((fat.into(), tramp.get_type()))
    }

    /// `true` when `name` would resolve to a free `fn` in `compile_expr`'s
    /// `Identifier` arm — i.e. it names a module function and is NOT shadowed by
    /// any higher-precedence binding (const-generic subst, local, module `let`,
    /// unit enum variant, or top-level `const`). Side-effect free: it inspects
    /// membership tables only, never `try_load_module_binding` /
    /// `try_unit_enum_variant` (those emit IR).
    fn name_resolves_to_free_fn(&self, name: &str) -> bool {
        if self.variables.contains_key(name)
            || self.const_subst.contains_key(name)
            || self.consts.contains_key(name)
            || self.module_bindings.contains_key(name)
        {
            return false;
        }
        // Unit enum variant (zero payload fields under some enum layout).
        let is_unit_variant = self.enum_layouts.values().any(|layout| {
            layout.tags.contains_key(name)
                && layout.field_counts.get(name).copied().unwrap_or(0) == 0
        });
        if is_unit_variant {
            return false;
        }
        self.module.get_function(name).is_some()
    }

    /// The env-first closure-call ABI `FunctionType` for `target`:
    /// `R (ptr env, P0, P1, …)` — `target`'s own signature with a leading env
    /// pointer prepended. This is the type of `target`'s reify trampoline and
    /// the `closure_fn_types` entry for any binding that holds `target` as a
    /// fn value.
    fn env_first_fn_type(&self, target: FunctionValue<'ctx>) -> FunctionType<'ctx> {
        let target_ty = target.get_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let mut param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr_ty.into()];
        // `FunctionType::get_param_types` already yields `BasicMetadataTypeEnum`.
        param_tys.extend(target_ty.get_param_types());
        match target_ty.get_return_type() {
            Some(BasicTypeEnum::IntType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::FloatType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::PointerType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::StructType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::ArrayType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::VectorType(t)) => t.fn_type(&param_tys, false),
            Some(BasicTypeEnum::ScalableVectorType(_)) | None => {
                self.context.void_type().fn_type(&param_tys, false)
            }
        }
    }

    /// If `let <name>[: ty] = value` binds a first-class fn value, return the
    /// env-first closure `FunctionType` to register in `closure_fn_types` (so a
    /// later `name(args)` lowers to an indirect call through the fat pointer).
    /// Recognizes, in precedence order: an explicit `Fn(...)` / `OnceFn(...)`
    /// annotation; a bare free-fn-name RHS; and a call whose callee's declared
    /// return type is `Fn(...)` (so `let f = pick()` where `pick -> Fn(..)`
    /// works un-annotated). Returns `None` when the binding is not a fn value,
    /// or when its signature can't be recovered at this layer — e.g. an
    /// un-annotated field/index read of a `Fn(..)` value, which still needs an
    /// explicit `let g: Fn(..) = h.f;` annotation (B-2026-06-21-2 residual).
    pub(super) fn let_binding_fn_value_type(
        &self,
        ty: Option<&TypeExpr>,
        value: &Expr,
    ) -> Option<FunctionType<'ctx>> {
        if let Some(TypeKind::FnType {
            params,
            return_type,
            ..
        }) = ty.map(|t| &t.kind)
        {
            return Some(self.closure_abi_fn_type(params, return_type.as_deref()));
        }
        if let ExprKind::Identifier(n) = &value.kind {
            if self.name_resolves_to_free_fn(n) {
                return self
                    .module
                    .get_function(n)
                    .map(|t| self.env_first_fn_type(t));
            }
        }
        if let ExprKind::Call { callee, .. } = &value.kind {
            if let ExprKind::Identifier(callee_name) = &callee.kind {
                if let Some(TypeKind::FnType {
                    params,
                    return_type,
                    ..
                }) = self.fn_return_type_exprs.get(callee_name).map(|t| &t.kind)
                {
                    return Some(self.closure_abi_fn_type(params, return_type.as_deref()));
                }
            }
        }
        // General fallback (B-2026-06-21-3): the typechecker typed the RHS
        // expression as a function — recover its `FnType` from the lowering
        // pass's `fn_value_typed_exprs` span table. Covers an un-annotated fn
        // value read from a struct field (`let g = h.f`), a `Vec[Fn]` element
        // (`let g = v[0]`), a method call, etc. — any inferred fn-value binding
        // whose RHS shape the cases above don't special-case.
        if let Some(TypeKind::FnType {
            params,
            return_type,
            ..
        }) = self
            .fn_value_typed_exprs
            .get(&(value.span.offset, value.span.length))
            .map(|t| &t.kind)
        {
            return Some(self.closure_abi_fn_type(params, return_type.as_deref()));
        }
        None
    }

    /// Synthesize a per-fn env-ignoring trampoline `__karac_fnval_<name>` whose
    /// signature is the env-first wrap of `target`'s own signature
    /// (`R (ptr env, P0, P1, …)`): it drops the leading env pointer and forwards
    /// the remaining args to `target`, returning its result. This lets a plain
    /// free fn (whose real signature has no env parameter) be invoked through
    /// the same indirect-call shape as a closure body. Deriving the signature
    /// from `target` (not from a `Fn(...)` annotation) keeps the one memoized
    /// `__karac_fnval_<name>` definition consistent across every reify site.
    /// Memoized by the caller via `module.get_function`.
    fn emit_fn_value_trampoline(
        &mut self,
        tramp_name: &str,
        target: FunctionValue<'ctx>,
    ) -> FunctionValue<'ctx> {
        let saved_bb = self.builder.get_insert_block();
        let tramp_ty = self.env_first_fn_type(target);
        let tramp = self.module.add_function(tramp_name, tramp_ty, None);
        let entry = self.context.append_basic_block(tramp, "entry");
        self.builder.position_at_end(entry);

        // Forward the user args (params 1..) to the target; param 0 (env) is
        // ignored — a free fn captures nothing.
        let fwd: Vec<BasicMetadataValueEnum<'ctx>> = tramp
            .get_params()
            .into_iter()
            .skip(1)
            .map(BasicMetadataValueEnum::from)
            .collect();
        let call = self.builder.build_call(target, &fwd, "fnval_fwd").unwrap();
        let ret_val = call.try_as_basic_value();
        if target.get_type().get_return_type().is_some() && !ret_val.is_instruction() {
            self.builder
                .build_return(Some(&ret_val.unwrap_basic()))
                .unwrap();
        } else {
            self.builder.build_return(None).unwrap();
        }

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        tramp
    }

    /// Compile `|params| body` into a fat-pointer value `{ fn_ptr, env_ptr }`.
    ///
    /// Sets `pending_closure_fn_type` so the surrounding `let` binding can register the
    /// function type for later indirect calls.
    ///
    /// `closure_span` is the `ExprKind::Closure` expression's own span — used
    /// as the lookup key into `Codegen::closure_capture_paths` (sourced from
    /// `OwnershipCheckResult::closure_capture_path_modes`). When the ownership
    /// pass supplied per-path mode data for this closure and every captured
    /// root resolves cleanly through `struct_field_names`, the env struct is
    /// laid out with one field per captured path (disjoint-capture slice 4);
    /// otherwise the legacy per-captured-name layout from
    /// `collect_closure_free_vars` is used.
    pub(super) fn compile_closure(
        &mut self,
        params: &[ClosureParam],
        body: &Expr,
        closure_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let id = self.closure_counter;
        self.closure_counter += 1;
        let fn_name = format!("__closure_{}", id);

        // 1. Collect free variables (names referenced in body, not in
        //    params, present in scope). Always run the per-name walker —
        //    it doubles as the fallback when no per-path layout is
        //    available, and the per-path layout consults it indirectly
        //    via `self.variables` for the root types.
        let free_vars = self.collect_closure_free_vars(params, body);

        // 1a. `mut ref` closure capture (B-2026-07-11-23): a stored closure
        //     VALUE whose body MUTATES a captured name captures that name BY
        //     REFERENCE (design.md Rule 2) — the write must land on the OUTER
        //     binding, not an env copy. Codegen captures such a name as a
        //     POINTER to its outer slot (env field is `ptr`, the body reads and
        //     writes through it), so mutations propagate to the real slot and
        //     the interpreter's shared-cell semantics are matched (`|x|{c=c+x}`
        //     over `f(3); f(4)` yields 7 on both engines).
        //
        //     Soundness: a by-reference capture is valid only while the outer
        //     slot outlives the closure — i.e. for a NON-escaping closure. An
        //     ESCAPING (returned → heap-env) closure would dangle, so it is
        //     still refused. `reject_escaping_capturing_closure`
        //     (`compile_function`) already rejects every OTHER escape route
        //     (return-of-a-non-tail, struct field, collection store, id chain),
        //     so a closure reaching here with `is_heap_env == false` provably
        //     does not escape its frame. The inlined `fold`/`any`/`all`/`sum`
        //     terminals never build a closure value, so they never reach here.
        let is_heap_env = self
            .current_fn_heap_closure_spans
            .contains(&(closure_span.offset, closure_span.length));
        let mutref_caps: HashSet<String> = {
            let mut assigned: HashSet<String> = HashSet::new();
            collect_assigned_roots_expr(body, &mut assigned);
            free_vars
                .iter()
                .filter(|n| assigned.contains(n.as_str()))
                .cloned()
                .collect()
        };
        if !mutref_caps.is_empty() && is_heap_env {
            let name = mutref_caps.iter().next().unwrap();
            return Err(format!(
                "a stored closure that BOTH mutates the captured variable `{name}` (`mut ref` \
                 capture, design.md Rule 2) AND escapes its defining function (returned) is not \
                 yet supported under `karac build`: the by-reference capture would outlive the \
                 frame that owns `{name}`. Re-run with `--interp` (or `KARAC_RUN_JIT=0`), or \
                 thread the mutated state through the closure's parameters / return value instead."
            ));
        }

        // 1b. Disjoint-capture slice 4: per-path env layout when the
        //     ownership pass supplied modes for this closure and every
        //     captured root resolves cleanly. Falls back to per-name
        //     layout when the data is missing (e.g., `compile_to_ir`
        //     called without ownership) or any captured root has a
        //     projection step that can't be resolved (treated as a
        //     whole-root capture for that root inside the path layout
        //     builder).
        //
        //     A `mut ref` capture (1a) forces the per-name layout: its env
        //     slot is a POINTER to the outer root (not a value / sub-field),
        //     which the per-path struct-field-precise layout does not model.
        let path_layout = if mutref_caps.is_empty() {
            self.build_capture_path_layout(closure_span, &free_vars)
        } else {
            None
        };

        // The original (value) type of each per-name capture, aligned with
        // `free_vars`. A `mut ref` capture stores a `ptr` to the outer slot in
        // the env, but the body still binds the var at its real type `T` (reads
        // / writes go through the pointer), so keep `T` here for the body's
        // `VarSlot.ty`. Only populated on the per-name path.
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let orig_cap_tys: Vec<BasicTypeEnum<'ctx>> = if path_layout.is_none() {
            free_vars.iter().map(|n| self.variables[n].ty).collect()
        } else {
            Vec::new()
        };

        // 2. Build the env struct type: { T0_cap, T1_cap, ... }.
        //    Use a dummy i8 when there are no captures so we always have
        //    a valid struct type. A `mut ref`-captured name's slot is a `ptr`
        //    to its outer binding (by-reference capture, 1a).
        let env_field_types: Vec<BasicTypeEnum<'ctx>> = if let Some(layout) = path_layout.as_ref() {
            if layout.slot_tys.is_empty() {
                vec![self.context.i8_type().into()]
            } else {
                layout.slot_tys.clone()
            }
        } else if free_vars.is_empty() {
            vec![self.context.i8_type().into()]
        } else {
            free_vars
                .iter()
                .zip(orig_cap_tys.iter())
                .map(|(n, &t)| {
                    if mutref_caps.contains(n) {
                        ptr_ty.into()
                    } else {
                        t
                    }
                })
                .collect()
        };
        let env_struct_ty = self.context.struct_type(&env_field_types, false);

        // Slice 1 (B-2026-06-22-2): does this closure escape its defining
        // function via the return? If so its environment must outlive the
        // frame, so it is allocated as a reference-counted HEAP box
        // `{ i64 refcount, <env_struct> }` instead of a stack alloca. The
        // closure body GEPs past the refcount to reach the payload; the
        // owning caller binding frees it (refcount dec) at scope exit.
        // (`is_heap_env` is computed in 1a — an escaping closure with a
        // `mut ref` capture was already refused there.)
        if is_heap_env {
            // Slice 1 supports POD captures only. A heap (String / Vec / shared)
            // capture would need its own drop when the env is freed (Slice 2);
            // until then, reject rather than leak it.
            let pod = env_field_types
                .iter()
                .all(|t| matches!(t, BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_)));
            if !pod {
                return Err(
                    "error[E_ESCAPING_CLOSURE_HEAP_CAPTURE_NOT_YET]: returning a closure \
                     that captures a heap value (String / Vec / shared) is not yet supported — \
                     only POD (integer / float / bool) captures can be returned today \
                     (heap-closure-environment epic B-2026-06-22-2, Slice 2). Workaround: pass \
                     the closure down by a `Fn(..)` parameter instead of returning it."
                        .to_string(),
                );
            }
        }
        let env_box_ty = self.context.struct_type(
            &[self.context.i64_type().into(), env_struct_ty.into()],
            false,
        );

        // 3. Determine param types. Source annotation wins, otherwise consult
        //    `pending_closure_param_hints` (caller pushdown — e.g. `Vec.sort_by`
        //    handing the element type to a `|a, b|` comparator), otherwise the
        //    typechecker's inferred `Fn(...)` type at the closure's own span
        //    (`fn_value_typed_exprs`, populated by the lowering pass from
        //    `expr_types` — contextual inference from the callee's declared
        //    `Fn` param covers the common un-annotated arg), otherwise fall
        //    back to i64. B-2026-07-02-12: before the span fallback, an
        //    un-annotated `|a| f"{a}!"` passed to a `Fn(String) -> String`
        //    param compiled as `(ptr, i64) -> i64` while the call site
        //    dispatched through the declared-`Fn` ABI — an indirect-call
        //    signature mismatch that silently printed the String's pointer
        //    word as an integer.
        let param_hints = self.pending_closure_param_hints.take();
        let inferred_fn_te = self
            .fn_value_typed_exprs
            .get(&(closure_span.offset, closure_span.length))
            .cloned();
        let inferred_param_tes: Vec<Option<TypeExpr>> = match inferred_fn_te.as_ref() {
            Some(TypeExpr {
                kind: TypeKind::FnType { params: ps, .. },
                ..
            }) => ps.iter().map(|te| Some(te.clone())).collect(),
            _ => Vec::new(),
        };
        let param_llvm_types: Vec<BasicTypeEnum<'ctx>> = params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if let Some(te) = p.ty.as_ref() {
                    return self.llvm_type_for_type_expr(te);
                }
                if let Some(hints) = param_hints.as_ref() {
                    if let Some(&hinted) = hints.get(i) {
                        return hinted;
                    }
                }
                if let Some(Some(te)) = inferred_param_tes.get(i) {
                    return self.llvm_type_for_type_expr(te);
                }
                self.context.i64_type().into()
            })
            .collect();

        // 4. Infer return type from the body expression.
        let closure_param_types: HashMap<String, BasicTypeEnum<'ctx>> = params
            .iter()
            .zip(param_llvm_types.iter())
            .filter_map(|(cp, ty)| {
                if let PatternKind::Binding(n) = &cp.pattern.kind {
                    Some((n.clone(), *ty))
                } else {
                    None
                }
            })
            .collect();
        let return_ty = self.infer_closure_return_type(body, &closure_param_types);

        // 5. Declare the closure function: fn(ptr env_ptr, T0, T1, ...) -> R.
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let mut fn_param_types: Vec<BasicMetadataTypeEnum<'ctx>> =
            vec![BasicMetadataTypeEnum::from(ptr_ty)];
        for &ty in &param_llvm_types {
            fn_param_types.push(BasicMetadataTypeEnum::from(ty));
        }
        let fn_type = match return_ty {
            BasicTypeEnum::IntType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::VectorType(t) => t.fn_type(&fn_param_types, false),
            BasicTypeEnum::ScalableVectorType(_) => {
                self.context.void_type().fn_type(&fn_param_types, false)
            }
        };
        let closure_fn = self.module.add_function(&fn_name, fn_type, None);

        // 6. Save outer codegen state — we're about to compile a new function inline.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_subst = std::mem::take(&mut self.type_subst);
        let saved_cfn = std::mem::take(&mut self.closure_fn_types);
        let saved_pct = self.pending_closure_fn_type.take();
        // Isolate the f-string accumulator staging slot. A closure body
        // may stage an `fstr.acc` alloca (e.g. an f-string moved into
        // `Result.Err(f"…")`); that alloca lives in the *closure* fn, so
        // if `last_fstr_acc` leaks back to the outer scope the outer
        // function's cleanup path emits GEPs into it and LLVM rejects them
        // ("Instruction does not dominate all uses"). The closure owns its
        // own staged f-string (moved into the returned value or drained by
        // its scope cleanup), so the outer slot is saved and restored intact.
        let saved_fstr_acc = self.last_fstr_acc.take();
        // Isolate the scope-cleanup frame (mirrors `emit_par_branch_fn`).
        // The closure body's cleanup actions — e.g. an f-string
        // accumulator's free — must be registered AND emitted inside the
        // closure fn, where their allocas dominate, and drained before the
        // closure returns. Without isolation they land in the OUTER
        // function's frame and the outer drain emits GEPs into closure-fn
        // allocas, which LLVM rejects ("Instruction does not dominate all
        // uses" on `fstr.acc`). The body push below gives `track_*`/cleanup
        // registration a frame of its own.
        let saved_cleanup = std::mem::take(&mut self.scope_cleanup_actions);
        self.scope_cleanup_actions.push(Vec::new());
        // Isolate the par-branch cancel pointer (B-2026-06-18-10). When the
        // enclosing scope is a `par {}` branch the auto-par pass produced,
        // `branch_cancel_ptr` points at THAT branch fn's `cancel_flag` arg. The
        // closure is a SEPARATE function, so a method call in its body that runs
        // `emit_branch_cancel_check` would load the cancel flag from an argument
        // of the wrong function ("referring to an argument in another
        // function"). Clear it for the body and restore after, exactly as the
        // par/reduce/task-group emitters do at their function boundaries.
        let saved_cancel_ptr = self.branch_cancel_ptr.take();

        // 7. Build the closure body.
        self.current_fn = Some(closure_fn);
        let entry = self.context.append_basic_block(closure_fn, "entry");
        self.builder.position_at_end(entry);

        // 7a. Load captured vars from the env struct (param 0 = env ptr).
        let mut env_ptr = closure_fn.get_nth_param(0).unwrap().into_pointer_value();
        // Slice 1: a heap-env closure receives the RC box `{ refcount, env }` as
        // its env pointer; GEP past the refcount (field 1) to the env payload so
        // the unpack below is identical to the stack-env case.
        if is_heap_env {
            env_ptr = self
                .builder
                .build_struct_gep(env_box_ty, env_ptr, 1, "__env_payload")
                .unwrap();
        }
        // Load the env struct value through the env pointer.
        let env_val = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(env_struct_ty.into(), env_ptr, "__env")
            .unwrap();

        if let Some(layout) = path_layout.as_ref() {
            // Per-path unpack: one env slot per captured CapturePath.
            // For whole-root entries the slot holds the root value as-is;
            // for path-precise entries we allocate a root-typed alloca
            // and stitch each leaf into its GEP chain, then register the
            // root alloca in `self.variables` so the body's `u.f` reads
            // walk it normally.
            let env_struct = env_val.into_struct_value();
            for (root_name, plan) in &layout.root_plans {
                if let Some(slot_idx) = plan.whole_root_slot {
                    let field_val = self
                        .builder
                        .build_extract_value(env_struct, slot_idx as u32, root_name)
                        .unwrap();
                    let alloca = self.create_entry_alloca(closure_fn, root_name, plan.root_ty);
                    self.builder.build_store(alloca, field_val).unwrap();
                    self.variables.insert(
                        root_name.clone(),
                        VarSlot {
                            ptr: alloca,
                            ty: plan.root_ty,
                        },
                    );
                } else {
                    // Stitch: allocate the root, write each captured leaf
                    // into its GEP chain. Other leaves stay undef — the
                    // ownership pass guarantees the body never reads them.
                    let alloca = self.create_entry_alloca(closure_fn, root_name, plan.root_ty);
                    for (slot_idx, gep_chain, leaf_ty) in &plan.sub_slots {
                        let leaf_val = self
                            .builder
                            .build_extract_value(
                                env_struct,
                                *slot_idx as u32,
                                &format!("{}.cap", root_name),
                            )
                            .unwrap();
                        let leaf_ptr = self.gep_root_chain(plan.root_ty, alloca, gep_chain);
                        self.builder.build_store(leaf_ptr, leaf_val).unwrap();
                        let _ = leaf_ty; // typed read at capture site; store inherits type from value.
                    }
                    self.variables.insert(
                        root_name.clone(),
                        VarSlot {
                            ptr: alloca,
                            ty: plan.root_ty,
                        },
                    );
                }
                if let Some(type_name) = &plan.type_name {
                    self.var_type_names
                        .insert(root_name.clone(), type_name.clone());
                }
            }
        } else if !free_vars.is_empty() {
            for (i, var_name) in free_vars.iter().enumerate() {
                let cap_ty = env_field_types[i];
                let field_val = self
                    .builder
                    .build_extract_value(env_val.into_struct_value(), i as u32, var_name)
                    .unwrap();
                if mutref_caps.contains(var_name) {
                    // By-reference (`mut ref`) capture: the env slot holds a
                    // POINTER to the outer binding. Register the var to read /
                    // write THROUGH it — no local copy — so body mutations land
                    // on the real outer slot (design.md Rule 2, B-2026-07-11-23).
                    self.variables.insert(
                        var_name.clone(),
                        VarSlot {
                            ptr: field_val.into_pointer_value(),
                            ty: orig_cap_tys[i],
                        },
                    );
                    if let Some(type_name) = saved_var_types.get(var_name) {
                        self.var_type_names
                            .insert(var_name.clone(), type_name.clone());
                    }
                    continue;
                }
                let alloca = self.create_entry_alloca(closure_fn, var_name, cap_ty);
                self.builder.build_store(alloca, field_val).unwrap();
                self.variables.insert(
                    var_name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: cap_ty,
                    },
                );
                // Propagate the outer scope's struct/enum type binding so
                // method dispatch inside the closure can route through the
                // user impl-block path.
                if let Some(type_name) = saved_var_types.get(var_name) {
                    self.var_type_names
                        .insert(var_name.clone(), type_name.clone());
                }
            }
        }

        // 7b. Bind closure params (fn params 1..n).
        for (i, (cp, ty)) in params.iter().zip(param_llvm_types.iter()).enumerate() {
            let param_val = closure_fn.get_nth_param((i + 1) as u32).unwrap();
            let param_name = match &cp.pattern.kind {
                PatternKind::Binding(n) => n.clone(),
                _ => format!("_cp{}", i),
            };
            let alloca = self.create_entry_alloca(closure_fn, &param_name, *ty);
            self.builder.build_store(alloca, param_val).unwrap();
            self.variables.insert(
                param_name.clone(),
                VarSlot {
                    ptr: alloca,
                    ty: *ty,
                },
            );
            // Register the param's Kāra struct/enum name under
            // `var_type_names` so `compile_field_access` inside the body
            // can resolve `param.field` reads. Without this, a closure
            // body like `|s: Score| s.v` silently lowers `s.v` to the
            // `i64 0` placeholder (`field_index_for` returns None →
            // generic field-access fall-through). The inline thunk in
            // vec_method::emit_sort_by_key_inline_thunk works around it
            // with its own var_type_names insertion at the param-bind
            // step; precompiled-closure callees (sort_by_key with a
            // closure-typed local) had no equivalent and silently produced
            // a no-op comparator. Pull the type name from the param's
            // declared type expr (single-segment Path catches the common
            // shapes: `Score`, `Item`, etc.; tuple / generic / etc. fall
            // through and the body's field access just uses the existing
            // body-path lookups).
            if let Some(te) = cp.ty.as_ref() {
                if let TypeKind::Path(p) = &te.kind {
                    if let Some(seg) = p.segments.last() {
                        self.record_var_type_name(param_name.clone(), seg.clone());
                    }
                }
            }
            // B-2026-07-02-12: register the collection / String side-tables
            // for the param from its effective type — the annotation when
            // present, else the typechecker's inferred `Fn(...)` param type
            // at the closure span (same source as the LLVM types above).
            // Without this an un-annotated String param was invisible to
            // `string_vars`, so an f-string interpolation in the body
            // formatted the `{ptr,len,cap}` value's first word as an i64.
            let effective_te = cp
                .ty
                .as_ref()
                .or_else(|| inferred_param_tes.get(i).and_then(Option::as_ref));
            if let Some(te) = effective_te {
                let te = te.clone();
                self.register_var_from_type_expr(&param_name, &te);
            }
        }

        // 7b½. Currying (B-2026-07-12-12): if this closure's tail is itself a
        // capturing closure literal, mark that inner span heap-env for the
        // body compile so the nested `compile_closure` gives it a per-call RC
        // heap box (each `make(n)` instance owns a distinct env — no aliasing).
        // Saved/restored around the body so sibling closures don't inherit it.
        let saved_heap_spans = self.current_fn_heap_closure_spans.clone();
        if let Some(inner_span) = self.closure_tail_heap_closure_span(params, body) {
            self.current_fn_heap_closure_spans.insert(inner_span);
        }

        // 7c. Compile body and build return.
        //
        // A BLOCK body is compiled like a function body — via the raw
        // `compile_block` (stmts + tail), NOT `compile_expr` (which routes
        // a block through `compile_block_with_frame`, opening a *nested*
        // scope whose cleanup runs INSIDE the body compilation). With the
        // nested scope, a returned heap binding `|| { let s = mk(); s }`
        // is freed by the block's scope-exit cleanup *before* the
        // tail-return suppression below can zero its `cap`, so the closure
        // hands back a dangling pointer (use-after-free / double-free).
        // Compiling the block raw makes its statements register their
        // cleanups in THIS closure's already-pushed frame, drained after
        // suppression. Non-block bodies are single expressions.
        let result = match &body.kind {
            ExprKind::Block(block) | ExprKind::Seq(block) => self
                .compile_block(block)?
                .unwrap_or_else(|| self.context.i64_type().const_int(0, false).into()),
            _ => self.compile_expr(body)?,
        };
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            // Move-aware drain (mirrors `compile_function`): suppress the
            // cleanup of whatever the closure RETURNS so the frame drain
            // below doesn't free the value handed back to the caller. For a
            // block body, suppress the tail binding's cleanup
            // (Vec/String/Map/user-`Drop`). For ANY returned f-string
            // (`|| f"…"`, or a block tail `f"…"`, or one moved into
            // `Result.Err(f"…")`), zero its accumulator `cap` so the
            // drained free is a runtime no-op.
            let returned_tail: Option<&Expr> = match &body.kind {
                ExprKind::Block(block) | ExprKind::Seq(block) => {
                    self.suppress_cleanup_for_tail_return(block);
                    block.final_expr.as_deref()
                }
                _ => Some(body),
            };
            if let Some(t) = returned_tail {
                self.suppress_fstr_acc_if_moved_out(t);
            }
            // Drain the closure's own cleanup frame before returning, so its
            // f-string / heap-local cleanups are emitted in THIS fn
            // (alloca-dominated) rather than leaking into the outer frame.
            self.emit_scope_cleanup();
            self.builder.build_return(Some(&result)).unwrap();
        }
        // Restore the heap-span set (7b½): the inner-closure marking is scoped
        // to this closure's body compile only.
        self.current_fn_heap_closure_spans = saved_heap_spans;

        // 8. Restore outer state.
        self.type_subst = saved_subst;
        self.loop_stack = saved_loop_stack;
        self.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_fn_types = saved_cfn;
        self.pending_closure_fn_type = saved_pct;
        self.last_fstr_acc = saved_fstr_acc;
        self.scope_cleanup_actions = saved_cleanup;
        self.branch_cancel_ptr = saved_cancel_ptr;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        // 9. In the outer context, allocate and populate the env struct.
        //    Non-escaping closure → cheap stack alloca (freed with the frame).
        //    Escaping (heap) closure → reference-counted heap box
        //    `{ refcount=1, env_struct }`; the env captures are stored into the
        //    box payload, the fat pointer carries the BOX pointer, and the
        //    owning caller binding frees it via `FreeClosureEnv` (Slice 1).
        let outer_fn = self.current_fn.unwrap();
        let (env_alloca, fat_env_ptr) = if is_heap_env {
            let box_ptr = self.emit_rc_alloc(env_box_ty);
            let payload = self
                .builder
                .build_struct_gep(env_box_ty, box_ptr, 1, "__env_box_payload")
                .unwrap();
            (payload, box_ptr)
        } else {
            let a = self.create_entry_alloca(outer_fn, "__closure_env", env_struct_ty.into());
            (a, a)
        };
        if let Some(layout) = path_layout.as_ref() {
            // Per-path capture: for each env slot, walk the source root's
            // GEP chain and store the leaf value into the slot.
            if !layout.slot_sources.is_empty() {
                let mut env_agg = env_struct_ty.get_undef();
                for (i, (root, gep_chain)) in layout.slot_sources.iter().enumerate() {
                    let slot = self.variables[root];
                    let val = if gep_chain.is_empty() {
                        // Whole-root: load the root binding directly.
                        self.builder.build_load(slot.ty, slot.ptr, root).unwrap()
                    } else {
                        // Path-precise: GEP into the root's alloca, load
                        // the leaf. `slot.ptr` is the alloca holding the
                        // root struct value (root captures gated to
                        // non-RC, non-ref-param roots in
                        // `build_capture_path_layout` so this is always
                        // a direct struct alloca).
                        let leaf_ptr = self.gep_root_chain(slot.ty, slot.ptr, gep_chain);
                        let leaf_ty = self.leaf_type_for_chain(slot.ty, gep_chain);
                        self.builder
                            .build_load(leaf_ty, leaf_ptr, &format!("{}.cap.read", root))
                            .unwrap()
                    };
                    env_agg = self
                        .builder
                        .build_insert_value(env_agg, val, i as u32, "__env_field")
                        .unwrap()
                        .into_struct_value();
                }
                self.builder.build_store(env_alloca, env_agg).unwrap();
            }
        } else if !free_vars.is_empty() {
            // Build the env struct by inserting each captured value. A
            // `mut ref`-captured name stores the ADDRESS of its outer slot (by-
            // reference capture, 1a) so the closure body writes through to the
            // real binding; every other name stores its loaded value.
            let mut env_agg = env_struct_ty.get_undef();
            for (i, var_name) in free_vars.iter().enumerate() {
                let slot = self.variables[var_name];
                let val: BasicValueEnum<'ctx> = if mutref_caps.contains(var_name) {
                    slot.ptr.into()
                } else {
                    self.builder
                        .build_load(slot.ty, slot.ptr, var_name)
                        .unwrap()
                };
                env_agg = self
                    .builder
                    .build_insert_value(env_agg, val, i as u32, "__env_field")
                    .unwrap()
                    .into_struct_value();
            }
            self.builder.build_store(env_alloca, env_agg).unwrap();
        }

        // 10. Build the fat-pointer closure struct: { fn_ptr, env_alloca }.
        let fn_ptr = closure_fn.as_global_value().as_pointer_value();
        let fat_ptr_ty = self.closure_value_type();
        let mut fat = fat_ptr_ty.get_undef();
        fat = self
            .builder
            .build_insert_value(fat, fn_ptr, 0, "closure_fn")
            .unwrap()
            .into_struct_value();
        fat = self
            .builder
            .build_insert_value(fat, fat_env_ptr, 1, "closure_env")
            .unwrap()
            .into_struct_value();

        // 11. Stage the LLVM function type for the surrounding let binding.
        self.pending_closure_fn_type = Some(fn_type);

        Ok(fat.into())
    }

    /// Execute an indirect call through a closure fat-pointer variable.
    pub(super) fn compile_closure_call(
        &mut self,
        name: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_type = match self.closure_fn_types.get(name).copied() {
            Some(t) => t,
            None => return Ok(self.context.i64_type().const_int(0, false).into()),
        };

        // Load the closure fat pointer value { fn_ptr, env_ptr }.
        let fat_val = self.load_variable(name)?;
        let fat_sv = fat_val.into_struct_value();
        let fn_ptr = self
            .builder
            .build_extract_value(fat_sv, 0, "closure_fn")
            .unwrap()
            .into_pointer_value();
        let env_ptr = self
            .builder
            .build_extract_value(fat_sv, 1, "closure_env")
            .unwrap()
            .into_pointer_value();

        // Build call args: env_ptr first, then user-supplied args.
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            vec![BasicMetadataValueEnum::from(env_ptr)];
        for arg in args {
            let val = self.compile_expr(&arg.value)?;
            call_args.push(BasicMetadataValueEnum::from(val));
        }

        let call = self
            .builder
            .build_indirect_call(fn_type, fn_ptr, &call_args, "closure_call")
            .unwrap();

        let basic_val = call.try_as_basic_value();
        if basic_val.is_instruction() {
            Ok(self.context.i64_type().const_int(0, false).into())
        } else {
            Ok(basic_val.unwrap_basic())
        }
    }

    /// Execute an indirect call through a closure fat-pointer VALUE produced by
    /// an arbitrary callee EXPRESSION rather than a named binding — a struct
    /// field `(h.f)(x)`, a Vec/array index `v[i](x)`, a tuple index `t.0(x)`, a
    /// parenthesized closure literal `(|x| x)(a)`, or a call result. The named-
    /// identifier closure case stays on the faster `closure_fn_types` /
    /// `load_variable` path in `compile_closure_call`; this generalization
    /// covers every other place expression that evaluates to a
    /// `{fn_ptr, env_ptr}` fat pointer (B-2026-06-22-4 — previously these fell
    /// through to a const-0 stub, a silent wrong-output miscompile under
    /// `karac build` while `karac run` was correct).
    ///
    /// The env-first ABI `FunctionType` is recovered from the callee
    /// expression's recorded `Fn(..)` type in `fn_value_typed_exprs` (the same
    /// lowering-pass span table `let_binding_fn_value_type` uses for an
    /// un-annotated `let g = h.f;`). Returns `Ok(None)` when the callee is not a
    /// function-typed expression, so the caller falls through to its existing
    /// unknown-callee const-0 fallback unchanged.
    pub(super) fn compile_closure_value_call(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Recover the callee's `Fn(..)` signature from the inferred-type span
        // table; bail out (the caller keeps its const-0 fallback) when the
        // callee isn't a function value.
        let fn_type = match self
            .fn_value_typed_exprs
            .get(&(callee.span.offset, callee.span.length))
            .map(|t| &t.kind)
        {
            Some(TypeKind::FnType {
                params,
                return_type,
                ..
            }) => self.closure_abi_fn_type(params, return_type.as_deref()),
            _ => return Ok(None),
        };

        // Evaluate the callee to its fat pointer { fn_ptr, env_ptr }.
        let fat_val = self.compile_expr(callee)?;
        let fat_sv = fat_val.into_struct_value();
        let fn_ptr = self
            .builder
            .build_extract_value(fat_sv, 0, "closure_fn")
            .unwrap()
            .into_pointer_value();
        let env_ptr = self
            .builder
            .build_extract_value(fat_sv, 1, "closure_env")
            .unwrap()
            .into_pointer_value();

        // Build call args: env_ptr first, then user-supplied args.
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            vec![BasicMetadataValueEnum::from(env_ptr)];
        for arg in args {
            let val = self.compile_expr(&arg.value)?;
            call_args.push(BasicMetadataValueEnum::from(val));
        }

        let call = self
            .builder
            .build_indirect_call(fn_type, fn_ptr, &call_args, "closure_call")
            .unwrap();

        let basic_val = call.try_as_basic_value();
        if basic_val.is_instruction() {
            Ok(Some(self.context.i64_type().const_int(0, false).into()))
        } else {
            Ok(Some(basic_val.unwrap_basic()))
        }
    }

    /// Lightweight return-type inference for closure bodies.
    /// Walks the expression shallowly to determine the LLVM type without building IR.
    pub(super) fn infer_closure_return_type(
        &self,
        expr: &Expr,
        param_types: &HashMap<String, BasicTypeEnum<'ctx>>,
    ) -> BasicTypeEnum<'ctx> {
        match &expr.kind {
            ExprKind::Integer(_, sfx) => self.llvm_int_type_for_suffix(*sfx).into(),
            ExprKind::Float(_, sfx) => self.llvm_float_type_for_suffix(*sfx).into(),
            ExprKind::Bool(_) => self.context.bool_type().into(),
            ExprKind::CharLit(_) => self.context.i32_type().into(),
            ExprKind::ByteLit(_) => self.context.i8_type().into(),
            ExprKind::StringLit(_) => self.context.ptr_type(AddressSpace::default()).into(),
            // An f-string evaluates to an owned `String` — the `{ptr, len,
            // cap}` heap-string aggregate (distinct from a borrowed string
            // *literal*, a bare `ptr` into rodata above).
            ExprKind::InterpolatedStringLit(_) => self.vec_struct_type().into(),
            ExprKind::Identifier(name) => {
                if let Some(&ty) = param_types.get(name) {
                    return ty;
                }
                if let Some(slot) = self.variables.get(name.as_str()) {
                    return slot.ty;
                }
                self.context.i64_type().into()
            }
            ExprKind::Binary { op, left, right } => match op {
                BinOp::Eq
                | BinOp::NotEq
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq
                | BinOp::And
                | BinOp::Or => self.context.bool_type().into(),
                _ => {
                    let lt = self.infer_closure_return_type(left, param_types);
                    let rt = self.infer_closure_return_type(right, param_types);
                    if lt.is_float_type() || rt.is_float_type() {
                        self.context.f64_type().into()
                    } else {
                        lt
                    }
                }
            },
            ExprKind::Unary { operand, .. } => self.infer_closure_return_type(operand, param_types),
            ExprKind::MethodCall { method, .. } if method == "cmp" => self
                .enum_layouts
                .get("Ordering")
                .map(|l| BasicTypeEnum::StructType(l.llvm_type))
                .unwrap_or_else(|| {
                    self.context
                        .struct_type(&[self.context.i64_type().into()], false)
                        .into()
                }),
            ExprKind::Cast { ty, .. } => self.llvm_type_for_type_expr(ty),
            ExprKind::Block(block) | ExprKind::Seq(block) => {
                if let Some(final_expr) = &block.final_expr {
                    // A bare-`Identifier` tail naming a body-local `let` is
                    // not in `param_types`/`self.variables` at inference
                    // time (the body hasn't been compiled), so resolve it
                    // against the block's own `let` bindings: prefer the
                    // let's type annotation, else infer from its value.
                    if let ExprKind::Identifier(tail) = &final_expr.kind {
                        if param_types.get(tail).is_none()
                            && !self.variables.contains_key(tail.as_str())
                        {
                            for stmt in &block.stmts {
                                if let StmtKind::Let {
                                    pattern, ty, value, ..
                                } = &stmt.kind
                                {
                                    if matches!(&pattern.kind, PatternKind::Binding(n) if n == tail)
                                    {
                                        return match ty {
                                            Some(te) => self.llvm_type_for_type_expr(te),
                                            None => {
                                                self.infer_closure_return_type(value, param_types)
                                            }
                                        };
                                    }
                                }
                            }
                        }
                    }
                    self.infer_closure_return_type(final_expr, param_types)
                } else {
                    self.context.i64_type().into()
                }
            }
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                if let Some(else_expr) = else_branch {
                    self.infer_closure_return_type(else_expr, param_types)
                } else if let Some(final_expr) = &then_block.final_expr {
                    self.infer_closure_return_type(final_expr, param_types)
                } else {
                    self.context.i64_type().into()
                }
            }
            ExprKind::Tuple(elems) => {
                let field_types: Vec<BasicTypeEnum<'ctx>> = elems
                    .iter()
                    .map(|e| self.infer_closure_return_type(e, param_types))
                    .collect();
                self.context.struct_type(&field_types, false).into()
            }
            // Calls: look up in module or use i64 fallback.
            ExprKind::Call { callee, args } => {
                if let ExprKind::Identifier(fname) = &callee.kind {
                    if let Some(f) = self.module.get_function(fname) {
                        return f
                            .get_type()
                            .get_return_type()
                            .unwrap_or_else(|| self.context.i64_type().into());
                    }
                }
                // Lowered operator dispatch: `<Primitive>.<op>(args)` —
                // the lowering pass produces these from BinOp/UnaryOp.
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2 {
                        let target = segments[0].as_str();
                        let method = segments[1].as_str();
                        // Enum-variant construction: `Enum.Variant(args)`
                        // (Result.Ok/Err, Option.Some, user enums) returns
                        // the enum's type-erased LLVM layout, NOT the
                        // payload type. Guard on the variant actually
                        // existing (`tags`) so a static method like
                        // `Enum.from(..)` doesn't get mis-typed. Without
                        // this the closure fn's declared return type
                        // collapses to the i64 fallback below while the
                        // compiled body produces the full enum struct —
                        // LLVM verifier: "return type does not match
                        // operand type of return inst".
                        if let Some(layout) = self.enum_layouts.get(target) {
                            if layout.tags.contains_key(method) {
                                return BasicTypeEnum::StructType(layout.llvm_type);
                            }
                        }
                        // Eq/Ord methods return bool regardless of operand type.
                        if matches!(method, "eq" | "ne" | "lt" | "le" | "gt" | "ge") {
                            return self.context.bool_type().into();
                        }
                        // Arithmetic, bitwise, shifts, not — return Self.
                        let is_self_returning = matches!(
                            method,
                            "add"
                                | "sub"
                                | "mul"
                                | "div"
                                | "rem"
                                | "neg"
                                | "bitand"
                                | "bitor"
                                | "bitxor"
                                | "shl"
                                | "shr"
                                | "not"
                        );
                        if is_self_returning {
                            return match target {
                                "f32" => self.context.f32_type().into(),
                                "f64" => self.context.f64_type().into(),
                                "bool" => self.context.bool_type().into(),
                                _ => {
                                    // Fall back to inferring from operand if available.
                                    if let Some(arg) = args.first() {
                                        return self
                                            .infer_closure_return_type(&arg.value, param_types);
                                    }
                                    self.context.i64_type().into()
                                }
                            };
                        }
                    }
                }
                self.context.i64_type().into()
            }
            // Bare enum-variant path expression: a unit variant used as a
            // value, e.g. `Option.None` / `Result`-less user unit variants
            // (no call args). Same type-erased-layout rule as the
            // `Enum.Variant(args)` call form above.
            ExprKind::Path { segments, .. } if segments.len() == 2 => {
                if let Some(layout) = self.enum_layouts.get(segments[0].as_str()) {
                    if layout.tags.contains_key(segments[1].as_str()) {
                        return BasicTypeEnum::StructType(layout.llvm_type);
                    }
                }
                self.context.i64_type().into()
            }
            // A closure body that is ITSELF a closure (currying:
            // `|n| |x| x + n`) evaluates to a closure fat pointer
            // `{ fn_ptr, env_ptr }`, not the `i64` default. Without this the
            // outer closure fn is declared `-> i64` while its body returns the
            // fat-pointer struct → LLVM verifier "return type does not match
            // operand type of return inst" (B-2026-07-12-12). The escaping
            // inner env is heap-allocated per outer call (see the tail-heap-
            // closure marking in `compile_closure`) so distinct instances
            // (`make(5)` / `make(10)`) don't alias one reused stack env.
            ExprKind::Closure { .. } => self.closure_value_type().into(),
            _ => self.context.i64_type().into(),
        }
    }

    // ── Disjoint-capture slice 4 helpers ───────────────────────────

    /// Build a per-path env layout for the closure at `closure_span`.
    /// Returns `None` when the ownership pass did not supply path-mode
    /// data for this closure (caller falls back to per-name layout).
    /// Roots that aren't safe for path-precise stitching (RC-fallback
    /// promoted, `ref`-param-shaped, or any projection step the resolver
    /// can't walk through `struct_field_names`) are collapsed to a single
    /// whole-root slot for that root — other roots in the same layout
    /// still get path-precise slots.
    fn build_capture_path_layout(
        &self,
        closure_span: &Span,
        free_vars: &[String],
    ) -> Option<CapturePathLayout<'ctx>> {
        let key = SpanKey::from_span(closure_span);
        let path_modes = self.closure_capture_paths.get(&key)?;

        // Group paths by root, preserving the slice-2 list order so
        // multiple paths under the same root keep deterministic ordering.
        let mut roots_in_order: Vec<String> = Vec::new();
        let mut by_root: HashMap<String, Vec<&CapturePath>> = HashMap::new();
        for (path, _mode) in path_modes {
            if !self.variables.contains_key(path.root.as_str()) {
                // Path references a binding the codegen scope doesn't
                // know about (e.g. captured by a nested closure but
                // shadowed before reaching this point) — skip; the
                // legacy per-name walker mirrors the same filter.
                continue;
            }
            if !by_root.contains_key(&path.root) {
                roots_in_order.push(path.root.clone());
            }
            by_root.entry(path.root.clone()).or_default().push(path);
        }
        // The slice-2 path set is keyed off the closure's free-variable
        // scan, which records roots even when the body only reaches them
        // through stopping constructs. Cross-check with `free_vars` so
        // any root the per-name walker found but slice 2 missed (and
        // vice-versa) doesn't silently drop from the env — fall back to
        // per-name layout if the two sets disagree.
        let path_root_set: HashSet<&String> = by_root.keys().collect();
        let free_var_set: HashSet<&String> = free_vars.iter().collect();
        if path_root_set != free_var_set {
            return None;
        }

        let mut slot_tys: Vec<BasicTypeEnum<'ctx>> = Vec::new();
        let mut slot_sources: Vec<(String, Vec<u32>)> = Vec::new();
        let mut root_plans: Vec<(String, RootUnpackPlan<'ctx>)> = Vec::new();

        for root in roots_in_order {
            let slot = *self.variables.get(root.as_str())?;
            let type_name = self.var_type_names.get(root.as_str()).cloned();
            let paths = by_root.get(&root).unwrap();

            // Conservative force-whole-root triggers: RC-fallback root
            // (slot.ty is `ptr`, body field-access goes through the
            // heap-deref path), ref-param root (alloca holds a pointer,
            // not a struct value), or any path under this root has a
            // projection chain that can't be resolved through
            // `struct_field_names`.
            let force_whole_root = self.is_rc_fallback_binding(&root)
                || self.ref_params.contains_key(root.as_str())
                || paths.iter().any(|p| {
                    !p.projection.is_empty()
                        && self
                            .resolve_gep_chain(slot.ty, type_name.as_deref(), &p.projection)
                            .is_none()
                });

            let any_whole = paths.iter().any(|p| p.projection.is_empty());

            if force_whole_root || any_whole {
                // One whole-root slot for this root. Drop sub-paths —
                // the body walks the whole root and field reads work
                // through normal compile_field_access dispatch.
                let slot_idx = slot_tys.len();
                slot_tys.push(slot.ty);
                slot_sources.push((root.clone(), Vec::new()));
                root_plans.push((
                    root.clone(),
                    RootUnpackPlan {
                        root_ty: slot.ty,
                        type_name,
                        whole_root_slot: Some(slot_idx),
                        sub_slots: Vec::new(),
                    },
                ));
            } else {
                // Per-path: one slot per non-empty projection. The slice-2
                // set guarantees every path here has non-empty projection
                // (`any_whole` is false in this branch).
                let mut sub_slots: Vec<(usize, Vec<u32>, BasicTypeEnum<'ctx>)> = Vec::new();
                for p in paths {
                    let gep_chain = self
                        .resolve_gep_chain(slot.ty, type_name.as_deref(), &p.projection)
                        .unwrap();
                    let leaf_ty = self.leaf_type_for_chain(slot.ty, &gep_chain);
                    let slot_idx = slot_tys.len();
                    slot_tys.push(leaf_ty);
                    slot_sources.push((root.clone(), gep_chain.clone()));
                    sub_slots.push((slot_idx, gep_chain, leaf_ty));
                }
                root_plans.push((
                    root.clone(),
                    RootUnpackPlan {
                        root_ty: slot.ty,
                        type_name,
                        whole_root_slot: None,
                        sub_slots,
                    },
                ));
            }
        }

        Some(CapturePathLayout {
            slot_tys,
            slot_sources,
            root_plans,
        })
    }

    /// Walk a projection chain (root-to-leaf field names, possibly mixed
    /// with numeric tuple indices) into a sequence of LLVM struct GEP
    /// indices. Returns `None` if any step can't be resolved — the
    /// caller treats that root as a whole-root capture. `type_name` is
    /// the source-level type of the root, looked up in
    /// `struct_field_names` to translate field-name → index.
    fn resolve_gep_chain(
        &self,
        root_ty: BasicTypeEnum<'ctx>,
        type_name: Option<&str>,
        projection: &[String],
    ) -> Option<Vec<u32>> {
        let mut current_ty = root_ty;
        let mut current_type_name: Option<String> = type_name.map(|s| s.to_string());
        let mut chain: Vec<u32> = Vec::with_capacity(projection.len());
        for step in projection {
            let struct_ty = match current_ty {
                BasicTypeEnum::StructType(st) => st,
                _ => return None,
            };
            // Try struct-field-name → index lookup first.
            let idx = if let Some(name) = current_type_name.as_deref() {
                if let Some(names) = self.struct_field_names.get(name) {
                    names.iter().position(|f| f == step).map(|p| p as u32)
                } else {
                    None
                }
            } else {
                None
            };
            // Fall back to numeric tuple-index parse.
            let idx = idx.or_else(|| step.parse::<u32>().ok())?;
            // Advance the LLVM and source type-name pointers.
            current_ty = struct_ty.get_field_type_at_index(idx)?;
            current_type_name = current_type_name
                .as_deref()
                .and_then(|name| self.struct_field_type_names.get(name))
                .and_then(|tys| tys.get(idx as usize).cloned())
                .flatten();
            chain.push(idx);
        }
        Some(chain)
    }

    /// Resolve the LLVM type at the end of a GEP chain rooted at
    /// `root_ty`. Used by both the capture-site loader (to type the load
    /// from the source root) and the unpack-site stitcher (to type the
    /// store into the stitched root).
    fn leaf_type_for_chain(
        &self,
        root_ty: BasicTypeEnum<'ctx>,
        chain: &[u32],
    ) -> BasicTypeEnum<'ctx> {
        let mut current = root_ty;
        for &idx in chain {
            if let BasicTypeEnum::StructType(st) = current {
                current = st.get_field_type_at_index(idx).unwrap();
            } else {
                // Builder guarantees the chain is resolvable; this branch
                // is only reached if a non-struct sneaks in, which would
                // be a bug — return the i64 fallback rather than panic.
                return self.context.i64_type().into();
            }
        }
        current
    }

    /// GEP into a struct alloca via a chain of field indices. Used by
    /// both the capture site (to read a leaf from the outer-scope root)
    /// and the unpack site (to write a leaf into the stitched-back
    /// root). The chain is rooted at field index 0 conceptually — every
    /// `struct_gep` step walks down one level from the current pointer.
    fn gep_root_chain(
        &self,
        root_ty: BasicTypeEnum<'ctx>,
        root_ptr: inkwell::values::PointerValue<'ctx>,
        chain: &[u32],
    ) -> inkwell::values::PointerValue<'ctx> {
        let mut current_ptr = root_ptr;
        let mut current_ty = root_ty;
        for (i, &idx) in chain.iter().enumerate() {
            let struct_ty = match current_ty {
                BasicTypeEnum::StructType(st) => st,
                _ => return current_ptr,
            };
            current_ptr = self
                .builder
                .build_struct_gep(struct_ty, current_ptr, idx, &format!("cap.gep.{}", i))
                .unwrap();
            current_ty = struct_ty.get_field_type_at_index(idx).unwrap();
        }
        current_ptr
    }

    /// Collect the names of variables captured by a closure (free variables from outer scope).
    ///
    /// A variable is captured if:
    /// 1. It is referenced in `body`.
    /// 2. It is NOT one of the closure's own parameters.
    /// 3. It is NOT defined by a `let` inside the closure body.
    /// 4. It IS present in the current outer scope (`self.variables`).
    pub(super) fn collect_closure_free_vars(
        &self,
        params: &[ClosureParam],
        body: &Expr,
    ) -> Vec<String> {
        let param_names: HashSet<String> = params
            .iter()
            .flat_map(|p| p.pattern.binding_names())
            .collect();

        let mut refs = HashSet::new();
        let mut inner_defs = HashSet::new();
        self.refs_in_expr(body, &mut refs, &mut inner_defs);

        let mut free: Vec<String> = refs
            .into_iter()
            .filter(|n| !param_names.contains(n) && !inner_defs.contains(n))
            .filter(|n| self.variables.contains_key(n.as_str()))
            .collect();
        free.sort(); // deterministic order
        free
    }

    /// Walk `expr` and collect all identifier references into `refs`,
    /// and all names bound by `let` statements into `defs`.
    pub(super) fn refs_in_expr(
        &self,
        expr: &Expr,
        refs: &mut HashSet<String>,
        defs: &mut HashSet<String>,
    ) {
        match &expr.kind {
            ExprKind::Identifier(n) => {
                refs.insert(n.clone());
            }
            // `self` inside an impl-method body parses as `SelfValue`,
            // not `Identifier("self")`. Without this arm, an auto-par
            // branch fn whose stmts read `self.X` would not include
            // `self` in its capture set, the env-struct unpack would
            // not bind `self` in the branch fn's `self.variables`, and
            // `load_variable("self")` would error with "Undefined
            // variable 'self'" when the branch body's field access
            // tries to resolve the receiver.
            ExprKind::SelfValue => {
                refs.insert("self".to_string());
            }
            ExprKind::Binary { left, right, .. } => {
                self.refs_in_expr(left, refs, defs);
                self.refs_in_expr(right, refs, defs);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.refs_in_expr(operand, refs, defs)
            }
            // `a | b` (pipe) and `a ?? b` (nil-coalesce) read both sides —
            // without these, a piped/coalesced read of a captured local
            // would be missed (same class as the `Unsafe` gap below).
            ExprKind::Pipe { left, right } | ExprKind::NilCoalesce { left, right } => {
                self.refs_in_expr(left, refs, defs);
                self.refs_in_expr(right, refs, defs);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.refs_in_expr(object, refs, defs);
                if let Some(args) = args {
                    for a in args {
                        self.refs_in_expr(&a.value, refs, defs);
                    }
                }
            }
            ExprKind::Call { callee, args } => {
                self.refs_in_expr(callee, refs, defs);
                for a in args {
                    self.refs_in_expr(&a.value, refs, defs);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.refs_in_expr(object, refs, defs);
                for a in args {
                    self.refs_in_expr(&a.value, refs, defs);
                }
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.refs_in_expr(condition, refs, defs);
                self.refs_in_block(then_block, refs, defs);
                if let Some(e) = else_branch {
                    self.refs_in_expr(e, refs, defs);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.refs_in_expr(condition, refs, defs);
                self.refs_in_block(body, refs, defs);
            }
            ExprKind::Loop { body, .. } => self.refs_in_block(body, refs, defs),
            // Block-bearing expression forms — every one can hold a read of
            // a captured outer local. `Unsafe` is the one that bit the
            // FFI-handle pattern (`unsafe { free(m) }` in an auto-par branch
            // left `m` out of the capture set → "Undefined variable 'm'"),
            // but `Try` / `Par` / `Lock` are the same latent gap. Mirrors
            // the concurrency analyzer's `collect_expr_reads`, which already
            // recurses into all of these — keeping the capture-set collector
            // and the dependency analyzer in agreement.
            ExprKind::Block(block)
            | ExprKind::Seq(block)
            | ExprKind::Unsafe(block)
            | ExprKind::Try(block)
            | ExprKind::Par(block) => {
                self.refs_in_block(block, refs, defs);
            }
            ExprKind::Lock { body, .. } => self.refs_in_block(body, refs, defs),
            ExprKind::Return(Some(e)) => self.refs_in_expr(e, refs, defs),
            ExprKind::Return(None) => {}
            ExprKind::Break { value: Some(e), .. } => self.refs_in_expr(e, refs, defs),
            ExprKind::Break { value: None, .. } => {}
            ExprKind::FieldAccess { object, .. } => self.refs_in_expr(object, refs, defs),
            ExprKind::TupleIndex { object, .. } => self.refs_in_expr(object, refs, defs),
            ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
                for e in elems {
                    self.refs_in_expr(e, refs, defs);
                }
            }
            ExprKind::StructLiteral { fields, .. } => {
                for f in fields {
                    self.refs_in_expr(&f.value, refs, defs);
                }
            }
            ExprKind::Cast { expr: inner, .. } => self.refs_in_expr(inner, refs, defs),
            ExprKind::Match { scrutinee, arms } => {
                self.refs_in_expr(scrutinee, refs, defs);
                for arm in arms {
                    for name in arm.pattern.binding_names() {
                        defs.insert(name);
                    }
                    self.refs_in_expr(&arm.body, refs, defs);
                }
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.refs_in_expr(iterable, refs, defs);
                for name in pattern.binding_names() {
                    defs.insert(name);
                }
                self.refs_in_block(body, refs, defs);
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.refs_in_expr(value, refs, defs);
                for name in pattern.binding_names() {
                    defs.insert(name);
                }
                self.refs_in_block(then_block, refs, defs);
                if let Some(e) = else_branch {
                    self.refs_in_expr(e, refs, defs);
                }
            }
            ExprKind::Closure { params, body, .. } => {
                // Nested closure: params shadow outer names; body refs are handled recursively
                // but we only care about what escapes into the outer scope.
                let inner_params: HashSet<String> = params
                    .iter()
                    .flat_map(|p| p.pattern.binding_names())
                    .collect();
                let mut inner_refs = HashSet::new();
                let mut inner_inner_defs = HashSet::new();
                self.refs_in_expr(body, &mut inner_refs, &mut inner_inner_defs);
                for r in inner_refs {
                    if !inner_params.contains(&r) && !inner_inner_defs.contains(&r) {
                        refs.insert(r);
                    }
                }
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.refs_in_expr(s, refs, defs);
                }
                if let Some(e) = end {
                    self.refs_in_expr(e, refs, defs);
                }
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let ParsedInterpolationPart::Expr(inner) = part {
                        self.refs_in_expr(inner, refs, defs);
                    }
                }
            }
            // `a[i]` indexes: walk both the indexed object and the
            // index expr. Without this, an auto-par branch fn whose
            // stmts read `nums[j]` would miss `nums` in its capture
            // set — the env-struct unpack would never bind `nums` in
            // the branch's `self.variables`, and `compile_slice_index`
            // (or `compile_vec_index` / `compile_map_index`) would
            // panic at the `get_data_ptr(name).unwrap()` site when
            // the slice/vec/map registries still report the type
            // (registered in the parent) but the variables table
            // doesn't have the alloca.
            ExprKind::Index { object, index } => {
                self.refs_in_expr(object, refs, defs);
                self.refs_in_expr(index, refs, defs);
            }
            _ => {}
        }
    }

    pub(super) fn refs_in_block(
        &self,
        block: &Block,
        refs: &mut HashSet<String>,
        defs: &mut HashSet<String>,
    ) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, value, .. } | StmtKind::LetElse { pattern, value, .. } => {
                    self.refs_in_expr(value, refs, defs);
                    for name in pattern.binding_names() {
                        defs.insert(name);
                    }
                }
                StmtKind::Expr(e) => self.refs_in_expr(e, refs, defs),
                StmtKind::Assign { target, value } => {
                    self.refs_in_expr(target, refs, defs);
                    self.refs_in_expr(value, refs, defs);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.refs_in_expr(target, refs, defs);
                    self.refs_in_expr(value, refs, defs);
                }
                _ => {}
            }
        }
        if let Some(e) = &block.final_expr {
            self.refs_in_expr(e, refs, defs);
        }
    }
}
