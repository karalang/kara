// src/closure_escape.rs
//! Closure escape analysis — the single predicate behind codegen's
//! `E_ESCAPING_CLOSURE_NOT_YET` gate AND the `escaping_closure` check-time
//! lint (B-2026-08-16-13).
//!
//! ## Why this is a crate-level plain-AST module and not part of codegen
//!
//! The heap-closure-environment epic (B-2026-06-22-2) guards every shape it
//! does not yet lower with two validators (`reject_escaping_capturing_closure`,
//! `reject_heap_env_misuse`) fed by four fixpoint passes over the program's
//! function ASTs. B-2026-08-16-13 measured that a hand-mirrored check-time
//! copy of that boundary CANNOT be kept in sync: the epic has widened the
//! supported set four times already, and each landed slice would turn a
//! mirrored lint arm into a Deny-level false positive on a program that
//! builds — with nothing in CI to catch the stale mirror (a full mirrored
//! lint was built, validated against a 394-file sweep, and deliberately
//! discarded — see the row). So the analysis lives HERE, reads only plain
//! ASTs and name-sets, and is consumed by BOTH:
//!
//! * codegen (`--features llvm`): `Codegen.closure_state.escape` holds an
//!   [`EscapeAnalysis`]; `compile` builds the producer sets once via
//!   [`EscapeAnalysis::compute`], `compile_function` runs
//!   [`EscapeAnalysis::check_function`] per function (validators + this
//!   function's owner tables), and emission reads the sets/owner tables to
//!   wire `FreeClosureEnv` drops exactly as before.
//! * `karac check` (feature-independent): the `escaping_closure` lint runs
//!   the same `compute` + `check_function` over the same function set and
//!   surfaces the violations as diagnostics.
//!
//! One predicate, zero drift: a future epic slice that widens the supported
//! set edits this module once, and both the build gate and the check
//! diagnostic move together. Keep it that way — never re-grow a copy of any
//! of these walks inside codegen or a lint.
//!
//! Codegen-containment note: this module must stay free of `inkwell` and any
//! LLVM-typed state (that is what lets a non-llvm `karac` run the lint). The
//! emission halves of closure lowering (env-struct layout, trampolines,
//! `closure_abi_fn_type`) stay in `src/codegen/closures.rs`.

use crate::ast::*;
use crate::token::Span;
use std::collections::{HashMap, HashSet};

/// A rejected escape shape: the codegen-identical message plus the best
/// span the analysis has for it. Validator 1 pins the offending return
/// expression; validator 2's misuse walk is boolean (it does not thread the
/// offending site out), so its violations carry the whole function's span —
/// making that walk site-precise is a recorded residual, not an oversight.
pub struct EscapeViolation {
    pub message: String,
    pub span: Span,
}

impl EscapeViolation {
    fn at(span: Span, message: &str) -> Self {
        EscapeViolation {
            message: message.to_string(),
            span,
        }
    }
}

/// The escape analysis: an owned snapshot of the program's non-generic free
/// function ASTs (`fn_asts`, keyed by name — the same map codegen's
/// declaration pass builds), the five program-wide producer sets the four
/// fixpoints compute, and the per-function owner tables the misuse guard
/// rebuilds on every [`Self::check_function`] call.
///
/// Field semantics are documented where codegen consumes them
/// (`src/codegen/closure_state.rs` kept the prose for years); the short
/// version: `fns_returning_heap_env*` name functions whose return value IS or
/// OWNS a reference-counted heap closure environment (directly / in a struct /
/// tuple / array / `Vec[Fn]`), and `heap_env_*_owners` name the CURRENT
/// function's locals that own such envs (so emission can register
/// `FreeClosureEnv` drops and the guard can sanction owner-scoped uses).
#[derive(Default)]
pub struct EscapeAnalysis {
    fn_asts: HashMap<String, Function>,
    pub fns_returning_heap_env: HashSet<String>,
    pub fns_returning_heap_env_aggregate: HashMap<String, HashSet<String>>,
    pub fns_returning_heap_env_tuple: HashMap<String, HashSet<usize>>,
    pub fns_returning_heap_env_array: HashMap<String, HashSet<usize>>,
    pub fns_returning_heap_env_vec: HashSet<String>,
    pub curry_closure_vars: HashSet<String>,
    pub heap_env_aggregate_owners: HashMap<String, HashSet<String>>,
    pub heap_env_tuple_owners: HashMap<String, HashSet<usize>>,
    pub heap_env_array_owners: HashMap<String, HashSet<usize>>,
    pub heap_env_vec_owners: HashSet<String>,
}

