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

/// B-2026-09-10-14 — does this `match` arm MATERIALIZE the whole-value
/// bindings of an `Option`/`Result` pattern, or only read through them?
///
/// `false` only when every top-level sub-pattern is a plain binding (or a
/// wildcard, which binds nothing) AND every bound name is read-through here.
/// A DESTRUCTURE (`Some((a, b))`) answers `true` by construction: its leaves
/// each take an element and each register a body of their own, which is the
/// case the payload-walk disarm was written for and gets right.
///
/// Lives in THIS module rather than in either backend so the two cannot drift:
/// codegen's `suppress_optres_payload_bodies_for_match_scoped` and the
/// interpreter's `record_enum_payload_arm_moves` call the same function on the
/// same AST and reach the same verdict by construction, which is the property
/// an agreed gap needs — both backends were wrong the same way, so only a
/// shared answer can move them together.
pub(crate) fn optres_arm_takes_whole_payload(
    pattern: &crate::ast::Pattern,
    body: &Expr,
    guard: Option<&Expr>,
) -> bool {
    let crate::ast::PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
        return true;
    };
    patterns.iter().any(|sub| match &sub.kind {
        crate::ast::PatternKind::Wildcard => false,
        crate::ast::PatternKind::Binding(n) => {
            !binding_only_read_through(n, body)
                || !guard.is_none_or(|g| binding_only_read_through(n, g))
        }
        _ => true,
    })
}

/// B-2026-09-19-12 — the TOP-LEVEL tuple element indices an arm MATERIALIZES
/// out of a whole-value `Option`/`Result` payload binding.
///
/// The companion to [`optres_arm_takes_whole_payload`], for the gap that
/// predicate deliberately leaves: `Some(t) => { let x = t.0 }` reads through
/// `t` — a `t.0` projection is a read of the binding — so the whole-payload
/// question answers `false` and the scrutinee keeps its walk. That is right
/// about `t` and wrong about ELEMENT 0, which the arm has taken, so the walk
/// must skip that one index and keep the rest.
///
/// Answered by the same walk, with the target one hop deeper: an index is
/// materialized when some mention of `name.<i>` is NOT itself read through.
/// Lives here for the reason the whole-payload predicate does — codegen asks
/// the same question at its move site and the two must not drift.
///
/// A mention count of zero means the arm never touches that element, which is
/// read-through by the same convention and so is not reported.
pub(crate) fn optres_arm_moved_tuple_elems(
    pattern: &crate::ast::Pattern,
    body: &Expr,
    guard: Option<&Expr>,
    arity: usize,
) -> std::collections::BTreeSet<usize> {
    let mut out = std::collections::BTreeSet::new();
    let crate::ast::PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
        return out;
    };
    for sub in patterns {
        let crate::ast::PatternKind::Binding(n) = &sub.kind else {
            continue;
        };
        for i in 0..arity {
            if !tuple_elem_only_read_through(n, i, body)
                || !guard.is_none_or(|g| tuple_elem_only_read_through(n, i, g))
            {
                out.insert(i);
            }
        }
    }
    out
}

