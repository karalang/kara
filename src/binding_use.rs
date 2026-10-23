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

/// B-2026-09-20-19 — how many times `name` appears in `body` as a bare VALUE
/// rather than as the base of a read (`name.f`, `name.0`, `name.m()`).
/// A capture counts as unbounded: it takes the binding however it is spelled.
pub(crate) fn bare_value_mentions(name: &str, body: &Expr) -> usize {
    let mut t = Tally::default();
    walk_expr(Target::Bare(name), body, &mut t);
    if t.captured {
        return usize::MAX;
    }
    t.mentions - t.read_through
}

/// B-2026-09-20-17 — can a place taken off a REBIND of an arm binding (`let u
/// = t; return u.0`) be masked out of the payload walk for every path out of
/// the arm, as far as the arm's EXITS go?
///
/// The mask is static: once the arm narrows the walk, every exit compiled
/// after it runs the narrowed walk, so an arm that can leave before its move
/// would lose the part's body on that path. The answer is yes only when the
/// arm's one exit is a `return` that is its last statement, or it has none.
/// Where a rebind place is taken under a branch is the other half of the same
/// question, and needs the payload's types to tell an owning part from a
/// scalar leaf: see [`optres_arm_rebind_places_taken_on_some_paths`].
///
/// The arm binding ITSELF is not held to this: `if k { return t.0; }` narrows
/// today and its conditional corner is B-2026-09-27-19's. A rebind must not
/// extend that corner to a spelling that did not have it.
pub(crate) fn optres_arm_exits_only_at_its_end(body: &Expr) -> bool {
    let mut t = Tally::default();
    walk_expr(Target::Bare(""), body, &mut t);
    let last_is_return = match &body.kind {
        ExprKind::Block(b) => match (b.final_expr.as_deref(), b.stmts.last()) {
            (Some(fe), _) => matches!(fe.kind, ExprKind::Return(_)),
            (None, Some(st)) => {
                matches!(&st.kind, StmtKind::Expr(e) if matches!(e.kind, ExprKind::Return(_)))
            }
            (None, None) => false,
        },
        ExprKind::Return(_) => true,
        _ => false,
    };
    t.exits <= usize::from(last_is_return)
}

/// True iff every occurrence of `name` inside `e` is a read THROUGH the
/// binding rather than a use OF it. See the module docs.
pub(crate) fn binding_only_read_through(name: &str, e: &Expr) -> bool {
    let mut t = Tally::default();
    walk_expr(Target::Bare(name), e, &mut t);
    t.verdict()
}

/// B-2026-08-31-3 — [`binding_only_read_through`] with a callee-mode oracle.
///
/// Identical except that a bare `name` handed to a FREE call whose parameter at
/// that position is a borrow counts as a read rather than a use, which is what
/// design.md's use-predicate table says ("`f(v)` where `f`'s parameter is
/// declared `ref` or `mut ref` | read"). `borrows` is called with the callee
/// expression and the argument index; answering `true` for a callee it cannot
/// resolve is the safe direction for its one caller, a hard error.
///
/// METHOD calls stay mode-blind, deliberately: resolving a receiver's method
/// needs the receiver's type, which this walk does not have, and the
/// conservative answer there is "a use", which can only under-accept.
pub(crate) fn binding_only_read_through_borrow_aware(
    name: &str,
    e: &Expr,
    borrows: &BorrowOracle<'_>,
) -> bool {
    let mut t = Tally {
        borrows: Some(borrows),
        ..Default::default()
    };
    walk_expr(Target::Bare(name), e, &mut t);
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
    walk_expr(Target::Bare(name), e, &mut t);
    !t.captured && t.mentions > 0 && t.mentions == t.match_scrutinee
}

/// B-2026-10-03-51 — is `name` the direct scrutinee of a nested `match` /
/// `if let` / `while let` ANYWHERE in `e`? The any-mention sibling of
/// [`binding_only_nested_match_scrutinee`]: a binding that is destructured
/// further hands what it holds to the inner arms, whatever else it does.
/// B-2026-10-03-54 narrowed it to a nested `match` with an arm that takes
/// what it binds; one whose arms only read leaves the leaf with the outer
/// place.
#[cfg_attr(not(feature = "llvm"), allow(dead_code))]
pub(crate) fn binding_is_nested_match_scrutinee(name: &str, e: &Expr) -> bool {
    let mut t = Tally::default();
    walk_expr(Target::Bare(name), e, &mut t);
    t.match_scrutinee_taking > 0
}

