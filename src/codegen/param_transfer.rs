//! Whole-program transfer eligibility for by-value struct parameters
//! (B-2026-08-29-63).
//!
//! Passing an own-heap struct BY VALUE deep-copies its heap fields at the call,
//! even though the argument is MOVED and the caller can never observe it again.
//! The copy is `make_aggregate_param_callee_owned_inst`'s struct arm
//! (`deep_copy_struct_heap_fields_in_place`, [`super::param_own`]), and it costs
//! exactly the heap content: measured at `N * 8` extra bytes for an
//! `N`-element `Vec[i64]` field at every size tried, and **1.96x wall-clock on a
//! hot loop** that passes a 1,024-element struct 200,000 times — the entire gap
//! between passing by value and passing by `ref`.
//!
//! # Why the copy cannot simply be deleted
//!
//! It is load-bearing. `UseAfterMove` is deliberately NON-FATAL on the compiled
//! surface (`kind_blocks_production`, [`crate::ownership`]): `karac check` on a
//! program that reads a binding after moving it prints `warning[ownership]` and
//! then `All checks passed`, and `karac build` emits a working binary. That
//! promise rests on the value the reuse reads still being intact, and at a call
//! ARGUMENT the entry copy is what keeps it intact — the targeted mechanism
//! (`uam_consume_sites` / `uam_defensive_copy`) has no call-argument site at
//! all. B-2026-08-29-64 settled that this stays a warning, so the copy has
//! something to protect and cannot be removed unconditionally.
//!
//! The other half of the obstacle is a STRUCTURAL asymmetry, and it is what this
//! module exists to resolve. The callee's decision is PER-FUNCTION — one body is
//! emitted, so one prologue serves every call site — while the shape that makes
//! a transfer safe is a property of the CALL SITE. Wherever the two disagree,
//! both frames free the same buffer. Measured on a prototype that flipped the
//! callee unconditionally and widened the caller's retraction: three shapes got
//! faster and correct, and four (a reused binding, a fresh call temp, a fresh
//! struct literal, a field argument) became a double free or a use-after-free.
//!
//! # The gate
//!
//! So the fact is computed the way [`super::bce_interproc`] computes its bounds
//! precondition, and for the same reason it gives: **a callee param is
//! transfer-owned only when EVERY call site in the program passes it in a
//! transfer-safe shape.** One site that cannot be proven — or one the walk does
//! not understand — disqualifies the param outright, and it stays on today's
//! entry-copy path with today's cost. There is no per-site specialisation
//! because there is no per-site body.
//!
//! A call site is transfer-safe when the argument is a plain `Identifier` that
//!
//!   * names a binding the enclosing function OWNS (a `let`-bound local, or an
//!     own-mode parameter — never a `ref`/`mut ref` parameter, whose buffers
//!     belong to a frame further up), and
//!   * is NOT in `use_after_move_consume_sites`, i.e. the ownership pass did not
//!     report the source as read again after this move.
//!
//! Every other shape disqualifies: a fresh temp (`eat(mk())`, `eat(R { .. })`)
//! whose caller-side cleanup is registered by
//! `track_inline_owned_aggregate_arg` and would then double with the callee's;
//! a field or index place (`eat(h.r)`) whose parent keeps ownership; a labelled
//! argument, whose positional mapping this walk does not attempt; a defaulted
//! parameter, whose value is a fresh temp minted at the call.
//!
//! A function whose name is ever mentioned OUTSIDE direct-callee position is
//! disqualified wholesale: it may be reached through a call this walk cannot
//! see. `pub` functions and `main` are disqualified for the same reason — their
//! call sites need not be in this program at all.
//!
//! # Soundness
//!
//! The walk MUST NOT miss a call site: a missed site leaves a param admitted
//! that a later frame still owns, which is a silent double free — the exact
//! class this subsystem has the most history with. So [`visit_expr`] and
//! [`visit_stmt`] match every variant of `ExprKind` and `StmtKind` EXHAUSTIVELY,
//! and [`all_regions`] does the same over `Item`, all three with no `_`
//! catch-all arm. That is deliberate and load-bearing: a variant
//! added later fails the build here instead of silently admitting an unsound
//! transfer. Do not add a catch-all to quiet it.
//!
//! Kill switch: `KARAC_MOVE_STRUCT_PARAMS=0` restores the unconditional entry
//! copy for every param.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::{
    Block, Expr, ExprKind, ImplItem, Item, Param, ParsedInterpolationPart, PatternKind, Program,
    Stmt, StmtKind, TraitItem, TypeKind,
};

