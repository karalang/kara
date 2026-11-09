//! Rust's `last_mut` / `first_mut` run-extension idiom, carried over
//! (B-2026-11-08-46).
//!
//! `match v.last_mut() { Some(x) if g => body, _ => other }` and
//! `if let Some(x) = v.last_mut() { body } else { other }` grow-or-extend a
//! run in Rust. Kāra has no `last_mut` (B-2026-11-06-39): a write goes
//! through an index. The rewrite is mechanical when `x` names the element
//! and nothing in the guard or the body names `v` itself: `if v.len() > 0
//! and g' { body' } else { other }`, where `'` reads `v[v.len() - 1]` (or
//! `v[0]`) for `x`. The `match`/`if let` is noted when it is checked, and
//! the refusal of `last_mut` on it carries the rewrite.

use crate::ast::{Expr, ExprKind, Pattern, PatternKind};
use crate::resolver::SpanKey;
use crate::span_visitor::MutVisitor;
use crate::token::Span;

use super::TypeChecker;

/// `Some(name)`.
fn some_binding(p: &Pattern) -> Option<&str> {
    let PatternKind::TupleVariant { path, patterns } = &p.kind else {
        return None;
    };
    if path.last().map(String::as_str) != Some("Some") {
        return None;
    }
    match patterns.as_slice() {
        [Pattern {
            kind: PatternKind::Binding(n),
            ..
        }] => Some(n),
        _ => None,
    }
}

/// A receiver that can be named again without running anything: `v`,
/// `self.runs`, `a.b.c`.
fn is_plain_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Identifier(_) | ExprKind::SelfValue => true,
        ExprKind::FieldAccess { object, .. } => is_plain_place(object),
        _ => false,
    }
}

/// The root name of a plain place (`self` for `self.runs`).
fn root(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n),
        ExprKind::SelfValue => Some("self"),
        ExprKind::FieldAccess { object, .. } => root(object),
        _ => None,
    }
}

/// Whether `e` names `name`: used, or bound again (a closure parameter, a
/// `let` or a pattern), either of which leaves the substitution unsound.
fn names(e: &Expr, name: &str) -> bool {
    let mut hit = false;
    crate::span_visitor::for_each_expr(e, &mut |x| match &x.kind {
        ExprKind::Identifier(n) if n == name => hit = true,
        ExprKind::SelfValue if name == "self" => hit = true,
        _ => {}
    });
    hit
}

fn rebinds(e: &Expr, name: &str) -> bool {
    struct Binds<'a>(&'a str, bool);
    impl MutVisitor for Binds<'_> {
        fn span(&mut self, _: &mut Span) {}
        fn pattern(&mut self, p: &mut Pattern) {
            if matches!(&p.kind, PatternKind::Binding(n) if n == self.0) {
                self.1 = true;
            }
        }
    }
    let mut copy = e.clone();
    let mut b = Binds(name, false);
    crate::span_visitor::walk_expr_mut_with(&mut copy, &mut b);
    b.1
}

/// Every `name` read as `with`.
struct Subst<'a> {
    name: &'a str,
    with: &'a Expr,
}

impl MutVisitor for Subst<'_> {
    fn span(&mut self, _: &mut Span) {}
    fn expr(&mut self, e: &mut Expr) {
        // `*x` is Rust's write through the element: the element itself.
        let named = |e: &Expr| matches!(&e.kind, ExprKind::Identifier(n) if n == self.name);
        let through = matches!(&e.kind, ExprKind::Unary { op: crate::ast::UnaryOp::Deref, operand }
            if named(operand));
        if through || named(e) {
            *e = self.with.clone();
        }
    }
}

/// The receiver and method of `recv.last_mut()` / `recv.first_mut()`.
fn end_mut_call(e: &Expr) -> Option<(&Expr, bool)> {
    let ExprKind::MethodCall {
        object,
        method,
        args,
        ..
    } = &e.kind
    else {
        return None;
    };
    let last = match method.as_str() {
        "last_mut" => true,
        "first_mut" => false,
        _ => return None,
    };
    (args.is_empty() && is_plain_place(object)).then_some((object, last))
}

