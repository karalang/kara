//! `?` followed by a member access, as design.md reads it: `?` is postfix,
//! so `f()?.x` is `(f()?).x` and `f()?.m()` is `(f()?).m()`
//! (§ Results and propagation, "There is no optional-chaining operator").
//!
//! The lexer still makes one `?.` token and the parser an
//! [`ExprKind::OptionalChain`], the removed operator the legacy pipeline
//! still runs. The v2 check and the MIR pipeline rewrite each chain step
//! into a propagation and a plain member access before resolving
//! (B-2026-10-14-50).

use crate::ast::{CallArg, Expr, ExprKind, Program};
use crate::ids::NodeId;
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;

struct Rewrite;

impl MutVisitor for Rewrite {
    fn span(&mut self, _span: &mut Span) {}

    fn expr(&mut self, e: &mut Expr) {
        let ExprKind::OptionalChain { .. } = &e.kind else {
            return;
        };
        let ExprKind::OptionalChain {
            object,
            field_or_method,
            args,
        } = std::mem::replace(&mut e.kind, ExprKind::Error)
        else {
            unreachable!()
        };
        e.kind = postfix(*object, field_or_method, args, e.span);
    }
}

/// `object?` and then `.member` or `.member(args)`, spanned as the parser
/// spans the steps: the `?` from the object's start through the `?`, the
/// access over the whole step.
fn postfix(object: Expr, member: String, args: Option<Vec<CallArg>>, span: Span) -> ExprKind {
    let question = Expr {
        id: NodeId::DUMMY,
        span: Span {
            length: object.span.length + 1,
            ..object.span
        },
        kind: ExprKind::Question(Box::new(object)),
    };
    match args {
        None => ExprKind::FieldAccess {
            object: Box::new(question),
            field: member,
        },
        Some(args) => {
            let end = span.offset + span.length;
            ExprKind::MethodCall {
                object: Box::new(question),
                method: member,
                turbofish: None,
                args,
                args_close_span: Span {
                    offset: end.saturating_sub(1),
                    length: 1,
                    line: span.line,
                    column: span.column + span.length.saturating_sub(1),
                },
            }
        }
    }
}

/// Rewrite every `?.` step in `program` as `?` then `.`.
pub fn postfix_question(program: &mut Program) {
    for item in program.items.iter_mut() {
        walk_item_mut_with(item, &mut Rewrite);
    }
}
