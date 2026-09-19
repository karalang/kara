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
//!     report the source as read again after this move, and
//!   * was NOT RC-FALLBACK PROMOTED by that pass, which is its OTHER answer for
//!     a source that outlives its consume and the one it gives for a consume
//!     inside a LOOP (B-2026-09-07-29). See `FrameOwned::rc_promoted`: the
//!     caller's retraction cannot fire against a `{i64 rc, T}` box handle, so
//!     admitting such a site is a double free once per trip.
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
    /// Names the ownership pass RC-FALLBACK PROMOTED in this frame
    /// (`OwnershipCheckResult::rc_values`, reaching here through
    /// `Codegen::drop_rc.rc_fallback_fns`). Subtracted for the same reason
    /// `Cx::uam` excludes a use-after-move consume site, and it is the SECOND
    /// HALF of that same condition rather than a new rule (B-2026-09-07-29).
    ///
    /// The gate's own statement of transfer-safety is "names a binding the
    /// enclosing function owns AND that the ownership pass did not report as
    /// read again after this move". A consume the source outlives has TWO
    /// possible answers from that pass, not one: report `UseAfterMove` — the
    /// `uam_consume_sites` half, already excluded — or RC-FALLBACK PROMOTE the
    /// binding, which is what it does for a consume inside a LOOP (`while i <
    /// 3 { takep(t); .. }`, the shape whose note reads `RC fallback inserted
    /// for 't' (direct re-use after consume)`). Only the first was subtracted,
    /// so the second admitted the param.
    ///
    /// Admitting it is a double free per TRIP, because the caller's half of the
    /// bargain cannot be paid. A promoted binding's alloca holds a `{i64 rc, T}`
    /// box HANDLE and its cleanup is an `RcDec` on the box, so
    /// `move_transferred_struct_arg`'s retraction — a `StructDrop`/`UserDrop`
    /// scan keyed on the binding — matches nothing and silently no-ops, exactly
    /// as its own doc says it will ("a no-op when no struct drop was registered
    /// for the binding"). The callee then takes buffers the box never gave up.
    /// Measured at -O0 and -O2 and on the JIT: 25 allocs against 26 / 27 / 28 /
    /// 30 frees at 1 / 2 / 3 / 5 trips, `free(): double free detected in tcache
    /// 2` before the program can print, against an interpreter that prints its
    /// output and exits clean.
    ///
    /// Nor can the box give the buffer up: the binding is promoted PRECISELY
    /// because it is read again on the next iteration, so the box has to go on
    /// owning it — the same conclusion B-2026-09-07-23 reached one shape over,
    /// where the answer was likewise to leave the retaining owner alone and
    /// change the consumer's side.
    ///
    /// Declining is the whole fix here, and it is cheap: the param falls back to
    /// today's entry copy, which is what a `Drop`-bearing struct in this exact
    /// shape already does (`struct_param_transfer_eligible` declines a type that
    /// reaches a user `Drop`) — and that spelling is measurably clean at every
    /// trip count, which is the control that says the entry-copy path handles
    /// this shape correctly and only the transfer path did not.
    ///
    /// Whole-frame rather than per-site, like `borrowed` and `assigned` above:
    /// the set is per-FUNCTION in the ownership result, and a binding promoted
    /// anywhere in a frame is never transferred from it.
    rc_promoted: FxHashSet<String>,
}

