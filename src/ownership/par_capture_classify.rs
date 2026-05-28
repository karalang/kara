//! Per-`par {}` block capture-mode classification — phase-7 codegen
//! tracker line 227 (L227).
//!
//! Walks every function body for `ExprKind::Par(block)` expressions
//! and produces a per-par-block `Vec<(name, ParCaptureMode)>` keyed by
//! the par expression's `SpanKey`. Today's MVP classifies:
//!
//! - **`SharedRc`** — bindings whose surface type resolves to a
//!   `shared struct` or `shared enum`. The codegen branch prologue
//!   emits one atomic rc_inc per branch and registers the binding
//!   with `track_rc_var`, so the branch-exit cleanup balances the
//!   refcount with an atomic rc_dec. Closes the latent miscompile
//!   when a single-branch capture flows into a function that consumes
//!   the reference: the parent's owning reference stays live through
//!   the par-run, and the branch's own +1 covers the branch-side
//!   consume.
//! - **`Copy`** — everything else (primitives, plain owned structs,
//!   Vec, String, ref params, …). Falls through to the existing
//!   by-value-through-env codegen path.
//!
//! The capture set produced here MUST match the codegen path's
//! free-variable collection in `compile_par_block`'s
//! `refs_in_block` — otherwise a classified name would not be
//! unpacked in the branch prologue (and the rc_inc would have nowhere
//! to attach). The walker mirrors `Codegen::refs_in_expr` /
//! `refs_in_block` shape-for-shape; any divergence between the two
//! is a bug. The classifier is conservative: missing entries default
//! to `Copy`, so a walker miss degrades to today's behavior rather
//! than emitting an inc against a non-RC payload.
//!
//! Multi-branch shared captures are already caught by
//! `E_CONCURRENT_SHARED_STRUCT` at `check_concurrent_shared_struct`
//! time; this pass runs strictly downstream, classifying the same
//! captures so the codegen path can emit correct RC code for the
//! single-branch (sole-ownership) case the diagnostic still admits.

use std::collections::HashSet;

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::typechecker::TypeCheckResult;

use super::{OwnershipChecker, ParCaptureMode};

