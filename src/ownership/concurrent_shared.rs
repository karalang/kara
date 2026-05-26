//! E_CONCURRENT_SHARED_STRUCT — phase-7 line 197.
//!
//! Detects `shared struct` / `shared enum` bindings that are referenced
//! from two or more top-level statements (branches) of the same `par {}`
//! block. Per design.md § Rc vs Arc — Two-Phase Algorithm "Rule for
//! `shared struct`":
//!
//!   if `live_range(v) ∩ parallel_region ≠ ∅` AND the allocation is
//!   reachable from more than one concurrent branch → compile error.
//!   Sole-ownership move into exactly one branch is not an error.
//!
//! The diagnostic prose mirrors design.md § Compiler-assisted migration
//! from `shared struct` to `par struct`: the four-step rewrite (rename
//! keyword, wrap mut fields in `Mutex[T]`, add `lock` blocks, surface
//! Rc→Arc clone semantics) is included in the suggestion text.
//!
//! Scope (v1 — prose diagnostic, no JSON envelope, no auto-fix):
//! - Direct binding-name references within branch statements count.
//! - Field-access (`shared.field`) and method-call (`shared.method(...)`)
//!   receivers count via the inner Identifier walk.
//! - Closure captures and `spawn(...)` boundaries fall outside the v1
//!   detection — added in a follow-up alongside the broader fix-diff
//!   JSON envelope.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::{OwnershipError, OwnershipErrorKind};

type BindingTypeMap = HashMap<SpanKey, String>;