/// Key for one by-value parameter: `(function name, parameter index)`.
pub(super) type ParamKey = (String, usize);

/// Per-function facts the call walk needs about the frame it is standing in.
#[derive(Default)]
struct FrameOwned {
    /// Names this function may hand over: `let`-bound simple bindings and
    /// own-mode (bare-`Path`) parameters.
    owned: FxHashSet<String>,
    /// Names bound as `ref` / `mut ref` / `Slice` parameters, and every name
    /// introduced by a PATTERN anywhere in the frame. Held separately
    /// and subtracted, so a `let` that shadows a borrow parameter cannot
    /// promote it — the walk has no flow sensitivity to tell the two uses
    /// apart, and guessing wrong here hands away a buffer the caller owns.
    ///
    /// Pattern names are in here rather than in `owned` because a pattern
    /// binding is often a VIEW, not an owner: `match r { S { f } => g(f) }` over
    /// a `ref` scrutinee binds `f` to a field of memory some other frame owns.
    /// The caller-side retraction cannot see that — it removes a `StructDrop`
    /// keyed by the binding's slot, finds none, and silently no-ops while the
    /// callee goes on to free the borrowed buffer. Since the sets are
    /// subtracted, listing pattern names here also settles the only way an
    /// unowned name could have reached `owned` at all: a COLLISION with a
    /// same-named `let` elsewhere in the frame, which the walk has no scoping to
    /// tell apart.
    ///
    /// The cost is a `for x in xs { f(x) }` loop variable, which really is owned
    /// per iteration and is declined anyway. That win is left on the table
    /// deliberately; taking it needs a scoped walk, not a wider set.
    borrowed: FxHashSet<String>,
    /// Names that are ever the ROOT of an assignment target anywhere in the
    /// frame. Subtracted for the same reason as `borrowed`, and it is the
    /// SELF-ASSIGNMENT shape that makes it necessary:
    ///
    /// ```text
    /// p = remake(p);      // `fn remake(b: Plain) -> Plain`
    /// ```
    ///
    /// `p` does not die at the call — it goes on to own the returned value —
    /// and the assignment FREES THE DISPLACED ORIGINAL before storing. Under
    /// transfer the callee has already freed those same buffers, so the two
    /// frees collide: measured as `AddressSanitizer: attempting double-free` in
    /// `asan_roundtrip_reassign_frees_displaced_original`. Retracting the
    /// caller's drop instead only trades it for a leak of the returned value,
    /// which is why this is a PREPASS decline (the callee keeps its entry copy)
    /// rather than a caller-side carve-out — the two sides must not disagree.
    ///
    /// Whole-frame rather than per-site: a binding that is reassigned ANYWHERE
    /// is never transferred. That gives up the win on a mutable binding passed
    /// by value elsewhere in the same function, which is the cheap side of the
    /// trade.
    assigned: FxHashSet<String>,
}

impl FrameOwned {
    fn admits(&self, name: &str) -> bool {
        self.owned.contains(name) && !self.borrowed.contains(name) && !self.assigned.contains(name)
    }
}

/// Mutable state threaded through the walk.
struct Cx<'a> {
    /// Consume-site spans the ownership pass flagged as read-again
    /// (`use_after_move_consume_sites`). An argument at one of these is a
    /// use-after-move whose source still has to be readable, so the entry copy
    /// is exactly what protects it.
    uam: &'a std::collections::HashSet<(usize, usize)>,
    /// Params still believed transfer-safe. Starts full and only shrinks.
    live: FxHashSet<ParamKey>,
    /// Every candidate function's name → its parameter count and whether any
    /// parameter carries a default value.
    fns: FxHashMap<String, (usize, bool)>,
    /// Functions disqualified wholesale (name escaped direct-callee position).
    poisoned: FxHashSet<String>,
}