impl OwnershipChecker<'_> {
    /// Final-pass walker — runs after `check_items` /
    /// `promote_rc_to_arc` / `check_concurrent_shared_struct`, so the
    /// typecheck-resolved binding-type map and shared-type info are
    /// available. Populates `self.par_capture_modes`. As a side
    /// effect, any binding classified as `SharedRc` is also added to
    /// `self.arc_values` under the enclosing function's key — this
    /// keeps `is_arc_binding` consistent on both sides of the par
    /// boundary so the parent's scope-exit rc_dec and the branch's
    /// rc_inc/rc_dec all dispatch through the atomic path. Without
    /// this, a shared struct captured into a par block but not
    /// otherwise classified as Rc-fallback by the use predicate
    /// would race between non-atomic parent dec and the branch's
    /// dec — atomic on one side, plain on the other, undefined.
    pub(crate) fn classify_par_capture_modes(&mut self) {
        let items: Vec<Item> = self.program.items.clone();
        for item in &items {
            match item {
                Item::Function(f) => {
                    let param_types = collect_param_type_names(&f.params);
                    let fn_key = f.name.clone();
                    classify_par_in_block(
                        &f.body,
                        &param_types,
                        &fn_key,
                        self.typecheck_result,
                        &mut self.par_capture_modes,
                        &mut self.arc_values,
                    );
                }
                Item::ImplBlock(imp) => {
                    let self_type = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned(),
                        _ => None,
                    };
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            let mut param_types = collect_param_type_names(&m.params);
                            if let Some(name) = &self_type {
                                param_types.push(("self".to_string(), name.clone()));
                            }
                            let fn_key = match &self_type {
                                Some(t) => format!("{}.{}", t, m.name),
                                None => m.name.clone(),
                            };
                            classify_par_in_block(
                                &m.body,
                                &param_types,
                                &fn_key,
                                self.typecheck_result,
                                &mut self.par_capture_modes,
                                &mut self.arc_values,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Surface type of each parameter, stripped of `ref` / `mut ref` /
/// `weak` wrappers and resolved to the path's last segment. Used as
/// the seed for binding-type lookups when traversing the body.
fn collect_param_type_names(params: &[Param]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for p in params {
        let Some(name) = p.name() else { continue };
        if let Some(head) = type_expr_head_name(&p.ty) {
            out.push((name.to_string(), head));
        }
    }
    out
}

fn type_expr_head_name(ty: &TypeExpr) -> Option<String> {
    match &ty.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        TypeKind::Ref(inner) | TypeKind::MutRef(inner) | TypeKind::Weak(inner) => {
            type_expr_head_name(inner)
        }
        _ => None,
    }
}

/// True iff `type_name` is a `shared struct` or `shared enum` per the
/// typechecker's collected metadata.
fn is_shared_type_name(name: &str, tcr: &TypeCheckResult) -> bool {
    if let Some(info) = tcr.struct_info.get(name) {
        return info.is_shared;
    }
    if let Some(info) = tcr.enum_info.get(name) {
        return info.is_shared;
    }
    false
}

/// Walk `block` collecting `(name, ParCaptureMode)` for each par-block
/// it (directly or transitively) contains. `binding_types` carries the
/// already-classified type-name for each binding in scope; let-bindings
/// extend it as the walker descends. `fn_key` is the enclosing
/// function's key (`fn_name` or `Type.method`) so SharedRc captures can
/// be promoted into `arc_values` in lockstep.
fn classify_par_in_block(
    block: &Block,
    binding_types: &[(String, String)],
    fn_key: &str,
    tcr: &TypeCheckResult,
    out: &mut std::collections::HashMap<SpanKey, Vec<(String, ParCaptureMode)>>,
    arc_values: &mut std::collections::HashMap<String, HashSet<String>>,
) {
    let mut scoped = binding_types.to_vec();
    for stmt in &block.stmts {
        classify_par_in_stmt(stmt, &mut scoped, fn_key, tcr, out, arc_values);
    }
    if let Some(e) = &block.final_expr {
        classify_par_in_expr(e, &scoped, fn_key, tcr, out, arc_values);
    }
}

fn classify_par_in_stmt(
    stmt: &Stmt,
    binding_types: &mut Vec<(String, String)>,
    fn_key: &str,
    tcr: &TypeCheckResult,
    out: &mut std::collections::HashMap<SpanKey, Vec<(String, ParCaptureMode)>>,
    arc_values: &mut std::collections::HashMap<String, HashSet<String>>,
) {
    match &stmt.kind {
        StmtKind::Let {
            pattern, value, ty, ..
        }
        | StmtKind::LetElse {
            pattern, value, ty, ..
        } => {
            classify_par_in_expr(value, binding_types, fn_key, tcr, out, arc_values);
            // Register any binding-pattern leaves with the resolved
            // surface type. Prefer the explicit annotation; fall back
            // to the typechecker's per-leaf binding-type map for
            // inferred lets (`let c = make_counter();`).
            for name in pattern.binding_names() {
                let resolved = ty.as_ref().and_then(type_expr_head_name).or_else(|| {
                    tcr.pattern_binding_types
                        .get(&SpanKey::from_span(&pattern.span))
                        .cloned()
                });
                if let Some(name_ty) = resolved {
                    binding_types.push((name, name_ty));
                }
            }
            if let StmtKind::LetElse { else_block, .. } = &stmt.kind {
                classify_par_in_block(else_block, binding_types, fn_key, tcr, out, arc_values);
            }
        }
        StmtKind::Expr(e) => classify_par_in_expr(e, binding_types, fn_key, tcr, out, arc_values),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            classify_par_in_expr(target, binding_types, fn_key, tcr, out, arc_values);
            classify_par_in_expr(value, binding_types, fn_key, tcr, out, arc_values);
        }
        _ => {}
    }
}

fn classify_par_in_expr(
    expr: &Expr,
    binding_types: &[(String, String)],
    fn_key: &str,
    tcr: &TypeCheckResult,
    out: &mut std::collections::HashMap<SpanKey, Vec<(String, ParCaptureMode)>>,
    arc_values: &mut std::collections::HashMap<String, HashSet<String>>,
) {
    match &expr.kind {
        ExprKind::Par(body) => {
            // Capture set = identifiers referenced inside `body` minus
            // names introduced inside the body itself. Mirrors
            // `Codegen::refs_in_block`.
            let mut refs = HashSet::new();
            let mut defs = HashSet::new();
            refs_in_block(body, &mut refs, &mut defs);
            let mut captures: Vec<String> = refs
                .into_iter()
                .filter(|n| !defs.contains(n))
                .filter(|n| binding_types.iter().any(|(b, _)| b == n))
                .collect();
            captures.sort();

            let mut modes: Vec<(String, ParCaptureMode)> = Vec::with_capacity(captures.len());
            for name in captures {
                let ty_name = binding_types
                    .iter()
                    .rev()
                    .find(|(b, _)| b == &name)
                    .map(|(_, t)| t.as_str())
                    .unwrap_or("");
                let mode = if is_shared_type_name(ty_name, tcr) {
                    arc_values
                        .entry(fn_key.to_string())
                        .or_default()
                        .insert(name.clone());
                    ParCaptureMode::SharedRc
                } else {
                    ParCaptureMode::Copy
                };
                modes.push((name, mode));
            }
            // Key by the par body's block span — codegen
            // (`compile_par_block`) forwards `block.span` to
            // `emit_par_run`, so the lookup site uses the inner block
            // span, not the outer `Par(body)` expression span.
            out.insert(SpanKey::from_span(&body.span), modes);
            // Descend into the par body so nested par-blocks get their
            // own entries.
            classify_par_in_block(body, binding_types, fn_key, tcr, out, arc_values);
        }
        ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Try(b) | ExprKind::Unsafe(b) => {
            classify_par_in_block(b, binding_types, fn_key, tcr, out, arc_values);
        }
        ExprKind::Binary { left, right, .. } => {
            classify_par_in_expr(left, binding_types, fn_key, tcr, out, arc_values);
            classify_par_in_expr(right, binding_types, fn_key, tcr, out, arc_values);
        }
        ExprKind::Unary { operand, .. } => {
            classify_par_in_expr(operand, binding_types, fn_key, tcr, out, arc_values)
        }
        ExprKind::Call { callee, args } => {
            classify_par_in_expr(callee, binding_types, fn_key, tcr, out, arc_values);
            for a in args {
                classify_par_in_expr(&a.value, binding_types, fn_key, tcr, out, arc_values);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            classify_par_in_expr(object, binding_types, fn_key, tcr, out, arc_values);
            for a in args {
                classify_par_in_expr(&a.value, binding_types, fn_key, tcr, out, arc_values);
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            classify_par_in_expr(condition, binding_types, fn_key, tcr, out, arc_values);
            classify_par_in_block(then_block, binding_types, fn_key, tcr, out, arc_values);
            if let Some(e) = else_branch {
                classify_par_in_expr(e, binding_types, fn_key, tcr, out, arc_values);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            classify_par_in_expr(condition, binding_types, fn_key, tcr, out, arc_values);
            classify_par_in_block(body, binding_types, fn_key, tcr, out, arc_values);
        }
        ExprKind::Loop { body, .. } => {
            classify_par_in_block(body, binding_types, fn_key, tcr, out, arc_values)
        }
        ExprKind::Return(Some(e)) => {
            classify_par_in_expr(e, binding_types, fn_key, tcr, out, arc_values)
        }
        ExprKind::Break { value: Some(e), .. } => {
            classify_par_in_expr(e, binding_types, fn_key, tcr, out, arc_values)
        }
        ExprKind::FieldAccess { object, .. } => {
            classify_par_in_expr(object, binding_types, fn_key, tcr, out, arc_values)
        }
        ExprKind::TupleIndex { object, .. } => {
            classify_par_in_expr(object, binding_types, fn_key, tcr, out, arc_values)
        }
        ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
            for e in elems {
                classify_par_in_expr(e, binding_types, fn_key, tcr, out, arc_values);
            }
        }
        ExprKind::StructLiteral { fields, .. } => {
            for f in fields {
                classify_par_in_expr(&f.value, binding_types, fn_key, tcr, out, arc_values);
            }
        }
        ExprKind::Cast { expr: inner, .. } => {
            classify_par_in_expr(inner, binding_types, fn_key, tcr, out, arc_values)
        }
        ExprKind::Match { scrutinee, arms } => {
            classify_par_in_expr(scrutinee, binding_types, fn_key, tcr, out, arc_values);
            for arm in arms {
                classify_par_in_expr(&arm.body, binding_types, fn_key, tcr, out, arc_values);
            }
        }
        ExprKind::For { iterable, body, .. } => {
            classify_par_in_expr(iterable, binding_types, fn_key, tcr, out, arc_values);
            classify_par_in_block(body, binding_types, fn_key, tcr, out, arc_values);
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            classify_par_in_expr(value, binding_types, fn_key, tcr, out, arc_values);
            classify_par_in_block(then_block, binding_types, fn_key, tcr, out, arc_values);
            if let Some(e) = else_branch {
                classify_par_in_expr(e, binding_types, fn_key, tcr, out, arc_values);
            }
        }
        ExprKind::Closure { body, .. } => {
            classify_par_in_expr(body, binding_types, fn_key, tcr, out, arc_values);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                classify_par_in_expr(s, binding_types, fn_key, tcr, out, arc_values);
            }
            if let Some(e) = end {
                classify_par_in_expr(e, binding_types, fn_key, tcr, out, arc_values);
            }
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts {
                if let ParsedInterpolationPart::Expr(inner) = part {
                    classify_par_in_expr(inner, binding_types, fn_key, tcr, out, arc_values);
                }
            }
        }
        ExprKind::Index { object, index } => {
            classify_par_in_expr(object, binding_types, fn_key, tcr, out, arc_values);
            classify_par_in_expr(index, binding_types, fn_key, tcr, out, arc_values);
        }
        _ => {}
    }
}

// ── Free-function mirror of Codegen::refs_in_{expr,block} ────────────
//
// Kept here as a strictly-local copy so the classifier sees the SAME
// capture set the codegen path computes. Any drift between the two
// would silently mis-classify (or miss) captures. If the codegen
// walker grows new arms, this mirror grows with it.

fn refs_in_expr(expr: &Expr, refs: &mut HashSet<String>, defs: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Identifier(n) => {
            refs.insert(n.clone());
        }
        ExprKind::SelfValue => {
            refs.insert("self".to_string());
        }
        ExprKind::Binary { left, right, .. } => {
            refs_in_expr(left, refs, defs);
            refs_in_expr(right, refs, defs);
        }
        ExprKind::Unary { operand, .. } => refs_in_expr(operand, refs, defs),
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
        ExprKind::Block(block) | ExprKind::Seq(block) => {
            refs_in_block(block, refs, defs);
        }
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
        ExprKind::StructLiteral { fields, .. } => {
            for f in fields {
                refs_in_expr(&f.value, refs, defs);
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
                if let ParsedInterpolationPart::Expr(inner) = part {
                    refs_in_expr(inner, refs, defs);
                }
            }
        }
        ExprKind::Index { object, index } => {
            refs_in_expr(object, refs, defs);
            refs_in_expr(index, refs, defs);
        }
        ExprKind::Par(b) | ExprKind::Try(b) | ExprKind::Unsafe(b) => {
            refs_in_block(b, refs, defs);
        }
        _ => {}
    }
}

fn refs_in_block(block: &Block, refs: &mut HashSet<String>, defs: &mut HashSet<String>) {
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
