//! Rust's array literal carried over: `let mut dp = [0, i32::MIN, i32::MIN];
//! .. let mut next = dp; .. dp = next;` copies a fixed array in Rust. In
//! Kāra `[a, b, c]` is a `Vec`, so the copy is a move, and the reuse of `dp`
//! after it was fixed with `.clone()`, a heap allocation per copy (kata
//! 1262's Rust arm ran 2.60 s with it and 1.40 s with a fixed array). When
//! the list never changes length and only ever indexes, walks, measures or
//! copies, `dp: Array[T, N]` is the same program with Rust's cost, and is the
//! fix offered instead (B-2026-11-20-86).

use crate::ast::{Block, ExprKind, PatternKind, StmtKind, UnaryOp};
use crate::resolver::{SpanKey, TextEdit};
use crate::span_visitor::{for_each_expr_in_block, for_each_stmt_in_block};
use crate::typechecker::{Type, TypeCheckResult};
use rustc_hash::FxHashSet;

/// The annotation `binding: Array[T, N]` at its `let`, and the type as
/// written, when the binding is an unannotated list literal of `Copy`
/// scalars that nothing in `body` grows, shrinks, lends or hands on: it, and
/// every binding copied out of it (`let next = dp`), is only indexed, walked
/// by a `for`, measured with `len()`, or copied whole.
pub(super) fn fixed_array_annotation(
    body: &Block,
    binding: &str,
    tc: &TypeCheckResult,
) -> Option<(TextEdit, String)> {
    // The binding's one `let`, unannotated, bound to a list literal.
    let mut lets = Vec::new();
    for_each_stmt_in_block(body, &mut |s| {
        if let StmtKind::Let {
            pattern, ty, value, ..
        } = &s.kind
        {
            if matches!(&pattern.kind, PatternKind::Binding(n) if n == binding) {
                lets.push((pattern.span, ty.is_some(), value.clone()));
            }
        }
    });
    let [(at, false, value)] = lets.as_slice() else {
        return None;
    };
    // A bare `[a, b, c]` reaches here as `Vec[a, b, c]`; one written
    // `Vec[..]` says it wants a `Vec`, and its first item sits past the
    // `Vec`.
    let elems = match &value.kind {
        ExprKind::ArrayLiteral(elems) => elems,
        ExprKind::PrefixCollectionLiteral { type_name, items } if type_name == "Vec" => {
            match items.first() {
                Some(first) if first.span.offset > value.span.offset + 1 => return None,
                _ => items,
            }
        }
        _ => return None,
    };
    if elems.is_empty() {
        return None;
    }
    let elem = match tc.expr_types.get(&SpanKey::from_span(&value.span))? {
        Type::Named { name, args } if name == "Vec" && args.len() == 1 => args[0].clone(),
        _ => return None,
    };
    if !matches!(
        elem,
        Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
    ) {
        return None;
    }
    // The bindings copied out of it, whole.
    let mut names: FxHashSet<String> = FxHashSet::default();
    names.insert(binding.to_string());
    let mut bad = false;
    loop {
        let mut grew = false;
        for_each_stmt_in_block(body, &mut |s| {
            let StmtKind::Let {
                pattern, ty, value, ..
            } = &s.kind
            else {
                return;
            };
            let PatternKind::Binding(y) = &pattern.kind else {
                return;
            };
            let from_alias = matches!(&value.kind, ExprKind::Identifier(x) if names.contains(x));
            if from_alias && ty.is_none() {
                grew |= names.insert(y.clone());
            } else if y != binding && names.contains(y) {
                // A name of the family bound again to something else.
                bad = true;
            }
        });
        if bad {
            return None;
        }
        if !grew {
            break;
        }
    }
    // Every use is one an `Array` takes as a `Vec` does.
    let mut uses = FxHashSet::default();
    let mut allowed = FxHashSet::default();
    let named = |e: &crate::ast::Expr| match &e.kind {
        ExprKind::Identifier(n) if names.contains(n) => Some(e.span.offset),
        _ => None,
    };
    for_each_expr_in_block(body, &mut |e| {
        if let Some(o) = named(e) {
            uses.insert(o);
        }
        match &e.kind {
            ExprKind::Index { object, index } if !matches!(index.kind, ExprKind::Range { .. }) => {
                allowed.extend(named(object));
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if args.is_empty() && matches!(method.as_str(), "len" | "iter") => {
                allowed.extend(named(object));
            }
            ExprKind::For { iterable, .. } => {
                let it = match &iterable.kind {
                    ExprKind::Unary {
                        op: UnaryOp::Ref,
                        operand,
                    } => operand.as_ref(),
                    _ => iterable.as_ref(),
                };
                allowed.extend(named(it));
            }
            _ => {}
        }
    });
    for_each_stmt_in_block(body, &mut |s| match &s.kind {
        StmtKind::Let { value, .. } => {
            allowed.extend(named(value));
        }
        StmtKind::Assign { target, value } => {
            if let (Some(t), Some(v)) = (named(target), named(value)) {
                allowed.insert(t);
                allowed.insert(v);
            }
        }
        _ => {}
    });
    if !uses.is_subset(&allowed) {
        return None;
    }
    let shown = format!(
        "Array[{}, {}]",
        crate::typechecker::type_display(&elem),
        elems.len()
    );
    Some((
        TextEdit {
            offset: at.offset + at.length,
            length: 0,
            replacement: format!(": {shown}"),
        },
        shown,
    ))
}
