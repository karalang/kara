//! Read-through vs. materializing classification for a pattern binding
//! (B-2026-08-28-67).
//!
//! A `match` / `if let` / `while let` arm that binds an enum payload out asks
//! the drop machinery one question: **does the arm take the payload, or does it
//! only look at it?** The two answers place the payload's `Drop` body in
//! different places — with the arm binding, or with the scrutinee it came from
//! — and the two backends must pick the same one.
//!
//! [`binding_only_read_through`] answers it structurally: `true` when every
//! occurrence of the binding is a READ THROUGH it — a `b.field` / `b.0` /
//! `b[i]` projection, a `b.m()` method receiver, or a write through one of
//! those — and `false` as soon as the binding appears as a bare value anywhere
//! (a call argument, a `let` right-hand side, an aggregate element, a returned
//! or tail value). A binding mentioned nowhere at all is read-through by
//! definition.
//!
//! This is deliberately NOT `codegen::consume_class::binding_only_borrowed`,
//! which answers a different question — *does ownership transfer away?* — and
//! models a free-function argument as entry-copied and therefore NON-consuming.
//! That is right for its callers and wrong here: `keep(r)` transfers nothing,
//! yet all three backends agree the arm binding owns `r` and drops it at the
//! arm's end, because the value was materialized. The predicates disagree on
//! exactly that shape, which is why this is its own function rather than a
//! reuse.
//!
//! ## Why the walk is exhaustive
//!
//! The verdict is used to SUPPRESS drop bookkeeping, so an occurrence the walk
//! fails to see reads as "no mention" — i.e. read-through — which is the
//! unsafe direction. `consume_class`'s `walk_exprs` has a `_ => {}` fallback
//! and does not descend into `InterpolatedStringLit`, so `f"{r.id}"` — the
//! single most common way a kata touches a payload — would be invisible to it.
//! The match below is exhaustive over `ExprKind` with no catch-all, so a new
//! variant is a compile error here rather than a silent under-count.

use crate::ast::{Block, Expr, ExprKind, ParsedInterpolationPart, Stmt, StmtKind};

/// True iff every occurrence of `name` inside `e` is a read THROUGH the
/// binding rather than a use OF it. See the module docs.
pub(crate) fn binding_only_read_through(name: &str, e: &Expr) -> bool {
    let mut t = Tally::default();
    walk_expr(name, e, &mut t);
    t.verdict()
}

/// `Block` sibling of [`binding_only_read_through`], for the `if let`
/// `then_block` / `while let` body scopes where the binding lives directly in a
/// block rather than in a single arm expression.
pub(crate) fn binding_only_read_through_block(name: &str, b: &Block) -> bool {
    let mut t = Tally::default();
    walk_block(name, b, &mut t);
    t.verdict()
}

/// True iff `name` is mentioned at least once inside `e` and EVERY mention is
/// the direct scrutinee of a nested `match` / `if let` / `while let`
/// (B-2026-08-30-52 (b)).
///
/// STRICTLY NARROWER than [`binding_only_read_through`], and the difference is
/// the whole point. That predicate explains a `b.field` projection, which for a
/// HEAP-BOXED payload is the field-move-out shape a borrow classification must
/// not swallow — the box is its own ownership regime, and admitting it
/// wholesale was measured to break five tests (see
/// `scrutinee_is_readonly_inline_optres_local`). A nested `match` over the
/// binding takes nothing by itself: whether it does is decided separately, by
/// the escape walk over the inner arms.
#[cfg_attr(not(feature = "llvm"), allow(dead_code))]
pub(crate) fn binding_only_nested_match_scrutinee(name: &str, e: &Expr) -> bool {
    let mut t = Tally::default();
    walk_expr(name, e, &mut t);
    !t.captured && t.mentions > 0 && t.mentions == t.match_scrutinee
}

/// `Block` sibling of [`binding_only_nested_match_scrutinee`].
///
/// Both carry `allow(dead_code)` off the `llvm` leg: unlike the two predicates
/// above — which the INTERPRETER calls — their only consumer is
/// `codegen::control_flow_match`, so on CI's default leg they are genuinely
/// unreferenced and `-D warnings` fails the build (the B-2026-08-18-23 trap
/// CLAUDE.md documents).
#[cfg_attr(not(feature = "llvm"), allow(dead_code))]
pub(crate) fn binding_only_nested_match_scrutinee_block(name: &str, b: &Block) -> bool {
    let mut t = Tally::default();
    walk_block(name, b, &mut t);
    !t.captured && t.mentions > 0 && t.mentions == t.match_scrutinee
}

