//! An item closure's parameter that holds what an earlier `map` closure
//! returned: `.map(|x| { sum += x; sum }).fold(0i32, |low, s| low.min(s))`.
//! `s` is each value of `sum`, so a refusal of `s` as the `i64` that `sum`'s
//! literal kept is fixed on `sum` (`sum: i32`), as a refusal of `sum` itself
//! already was (B-2026-11-26-8). While the call's arguments are checked, the
//! parameter's name carries `sum`'s entry in `literal_lets`.

use super::{LiteralLet, TypeChecker};
use crate::ast::*;

/// The entries [`TypeChecker::enter_item_literal_params`] replaced, to put
/// back once the call's arguments are checked.
pub(crate) type SavedLiteralParams = Vec<(String, Option<LiteralLet>)>;

impl TypeChecker<'_> {
    /// Which parameter of `method`'s closure argument (at which argument)
    /// is the item: `fold(seed, |acc, x| ..)` takes it second.
    fn item_param_slot(method: &str, nargs: usize) -> Option<(usize, usize)> {
        match (method, nargs) {
            ("fold", 2) => Some((1, 1)),
            (
                "map" | "filter" | "for_each" | "any" | "all" | "find" | "take_while"
                | "skip_while" | "inspect" | "filter_map" | "position" | "min_by_key"
                | "max_by_key",
                1,
            ) => Some((0, 0)),
            _ => None,
        }
    }

    /// The literal binding each item of `object` is a copy of: `object` is
    /// a `map` whose closure ends in that binding, read through adaptors
    /// that pass the items on unchanged.
    fn item_literal_source(&self, object: &Expr) -> Option<LiteralLet> {
        let mut at = object;
        loop {
            let ExprKind::MethodCall {
                object: inner,
                method,
                args,
                ..
            } = &at.kind
            else {
                return None;
            };
            match method.as_str() {
                "filter" | "rev" | "skip" | "take" | "step_by" | "peekable" | "inspect"
                | "take_while" | "skip_while" => at = inner,
                "map" if args.len() == 1 => {
                    let ExprKind::Closure { body, .. } = &args[0].value.kind else {
                        return None;
                    };
                    let tail = match &body.kind {
                        ExprKind::Block(block) => block.final_expr.as_deref()?,
                        _ => body,
                    };
                    let ExprKind::Identifier(t) = &tail.kind else {
                        return None;
                    };
                    return self
                        .literal_lets
                        .get(t.as_str())
                        .filter(|e| !e.suffix && e.depth == 0 && e.root == *t)
                        .cloned();
                }
                _ => return None,
            }
        }
    }

    /// Before `object.method(args)`'s arguments are checked: the item
    /// parameter of its closure, when the items are copies of a literal
    /// binding, carries that binding's entry. Undone by
    /// [`Self::leave_item_literal_params`].
    pub(super) fn enter_item_literal_params(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> SavedLiteralParams {
        let Some((arg, slot)) = Self::item_param_slot(method, args.len()) else {
            return Vec::new();
        };
        let ExprKind::Closure { params, .. } = &args[arg].value.kind else {
            return Vec::new();
        };
        let Some(PatternKind::Binding(name)) = params.get(slot).map(|p| &p.pattern.kind) else {
            return Vec::new();
        };
        let Some(entry) = self.item_literal_source(object) else {
            return Vec::new();
        };
        let old = self.literal_lets.insert(name.clone(), entry);
        vec![(name.clone(), old)]
    }

    pub(super) fn leave_item_literal_params(&mut self, saved: SavedLiteralParams) {
        for (name, old) in saved {
            match old {
                Some(e) => {
                    self.literal_lets.insert(name, e);
                }
                None => {
                    self.literal_lets.remove(&name);
                }
            }
        }
    }
}
