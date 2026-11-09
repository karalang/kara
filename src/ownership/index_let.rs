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
//!
//! When `t`'s fields are written afterwards (`t.status = 2;`), a copy would
//! take the writes away from the element, so on a writable collection the
//! repair is `let t = mut ref v[i];` (B-2026-11-07-15). That borrow is
//! exclusive: any use of the collection while `t` is still to be used rules
//! it out.

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

use crate::ast::{
    Block, Expr, ExprKind, Function, Pattern, PatternKind, SelfParam, StmtKind, TypeKind, UnaryOp,
};
use crate::cfg::{Classification, UseKind};
use crate::index_disjoint::{for_each_block_child, for_each_child_public, Child};
use crate::resolver::SpanKey;
use std::collections::{HashMap, HashSet};

/// A `let t = v[i];` on a writable collection whose binding's fields are
/// written afterwards (B-2026-11-07-15).
#[derive(Debug, Clone, Default)]
pub struct WrittenIndexLet {
    /// Whether `mut ref v[i]` is a borrow the rest of the block allows: the
    /// collection is not used again while `t` is still to be used.
    pub borrowable: bool,
    /// The offset of `t` in each write through it, where the refusal of a
    /// write to a binding without `mut` is reported.
    pub writes: Vec<usize>,
}

/// What [`index_lets`] finds in one function.
pub(super) struct IndexLets<'a> {
    /// The roots the function may write: a `mut ref` parameter, `self` in a
    /// `mut ref self` method, and every `let mut` binding.
    writable: HashSet<String>,
    pub read_only: &'a mut HashSet<SpanKey>,
    pub written: &'a mut HashMap<SpanKey, WrittenIndexLet>,
    /// The `read_only` lets on a writable collection, with every later use
    /// of the binding in `writes`: one the ownership pass finds writing
    /// (`t.items.push(x)` with `t` not `mut`) makes the let a written one.
    pub only_read: &'a mut HashMap<SpanKey, WrittenIndexLet>,
    /// The `read_only` lets whose initializer is a whole element `v[i]`.
    pub elements: &'a mut HashSet<SpanKey>,
}

impl<'a> IndexLets<'a> {
    pub(super) fn new(
        f: &Function,
        read_only: &'a mut HashSet<SpanKey>,
        written: &'a mut HashMap<SpanKey, WrittenIndexLet>,
        only_read: &'a mut HashMap<SpanKey, WrittenIndexLet>,
        elements: &'a mut HashSet<SpanKey>,
    ) -> Self {
        let mut writable = HashSet::new();
        if matches!(f.self_param, Some(SelfParam::MutRef)) {
            writable.insert("self".to_string());
        }
        for p in &f.params {
            if let (TypeKind::MutRef(_), PatternKind::Binding(n)) = (&p.ty.kind, &p.pattern.kind) {
                writable.insert(n.clone());
            }
        }
        mut_lets(&f.body, &mut writable);
        IndexLets {
            writable,
            read_only,
            written,
            only_read,
            elements,
        }
    }

    fn writable(&self, root: &str) -> bool {
        self.writable
            .contains(root.split('.').next().unwrap_or(root))
    }
}

fn mut_lets(b: &Block, out: &mut HashSet<String>) {
    for stmt in &b.stmts {
        if let StmtKind::Let {
            is_mut: true,
            pattern,
            ..
        } = &stmt.kind
        {
            out.extend(pattern.binding_name_spans().into_iter().map(|(n, _)| n));
        }
    }
    for_each_block_child(b, &mut |ch| mut_lets_in(ch, out));
}

fn mut_lets_in(ch: Child<'_>, out: &mut HashSet<String>) {
    match ch {
        Child::Block(b) => mut_lets(b, out),
        Child::Expr(e) => for_each_child_public(e, &mut |ch| mut_lets_in(ch, out)),
    }
}