impl Cx<'_> {
    fn disqualify(&mut self, f: &str, i: usize) {
        self.live.remove(&(f.to_string(), i));
    }

    /// Drop every parameter of `f`. Used when the function's name is seen
    /// outside direct-callee position, where an unseen indirect call could pass
    /// any shape at all.
    fn poison(&mut self, f: &str) {
        if self.fns.contains_key(f) && self.poisoned.insert(f.to_string()) {
            self.live.retain(|(name, _)| name != f);
        }
    }
}

/// Compute the set of `(function, param index)` pairs whose by-value struct
/// parameter may be owned by TRANSFER rather than by entry copy.
///
/// Consumed at two points that must stay in lockstep — the callee prologue
/// (`make_aggregate_param_callee_owned_inst`) and the caller's drop retraction
/// (`move_transferred_struct_arg`). Both consult this same set, which is what
/// makes their agreement structural rather than conventional.
///
/// The result is a permission, not an instruction: the callee still applies its
/// own type predicates (non-shared, copy-supported, not self-referential) before
/// acting on it, and the caller still requires a struct drop actually registered
/// for the binding it retracts.
pub(super) fn compute_transferable_struct_params(
    program: &Program,
    uam_consume_sites: &std::collections::HashSet<(usize, usize)>,
) -> FxHashSet<ParamKey> {
    if std::env::var("KARAC_MOVE_STRUCT_PARAMS").as_deref() == Ok("0") {
        return FxHashSet::default();
    }

    // ── 1. Candidates: by-value params of non-exported free functions ──
    //
    // `pub` and `main` are excluded because their callers need not appear in
    // this program, so "every call site" is not a question this walk can answer
    // for them.
    let mut live: FxHashSet<ParamKey> = FxHashSet::default();
    let mut fns: FxHashMap<String, (usize, bool)> = FxHashMap::default();
    for item in &program.items {
        let Item::Function(f) = item else { continue };
        if f.is_pub || f.name == "main" {
            continue;
        }
        let has_default = f.params.iter().any(|p| p.default_value.is_some());
        fns.insert(f.name.clone(), (f.params.len(), has_default));
        for (i, p) in f.params.iter().enumerate() {
            // Bare `Path` only: `ref T` / `mut ref T` / `Slice[T]` are borrows,
            // and a destructuring pattern has no single binding to hand over.
            if matches!(p.ty.kind, TypeKind::Path(_)) && p.name().is_some() {
                live.insert((f.name.clone(), i));
            }
        }
    }
    if live.is_empty() {
        return live;
    }

    let mut cx = Cx {
        uam: uam_consume_sites,
        live,
        fns,
        poisoned: FxHashSet::default(),
    };

    // ── 2. Walk every body in the program ──
    //
    // Per body: one exhaustive traversal collects the frame's owned names, the
    // direct-callee positions, and every other identifier occurrence; then the
    // call records are classified against that frame. Splitting it this way is
    // what lets a call site anywhere in the body be judged against a `let` that
    // appears later in it.
    for region in all_regions(program) {
        let params: &[Param] = match region {
            Region::Body(p, _) => p,
            Region::Loose(_) => &[],
        };
        let mut fr = frame_of(params);
        let mut calls: Vec<(String, &[crate::ast::CallArg])> = Vec::new();
        let mut callees: FxHashSet<*const Expr> = FxHashSet::default();
        let mut mentions: Vec<(String, *const Expr)> = Vec::new();
        let mut collect = |n| match n {
            Node::Stmt(st) => match &st.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    match &pattern.kind {
                        PatternKind::Binding(b) => {
                            fr.owned.insert(b.clone());
                        }
                        // A DESTRUCTURING let binds views, not a whole value.
                        other => pattern_names(other, &mut fr.borrowed),
                    }
                }
                StmtKind::LetUninit { name, .. } => {
                    fr.owned.insert(name.clone());
                }
                StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
                    if let Some(r) = place_root(target) {
                        fr.assigned.insert(r.to_string());
                    }
                }
                StmtKind::MultiAssign { targets, .. } => {
                    for t in targets {
                        if let Some(r) = place_root(t) {
                            fr.assigned.insert(r.to_string());
                        }
                    }
                }
                _ => {}
            },
            Node::Expr(e) => match &e.kind {
                // Every pattern-introduced name, from every binding construct.
                ExprKind::IfLet { pattern, .. }
                | ExprKind::WhileLet { pattern, .. }
                | ExprKind::For { pattern, .. } => pattern_names(&pattern.kind, &mut fr.borrowed),
                ExprKind::Match { arms, .. } => {
                    for a in arms {
                        pattern_names(&a.pattern.kind, &mut fr.borrowed);
                    }
                }
                ExprKind::Closure { params, .. } => {
                    for cp in params {
                        pattern_names(&cp.pattern.kind, &mut fr.borrowed);
                    }
                }
                ExprKind::Call { callee, args } => {
                    if let ExprKind::Identifier(f) = &callee.kind {
                        callees.insert(&**callee as *const Expr);
                        calls.push((f.clone(), args.as_slice()));
                    }
                }
                ExprKind::Identifier(n) => mentions.push((n.clone(), e as *const Expr)),
                ExprKind::Path { segments, .. } => {
                    if let Some(last) = segments.last() {
                        mentions.push((last.clone(), e as *const Expr));
                    }
                }
                _ => {}
            },
        };
        match region {
            Region::Body(_, b) => visit_block(b, &mut collect),
            Region::Loose(e) => visit_expr(e, &mut collect),
        }
        // A candidate's name anywhere but direct-callee position means an
        // indirect call this walk cannot see — give up every param of it.
        for (n, ptr) in mentions {
            if !callees.contains(&ptr) {
                cx.poison(&n);
            }
        }
        for (f, args) in calls {
            classify_call(&f, args, &fr, &mut cx);
        }
    }
    cx.live
}

