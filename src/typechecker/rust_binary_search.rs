//! Rust's `binary_search` taken apart as a `Result`, carried over
//! (B-2026-11-09-14).
//!
//! Rust's `binary_search` returns `Result<usize, usize>`: `Ok` of an index
//! holding the value, `Err` of the insertion point. Kāra's returns
//! `Option[i64]` (docs/library/collections.md), and the insertion point is
//! `partition_point(|y| y < v)`, which is also a valid `Ok` index when the
//! value is there (Rust leaves which equal element unspecified). When a
//! Rust shape fails to check, its errors give way to one that says so, and
//! the mechanical shapes carry the rewrite:
//!
//! - `xs.binary_search(v).unwrap_or_else(|i| i)` is
//!   `xs.partition_point(|y| y < v)`; another body `f(i)` is
//!   `{ let i = xs.partition_point(..); if i < xs.len() and xs[i] == v { i } else { f(i) } }`.
//! - `match xs.binary_search(v) { Ok(i) => a, Err(j) => b }` and
//!   `if let Ok(i)`/`Err(i) = xs.binary_search(v) { .. } else { .. }` are
//!   `{ let i = xs.partition_point(|y| y < v); if i < xs.len() and xs[i] == v { a } else { b } }`.
//!
//! `xs` must be a plain place and `v` free of effects, since the rewrite
//! names each more than once; otherwise the message says so.

use crate::ast::{CallArg, Expr, ExprKind, MatchArm, Pattern, PatternKind};
use crate::resolver::SpanKey;
use crate::token::Span;

use super::{TypeChecker, TypeErrorKind};

/// `xs.binary_search(v)`: the receiver and the value.
fn binary_search_call(e: &Expr) -> Option<(&Expr, &Expr)> {
    match &e.kind {
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } if method == "binary_search" && args.len() == 1 => Some((object, &args[0].value)),
        _ => None,
    }
}

/// A place named again without running anything.
fn is_plain_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Identifier(_) | ExprKind::SelfValue => true,
        ExprKind::FieldAccess { object, .. } => is_plain_place(object),
        _ => false,
    }
}

/// A value evaluated again with the same result and no effect.
fn is_pure(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::Bool(_) => true,
        ExprKind::Unary { operand, .. } => is_pure(operand),
        ExprKind::Binary { left, right, .. } => is_pure(left) && is_pure(right),
        ExprKind::Index { object, index } => is_pure(object) && is_pure(index),
        _ => is_plain_place(e),
    }
}

/// A name for the predicate's parameter that `v` does not use.
fn fresh(v: &Expr) -> &'static str {
    ["y", "z", "w", "el", "elem"]
        .into_iter()
        .find(|n| {
            let mut used = false;
            crate::span_visitor::for_each_expr(v, &mut |x| {
                used |= matches!(&x.kind, ExprKind::Identifier(m) if m == n);
            });
            !used
        })
        .unwrap_or("elem")
}

/// The pieces of a rewrite: `xs.partition_point(|y| y < v)` and the test
/// that the index found holds `v`.
fn search(xs: &Expr, v: &Expr, at: &str) -> (String, String) {
    let r = crate::formatter::render_expr(xs);
    let val = crate::formatter::render_expr(v);
    let y = fresh(v);
    (
        format!("{r}.partition_point(|{y}| {y} < {val})"),
        format!("{at} < {r}.len() and {r}[{at}] == {val}"),
    )
}

/// Why the rewrite cannot name `xs` and `v` again, if it cannot.
fn not_mechanical(xs: &Expr, v: &Expr) -> Option<&'static str> {
    if !is_plain_place(xs) {
        Some("the rewrite names the sequence more than once, so bind it to a `let` first")
    } else if !is_pure(v) {
        Some("the rewrite names the value more than once, so bind it to a `let` first")
    } else {
        None
    }
}

/// `Ok(name)` / `Err(name)` / `Ok(_)`: the variant and the bound name.
fn result_arm(p: &Pattern) -> Option<(bool, Option<&str>)> {
    let PatternKind::TupleVariant { path, patterns } = &p.kind else {
        return None;
    };
    let ok = match path.last().map(String::as_str) {
        Some("Ok") => true,
        Some("Err") => false,
        _ => return None,
    };
    match patterns.as_slice() {
        [Pattern {
            kind: PatternKind::Binding(n),
            ..
        }] => Some((ok, Some(n))),
        [Pattern {
            kind: PatternKind::Wildcard,
            ..
        }] => Some((ok, None)),
        _ => None,
    }
}

/// A branch's text, as a block.
fn block_text(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Block(_) => crate::formatter::render_expr(e),
        _ => format!("{{ {} }}", crate::formatter::render_expr(e)),
    }
}

/// `body`, the block of the branch that binds `name`, reading `at` as it.
fn bound_branch(name: Option<&str>, at: &str, body: &Expr) -> String {
    let text = block_text(body);
    match name {
        Some(n) if n != at => match text.strip_prefix('{') {
            Some(rest) => format!("{{ let {n} = {at};{rest}"),
            None => text,
        },
        _ => text,
    }
}