/// The initializer of each un-annotated, non-`mut` `let name = <index>` in
/// `body` whose binding the classifier saw only read (into `read_only`), or
/// only read and written through (into `written`).
pub(super) fn index_lets(body: &Block, c: &Classification, out: &mut IndexLets<'_>) {
    for stmt in &body.stmts {
        // A `let … else` whose pattern binds parts of an element
        // (`let Json.Array(xs) = v[i] else { … };`) moves them out the same
        // way, and borrows the same way when they are only read
        // (B-2026-11-09-27).
        let (pattern, value) = match &stmt.kind {
            StmtKind::Let {
                is_mut: false,
                pattern,
                ty: None,
                value,
            }
            | StmtKind::LetElse {
                pattern,
                ty: None,
                value,
                ..
            } => (pattern, value),
            _ => continue,
        };
        // A destructuring `let (a, b) = v[i];` borrows the same way when
        // every part is only read (B-2026-10-24-38).
        let mut names = Vec::new();
        // A field of an element (`let s = v[i].samples;`) moves out of the
        // collection the same way, and borrows the same way (B-2026-11-04-3).
        let Some(indexed) = indexed_place(value) else {
            continue;
        };
        if !simple_bindings(pattern, &mut names) {
            continue;
        }
        // The uses after the `let` are the ones past its initializer.
        let end = value.span.offset + value.span.length;
        let mut uses = Vec::new();
        for name in &names {
            uses_in_block(body, name, &mut uses);
        }
        uses.retain(|k| k.0 >= end);
        // The writes through a binding (`t.f = x`, `t.n += 1`, `mut t.v`):
        // the classifier sees a field store as a `Reassign` of `t.f`, and a
        // compound one as a read.
        let mut writes = HashSet::new();
        for name in &names {
            writes_through_block(body, name, &mut writes);
        }
        if !uses.iter().all(|k| match c.kinds.get(k) {
            Some(UseKind::Read) => true,
            Some(UseKind::Reassign) => writes.contains(&k.0),
            _ => false,
        }) {
            continue;
        }
        let written = uses.iter().any(|k| writes.contains(&k.0));
        let ExprKind::Index { object, .. } = &indexed.kind else {
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
        // An element of a temporary (`vec!["LR", "RL"][k]`, `f()[i]`) is
        // not borrowed: the temporary drops at the end of the `let`, so
        // `ref` there is an E0516, and the `.clone()` fix stands
        // (B-2026-11-03-23).
        let Some(root) = place_root(object) else {
            continue;
        };
        let key = SpanKey::from_span(&value.span);
        let uses: Vec<usize> = uses.iter().map(|k| k.0).collect();
        // A collection the function may not write keeps the copy when the
        // binding is written: the writes can only be meant for it.
        let mutable = pattern_is_binding(pattern) && out.writable(&root);
        if written {
            if !mutable {
                continue;
            }
            let borrowable = !used_while_held(body, &root, end, &uses);
            let mut writes: Vec<usize> = uses
                .iter()
                .copied()
                .filter(|u| writes.contains(u))
                .collect();
            writes.sort_unstable();
            out.written
                .insert(key, WrittenIndexLet { borrowable, writes });
            continue;
        }
        let mut written = false;
        writes_in_block(body, &root, window, c, &mut written);
        if written {
            continue;
        }
        out.read_only.insert(key);
        if std::ptr::eq(indexed, value) {
            out.elements.insert(key);
        }
        if mutable {
            let borrowable = !used_while_held(body, &root, end, &uses);
            out.only_read.insert(
                key,
                WrittenIndexLet {
                    borrowable,
                    writes: uses,
                },
            );
        }
    }
    for_each_block_child(body, &mut |ch| nested(ch, c, out));
}

fn pattern_is_binding(p: &Pattern) -> bool {
    matches!(p.kind, PatternKind::Binding(_))
}

/// The offset of `name` at the root of each place written in `b`: an
/// assignment's target (`name.f = x`, `name.n += 1`, not `name = x`), and a
/// `mut` argument or `mut ref` operand rooted at it.
fn writes_through_block(b: &Block, name: &str, out: &mut HashSet<usize>) {
    for stmt in &b.stmts {
        let targets: Vec<&Expr> = match &stmt.kind {
            StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
                vec![target]
            }
            StmtKind::MultiAssign { targets, .. } => targets.iter().collect(),
            _ => Vec::new(),
        };
        for t in targets {
            if !matches!(t.kind, ExprKind::Identifier(_)) {
                root_ident(t, name, out);
            }
        }
    }
    for_each_block_child(b, &mut |ch| writes_through(ch, name, out));
}