#[derive(Default)]
struct Tally {
    /// Every `Identifier(name)` node seen, at any depth.
    mentions: usize,
    /// The subset of those that sit directly under a projection or as a
    /// method receiver.
    read_through: usize,
    /// The subset of those that ARE the scrutinee of a nested `match` /
    /// `if let` / `while let`. Tallied separately from `read_through` because
    /// its one consumer needs the narrower set — see
    /// [`binding_only_nested_match_scrutinee`].
    match_scrutinee: usize,
    /// A closure body mentions `name`. Closures capture by value under the
    /// heap-env model, so the capture materializes the binding however it is
    /// spelled inside — `|| r.id` takes `r` with it.
    captured: bool,
}

impl Tally {
    fn verdict(&self) -> bool {
        !self.captured && self.mentions == self.read_through
    }
}

fn is_bare(name: &str, e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Identifier(n) if n == name)
}

fn walk_expr(name: &str, e: &Expr, t: &mut Tally) {
    if is_bare(name, e) {
        t.mentions += 1;
    }
    // A bare `name` in one of these positions is read, not taken. Counting the
    // position rather than rewriting the recursion keeps the two tallies over
    // the SAME node set, so `mentions == read_through` means exactly "every
    // mention was explained".
    match &e.kind {
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. }
        | ExprKind::MethodCall { object, .. }
        | ExprKind::OptionalChain { object, .. }
            if is_bare(name, object) =>
        {
            t.read_through += 1;
        }
        _ => {}
    }
    match &e.kind {
        ExprKind::Match {
            scrutinee: head, ..
        }
        | ExprKind::IfLet { value: head, .. }
        | ExprKind::WhileLet { value: head, .. }
            if is_bare(name, head) =>
        {
            t.match_scrutinee += 1;
        }
        _ => {}
    }
    match &e.kind {
        // ── Leaves ────────────────────────────────────────────────────────
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::ByteStringLit(_)
        | ExprKind::Bool(_)
        | ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}
        // ── One child ─────────────────────────────────────────────────────
        ExprKind::Unary { operand: x, .. }
        | ExprKind::Question(x)
        | ExprKind::FieldAccess { object: x, .. }
        | ExprKind::TupleIndex { object: x, .. }
        | ExprKind::Cast { expr: x, .. } => walk_expr(name, x, t),
        // ── Two children ──────────────────────────────────────────────────
        ExprKind::Binary {
            left: a, right: b, ..
        }
        | ExprKind::NilCoalesce { left: a, right: b }
        | ExprKind::Pipe { left: a, right: b }
        | ExprKind::Index {
            object: a,
            index: b,
        }
        | ExprKind::RepeatLiteral {
            value: a, count: b, ..
        } => {
            walk_expr(name, a, t);
            walk_expr(name, b, t);
        }
        // ── Calls ─────────────────────────────────────────────────────────
        ExprKind::Call { callee: obj, args }
        | ExprKind::MethodCall {
            object: obj, args, ..
        } => {
            walk_expr(name, obj, t);
            for a in args {
                walk_expr(name, &a.value, t);
            }
        }
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(name, object, t);
            for a in args.iter().flatten() {
                walk_expr(name, &a.value, t);
            }
        }
        // ── Sequences ─────────────────────────────────────────────────────
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items {
                walk_expr(name, x, t);
            }
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                walk_expr(name, k, t);
                walk_expr(name, v, t);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                walk_expr(name, &f.value, t);
            }
            if let Some(s) = spread.as_deref() {
                walk_expr(name, s, t);
            }
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(x, _) = p {
                    walk_expr(name, x, t);
                }
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                walk_expr(name, s, t);
            }
            if let Some(x) = end.as_deref() {
                walk_expr(name, x, t);
            }
        }
        // ── Blocks ────────────────────────────────────────────────────────
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::Loop { body: b, .. }
        | ExprKind::LabeledBlock { body: b, .. } => walk_block(name, b, t),
        // ── Control flow ──────────────────────────────────────────────────
        ExprKind::If {
            condition: head,
            then_block,
            else_branch,
        } => {
            walk_expr(name, head, t);
            walk_block(name, then_block, t);
            if let Some(x) = else_branch.as_deref() {
                walk_expr(name, x, t);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(name, value, t);
            walk_block(name, then_block, t);
            if let Some(x) = else_branch.as_deref() {
                walk_expr(name, x, t);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(name, scrutinee, t);
            for a in arms {
                if let Some(g) = &a.guard {
                    walk_expr(name, g, t);
                }
                walk_expr(name, &a.body, t);
            }
        }
        ExprKind::While {
            condition: head,
            body,
            ..
        }
        | ExprKind::WhileLet {
            value: head, body, ..
        }
        | ExprKind::For {
            iterable: head,
            body,
            ..
        }
        | ExprKind::Lock {
            mutex: head, body, ..
        } => {
            walk_expr(name, head, t);
            walk_block(name, body, t);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                walk_expr(name, &b.value, t);
            }
            walk_block(name, body, t);
        }
        ExprKind::Return(x) => {
            if let Some(x) = x.as_deref() {
                walk_expr(name, x, t);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(x) = value.as_deref() {
                walk_expr(name, x, t);
            }
        }
        // ── Capture ───────────────────────────────────────────────────────
        ExprKind::Closure { body, .. } => {
            let before = t.mentions;
            walk_expr(name, body, t);
            if t.mentions > before {
                t.captured = true;
            }
        }
    }
}

