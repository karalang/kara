//! A `fold` whose seed takes a narrow integer from a value
//! (`fold((vals[0], 1), |(s, c), k| (s + k, c + 1))` with `vals: Vec[i32]`
//! over `i64` items): the accumulator's slot is the seed's `i32`, and `s + k`
//! is refused as a narrowing whose advice was `as i32`, a truncation of the
//! sum. The seed is what to widen (`vals[0] as i64`), as the literal-seed case
//! already offers a suffix (B-2026-11-18-44).

use super::TypeChecker;
use crate::ast::*;
use crate::resolver::{SpanKey, TextEdit};
use crate::token::Span;
use crate::typechecker::{type_display, Type};

/// A `fold` call's seed and closure, while its arguments are checked.
#[derive(Debug, Clone)]
pub(crate) struct FoldSeedFrame {
    pub seed: Expr,
    pub closure: Expr,
}

impl TypeChecker<'_> {
    /// Before a method call's arguments are checked: a `fold(seed, |..| ..)`
    /// pushes its frame, popped by [`Self::leave_fold_seed`].
    pub(super) fn enter_fold_seed(&mut self, method: &str, args: &[CallArg]) -> bool {
        if method != "fold" || args.len() != 2 {
            return false;
        }
        if !matches!(args[1].value.kind, ExprKind::Closure { .. }) {
            return false;
        }
        self.fold_seed_frames.push(FoldSeedFrame {
            seed: args[0].value.clone(),
            closure: args[1].value.clone(),
        });
        true
    }

    pub(super) fn leave_fold_seed(&mut self, entered: bool) {
        if entered {
            self.fold_seed_frames.pop();
        }
    }

    /// `expr` (an `i64`) refused where a `fold`'s accumulator slot wants
    /// `target`, because the seed put a narrower value there: the edits widen
    /// that seed slot to `source`.
    pub(super) fn fold_seed_widening(
        &self,
        expr: &Expr,
        source: &Type,
        target: &Type,
    ) -> Option<(Vec<TextEdit>, String)> {
        if !matches!(source, Type::Int(_) | Type::UInt(_))
            || !matches!(target, Type::Int(_) | Type::UInt(_))
        {
            return None;
        }
        let inside = |outer: &Span, inner: &Span| {
            outer.offset <= inner.offset
                && inner.offset + inner.length <= outer.offset + outer.length
        };
        for frame in self.fold_seed_frames.iter().rev() {
            let ExprKind::Closure { body, .. } = &frame.closure.kind else {
                continue;
            };
            if !inside(&body.span, &expr.span) {
                continue;
            }
            let mut tail = body.as_ref();
            while let ExprKind::Block(b) = &tail.kind {
                tail = b.final_expr.as_deref()?;
            }
            let slot = match (&tail.kind, &frame.seed.kind) {
                (ExprKind::Tuple(vals), ExprKind::Tuple(seeds)) if vals.len() == seeds.len() => {
                    let i = vals.iter().position(|v| v.span == expr.span)?;
                    &seeds[i]
                }
                _ if tail.span == expr.span => &frame.seed,
                _ => return None,
            };
            // A literal seed is the suffix fix's (`fold_seed_literals`).
            if matches!(
                slot.kind,
                ExprKind::Integer(..) | ExprKind::Unary { .. } | ExprKind::Float(..)
            ) {
                return None;
            }
            let slot_ty = self.expr_types.get(&SpanKey::from_span(&slot.span))?;
            if self.resolved_now(slot_ty) != *target {
                return None;
            }
            let wide = type_display(source);
            let postfix = matches!(
                slot.kind,
                ExprKind::Identifier(_)
                    | ExprKind::Path { .. }
                    | ExprKind::Call { .. }
                    | ExprKind::MethodCall { .. }
                    | ExprKind::FieldAccess { .. }
                    | ExprKind::Index { .. }
                    | ExprKind::TupleIndex { .. }
            );
            let end = slot.span.offset + slot.span.length;
            let mut edits = Vec::new();
            if !postfix {
                edits.push(TextEdit {
                    offset: slot.span.offset,
                    length: 0,
                    replacement: "(".to_string(),
                });
            }
            edits.push(TextEdit {
                offset: end,
                length: 0,
                replacement: if postfix {
                    format!(" as {wide}")
                } else {
                    format!(") as {wide}")
                },
            });
            let shown = crate::formatter::render_expr(slot);
            let why = format!(
                "the `fold`'s accumulator is '{}' because its seed `{shown}` is; widen the seed \
                 (`{shown} as {wide}`) rather than truncate what accumulates",
                type_display(target)
            );
            return Some((edits, why));
        }
        None
    }
}