/// One side of a Rust match or `if let` over a `binary_search`: the name
/// its pattern binds, and its body.
type Side<'a> = (Option<&'a str>, Expr);

/// The `Ok` and `Err` sides, either possibly absent (an `if let` with no
/// `else`), and the scrutinee.
fn sides<'a>(e: &'a Expr) -> Option<(&'a Expr, Option<Side<'a>>, Option<Side<'a>>)> {
    match &e.kind {
        ExprKind::Match { scrutinee, arms } => {
            let [a, b]: &[MatchArm] = arms.as_slice() else {
                return None;
            };
            if a.guard.is_some() || b.guard.is_some() {
                return None;
            }
            // `Some(ok)` for an `Ok`/`Err` pattern, `None` for `_`.
            let kind = |arm: &'a MatchArm| match &arm.pattern.kind {
                PatternKind::Wildcard => Some((None, None)),
                _ => result_arm(&arm.pattern).map(|(ok, n)| (Some(ok), n)),
            };
            let (ka, na) = kind(a)?;
            let (kb, nb) = kind(b)?;
            let a_is_ok = match (ka, kb) {
                (Some(x), Some(y)) if x != y => x,
                (Some(x), None) => x,
                (None, Some(y)) => !y,
                _ => return None,
            };
            let (sa, sb) = ((na, a.body.clone()), (nb, b.body.clone()));
            let (ok, err) = if a_is_ok { (sa, sb) } else { (sb, sa) };
            Some((scrutinee, Some(ok), Some(err)))
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            let (ok, name) = result_arm(pattern)?;
            let then = Expr {
                kind: ExprKind::Block(then_block.clone()),
                span: then_block.span,
                id: crate::ids::NodeId::DUMMY,
            };
            let other = else_branch.as_deref().map(|o| (None, o.clone()));
            Some(if ok {
                (value, Some((name, then)), other)
            } else {
                (value, other, Some((name, then)))
            })
        }
        _ => None,
    }
}

/// The rewrite of a Rust match or `if let` over `xs.binary_search(v)`.
fn rewrite_branches(e: &Expr) -> Option<Result<String, String>> {
    let (scrutinee, ok, err) = sides(e)?;
    let (xs, v) = binary_search_call(scrutinee)?;
    if let Some(why) = not_mechanical(xs, v) {
        return Some(Err(why.to_string()));
    }
    let at = ok
        .as_ref()
        .and_then(|s| s.0)
        .or(err.as_ref().and_then(|s| s.0))
        .unwrap_or("at");
    let (find, holds) = search(xs, v, at);
    let text = |s: &Side| bound_branch(s.0, at, &s.1);
    let tail = match (ok.as_ref().map(text), err.as_ref().map(text)) {
        (Some(o), Some(r)) => format!("if {holds} {o} else {r}"),
        (Some(o), None) => format!("if {holds} {o}"),
        (None, Some(r)) => format!("if not ({holds}) {r}"),
        (None, None) => return None,
    };
    Some(Ok(format!("{{ let {at} = {find}; {tail} }}")))
}

/// The rewrite of `xs.binary_search(v).unwrap_or_else(|i| f)`.
fn rewrite_unwrap_or_else(xs: &Expr, v: &Expr, closure: &Expr) -> Option<Result<String, String>> {
    let ExprKind::Closure { params, body, .. } = &closure.kind else {
        return None;
    };
    let [p] = params.as_slice() else {
        return None;
    };
    let name = match &p.pattern.kind {
        PatternKind::Binding(n) => n.as_str(),
        PatternKind::Wildcard => "_",
        _ => return None,
    };
    if matches!(&body.kind, ExprKind::Identifier(n) if n == name) {
        if !is_pure(v) {
            return Some(Err(
                "the predicate names the value once per probe, so bind it to a `let` first"
                    .to_string(),
            ));
        }
        return Some(Ok(search(xs, v, name).0));
    }
    if let Some(why) = not_mechanical(xs, v) {
        return Some(Err(why.to_string()));
    }
    let at = if name == "_" { "at" } else { name };
    let (find, holds) = search(xs, v, at);
    Some(Ok(format!(
        "{{ let {at} = {find}; if {holds} {{ {at} }} else {} }}",
        block_text(body)
    )))
}

const SHAPE: &str = "Kāra's `binary_search` returns `Option[i64]`, the index of an element \
                     equal to the value or `None`, not Rust's `Result` of an index or the \
                     insertion point; the insertion point is `partition_point(|y| y < v)`";

/// `text` laid out by the formatter, its lines after the first indented
/// to `column`, where the expression it replaces starts; as it is when it
/// does not parse alone.
fn reflow(text: &str, column: usize) -> String {
    let parsed = crate::parse(&format!("fn __reflow() {{\n{text}\n}}\n"));
    let laid = match parsed.program.items.first() {
        Some(crate::ast::Item::Function(f)) if parsed.errors.is_empty() => {
            match (&f.body.final_expr, f.body.stmts.as_slice()) {
                (Some(e), []) => crate::formatter::render_expr(e),
                _ => return text.to_string(),
            }
        }
        _ => return text.to_string(),
    };
    let indent = " ".repeat(column.saturating_sub(1));
    laid.lines()
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"))
}

