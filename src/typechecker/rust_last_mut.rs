//! Rust's `last_mut` / `first_mut` run-extension idiom, carried over
//! (B-2026-11-08-46).
//!
//! `match v.last_mut() { Some(x) if g => body, _ => other }` and
//! `if let Some(x) = v.last_mut() { body } else { other }` grow-or-extend a
//! run in Rust. Kāra has no `last_mut` (B-2026-11-06-39): a write goes
//! through an index. The rewrite is mechanical when `x` names the element
//! and the guard and the body name `v` itself only after `x`'s last use, as
//! Rust's borrow checker requires: `if v.len() > 0 and g' { body' } else {
//! other }`, where `'` reads `v[v.len() - 1]` (or `v[0]`) for `x`. A pattern
//! `Some((a, b))` over a vector of tuples reads the element's fields for
//! `a` and `b`. The `match`/`if let` is noted when it is checked, and the
//! refusal of `last_mut` on it carries the rewrite.
//!
//! `let x = v.last_mut().unwrap();` (or `.expect(..)`) is the third shape
//! (B-2026-11-10-51): the `let` goes, and every `x` the statements after it
//! write or read through (`x.f`, `x[i]`, `*x`, `x.m(..)`) reads `v[v.len() -
//! 1]`, edited in place so the rest of the block keeps its own text. As
//! above, nothing after the `let` may name `v` or bind `x` again.

use crate::ast::{Block, Expr, ExprKind, Pattern, PatternKind, StmtKind};
use crate::resolver::{SpanKey, TextEdit};
use crate::span_visitor::MutVisitor;
use crate::token::Span;

use super::TypeChecker;

