//! Conservative "does a `let` binding escape?" analysis for the
//! `Result[shared]` scope-exit-RC residual of B-2026-07-12-24.
//!
//! ## Why
//!
//! `track_rc_result_var` (codegen) queues a scope-exit `RcDecOption` that
//! releases a `Result[shared T, E]` binding's payload node. That is correct
//! ONLY when the binding is consumed IN PLACE — a binding that is moved OUT
//! (returned, pushed into a collection, passed to a consuming call, stored in
//! a struct/tuple, captured by a closure, reassigned) hands its `+1` to a
//! second owner, so a producer-side dec would double-free (`Result` has no
//! move-out coordination — the `var_option_shared_heap`-keyed inc/suppress
//! machinery is `Option`-only).
//!
//! This module answers, conservatively, "is binding `name` used ONLY as a
//! direct `match` scrutinee (or unused) within the function body?" — the one
//! shape that is provably consume-in-place. Every OTHER position counts as an
//! escape, so an uncertain use is always classified escaping (leak, never a
//! double-free).
//!
//! ## Soundness
//!
//! The per-`ExprKind` / per-`StmtKind` walks below are **exhaustive matches
//! with no `_` wildcard**, so adding an AST node breaks this file's build
//! rather than silently skipping a position where a value could move out. The
//! count rule is: a name is non-escaping iff its TOTAL `Identifier` uses equal
//! its `match`-scrutinee uses (i.e. it appears nowhere else). Shadowing (an
//! inner `let name = …` of the same identifier) merges counts, which only ever
//! makes the result MORE conservative (an inner use inflates the total →
//! treated as escaping → the binding is left leaking, never double-freed).

use crate::ast::{
    Block, CallArg, Expr, ExprKind, Function, MatchArm, ParsedInterpolationPart, Stmt, StmtKind,
};
use std::collections::{HashMap, HashSet};

/// Per-binding-name use tally: `(total Identifier uses, uses that are a direct
/// `match` scrutinee)`.
#[derive(Default)]
struct Acc<'a> {
    counts: HashMap<&'a str, (u32, u32)>,
    /// `(binding name, value-span (offset,length))` for every `Binding`-pattern
    /// `let` / `let…else` encountered — filtered against `counts` after the walk.
    lets: Vec<(&'a str, (usize, usize))>,
    /// True while walking inside a closure body. A reference to an OUTER binding
    /// there is a CAPTURE — an escape into an env that can outlive the binding's
    /// scope — so `match`-scrutinee safety is suppressed (even `match d` inside a
    /// closure counts `d` as escaping). Without this a captured `Result[shared]`
    /// would get a producer-side dec that use-after-frees the escaping closure's
    /// env. Closure-local bindings are only ever made MORE conservative by this.
    in_closure: bool,
}

/// Value-spans of every `let <Binding> = <value>` in `func` whose binding name
/// never escapes (see module docs). Keyed by `(value.span.offset,
/// value.span.length)` — the same key codegen's let-statement handler uses.
/// Run on the POST-lowering AST (codegen's view) so the recorded spans match
/// the nodes the handler sees.
pub fn nonescaping_let_value_spans(func: &Function) -> HashSet<(usize, usize)> {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    let mut out = HashSet::new();
    for (name, span) in &acc.lets {
        let (total, scrut) = acc.counts.get(name).copied().unwrap_or((0, 0));
        if total == scrut {
            out.insert(*span);
        }
    }
    out
}

/// Names of `func`'s PARAMETERS that never escape the body — used only as a
/// direct `match` scrutinee, or unused (same rule as [`nonescaping_let_value_spans`]).
/// An OWNED `Result[shared]` param that is consumed in place owns the caller's
/// transferred `+1` and can safely release it at scope exit; a forwarded param
/// (passed on to another consuming call / returned) escapes → left out → the
/// terminal consumer's dec stays the only one. Borrowed (`ref`) params are the
/// caller's to drop; the codegen param site excludes them by type separately.
pub fn nonescaping_param_names(func: &Function) -> HashSet<String> {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            let (total, scrut) = acc.counts.get(name.as_str()).copied().unwrap_or((0, 0));
            (total == scrut).then(|| name.clone())
        })
        .collect()
}

