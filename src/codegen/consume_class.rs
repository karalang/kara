//! Use-site consumption classifier — Phase 0 foundation for the caller-retains
//! parameter model (`docs/spikes/caller-retains-param-model.md`).
//!
//! Codegen frees every owned heap allocation via the scope-drop of its owning
//! binding, UNLESS that binding's ownership is transferred away (it escapes the
//! frame or is moved into a new owner that will free it). Under this compiler's
//! convention, **passing a value to a user function is not a transfer**: the
//! callee entry-deep-copies an owned aggregate param, or defensively copies an
//! owned `Vec`/`String` at its own consume sites — so the caller retains and
//! frees the original. The scattered per-shape cleanup suppressors re-derive
//! "does this use transfer ownership?" with local heuristics; two of them
//! (B-2026-07-03-28, B-2026-07-03-31) get it wrong on specific payload shapes.
//!
//! This module provides ONE predicate they can share:
//! [`binding_only_borrowed`] — does a bound variable appear ONLY in
//! non-consuming (borrow / entry-copied free-fn call-arg) positions within an
//! expression? It is deliberately CONSERVATIVE: it returns `true` only when
//! every occurrence is provably non-consuming, and treats any unknown or
//! transferring position as consuming. That bias is load-bearing — a false
//! "only-borrowed" would drop a cleanup suppression and risk a DOUBLE-FREE,
//! whereas a false "consumed" only keeps today's (at worst leaking) behavior.
//!
//! Phase 1 wires [`binding_only_borrowed`] into
//! `suppress_inline_option_agg_payload_cleanup` (the B-31 site); Phase 2 reuses
//! `classify_binding_in_expr` for the shared-owning-struct rc-transfer decision
//! (B-28).

use crate::ast::{Expr, ExprKind, Stmt, StmtKind};

/// Consumption verdict for a use-site. `NonConsuming` means the source retains
/// ownership (borrow, or an argument entry-copied by the callee); `Consumed`
/// means ownership transfers to a new owner that will free it (return, an
/// aggregate/collection element, a container mutator, a store, a move-binding,
/// or a closure capture).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Consumption {
    NonConsuming,
    Consumed,
}

/// True iff EVERY occurrence of `name` within `e` is in a provably
/// non-consuming position — i.e. `classify_binding_in_expr` is `NonConsuming`.
/// The conservative default is `Consumed`, so this returns `true` only for the
/// confidently-borrow shapes.
pub(crate) fn binding_only_borrowed(name: &str, e: &Expr) -> bool {
    classify_binding_in_expr(name, e) == Consumption::NonConsuming
}

/// Block sibling of [`binding_only_borrowed`], for the if-let `then_block` /
/// while-let `body` scopes where the binding lives directly in a `Block` rather
/// than a single arm expression. A block's value is its `final_expr`, so a
/// `final_expr` that forwards `name` (the block result escapes) is a transfer,
/// as is any consuming sink among its statements.
pub(crate) fn binding_only_borrowed_block(name: &str, b: &crate::ast::Block) -> bool {
    let consumed = b
        .final_expr
        .as_deref()
        .is_some_and(|t| value_derived_from(name, t))
        || block_has_sink(name, b);
    !consumed
}

/// Classify how `name` is used across the whole of `e`. `Consumed` if ANY use
/// transfers ownership; `NonConsuming` only if every use is a borrow or an
/// entry-copied free-function call argument.
pub(crate) fn classify_binding_in_expr(name: &str, e: &Expr) -> Consumption {
    // The value of `e` itself flowing out (the arm/expression result) is a
    // transfer if it is derived from `name` (the bare binding, or a
    // field/tuple/index projection rooted at it — a partial move).
    if value_derived_from(name, e) {
        return Consumption::Consumed;
    }
    if has_consuming_sink(name, e) {
        return Consumption::Consumed;
    }
    Consumption::NonConsuming
}

/// Does the VALUE of `e` carry `name`'s ownership directly — the bare binding,
/// a projection (`v.field` / `v.0` / `v[i]`) rooted at it, or such a value
/// forwarded through a value-transparent wrapper (block tail, `if`/`match`
/// arms)? Crucially this does NOT see through a call, method call, or operator:
/// those produce a fresh/entry-copied value that no longer aliases `name`.
fn value_derived_from(name: &str, e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Identifier(n) => n == name,
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => value_derived_from(name, object),
        ExprKind::Block(b) => b
            .final_expr
            .as_deref()
            .is_some_and(|t| value_derived_from(name, t)),
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
            then_block
                .final_expr
                .as_deref()
                .is_some_and(|t| value_derived_from(name, t))
                || else_branch
                    .as_deref()
                    .is_some_and(|t| value_derived_from(name, t))
        }
        ExprKind::Match { arms, .. } => arms.iter().any(|a| value_derived_from(name, &a.body)),
        _ => false,
    }
}