impl EscapeAnalysis {
    /// Build the analysis for a program: snapshot `fn_asts` and run the four
    /// producer-set fixpoints in their required order (aggregate reads the
    /// base set; tuple/array read both; vec reads all of the above).
    pub fn compute(fn_asts: &HashMap<String, Function>) -> Self {
        let mut a = EscapeAnalysis {
            fn_asts: fn_asts.clone(),
            ..Default::default()
        };
        a.compute_fns_returning_heap_env();
        a.compute_fns_returning_heap_env_aggregate();
        a.compute_fns_returning_heap_env_tuple_array();
        a.compute_fns_returning_heap_env_vec();
        a
    }

    /// Run both validators for one function, rebuilding its owner tables
    /// (`curry_closure_vars`, `heap_env_{aggregate,tuple,array,vec}_owners`)
    /// as a side effect — codegen's emission reads them for `FreeClosureEnv`
    /// wiring after this returns `Ok`. Mirrors the pre-extraction order in
    /// `compile_function`: validator 1, then the per-function owner resets +
    /// curry scan, then the misuse guard (which assigns the aggregate owner
    /// map itself and reads the other tables its collectors fill in).
    pub fn check_function(&mut self, func: &Function) -> Result<(), EscapeViolation> {
        self.reject_escaping_capturing_closure(func)?;
        self.heap_env_tuple_owners.clear();
        self.heap_env_array_owners.clear();
        self.heap_env_vec_owners.clear();
        self.curry_closure_vars = self.compute_curry_closure_vars(func);
        self.reject_heap_env_misuse(func)
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
    fn reject_escaping_capturing_closure(&self, func: &Function) -> Result<(), EscapeViolation> {
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
        if let Some(offender) = return_values.iter().find(|e| {
            self.tail_escapes_capturing_closure(e, &outer, &capturing_vars, &capturing_fields)
        }) {
            return Err(EscapeViolation::at(
                offender.span,
                "error[E_ESCAPING_CLOSURE_NOT_YET]: returning a closure that captures a \
                 local variable is not yet supported — the closure's captured environment lives \
                 on the returning function's stack frame, which is freed when it returns (it \
                 would read garbage). Tracked as the heap-closure-environment epic \
                 (B-2026-06-22-2). Workaround: return a non-capturing closure or a named `fn`, or \
                 pass the closure down by a `Fn(..)` parameter instead of returning it.",
            ));
        }
        Ok(())
    }

    /// `true` when `e` is a call to a free function that RETURNS a heap-env
    /// closure (`make(..)` with `make` ∈ `fns_returning_heap_env`). Such a call
    /// mints a reference-counted heap environment that an owner must free; only
    /// a `let f = <call>` binding is wired to free it (a `FreeClosureEnv`
    /// cleanup), so any other occurrence of the call leaks or escapes the env.
    pub fn is_heap_env_producing_call(&self, e: &Expr) -> bool {
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
    fn reject_heap_env_misuse(&mut self, func: &Function) -> Result<(), EscapeViolation> {
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
            // B-2026-08-16-13 (message half): keep the two lists in step with
            // the epic's landed slices. "Passing it as a call argument" and
            // "capturing it in a nested closure" sat in the refusal list after
            // both had become supported — while the workaround sentence
            // recommended the Fn-param hand-off the refusal list forbade. An
            // author steering by the message avoided a working shape; each
            // future slice should move its phrase from one list to the other.
            return Err(EscapeViolation::at(
                func.span,
                "error[E_ESCAPING_CLOSURE_NOT_YET]: a returned capturing closure can currently \
                 be CALLED in the function that binds it (`let f = make(..); f(x)`), COPIED to \
                 another binding (`let g = f`), RETURNED as a bare tail / top-level `return f`, \
                 passed down by a `Fn(..)` parameter (`use_it(f)`), captured by a nested \
                 closure, or OWNED by a `let`-bound struct/tuple/array literal or `Vec[Fn]` \
                 push and called through the owner's field or index. Storing it into a struct \
                 literal anywhere but a `let` RHS, passing the owner on, copying the closure \
                 back out of an owner (`let g = r.f;`), returning it from inside a branch, or \
                 leaving a `make(..)` result unbound (any non-`let` use of the call — \
                 `make(..);`, `make(..)(x)`, `use_it(make(..))`) is not yet supported — the \
                 reference-counted closure environment would outlive or be double-freed by its \
                 owner set (heap-closure-environment epic B-2026-06-22-2). Workaround: bind the \
                 closure with a `let` first, or store the closure's data and dispatch with a \
                 plain fn.",
            ));
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
                .any(|p| matches!(p, ParsedInterpolationPart::Expr(inner, _) if mis(inner))),
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
            | ExprKind::ByteStringLit(..)
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
                .any(|p| matches!(p, ParsedInterpolationPart::Expr(inner, _) if esc(inner))),
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
            | ExprKind::ByteStringLit(..)
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
    pub fn func_tail_heap_closure_span(&self, func: &Function) -> Option<(usize, usize)> {
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
    pub fn closure_tail_heap_closure_span(
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
    fn compute_curry_closure_vars(&self, func: &Function) -> HashSet<String> {
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
    fn compute_fns_returning_heap_env(&mut self) {
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
    fn compute_fns_returning_heap_env_aggregate(&mut self) {
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
    fn compute_fns_returning_heap_env_tuple_array(&mut self) {
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
                let is_fn_vec = vec_inner_type_expr(te)
                    .is_some_and(|inner| matches!(inner.kind, TypeKind::FnType { .. }));
                let fresh_vec = is_vec_new_call(value) || is_vec_with_capacity_call(value);
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
    fn compute_fns_returning_heap_env_vec(&mut self) {
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
        refs_in_expr(body, &mut refs, &mut inner);
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
}

/// Walk `expr` and collect all identifier references into `refs`,
/// and all names bound by `let` statements into `defs`.
pub fn refs_in_expr(expr: &Expr, refs: &mut HashSet<String>, defs: &mut HashSet<String>) {
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
            refs_in_expr(left, refs, defs);
            refs_in_expr(right, refs, defs);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
            refs_in_expr(operand, refs, defs)
        }
        // `a | b` (pipe) and `a ?? b` (nil-coalesce) read both sides —
        // without these, a piped/coalesced read of a captured local
        // would be missed (same class as the `Unsafe` gap below).
        ExprKind::Pipe { left, right } | ExprKind::NilCoalesce { left, right } => {
            refs_in_expr(left, refs, defs);
            refs_in_expr(right, refs, defs);
        }
        ExprKind::OptionalChain { object, args, .. } => {
            refs_in_expr(object, refs, defs);
            if let Some(args) = args {
                for a in args {
                    refs_in_expr(&a.value, refs, defs);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            refs_in_expr(callee, refs, defs);
            for a in args {
                refs_in_expr(&a.value, refs, defs);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            refs_in_expr(object, refs, defs);
            for a in args {
                refs_in_expr(&a.value, refs, defs);
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            refs_in_expr(condition, refs, defs);
            refs_in_block(then_block, refs, defs);
            if let Some(e) = else_branch {
                refs_in_expr(e, refs, defs);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            refs_in_expr(condition, refs, defs);
            refs_in_block(body, refs, defs);
        }
        ExprKind::Loop { body, .. } => refs_in_block(body, refs, defs),
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
            refs_in_block(block, refs, defs);
        }
        ExprKind::Lock { body, .. } => refs_in_block(body, refs, defs),
        ExprKind::Return(Some(e)) => refs_in_expr(e, refs, defs),
        ExprKind::Return(None) => {}
        ExprKind::Break { value: Some(e), .. } => refs_in_expr(e, refs, defs),
        ExprKind::Break { value: None, .. } => {}
        ExprKind::FieldAccess { object, .. } => refs_in_expr(object, refs, defs),
        ExprKind::TupleIndex { object, .. } => refs_in_expr(object, refs, defs),
        ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
            for e in elems {
                refs_in_expr(e, refs, defs);
            }
        }
        // The PREFIX collection forms — `Vec[a, b]`, `Set[..]`, `Map[..]`,
        // `[v; n]` — and the map literal. A BARE `[a, b]` checked against a
        // `Vec`-typed annotation is normalized to
        // `PrefixCollectionLiteral{type_name:"Vec"}` before codegen sees it,
        // so the `ArrayLiteral` arm above does NOT cover the common
        // `let c: Vec[Vec[i64]] = [a, b]` (B-2026-08-16-1): the elements
        // went unseen, `a`/`b` never entered the outside-reads set, the
        // auto-par group gave them no return slot, and the parent body died
        // on "Undefined variable 'a'" — a DEFAULT-build failure on a program
        // that compiles with `KARAC_AUTO_PAR=0`.
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items {
                refs_in_expr(e, refs, defs);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            refs_in_expr(value, refs, defs);
            refs_in_expr(count, refs, defs);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                refs_in_expr(k, refs, defs);
                refs_in_expr(v, refs, defs);
            }
        }
        // `spread` as well as `fields`: `Point { x: 1, ..base }` READS
        // `base`, and dropping it would be the same under-approximation as
        // the literal arms above. Unreachable TODAY — the parser accepts
        // spread but the typechecker rejects the form ("missing field 'y'")
        // and design.md does not spec it — so this is the sibling arms'
        // stated policy applied ahead of time: a variant that is merely
        // unreachable is one language widening away from the same bug, and
        // the walker costs nothing to keep correct.
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                refs_in_expr(&f.value, refs, defs);
            }
            if let Some(s) = spread {
                refs_in_expr(s, refs, defs);
            }
        }
        ExprKind::Cast { expr: inner, .. } => refs_in_expr(inner, refs, defs),
        ExprKind::Match { scrutinee, arms } => {
            refs_in_expr(scrutinee, refs, defs);
            for arm in arms {
                for name in arm.pattern.binding_names() {
                    defs.insert(name);
                }
                refs_in_expr(&arm.body, refs, defs);
            }
        }
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            refs_in_expr(iterable, refs, defs);
            for name in pattern.binding_names() {
                defs.insert(name);
            }
            refs_in_block(body, refs, defs);
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            refs_in_expr(value, refs, defs);
            for name in pattern.binding_names() {
                defs.insert(name);
            }
            refs_in_block(then_block, refs, defs);
            if let Some(e) = else_branch {
                refs_in_expr(e, refs, defs);
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
            refs_in_expr(body, &mut inner_refs, &mut inner_inner_defs);
            for r in inner_refs {
                if !inner_params.contains(&r) && !inner_inner_defs.contains(&r) {
                    refs.insert(r);
                }
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                refs_in_expr(s, refs, defs);
            }
            if let Some(e) = end {
                refs_in_expr(e, refs, defs);
            }
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts {
                if let ParsedInterpolationPart::Expr(inner, _) = part {
                    refs_in_expr(inner, refs, defs);
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
            refs_in_expr(object, refs, defs);
            refs_in_expr(index, refs, defs);
        }
        // B-2026-08-08-16 — block-carrying forms that fell through the
        // catch-all below, so NEITHER their scrutinee/bindings NOR their
        // body contributed to the capture set.
        //
        // `while let` is the one that was reachable and it cost a build:
        // an auto-par branch containing `while let H.Full(v) = next(i) {
        // … i = i + 1; }` never captured `i`, so `emit_par_branch_fn`
        // compiled the outlined statement and `next(i)` failed with
        // `Undefined variable 'i'` — `karac build` refusing a program
        // `KARAC_NO_AUTOPAR=1` compiles and runs. The `if let` arm directly
        // above has always been here; the `while let` twin simply was not,
        // and every OTHER walker in this file handles both.
        //
        // The rest are added together because the failure mode is a MISSED
        // CAPTURE — a hard "Undefined variable" at the outlining site, not
        // a conservative miss — so a variant that is merely unreachable
        // today is one par-analysis widening away from the same bug. Each
        // mirrors the shape of its sibling arm above.
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            refs_in_expr(value, refs, defs);
            for name in pattern.binding_names() {
                defs.insert(name);
            }
            refs_in_block(body, refs, defs);
        }
        ExprKind::LabeledBlock { body, .. } | ExprKind::Comptime(body) => {
            refs_in_block(body, refs, defs)
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                refs_in_expr(&b.value, refs, defs);
            }
            refs_in_block(body, refs, defs);
        }
        // LEAF variants — enumerated rather than swallowed by a `_ => {}`,
        // because a wildcard here fails OPEN. This walker's output is the
        // set of names read outside a par group, and a name it misses gets
        // no return slot: the binding is lifted into a branch fn and left
        // undefined in the parent ("Undefined variable 'x'"). Over-
        // approximating is free — slots are `defined ∩ refs`, so a name that
        // no group statement binds is simply filtered out — while under-
        // approximating is a build failure on valid code. The asymmetry is
        // entirely one-directional, so the match is exhaustive on purpose:
        // a new `ExprKind` breaks THIS LINE at compile time instead of
        // silently reading as "mentions nothing" (the same fail-closed
        // discipline `bce_length_pin::block_all` documents).
        //
        // `Path`'s `generic_args` can hold a `GenericArg::Const(Expr)`, but
        // a const generic argument is a compile-time constant and cannot
        // name a runtime local, so there is nothing here for a par group to
        // return.
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..) | ExprKind::ByteStringLit(..)
        | ExprKind::StringLit(..)
        | ExprKind::MultiStringLit(..)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(..)
        | ExprKind::Path { .. }
        | ExprKind::SelfType
        | ExprKind::OffsetOf { .. }
        | ExprKind::Continue { .. }
        // `PipePlaceholder` is the `_` standing in for the piped value —
        // the value itself is the `Pipe` arm's `left`, already walked
        // there. `Error` is the parser's recovery placeholder and never
        // reaches a compiled body.
        | ExprKind::PipePlaceholder
        | ExprKind::Error => {}
    }
}

pub fn refs_in_block(block: &Block, refs: &mut HashSet<String>, defs: &mut HashSet<String>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } | StmtKind::LetElse { pattern, value, .. } => {
                refs_in_expr(value, refs, defs);
                for name in pattern.binding_names() {
                    defs.insert(name);
                }
            }
            StmtKind::Expr(e) => refs_in_expr(e, refs, defs),
            StmtKind::Assign { target, value } => {
                refs_in_expr(target, refs, defs);
                refs_in_expr(value, refs, defs);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                refs_in_expr(target, refs, defs);
                refs_in_expr(value, refs, defs);
            }
            _ => {}
        }
    }
    if let Some(e) = &block.final_expr {
        refs_in_expr(e, refs, defs);
    }
}

/// Recognise `Vec.new()` — a 2-segment associated call whose callee path is
/// `["Vec", "new"]`. Shared with codegen's `types_lowering` (which delegates
/// here) so the escape analysis and the SoA/let lowering agree on what "a
/// fresh Vec binding" is.
pub fn is_vec_new_call(expr: &Expr) -> bool {
    if let ExprKind::Call { callee, .. } = &expr.kind {
        if let ExprKind::Path { segments, .. } = &callee.kind {
            return segments.len() == 2 && segments[0] == "Vec" && segments[1] == "new";
        }
    }
    false
}

/// Recognise `Vec.with_capacity(n)` — the capacity-presized form of
/// `Vec.new()` (the `presize` pass rewrites counted-loop fills into this, so
/// any recognizer of "a fresh Vec binding" must accept both spellings).
pub fn is_vec_with_capacity_call(expr: &Expr) -> bool {
    if let ExprKind::Call { callee, .. } = &expr.kind {
        if let ExprKind::Path { segments, .. } = &callee.kind {
            return segments.len() == 2 && segments[0] == "Vec" && segments[1] == "with_capacity";
        }
    }
    false
}

/// Pull the element `TypeExpr` out of `Vec[T]` / `VecDeque[T]` (the codegen
/// alias — VecDeque rides on Vec's struct shape). Shared with codegen's
/// `helpers` (which delegates here) so the escape analysis and the let/SoA
/// lowering agree on what "a Vec element type" is.
pub fn vec_inner_type_expr(te: &TypeExpr) -> Option<TypeExpr> {
    if let TypeKind::Path(path) = &te.kind {
        let name = path.segments.first().map(|s| s.as_str());
        if name == Some("Vec") || name == Some("VecDeque") {
            if let Some(args) = &path.generic_args {
                if let Some(GenericArg::Type(elem)) = args.first() {
                    return Some(elem.clone());
                }
            }
        }
    }
    None
}

/// Rewrite every bare `Self` path in `te` to the concrete impl-target type
/// `type_name`, recursing through the compound type forms. Applied to a
/// synthesized impl-method's `-> Self` return type so its prototype's LLVM
/// return type matches the concrete aggregate the body actually returns —
/// otherwise `llvm_return_type` hits the unknown-name `i64` fall-through for
/// `Self` and the module verifier rejects the mismatched `ret` (e.g.
/// `ret { i64 } %field` against an `i64` fn type). Mirrors the typechecker's
/// `resolve_self_in_type`.
pub fn rewrite_self_in_type_expr(te: &TypeExpr, type_name: &str) -> TypeExpr {
    let kind = match &te.kind {
        TypeKind::Path(p) => {
            if p.segments.len() == 1 && p.segments[0] == "Self" && p.generic_args.is_none() {
                TypeKind::Path(PathExpr {
                    segments: vec![type_name.to_string()],
                    generic_args: None,
                    span: p.span,
                })
            } else {
                TypeKind::Path(PathExpr {
                    segments: p.segments.clone(),
                    generic_args: p.generic_args.as_ref().map(|args| {
                        args.iter()
                            .map(|a| match a {
                                GenericArg::Type(t) => {
                                    GenericArg::Type(rewrite_self_in_type_expr(t, type_name))
                                }
                                other => other.clone(),
                            })
                            .collect()
                    }),
                    span: p.span,
                })
            }
        }
        TypeKind::Tuple(elems) => TypeKind::Tuple(
            elems
                .iter()
                .map(|e| rewrite_self_in_type_expr(e, type_name))
                .collect(),
        ),
        TypeKind::Array { element, size } => TypeKind::Array {
            element: Box::new(rewrite_self_in_type_expr(element, type_name)),
            size: size.clone(),
        },
        TypeKind::Pointer { is_mut, inner } => TypeKind::Pointer {
            is_mut: *is_mut,
            inner: Box::new(rewrite_self_in_type_expr(inner, type_name)),
        },
        TypeKind::Ref(inner) => {
            TypeKind::Ref(Box::new(rewrite_self_in_type_expr(inner, type_name)))
        }
        TypeKind::MutRef(inner) => {
            TypeKind::MutRef(Box::new(rewrite_self_in_type_expr(inner, type_name)))
        }
        TypeKind::MutSlice(inner) => {
            TypeKind::MutSlice(Box::new(rewrite_self_in_type_expr(inner, type_name)))
        }
        TypeKind::Weak(inner) => {
            TypeKind::Weak(Box::new(rewrite_self_in_type_expr(inner, type_name)))
        }
        TypeKind::FnType {
            params,
            return_type,
            effect_spec,
            is_once,
        } => TypeKind::FnType {
            params: params
                .iter()
                .map(|p| rewrite_self_in_type_expr(p, type_name))
                .collect(),
            return_type: return_type
                .as_ref()
                .map(|r| Box::new(rewrite_self_in_type_expr(r, type_name))),
            effect_spec: effect_spec.clone(),
            is_once: *is_once,
        },
        _ => te.kind.clone(),
    };
    TypeExpr {
        kind,
        span: te.span,
    }
}

/// Synthesize the plain `Function` codegen compiles for a non-generic impl
/// method: qualify the name to `Type.method`, rewrite `-> Self` to the
/// concrete target, and prepend a `self` parameter whose type mirrors the
/// source self mode (`self` → `Type`, `ref self` → `ref Type`, `mut ref
/// self` → `mut ref Type`). Lives here (not in codegen's `helpers`) because
/// the `escaping_closure` check lint must run the escape validators over the
/// SAME synthesized shape the compile loop validates — same synthesis, no
/// drift (B-2026-08-16-13).
pub fn make_impl_method_function(
    type_name: &str,
    method: &Function,
    target_type: &TypeExpr,
) -> Function {
    let mut f = method.clone();
    f.name = format!("{}.{}", type_name, method.name);
    // Resolve `Self` in the return type to the concrete target so the
    // prototype's LLVM return type matches the body's actual return value.
    if let Some(rt) = f.return_type.as_ref() {
        f.return_type = Some(rewrite_self_in_type_expr(rt, type_name));
    }
    if let Some(self_kind) = method.self_param.as_ref() {
        let span = method.span;
        // A CONCRETE BUILTIN CONTAINER target keeps its element args on
        // `self`, so the container-typed-param registration path populates
        // the per-name side-tables codegen dispatches from —
        // `column_var_infos`/`tensor_var_infos["self"]` with the concrete
        // element kind (otherwise `self.sum()` has no element and can't pick
        // the concrete kernel), and `slice_elem_types`/`map_key_types`
        // likewise (otherwise `self[0]` compiles the receiver to a bare
        // aggregate and dies on "Index operator applied to non-array type" —
        // B-2026-08-17-44). Struct/enum targets keep the bare-name `self`
        // they relied on; element inference runs off the receiver's
        // instantiation elsewhere for those.
        //
        // The head set is `impl_head_keeps_type_args`, shared with the
        // typechecker's `check_impl_block`, which types `self` for the same
        // body. Spelled separately, the two drifted. Mirrors
        // `make_generic_impl_method_function`, which keeps the full target
        // expr for the generic case.
        let self_generic_args = if crate::impl_dispatch::impl_head_keeps_type_args(type_name) {
            match &target_type.kind {
                TypeKind::Path(p) => p.generic_args.clone(),
                _ => None,
            }
        } else {
            None
        };
        let base = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![type_name.to_string()],
                generic_args: self_generic_args,
                span,
            }),
            span,
        };
        let ty = match self_kind {
            SelfParam::Owned => base,
            SelfParam::Ref => TypeExpr {
                kind: TypeKind::Ref(Box::new(base)),
                span,
            },
            SelfParam::MutRef => TypeExpr {
                kind: TypeKind::MutRef(Box::new(base)),
                span,
            },
        };
        let self_param = Param {
            span,
            pattern: Pattern {
                kind: PatternKind::Binding("self".to_string()),
                span,
            },
            ty,
            default_value: None,
            doc_comment: None,
            is_comptime: false,
            // A desugared `self` receiver. `frozen self` is not a stage-1
            // form — the mode is only accepted by `parse_param`, and self
            // params never go through it.
            is_frozen: false,
        };
        f.params.insert(0, self_param);
    }
    f.self_param = None;
    f
}