fn walk_block(name: &str, b: &Block, t: &mut Tally) {
    for s in &b.stmts {
        walk_stmt(name, s, t);
    }
    if let Some(e) = b.final_expr.as_deref() {
        walk_expr(name, e, t);
    }
}

fn walk_stmt(name: &str, s: &Stmt, t: &mut Tally) {
    match &s.kind {
        StmtKind::Let { value, .. } => walk_expr(name, value, t),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(name, value, t);
            walk_block(name, else_block, t);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => walk_block(name, body, t),
        // A write THROUGH the binding (`b.f = x`) mutates in place and takes
        // nothing, so the target is walked on the same footing as a read.
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(name, target, t);
            walk_expr(name, value, t);
        }
        StmtKind::MultiAssign { targets, values } => {
            for x in targets {
                walk_expr(name, x, t);
            }
            for x in values {
                walk_expr(name, x, t);
            }
        }
        StmtKind::Expr(e) => walk_expr(name, e, t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a snippet as a function body and hand back its tail expression, so
    /// cases can be written as natural arm bodies. Mirrors
    /// `codegen::consume_class`'s helper of the same shape.
    fn arm_body(src: &str) -> Expr {
        let full = format!("fn f() {{ {src} }}");
        let parsed = crate::parse(&full);
        assert!(parsed.errors.is_empty(), "parse {src}: {:?}", parsed.errors);
        let crate::ast::Item::Function(func) = &parsed.program.items[0] else {
            panic!("expected fn");
        };
        func.body
            .final_expr
            .as_deref()
            .cloned()
            .unwrap_or_else(|| panic!("no tail expr in: {src}"))
    }

    fn read_through(src: &str) -> bool {
        binding_only_read_through("r", &arm_body(src))
    }

    #[test]
    fn projections_and_receivers_are_reads() {
        for src in [
            "{ println(\"x\") }",              // never mentioned at all
            "{ r.id }",                        // field
            "{ r.0 }",                         // tuple index
            "{ r.items[1] }",                  // projection chain
            "{ r[2] }",                        // index of the binding
            "{ r.get() }",                     // method receiver, no args
            "{ r.at(1i64) }",                  // method receiver with a scalar arg
            "{ println(f\"v{r.id}\") }",       // inside an interpolation hole
            "{ r.id = 3i64; println(\"w\") }", // a WRITE through it moves nothing
            "{ if r.id == 1i64 { println(\"a\") } else { println(\"b\") } }",
            "{ let n = r.id; println(f\"n{n}\") }",
        ] {
            assert!(read_through(src), "should be read-through: {src}");
        }
    }

    #[test]
    fn any_bare_value_position_materializes() {
        for src in [
            "{ let m = r; m.id }",          // move into a new binding
            "{ keep(r) }",                  // free-fn argument — see module docs
            "{ println(f\"h{keep(r)}\") }", // ... including inside an f-string
            "{ v.push(r) }",                // method ARGUMENT, not receiver
            "{ W { r: r } }",               // aggregate field
            "{ (r, 1i64) }",                // tuple element
            "{ [r] }",                      // array element
            "{ return r }",                 // escapes the frame
            "{ r }",                        // the arm's own value
            "{ q[r] }",                     // used AS an index
            "{ let f = || r.id; f() }",     // captured by a closure
        ] {
            assert!(!read_through(src), "should be materialized: {src}");
        }
    }

    /// The shape that makes this a separate predicate from
    /// `consume_class::binding_only_borrowed`: that one models a free-function
    /// argument as entry-copied and therefore NON-consuming, which is right for
    /// its own callers and the wrong answer here. Pinning the disagreement
    /// stops a later "why are there two of these?" cleanup from collapsing them.
    #[test]
    fn disagrees_with_binding_only_borrowed_on_a_free_fn_argument() {
        let e = arm_body("{ keep(r) }");
        assert!(!binding_only_read_through("r", &e));
        #[cfg(feature = "llvm")]
        assert!(crate::codegen::consume_class::binding_only_borrowed(
            "r", &e
        ));
    }
}
