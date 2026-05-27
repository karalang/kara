//! E_CONCURRENT_SHARED_STRUCT + E_CONCURRENT_PLAIN_STRUCT — phase-7 line 197.
//!
//! Detects struct/enum bindings that are referenced from two or more
//! top-level statements (branches) of the same `par {}` block.
//!
//! Two related diagnostics, one detection pass:
//!
//! - **`E_CONCURRENT_SHARED_STRUCT`** — `shared struct` / `shared enum`
//!   binding. Per design.md § Rc vs Arc — Two-Phase Algorithm "Rule for
//!   `shared struct`": `live_range(v) ∩ parallel_region ≠ ∅` AND
//!   reachable from > 1 concurrent branch → compile error. Sole-
//!   ownership move into exactly one branch is not an error.
//! - **`E_CONCURRENT_PLAIN_STRUCT`** — plain (non-shared) `struct`
//!   binding. Per design.md § Compiler-assisted migration from plain
//!   `struct` to `par struct`: silent promotion is rejected (the field
//!   constraints differ structurally), so the compiler emits the
//!   structured error and the programmer migrates to `par struct`.
//!
//! Both diagnostics carry the same `fix_diff` envelope: per-`mut`-field
//! `Mutex[T]` wrap edits derivable from each `StructField.ty.span` —
//! two pure-insertion `TextEdit`s per field (`Mutex[` prefix + `]`
//! suffix around the field's type). Stored on
//! `OwnershipChecker.error_fix_diffs` keyed by the diagnostic's primary
//! `SpanKey`, surfaced via `OwnershipCheckResult.error_fix_diffs`.
//!
//! Keyword rename (`shared struct` / `struct` → `par struct`) and the
//! `mut ` keyword stripping live in suggestion prose until the parser
//! exposes keyword spans on `StructDef`. Lock-block insertion at write
//! sites and Rc→Arc clone-semantics notes are sibling follow-ups.
//!
//! Scope (v1):
//! - Direct binding-name references within branch statements count.
//! - Field-access (`s.field`) and method-call (`s.method(...)`)
//!   receivers count via the inner Identifier walk.
//! - Closure captures count via `closure_bindings` expansion (mirrors
//!   the round-12.34 mechanism in `par_helpers.rs`): a `let f = ||
//!   use(c);` registration plus a sibling-branch `spawn(f)` counts as
//!   a branch-use of `c`. Inline `spawn(|| use(c))` closures count via
//!   `OwnershipChecker.closure_captures` lookup at the closure's span.
//! - `spawn(...)` boundaries beyond a `par {}` block fall outside the
//!   v1 detection — sibling follow-up.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::resolver::{SpanKey, TextEdit};
use crate::token::Span;

use super::{OwnershipError, OwnershipErrorKind, OwnershipMode};

type BindingTypeMap = HashMap<SpanKey, String>;
type ClosureCaptures = HashMap<SpanKey, Vec<(String, OwnershipMode)>>;
type ClosureBindings = HashMap<String, Vec<String>>;

/// Discriminator carried alongside each tracked binding so one walk
/// catches both diagnostic flavors without two parallel maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingKind {
    Shared,
    Plain,
}

#[derive(Debug, Clone)]
struct TrackedBinding {
    type_name: String,
    kind: BindingKind,
}

