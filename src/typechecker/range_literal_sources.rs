//! A range of integer literals carried through iterator closures keeps the
//! default `i64` (design.md §8 Literals) beside the narrower integers the
//! closures combine it with, so the items that come out are `i64` and are
//! refused far from the range: at `collect` into a `Vec[i32]`
//! (`(0..1 << n).map(|i| start ^ i).collect()`, B-2026-11-19-33), or as a
//! tuple handed to `contains` on a `Set[(i32, i32)]` (`(-1..=1).flat_map(..)
//! .filter_map(|(dx, dy)| (1..8).map(|k| (kx + k * dx, ..)).find(|p|
//! placed.contains(p)))`, B-2026-11-19-29). Rust infers each range's type
//! from what its items meet; Kāra's fix is the suffix on each range's first
//! literal, `0i32..1 << n`, which every closure parameter and bound follows.
//!
//! The walk here is syntactic, over the item-carrying adapters only, and it
//! gives up (`None`) on anything that would keep an item `i64` whatever the
//! ranges are: an `i64` binding in the arithmetic, a range bound that is not
//! a literal or a narrower integer, a cast to `i64`. Its answer is the first
//! literal of each range the items come from.

use crate::ast::{BinOp, Expr, ExprKind, UnaryOp};
use crate::resolver::SpanKey;
use crate::token::Span;

use super::inference::resolve_type_var_top;
use super::types::{IntSize, Type, UIntSize};
use super::TypeChecker;

/// A range's first literal: where its suffix goes, and its value (negated
/// when written `-1`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RangeLiteral {
    pub(super) at: Span,
    pub(super) value: i128,
    pub(super) text: String,
}

/// The adapters whose closure parameters are the receiver's items.
const ITEM_CLOSURE_METHODS: &[&str] = &[
    "map",
    "filter",
    "filter_map",
    "flat_map",
    "find",
    "find_map",
    "any",
    "all",
    "take_while",
    "skip_while",
    "inspect",
    "for_each",
    "position",
    "max_by_key",
    "min_by_key",
];

/// The adapters whose items are the closure's result.
const MAPPING_METHODS: &[&str] = &["map", "filter_map", "flat_map", "find_map"];

/// The adapters and terminals whose items (or result) are the receiver's.
const PASSING_METHODS: &[&str] = &[
    "filter",
    "find",
    "take_while",
    "skip_while",
    "inspect",
    "rev",
    "step_by",
    "take",
    "skip",
    "peekable",
    "copied",
    "cloned",
    "iter",
    "into_iter",
    "collect",
    "max",
    "min",
    "last",
    "next",
    "nth",
    "sum",
    "product",
    "unwrap",
    "expect",
    "max_by_key",
    "min_by_key",
];