/// B-2026-08-31-3 — "does the callee at this argument position take a borrow?",
/// answered by whoever has the signatures. Named rather than spelled inline so
/// the three places that pass it around agree, and so `Tally` stays readable.
type BorrowOracle<'a> = dyn Fn(&Expr, usize) -> bool + 'a;

#[derive(Default)]
struct Tally<'a> {
    /// B-2026-08-31-3 — optional callee-mode oracle. `Some(f)` makes a bare
    /// `name` in a free-call ARGUMENT position count as a read-through when
    /// `f(callee, idx)` says the parameter at that index is a borrow.
    ///
    /// The default walk is mode-BLIND and tallies every call argument as a use
    /// of the binding, which is right for its own callers (they ask "was the
    /// value materialized?", and a callee that borrows still forces the arm to
    /// have something to lend). It is wrong for a caller asking design.md's
    /// use-predicate question — "`f(v)` where `f`'s parameter is declared `ref`
    /// or `mut ref` | read" — and `println(s)` is the shape that proves it: the
    /// single most common thing an arm does with a payload, and a consume under
    /// the blind walk.
    borrows: Option<&'a BorrowOracle<'a>>,
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
    /// B-2026-10-03-54 — the subset of `match_scrutinee` whose nested
    /// construct hands something it binds on (see
    /// [`binding_is_nested_match_scrutinee`]).
    match_scrutinee_taking: usize,
    /// A closure body mentions `name`. Closures capture by value under the
    /// heap-env model, so the capture materializes the binding however it is
    /// spelled inside — `|| r.id` takes `r` with it.
    captured: bool,
    /// B-2026-09-19-34 — when `Some(name)`, every PLACE rooted at `name` that
    /// the walk passes is recorded in `places`, whatever its depth.
    ///
    /// Collection rides on THIS walk rather than on a visitor of its own
    /// because the candidate set has to be complete over the same node set the
    /// verdict is computed on: a second visitor that missed an `ExprKind` would
    /// silently drop a candidate, and a dropped candidate is a part that never
    /// gets masked — the defect, not a conservative answer.
    collect_root: Option<&'a str>,
    /// Every place rooted at `collect_root`, with duplicates and with every
    /// PREFIX of a longer chain, since the recursion visits each sub-chain as
    /// a node in its own right. The caller filters; this only enumerates.
    places: Vec<crate::ast::ParamPath>,
    /// B-2026-09-20-17 — how many branches, loop bodies or short-circuited
    /// operands the walk is currently inside. Not every path reaches a node
    /// counted at a positive depth.
    cond_depth: usize,
    /// The subsets of `mentions` / `read_through` counted at a positive
    /// [`Self::cond_depth`]: a target materialized on SOME paths only has
    /// `cond_mentions > cond_read_through`.
    cond_mentions: usize,
    cond_read_through: usize,
    /// Every `return`, `?`, `break` and `continue` the walk passed, closures
    /// aside — each one a path that may leave before a later node runs.
    exits: usize,
}

impl Tally<'_> {
    fn verdict(&self) -> bool {
        !self.captured && self.mentions == self.read_through
    }
}

fn is_bare(name: &str, e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Identifier(n) if n == name)
}

/// B-2026-09-19-12 — what the walk is tallying mentions OF.
///
/// `Bare` is the subject, a binding named by identifier. The deeper places
/// (`t.0`, `t.0.1`) the walk once also tallied were asked only by the legacy
/// backend, which M3 removed.
#[derive(Clone, Copy)]
enum Target<'a> {
    Bare(&'a str),
}

impl Target<'_> {
    fn matches(&self, e: &Expr) -> bool {
        match self {
            Target::Bare(name) => is_bare(name, e),
        }
    }
}