/// Is `name` transferred at some CONSUMING SINK anywhere inside `e`? A sink is a
/// position that hands ownership to a new owner: `return`, an aggregate or
/// collection literal element, an enum-variant / capitalized / path-callee
/// construction argument, a method-call argument (conservatively — covers
/// container mutators like `push`/`insert`), a store or move-binding RHS, or a
/// closure capture. Each sink checks `value_derived_from` on its operand; the
/// walk recurses into non-consuming children (free-fn call args, receivers,
/// operator operands) to catch sinks nested inside them.
fn has_consuming_sink(name: &str, e: &Expr) -> bool {
    let derived = |x: &Expr| value_derived_from(name, x);
    match &e.kind {
        // ── Sinks ──────────────────────────────────────────────────────────
        ExprKind::Return(inner) => {
            inner.as_deref().is_some_and(derived)
                || inner
                    .as_deref()
                    .is_some_and(|x| has_consuming_sink(name, x))
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            fields.iter().any(|f| derived(&f.value))
                || spread.as_deref().is_some_and(derived)
                || fields.iter().any(|f| has_consuming_sink(name, &f.value))
                || spread
                    .as_deref()
                    .is_some_and(|s| has_consuming_sink(name, s))
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            items.iter().any(derived) || items.iter().any(|x| has_consuming_sink(name, x))
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            items.iter().any(derived) || items.iter().any(|x| has_consuming_sink(name, x))
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            derived(value) || has_consuming_sink(name, value) || has_consuming_sink(name, count)
        }
        ExprKind::MapLiteral(pairs) => pairs.iter().any(|(k, v)| {
            derived(k) || derived(v) || has_consuming_sink(name, k) || has_consuming_sink(name, v)
        }),
        // A `Call` is a free-function call ONLY when its callee is a bare
        // lowercase-ish identifier. Anything else (a `Path` such as
        // `E.Variant`, or a capitalized constructor) is treated as a
        // CONSUMING construction — the safe bias, since misreading a
        // constructor as a borrow would double-free.
        ExprKind::Call { callee, args } => {
            let is_free_fn = matches!(&callee.kind, ExprKind::Identifier(_));
            if is_free_fn {
                // Entry-copied args: a derived arg is fine; only recurse for
                // nested sinks.
                args.iter().any(|a| has_consuming_sink(name, &a.value))
                    || has_consuming_sink(name, callee)
            } else {
                args.iter().any(|a| derived(&a.value))
                    || args.iter().any(|a| has_consuming_sink(name, &a.value))
                    || has_consuming_sink(name, callee)
            }
        }
        // Method calls: the RECEIVER is a borrow (non-consuming), but an
        // ARGUMENT is conservatively a transfer — this is what covers
        // `v.push(x)` / `m.insert(k, x)` without an allowlist of mutators.
        ExprKind::MethodCall { object, args, .. } => {
            args.iter().any(|a| derived(&a.value))
                || has_consuming_sink(name, object)
                || args.iter().any(|a| has_consuming_sink(name, &a.value))
        }
        // A closure that references `name` captures it (by value/move under the
        // heap-env model) — a transfer out of the current control flow.
        ExprKind::Closure { body, .. } => expr_mentions(name, body),
        // ── Non-consuming reads: recurse for nested sinks only ─────────────
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Unary {
            operand: object, ..
        }
        | ExprKind::Cast { expr: object, .. } => has_consuming_sink(name, object),
        ExprKind::Index { object, index } => {
            has_consuming_sink(name, object) || has_consuming_sink(name, index)
        }
        ExprKind::Binary { left, right, .. } => {
            has_consuming_sink(name, left) || has_consuming_sink(name, right)
        }
        ExprKind::Range { start, end, .. } => {
            start
                .as_deref()
                .is_some_and(|s| has_consuming_sink(name, s))
                || end.as_deref().is_some_and(|s| has_consuming_sink(name, s))
        }
        // ── Control flow: recurse into all sub-exprs / stmts ───────────────
        ExprKind::Block(b) => block_has_sink(name, b),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            has_consuming_sink(name, condition)
                || block_has_sink(name, then_block)
                || else_branch
                    .as_deref()
                    .is_some_and(|e| has_consuming_sink(name, e))
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            has_consuming_sink(name, value)
                || block_has_sink(name, then_block)
                || else_branch
                    .as_deref()
                    .is_some_and(|e| has_consuming_sink(name, e))
        }
        ExprKind::Match { scrutinee, arms } => {
            has_consuming_sink(name, scrutinee)
                || arms.iter().any(|a| {
                    a.guard.as_ref().is_some_and(|g| has_consuming_sink(name, g))
                        // An arm whose RESULT forwards `name` transfers it out
                        // of this expression (the match value flows onward).
                        || value_derived_from(name, &a.body)
                        || has_consuming_sink(name, &a.body)
                })
        }
        // Everything else (literals, identifiers, paths, …) holds no sink.
        _ => false,
    }
}