impl<'a> super::OwnershipChecker<'a> {
    /// Walk every function body, collect bindings whose surface type is
    /// a `shared struct` / `shared enum`, and emit
    /// `E_CONCURRENT_SHARED_STRUCT` for any such binding referenced from
    /// more than one branch (top-level statement) of the same `par {}`
    /// block.
    pub(crate) fn check_concurrent_shared_struct(&mut self) {
        let items: Vec<Item> = self.program.items.clone();
        let mut errors: Vec<OwnershipError> = Vec::new();
        for item in &items {
            match item {
                Item::Function(f) => {
                    let shared = self.collect_shared_bindings(&f.params, &f.body);
                    if !shared.is_empty() {
                        Self::scan_block_for_par_conflicts(&f.body, &shared, &mut errors);
                    }
                }
                Item::ImplBlock(imp) => {
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            let shared = self.collect_shared_bindings(&m.params, &m.body);
                            if !shared.is_empty() {
                                Self::scan_block_for_par_conflicts(&m.body, &shared, &mut errors);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.errors.extend(errors);
    }

    /// Collect bindings (parameters + let-introduced) whose surface type
    /// resolves to a `shared struct` / `shared enum`. The map is keyed by
    /// binding name; values record `(type_name, def_span)` for diagnostic
    /// composition. Function-local — does not look across functions.
    fn collect_shared_bindings(
        &self,
        params: &[Param],
        body: &Block,
    ) -> HashMap<String, (String, Span)> {
        let mut shared = HashMap::new();
        for p in params {
            let Some(name) = p.name() else { continue };
            let Some(head) = type_expr_head_name(&p.ty) else {
                continue;
            };
            if self.is_shared_type(&head) {
                shared.insert(name.to_string(), (head, p.ty.span.clone()));
            }
        }
        collect_let_shared_bindings(
            body,
            &self.typecheck_result.pattern_binding_types,
            &mut shared,
            |n| self.is_shared_type(n),
        );
        shared
    }

    /// Scan `body` for `ExprKind::Par` blocks; for each, walk every
    /// top-level statement (branch) collecting referenced names, and
    /// emit `E_CONCURRENT_SHARED_STRUCT` for any shared binding present
    /// in more than one branch.
    fn scan_block_for_par_conflicts(
        block: &Block,
        shared: &HashMap<String, (String, Span)>,
        errors: &mut Vec<OwnershipError>,
    ) {
        for stmt in &block.stmts {
            scan_stmt_for_par_conflicts(stmt, shared, errors);
        }
        if let Some(e) = &block.final_expr {
            scan_expr_for_par_conflicts(e, shared, errors);
        }
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
/// leaf binding whose `pattern_binding_types`-recorded surface type is
/// a shared struct/enum. Threads through nested blocks so let-bindings
/// in inner scopes are seen too (their visibility extends to nested
/// `par {}` reads, which is exactly what we want to catch).
fn collect_let_shared_bindings(
    block: &Block,
    pattern_binding_types: &BindingTypeMap,
    out: &mut HashMap<String, (String, Span)>,
    is_shared: impl Fn(&str) -> bool + Copy,
) {
    for stmt in &block.stmts {
        collect_let_in_stmt(stmt, pattern_binding_types, out, is_shared);
    }
    if let Some(e) = &block.final_expr {
        collect_let_in_expr(e, pattern_binding_types, out, is_shared);
    }
}

fn collect_let_in_stmt(
    stmt: &Stmt,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, (String, Span)>,
    is_shared: impl Fn(&str) -> bool + Copy,
) {
    match &stmt.kind {
        StmtKind::Let { pattern, value, .. } | StmtKind::LetElse { pattern, value, .. } => {
            record_pattern_bindings(pattern, pbt, out, is_shared);
            collect_let_in_expr(value, pbt, out, is_shared);
            if let StmtKind::LetElse { else_block, .. } = &stmt.kind {
                collect_let_shared_bindings(else_block, pbt, out, is_shared);
            }
        }
        StmtKind::LetUninit { .. } => {
            // Uninitialized bindings are restricted to a single name with
            // a required type annotation. `shared struct` detection via
            // `pattern_binding_types` doesn't apply here (no pattern span
            // is keyed), and uninitialized `shared struct` values are
            // already rejected upstream — nothing to record.
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_let_shared_bindings(body, pbt, out, is_shared);
        }
        StmtKind::Assign { target, value } => {
            collect_let_in_expr(target, pbt, out, is_shared);
            collect_let_in_expr(value, pbt, out, is_shared);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            collect_let_in_expr(target, pbt, out, is_shared);
            collect_let_in_expr(value, pbt, out, is_shared);
        }
        StmtKind::Expr(e) => {
            collect_let_in_expr(e, pbt, out, is_shared);
        }
    }
}

fn record_pattern_bindings(
    pattern: &Pattern,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, (String, Span)>,
    is_shared: impl Fn(&str) -> bool + Copy,
) {
    record_pattern_inner(pattern, pbt, out, is_shared);
}

fn record_pattern_inner(
    pattern: &Pattern,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, (String, Span)>,
    is_shared: impl Fn(&str) -> bool + Copy,
) {
    match &pattern.kind {
        PatternKind::Binding(name) => {
            let key = SpanKey::from_span(&pattern.span);
            if let Some(head) = pbt.get(&key) {
                if is_shared(head) {
                    out.insert(name.clone(), (head.clone(), pattern.span.clone()));
                }
            }
        }
        PatternKind::AtBinding { name, pattern: sub } => {
            let key = SpanKey::from_span(&pattern.span);
            if let Some(head) = pbt.get(&key) {
                if is_shared(head) {
                    out.insert(name.clone(), (head.clone(), pattern.span.clone()));
                }
            }
            record_pattern_inner(sub, pbt, out, is_shared);
        }
        PatternKind::Tuple(items) => {
            for p in items {
                record_pattern_inner(p, pbt, out, is_shared);
            }
        }
        PatternKind::Struct { fields, .. } => {
            for f in fields {
                if let Some(p) = &f.pattern {
                    record_pattern_inner(p, pbt, out, is_shared);
                }
            }
        }
        PatternKind::TupleVariant { patterns, .. } => {
            for p in patterns {
                record_pattern_inner(p, pbt, out, is_shared);
            }
        }
        _ => {}
    }
}

fn collect_let_in_expr(
    expr: &Expr,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, (String, Span)>,
    is_shared: impl Fn(&str) -> bool + Copy,
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
            collect_let_shared_bindings(b, pbt, out, is_shared);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_let_in_expr(condition, pbt, out, is_shared);
            collect_let_shared_bindings(then_block, pbt, out, is_shared);
            if let Some(else_b) = else_branch {
                collect_let_in_expr(else_b, pbt, out, is_shared);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_let_in_expr(condition, pbt, out, is_shared);
            collect_let_shared_bindings(body, pbt, out, is_shared);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_let_in_expr(iterable, pbt, out, is_shared);
            collect_let_shared_bindings(body, pbt, out, is_shared);
        }
        _ => {}
    }
}

fn scan_stmt_for_par_conflicts(
    stmt: &Stmt,
    shared: &HashMap<String, (String, Span)>,
    errors: &mut Vec<OwnershipError>,
) {
    match &stmt.kind {
        StmtKind::Let { value, .. } => {
            scan_expr_for_par_conflicts(value, shared, errors);
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            scan_expr_for_par_conflicts(value, shared, errors);
            for s in &else_block.stmts {
                scan_stmt_for_par_conflicts(s, shared, errors);
            }
            if let Some(e) = &else_block.final_expr {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                scan_stmt_for_par_conflicts(s, shared, errors);
            }
            if let Some(e) = &body.final_expr {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
        }
        StmtKind::Assign { target, value } => {
            scan_expr_for_par_conflicts(target, shared, errors);
            scan_expr_for_par_conflicts(value, shared, errors);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            scan_expr_for_par_conflicts(target, shared, errors);
            scan_expr_for_par_conflicts(value, shared, errors);
        }
        StmtKind::Expr(e) => {
            scan_expr_for_par_conflicts(e, shared, errors);
        }
    }
}

/// At every `ExprKind::Par(block)` encountered, examine the par block's
/// top-level statements (each a "branch") and detect any shared binding
/// referenced in more than one branch. Non-`Par` exprs are walked
/// recursively so nested par blocks are reached.
fn scan_expr_for_par_conflicts(
    expr: &Expr,
    shared: &HashMap<String, (String, Span)>,
    errors: &mut Vec<OwnershipError>,
) {
    match &expr.kind {
        ExprKind::Par(par_body) => {
            detect_par_block_conflicts(par_body, shared, errors);
            for stmt in &par_body.stmts {
                scan_stmt_for_par_conflicts(stmt, shared, errors);
            }
            if let Some(e) = &par_body.final_expr {
                scan_expr_for_par_conflicts(e, shared, errors);
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
                scan_stmt_for_par_conflicts(stmt, shared, errors);
            }
            if let Some(e) = &b.final_expr {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            scan_expr_for_par_conflicts(condition, shared, errors);
            for s in &then_block.stmts {
                scan_stmt_for_par_conflicts(s, shared, errors);
            }
            if let Some(e) = &then_block.final_expr {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
            if let Some(else_b) = else_branch {
                scan_expr_for_par_conflicts(else_b, shared, errors);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            scan_expr_for_par_conflicts(condition, shared, errors);
            for s in &body.stmts {
                scan_stmt_for_par_conflicts(s, shared, errors);
            }
            if let Some(e) = &body.final_expr {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
        }
        ExprKind::For { iterable, body, .. } => {
            scan_expr_for_par_conflicts(iterable, shared, errors);
            for s in &body.stmts {
                scan_stmt_for_par_conflicts(s, shared, errors);
            }
            if let Some(e) = &body.final_expr {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            scan_expr_for_par_conflicts(scrutinee, shared, errors);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    scan_expr_for_par_conflicts(g, shared, errors);
                }
                scan_expr_for_par_conflicts(&arm.body, shared, errors);
            }
        }
        ExprKind::Call { callee, args } => {
            scan_expr_for_par_conflicts(callee, shared, errors);
            for a in args {
                scan_expr_for_par_conflicts(&a.value, shared, errors);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            scan_expr_for_par_conflicts(object, shared, errors);
            for a in args {
                scan_expr_for_par_conflicts(&a.value, shared, errors);
            }
        }
        ExprKind::FieldAccess { object, .. } => {
            scan_expr_for_par_conflicts(object, shared, errors);
        }
        ExprKind::Index { object, index } => {
            scan_expr_for_par_conflicts(object, shared, errors);
            scan_expr_for_par_conflicts(index, shared, errors);
        }
        ExprKind::Binary { left, right, .. } => {
            scan_expr_for_par_conflicts(left, shared, errors);
            scan_expr_for_par_conflicts(right, shared, errors);
        }
        ExprKind::Unary { operand, .. } => {
            scan_expr_for_par_conflicts(operand, shared, errors);
        }
        ExprKind::Tuple(items) => {
            for e in items {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
        }
        ExprKind::Return(inner) => {
            if let Some(e) = inner.as_deref() {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value.as_deref() {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
        }
        ExprKind::Cast { expr, .. } => {
            scan_expr_for_par_conflicts(expr, shared, errors);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                scan_expr_for_par_conflicts(s, shared, errors);
            }
            if let Some(e) = end.as_deref() {
                scan_expr_for_par_conflicts(e, shared, errors);
            }
        }
        _ => {}
    }
}

/// Walk each top-level statement of `par_body` collecting the set of
/// shared-binding names referenced anywhere in that branch's subtree.
/// Any binding appearing in two or more branches → emit
/// `E_CONCURRENT_SHARED_STRUCT` at the second-branch use site, with the
/// first-branch use threaded as the `consume_span` secondary.
fn detect_par_block_conflicts(
    par_body: &Block,
    shared: &HashMap<String, (String, Span)>,
    errors: &mut Vec<OwnershipError>,
) {
    // For each binding name, record the (branch_idx, first-use Span)
    // for the first branch that uses it. When a later branch uses it
    // too, emit the diagnostic immediately. Reporting only the first
    // pair keeps the diagnostic output focused — `karac` aims for one
    // error per concurrent-misuse, not a cascade.
    let mut first_use: HashMap<String, (usize, Span)> = HashMap::new();
    let mut reported: HashSet<String> = HashSet::new();

    for (branch_idx, stmt) in par_body.stmts.iter().enumerate() {
        let mut uses: HashMap<String, Span> = HashMap::new();
        collect_identifier_uses_in_stmt(stmt, shared, &mut uses);
        for (name, use_span) in uses {
            if reported.contains(&name) {
                continue;
            }
            match first_use.get(&name) {
                Some((prev_idx, prev_span)) if *prev_idx != branch_idx => {
                    let (type_name, _def_span) = &shared[&name];
                    errors.push(build_concurrent_shared_struct_error(
                        &name,
                        type_name,
                        use_span,
                        prev_span.clone(),
                    ));
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

fn build_concurrent_shared_struct_error(
    binding: &str,
    type_name: &str,
    second_use_span: Span,
    first_use_span: Span,
) -> OwnershipError {
    let message = format!(
        "shared struct `{}` cannot be accessed from multiple concurrent tasks (binding `{}` reachable from two par-block branches: first use at line {}:{}, conflicting use here)",
        type_name, binding, first_use_span.line, first_use_span.column,
    );
    let suggestion = format!(
        "convert `{ty}` to `par struct` and wrap mut fields in `Mutex[T]`/`Atomic[T]`. The migration is structural:\n  1. rename `shared struct {ty}` to `par struct {ty}`\n  2. wrap each bare `mut` field in `Mutex[T]` (refine to `Atomic[T]` post-review where lock-free access is appropriate)\n  3. insert `lock field {{ ... }}` blocks at every write site within `par` regions\n  4. call sites that previously relied on implicit `Rc`-clone now produce `Arc`-clone semantics\nor run `karac migrate shared-to-par {ty}` for a preemptive workspace rewrite.",
        ty = type_name,
    );
    OwnershipError {
        message,
        span: second_use_span,
        kind: OwnershipErrorKind::ConcurrentSharedStruct {
            type_name: type_name.to_string(),
            binding: binding.to_string(),
        },
        suggestion: Some(suggestion),
        replacement: None,
        consume_span: Some(first_use_span),
    }
}

fn collect_identifier_uses_in_stmt(
    stmt: &Stmt,
    shared: &HashMap<String, (String, Span)>,
    out: &mut HashMap<String, Span>,
) {
    match &stmt.kind {
        StmtKind::Let { value, .. } => {
            collect_identifier_uses_in_expr(value, shared, out);
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            collect_identifier_uses_in_expr(value, shared, out);
            for s in &else_block.stmts {
                collect_identifier_uses_in_stmt(s, shared, out);
            }
            if let Some(e) = &else_block.final_expr {
                collect_identifier_uses_in_expr(e, shared, out);
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                collect_identifier_uses_in_stmt(s, shared, out);
            }
            if let Some(e) = &body.final_expr {
                collect_identifier_uses_in_expr(e, shared, out);
            }
        }
        StmtKind::Assign { target, value } => {
            collect_identifier_uses_in_expr(target, shared, out);
            collect_identifier_uses_in_expr(value, shared, out);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            collect_identifier_uses_in_expr(target, shared, out);
            collect_identifier_uses_in_expr(value, shared, out);
        }
        StmtKind::Expr(e) => {
            collect_identifier_uses_in_expr(e, shared, out);
        }
    }
}

fn collect_identifier_uses_in_expr(
    expr: &Expr,
    shared: &HashMap<String, (String, Span)>,
    out: &mut HashMap<String, Span>,
) {
    match &expr.kind {
        ExprKind::Identifier(name) if shared.contains_key(name) => {
            out.entry(name.clone()).or_insert_with(|| expr.span.clone());
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
                collect_identifier_uses_in_stmt(s, shared, out);
            }
            if let Some(e) = &b.final_expr {
                collect_identifier_uses_in_expr(e, shared, out);
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_identifier_uses_in_expr(condition, shared, out);
            for s in &then_block.stmts {
                collect_identifier_uses_in_stmt(s, shared, out);
            }
            if let Some(e) = &then_block.final_expr {
                collect_identifier_uses_in_expr(e, shared, out);
            }
            if let Some(eb) = else_branch {
                collect_identifier_uses_in_expr(eb, shared, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_identifier_uses_in_expr(condition, shared, out);
            for s in &body.stmts {
                collect_identifier_uses_in_stmt(s, shared, out);
            }
            if let Some(e) = &body.final_expr {
                collect_identifier_uses_in_expr(e, shared, out);
            }
        }
        ExprKind::For { iterable, body, .. } => {
            collect_identifier_uses_in_expr(iterable, shared, out);
            for s in &body.stmts {
                collect_identifier_uses_in_stmt(s, shared, out);
            }
            if let Some(e) = &body.final_expr {
                collect_identifier_uses_in_expr(e, shared, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_identifier_uses_in_expr(scrutinee, shared, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_identifier_uses_in_expr(g, shared, out);
                }
                collect_identifier_uses_in_expr(&arm.body, shared, out);
            }
        }
        ExprKind::Call { callee, args } => {
            collect_identifier_uses_in_expr(callee, shared, out);
            for a in args {
                collect_identifier_uses_in_expr(&a.value, shared, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_identifier_uses_in_expr(object, shared, out);
            for a in args {
                collect_identifier_uses_in_expr(&a.value, shared, out);
            }
        }
        ExprKind::FieldAccess { object, .. } => {
            collect_identifier_uses_in_expr(object, shared, out);
        }
        ExprKind::Index { object, index } => {
            collect_identifier_uses_in_expr(object, shared, out);
            collect_identifier_uses_in_expr(index, shared, out);
        }
        ExprKind::Binary { left, right, .. } => {
            collect_identifier_uses_in_expr(left, shared, out);
            collect_identifier_uses_in_expr(right, shared, out);
        }
        ExprKind::Unary { operand, .. } => {
            collect_identifier_uses_in_expr(operand, shared, out);
        }
        ExprKind::Tuple(items) => {
            for e in items {
                collect_identifier_uses_in_expr(e, shared, out);
            }
        }
        ExprKind::Return(inner) => {
            if let Some(e) = inner.as_deref() {
                collect_identifier_uses_in_expr(e, shared, out);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value.as_deref() {
                collect_identifier_uses_in_expr(e, shared, out);
            }
        }
        ExprKind::Cast { expr, .. } => {
            collect_identifier_uses_in_expr(expr, shared, out);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                collect_identifier_uses_in_expr(s, shared, out);
            }
            if let Some(e) = end.as_deref() {
                collect_identifier_uses_in_expr(e, shared, out);
            }
        }
        _ => {}
    }
}
