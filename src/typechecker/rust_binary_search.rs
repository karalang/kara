//! Rust's `binary_search` taken apart as a `Result`, carried over
//! (B-2026-11-09-14).
//!
//! Rust's `binary_search` returns `Result<usize, usize>`: `Ok` of an index
//! holding the value, `Err` of the insertion point. Kāra's returns
//! `Option[i64]` (docs/library/collections.md). Rust's documentation leaves
//! which of several equal elements `Ok` names unspecified, but its
//! implementation names the last, as Kāra's `binary_search` does, and a
//! carried program can depend on it (kata 1235's search arm, a list of
//! repeated keys whose last is the best, B-2026-11-19-32). So the rewrite
//! searches once for the end of the equal run,
//! `hi = partition_point(|y| y <= v)`: when `xs[hi - 1] == v`, `Ok`'s index
//! is `hi - 1`, and otherwise `hi` is `Err`'s insertion point, the same
//! as `partition_point(|y| y < v)` when `v` is absent. When a Rust shape
//! fails to check, its errors give way to one that says so, and the
//! mechanical shapes carry the rewrite:
//!
//! - `xs.binary_search(v).unwrap_or_else(|i| f(i))` is
//!   `{ let i = xs.partition_point(|y| y <= v); if i > 0 and xs[i - 1] == v { i - 1 } else { f(i) } }`.
//! - `xs.binary_search_by_key(k, f).unwrap()` and `.expect(m)` are the same
//!   with `panic(..)` in place of `f(i)`.
//! - `match xs.binary_search(v) { Ok(i) => a, Err(j) => b }` and
//!   `if let Ok(i)`/`Err(i) = xs.binary_search(v) { .. } else { .. }` are
//!   `{ let j = xs.partition_point(|y| y <= v); if j > 0 and xs[j - 1] == v { let i = j - 1; a } else { b } }`.
//!   When no side reads `Ok`'s index (`Ok(_)`, or no `Ok` side), the
//!   search finds the insertion point instead, and the test reads the
//!   element there: `{ let j = xs.partition_point(|y| y < v); if j < xs.len() and xs[j] == v { a } else { b } }`.
//!   When both sides are alike the test is dropped, and an empty side
//!   is left out when the other is not: `if not (..) { b }`.
//!
//! `xs` must be a plain place and `v` free of effects, since the rewrite
//! names each more than once; otherwise the message says so.
//!
//! Kāra has no `binary_search_by_key` or `binary_search_by`, and the same
//! shapes over them carry the same way (B-2026-11-13-6): the key closure
//! `|p| key` against `k` finds the end of the run with
//! `partition_point(|p| key <= k)`, and the run is there when
//! `partition_point(|p| key < k)` lies below it; an ordering closure
//! `|p| order` reads `order != Ordering.Greater` and
//! `order == Ordering.Less` for the two.

use crate::ast::{BinOp, CallArg, Expr, ExprKind, MatchArm, Pattern, PatternKind};
use crate::resolver::SpanKey;
use crate::token::Span;

use super::{TypeChecker, TypeErrorKind};

/// What a Rust binary search compares each element by.
#[derive(Clone, Copy)]
enum By<'a> {
    /// `xs.binary_search(v)`: the element itself, against `v`.
    Value(&'a Expr),
    /// `xs.binary_search_by_key(k, |p| key)`: the key, against `k`.
    Key(&'a Expr, &'a Expr),
    /// `xs.binary_search_by(|p| order)`: the `Ordering` the closure gives.
    Order(&'a Expr),
}

/// A closure of one parameter.
fn is_closure1(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Closure { params, .. } if params.len() == 1)
}

/// Rust's `&v`, read as `v`: the rewrite compares by value.
fn deref_arg(e: &Expr) -> &Expr {
    match &e.kind {
        ExprKind::Unary {
            op: crate::ast::UnaryOp::Ref,
            operand,
        } => operand,
        _ => e,
    }
}