fn block_has_sink(name: &str, b: &crate::ast::Block) -> bool {
    for s in &b.stmts {
        if stmt_has_sink(name, s) {
            return true;
        }
    }
    b.final_expr
        .as_deref()
        .is_some_and(|e| has_consuming_sink(name, e))
}

fn stmt_has_sink(name: &str, s: &Stmt) -> bool {
    match &s.kind {
        // A `let w = <derived-from-name>` moves ownership into `w`.
        StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
            value_derived_from(name, value) || has_consuming_sink(name, value)
        }
        // `target = <derived>` / `target op= <derived>` — a store transfers.
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            value_derived_from(name, value)
                || has_consuming_sink(name, value)
                || has_consuming_sink(name, target)
        }
        StmtKind::Expr(e) => has_consuming_sink(name, e),
        _ => false,
    }
}

/// Does `e` reference `name` at all (used for closure-capture detection)?
fn expr_mentions(name: &str, e: &Expr) -> bool {
    let mut found = false;
    walk_exprs(e, &mut |x| {
        if let ExprKind::Identifier(n) = &x.kind {
            if n == name {
                found = true;
            }
        }
    });
    found
}

/// Minimal structural walk over the direct child expressions of `e`, applying
/// `f` to `e` and every descendant. Only the child-bearing variants this
/// module reasons about need enumerating; the rest have no `name` occurrence
/// that matters for capture detection.
fn walk_exprs(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Unary {
            operand: object, ..
        }
        | ExprKind::Cast { expr: object, .. } => walk_exprs(object, f),
        ExprKind::Index { object, index } => {
            walk_exprs(object, f);
            walk_exprs(index, f);
        }
        ExprKind::Binary { left, right, .. } => {
            walk_exprs(left, f);
            walk_exprs(right, f);
        }
        ExprKind::Return(inner) => {
            if let Some(x) = inner.as_deref() {
                walk_exprs(x, f);
            }
        }
        ExprKind::Call { callee, args } => {
            walk_exprs(callee, f);
            for a in args {
                walk_exprs(&a.value, f);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_exprs(object, f);
            for a in args {
                walk_exprs(&a.value, f);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for fi in fields {
                walk_exprs(&fi.value, f);
            }
            if let Some(s) = spread.as_deref() {
                walk_exprs(s, f);
            }
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for x in items {
                walk_exprs(x, f);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items {
                walk_exprs(x, f);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk_exprs(value, f);
            walk_exprs(count, f);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                walk_exprs(k, f);
                walk_exprs(v, f);
            }
        }
        ExprKind::Closure { body, .. } => walk_exprs(body, f),
        ExprKind::Block(b) => walk_block(b, f),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_exprs(condition, f);
            walk_block(then_block, f);
            if let Some(e) = else_branch.as_deref() {
                walk_exprs(e, f);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_exprs(value, f);
            walk_block(then_block, f);
            if let Some(e) = else_branch.as_deref() {
                walk_exprs(e, f);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_exprs(scrutinee, f);
            for a in arms {
                if let Some(g) = &a.guard {
                    walk_exprs(g, f);
                }
                walk_exprs(&a.body, f);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                walk_exprs(s, f);
            }
            if let Some(en) = end.as_deref() {
                walk_exprs(en, f);
            }
        }
        _ => {}
    }
}

fn walk_block(b: &crate::ast::Block, f: &mut impl FnMut(&Expr)) {
    for s in &b.stmts {
        match &s.kind {
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => walk_exprs(value, f),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                walk_exprs(target, f);
                walk_exprs(value, f);
            }
            StmtKind::Expr(e) => walk_exprs(e, f),
            _ => {}
        }
    }
    if let Some(e) = b.final_expr.as_deref() {
        walk_exprs(e, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_arm_body(src: &str) -> Expr {
        // Wrap the snippet as a function body and pull the tail expression out,
        // so tests can write natural arm-body expressions.
        let full = format!("fn f() {{ {src} }}");
        let parsed = crate::parse(&full);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let crate::ast::Item::Function(func) = &parsed.program.items[0] else {
            panic!("expected fn");
        };
        func.body
            .final_expr
            .as_deref()
            .cloned()
            .unwrap_or_else(|| panic!("no tail expr in: {src}"))
    }

    fn only_borrowed(src: &str) -> bool {
        binding_only_borrowed("v", &parse_arm_body(src))
    }

    // ── NON-consuming: source retains, drop must stay armed ────────────────

    #[test]
    fn free_fn_call_arg_is_non_consuming() {
        // The B-2026-07-03-31 shape: `ident_len(v)` entry-copies v.
        assert!(only_borrowed("ident_len(v)"));
        assert!(only_borrowed("g(v, 1)"));
        assert!(only_borrowed("h(v) + k(v)"));
    }

    #[test]
    fn field_read_and_method_receiver_are_non_consuming() {
        // `v.len()` borrows the receiver; its RESULT is a fresh i64, so as an
        // arm tail it does not forward `v`. `f(v.field)` copies the field into
        // `f`. Neither transfers `v`'s ownership. (A BARE `v.field` tail *does*
        // escape — see `projection_that_escapes_is_consumed`.)
        assert!(only_borrowed("v.len()"));
        assert!(only_borrowed("f(v.field)"));
        assert!(only_borrowed("if v.len() > 0 { 1 } else { 0 }"));
        assert!(only_borrowed("{ let n = v.len(); n }"));
    }

    #[test]
    fn unused_binding_is_non_consuming() {
        assert!(only_borrowed("0"));
        assert!(only_borrowed("other()"));
    }

    // ── Consuming: ownership transfers, source drop must be suppressed ──────

    #[test]
    fn tail_forward_of_binding_is_consumed() {
        assert!(!only_borrowed("v"));
        assert!(!only_borrowed("{ let n = 1; v }"));
        assert!(!only_borrowed("if c() { v } else { other() }"));
    }

    #[test]
    fn projection_that_escapes_is_consumed() {
        // Partial move: `v.field` as the result moves the field out; the
        // source's whole-payload drop would double-free it.
        assert!(!only_borrowed("v.field"));
        assert!(!only_borrowed("v.0"));
    }

    #[test]
    fn aggregate_literal_element_is_consumed() {
        assert!(!only_borrowed("Foo { x: v }"));
        assert!(!only_borrowed("(v, 1)"));
        assert!(!only_borrowed("Vec[v]"));
        assert!(!only_borrowed("Foo { x: v.field }"));
    }

    #[test]
    fn container_mutator_arg_is_consumed() {
        assert!(!only_borrowed("{ out.push(v); 0 }"));
        assert!(!only_borrowed("{ m.insert(k, v); 0 }"));
    }

    #[test]
    fn store_and_move_binding_are_consumed() {
        assert!(!only_borrowed("{ let w = v; w.len() }"));
        assert!(!only_borrowed("{ out = v; 0 }"));
    }

    #[test]
    fn return_of_binding_is_consumed() {
        assert!(!only_borrowed("{ return v; }"));
        assert!(!only_borrowed("{ return v.field; }"));
    }

    #[test]
    fn path_or_variant_construction_arg_is_consumed() {
        // Not a bare-identifier callee → treated as a construction (safe bias).
        assert!(!only_borrowed("Opt.Some(v)"));
    }

    #[test]
    fn closure_capture_is_consumed() {
        assert!(!only_borrowed("{ let f = || v.len(); f() }"));
    }

    #[test]
    fn nested_sink_inside_free_fn_arg_is_consumed() {
        // `f(Foo { x: v })` — the struct literal consumes v before f copies it.
        assert!(!only_borrowed("f(Foo { x: v })"));
    }
}