impl<'a> super::OwnershipChecker<'a> {
    /// Walk every function body, collect bindings whose surface type is
    /// a (shared OR plain) `struct` / `shared enum`, and emit
    /// `E_CONCURRENT_SHARED_STRUCT` / `E_CONCURRENT_PLAIN_STRUCT` for
    /// any such binding referenced from more than one branch (top-level
    /// statement) of the same `par {}` block. Plain enums and unions
    /// are skipped — design.md only calls out the struct/shared-struct/
    /// shared-enum cases.
    pub(crate) fn check_concurrent_shared_struct(&mut self) {
        let items: Vec<Item> = self.program.items.clone();
        let mut errors: Vec<OwnershipError> = Vec::new();
        let mut fix_diffs: HashMap<SpanKey, Vec<TextEdit>> = HashMap::new();
        let closure_captures = &self.closure_captures;
        for item in &items {
            match item {
                Item::Function(f) => {
                    let tracked = self.collect_tracked_bindings(&f.params, &f.body);
                    if !tracked.is_empty() {
                        let mut closure_bindings = ClosureBindings::new();
                        build_closure_bindings(&f.body, closure_captures, &mut closure_bindings);
                        scan_block_for_par_conflicts(
                            &f.body,
                            &tracked,
                            &items,
                            closure_captures,
                            &closure_bindings,
                            &mut errors,
                            &mut fix_diffs,
                        );
                    }
                }
                Item::ImplBlock(imp) => {
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            let tracked = self.collect_tracked_bindings(&m.params, &m.body);
                            if !tracked.is_empty() {
                                let mut closure_bindings = ClosureBindings::new();
                                build_closure_bindings(
                                    &m.body,
                                    closure_captures,
                                    &mut closure_bindings,
                                );
                                scan_block_for_par_conflicts(
                                    &m.body,
                                    &tracked,
                                    &items,
                                    closure_captures,
                                    &closure_bindings,
                                    &mut errors,
                                    &mut fix_diffs,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.errors.extend(errors);
        self.error_fix_diffs.extend(fix_diffs);
    }

    /// Collect bindings (parameters + let-introduced) whose surface
    /// type resolves to either a `shared struct` / `shared enum` OR a
    /// plain (non-shared) `struct`. The discriminator is recorded as
    /// `BindingKind` so the per-branch detector picks the right
    /// diagnostic flavor without re-querying. Plain enums and unions
    /// fall through (no migration target spec'd).
    fn collect_tracked_bindings(
        &self,
        params: &[Param],
        body: &Block,
    ) -> HashMap<String, TrackedBinding> {
        let mut tracked = HashMap::new();
        for p in params {
            let Some(name) = p.name() else { continue };
            let Some(head) = type_expr_head_name(&p.ty) else {
                continue;
            };
            if let Some(kind) = self.classify_binding_type(&head) {
                tracked.insert(
                    name.to_string(),
                    TrackedBinding {
                        type_name: head,
                        kind,
                    },
                );
            }
        }
        collect_let_tracked_bindings(
            body,
            &self.typecheck_result.pattern_binding_types,
            &mut tracked,
            |n| self.classify_binding_type(n),
        );
        tracked
    }

    /// Return `Shared` for `shared struct` / `shared enum`, `Plain` for
    /// non-shared `struct`, `None` for anything else (plain enum,
    /// union, primitive, generic param, …).
    fn classify_binding_type(&self, name: &str) -> Option<BindingKind> {
        if let Some(info) = self.typecheck_result.struct_info.get(name) {
            return Some(if info.is_shared {
                BindingKind::Shared
            } else {
                BindingKind::Plain
            });
        }
        if let Some(info) = self.typecheck_result.enum_info.get(name) {
            if info.is_shared {
                return Some(BindingKind::Shared);
            }
        }
        None
    }
}

/// Scan `body` for `ExprKind::Par` blocks; for each, walk every
/// top-level statement (branch) collecting referenced names, and
/// emit one diagnostic per binding present in more than one branch.
fn scan_block_for_par_conflicts(
    block: &Block,
    tracked: &HashMap<String, TrackedBinding>,
    program_items: &[Item],
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    errors: &mut Vec<OwnershipError>,
    fix_diffs: &mut HashMap<SpanKey, Vec<TextEdit>>,
) {
    for stmt in &block.stmts {
        scan_stmt_for_par_conflicts(
            stmt,
            tracked,
            program_items,
            closure_captures,
            closure_bindings,
            errors,
            fix_diffs,
        );
    }
    if let Some(e) = &block.final_expr {
        scan_expr_for_par_conflicts(
            e,
            tracked,
            program_items,
            closure_captures,
            closure_bindings,
            errors,
            fix_diffs,
        );
    }
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

/// Walk a block looking for `let pat = rhs;` forms and record every
/// leaf binding whose `pattern_binding_types`-recorded surface type
/// resolves to a tracked kind. Threads through nested blocks.
fn collect_let_tracked_bindings(
    block: &Block,
    pattern_binding_types: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<BindingKind> + Copy,
) {
    for stmt in &block.stmts {
        collect_let_in_stmt(stmt, pattern_binding_types, out, classify);
    }
    if let Some(e) = &block.final_expr {
        collect_let_in_expr(e, pattern_binding_types, out, classify);
    }
}

fn collect_let_in_stmt(
    stmt: &Stmt,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<BindingKind> + Copy,
) {
    match &stmt.kind {
        StmtKind::Let { pattern, value, .. } | StmtKind::LetElse { pattern, value, .. } => {
            record_pattern_bindings(pattern, pbt, out, classify);
            collect_let_in_expr(value, pbt, out, classify);
            if let StmtKind::LetElse { else_block, .. } = &stmt.kind {
                collect_let_tracked_bindings(else_block, pbt, out, classify);
            }
        }
        StmtKind::LetUninit { .. } => {
            // Uninitialized bindings are restricted to a single name with
            // a required type annotation. Plain-struct uninitialized
            // bindings are flagged at first read by definite-assignment
            // analysis (UseOfUninitialized) before they could reach a
            // par-block; shared-struct uninitialized bindings cannot
            // construct via let-uninit at all. Skip safely.
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_let_tracked_bindings(body, pbt, out, classify);
        }
        StmtKind::Assign { target, value } => {
            collect_let_in_expr(target, pbt, out, classify);
            collect_let_in_expr(value, pbt, out, classify);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            collect_let_in_expr(target, pbt, out, classify);
            collect_let_in_expr(value, pbt, out, classify);
        }
        StmtKind::Expr(e) => {
            collect_let_in_expr(e, pbt, out, classify);
        }
    }
}

fn record_pattern_bindings(
    pattern: &Pattern,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<BindingKind> + Copy,
) {
    record_pattern_inner(pattern, pbt, out, classify);
}

fn record_pattern_inner(
    pattern: &Pattern,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<BindingKind> + Copy,
) {
    match &pattern.kind {
        PatternKind::Binding(name) => {
            let key = SpanKey::from_span(&pattern.span);
            if let Some(head) = pbt.get(&key) {
                if let Some(kind) = classify(head) {
                    out.insert(
                        name.clone(),
                        TrackedBinding {
                            type_name: head.clone(),
                            kind,
                        },
                    );
                }
            }
        }
        PatternKind::AtBinding { name, pattern: sub } => {
            let key = SpanKey::from_span(&pattern.span);
            if let Some(head) = pbt.get(&key) {
                if let Some(kind) = classify(head) {
                    out.insert(
                        name.clone(),
                        TrackedBinding {
                            type_name: head.clone(),
                            kind,
                        },
                    );
                }
            }
            record_pattern_inner(sub, pbt, out, classify);
        }
        PatternKind::Tuple(items) => {
            for p in items {
                record_pattern_inner(p, pbt, out, classify);
            }
        }
        PatternKind::Struct { fields, .. } => {
            for f in fields {
                if let Some(p) = &f.pattern {
                    record_pattern_inner(p, pbt, out, classify);
                }
            }
        }
        PatternKind::TupleVariant { patterns, .. } => {
            for p in patterns {
                record_pattern_inner(p, pbt, out, classify);
            }
        }
        _ => {}
    }
}

fn collect_let_in_expr(
    expr: &Expr,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<BindingKind> + Copy,
) {
    match &expr.kind {
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            collect_let_tracked_bindings(b, pbt, out, classify);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_let_in_expr(condition, pbt, out, classify);
            collect_let_tracked_bindings(then_block, pbt, out, classify);
            if let Some(else_b) = else_branch {
                collect_let_in_expr(else_b, pbt, out, classify);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_let_in_expr(condition, pbt, out, classify);
            collect_let_tracked_bindings(body, pbt, out, classify);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_let_in_expr(iterable, pbt, out, classify);
            collect_let_tracked_bindings(body, pbt, out, classify);
        }
        _ => {}
    }
}

fn scan_stmt_for_par_conflicts(
    stmt: &Stmt,
    tracked: &HashMap<String, TrackedBinding>,
    program_items: &[Item],
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    errors: &mut Vec<OwnershipError>,
    fix_diffs: &mut HashMap<SpanKey, Vec<TextEdit>>,
) {
    match &stmt.kind {
        StmtKind::Let { value, .. } => {
            scan_expr_for_par_conflicts(
                value,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            scan_expr_for_par_conflicts(
                value,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            for s in &else_block.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
            if let Some(e) = &else_block.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
            if let Some(e) = &body.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        StmtKind::Assign { target, value } => {
            scan_expr_for_par_conflicts(
                target,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            scan_expr_for_par_conflicts(
                value,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            scan_expr_for_par_conflicts(
                target,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            scan_expr_for_par_conflicts(
                value,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
        }
        StmtKind::Expr(e) => {
            scan_expr_for_par_conflicts(
                e,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
        }
    }
}

fn scan_expr_for_par_conflicts(
    expr: &Expr,
    tracked: &HashMap<String, TrackedBinding>,
    program_items: &[Item],
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    errors: &mut Vec<OwnershipError>,
    fix_diffs: &mut HashMap<SpanKey, Vec<TextEdit>>,
) {
    match &expr.kind {
        ExprKind::Par(par_body) => {
            detect_par_block_conflicts(
                par_body,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            for stmt in &par_body.stmts {
                scan_stmt_for_par_conflicts(
                    stmt,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
            if let Some(e) = &par_body.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::Block(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            for stmt in &b.stmts {
                scan_stmt_for_par_conflicts(
                    stmt,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
            if let Some(e) = &b.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            scan_expr_for_par_conflicts(
                condition,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            for s in &then_block.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
            if let Some(e) = &then_block.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
            if let Some(else_b) = else_branch {
                scan_expr_for_par_conflicts(
                    else_b,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            scan_expr_for_par_conflicts(
                condition,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            for s in &body.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
            if let Some(e) = &body.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::For { iterable, body, .. } => {
            scan_expr_for_par_conflicts(
                iterable,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            for s in &body.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
            if let Some(e) = &body.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            scan_expr_for_par_conflicts(
                scrutinee,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            for arm in arms {
                if let Some(g) = &arm.guard {
                    scan_expr_for_par_conflicts(
                        g,
                        tracked,
                        program_items,
                        closure_captures,
                        closure_bindings,
                        errors,
                        fix_diffs,
                    );
                }
                scan_expr_for_par_conflicts(
                    &arm.body,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::Call { callee, args } => {
            scan_expr_for_par_conflicts(
                callee,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            for a in args {
                scan_expr_for_par_conflicts(
                    &a.value,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            scan_expr_for_par_conflicts(
                object,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            for a in args {
                scan_expr_for_par_conflicts(
                    &a.value,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::FieldAccess { object, .. } => {
            scan_expr_for_par_conflicts(
                object,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
        }
        ExprKind::Index { object, index } => {
            scan_expr_for_par_conflicts(
                object,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            scan_expr_for_par_conflicts(
                index,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
        }
        ExprKind::Binary { left, right, .. } => {
            scan_expr_for_par_conflicts(
                left,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
            scan_expr_for_par_conflicts(
                right,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
        }
        ExprKind::Unary { operand, .. } => {
            scan_expr_for_par_conflicts(
                operand,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
        }
        ExprKind::Tuple(items) => {
            for e in items {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::Return(inner) => {
            if let Some(e) = inner.as_deref() {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value.as_deref() {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        ExprKind::Cast { expr, .. } => {
            scan_expr_for_par_conflicts(
                expr,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                errors,
                fix_diffs,
            );
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                scan_expr_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
            if let Some(e) = end.as_deref() {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    errors,
                    fix_diffs,
                );
            }
        }
        _ => {}
    }
}

/// Walk each top-level statement of `par_body` collecting the set of
/// tracked-binding names referenced anywhere in that branch's subtree.
/// Any binding appearing in two or more branches → emit the right
/// diagnostic flavor at the second-branch use site, with the first-
/// branch use threaded as the `consume_span` secondary.
fn detect_par_block_conflicts(
    par_body: &Block,
    tracked: &HashMap<String, TrackedBinding>,
    program_items: &[Item],
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    errors: &mut Vec<OwnershipError>,
    fix_diffs: &mut HashMap<SpanKey, Vec<TextEdit>>,
) {
    let mut first_use: HashMap<String, (usize, Span)> = HashMap::new();
    let mut reported: HashSet<String> = HashSet::new();

    for (branch_idx, stmt) in par_body.stmts.iter().enumerate() {
        let mut uses: HashMap<String, Span> = HashMap::new();
        collect_identifier_uses_in_stmt(
            stmt,
            tracked,
            closure_captures,
            closure_bindings,
            &mut uses,
        );
        for (name, use_span) in uses {
            if reported.contains(&name) {
                continue;
            }
            match first_use.get(&name) {
                Some((prev_idx, prev_span)) if *prev_idx != branch_idx => {
                    let binding = &tracked[&name];
                    let err = build_concurrent_struct_error(
                        &name,
                        binding,
                        use_span.clone(),
                        prev_span.clone(),
                    );
                    let edits = build_fix_diff_edits(&binding.type_name, program_items);
                    if !edits.is_empty() {
                        fix_diffs.insert(SpanKey::from_span(&err.span), edits);
                    }
                    errors.push(err);
                    reported.insert(name);
                }
                None => {
                    first_use.insert(name, (branch_idx, use_span));
                }
                _ => {}
            }
        }
    }
}

fn build_concurrent_struct_error(
    binding: &str,
    tracked: &TrackedBinding,
    second_use_span: Span,
    first_use_span: Span,
) -> OwnershipError {
    let (kind_label, kind_variant) = match tracked.kind {
        BindingKind::Shared => (
            "shared struct",
            OwnershipErrorKind::ConcurrentSharedStruct {
                type_name: tracked.type_name.clone(),
                binding: binding.to_string(),
            },
        ),
        BindingKind::Plain => (
            "plain struct",
            OwnershipErrorKind::ConcurrentPlainStruct {
                type_name: tracked.type_name.clone(),
                binding: binding.to_string(),
            },
        ),
    };
    let message = format!(
        "{kind_label} `{ty}` cannot be accessed from multiple concurrent tasks (binding `{bn}` reachable from two par-block branches: first use at line {fl}:{fc}, conflicting use here)",
        ty = tracked.type_name,
        bn = binding,
        fl = first_use_span.line,
        fc = first_use_span.column,
    );
    let suggestion = match tracked.kind {
        BindingKind::Shared => format!(
            "convert `{ty}` to `par struct` and wrap mut fields in `Mutex[T]`/`Atomic[T]`. The migration is structural:\n  1. rename `shared struct {ty}` to `par struct {ty}`\n  2. wrap each bare `mut` field in `Mutex[T]` (refine to `Atomic[T]` post-review where lock-free access is appropriate)\n  3. insert `lock field {{ ... }}` blocks at every write site within `par` regions\n  4. call sites that previously relied on implicit `Rc`-clone now produce `Arc`-clone semantics\nor run `karac migrate shared-to-par {ty}` for a preemptive workspace rewrite.",
            ty = tracked.type_name,
        ),
        BindingKind::Plain => format!(
            "convert `{ty}` to `par struct` and wrap mut fields in `Mutex[T]`/`Atomic[T]`. The migration is structural:\n  1. rename `struct {ty}` to `par struct {ty}`\n  2. wrap each bare `mut` field in `Mutex[T]` (refine to `Atomic[T]` post-review where lock-free access is appropriate)\n  3. insert `lock field {{ ... }}` blocks at every write site within `par` regions\n  4. consumers that took the value by ownership now share an `Arc` clone\nThe machine-applicable `fix_diff` covers the per-field `Mutex[T]` wraps; the keyword rewrite and `mut ` stripping are left as the human review step.",
            ty = tracked.type_name,
        ),
    };
    OwnershipError {
        message,
        span: second_use_span,
        kind: kind_variant,
        suggestion: Some(suggestion),
        replacement: None,
        consume_span: Some(first_use_span),
    }
}

/// For every `mut field: T` in the matching `StructDef`, build two
/// pure-insertion `TextEdit`s — `Mutex[` prefix before `ty.span.offset`
/// and `]` suffix after `ty.span.offset + ty.span.length`. Order
/// matters only for downstream diff rendering; both ends of one field
/// don't overlap with another field's edits, so the consumer can apply
/// them in any order as long as offsets stay stable (apply back-to-
/// front per the standard `karac fix` discipline). Returns an empty
/// vec when no matching struct is found or it has no mut fields.
fn build_fix_diff_edits(type_name: &str, program_items: &[Item]) -> Vec<TextEdit> {
    let Some(struct_def) = program_items.iter().find_map(|it| match it {
        Item::StructDef(s) if s.name == type_name => Some(s),
        _ => None,
    }) else {
        return Vec::new();
    };
    let mut edits = Vec::new();
    for field in &struct_def.fields {
        if !field.is_mut {
            continue;
        }
        let ty_off = field.ty.span.offset;
        let ty_len = field.ty.span.length;
        edits.push(TextEdit {
            offset: ty_off,
            length: 0,
            replacement: "Mutex[".to_string(),
        });
        edits.push(TextEdit {
            offset: ty_off + ty_len,
            length: 0,
            replacement: "]".to_string(),
        });
    }
    edits
}

fn collect_identifier_uses_in_stmt(
    stmt: &Stmt,
    tracked: &HashMap<String, TrackedBinding>,
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    out: &mut HashMap<String, Span>,
) {
    match &stmt.kind {
        StmtKind::Let { value, .. } => {
            collect_identifier_uses_in_expr(
                value,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            collect_identifier_uses_in_expr(
                value,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for s in &else_block.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &else_block.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &body.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        StmtKind::Assign { target, value } => {
            collect_identifier_uses_in_expr(
                target,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            collect_identifier_uses_in_expr(
                value,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            collect_identifier_uses_in_expr(
                target,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            collect_identifier_uses_in_expr(
                value,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        StmtKind::Expr(e) => {
            collect_identifier_uses_in_expr(e, tracked, closure_captures, closure_bindings, out);
        }
    }
}

fn collect_identifier_uses_in_expr(
    expr: &Expr,
    tracked: &HashMap<String, TrackedBinding>,
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    out: &mut HashMap<String, Span>,
) {
    match &expr.kind {
        ExprKind::Identifier(name) => {
            // Direct tracked-binding reference.
            if tracked.contains_key(name) {
                out.entry(name.clone()).or_insert_with(|| expr.span.clone());
            }
            // Indirect reference via a let-bound closure that captures
            // tracked bindings — `let f = || use(c);` followed by a
            // sibling-branch `Identifier(f)` counts as a branch-use of
            // `c`. Mirrors `par_helpers.rs::scan_expr_for_par_uses`'s
            // round-12.34 closure_bindings propagation so closure-
            // dispatched flows of a shared/plain struct don't slip past
            // the per-branch identifier walk.
            for cap in expand_through_closure_bindings(name, closure_bindings) {
                if tracked.contains_key(&cap) {
                    out.entry(cap).or_insert_with(|| expr.span.clone());
                }
            }
        }
        ExprKind::Closure { body, .. } => {
            // Inline closure form — `spawn(|| use(c))`. The closure's
            // captures (resolved by the OwnershipChecker's capture-
            // inference pass and stored in `closure_captures` keyed by
            // the closure expression's span) are the explicit set of
            // outer names the body references; each captured name that
            // resolves to a tracked binding (directly or transitively
            // via another closure binding) counts as a branch-use of
            // that name, recorded at the closure expression's span.
            let key = SpanKey::from_span(&expr.span);
            if let Some(captures) = closure_captures.get(&key) {
                for (cap_name, _) in captures {
                    if tracked.contains_key(cap_name) {
                        out.entry(cap_name.clone())
                            .or_insert_with(|| expr.span.clone());
                    }
                    for chained in expand_through_closure_bindings(cap_name, closure_bindings) {
                        if tracked.contains_key(&chained) {
                            out.entry(chained).or_insert_with(|| expr.span.clone());
                        }
                    }
                }
            }
            // Closure bodies are NOT recursively walked here — the
            // `closure_captures` map is the authoritative set of outer
            // names the body references (filtered through the body's
            // own bindings and shadowing). Walking the body textually
            // would risk double-counting locals or shadowed names.
            // The body's effects on outer state are already reflected
            // in the captures list. Nested par-blocks inside closure
            // bodies are likewise not detected here at v1; if a future
            // workload surfaces them, the fix is to recurse via
            // `scan_block_for_par_conflicts` over the closure body
            // rather than through this identifier walker.
            let _ = body;
        }
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            for s in &b.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &b.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_identifier_uses_in_expr(
                condition,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for s in &then_block.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &then_block.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(eb) = else_branch {
                collect_identifier_uses_in_expr(
                    eb,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_identifier_uses_in_expr(
                condition,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for s in &body.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &body.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::For { iterable, body, .. } => {
            collect_identifier_uses_in_expr(
                iterable,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for s in &body.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &body.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_identifier_uses_in_expr(
                scrutinee,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_identifier_uses_in_expr(
                        g,
                        tracked,
                        closure_captures,
                        closure_bindings,
                        out,
                    );
                }
                collect_identifier_uses_in_expr(
                    &arm.body,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Call { callee, args } => {
            collect_identifier_uses_in_expr(
                callee,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for a in args {
                collect_identifier_uses_in_expr(
                    &a.value,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_identifier_uses_in_expr(
                object,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for a in args {
                collect_identifier_uses_in_expr(
                    &a.value,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::FieldAccess { object, .. } => {
            collect_identifier_uses_in_expr(
                object,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        ExprKind::Index { object, index } => {
            collect_identifier_uses_in_expr(
                object,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            collect_identifier_uses_in_expr(
                index,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        ExprKind::Binary { left, right, .. } => {
            collect_identifier_uses_in_expr(left, tracked, closure_captures, closure_bindings, out);
            collect_identifier_uses_in_expr(
                right,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        ExprKind::Unary { operand, .. } => {
            collect_identifier_uses_in_expr(
                operand,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        ExprKind::Tuple(items) => {
            for e in items {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Return(inner) => {
            if let Some(e) = inner.as_deref() {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value.as_deref() {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Cast { expr, .. } => {
            collect_identifier_uses_in_expr(expr, tracked, closure_captures, closure_bindings, out);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                collect_identifier_uses_in_expr(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = end.as_deref() {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        _ => {}
    }
}

/// Expand `name` through `closure_bindings` and return every closure-
/// captured name reachable from it (excluding `name` itself). Cycle-
/// safe via the visited set — guards against pathological self-capture
/// shapes the parser can't produce today but the data structure
/// permits. The returned set never contains `name` itself; the caller
/// is responsible for recording the direct hit when applicable.
fn expand_through_closure_bindings(name: &str, closure_bindings: &ClosureBindings) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(name.to_string());
    let mut stack: Vec<String> = vec![name.to_string()];
    while let Some(n) = stack.pop() {
        if let Some(captures) = closure_bindings.get(&n) {
            for c in captures {
                if seen.insert(c.clone()) {
                    out.push(c.clone());
                    stack.push(c.clone());
                }
            }
        }
    }
    out
}

/// Forward-walk every `let pat = closure_expr;` form in `body` and
/// register each pattern binding name in `out` mapping to the closure's
/// inferred capture list. Mirrors `par_helpers.rs`'s round-12.34 Step
/// 6 mechanism so a sibling-branch `Identifier(f)` use of a let-bound
/// closure can transitively count as a use of each captured tracked
/// binding. Single forward pass is sufficient because closure bindings
/// must be declared before they are referenced in source order.
fn build_closure_bindings(
    body: &Block,
    closure_captures: &ClosureCaptures,
    out: &mut ClosureBindings,
) {
    for stmt in &body.stmts {
        build_closure_bindings_stmt(stmt, closure_captures, out);
    }
    if let Some(e) = &body.final_expr {
        build_closure_bindings_expr(e, closure_captures, out);
    }
}

fn build_closure_bindings_stmt(
    stmt: &Stmt,
    closure_captures: &ClosureCaptures,
    out: &mut ClosureBindings,
) {
    match &stmt.kind {
        StmtKind::Let { pattern, value, .. } => {
            if matches!(value.kind, ExprKind::Closure { .. }) {
                if let Some(captures) = closure_captures.get(&SpanKey::from_span(&value.span)) {
                    let names: Vec<String> = captures.iter().map(|(n, _)| n.clone()).collect();
                    for binding in pattern.binding_names() {
                        out.insert(binding, names.clone());
                    }
                }
            }
            build_closure_bindings_expr(value, closure_captures, out);
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            build_closure_bindings_expr(value, closure_captures, out);
            build_closure_bindings(else_block, closure_captures, out);
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            build_closure_bindings(body, closure_captures, out);
        }
        StmtKind::Assign { target, value } => {
            build_closure_bindings_expr(target, closure_captures, out);
            build_closure_bindings_expr(value, closure_captures, out);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            build_closure_bindings_expr(target, closure_captures, out);
            build_closure_bindings_expr(value, closure_captures, out);
        }
        StmtKind::Expr(e) => {
            build_closure_bindings_expr(e, closure_captures, out);
        }
    }
}

fn build_closure_bindings_expr(
    expr: &Expr,
    closure_captures: &ClosureCaptures,
    out: &mut ClosureBindings,
) {
    match &expr.kind {
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            build_closure_bindings(b, closure_captures, out);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            build_closure_bindings_expr(condition, closure_captures, out);
            build_closure_bindings(then_block, closure_captures, out);
            if let Some(eb) = else_branch {
                build_closure_bindings_expr(eb, closure_captures, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            build_closure_bindings_expr(condition, closure_captures, out);
            build_closure_bindings(body, closure_captures, out);
        }
        ExprKind::For { iterable, body, .. } => {
            build_closure_bindings_expr(iterable, closure_captures, out);
            build_closure_bindings(body, closure_captures, out);
        }
        ExprKind::Match { scrutinee, arms } => {
            build_closure_bindings_expr(scrutinee, closure_captures, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    build_closure_bindings_expr(g, closure_captures, out);
                }
                build_closure_bindings_expr(&arm.body, closure_captures, out);
            }
        }
        ExprKind::Call { callee, args } => {
            build_closure_bindings_expr(callee, closure_captures, out);
            for a in args {
                build_closure_bindings_expr(&a.value, closure_captures, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            build_closure_bindings_expr(object, closure_captures, out);
            for a in args {
                build_closure_bindings_expr(&a.value, closure_captures, out);
            }
        }
        _ => {}
    }
}
