//! v2 core: `let t = v[i];` over a non-`Copy` element is refused, since it
//! would move the element out of the collection (E_INDEX_MOVE_NON_COPY,
//! `docs/core-semantics.md` §3.7). When every later use of `t` only reads
//! it, the repair is the borrow `let t = ref v[i];`, not a copy of the
//! element (B-2026-10-24-18). This finds those `let`s.
//!
//! The borrow holds the collection for as long as `t` is used, so a write
//! to the collection (or a move of it) in that stretch rules the borrow out
//! (§5.6). The stretch is measured in whole statements of the `let`'s block:
//! a loop that holds a use of `t` is held whole.

/// Methods that only read their receiver, so calling one on the collection
/// does not conflict with a borrow of its element. Any other method is taken
/// to write.
const READING_METHODS: &[&str] = &[
    "len",
    "is_empty",
    "get",
    "contains",
    "contains_key",
    "first",
    "last",
    "iter",
    "keys",
    "values",
    "clone",
    "to_string",
    "to_vec",
    "as_slice",
];

use crate::ast::{Block, Expr, ExprKind, Pattern, PatternKind, StmtKind, UnaryOp};
use crate::cfg::{Classification, UseKind};
use crate::index_disjoint::{for_each_block_child, for_each_child_public, Child};
use crate::resolver::SpanKey;
use std::collections::HashSet;

/// The initializer of each un-annotated, non-`mut` `let name = <index>` in
/// `body` whose binding the classifier saw only read.
pub(super) fn read_only_index_lets(body: &Block, c: &Classification, out: &mut HashSet<SpanKey>) {
    for stmt in &body.stmts {
        let StmtKind::Let {
            is_mut: false,
            pattern,
            ty: None,
            value,
        } = &stmt.kind
        else {
            continue;
        };
        // A destructuring `let (a, b) = v[i];` borrows the same way when
        // every part is only read (B-2026-10-24-38).
        let mut names = Vec::new();
        if !matches!(value.kind, ExprKind::Index { .. }) || !simple_bindings(pattern, &mut names) {
            continue;
        }
        // The uses after the `let` are the ones past its initializer.
        let end = value.span.offset + value.span.length;
        let mut uses = Vec::new();
        for name in &names {
            uses_in_block(body, name, &mut uses);
        }
        uses.retain(|k| k.0 >= end);
        if !uses
            .iter()
            .all(|k| matches!(c.kinds.get(k), Some(UseKind::Read)))
        {
            continue;
        }
        let ExprKind::Index { object, .. } = &value.kind else {
            continue;
        };
        // The statements from the `let` to the one holding the last use.
        let last = uses.iter().map(|k| k.0).max().unwrap_or(end);
        let held_to = body
            .stmts
            .iter()
            .map(|s| s.span.offset + s.span.length)
            .chain(
                body.final_expr
                    .iter()
                    .map(|e| e.span.offset + e.span.length),
            )
            .filter(|&e| e > last)
            .min()
            .unwrap_or(last);
        let window = (end, held_to.max(last + 1));
        if let Some(root) = place_root(object) {
            let mut written = false;
            writes_in_block(body, root, window, c, &mut written);
            if written {
                continue;
            }
        }
        out.insert(SpanKey::from_span(&value.span));
    }
    for_each_block_child(body, &mut |ch| nested(ch, c, out));
}

/// The names a pattern of plain bindings, `_` and tuples of them binds, or
/// `false` for any other pattern.
fn simple_bindings<'p>(p: &'p Pattern, out: &mut Vec<&'p str>) -> bool {
    match &p.kind {
        PatternKind::Binding(n) => {
            out.push(n.as_str());
            true
        }
        PatternKind::Wildcard => true,
        PatternKind::Tuple(ps) => ps.iter().all(|p| simple_bindings(p, out)),
        _ => false,
    }
}

fn nested(ch: Child<'_>, c: &Classification, out: &mut HashSet<SpanKey>) {
    match ch {
        Child::Block(b) => read_only_index_lets(b, c, out),
        Child::Expr(e) => for_each_child_public(e, &mut |ch| nested(ch, c, out)),
    }
}

fn uses_in_block(b: &Block, name: &str, out: &mut Vec<SpanKey>) {
    for_each_block_child(b, &mut |ch| uses_in(ch, name, out));
}

fn uses_in(ch: Child<'_>, name: &str, out: &mut Vec<SpanKey>) {
    match ch {
        Child::Block(b) => uses_in_block(b, name, out),
        Child::Expr(e) => uses_in_expr(e, name, out),
    }
}

fn uses_in_expr(e: &Expr, name: &str, out: &mut Vec<SpanKey>) {
    if matches!(&e.kind, ExprKind::Identifier(n) if n == name) {
        out.push(SpanKey::from_span(&e.span));
    }
    for_each_child_public(e, &mut |ch| uses_in(ch, name, out));
}

/// The binding a place is rooted at.
fn place_root(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n),
        ExprKind::SelfValue => Some("self"),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root(object),
        _ => None,
    }
}

fn in_window(e: &Expr, window: (usize, usize)) -> bool {
    e.span.offset >= window.0 && e.span.offset < window.1
}

/// Whether `root` is written or moved inside `window` of `b`.
fn writes_in_block(
    b: &Block,
    root: &str,
    window: (usize, usize),
    c: &Classification,
    w: &mut bool,
) {
    for stmt in &b.stmts {
        let targets: Vec<&Expr> = match &stmt.kind {
            StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
                vec![target]
            }
            StmtKind::MultiAssign { targets, .. } => targets.iter().collect(),
            _ => Vec::new(),
        };
        *w |= targets
            .into_iter()
            .any(|t| in_window(t, window) && place_root(t) == Some(root));
    }
    for_each_block_child(b, &mut |ch| writes_in(ch, root, window, c, w));
}

fn writes_in(ch: Child<'_>, root: &str, window: (usize, usize), c: &Classification, w: &mut bool) {
    match ch {
        Child::Block(b) => writes_in_block(b, root, window, c, w),
        Child::Expr(e) => writes_in_expr(e, root, window, c, w),
    }
}

fn writes_in_expr(e: &Expr, root: &str, window: (usize, usize), c: &Classification, w: &mut bool) {
    if in_window(e, window) {
        let args = match &e.kind {
            ExprKind::Identifier(n) if n == root => {
                *w |= matches!(
                    c.kinds.get(&SpanKey::from_span(&e.span)),
                    Some(UseKind::Consume)
                );
                None
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                *w |=
                    place_root(object) == Some(root) && !READING_METHODS.contains(&method.as_str());
                Some(args)
            }
            ExprKind::Call { args, .. } => Some(args),
            ExprKind::Unary {
                op: UnaryOp::MutRef,
                operand,
            } => {
                *w |= place_root(operand) == Some(root);
                None
            }
            _ => None,
        };
        *w |= args.is_some_and(|args| {
            args.iter()
                .any(|a| a.mut_marker && place_root(&a.value) == Some(root))
        });
    }
    for_each_child_public(e, &mut |ch| writes_in(ch, root, window, c, w));
}
