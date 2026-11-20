//! Rust's linked-list cursor step, carried over (B-2026-11-20-12).
//!
//! `while tail.is_some() { tail = &mut tail.as_mut().unwrap().next; }`
//! walks a `&mut Option<Box<Node>>` to the list's tail in Rust. In Kāra
//! `mut ref tail.as_mut().unwrap().next` borrows through a call result,
//! which §5.1 refuses. The accepted spelling is the cursor walk of
//! core-semantics.md §5.3, `while let Some(n) = tail { tail = mut ref
//! n.next; }`, and the rewrite is mechanical when the loop's body is that
//! one step: the condition becomes `let Some(n) = tail` and the borrow's
//! operand reads `n` for `tail.as_mut().unwrap()`. The `while` is noted when
//! it is checked, and the refusal of the `mut ref` carries the rewrite.

use crate::ast::{Block, Expr, ExprKind, StmtKind, UnaryOp};
use crate::resolver::{SpanKey, TextEdit};
use crate::token::Span;

use super::TypeChecker;

/// `<c>.as_mut().unwrap()` (or `.expect(..)`): the cursor's name and the
/// end of the `unwrap` call.
fn unwrapped_as_mut(e: &Expr) -> Option<(&str, usize)> {
    let ExprKind::MethodCall {
        object,
        method,
        args_close_span,
        ..
    } = &e.kind
    else {
        return None;
    };
    if !matches!(method.as_str(), "unwrap" | "expect") {
        return None;
    }
    let ExprKind::MethodCall {
        object: inner,
        method: as_mut,
        args,
        ..
    } = &object.kind
    else {
        return None;
    };
    if as_mut != "as_mut" || !args.is_empty() {
        return None;
    }
    let ExprKind::Identifier(c) = &inner.kind else {
        return None;
    };
    (args_close_span.length > 0)
        .then(|| (c.as_str(), args_close_span.offset + args_close_span.length))
}

/// The operand `<c>.as_mut().unwrap().<f>` of a `mut ref`: the cursor's
/// name, the field, and the end of the `unwrap` call.
pub(super) fn cursor_operand(operand: &Expr) -> Option<(&str, &str, usize)> {
    let ExprKind::FieldAccess { object, field } = &operand.kind else {
        return None;
    };
    let (c, end) = unwrapped_as_mut(object)?;
    Some((c, field.as_str(), end))
}

/// The rewrite of `while <c>.is_some() { <c> = mut ref
/// <c>.as_mut().unwrap().<f>; }`, keyed by the `mut ref`'s span.
fn rewrite(condition: &Expr, body: &Block) -> Option<(Span, Vec<TextEdit>)> {
    let ExprKind::MethodCall {
        object,
        method,
        args,
        args_close_span,
        ..
    } = &condition.kind
    else {
        return None;
    };
    let ExprKind::Identifier(c) = &object.kind else {
        return None;
    };
    if method != "is_some" || !args.is_empty() || args_close_span.length == 0 {
        return None;
    }
    let ([stmt], None) = (body.stmts.as_slice(), &body.final_expr) else {
        return None;
    };
    let StmtKind::Assign { target, value } = &stmt.kind else {
        return None;
    };
    if !matches!(&target.kind, ExprKind::Identifier(t) if t == c) {
        return None;
    }
    let ExprKind::Unary {
        op: UnaryOp::MutRef,
        operand,
    } = &value.kind
    else {
        return None;
    };
    let (cursor, _, unwrap_end) = cursor_operand(operand)?;
    if cursor != c {
        return None;
    }
    let n = if c == "n" { "node" } else { "n" };
    let cond_end = args_close_span.offset + args_close_span.length;
    let edits = vec![
        TextEdit {
            offset: condition.span.offset,
            length: cond_end.saturating_sub(condition.span.offset),
            replacement: format!("let Some({n}) = {c}"),
        },
        TextEdit {
            offset: operand.span.offset,
            length: unwrap_end.saturating_sub(operand.span.offset),
            replacement: n.to_string(),
        },
    ];
    Some((value.span, edits))
}

impl TypeChecker<'_> {
    /// Notes the rewrite of a `while` that walks a cursor with Rust's
    /// `as_mut().unwrap()` step, for the refusal of the step's `mut ref` to
    /// carry (B-2026-11-20-12).
    pub(super) fn note_cursor_walk_rewrite(&mut self, condition: &Expr, body: &Block) {
        if let Some((at, edits)) = rewrite(condition, body) {
            self.cursor_walk_rewrites
                .insert(SpanKey::from_span(&at), edits);
        }
    }

    /// The noted rewrite of the `while` whose cursor step is the `mut ref`
    /// at `at`.
    pub(super) fn cursor_walk_rewrite(&self, at: &Span) -> Option<Vec<TextEdit>> {
        self.cursor_walk_rewrites
            .get(&SpanKey::from_span(at))
            .cloned()
    }
}
