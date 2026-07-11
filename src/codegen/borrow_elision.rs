//! Borrow-elision analysis for read-only `let r = v[i]` indexed-element
//! bindings (B-2026-06-19-6, clone-elision).
//!
//! A `let r = v[i]` that binds a HEAP-owning element out of a `Vec` deep-clones
//! the element (`clone_owned_vec_index_element`) so the binding owns a distinct
//! buffer — the safe default that guards the alias double-free / dangling-after-
//! container-drop class (B-2026-06-14-11, B-2026-06-18-9). When `r` is only read
//! and never escapes, and the container `v` is not mutated while `r` is live, the
//! clone is observationally redundant: `r` can alias the element buffer and the
//! container stays the unique owner. This pass finds exactly those bindings.
//!
//! The analysis is deliberately CONSERVATIVE and whitelist-based: a binding is
//! marked borrow-safe only when every use of `r` and `v` within `r`'s lexical
//! scope is a recognised read. Anything unrecognised — any non-read use of `r`,
//! any mention of `v` outside an index-read, any closure (which could capture and
//! mutate or escape either name), any shadow-rebind — disqualifies the binding,
//! falling back to the existing clone. False negatives merely keep the clone;
//! only a false positive would be a use-after-free, so the gate errs entirely
//! toward "clone".
//!
//! Scope model: `r`'s lexical scope is the statements that follow the `let` in
//! the SAME block, plus that block's trailing expression. Nested blocks inside
//! those statements are in scope and are walked; sibling/outer blocks that close
//! before `r`'s block ends are not. A mutation of `v` that lexically PRECEDES the
//! `let` (even on a loop back-edge) is safe — `r` re-reads `v[i]` after it each
//! iteration — so only the post-`let` tail is scanned.

use crate::ast::{Block, Expr, ExprKind, Pattern, PatternKind, Stmt, StmtKind};
use crate::resolver::SpanKey;
use std::collections::HashSet;

/// Read-only methods on a `Vec`/`String` receiver that neither mutate, move, nor
/// hand out a buffer pointer that could outlive the borrow. Kept intentionally
/// tiny; any other method on `r` or `v` disqualifies the binding.
fn is_read_only_vec_method(method: &str) -> bool {
    matches!(method, "len" | "is_empty")
}

/// Element-copyability oracle: given a container variable name `v`, returns
/// `(v_elem_heap, v_elem_elem_heap)` — whether `v[i]` and `v[i][j]` are
/// non-trivially-copyable (heap/RC-owning) values. Supplied by codegen (which
/// alone has the element types); `(false, false)` for an unknown name conserves
/// the prior borrow-eliding behaviour. Used to decide whether a `r[idx]` call
/// argument is a genuine ownership CONSUME (heap element → disqualify the
/// borrow) or a trivially-copyable read (`i64` → keep the borrow).
pub(crate) type HeapElemOracle<'o> = dyn Fn(&str) -> (bool, bool) + 'o;

/// Entry point: walk a function body and return the set of `SpanKey`s (keyed by
/// the RHS index expression) for every `let r = v[i]` binding that is safe to
/// bind as a borrow instead of a deep clone.
pub(crate) fn compute_vec_index_borrow_spans(
    body: &Block,
    heap_elem: &HeapElemOracle<'_>,
) -> HashSet<SpanKey> {
    let mut out = HashSet::new();
    scan_block(body, &mut out, heap_elem);
    out
}

/// Walk every block in the body, and at each `let r = v[i]` candidate run the
/// conservative gate over `r`'s scope (the rest of the enclosing block).
fn scan_block(block: &Block, out: &mut HashSet<SpanKey>, heap_elem: &HeapElemOracle<'_>) {
    for (idx, stmt) in block.stmts.iter().enumerate() {
        if let Some((r_name, v_name, rhs_span)) = candidate_binding(stmt) {
            let (v_elem_heap, r_elem_heap) = heap_elem(&v_name);
            if binding_is_borrow_safe(
                &r_name,
                &v_name,
                r_elem_heap,
                v_elem_heap,
                &block.stmts[idx + 1..],
                &block.final_expr,
            ) {
                out.insert(SpanKey::from_span(&rhs_span));
            }
        }
        // Recurse into nested blocks reachable from this statement so candidates
        // declared inside loops / conditionals / matches are also analysed.
        stmt_walk_nested_blocks(stmt, out, heap_elem);
    }
    if let Some(fe) = &block.final_expr {
        expr_walk_nested_blocks(fe, out, heap_elem);
    }
}