impl TypeChecker<'_> {
    /// Takes back the errors at or after `before` that lie inside `within`;
    /// true when there were any.
    fn retract_errors_within(&mut self, before: usize, within: Span) -> bool {
        let inside = |s: &Span| {
            s.offset >= within.offset && s.offset + s.length <= within.offset + within.length
        };
        let mut any = false;
        let mut i = before;
        while i < self.errors.len() {
            if inside(&self.errors[i].span) {
                let e = self.errors.remove(i);
                self.error_fix_diffs.remove(&SpanKey::from_span(&e.span));
                any = true;
            } else {
                i += 1;
            }
        }
        any
    }

    fn report_binary_search(&mut self, at: Span, rewrite: Result<String, String>) {
        match rewrite {
            Ok(text) => {
                let text = reflow(&text, at.column);
                self.error_fix_diffs.insert(
                    SpanKey::from_span(&at),
                    vec![crate::resolver::TextEdit {
                        offset: at.offset,
                        length: at.length,
                        replacement: text.clone(),
                    }],
                );
                let write = if text.contains('\n') {
                    "; `karac fix` rewrites this one through `partition_point`".to_string()
                } else {
                    format!(": write `{text}`")
                };
                self.type_error(format!("{SHAPE}{write}"), at, TypeErrorKind::TypeMismatch);
            }
            Err(why) => self.type_error(
                format!("{SHAPE}; this one is not rewritten, as {why}"),
                at,
                TypeErrorKind::TypeMismatch,
            ),
        }
    }

    /// After `xs.binary_search(v).method(args)` failed to check, spanning
    /// `whole`: Rust's `Result` methods give way to Kāra's shape.
    pub(super) fn explain_rust_binary_search_call(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        whole: Span,
        before: usize,
    ) {
        let Some((xs, v)) = binary_search_call(object) else {
            return;
        };
        let rewrite = match (method, args) {
            ("unwrap_or_else", [f]) => rewrite_unwrap_or_else(xs, v, &f.value),
            (
                "unwrap_err" | "expect_err" | "map_err" | "err" | "ok" | "is_ok_and" | "is_err_and",
                _,
            ) => Some(Err(format!(
                "`.{method}()` takes Rust's `Result` apart, and the `Option` has no counterpart"
            ))),
            _ => None,
        };
        let Some(rewrite) = rewrite else {
            return;
        };
        if self.retract_errors_within(before, whole) {
            self.report_binary_search(whole, rewrite);
        }
    }

    /// After a `match` or `if let` failed to check: one over a
    /// `binary_search` with Rust's `Ok`/`Err` patterns gives way to
    /// Kāra's shape.
    pub(super) fn explain_rust_binary_search_branches(&mut self, e: &Expr, before: usize) {
        if self.errors.len() <= before {
            return;
        }
        let Some(rewrite) = rewrite_branches(e) else {
            return;
        };
        if self.retract_errors_within(before, e.span) {
            self.report_binary_search(e.span, rewrite);
        }
    }
}

impl TypeChecker<'_> {
    /// `deprecated` on `is_ok()` / `is_err()` called on an `Option`
    /// (docs/library/core-types.md, review-thread ruling 2026-11-09,
    /// B-2026-11-09-21): `Result` methods that an `Option` accepts as
    /// `is_some()` / `is_none()`, which Rust's `binary_search(..).is_ok()`
    /// reaches. 0.8.x warns, with the rewrite; the next minor refuses them.
    pub(super) fn warn_option_result_predicate(
        &mut self,
        object: &Expr,
        method: &str,
        obj_ty: &super::Type,
        close: &Span,
    ) {
        let mut ty = obj_ty;
        while let super::Type::Ref(t) | super::Type::MutRef(t) = ty {
            ty = t;
        }
        if self.compiling_stdlib
            || !matches!(ty, super::Type::Named { name, .. } if name == "Option")
            || !self.option_result_predicates_reported.insert(close.offset)
        {
            return;
        }
        let to = if method == "is_ok" {
            "is_some"
        } else {
            "is_none"
        };
        let from = object.span.offset + object.span.length;
        let end = close.offset + close.length;
        let fix_it = (end > from && close.length > 0).then(|| super::FixIt {
            span: Span {
                offset: from,
                length: end - from,
                ..object.span
            },
            replacement: format!(".{to}()"),
        });
        self.type_lint_warning_with_fix(
            format!(
                "use of deprecated `{method}()` on an `Option`: it is a `Result` method, and \
                 on an `Option` it means `{to}()`; it becomes an error at the next minor \
                 release — suppress with `#[allow(deprecated)]` on the enclosing item"
            ),
            Span {
                length: end.saturating_sub(object.span.offset),
                ..object.span
            },
            TypeErrorKind::Deprecated,
            "deprecated",
            fix_it,
        );
    }
}