fn record_use<'a>(acc: &mut Acc<'a>, name: &'a str, scrutinee: bool) {
    let e = acc.counts.entry(name).or_insert((0, 0));
    e.0 += 1;
    if scrutinee {
        e.1 += 1;
    }
}

/// Walk a pattern-matching CONSTRUCT's scrutinee (`match` / `if let` / `while
/// let` / `let…else` value). A bare `Identifier(n)` scrutinee is a
/// consume-in-place use (counted as a scrutinee use — safe), UNLESS inside a
/// closure where referencing an outer binding is a capture (escape). The bare
/// identifier is counted directly (NOT recursed into, which would double-count
/// it as a plain use); any other scrutinee shape recurses normally. All four
/// pattern-match forms are match-sugar, so they share this consume semantics.
fn walk_scrutinee<'a>(acc: &mut Acc<'a>, scrutinee: &'a Expr) {
    if let ExprKind::Identifier(n) = &scrutinee.kind {
        record_use(acc, n.as_str(), !acc.in_closure);
    } else {
        walk_expr(scrutinee, acc);
    }
}

fn walk_block<'a>(b: &'a Block, acc: &mut Acc<'a>) {
    for s in &b.stmts {
        walk_stmt(s, acc);
    }
    if let Some(fe) = &b.final_expr {
        walk_expr(fe, acc);
    }
}

fn walk_stmt<'a>(s: &'a Stmt, acc: &mut Acc<'a>) {
    match &s.kind {
        StmtKind::Let { pattern, value, .. } => {
            if let crate::ast::PatternKind::Binding(name) = &pattern.kind {
                acc.lets
                    .push((name.as_str(), (value.span.offset, value.span.length)));
            }
            walk_expr(value, acc);
        }
        StmtKind::LetElse {
            pattern,
            value,
            else_block,
            ..
        } => {
            if let crate::ast::PatternKind::Binding(name) = &pattern.kind {
                // Irrefutable-binding let-else (`let x = v else`, rare) —
                // introduces `x`, so record it and treat `v` as its RHS.
                acc.lets
                    .push((name.as_str(), (value.span.offset, value.span.length)));
                walk_expr(value, acc);
            } else {
                // Refutable `let Pat = <scrutinee> else { … }` — match-sugar
                // over `value`, so `value` is a consume-in-place scrutinee.
                walk_scrutinee(acc, value);
            }
            walk_block(else_block, acc);
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => walk_block(body, acc),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, acc);
            walk_expr(value, acc);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                walk_expr(t, acc);
            }
            for v in values {
                walk_expr(v, acc);
            }
        }
        StmtKind::Expr(e) => walk_expr(e, acc),
    }
}

fn walk_call_arg<'a>(a: &'a CallArg, acc: &mut Acc<'a>) {
    walk_expr(&a.value, acc);
}

fn walk_match_arm<'a>(a: &'a MatchArm, acc: &mut Acc<'a>) {
    // Patterns bind NEW names; they are not uses of an outer binding. Guard and
    // body ARE ordinary use positions (any binding referenced there escapes).
    if let Some(g) = &a.guard {
        walk_expr(g, acc);
    }
    walk_expr(&a.body, acc);
}