/// One walkable region of the program: the frame's parameters (empty when the
/// region is not a function body) and the code to walk.
enum Region<'a> {
    Body(&'a [Param], &'a Block),
    /// A bare initializer expression — a `const` / module binding's value. It
    /// has no frame, so [`FrameOwned::admits`] answers `false` for every name in
    /// it and any call site found there disqualifies its callee. That is the
    /// intended answer, not a shortcoming: a value at this position is not a
    /// local the caller can hand over.
    Loose(&'a Expr),
}

/// Every region of the program a call can appear in.
///
/// EXHAUSTIVE over `Item`, with no `_` arm, for the same reason
/// [`visit_expr`] is exhaustive over `ExprKind`: a call site this function
/// cannot see is a param left admitted that some other frame still owns, which
/// is a silent double free. Enumerating "the items that have bodies" from
/// memory is exactly how one gets missed — `TestCase` carries a `Block` and
/// `ConstDecl` / `ModuleBinding` carry an initializer `Expr`, none of which look
/// like function definitions.
fn all_regions(program: &Program) -> Vec<Region<'_>> {
    let mut out: Vec<Region<'_>> = Vec::new();
    for item in &program.items {
        match item {
            Item::Function(f) => out.push(Region::Body(&f.params, &f.body)),
            Item::ImplBlock(b) => {
                for inner in &b.items {
                    if let ImplItem::Method(m) = inner {
                        out.push(Region::Body(&m.params, &m.body));
                    }
                }
            }
            Item::TraitDef(t) => {
                for inner in &t.items {
                    if let TraitItem::Method(m) = inner {
                        if let Some(body) = &m.body {
                            out.push(Region::Body(&m.params, body));
                        }
                    }
                }
            }
            // A test body is ordinary code and calls ordinary functions; it is
            // simply not spelled `fn`.
            Item::TestCase(t) => out.push(Region::Body(&[], &t.body)),
            Item::ConstDecl(c) => out.push(Region::Loose(&c.value)),
            Item::ModuleBinding(m) => out.push(Region::Loose(&m.value)),
            // Carry no expression that can contain a call: type and effect
            // declarations, imports, aliases, and `extern` signatures (whose
            // bodies live in another language entirely).
            Item::StructDef(_)
            | Item::UnionDef(_)
            | Item::EnumDef(_)
            | Item::TraitAlias(_)
            | Item::MarkerTrait(_)
            | Item::EffectResource(_)
            | Item::EffectGroup(_)
            | Item::EffectVerbDecl(_)
            | Item::LayoutDef(_)
            | Item::UseDecl(_)
            | Item::Import(_)
            | Item::AliasDecl(_)
            | Item::IndependentDecl(_)
            | Item::ExternFunction(_)
            | Item::ExternBlock(_)
            | Item::TypeAlias(_)
            | Item::DistinctType(_) => {}
        }
    }
    out
}