/// `xs.binary_search(v)`, `xs.binary_search_by_key(k, f)` or
/// `xs.binary_search_by(f)`: the receiver and what it searches by.
fn binary_search_call(e: &Expr) -> Option<(&Expr, By<'_>)> {
    let ExprKind::MethodCall {
        object,
        method,
        args,
        ..
    } = &e.kind
    else {
        return None;
    };
    let by = match (method.as_str(), args.as_slice()) {
        ("binary_search", [v]) => By::Value(&v.value),
        ("binary_search_by_key", [k, f]) if is_closure1(&f.value) => {
            By::Key(deref_arg(&k.value), &f.value)
        }
        ("binary_search_by", [f]) if is_closure1(&f.value) => By::Order(&f.value),
        _ => return None,
    };
    Some((object, by))
}

/// `closure` with its body `body op right`.
fn closure_with(closure: &Expr, op: BinOp, right: Expr) -> String {
    let mut c = closure.clone();
    if let ExprKind::Closure { body, .. } = &mut c.kind {
        let left = std::mem::replace(
            body,
            Box::new(Expr {
                kind: ExprKind::Bool(false),
                span: closure.span,
                id: crate::ids::NodeId::DUMMY,
            }),
        );
        **body = Expr {
            kind: ExprKind::Binary {
                op,
                left,
                right: Box::new(right),
            },
            span: closure.span,
            id: crate::ids::NodeId::DUMMY,
        };
    }
    crate::formatter::render_expr(&c)
}