/// `y.f.0` → `("y", [Field(f), TupleIndex(0)])`; a bare `y` gives an empty
/// path. `None` for anything that is not a field / tuple-index chain over an
/// identifier.
///
/// The local twin of `ast::items::place_chain_root_and_path`, which is private
/// to that module. Copied rather than exported because the two answer for
/// different subjects — that one decomposes a place in a RETURN position over
/// a parameter, this one every place in an arm body over a pattern binding —
/// and a shared helper would tie this module's candidate enumeration to that
/// one's evolution for no shared caller.
fn place_root_and_path(e: &Expr) -> Option<(&str, crate::ast::ParamPath)> {
    let mut chain: crate::ast::ParamPath = Vec::new();
    let mut cur = e;
    loop {
        match &cur.kind {
            ExprKind::FieldAccess { object, field } => {
                chain.push(crate::ast::ParamPart::Field(field.clone()));
                cur = object;
            }
            ExprKind::TupleIndex { object, index } => {
                chain.push(crate::ast::ParamPart::TupleIndex(*index as usize));
                cur = object;
            }
            ExprKind::Identifier(n) => {
                chain.reverse();
                return Some((n.as_str(), chain));
            }
            _ => return None,
        }
    }
}

fn walk_expr(tgt: Target<'_>, e: &Expr, t: &mut Tally<'_>) {
    if let Some(root) = t.collect_root {
        if let Some((r, path)) = place_root_and_path(e) {
            if r == root && !path.is_empty() {
                t.places.push(path);
            }
        }
    }
    if tgt.matches(e) {
        t.mentions += 1;
        if t.cond_depth > 0 {
            t.cond_mentions += 1;
        }
    }
    if matches!(
        &e.kind,
        ExprKind::Return(_)
            | ExprKind::Question(_)
            | ExprKind::Break { .. }
            | ExprKind::Continue { .. }
    ) {
        t.exits += 1;
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
            if tgt.matches(object) =>
        {
            t.read_through += 1;
            if t.cond_depth > 0 {
                t.cond_read_through += 1;
            }
        }
        _ => {}
    }
    match &e.kind {
        ExprKind::Match {
            scrutinee: head, ..
        }
        | ExprKind::IfLet { value: head, .. }
        | ExprKind::WhileLet { value: head, .. }
            if tgt.matches(head) =>
        {
            t.match_scrutinee += 1;
            // B-2026-10-03-54 — a nested `match` whose every arm only READS
            // what it binds hands nothing on: `match o { Some(i) => i.id, .. }`.
            // `if let` / `while let` stay counted, as before.
            let takes = match &e.kind {
                ExprKind::Match { arms, .. } => arms.iter().any(|arm| {
                    crate::cfg::pattern_bindings(&arm.pattern).iter().any(|b| {
                        !binding_only_read_through(b, &arm.body)
                            || !arm
                                .guard
                                .as_ref()
                                .is_none_or(|g| binding_only_read_through(b, g))
                            || binding_is_nested_match_scrutinee(b, &arm.body)
                    })
                }),
                _ => true,
            };
            if takes {
                t.match_scrutinee_taking += 1;
            }
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
        | ExprKind::Cast { expr: x, .. } => walk_expr(tgt, x, t),
        // ── Two children ──────────────────────────────────────────────────
        ExprKind::Binary {
            op: crate::ast::BinOp::And | crate::ast::BinOp::Or,
            left: a,
            right: b,
        }
        | ExprKind::NilCoalesce { left: a, right: b } => {
            walk_expr(tgt, a, t);
            t.cond_depth += 1;
            walk_expr(tgt, b, t);
            t.cond_depth -= 1;
        }
        ExprKind::Binary {
            left: a, right: b, ..
        }
        | ExprKind::Pipe { left: a, right: b }
        | ExprKind::Index {
            object: a,
            index: b,
        }
        | ExprKind::RepeatLiteral {
            value: a, count: b, ..
        } => {
            walk_expr(tgt, a, t);
            walk_expr(tgt, b, t);
        }
        // ── Calls ─────────────────────────────────────────────────────────
        ExprKind::Call { callee: obj, args } => {
            walk_expr(tgt, obj, t);
            for (i, a) in args.iter().enumerate() {
                // B-2026-08-31-3 — a borrow-position argument is a READ. Only
                // with an oracle: without one this falls through to the mention
                // tally exactly as before.
                if tgt.matches(&a.value) && t.borrows.is_some_and(|f| f(obj, i)) {
                    t.mentions += 1;
                    t.read_through += 1;
                    if t.cond_depth > 0 {
                        t.cond_mentions += 1;
                        t.cond_read_through += 1;
                    }
                    continue;
                }
                walk_expr(tgt, &a.value, t);
            }
        }
        ExprKind::MethodCall {
            object: obj, args, ..
        } => {
            walk_expr(tgt, obj, t);
            for a in args {
                walk_expr(tgt, &a.value, t);
            }
        }
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(tgt, object, t);
            t.cond_depth += 1;
            for a in args.iter().flatten() {
                walk_expr(tgt, &a.value, t);
            }
            t.cond_depth -= 1;
        }
        // ── Sequences ─────────────────────────────────────────────────────
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items {
                walk_expr(tgt, x, t);
            }
        }
        ExprKind::MapLiteral { entries: pairs, .. } => {
            for (k, v) in pairs {
                walk_expr(tgt, k, t);
                walk_expr(tgt, v, t);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                walk_expr(tgt, &f.value, t);
            }
            if let Some(s) = spread.as_deref() {
                walk_expr(tgt, s, t);
            }
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(x, _) = p {
                    walk_expr(tgt, x, t);
                }
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                walk_expr(tgt, s, t);
            }
            if let Some(x) = end.as_deref() {
                walk_expr(tgt, x, t);
            }
        }
        // ── Blocks ────────────────────────────────────────────────────────
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => walk_block(tgt, b, t),
        ExprKind::Loop { body: b, .. } | ExprKind::LabeledBlock { body: b, .. } => {
            t.cond_depth += 1;
            walk_block(tgt, b, t);
            t.cond_depth -= 1;
        }
        // ── Control flow ──────────────────────────────────────────────────
        ExprKind::If {
            condition: head,
            then_block,
            else_branch,
        } => {
            walk_expr(tgt, head, t);
            t.cond_depth += 1;
            walk_block(tgt, then_block, t);
            if let Some(x) = else_branch.as_deref() {
                walk_expr(tgt, x, t);
            }
            t.cond_depth -= 1;
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(tgt, value, t);
            t.cond_depth += 1;
            walk_block(tgt, then_block, t);
            if let Some(x) = else_branch.as_deref() {
                walk_expr(tgt, x, t);
            }
            t.cond_depth -= 1;
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(tgt, scrutinee, t);
            t.cond_depth += 1;
            for a in arms {
                if let Some(g) = &a.guard {
                    walk_expr(tgt, g, t);
                }
                walk_expr(tgt, &a.body, t);
            }
            t.cond_depth -= 1;
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
        } => {
            walk_expr(tgt, head, t);
            t.cond_depth += 1;
            walk_block(tgt, body, t);
            t.cond_depth -= 1;
        }
        ExprKind::Lock {
            mutex: head, body, ..
        } => {
            walk_expr(tgt, head, t);
            walk_block(tgt, body, t);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                walk_expr(tgt, &b.value, t);
            }
            walk_block(tgt, body, t);
        }
        ExprKind::Return(x) => {
            if let Some(x) = x.as_deref() {
                walk_expr(tgt, x, t);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(x) = value.as_deref() {
                walk_expr(tgt, x, t);
            }
        }
        // ── Capture ───────────────────────────────────────────────────────
        ExprKind::Closure { body, .. } => {
            let before = t.mentions;
            // A `return` inside a closure leaves the closure, not the arm.
            let exits = t.exits;
            walk_expr(tgt, body, t);
            t.exits = exits;
            if t.mentions > before {
                t.captured = true;
            }
        }
    }
}

fn walk_block(tgt: Target<'_>, b: &Block, t: &mut Tally<'_>) {
    for s in &b.stmts {
        walk_stmt(tgt, s, t);
    }
    if let Some(e) = b.final_expr.as_deref() {
        walk_expr(tgt, e, t);
    }
}

fn walk_stmt(tgt: Target<'_>, s: &Stmt, t: &mut Tally<'_>) {
    match &s.kind {
        StmtKind::Let { value, .. } => walk_expr(tgt, value, t),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(tgt, value, t);
            walk_block(tgt, else_block, t);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => walk_block(tgt, body, t),
        // A write THROUGH the binding (`b.f = x`) mutates in place and takes
        // nothing, so the target is walked on the same footing as a read.
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(tgt, target, t);
            walk_expr(tgt, value, t);
        }
        StmtKind::MultiAssign { targets, values } => {
            for x in targets {
                walk_expr(tgt, x, t);
            }
            for x in values {
                walk_expr(tgt, x, t);
            }
        }
        StmtKind::Expr(e) => walk_expr(tgt, e, t),
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
        assert!(crate::consume_class::binding_only_borrowed("r", &e));
    }
}