/// Seed a frame from its parameter list. `let`-bound names are added by the
/// traversal above; shadowing is handled by SUBTRACTION rather than scoping — a
/// name that is ever a borrow parameter is never admitted, whatever else it is,
/// because this walk has no flow sensitivity to tell the two uses apart and
/// guessing wrong hands away a buffer the caller still owns.
fn frame_of(params: &[Param]) -> FrameOwned {
    let mut fr = FrameOwned::default();
    for p in params {
        let Some(n) = p.name() else { continue };
        match p.ty.kind {
            TypeKind::Path(_) => fr.owned.insert(n.to_string()),
            _ => fr.borrowed.insert(n.to_string()),
        };
    }
    fr
}

/// Every name a pattern introduces, collected into `out`.
///
/// EXHAUSTIVE over `PatternKind`, no `_` arm, for the reason the expression walk
/// is: a name missed here can reach `admits` through a `let` collision and be
/// handed to a callee that frees memory this frame does not own.
fn pattern_names(k: &PatternKind, out: &mut FxHashSet<String>) {
    match k {
        PatternKind::Binding(n) => {
            out.insert(n.clone());
        }
        PatternKind::AtBinding { name, pattern, .. } => {
            out.insert(name.clone());
            pattern_names(&pattern.kind, out);
        }
        PatternKind::Struct { fields, .. } => {
            for f in fields {
                match &f.pattern {
                    Some(p) => pattern_names(&p.kind, out),
                    // Shorthand `S { f }` binds the field name itself.
                    None => {
                        out.insert(f.name.clone());
                    }
                }
            }
        }
        PatternKind::TupleVariant { patterns, .. }
        | PatternKind::Tuple(patterns)
        | PatternKind::Or(patterns) => {
            for p in patterns {
                pattern_names(&p.kind, out);
            }
        }
        PatternKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            for p in prefix.iter().chain(suffix.iter()) {
                pattern_names(&p.kind, out);
            }
            if let Some(crate::ast::RestPattern::Bound(n)) = rest {
                out.insert(n.clone());
            }
        }
        PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
    }
}

/// The root binding name of a place expression — `h` for `h.r`, `v` for
/// `v[i].f`. `None` when the place is not rooted at a plain identifier.
fn place_root(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root(object),
        _ => None,
    }
}

/// What [`visit_block`] hands its visitor.
enum Node<'a> {
    Stmt(&'a Stmt),
    Expr(&'a Expr),
}

// ── The exhaustive traversal ─────────────────────────────────────

fn visit_block<'a, F: FnMut(Node<'a>)>(b: &'a Block, f: &mut F) {
    for s in &b.stmts {
        visit_stmt(s, f);
    }
    if let Some(fe) = &b.final_expr {
        visit_expr(fe, f);
    }
}

/// EXHAUSTIVE over `StmtKind` — see the module doc. No `_` arm.
fn visit_stmt<'a, F: FnMut(Node<'a>)>(s: &'a Stmt, f: &mut F) {
    f(Node::Stmt(s));
    match &s.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => visit_expr(value, f),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            visit_expr(value, f);
            visit_block(else_block, f);
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } => visit_block(body, f),
        StmtKind::ErrDefer { body, .. } => visit_block(body, f),
        StmtKind::Assign { target, value } => {
            visit_expr(target, f);
            visit_expr(value, f);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                visit_expr(t, f);
            }
            for v in values {
                visit_expr(v, f);
            }
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            visit_expr(target, f);
            visit_expr(value, f);
        }
    }
}