impl TypeChecker<'_> {
    /// The first literals of the literal ranges `e`'s items come from, with
    /// `local` the closure parameters bound so far in this walk. `None` when
    /// something else keeps them `i64`.
    pub(super) fn range_literal_sources(
        &self,
        e: &Expr,
        local: &mut Vec<(String, Vec<RangeLiteral>)>,
    ) -> Option<Vec<RangeLiteral>> {
        match &e.kind {
            ExprKind::Integer(_, None) => Some(Vec::new()),
            ExprKind::Integer(_, Some(sfx)) => {
                use crate::token::IntSuffix as S;
                (!matches!(sfx, S::I64 | S::I128 | S::U64 | S::U128 | S::Usize)).then(Vec::new)
            }
            ExprKind::Block(b) => match b.final_expr.as_deref() {
                Some(f) if b.stmts.is_empty() => self.range_literal_sources(f, local),
                _ => None,
            },
            ExprKind::Unary {
                op: UnaryOp::Neg | UnaryOp::Ref,
                operand,
            } => self.range_literal_sources(operand, local),
            ExprKind::Identifier(n) => {
                if let Some((_, s)) = local.iter().rev().find(|(m, _)| m == n) {
                    return Some(s.clone());
                }
                if let Some((_, s)) = self.range_param_sources.iter().rev().find(|(m, _)| m == n) {
                    return Some(s.clone());
                }
                let t = self.local_scope.lookup(n.as_str())?.clone();
                self.narrow_type_or_none(&t)
            }
            ExprKind::Tuple(xs) | ExprKind::ArrayLiteral(xs) => {
                let mut out = Vec::new();
                for x in xs {
                    union(&mut out, self.range_literal_sources(x, local)?);
                }
                Some(out)
            }
            ExprKind::Binary { op, left, right } => {
                if !matches!(
                    op,
                    BinOp::Add
                        | BinOp::Sub
                        | BinOp::Mul
                        | BinOp::Div
                        | BinOp::Mod
                        | BinOp::BitAnd
                        | BinOp::BitOr
                        | BinOp::BitXor
                        | BinOp::Shl
                        | BinOp::Shr
                ) {
                    return None;
                }
                let mut out = self.range_literal_sources(left, local)?;
                // A shift's count does not give the result its type.
                if !matches!(op, BinOp::Shl | BinOp::Shr) {
                    union(&mut out, self.range_literal_sources(right, local)?);
                }
                Some(out)
            }
            ExprKind::Range { start, end, .. } => {
                let start = start.as_deref()?;
                let lit = Self::unsuffixed_literal(start)?;
                if let Some(end) = end.as_deref() {
                    // The end follows the start's suffix only when its own
                    // `i64` comes from literals alone.
                    if !self.range_literal_sources(end, local)?.is_empty() {
                        return None;
                    }
                }
                Some(vec![lit])
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                let method = method.as_str();
                if MAPPING_METHODS.contains(&method) {
                    let items = self.range_literal_sources(object, local)?;
                    let [arg] = args.as_slice() else {
                        return None;
                    };
                    let ExprKind::Closure { params, body, .. } = &arg.value.kind else {
                        return None;
                    };
                    let mark = local.len();
                    for p in params {
                        for n in p.pattern.binding_names() {
                            local.push((n, items.clone()));
                        }
                    }
                    let out = self.range_literal_sources(body, local);
                    local.truncate(mark);
                    out
                } else if PASSING_METHODS.contains(&method) {
                    self.range_literal_sources(object, local)
                } else {
                    None
                }
            }
            _ => self.narrow_or_none(e),
        }
    }

    /// `1`, `-1` (a range's first literal, unsuffixed).
    fn unsuffixed_literal(e: &Expr) -> Option<RangeLiteral> {
        match &e.kind {
            ExprKind::Integer(v, None) => Some(RangeLiteral {
                at: Span {
                    offset: e.span.offset + e.span.length,
                    length: 0,
                    ..e.span
                },
                value: *v,
                text: v.to_string(),
            }),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => Self::unsuffixed_literal(operand).map(|l| RangeLiteral {
                value: -l.value,
                text: format!("-{}", l.text),
                ..l
            }),
            _ => None,
        }
    }

    /// An expression whose recorded type is a narrower integer (or no
    /// integer at all) adds no range; an `i64` one keeps the items `i64`.
    fn narrow_or_none(&self, e: &Expr) -> Option<Vec<RangeLiteral>> {
        let t = self.expr_types.get(&SpanKey::from_span(&e.span))?.clone();
        self.narrow_type_or_none(&t)
    }

    fn narrow_type_or_none(&self, t: &Type) -> Option<Vec<RangeLiteral>> {
        match resolve_type_var_top(t, &self.env.substitutions) {
            Type::Int(IntSize::I64 | IntSize::I128)
            | Type::UInt(UIntSize::U64 | UIntSize::U128)
            | Type::TypeVar(_) => None,
            _ => Some(Vec::new()),
        }
    }

    /// Before a method call's arguments are checked: the item closures'
    /// parameters, bound to the ranges the receiver's items come from. The
    /// mark to restore after.
    pub(super) fn enter_range_closures(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[crate::ast::CallArg],
    ) -> usize {
        let mark = self.range_param_sources.len();
        let closures = || {
            args.iter().filter_map(|a| match &a.value.kind {
                ExprKind::Closure { params, .. } => Some(params),
                _ => None,
            })
        };
        if !ITEM_CLOSURE_METHODS.contains(&method) || closures().next().is_none() {
            return mark;
        }
        let Some(items) = self.range_literal_sources(object, &mut Vec::new()) else {
            return mark;
        };
        if items.is_empty() {
            return mark;
        }
        for params in closures() {
            for p in params {
                for n in p.pattern.binding_names() {
                    self.range_param_sources.push((n, items.clone()));
                }
            }
        }
        mark
    }

    /// A use of a closure parameter bound by [`Self::enter_range_closures`]:
    /// a refusal at its span offers the ranges' suffixes.
    pub(super) fn note_range_param_use(&mut self, expr: &Expr) {
        if self.range_param_sources.is_empty() {
            return;
        }
        let ExprKind::Identifier(n) = &expr.kind else {
            return;
        };
        if let Some((_, s)) = self.range_param_sources.iter().rev().find(|(m, _)| m == n) {
            let s = s.clone();
            self.range_source_uses
                .insert(SpanKey::from_span(&expr.span), s);
        }
    }

    /// `xs.collect()` over items that come from literal ranges: remembered
    /// at the call, where a `FromIterator` refusal is reported.
    pub(super) fn note_range_collect(&mut self, object: &Expr, spans: &[Span]) {
        let Some(items) = self.range_literal_sources(object, &mut Vec::new()) else {
            return;
        };
        if items.is_empty() {
            return;
        }
        for s in spans {
            self.range_source_uses
                .insert(SpanKey::from_span(s), items.clone());
        }
    }

    /// The refusal at `span` of `found` where `expected` is wanted, when the
    /// two differ only in `i64`s where one narrower integer is wanted and the
    /// value's items come from literal ranges: records the suffix on each
    /// range's first literal as the fix and answers the advice.
    pub(super) fn range_literal_suffix_fix(
        &mut self,
        span: &Span,
        expected: &Type,
        found: &Type,
    ) -> Option<String> {
        let sources = self
            .range_source_uses
            .get(&SpanKey::from_span(span))?
            .clone();
        let mut target = None;
        if !narrowed_to(expected, found, &mut target) {
            return None;
        }
        let target = target?;
        let (lo, hi) = Self::int_literal_range(&target)?;
        if sources.iter().any(|l| l.value < lo || l.value > hi) {
            return None;
        }
        let t = super::types::type_display(&target);
        let edits = sources
            .iter()
            .map(|l| crate::resolver::TextEdit {
                offset: l.at.offset,
                length: 0,
                replacement: t.clone(),
            })
            .collect();
        self.error_fix_diffs.insert(SpanKey::from_span(span), edits);
        let first = &sources[0].text;
        let ranges = if sources.len() == 1 {
            format!("the range starting `{first}`")
        } else {
            format!(
                "the {} ranges these items come from (the first starting `{first}`)",
                sources.len()
            )
        };
        let each = if sources.len() == 1 {
            "its"
        } else {
            "each range's"
        };
        Some(format!(
            ". The 'i64' comes from {ranges}: a range of literals keeps the default \
             'i64' beside a narrower integer, since an operator or a method does not fix \
             a literal's type (design.md §8 Literals); suffix {each} first literal, \
             `{first}{t}`"
        ))
    }
}