/// B-2026-09-19-34 — the PATH-VALUED sibling of
/// [`optres_arm_moved_tuple_elems`]: which PLACES inside a whole-value
/// `Option`/`Result` payload binding does an arm MATERIALIZE, at any depth.
///
/// The index-valued predicate above answers one hop, which is all its own
/// caller — the inline channel's move suppressor — can express. The BOXED
/// channel's caller can express a whole [`crate::codegen`] skip tree, and one
/// hop is not merely coarse there, it is WRONG IN THE OTHER DIRECTION:
/// `Some(t) => { return t.1.0 }` over `Option[(H, (H, H))]` materializes
/// `t.1.0` and nothing else, and reporting its first hop would mask the
/// callee's walk of the whole of `t.1` while the callee still owes `t.1.1`.
/// Measured on that cell: the defect is a DOUBLE of `t.1.0`'s body, and
/// coarsening turns it into a LOSS of `t.1.1`'s. So the answer has to carry
/// the depth the use site carries.
///
/// The verdict per place is the same read-through-vs-materialized question the
/// rest of this module asks, one target deeper: a place is materialized when
/// some mention of it is not itself read through. That falls out of the
/// candidate enumeration: for `return t.1.0` the walk sees `t.1.0` (mentioned
/// once, read through never) AND its prefix `t.1` (mentioned once, as the
/// object of a projection, so read through once), which is exactly the
/// distinction the caller needs.
///
/// PRUNED to the shortest materialized place on each chain, so the answer is
/// canonical: `t.0` and `t.0.1` both materialized means the sibling's body is
/// gone with the parent's, and reporting both would ask the tree to mask a
/// level below one it has already masked whole.
///
/// EMPTY when the binding is CAPTURED by a closure. A capture materializes the
/// binding under the heap-env model however it is spelled inside, so no place
/// answer about it is usable and "cannot narrow" is the honest reply — the
/// same conservatism its two neighbours keep, and the direction that leaves
/// today's behaviour rather than inventing a mask.
///
/// Its only caller today is codegen's boxed-payload arm narrowing, so the
/// DEFAULT feature leg sees it as dead — the same `cfg_attr` its neighbours
/// carry.
#[cfg_attr(not(feature = "llvm"), allow(dead_code))]
pub(crate) fn optres_arm_moved_tuple_paths(
    pattern: &crate::ast::Pattern,
    body: &Expr,
    guard: Option<&Expr>,
) -> Vec<crate::ast::ParamPath> {
    let mut out: Vec<crate::ast::ParamPath> = Vec::new();
    let crate::ast::PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
        return out;
    };
    for sub in patterns {
        let crate::ast::PatternKind::Binding(n) = &sub.kind else {
            continue;
        };
        // A capture takes the whole binding, so nothing finer is answerable.
        let mut cap = Tally::default();
        walk_expr(Target::Bare(n), body, &mut cap);
        if let Some(g) = guard {
            walk_expr(Target::Bare(n), g, &mut cap);
        }
        if cap.captured {
            return Vec::new();
        }
        let mut cand = Tally {
            collect_root: Some(n),
            ..Default::default()
        };
        walk_expr(Target::Bare(n), body, &mut cand);
        if let Some(g) = guard {
            walk_expr(Target::Bare(n), g, &mut cand);
        }
        let mut seen: Vec<crate::ast::ParamPath> = Vec::new();
        for path in cand.places {
            if !seen.contains(&path) {
                seen.push(path);
            }
        }
        for path in seen {
            if !place_only_read_through(n, &path, body)
                || !guard.is_none_or(|g| place_only_read_through(n, &path, g))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out.dedup();
    // Drop anything under a place already masked whole.
    let pruned: Vec<crate::ast::ParamPath> = out
        .iter()
        .filter(|p| {
            !out.iter()
                .any(|q| q.len() < p.len() && p.starts_with(q.as_slice()))
        })
        .cloned()
        .collect();
    pruned
}

/// True iff every mention of the place `name` + `path` inside `e` is a read
/// THROUGH it rather than a use of it; vacuously true when there is none.
///
/// The [`Target::Path`] sibling of [`tuple_elem_only_read_through`], and it
/// deliberately does NOT consult `captured`: a closure capturing the ROOT is
/// handled by its caller, which declines outright, and a tally over a place
/// can never set that flag for anything finer.
fn place_only_read_through(name: &str, path: &[crate::ast::ParamPart], e: &Expr) -> bool {
    let mut t = Tally::default();
    walk_expr(Target::Path(name, path), e, &mut t);
    t.mentions == t.read_through
}

/// B-2026-09-14-18 — the tuple-element indices an arm MATERIALIZES out of a
/// DESTRUCTURED `Option`/`Result` payload (`Some((a, b)) => { return b }`).
///
/// The third member of the family: [`optres_arm_takes_whole_payload`] answers
/// the whole-payload question and reports `true` for a destructure by
/// construction, and [`optres_arm_moved_tuple_elems`] answers it per element
/// for the `Some(t)` + `t.0` spelling. Neither can see INSIDE a destructure,
/// where each element has a NAME of its own rather than an index — so a
/// caller that needs to know which halves of `(a, b)` left had to treat the
/// pattern as all-or-nothing.
///
/// An element counts as moved when its leaf binding is materialized anywhere
/// in the body or the guard, by the same [`binding_only_read_through`] walk
/// the whole-payload predicate uses. A wildcard leaf moves nothing; any other
/// leaf shape (a nested destructure, a literal) is reported as moved, which is
/// the conservative direction here — it masks a body out rather than running
/// one that someone else also runs.
///
/// Returns an empty set for any pattern that is not a single tuple
/// destructure, so a caller can treat "empty" as "nothing to narrow".
///
/// Its only caller today is codegen's boxed-payload arm suppressor, so the
/// DEFAULT feature leg sees it as dead — the same `cfg_attr` its two neighbours
/// below carry, and the same trap CLAUDE.md records for `src/cli.rs`. The
/// interpreter needs no caller here because it has no boxed channel to narrow:
/// a payload's parts are values, not a layout.
#[cfg_attr(not(feature = "llvm"), allow(dead_code))]
pub(crate) fn optres_arm_moved_destructured_elems(
    pattern: &crate::ast::Pattern,
    body: &Expr,
    guard: Option<&Expr>,
) -> (usize, std::collections::BTreeSet<usize>) {
    let mut out = std::collections::BTreeSet::new();
    let crate::ast::PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
        return (0, out);
    };
    let [sub] = patterns.as_slice() else {
        return (0, out);
    };
    let crate::ast::PatternKind::Tuple(leaves) = &sub.kind else {
        return (0, out);
    };
    for (i, leaf) in leaves.iter().enumerate() {
        match &leaf.kind {
            crate::ast::PatternKind::Wildcard => {}
            crate::ast::PatternKind::Binding(n) => {
                if !binding_only_read_through(n, body)
                    || !guard.is_none_or(|g| binding_only_read_through(n, g))
                {
                    out.insert(i);
                }
            }
            _ => {
                out.insert(i);
            }
        }
    }
    (leaves.len(), out)
}