/// Recognise `let r = v[i]` with a simple immutable binding, a bare-identifier
/// container, and a non-range element index. Returns `(r, v, rhs_index_span)`.
fn candidate_binding(stmt: &Stmt) -> Option<(String, String, crate::token::Span)> {
    let StmtKind::Let {
        is_mut,
        pattern,
        value,
        ..
    } = &stmt.kind
    else {
        return None;
    };
    if *is_mut {
        return None;
    }
    let PatternKind::Binding(r_name) = &pattern.kind else {
        return None;
    };
    let ExprKind::Index { object, index } = &value.kind else {
        return None;
    };
    if matches!(&index.kind, ExprKind::Range { .. }) {
        return None;
    }
    let ExprKind::Identifier(v_name) = &object.kind else {
        return None;
    };
    if r_name == v_name {
        return None;
    }
    Some((r_name.clone(), v_name.clone(), value.span.clone()))
}

/// Conservative gate: every occurrence of `r` and `v` in the binding's scope
/// (the trailing statements + final expr) must be a recognised read.
fn binding_is_borrow_safe(
    r: &str,
    v: &str,
    r_elem_heap: bool,
    v_elem_heap: bool,
    rest: &[Stmt],
    final_expr: &Option<Box<Expr>>,
) -> bool {
    let mut ctx = ScanCtx {
        r,
        v,
        r_elem_heap,
        v_elem_heap,
        disqualified: false,
    };
    for stmt in rest {
        ctx.scan_stmt(stmt);
        if ctx.disqualified {
            return false;
        }
    }
    if let Some(fe) = final_expr {
        ctx.scan_expr(fe);
    }
    !ctx.disqualified
}

struct ScanCtx<'a> {
    r: &'a str,
    v: &'a str,
    /// `r[i]`'s element type is non-trivially-copyable (heap/RC). `r == v[k]`,
    /// so this is `v`'s element type peeled one more Vec layer.
    r_elem_heap: bool,
    /// `v[i]`'s element type is non-trivially-copyable (heap/RC).
    v_elem_heap: bool,
    disqualified: bool,
}