/// Whether `found` is `expected` with some `i64`s where one narrower
/// integer type, set in `target`, is wanted.
fn narrowed_to(expected: &Type, found: &Type, target: &mut Option<Type>) -> bool {
    let peel = |t: &Type| match t {
        Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref().clone(),
        t => t.clone(),
    };
    let (e, f) = (peel(expected), peel(found));
    match (&e, &f) {
        _ if e == f => true,
        (Type::Int(_) | Type::UInt(_), Type::Int(IntSize::I64)) => {
            if matches!(
                e,
                Type::Int(IntSize::I128) | Type::UInt(UIntSize::U64 | UIntSize::U128)
            ) {
                return false;
            }
            match target {
                Some(t) => *t == e,
                None => {
                    *target = Some(e.clone());
                    true
                }
            }
        }
        (Type::Tuple(xs), Type::Tuple(ys)) if xs.len() == ys.len() => {
            xs.iter().zip(ys).all(|(x, y)| narrowed_to(x, y, target))
        }
        (Type::Named { name: n1, args: xs }, Type::Named { name: n2, args: ys })
            if n1 == n2 && xs.len() == ys.len() =>
        {
            xs.iter().zip(ys).all(|(x, y)| narrowed_to(x, y, target))
        }
        (
            Type::Array {
                element: x,
                size: a,
            },
            Type::Array {
                element: y,
                size: b,
            },
        ) if a == b => narrowed_to(x, y, target),
        _ => false,
    }
}

fn union(out: &mut Vec<RangeLiteral>, more: Vec<RangeLiteral>) {
    for l in more {
        if !out.iter().any(|o| o.at == l.at) {
            out.push(l);
        }
    }
}