/// `Some(x)`, or `Some((a, b, ..))` whose parts are names or `_` (Rust's
/// `if let Some((c, n)) = st.last_mut()` over a stack of pairs, kata 1209):
/// each name bound, with the tuple field it reads, `None` for the whole
/// element.
fn some_bindings(p: &Pattern) -> Option<Vec<(&str, Option<usize>)>> {
    let PatternKind::TupleVariant { path, patterns } = &p.kind else {
        return None;
    };
    if path.last().map(String::as_str) != Some("Some") {
        return None;
    }
    let [inner] = patterns.as_slice() else {
        return None;
    };
    match &inner.kind {
        PatternKind::Binding(n) => Some(vec![(n.as_str(), None)]),
        PatternKind::Tuple(parts) => {
            let mut out = Vec::new();
            for (i, part) in parts.iter().enumerate() {
                match &part.kind {
                    PatternKind::Binding(n) => out.push((n.as_str(), Some(i))),
                    PatternKind::Wildcard => {}
                    _ => return None,
                }
            }
            (!out.is_empty()).then_some(out)
        }
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
pub(super) fn names(e: &Expr, name: &str) -> bool {
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

/// Whether every place `parts` name `root` comes after every use of the
/// bound names, and no loop or closure holds both. Rust's borrow of the
/// element ends at its last use, so a body may grow or shrink the vector
/// after it (`top.1 += 1; if top.1 == k { stack.pop(); }`, kata 1209); read
/// through an index, each use still reads the element it did.
fn root_after_uses(parts: &[&Expr], root: &str, bound: &[&str]) -> bool {
    let is_root = |x: &Expr| match &x.kind {
        ExprKind::Identifier(n) => n == root,
        ExprKind::SelfValue => root == "self",
        _ => false,
    };
    let is_bound =
        |x: &Expr| matches!(&x.kind, ExprKind::Identifier(n) if bound.contains(&n.as_str()));
    let mut first_root = usize::MAX;
    let mut last_use = 0;
    let mut tangled = false;
    for part in parts {
        crate::span_visitor::for_each_expr(part, &mut |x| {
            if is_root(x) {
                first_root = first_root.min(x.span.offset);
            }
            if is_bound(x) {
                last_use = last_use.max(x.span.offset + 1);
            }
            if matches!(
                x.kind,
                ExprKind::While { .. }
                    | ExprKind::WhileLet { .. }
                    | ExprKind::For { .. }
                    | ExprKind::Loop { .. }
                    | ExprKind::Closure { .. }
            ) {
                let (mut r, mut b) = (false, false);
                crate::span_visitor::for_each_expr(x, &mut |y| {
                    r |= is_root(y);
                    b |= is_bound(y);
                });
                tangled |= r && b;
            }
        });
    }
    !tangled && last_use <= first_root
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
    type Parts<'e> = (
        &'e Expr,
        Vec<(&'e str, Option<usize>)>,
        Option<&'e Expr>,
        Expr,
        Option<Expr>,
    );
    let (call, bound, guard, body, other): Parts = match &e.kind {
        ExprKind::Match { scrutinee, arms } => {
            let [first, rest] = arms.as_slice() else {
                return None;
            };
            if !matches!(rest.pattern.kind, PatternKind::Wildcard) || rest.guard.is_some() {
                return None;
            }
            (
                scrutinee,
                some_bindings(&first.pattern)?,
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
            some_bindings(pattern)?,
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
    let parts: Vec<&Expr> = guard.into_iter().chain(std::iter::once(&body)).collect();
    let names_only: Vec<&str> = bound.iter().map(|(n, _)| *n).collect();
    if parts
        .iter()
        .any(|p| names_only.iter().any(|n| rebinds(p, n)))
    {
        return None;
    }
    if parts.iter().any(|p| names(p, recv_root)) && !root_after_uses(&parts, recv_root, &names_only)
    {
        return None;
    }
    let r = crate::formatter::render_expr(recv);
    // The element (or its field) stands in for each binding: the rewrite is
    // rendered, never checked as it stands. It is built as the expression
    // it is, not as a name spelling it, which the renderer escapes when it
    // begins with a keyword: `r#self.owed[..]` for a receiver of
    // `self.owed` (kata 1381, B-2026-11-21-65).
    let node = |kind: ExprKind| Expr {
        kind,
        span: recv.span,
        id: crate::ids::NodeId::DUMMY,
    };
    let index = if last {
        node(ExprKind::Binary {
            op: crate::ast::BinOp::Sub,
            left: Box::new(node(ExprKind::MethodCall {
                object: Box::new(recv.clone()),
                method: "len".to_string(),
                turbofish: None,
                args: Vec::new(),
                args_close_span: recv.span,
            })),
            right: Box::new(node(ExprKind::Integer(1, None))),
        })
    } else {
        node(ExprKind::Integer(0, None))
    };
    let element = node(ExprKind::Index {
        object: Box::new(recv.clone()),
        index: Box::new(index),
    });
    let elems: Vec<(&str, Expr)> = bound
        .iter()
        .map(|(n, field)| {
            let e = match field {
                Some(i) => node(ExprKind::TupleIndex {
                    object: Box::new(element.clone()),
                    index: *i as u64,
                }),
                None => element.clone(),
            };
            (*n, e)
        })
        .collect();
    let sub = |x: &Expr| {
        let mut x = x.clone();
        for (name, with) in &elems {
            crate::span_visitor::walk_expr_mut_with(&mut x, &mut Subst { name, with });
        }
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

/// The edits for `let x = v.last_mut().unwrap();` in `block`, keyed by the
/// receiver's span: the `let` deleted, and each `x` read through after it
/// replaced by the element.
fn let_rewrites(block: &Block) -> Vec<(Span, Vec<TextEdit>)> {
    let mut out = Vec::new();
    for (i, stmt) in block.stmts.iter().enumerate() {
        let StmtKind::Let {
            pattern,
            ty: None,
            value,
            ..
        } = &stmt.kind
        else {
            continue;
        };
        let PatternKind::Binding(bound) = &pattern.kind else {
            continue;
        };
        let ExprKind::MethodCall { object, method, .. } = &value.kind else {
            continue;
        };
        if !matches!(method.as_str(), "unwrap" | "expect") {
            continue;
        }
        let Some((recv, last)) = end_mut_call(object) else {
            continue;
        };
        let Some(recv_root) = root(recv) else {
            continue;
        };
        // Where the `let` ends: the next statement, or the tail.
        let Some(next) = block
            .stmts
            .get(i + 1)
            .map(|s| s.span)
            .or_else(|| block.final_expr.as_ref().map(|e| e.span))
        else {
            continue;
        };
        let rest = Expr {
            kind: ExprKind::Block(Block {
                stmts: block.stmts[i + 1..].to_vec(),
                final_expr: block.final_expr.clone(),
                span: block.span,
            }),
            span: block.span,
            id: crate::ids::NodeId::DUMMY,
        };
        if names(&rest, recv_root) || rebinds(&rest, bound) {
            continue;
        }
        let is_bound = |e: &Expr| matches!(&e.kind, ExprKind::Identifier(n) if n == bound);
        // Every use of `x`, and the uses that read or write through it with
        // the span each one's replacement covers (`*x` as a whole).
        let mut uses: Vec<Span> = Vec::new();
        let mut through: Vec<(Span, Span)> = Vec::new();
        crate::span_visitor::for_each_expr(&rest, &mut |x| match &x.kind {
            ExprKind::Identifier(n) if n == bound => uses.push(x.span),
            ExprKind::FieldAccess { object, .. }
            | ExprKind::Index { object, .. }
            | ExprKind::MethodCall { object, .. }
                if is_bound(object) =>
            {
                through.push((object.span, object.span))
            }
            ExprKind::Unary {
                op: crate::ast::UnaryOp::Deref,
                operand,
            } if is_bound(operand) => through.push((operand.span, x.span)),
            _ => {}
        });
        let key = |s: &Span| (s.offset, s.length);
        if uses.is_empty()
            || uses
                .iter()
                .any(|u| !through.iter().any(|(t, _)| key(t) == key(u)))
        {
            continue;
        }
        let r = crate::formatter::render_expr(recv);
        let at = if last {
            format!("{r}[{r}.len() - 1]")
        } else {
            format!("{r}[0]")
        };
        let mut edits = vec![TextEdit {
            offset: stmt.span.offset,
            length: next.offset.saturating_sub(stmt.span.offset),
            replacement: String::new(),
        }];
        // A `*x` covers its `x`: one edit for the two.
        through.sort_by_key(|(_, whole)| (whole.offset, std::cmp::Reverse(whole.length)));
        let mut end = 0;
        for (_, whole) in through {
            if whole.offset < end {
                continue;
            }
            end = whole.offset + whole.length;
            edits.push(TextEdit {
                offset: whole.offset,
                length: whole.length,
                replacement: at.clone(),
            });
        }
        out.push((recv.span, edits));
    }
    out
}

impl TypeChecker<'_> {
    /// Notes the rewrites of `let x = v.last_mut().unwrap();` in a block for
    /// the refusal of `last_mut` to carry (B-2026-11-10-51).
    pub(super) fn note_end_mut_let_rewrites(&mut self, block: &Block) {
        if !block.stmts.iter().any(|s| {
            matches!(&s.kind, StmtKind::Let { value: Expr { kind: ExprKind::MethodCall { method, .. }, .. }, .. }
                if method == "unwrap" || method == "expect")
        }) {
            return;
        }
        for (recv, edits) in let_rewrites(block) {
            self.end_mut_let_rewrites
                .insert(SpanKey::from_span(&recv), edits);
        }
    }

    /// The noted rewrite of the `let` whose `last_mut` call has the
    /// receiver at `recv`.
    pub(super) fn end_mut_let_rewrite(&self, recv: &Span) -> Option<Vec<TextEdit>> {
        self.end_mut_let_rewrites
            .get(&SpanKey::from_span(recv))
            .cloned()
    }

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