fn walk_expr<'a>(e: &'a Expr, acc: &mut Acc<'a>) {
    match &e.kind {
        // A bare identifier reached HERE is a use in a non-`match`-scrutinee
        // position (the scrutinee case is intercepted in the `Match` arm below
        // and never recurses here), so it is an escape.
        ExprKind::Identifier(n) => record_use(acc, n.as_str(), false),
        ExprKind::Match { scrutinee, arms } => {
            walk_scrutinee(acc, scrutinee);
            for arm in arms {
                walk_match_arm(arm, acc);
            }
        }
        // Leaves with no sub-expressions.
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::Error => {}
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(inner) = p {
                    walk_expr(inner, acc);
                }
            }
        }
        ExprKind::Binary { left, right, .. } => {
            walk_expr(left, acc);
            walk_expr(right, acc);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, acc),
        ExprKind::Question(inner) => walk_expr(inner, acc),
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(object, acc);
            if let Some(a) = args {
                for arg in a {
                    walk_call_arg(arg, acc);
                }
            }
        }
        ExprKind::NilCoalesce { left, right } => {
            walk_expr(left, acc);
            walk_expr(right, acc);
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, acc);
            for a in args {
                walk_call_arg(a, acc);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object, acc);
            for a in args {
                walk_call_arg(a, acc);
            }
        }
        ExprKind::FieldAccess { object, .. } => walk_expr(object, acc),
        ExprKind::TupleIndex { object, .. } => walk_expr(object, acc),
        ExprKind::Index { object, index } => {
            walk_expr(object, acc);
            walk_expr(index, acc);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => walk_block(b, acc),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, acc);
            walk_block(then_block, acc);
            if let Some(e) = else_branch {
                walk_expr(e, acc);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            // `if let Pat = <scrutinee>` is match-sugar — consume-in-place.
            walk_scrutinee(acc, value);
            walk_block(then_block, acc);
            if let Some(e) = else_branch {
                walk_expr(e, acc);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, acc);
            walk_block(body, acc);
        }
        ExprKind::WhileLet { value, body, .. } => {
            // `while let Pat = <scrutinee>` is match-sugar — consume-in-place.
            walk_scrutinee(acc, value);
            walk_block(body, acc);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable, acc);
            walk_block(body, acc);
        }
        ExprKind::Loop { body, .. } => walk_block(body, acc),
        ExprKind::LabeledBlock { body, .. } => walk_block(body, acc),
        ExprKind::Closure { body, .. } => {
            let prev = acc.in_closure;
            acc.in_closure = true;
            walk_expr(body, acc);
            acc.in_closure = prev;
        }
        ExprKind::Return(opt) => {
            if let Some(inner) = opt {
                walk_expr(inner, acc);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                walk_expr(v, acc);
            }
        }
        ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
            for x in exprs {
                walk_expr(x, acc);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items {
                walk_expr(x, acc);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk_expr(value, acc);
            walk_expr(count, acc);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                walk_expr(k, acc);
                walk_expr(v, acc);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                walk_expr(&f.value, acc);
            }
            if let Some(sp) = spread {
                walk_expr(sp, acc);
            }
        }
        ExprKind::Pipe { left, right } => {
            walk_expr(left, acc);
            walk_expr(right, acc);
        }
        ExprKind::Cast { expr, .. } => walk_expr(expr, acc),
        ExprKind::OffsetOf { .. } => {}
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_expr(s, acc);
            }
            if let Some(e) = end {
                walk_expr(e, acc);
            }
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            walk_block(b, acc)
        }
        ExprKind::Lock { body, .. } => walk_block(body, acc),
        ExprKind::Providers { bindings, body } => {
            for pb in bindings {
                walk_expr(&pb.value, acc);
            }
            walk_block(body, acc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Item;
    use std::collections::HashSet;

    /// Non-escaping binding NAMES in the first function of `src` — maps the
    /// span-keyed production result back to names for readable assertions.
    fn nonescaping_names(src: &str) -> HashSet<String> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let func = parsed
            .program
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("no function");
        let spans = nonescaping_let_value_spans(func);
        // Second walk: collect (name, value-span) for every Binding let, keep
        // names whose span is in the non-escaping set.
        let mut acc = Acc::default();
        walk_block(&func.body, &mut acc);
        acc.lets
            .iter()
            .filter(|(_, sp)| spans.contains(sp))
            .map(|(n, _)| n.to_string())
            .collect()
    }

    #[test]
    fn consume_in_place_and_discard_are_nonescaping() {
        // matched-in-place and unused bindings are safe to release.
        let names = nonescaping_names(
            "fn f() -> i64 { let d = g(); let u = g(); match d { A(n) => n, B => 0 } }",
        );
        assert!(
            names.contains("d"),
            "matched-in-place `d` should be non-escaping"
        );
        assert!(names.contains("u"), "unused `u` should be non-escaping");
    }

    #[test]
    fn if_let_scrutinee_is_nonescaping() {
        // `if let Pat = d` is match-sugar → consume-in-place.
        let names =
            nonescaping_names("fn f() -> i64 { let d = g(); if let A(n) = d { n } else { 0 } }");
        assert!(
            names.contains("d"),
            "if-let scrutinee `d` should be non-escaping"
        );
    }

    #[test]
    fn if_let_capture_into_closure_escapes() {
        // if-let inside a closure body is still a capture (escape).
        let names = nonescaping_names(
            "fn f() -> i64 { let d = g(); let c = || { if let A(n) = d { n } else { 0 } }; c() }",
        );
        assert!(
            !names.contains("d"),
            "if-let inside a closure captures `d` → escape"
        );
    }

    #[test]
    fn multiple_matches_of_same_binding_are_nonescaping() {
        let names =
            nonescaping_names("fn f() { let d = g(); match d { _ => {} } match d { _ => {} } }");
        assert!(names.contains("d"));
    }

    #[test]
    fn returned_binding_escapes() {
        let names = nonescaping_names("fn f() -> R { let d = g(); d }");
        assert!(!names.contains("d"), "returned `d` must escape");
    }

    #[test]
    fn call_arg_use_escapes() {
        // `d` used as a call argument (and as an alias RHS) is a non-scrutinee
        // use → escapes. (`e`, matched only, satisfies THIS module's contract —
        // "used only as a match scrutinee"; the alias-RHS hazard for `e` is
        // handled separately by codegen's `owned_rhs` gate, which refuses an
        // identifier-alias RHS. This module answers escape, not alias-ownership.)
        let names = nonescaping_names(
            "fn f() -> i64 { let d = g(); let e = d; eat(d); match e { _ => 0 } }",
        );
        assert!(
            !names.contains("d"),
            "`d` passed to a call / aliased must escape"
        );
    }

    #[test]
    fn struct_tuple_field_positions_escape() {
        let s = nonescaping_names(
            "fn f() -> i64 { let d = g(); let b = S { r: d }; match b.r { _ => 0 } }",
        );
        assert!(!s.contains("d"), "struct-field init must escape");
        let t = nonescaping_names(
            "fn f() -> i64 { let d = g(); let p = (d, 1); match p.0 { _ => 0 } }",
        );
        assert!(!t.contains("d"), "tuple element must escape");
    }

    #[test]
    fn reassigned_binding_escapes() {
        // `d = g()` reads `d` as an assign target → a non-scrutinee use.
        let names =
            nonescaping_names("fn f() -> i64 { let mut d = g(); d = g(); match d { _ => 0 } }");
        assert!(
            !names.contains("d"),
            "reassigned `d` must escape (conservative)"
        );
    }

    /// Non-escaping PARAM names of the first function in `src`.
    fn nonescaping_params(src: &str) -> HashSet<String> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let func = parsed
            .program
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("no function");
        nonescaping_param_names(func)
    }

    #[test]
    fn param_consumed_in_place_is_nonescaping() {
        // A param used only as a `match` scrutinee owns the caller's transferred
        // +1 and can be released.
        let names = nonescaping_params("fn eat(r: R) -> i64 { match r { A(n) => n, B => 0 } }");
        assert!(
            names.contains("r"),
            "in-place-consumed param `r` should be non-escaping"
        );
    }

    #[test]
    fn forwarded_param_escapes() {
        // A param passed on to another consuming call escapes → the intermediate
        // must not release it (the terminal consumer does).
        let names = nonescaping_params("fn eat(r: R) -> i64 { eat2(r) }");
        assert!(!names.contains("r"), "forwarded param `r` must escape");
    }

    #[test]
    fn returned_param_escapes() {
        let names = nonescaping_params("fn id(r: R) -> R { r }");
        assert!(!names.contains("r"), "returned param `r` must escape");
    }

    #[test]
    fn capture_into_closure_escapes() {
        // `match d` INSIDE a closure body is a capture — must NOT be treated as a
        // safe scrutinee use (would use-after-free an escaping closure's env).
        let names = nonescaping_names(
            "fn f() -> i64 { let d = g(); let c = || { match d { A(n) => n, B => 0 } }; c() }",
        );
        assert!(
            !names.contains("d"),
            "binding captured by a closure must escape even if only matched inside"
        );
    }
}