/// `Ordering.<variant>`, as an expression to render.
fn ordering(variant: &str, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Identifier(format!("Ordering.{variant}")),
        span,
        id: crate::ids::NodeId::DUMMY,
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

/// The pieces of a rewrite: the search, with `at` naming where it ends,
/// and the test that the value is there. When `Ok`'s index is read
/// (`last`), the search finds the end of the run of equal elements,
/// `xs.partition_point(|y| y <= v)`, and `Ok`'s index is `at - 1`, the last
/// equal element, as Rust's implementation gives it (B-2026-11-19-32).
/// Otherwise it finds the insertion point, `xs.partition_point(|y| y < v)`,
/// and the test reads the element there. Either way, without the value,
/// `at` is `Err`'s insertion point.
fn search(xs: &Expr, by: By, at: &str, last: bool) -> (String, String) {
    let r = crate::formatter::render_expr(xs);
    let (to, past) = if last {
        (BinOp::LtEq, BinOp::Lt)
    } else {
        (BinOp::Lt, BinOp::LtEq)
    };
    // The other end of the run, against `at`.
    let other = |p: String| {
        if last {
            format!("{p} < {at}")
        } else {
            format!("{at} < {p}")
        }
    };
    match by {
        By::Value(v) => {
            let val = crate::formatter::render_expr(v);
            let y = fresh(v);
            let op = if last { "<=" } else { "<" };
            let holds = if last {
                format!("{at} > 0 and {r}[{at} - 1] == {val}")
            } else {
                format!("{at} < {r}.len() and {r}[{at}] == {val}")
            };
            (format!("{r}.partition_point(|{y}| {y} {op} {val})"), holds)
        }
        By::Key(k, f) => (
            format!("{r}.partition_point({})", closure_with(f, to, k.clone())),
            other(format!(
                "{r}.partition_point({})",
                closure_with(f, past, k.clone())
            )),
        ),
        By::Order(f) => {
            let (end, start) = (
                closure_with(f, BinOp::NotEq, ordering("Greater", f.span)),
                closure_with(f, BinOp::Eq, ordering("Less", f.span)),
            );
            let (find, test) = if last { (end, start) } else { (start, end) };
            (
                format!("{r}.partition_point({find})"),
                other(format!("{r}.partition_point({test})")),
            )
        }
    }
}

/// Why the rewrite cannot name `xs` and the value again, if it cannot.
fn not_mechanical(xs: &Expr, by: By) -> Option<&'static str> {
    let value = match by {
        By::Value(v) | By::Key(v, _) => Some(v),
        By::Order(_) => None,
    };
    if !is_plain_place(xs) {
        Some("the rewrite names the sequence more than once, so bind it to a `let` first")
    } else if value.is_some_and(|v| !is_pure(v)) {
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

/// `body`, the block of the branch that binds `name`, reading `index` (the
/// end of the search, or one less on the `Ok` side) as it.
fn bound_branch(name: Option<&str>, index: &str, body: &Expr) -> String {
    if let Some(n) = name {
        // `Ok(i) => i`: the index itself.
        if matches!(&body.kind, ExprKind::Identifier(b) if b == n) {
            return format!("{{ {index} }}");
        }
    }
    let text = block_text(body);
    match name {
        Some(n) if n != index => match text.strip_prefix('{') {
            Some(rest) => format!("{{ let {n} = {index};{rest}"),
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

/// The rewrite of a Rust match or `if let` over `xs.binary_search(v)`,
/// and the search it takes apart.
fn rewrite_branches(e: &Expr) -> Option<(By<'_>, Result<String, String>)> {
    let (scrutinee, ok, err) = sides(e)?;
    let (xs, by) = binary_search_call(scrutinee)?;
    if let Some(why) = not_mechanical(xs, by) {
        return Some((by, Err(why.to_string())));
    }
    // The search names the end of the run: `Err`'s index as it stands,
    // `Ok`'s one less.
    let at = err
        .as_ref()
        .and_then(|s| s.0)
        .or(ok.as_ref().and_then(|s| s.0))
        .unwrap_or("at");
    // Only a side that reads `Ok`'s index needs the last equal element.
    let last = ok.as_ref().is_some_and(|s| s.0.is_some());
    let (find, holds) = search(xs, by, at, last);
    let index = format!("{at} - 1");
    // An empty side, `Ok(_) => {}`, is left out when the other is not, so
    // the rewrite reads `if not (..) { .. }` rather than keeping an empty
    // branch (kata 1713's Rust arm, B-2026-11-20-43).
    let empty = |side: &Option<Side>| {
        side.as_ref().is_some_and(|s| {
            matches!(&s.1.kind, ExprKind::Block(b) if b.stmts.is_empty() && b.final_expr.is_none())
        })
    };
    let (ok_empty, err_empty) = (empty(&ok), empty(&err));
    let ok_text = ok
        .as_ref()
        .filter(|_| !(ok_empty && !err_empty))
        .map(|s| bound_branch(s.0, &index, &s.1));
    let err_text = err
        .as_ref()
        .filter(|_| !(err_empty && !ok_empty))
        .map(|s| bound_branch(s.0, at, &s.1));
    let tail = match (ok_text, err_text) {
        // Both sides alike: the test decides nothing, and the side's
        // block joins the rewrite's (`Ok(i) => i - 1, Err(i) => i - 1`,
        // kata 1235's search arm, is `{ let i = ..; i - 1 }`).
        (Some(o), Some(r)) if o == r => match o.strip_prefix('{').and_then(|t| t.strip_suffix('}'))
        {
            Some(inner) => inner.trim().to_string(),
            None => o,
        },
        (Some(o), Some(r)) => format!("if {holds} {o} else {r}"),
        (Some(o), None) => format!("if {holds} {o}"),
        (None, Some(r)) => format!("if not ({holds}) {r}"),
        (None, None) => return None,
    };
    Some((by, Ok(format!("{{ let {at} = {find}; {tail} }}"))))
}

/// The rewrite of `xs.binary_search(v).unwrap_or_else(|i| f)`.
fn rewrite_unwrap_or_else(xs: &Expr, by: By, closure: &Expr) -> Option<Result<String, String>> {
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
    if let Some(why) = not_mechanical(xs, by) {
        return Some(Err(why.to_string()));
    }
    let at = if name == "_" { "at" } else { name };
    let (find, holds) = search(xs, by, at, true);
    // `Ok`'s index is the last equal element (B-2026-11-19-32), so even
    // `|i| i` keeps the test.
    Some(Ok(format!(
        "{{ let {at} = {find}; if {holds} {{ {at} - 1 }} else {} }}",
        block_text(body)
    )))
}

/// The rewrite of `xs.binary_search_by_key(k, f).unwrap()` or
/// `.expect(m)` (kata 1357's search arm, B-2026-11-21-61): `Ok`'s index,
/// the last equal element, and otherwise the panic Rust's `unwrap` or
/// `expect` would raise. A plain `binary_search(v).unwrap()` checks as it
/// stands, since Kāra's `Option` has `unwrap`, and never reaches here.
fn rewrite_unwrap(xs: &Expr, by: By, message: Option<&Expr>) -> Option<Result<String, String>> {
    if let Some(why) = not_mechanical(xs, by) {
        return Some(Err(why.to_string()));
    }
    let (find, holds) = search(xs, by, "at", true);
    let message = message.map_or_else(
        || "\"the binary search found no match\"".to_string(),
        crate::formatter::render_expr,
    );
    Some(Ok(format!(
        "{{ let at = {find}; if {holds} {{ at - 1 }} else {{ panic({message}) }} }}"
    )))
}

const SHAPE: &str = "Kāra's `binary_search` returns `Option[i64]`, the index of an element \
                     equal to the value or `None`, not Rust's `Result` of an index or the \
                     insertion point; the insertion point is `partition_point(|y| y < v)`, \
                     and the last equal element, the one Rust's `Ok` names, lies just below \
                     `partition_point(|y| y <= v)`";

/// What the message says of the search it rewrites.
fn shape(by: By) -> String {
    match by {
        By::Value(_) => SHAPE.to_string(),
        By::Key(..) => "Kāra has no `binary_search_by_key`; the insertion point of `k` is \
                        `partition_point(|p| key < k)`, and when it lies below \
                        `partition_point(|p| key <= k)` the last element just below that, \
                        the one Rust's `Ok` names, holds `k`"
            .to_string(),
        By::Order(_) => "Kāra has no `binary_search_by`; the insertion point is \
                         `partition_point(|p| order == Ordering.Less)`, and when it lies below \
                         `partition_point(|p| order != Ordering.Greater)` the last element \
                         just below that, the one Rust's `Ok` names, is a match"
            .to_string(),
    }
}

/// `text` laid out by the formatter, its lines after the first indented
/// to `column`, where the statement holding the expression it replaces
/// starts; as it is when it does not parse alone.
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
    pub(super) fn retract_errors_within(&mut self, before: usize, within: Span) -> bool {
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

    fn report_binary_search(&mut self, at: Span, by: By, rewrite: Result<String, String>) {
        let shape = shape(by);
        match rewrite {
            Ok(text) => {
                // The later lines line up with the statement the search
                // starts on, not with the search itself (`let k = match ..`
                // in kata 1182's Rust arm, B-2026-11-17-108).
                let column = match self.current_stmt {
                    Some(stmt) if stmt.line == at.line => stmt.column,
                    _ => at.column,
                };
                let text = reflow(&text, column);
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
                self.type_error(format!("{shape}{write}"), at, TypeErrorKind::TypeMismatch);
            }
            Err(why) => self.type_error(
                format!("{shape}; this one is not rewritten, as {why}"),
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
        let Some((xs, by)) = binary_search_call(object) else {
            return;
        };
        let rewrite = match (method, args) {
            ("unwrap_or_else", [f]) => rewrite_unwrap_or_else(xs, by, &f.value),
            ("unwrap", []) => rewrite_unwrap(xs, by, None),
            ("expect", [m]) => rewrite_unwrap(xs, by, Some(&m.value)),
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
            self.report_binary_search(whole, by, rewrite);
        }
    }

    /// After a `match` or `if let` failed to check: one over a
    /// `binary_search` with Rust's `Ok`/`Err` patterns gives way to
    /// Kāra's shape.
    pub(super) fn explain_rust_binary_search_branches(&mut self, e: &Expr, before: usize) {
        if self.errors.len() <= before {
            return;
        }
        let Some((by, rewrite)) = rewrite_branches(e) else {
            return;
        };
        if self.retract_errors_within(before, e.span) {
            self.report_binary_search(e.span, by, rewrite);
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