/// True iff every mention of `name.<idx>` inside `e` is a read THROUGH that
/// element rather than a use of it; vacuously true when there is none.
fn tuple_elem_only_read_through(name: &str, idx: usize, e: &Expr) -> bool {
    let mut t = Tally::default();
    walk_expr(Target::Elem(name, idx), e, &mut t);
    !t.captured && t.mentions == t.read_through
}

/// `Block` sibling of [`optres_arm_takes_whole_payload`], for the `if let` /
/// `while let` scopes whose binding lives in a block rather than an arm
/// expression. Deliberately not offered to `let … else`, whose binding escapes
/// into the enclosing scope and is materialized by definition — the same
/// carve-out the interpreter's `let_form_only_reads_payload_through` makes.
pub(crate) fn optres_block_takes_whole_payload(
    pattern: &crate::ast::Pattern,
    block: &Block,
) -> bool {
    let crate::ast::PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
        return true;
    };
    patterns.iter().any(|sub| match &sub.kind {
        crate::ast::PatternKind::Wildcard => false,
        crate::ast::PatternKind::Binding(n) => !binding_only_read_through_block(n, block),
        _ => true,
    })
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

/// `Block` sibling of [`binding_only_read_through`], for the `if let`
/// `then_block` / `while let` body scopes where the binding lives directly in a
/// block rather than in a single arm expression.
pub(crate) fn binding_only_read_through_block(name: &str, b: &Block) -> bool {
    let mut t = Tally::default();
    walk_block(Target::Bare(name), b, &mut t);
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
    walk_block(Target::Bare(name), b, &mut t);
    !t.captured && t.mentions > 0 && t.mentions == t.match_scrutinee
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
/// `Bare` is the original subject, a binding named by identifier. `Elem` is one
/// TOP-LEVEL tuple element of such a binding (`t.0`), which lets the same walk
/// answer the same read-through-vs-materialized question one hop deeper
/// without a second, divergence-prone copy of it. Everything else about the
/// walk is unchanged: it still counts a mention wherever the target appears and
/// explains it wherever the target sits under a projection or as a receiver, so
/// `t.0.name` reads through while `let x = t.0` does not.
#[derive(Clone, Copy)]
enum Target<'a> {
    Bare(&'a str),
    Elem(&'a str, usize),
    /// B-2026-09-19-34 — a PLACE at arbitrary depth under such a binding
    /// (`t.0.1`, `t.1.a`), spelled as the same [`crate::ast::ParamPath`] the
    /// escape analysis and [`crate::codegen`]'s skip trees already use.
    ///
    /// The generalization of `Elem`, which answers one hop only. Both are kept
    /// because `Elem`'s three callers ask a one-hop question by construction
    /// and a `&[ParamPart]` there would be churn; a one-element `Path` and the
    /// matching `Elem` agree on every expression by inspection — the loop
    /// below reduces to `Elem`'s `matches!` when the slice has one
    /// `TupleIndex` in it.
    Path(&'a str, &'a [crate::ast::ParamPart]),
}

impl Target<'_> {
    fn matches(&self, e: &Expr) -> bool {
        match self {
            Target::Bare(name) => is_bare(name, e),
            Target::Elem(name, idx) => matches!(
                &e.kind,
                ExprKind::TupleIndex { object, index }
                    if *index as usize == *idx && is_bare(name, object)
            ),
            Target::Path(name, path) => place_matches(name, path, e),
        }
    }
}

/// Is `e` exactly the place `name` + `path` — `t` `[TupleIndex(0),
/// Field("a")]` against `t.0.a`?
///
/// Walks the chain from the OUTSIDE in, which is the direction the AST nests,
/// so the path is consumed back to front and the recursion bottoms out on the
/// root identifier. An empty path is the bare binding, which makes
/// `Path(n, &[])` and `Bare(n)` the same target by construction rather than by
/// agreement.
fn place_matches(name: &str, path: &[crate::ast::ParamPart], e: &Expr) -> bool {
    let mut cur = e;
    for part in path.iter().rev() {
        match (&cur.kind, part) {
            (ExprKind::FieldAccess { object, field }, crate::ast::ParamPart::Field(f))
                if field == f =>
            {
                cur = object;
            }
            (ExprKind::TupleIndex { object, index }, crate::ast::ParamPart::TupleIndex(i))
                if *index as usize == *i =>
            {
                cur = object;
            }
            _ => return false,
        }
    }
    is_bare(name, cur)
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
        | ExprKind::Cast { expr: x, .. } => walk_expr(tgt, x, t),
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
            for a in args.iter().flatten() {
                walk_expr(tgt, &a.value, t);
            }
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
        | ExprKind::Par(b)
        | ExprKind::Loop { body: b, .. }
        | ExprKind::LabeledBlock { body: b, .. } => walk_block(tgt, b, t),
        // ── Control flow ──────────────────────────────────────────────────
        ExprKind::If {
            condition: head,
            then_block,
            else_branch,
        } => {
            walk_expr(tgt, head, t);
            walk_block(tgt, then_block, t);
            if let Some(x) = else_branch.as_deref() {
                walk_expr(tgt, x, t);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(tgt, value, t);
            walk_block(tgt, then_block, t);
            if let Some(x) = else_branch.as_deref() {
                walk_expr(tgt, x, t);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(tgt, scrutinee, t);
            for a in arms {
                if let Some(g) = &a.guard {
                    walk_expr(tgt, g, t);
                }
                walk_expr(tgt, &a.body, t);
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
            walk_expr(tgt, body, t);
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
        assert!(crate::codegen::consume_class::binding_only_borrowed(
            "r", &e
        ));
    }
}