impl FrameOwned {
    fn admits(&self, name: &str) -> bool {
        self.owned.contains(name)
            && !self.borrowed.contains(name)
            && !self.assigned.contains(name)
            && !self.rc_promoted.contains(name)
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
    rc_fallback_bindings: &std::collections::HashMap<String, std::collections::HashSet<String>>,
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
    for (fn_key, region) in all_regions(program) {
        let params: &[Param] = match region {
            Region::Body(p, _) => p,
            Region::Loose(_) => &[],
        };
        let mut fr = frame_of(params);
        // B-2026-09-07-29 — the frame's RC-fallback-promoted bindings, keyed the
        // way the ownership pass keys them (bare name for a free function,
        // `Type.method` for an impl method). A region with no key is one the
        // ownership pass does not visit at all — a trait default body, a test
        // case, a `const` initializer — so it has no promotions to subtract;
        // see `all_regions`.
        if let Some(k) = fn_key.as_deref() {
            if let Some(names) = rc_fallback_bindings.get(k) {
                fr.rc_promoted.extend(names.iter().cloned());
            }
        }
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
fn all_regions(program: &Program) -> Vec<(Option<String>, Region<'_>)> {
    let mut out: Vec<(Option<String>, Region<'_>)> = Vec::new();
    for item in &program.items {
        match item {
            Item::Function(f) => out.push((Some(f.name.clone()), Region::Body(&f.params, &f.body))),
            Item::ImplBlock(b) => {
                // The ownership pass's own key for a method body
                // (`OwnershipChecker::check_function`): the impl target's LAST
                // path segment, then `.`, then the method name. Built the same
                // way here — and to the same `TypeKind::Path`-only restriction,
                // where that pass `continue`s — so a lookup into `rc_values`
                // either hits the right frame or misses a frame that pass never
                // recorded anything for.
                let type_name = match &b.target_type.kind {
                    TypeKind::Path(p) => p.segments.last().cloned(),
                    _ => None,
                };
                for inner in &b.items {
                    if let ImplItem::Method(m) = inner {
                        let key = type_name.as_ref().map(|t| format!("{}.{}", t, m.name));
                        out.push((key, Region::Body(&m.params, &m.body)));
                    }
                }
            }
            Item::TraitDef(t) => {
                for inner in &t.items {
                    if let TraitItem::Method(m) = inner {
                        if let Some(body) = &m.body {
                            // No key: `check_function` is reached only from
                            // `Item::Function` and `Item::ImplBlock`, so a trait
                            // DEFAULT body is never ownership-checked and can
                            // carry no RC-fallback promotion. Codegen agrees —
                            // `is_rc_fallback_binding` reads the same table — so
                            // nothing there is boxed either.
                            out.push((None, Region::Body(&m.params, body)));
                        }
                    }
                }
            }
            // A test body is ordinary code and calls ordinary functions; it is
            // simply not spelled `fn`. Keyless for the same reason as a trait
            // default body.
            Item::TestCase(t) => out.push((None, Region::Body(&[], &t.body))),
            Item::ConstDecl(c) => out.push((None, Region::Loose(&c.value))),
            Item::ModuleBinding(m) => out.push((None, Region::Loose(&m.value))),
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
///
/// B-2026-09-13-4 — `pub(crate)` so the drop differential can resolve a call
/// ARGUMENT to the caller-side place codegen records a drop against. Same
/// question this module asks of a transfer source, asked one call boundary out.
pub(crate) fn place_root(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root(object),
        _ => None,
    }
}

/// What [`visit_block`] hands its visitor.
pub(crate) enum Node<'a> {
    Stmt(&'a Stmt),
    Expr(&'a Expr),
}

// ── The exhaustive traversal ─────────────────────────────────────

pub(crate) fn visit_block<'a, F: FnMut(Node<'a>)>(b: &'a Block, f: &mut F) {
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
        ExprKind::MapLiteral { entries: pairs, .. } => {
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

/// B-2026-09-06-69 — whole-program call-site gate for the CONDITIONAL hand-back
/// ownership flip.
///
/// A sibling of [`compute_transferable_struct_params`] above, computed here for
/// the reason that module doc gives and answering a different question. The
/// flip lets a MIXED-PATH callee own the MEMORY of a by-value struct param that
/// its prologue refused to copy or take by transfer, under the per-path flag
/// that already guards the `Drop` body — so the buffer is freed on the exit
/// where the value died inside and left alone on the exit that handed it back.
/// The caller stands all the way down in exchange.
///
/// The structural asymmetry is the same one: the callee's decision is
/// PER-FUNCTION, one body serving every call site, while whether the caller CAN
/// stand down is a property of the CALL SITE. Measured on the version of this
/// fix without this gate: `fn g(a: R, c: bool) { f(a, c); }` — an enclosing
/// frame handing ITS OWN caller-retained param straight on — went from clean to
/// `free(): double free detected in tcache 2`, because `g` has no registration
/// to retract (the buffer belongs to a frame further up) and the callee then
/// freed it anyway.
///
/// So: **the flip applies to a param only when EVERY call site in the program
/// passes it in a shape whose caller-side owner this fix can actually retract.**
/// Two such shapes, and nothing else:
///
///   * A FRESH TEMP — `f(mk(1), c)`, `f(R { .. }, c)`, `f(x.build(), c)` — whose
///     owner is the argument registrar's, retracted by
///     `call_arg_flows_into_return`.
///   * A `let`-bound LOCAL whose initializer was itself a fresh temp, retracted
///     by name at the call (`suppress_user_drop_for_var`).
///
/// Everything else disqualifies, and the two that matter are worth naming. A
/// PARAMETER of the enclosing frame is the measured hazard above. A local
/// REBOUND from one (`let q = a; f(q, c);`) is the same hazard one hop on, so
/// admission is seeded from fresh temps and propagated through `let a = b;`
/// rather than granted to every local — the taint travels the way
/// `caller_retained_aggregate_memory`'s own induction step does.
///
/// Note this is the OPPOSITE polarity to the transfer gate on one point: there,
/// an own-mode parameter is an admitted source (its buffers are the callee's
/// after the transfer); here it is precisely the disqualifying one (its buffers
/// may be a further frame's). The two gates are not interchangeable and neither
/// subsumes the other.
///
/// Soundness rests on the same exhaustive walk, so a missed call site is a
/// build error rather than a silent double free.
pub(crate) fn compute_handback_safe_params(program: &Program) -> FxHashSet<ParamKey> {
    let mut live: FxHashSet<ParamKey> = FxHashSet::default();
    let mut fns: FxHashMap<String, (usize, bool)> = FxHashMap::default();
    // B-2026-09-07-4 — impl-block methods and assoc fns are candidates too, and
    // this map is what lets a `recv.m(..)` call site disqualify them: the AST
    // walk below has no type information, so a METHOD call names only `m`. Every
    // `Type.m` in the program is disqualified by any call site spelling `.m(`
    // whose argument shape this fix cannot retract — over-broad across
    // same-named methods of unrelated types, which costs the flip and never
    // grants it. Assoc calls are spelled `Type.f(..)` and resolve exactly.
    let mut by_method: FxHashMap<String, Vec<String>> = FxHashMap::default();
    // Methods reachable as `Type.m(recv, ..)`, where the receiver occupies an
    // argument slot and this gate's indices would be off by one. Disqualified
    // outright rather than shifted.
    let mut has_receiver: FxHashSet<String> = FxHashSet::default();
    let consider = |key: String,
                    f: &crate::ast::Function,
                    live: &mut FxHashSet<ParamKey>,
                    fns: &mut FxHashMap<String, (usize, bool)>| {
        let has_default = f.params.iter().any(|p| p.default_value.is_some());
        fns.insert(key.clone(), (f.params.len(), has_default));
        for (i, p) in f.params.iter().enumerate() {
            if matches!(p.ty.kind, TypeKind::Path(_)) && p.name().is_some() {
                live.insert((key.clone(), i));
            }
        }
    };
    for item in &program.items {
        match item {
            Item::Function(f) => {
                // `pub` and `main`: same reason as the transfer gate — their call
                // sites need not be in this program, so "every call site" is
                // unanswerable.
                if f.is_pub || f.name == "main" {
                    continue;
                }
                consider(f.name.clone(), f, &mut live, &mut fns);
            }
            Item::ImplBlock(b) => {
                // A TRAIT impl is reachable through dispatch this walk cannot
                // enumerate, and a GENERIC impl is compiled per monomorph, whose
                // param loop is a different registrar. Both decline outright.
                if b.trait_name.is_some() || b.generic_params.is_some() {
                    continue;
                }
                let TypeKind::Path(tp) = &b.target_type.kind else {
                    continue;
                };
                let Some(type_name) = tp.segments.last() else {
                    continue;
                };
                for ii in &b.items {
                    let ImplItem::Method(m) = ii else { continue };
                    if m.is_pub || m.generic_params.is_some() {
                        continue;
                    }
                    let key = format!("{type_name}.{}", m.name);
                    if m.self_param.is_some() {
                        has_receiver.insert(key.clone());
                    }
                    by_method
                        .entry(m.name.clone())
                        .or_default()
                        .push(key.clone());
                    consider(key, m, &mut live, &mut fns);
                }
            }
            _ => {}
        }
    }
    if live.is_empty() {
        return live;
    }
    let mut poisoned: FxHashSet<String> = FxHashSet::default();

    for (_, region) in all_regions(program) {
        let params: &[Param] = match region {
            Region::Body(p, _) => p,
            Region::Loose(_) => &[],
        };
        // Every parameter name, in EVERY mode. The transfer gate splits owned
        // from borrowed here; this one does not, because an own-mode parameter
        // is exactly the shape whose memory may belong to a frame further up.
        let mut forbidden: FxHashSet<String> = FxHashSet::default();
        forbidden.insert("self".to_string());
        for p in params {
            if let Some(n) = p.name() {
                forbidden.insert(n.to_string());
            }
        }
        // Locals seeded by a fresh temp, plus the `let a = b;` edges that carry
        // that status on. Collected first and closed afterwards so a call site
        // can be judged against a `let` appearing anywhere in the body.
        let mut fresh: FxHashSet<String> = FxHashSet::default();
        let mut aliased: Vec<(String, String)> = Vec::new();
        let mut calls: Vec<(String, &[crate::ast::CallArg])> = Vec::new();
        let mut mcalls: Vec<(String, &[crate::ast::CallArg])> = Vec::new();
        let mut callees: FxHashSet<*const Expr> = FxHashSet::default();
        let mut mentions: Vec<(String, *const Expr)> = Vec::new();
        let mut collect = |n| match n {
            Node::Stmt(st) => match &st.kind {
                StmtKind::Let { pattern, value, .. } => {
                    if let PatternKind::Binding(b) = &pattern.kind {
                        match &value.kind {
                            ExprKind::Call { .. }
                            | ExprKind::MethodCall { .. }
                            | ExprKind::StructLiteral { .. } => {
                                fresh.insert(b.clone());
                            }
                            ExprKind::Identifier(src) => {
                                aliased.push((b.clone(), src.clone()));
                            }
                            _ => {}
                        }
                    } else {
                        pattern_names(&pattern.kind, &mut forbidden);
                    }
                }
                StmtKind::LetElse { pattern, .. } => pattern_names(&pattern.kind, &mut forbidden),
                StmtKind::LetUninit { name, .. } => {
                    forbidden.insert(name.clone());
                }
                // A name ever written through is not the value its `let`
                // produced, so it stops being a retractable fresh temp.
                StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
                    if let Some(r) = place_root(target) {
                        forbidden.insert(r.to_string());
                    }
                }
                StmtKind::MultiAssign { targets, .. } => {
                    for t in targets {
                        if let Some(r) = place_root(t) {
                            forbidden.insert(r.to_string());
                        }
                    }
                }
                _ => {}
            },
            Node::Expr(e) => match &e.kind {
                ExprKind::IfLet { pattern, .. }
                | ExprKind::WhileLet { pattern, .. }
                | ExprKind::For { pattern, .. } => pattern_names(&pattern.kind, &mut forbidden),
                ExprKind::Match { arms, .. } => {
                    for a in arms {
                        pattern_names(&a.pattern.kind, &mut forbidden);
                    }
                }
                ExprKind::Closure { params, .. } => {
                    for cp in params {
                        pattern_names(&cp.pattern.kind, &mut forbidden);
                    }
                }
                ExprKind::Call { callee, args } => {
                    match &callee.kind {
                        ExprKind::Identifier(f) => {
                            callees.insert(&**callee as *const Expr);
                            calls.push((f.clone(), args.as_slice()));
                        }
                        // B-2026-09-07-4 — `Type.f(..)`, the assoc-fn spelling.
                        // Two segments resolve to exactly one candidate, so this
                        // joins the precise list rather than the by-name one.
                        //
                        // Deliberately NOT added to `callees`. Doing so would
                        // stop the `mentions` walk below poisoning the path's
                        // LAST SEGMENT, which today disqualifies a same-named
                        // FREE function — over-conservative, but pre-existing,
                        // and widening the free-fn flip is not this row's
                        // business. The method key is `Type.f`, so the bare-name
                        // poisoning never reaches it.
                        ExprKind::Path { segments, .. } if segments.len() == 2 => {
                            calls.push((segments.join("."), args.as_slice()));
                        }
                        _ => {}
                    }
                }
                // B-2026-09-07-4 — a method call names no TYPE (this walk has no
                // type information), so it is matched by method name against
                // every `Type.m` candidate below.
                ExprKind::MethodCall { method, args, .. } => {
                    mcalls.push((method.clone(), args.as_slice()));
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
        for (n, ptr) in mentions {
            if !callees.contains(&ptr) {
                poisoned.insert(n);
            }
        }
        // Close the `let a = b;` edges to a fixpoint: `a` is retractable only
        // if `b` was, whatever order the two statements appear in.
        loop {
            let mut grew = false;
            for (dst, src) in &aliased {
                if fresh.contains(src.as_str()) && !fresh.contains(dst.as_str()) {
                    fresh.insert(dst.clone());
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        let judge = |f: &str, args: &[crate::ast::CallArg], live: &mut FxHashSet<ParamKey>| {
            let (nparams, has_default) = fns.get(f).copied().unwrap_or((0, false));
            if args.iter().any(|a| a.label.is_some()) {
                for i in 0..nparams {
                    live.remove(&(f.to_string(), i));
                }
                return;
            }
            if has_default && args.len() < nparams {
                for i in args.len()..nparams {
                    live.remove(&(f.to_string(), i));
                }
            }
            for (i, a) in args.iter().enumerate() {
                let admit = match &a.value.kind {
                    ExprKind::Call { .. }
                    | ExprKind::MethodCall { .. }
                    | ExprKind::StructLiteral { .. } => true,
                    ExprKind::Identifier(n) => {
                        fresh.contains(n.as_str()) && !forbidden.contains(n.as_str())
                    }
                    _ => false,
                };
                if !admit {
                    live.remove(&(f.to_string(), i));
                }
            }
        };
        for (f, args) in calls {
            // A method with a receiver, reached through the `Type.m(recv, ..)`
            // spelling, puts the receiver in an ARGUMENT slot and shifts every
            // index this gate computes. The caller-side registrars key on the
            // receiver-EXCLUDING index, so decline the whole callee rather than
            // reconcile two conventions here.
            if has_receiver.contains(&f) {
                let n = fns.get(&f).map(|(n, _)| *n).unwrap_or(0);
                for i in 0..n {
                    live.remove(&(f.clone(), i));
                }
                continue;
            }
            judge(&f, args, &mut live);
        }
        // B-2026-09-07-4 — a method call site judges EVERY `Type.m` sharing the
        // name, because this walk cannot resolve the receiver's type. The effect
        // is one-directional: a call site can only take a candidate out of
        // `live`, never put one in, so matching too many types costs the flip on
        // an unrelated method and can never grant it on an unproven one.
        for (m, args) in mcalls {
            let Some(keys) = by_method.get(&m) else {
                continue;
            };
            for key in keys.clone() {
                judge(&key, args, &mut live);
            }
        }
    }
    live.retain(|(f, _)| !poisoned.contains(f));
    live
}