fn writes_through(ch: Child<'_>, name: &str, out: &mut HashSet<usize>) {
    match ch {
        Child::Block(b) => writes_through_block(b, name, out),
        Child::Expr(e) => {
            match &e.kind {
                ExprKind::Unary {
                    op: UnaryOp::MutRef,
                    operand,
                } => root_ident(operand, name, out),
                ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. } => {
                    for a in args.iter().filter(|a| a.mut_marker) {
                        root_ident(&a.value, name, out);
                    }
                }
                _ => {}
            }
            for_each_child_public(e, &mut |ch| writes_through(ch, name, out));
        }
    }
}

/// Records the offset of the identifier a place hangs off, when it is `name`.
fn root_ident(e: &Expr, name: &str, out: &mut HashSet<usize>) {
    match &e.kind {
        ExprKind::Identifier(n) if n == name => {
            out.insert(e.span.offset);
        }
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => root_ident(object, name, out),
        _ => {}
    }
}

/// Whether `root` is used, after `from`, at a point a later use in `uses`
/// can follow: a `mut ref` borrow of an element of `root` held by those uses
/// would conflict with it (§5.6). A use in one branch of an `if` or `match`
/// is not followed by one in another branch of it, unless a loop that
/// starts after `from` holds both.
fn used_while_held(b: &Block, root: &str, from: usize, uses: &[usize]) -> bool {
    let mut mentions = Vec::new();
    mentions_in_block(b, root, &mut mentions);
    let mut shape = Shape::default();
    shape_of_block(b, &mut shape);
    mentions.into_iter().filter(|&m| m >= from).any(|m| {
        uses.iter().any(|&u| {
            let looped = shape
                .loops
                .iter()
                .any(|&(s, e)| s >= from && (s..e).contains(&m) && (s..e).contains(&u));
            looped || (u > m && !shape.apart(m, u))
        })
    })
}

/// The branch and loop extents of a block.
#[derive(Default)]
struct Shape {
    /// Each `if` / `match`, as the extents of its branches.
    branches: Vec<Vec<(usize, usize)>>,
    loops: Vec<(usize, usize)>,
}

impl Shape {
    /// Whether `a` and `b` lie in different branches of one `if` or `match`.
    fn apart(&self, a: usize, b: usize) -> bool {
        self.branches.iter().any(|arms| {
            let arm_of = |x: usize| arms.iter().position(|&(s, e)| (s..e).contains(&x));
            matches!((arm_of(a), arm_of(b)), (Some(i), Some(j)) if i != j)
        })
    }
}

fn extent(s: &crate::token::Span) -> (usize, usize) {
    (s.offset, s.offset + s.length)
}

fn shape_of_block(b: &Block, out: &mut Shape) {
    for_each_block_child(b, &mut |ch| shape_of(ch, out));
}

fn shape_of(ch: Child<'_>, out: &mut Shape) {
    let e = match ch {
        Child::Block(b) => return shape_of_block(b, out),
        Child::Expr(e) => e,
    };
    match &e.kind {
        ExprKind::If {
            then_block,
            else_branch,
            ..
        }
        | ExprKind::IfLet {
            then_block,
            else_branch,
            ..
        } => {
            let mut arms = vec![extent(&then_block.span)];
            arms.extend(else_branch.iter().map(|e| extent(&e.span)));
            out.branches.push(arms);
        }
        ExprKind::Match { arms, .. } => {
            out.branches
                .push(arms.iter().map(|a| extent(&a.span)).collect());
        }
        ExprKind::While { .. }
        | ExprKind::WhileLet { .. }
        | ExprKind::For { .. }
        | ExprKind::Loop { .. } => out.loops.push(extent(&e.span)),
        _ => {}
    }
    for_each_child_public(e, &mut |ch| shape_of(ch, out));
}