/// EXHAUSTIVE over `ExprKind` — see the module doc. No `_` arm: a variant added
/// later must fail the build here rather than silently admit an unsound
/// transfer.
fn visit_expr<'a, F: FnMut(Node<'a>)>(e: &'a Expr, f: &mut F) {
    f(Node::Expr(e));
    match &e.kind {
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
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(inner, _) = p {
                    visit_expr(inner, f);
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            visit_expr(left, f);
            visit_expr(right, f);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => visit_expr(operand, f),
        ExprKind::OptionalChain { object, args, .. } => {
            visit_expr(object, f);
            if let Some(args) = args {
                for a in args {
                    visit_expr(&a.value, f);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            visit_expr(callee, f);
            for a in args {
                visit_expr(&a.value, f);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            visit_expr(object, f);
            for a in args {
                visit_expr(&a.value, f);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            visit_expr(object, f)
        }
        ExprKind::Index { object, index } => {
            visit_expr(object, f);
            visit_expr(index, f);
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => visit_block(b, f),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            visit_expr(condition, f);
            visit_block(then_block, f);
            if let Some(eb) = else_branch {
                visit_expr(eb, f);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            visit_expr(value, f);
            visit_block(then_block, f);
            if let Some(eb) = else_branch {
                visit_expr(eb, f);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            visit_expr(scrutinee, f);
            for a in arms {
                if let Some(g) = &a.guard {
                    visit_expr(g, f);
                }
                visit_expr(&a.body, f);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            visit_expr(condition, f);
            visit_block(body, f);
        }
        ExprKind::WhileLet { value, body, .. } => {
            visit_expr(value, f);
            visit_block(body, f);
        }
        ExprKind::For { iterable, body, .. } => {
            visit_expr(iterable, f);
            visit_block(body, f);
        }
        ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => visit_block(body, f),
        ExprKind::Closure { body, .. } => visit_expr(body, f),
        ExprKind::Return(v) => {
            if let Some(v) = v {
                visit_expr(v, f);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                visit_expr(v, f);
            }
        }
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for i in items {
                visit_expr(i, f);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            visit_expr(value, f);
            visit_expr(count, f);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                visit_expr(k, f);
                visit_expr(v, f);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for fi in fields {
                visit_expr(&fi.value, f);
            }
            if let Some(sp) = spread {
                visit_expr(sp, f);
            }
        }
        ExprKind::Cast { expr, .. } => visit_expr(expr, f),
        ExprKind::Range { start, end, .. } => {
            if let Some(st) = start {
                visit_expr(st, f);
            }
            if let Some(en) = end {
                visit_expr(en, f);
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            visit_expr(mutex, f);
            visit_block(body, f);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                visit_expr(&b.value, f);
            }
            visit_block(body, f);
        }
    }
}

/// Decide, for one direct call, which of the callee's params survive.
fn classify_call(f: &str, args: &[crate::ast::CallArg], fr: &FrameOwned, cx: &mut Cx) {
    let (nparams, has_default) = cx.fns.get(f).copied().unwrap_or((0, false));

    // A LABELLED argument may sit anywhere in the list, so index `i` no longer
    // names parameter `i`. Rather than reconstruct the mapping, decline the
    // whole call — the param keeps its entry copy and its current cost.
    if args.iter().any(|a| a.label.is_some()) {
        for i in 0..nparams {
            cx.disqualify(f, i);
        }
        return;
    }

    // A parameter left off the call takes its DEFAULT VALUE, which is a fresh
    // temp minted at the call site with no binding to retract.
    if has_default && args.len() < nparams {
        for i in args.len()..nparams {
            cx.disqualify(f, i);
        }
    }

    for (i, a) in args.iter().enumerate() {
        let admit = match &a.value.kind {
            ExprKind::Identifier(n) => {
                fr.admits(n) && !cx.uam.contains(&(a.value.span.offset, a.value.span.length))
            }
            _ => false,
        };
        if !admit {
            cx.disqualify(f, i);
        }
    }
}