/// The rewrite of a `match`/`if let` over `v.last_mut()`, keyed by the
/// receiver's span: the whole expression's span and its replacement.
fn rewrite(e: &Expr) -> Option<(Span, Span, String)> {
    let (call, bound, guard, body, other): (&Expr, &str, Option<&Expr>, Expr, Option<Expr>) =
        match &e.kind {
            ExprKind::Match { scrutinee, arms } => {
                let [first, rest] = arms.as_slice() else {
                    return None;
                };
                if !matches!(rest.pattern.kind, PatternKind::Wildcard) || rest.guard.is_some() {
                    return None;
                }
                (
                    scrutinee,
                    some_binding(&first.pattern)?,
                    first.guard.as_ref(),
                    first.body.clone(),
                    Some(rest.body.clone()),
                )
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => (
                value,
                some_binding(pattern)?,
                None,
                Expr {
                    kind: ExprKind::Block(then_block.clone()),
                    span: then_block.span,
                    id: crate::ids::NodeId::DUMMY,
                },
                else_branch.as_deref().cloned(),
            ),
            _ => return None,
        };
    let (recv, last) = end_mut_call(call)?;
    let recv_root = root(recv)?;
    for part in guard.into_iter().chain(std::iter::once(&body)) {
        if names(part, recv_root) || rebinds(part, bound) {
            return None;
        }
    }
    let r = crate::formatter::render_expr(recv);
    let at = if last {
        format!("{r}[{r}.len() - 1]")
    } else {
        format!("{r}[0]")
    };
    // The element's text stands in for the binding: the rewrite is
    // rendered, never checked as it stands.
    let elem = Expr {
        kind: ExprKind::Identifier(at),
        span: recv.span,
        id: crate::ids::NodeId::DUMMY,
    };
    let sub = |x: &Expr| {
        let mut x = x.clone();
        crate::span_visitor::walk_expr_mut_with(
            &mut x,
            &mut Subst {
                name: bound,
                with: &elem,
            },
        );
        crate::formatter::render_expr(&x)
    };
    let mut cond = format!("{r}.len() > 0");
    if let Some(g) = guard {
        cond = format!("{cond} and {}", sub(g));
    }
    let then = match &body.kind {
        ExprKind::Block(_) => sub(&body),
        _ => format!("{{ {}; }}", sub(&body)),
    };
    let text = match other {
        None => format!("if {cond} {then}"),
        Some(o) => {
            let o = match &o.kind {
                ExprKind::Block(_) | ExprKind::If { .. } | ExprKind::IfLet { .. } => {
                    crate::formatter::render_expr(&o)
                }
                _ => format!("{{ {}; }}", crate::formatter::render_expr(&o)),
            };
            format!("if {cond} {then} else {o}")
        }
    };
    // Lines after the first keep the expression's own indentation.
    let indent = " ".repeat(e.span.column.saturating_sub(1));
    let text = text
        .lines()
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"));
    Some((recv.span, e.span, text))
}

impl TypeChecker<'_> {
    /// Notes the rewrite of a `match`/`if let` over `v.last_mut()` for the
    /// refusal of `last_mut` to carry.
    pub(super) fn note_end_mut_rewrite(&mut self, e: &Expr) {
        if let Some((recv, whole, text)) = rewrite(e) {
            self.end_mut_rewrites
                .insert(SpanKey::from_span(&recv), (whole, text));
        }
    }

    /// The noted rewrite of the `match`/`if let` whose `last_mut` call has
    /// the receiver at `recv`.
    pub(super) fn end_mut_rewrite(&self, recv: &Span) -> Option<(Span, String)> {
        self.end_mut_rewrites
            .get(&SpanKey::from_span(recv))
            .cloned()
    }
}