/// The offset of each use of a place that can overlap `root`: a variable,
/// `self`, or `self.f`.
fn mentions_in_block(b: &Block, root: &str, out: &mut Vec<usize>) {
    for_each_block_child(b, &mut |ch| mentions_in(ch, root, out));
}

fn mentions_in(ch: Child<'_>, root: &str, out: &mut Vec<usize>) {
    let e = match ch {
        Child::Block(b) => return mentions_in_block(b, root, out),
        Child::Expr(e) => e,
    };
    let at = match &e.kind {
        ExprKind::FieldAccess { object, field } if matches!(object.kind, ExprKind::SelfValue) => {
            if overlaps(Some(format!("self.{field}")), root) {
                out.push(e.span.offset);
            }
            return;
        }
        ExprKind::Identifier(n) => Some(n.clone()),
        ExprKind::SelfValue => Some("self".to_string()),
        _ => None,
    };
    if overlaps(at, root) {
        out.push(e.span.offset);
    }
    for_each_child_public(e, &mut |ch| mentions_in(ch, root, out));
}

/// The element access a `let` initializer reads: the `v[i]` of `v[i]`,
/// `v[i].f` or `v[i].f.0`, or None when the initializer is no such place.
fn indexed_place(e: &Expr) -> Option<&Expr> {
    match &e.kind {
        ExprKind::Index { .. } => Some(e),
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            indexed_place(object)
        }
        _ => None,
    }
}

/// The names a pattern of plain bindings, `_`, and tuples, variants and
/// structs of them binds, or `false` for any other pattern.
fn simple_bindings<'p>(p: &'p Pattern, out: &mut Vec<&'p str>) -> bool {
    match &p.kind {
        PatternKind::Binding(n) => {
            out.push(n.as_str());
            true
        }
        PatternKind::Wildcard | PatternKind::Literal(_) => true,
        PatternKind::Tuple(ps) | PatternKind::TupleVariant { patterns: ps, .. } => {
            ps.iter().all(|p| simple_bindings(p, out))
        }
        PatternKind::Struct { fields, .. } => fields.iter().all(|f| match &f.pattern {
            Some(p) => simple_bindings(p, out),
            None => {
                out.push(f.name.as_str());
                true
            }
        }),
        _ => false,
    }
}

fn nested(ch: Child<'_>, c: &Classification, out: &mut IndexLets<'_>) {
    match ch {
        Child::Block(b) => index_lets(b, c, out),
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
/// The variable a place hangs off, and under `self` the field too: in a
/// method every place is `self`'s, so `self.series[k]` and `self.out` are
/// told apart (B-2026-11-04-3).
fn place_root(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        ExprKind::SelfValue => Some("self".to_string()),
        ExprKind::FieldAccess { object, field } if matches!(object.kind, ExprKind::SelfValue) => {
            Some(format!("self.{field}"))
        }
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root(object),
        _ => None,
    }
}

/// Whether a write to the place under `written` can touch the one under
/// `root`: the same root, or one inside the other (`self` and `self.out`).
fn overlaps(written: Option<String>, root: &str) -> bool {
    written.is_some_and(|w| {
        w == root
            || root
                .strip_prefix(w.as_str())
                .is_some_and(|r| r.starts_with('.'))
            || w.strip_prefix(root).is_some_and(|r| r.starts_with('.'))
    })
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
            .any(|t| in_window(t, window) && overlaps(place_root(t), root));
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
            ExprKind::Identifier(n) if overlaps(Some(n.clone()), root) => {
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
                *w |= overlaps(place_root(object), root)
                    && !READING_METHODS.contains(&method.as_str());
                Some(args)
            }
            ExprKind::Call { args, .. } => Some(args),
            ExprKind::Unary {
                op: UnaryOp::MutRef,
                operand,
            } => {
                *w |= overlaps(place_root(operand), root);
                None
            }
            _ => None,
        };
        *w |= args.is_some_and(|args| {
            args.iter()
                .any(|a| a.mut_marker && overlaps(place_root(&a.value), root))
        });
    }
    for_each_child_public(e, &mut |ch| writes_in(ch, root, window, c, w));
}