impl ScanCtx<'_> {
    fn is_target(&self, name: &str) -> bool {
        name == self.r || name == self.v
    }

    /// True when `expr` is a call argument `r[idx]` / `v[idx]` (a plain
    /// element index of a target binding, non-range) whose ELEMENT TYPE is
    /// non-trivially-copyable — a heap/RC-owning value (`Option[shared]`,
    /// `shared`, `String`, `Vec[..]`, …). Passing such an element by value
    /// hands the callee ownership, and the callee's scope-exit drop frees a
    /// buffer the container still owns — so borrow-eliding the binding into a
    /// shallow alias double-frees (B-2026-07-11-24). A trivially-Copy element
    /// (`r[i] -> i64`, e.g. the desugared operand of a checked `+`) is a pure
    /// read and must NOT disqualify — hence the `heap`-flag gate, resolved by
    /// codegen (which alone has the element types) and cached per binding.
    fn arg_is_consumed_heap_element(&self, expr: &Expr) -> bool {
        let ExprKind::Index { object, index } = &expr.kind else {
            return false;
        };
        if matches!(&index.kind, ExprKind::Range { .. }) {
            return false;
        }
        match &object.kind {
            ExprKind::Identifier(name) if name == self.r => self.r_elem_heap,
            ExprKind::Identifier(name) if name == self.v => self.v_elem_heap,
            _ => false,
        }
    }

    fn scan_stmt(&mut self, stmt: &Stmt) {
        if self.disqualified {
            return;
        }
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { pattern, value, .. } => {
                // A shadow-rebind of either name changes what the name refers to
                // for the remaining scope — bail rather than reason about it.
                if pattern_binds_target(pattern, self.r) || pattern_binds_target(pattern, self.v) {
                    self.disqualified = true;
                    return;
                }
                self.scan_expr(value);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                if pattern_binds_target(pattern, self.r) || pattern_binds_target(pattern, self.v) {
                    self.disqualified = true;
                    return;
                }
                self.scan_expr(value);
                self.scan_block(else_block);
            }
            StmtKind::LetUninit { name, .. } => {
                if self.is_target(name) {
                    self.disqualified = true;
                }
            }
            StmtKind::Assign { target, value } => {
                // Any write whose place mentions r or v is unsafe (element store,
                // reassignment, …). `r[i] = x` / `v[i] = x` / `r = x` all caught.
                if expr_mentions_name(target, self.r) || expr_mentions_name(target, self.v) {
                    self.disqualified = true;
                    return;
                }
                self.scan_expr(value);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                if expr_mentions_name(target, self.r) || expr_mentions_name(target, self.v) {
                    self.disqualified = true;
                    return;
                }
                self.scan_expr(value);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.scan_block(body);
            }
            StmtKind::Expr(e) => self.scan_expr(e),
        }
    }

    fn scan_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.scan_stmt(stmt);
            if self.disqualified {
                return;
            }
        }
        if let Some(fe) = &block.final_expr {
            self.scan_expr(fe);
        }
    }

    /// Walk an expression. Two parent forms consume a target identifier as a
    /// safe READ without descending into the bare identifier: `name[idx]` (read)
    /// and `name.len()` / `name.is_empty()`. Every other path recurses
    /// generically, and a bare `Identifier(r|v)` reached that way disqualifies.
    fn scan_expr(&mut self, expr: &Expr) {
        if self.disqualified {
            return;
        }
        match &expr.kind {
            // Safe read: `name[idx]` as an rvalue (assignment targets are handled
            // in `scan_stmt`, so any Index reached here is a read). Recurse only
            // into the index expression, not the receiver identifier.
            ExprKind::Index { object, index } => {
                if let ExprKind::Identifier(name) = &object.kind {
                    if self.is_target(name) {
                        self.scan_expr(index);
                        return;
                    }
                }
                self.scan_expr(object);
                self.scan_expr(index);
            }
            // A call argument `r[idx]` is a CONSUME, not a read: passing an
            // element by value hands the callee ownership of that element, and
            // for a heap/RC element type (`Vec[Option[shared]]`, `Vec[shared]`,
            // `Vec[String]`, …) the callee's scope-exit drop decrements/frees a
            // buffer the container `v` still owns — so borrow-eliding `r` into a
            // shallow alias double-frees. (B-2026-07-11-24: `clone_offset(lefts[a], ..)`
            // over a `let lefts = shapes[i]` inner Vec of `Option[shared]` — the
            // Index arm below would otherwise treat `lefts[a]` as a safe read and
            // shallow-alias, corrupting the shared refcounts.) Disqualify so the
            // binding falls back to the deep clone (`clone_owned_vec_index_element`),
            // which gives `r` an independent buffer with its elements retained.
            // A trivially-Copy element (`v[i] -> i64`) would deep-"clone" to a
            // no-op, so this only ever forces a real clone where one is needed.
            ExprKind::Call { callee, args } => {
                self.scan_expr(callee);
                for a in args {
                    if self.arg_is_consumed_heap_element(&a.value) {
                        self.disqualified = true;
                        return;
                    }
                    self.scan_expr(&a.value);
                }
            }
            // Safe read: `name.len()` / `name.is_empty()` (no args). Any other
            // method, or any args, falls through to the generic walk where the
            // bare receiver identifier disqualifies. A `r[idx]` argument to any
            // method call is a consume, same as a free-fn call arg above.
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                if args.is_empty() && is_read_only_vec_method(method) {
                    if let ExprKind::Identifier(name) = &object.kind {
                        if self.is_target(name) {
                            return;
                        }
                    }
                }
                self.scan_expr(object);
                for a in args {
                    if self.arg_is_consumed_heap_element(&a.value) {
                        self.disqualified = true;
                        return;
                    }
                    self.scan_expr(&a.value);
                }
            }
            // A closure could capture r or v by reference and mutate them or let
            // them escape — out of scope for this conservative pass.
            ExprKind::Closure { .. } => {
                self.disqualified = true;
            }
            // Bare use of a target in any other position (call arg, return,
            // operand, composite literal, for-loop iterable, …) is not a
            // recognised read.
            ExprKind::Identifier(name) => {
                if self.is_target(name) {
                    self.disqualified = true;
                }
            }
            _ => self.walk_children(expr),
        }
    }

    /// Generic structural recursion for every `ExprKind` not special-cased in
    /// `scan_expr`. Visits every nested expression and block. Any construct that
    /// rebinds `r` or `v` with a fresh pattern (match arm / if-let / while-let /
    /// for) disqualifies, since the inner scope's uses would refer to the shadow.
    fn walk_children(&mut self, expr: &Expr) {
        match &expr.kind {
            // Leaves — no target identifier possible.
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Path { .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Return(None)
            | ExprKind::Continue { .. }
            | ExprKind::Error => {}
            // Special-cased in scan_expr — reached only if routed here directly.
            ExprKind::Identifier(_)
            | ExprKind::Index { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::Closure { .. } => self.scan_expr(expr),
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e) = p {
                        self.scan_expr(e);
                    }
                }
            }
            ExprKind::Binary { left, right, .. } | ExprKind::NilCoalesce { left, right } => {
                self.scan_expr(left);
                self.scan_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.scan_expr(operand),
            ExprKind::Question(e) | ExprKind::Cast { expr: e, .. } => self.scan_expr(e),
            ExprKind::OptionalChain { object, args, .. } => {
                self.scan_expr(object);
                if let Some(args) = args {
                    for a in args {
                        self.scan_expr(&a.value);
                    }
                }
            }
            ExprKind::Call { callee, args, .. } => {
                self.scan_expr(callee);
                for a in args {
                    self.scan_expr(&a.value);
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.scan_expr(object)
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b)
            | ExprKind::Loop { body: b, .. }
            | ExprKind::LabeledBlock { body: b, .. } => self.scan_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.scan_expr(condition);
                self.scan_block(then_block);
                if let Some(eb) = else_branch {
                    self.scan_expr(eb);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                if self.pattern_shadows(pattern) {
                    return;
                }
                self.scan_expr(value);
                self.scan_block(then_block);
                if let Some(eb) = else_branch {
                    self.scan_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.scan_expr(scrutinee);
                for arm in arms {
                    if self.pattern_shadows(&arm.pattern) {
                        return;
                    }
                    if let Some(g) = &arm.guard {
                        self.scan_expr(g);
                    }
                    self.scan_expr(&arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.scan_expr(condition);
                self.scan_block(body);
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                if self.pattern_shadows(pattern) {
                    return;
                }
                self.scan_expr(value);
                self.scan_block(body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                if self.pattern_shadows(pattern) {
                    return;
                }
                self.scan_expr(iterable);
                self.scan_block(body);
            }
            ExprKind::Return(Some(e)) => self.scan_expr(e),
            ExprKind::Break { value: Some(e), .. } => self.scan_expr(e),
            ExprKind::Break { .. } => {}
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for e in items {
                    self.scan_expr(e);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.scan_expr(e);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.scan_expr(value);
                self.scan_expr(count);
            }
            ExprKind::MapLiteral(entries) => {
                for (k, val) in entries {
                    self.scan_expr(k);
                    self.scan_expr(val);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.scan_expr(&f.value);
                }
                if let Some(b) = spread {
                    self.scan_expr(b);
                }
            }
            ExprKind::Pipe { left, right } => {
                self.scan_expr(left);
                self.scan_expr(right);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.scan_expr(s);
                }
                if let Some(e) = end {
                    self.scan_expr(e);
                }
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.scan_expr(mutex);
                self.scan_block(body);
            }
            // `providers { … } in { … }` and any future variant not explicitly
            // enumerated: disqualify rather than risk missing a target use.
            ExprKind::Providers { .. } => self.disqualified = true,
        }
    }

    /// `true` (and disqualifies) if a binding pattern rebinds `r` or `v`.
    fn pattern_shadows(&mut self, pattern: &Pattern) -> bool {
        if pattern_binds_target(pattern, self.r) || pattern_binds_target(pattern, self.v) {
            self.disqualified = true;
            return true;
        }
        false
    }
}

/// `true` if `pattern` binds the given name anywhere (simple or destructured).
fn pattern_binds_target(pattern: &Pattern, name: &str) -> bool {
    pattern.binding_names().iter().any(|n| n == name)
}

/// `true` if `name` appears anywhere in `expr` (used for assignment-target place
/// checks, where ANY mention of the borrowed names is unsafe).
fn expr_mentions_name(expr: &Expr, name: &str) -> bool {
    let mut found = false;
    mentions_rec(expr, name, &mut found);
    found
}

fn mentions_rec(expr: &Expr, name: &str, found: &mut bool) {
    if *found {
        return;
    }
    if let ExprKind::Identifier(n) = &expr.kind {
        if n == name {
            *found = true;
            return;
        }
    }
    // A shallow structural walk is enough for assignment targets (places are
    // identifiers, field accesses, and index chains), but cover the common
    // nested forms so a target like `m[r]` or `a.b[v]` is caught.
    match &expr.kind {
        ExprKind::Index { object, index } => {
            mentions_rec(object, name, found);
            mentions_rec(index, name, found);
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            mentions_rec(object, name, found)
        }
        ExprKind::Unary { operand, .. } => mentions_rec(operand, name, found),
        _ => {}
    }
}

/// Recurse into nested blocks of a statement so `scan_block`'s candidate search
/// reaches `let r = v[i]` bindings declared inside loops / conditionals.
fn stmt_walk_nested_blocks(
    stmt: &Stmt,
    out: &mut HashSet<SpanKey>,
    heap_elem: &HeapElemOracle<'_>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. } => expr_walk_nested_blocks(value, out, heap_elem),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            expr_walk_nested_blocks(value, out, heap_elem);
            scan_block(else_block, out, heap_elem);
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            expr_walk_nested_blocks(target, out, heap_elem);
            expr_walk_nested_blocks(value, out, heap_elem);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            scan_block(body, out, heap_elem)
        }
        StmtKind::Expr(e) => expr_walk_nested_blocks(e, out, heap_elem),
        StmtKind::LetUninit { .. } => {}
    }
}

/// Recurse into nested blocks of an expression for the candidate search.
fn expr_walk_nested_blocks(
    expr: &Expr,
    out: &mut HashSet<SpanKey>,
    heap_elem: &HeapElemOracle<'_>,
) {
    match &expr.kind {
        ExprKind::Block(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. }
        | ExprKind::Providers { body: b, .. } => scan_block(b, out, heap_elem),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            expr_walk_nested_blocks(condition, out, heap_elem);
            scan_block(then_block, out, heap_elem);
            if let Some(eb) = else_branch {
                expr_walk_nested_blocks(eb, out, heap_elem);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            expr_walk_nested_blocks(value, out, heap_elem);
            scan_block(then_block, out, heap_elem);
            if let Some(eb) = else_branch {
                expr_walk_nested_blocks(eb, out, heap_elem);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            expr_walk_nested_blocks(condition, out, heap_elem);
            scan_block(body, out, heap_elem);
        }
        ExprKind::WhileLet { value, body, .. } => {
            expr_walk_nested_blocks(value, out, heap_elem);
            scan_block(body, out, heap_elem);
        }
        ExprKind::For { iterable, body, .. } => {
            expr_walk_nested_blocks(iterable, out, heap_elem);
            scan_block(body, out, heap_elem);
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_walk_nested_blocks(scrutinee, out, heap_elem);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    expr_walk_nested_blocks(g, out, heap_elem);
                }
                expr_walk_nested_blocks(&arm.body, out, heap_elem);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            expr_walk_nested_blocks(left, out, heap_elem);
            expr_walk_nested_blocks(right, out, heap_elem);
        }
        ExprKind::Call { callee, args, .. } => {
            expr_walk_nested_blocks(callee, out, heap_elem);
            for a in args {
                expr_walk_nested_blocks(&a.value, out, heap_elem);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            expr_walk_nested_blocks(object, out, heap_elem);
            for a in args {
                expr_walk_nested_blocks(&a.value, out, heap_elem);
            }
        }
        _ => {}
    }
}
