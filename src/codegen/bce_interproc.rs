//! Interprocedural bounds precondition — the cross-call half of the converging
//! two-pointer elision (B-2026-08-05-6).
//!
//! [`super::bce_length_pin`]'s converging-skip analysis proves `base + idx <
//! v.len()` entirely inside one function: a local fill pins `v.len()`, an
//! enclosing counter bounds the row index, and `base` is linear in that
//! counter. The **row-helper** shape puts all three of those facts on the other
//! side of a call:
//!
//! ```text
//! fn row_is_palindrome(corpus: ref Vec[u8], base: i64, len: i64) -> bool {
//!     let mut lo = 0;
//!     let mut hi = len - 1;
//!     while lo <= hi { .. corpus[base + lo] .. corpus[base + hi] .. }
//! }
//! ```
//!
//! Nothing in that body can prove the index is in range: it holds only because
//! every caller passes `base = k * len` with `k < n` into a `corpus` of
//! `n * len` elements. Every fact the intra-function pass composes arrives here
//! as a parameter value, so no amount of strengthening that pass reaches it —
//! hence a separate module rather than a widening. This is the ordinary "hand a
//! helper one row of a flat buffer" idiom (matrix rows, fixed-stride records,
//! per-row two-pointer checks), not a kata-specific shape.
//!
//! The pass runs in two halves:
//!
//!   1. **Infer** ([`infer_precondition`]) — recognise the shape in a callee and
//!      record what it *needs* of its callers: `base + hi_init < v.len()`, and
//!      optionally `base + lo_init >= 0`, both expressed over the callee's
//!      PARAMETER names.
//!   2. **Discharge** ([`discharge_call_site`]) — at every call site in the
//!      program, substitute the actual arguments for those parameter names and
//!      re-prove the obligation against the caller's own local facts, using the
//!      same `init_below_bound` / `sum_nonneg_at_min` linear cancellation the
//!      intra-function path uses.
//!
//! A precondition is honoured only when EVERY call site in the program
//! discharges it. One site that cannot be proven — or one the walk does not
//! understand — disqualifies the callee outright. There is no per-site
//! specialisation: a single body is emitted, so a single fact must cover it.
//!
//! **The output is the existing [`ConvergingSkip`] record**, keyed by the inner
//! loop's condition span exactly as `compute_converging_skips` keys its own.
//! Codegen is unchanged — `compile_while` pushes the same `UpperBoundSum` facts
//! whether the proof came from local statements or from a discharged
//! precondition. Keeping the output on the existing channel is what makes this
//! a new *analysis* rather than a new IR path, and it is why the whole feature
//! costs codegen one map merge in `compile_function`.
//!
//! **Soundness.** The skip is emitted only when both of these hold:
//!
//!   * *Callee side.* At every `v[base + idx]` in the inner body, `idx <=
//!     hi_init` — `hi` is monotone non-increasing and `lo`'s steps all come
//!     after its uses, so the guard `lo <= hi <= hi_init` bounds both. `base`,
//!     `hi_init` and `lo_init` name only parameters, and every one of those
//!     parameters is unwritten, unshadowed and un-mut-borrowed across the whole
//!     body — so each still holds its argument value at the index sites, and
//!     substituting the caller's argument expression for it is exact.
//!   * *Caller side.* At every call site, the substituted `base + hi_init` is
//!     `< v.len()`, proven by the same linear cancellation the intra-function
//!     path uses against a `vec_length_lower_bounds` pin on the Vec argument.
//!
//! Together those give `base + idx <= base + hi_init < v.len()` at every index
//! site, for every call — which is exactly the fact `ConvergingSkip` asserts.
//!
//! **Fail-closed inventory.** Every one of the following drops the candidate
//! rather than guessing, mirroring `bce_length_pin.rs`'s discipline: a generic,
//! `pub`, or attributed callee; a method or trait-default body (only free
//! functions are candidates, so a call can never arrive through dynamic
//! dispatch); a duplicated free-fn name; the callee's name used as a bare value
//! anywhere (it could be called through a function pointer whose site the walk
//! cannot see); a non-`ref Vec[..]` buffer parameter, or one whose length is
//! not provably stable across the body; any write, rebind or mut-borrow of a
//! parameter the obligation names; a call site whose Vec argument is not a
//! plain identifier with a known length pin; an argument that is not a
//! pure-arithmetic expression; an enclosing counter that is not entry-valued at
//! the call; a call reached from a deferred or closure body, where no enclosing
//! counter fact survives; and any obligation the linear cancellation cannot
//! discharge outright.
//!
//! **Why "every call site" is knowable at all.** Both multi-module paths hand
//! codegen a merged SUPER-PROGRAM carrying every module's items — the entry-file
//! path via `try_build_run_super_program`, the project path by stitching
//! `super_items` — so `all_bodies` really does see every caller in the
//! compilation unit, not just the entry file's. The `pub` gate is what closes
//! the remaining hole: a `pub` function can be called from *outside* the unit,
//! and no census inside it would know. Checked in `src/cli.rs` rather than
//! assumed; if the pipeline ever compiles modules separately, this pass must be
//! disabled with it.
//!
//! **Walk totality is the load-bearing premise** — a call site the walk fails
//! to visit is a call site that never has to discharge, which is the one way
//! this analysis could go unsound. [`walk_block`] therefore enumerates every
//! `StmtKind` explicitly (no `_` arm) and [`walk_expr`] intercepts every
//! block-bearing `ExprKind` before delegating the rest to
//! `bce_length_pin`'s exhaustive `expr_children_all`. Anything either walk does
//! not model sets `opaque`, which drops every candidate program-wide.
//!
//! Opt out with `KARAC_BCE_INTERPROC=0` (soundness-critical BCE escape hatch
//! and A/B lever, mirroring `KARAC_BCE_CONV_SKIP` / `KARAC_BCE_DESC_SKIP`);
//! `KARAC_BCE_CONV_SKIP=0` also disables it, since it feeds that mechanism.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use super::bce_length_pin as pin;
use super::bce_length_pin::{BoundOp, BoundTerm, ConvergingSkip};
use crate::ast::*;
use crate::resolver::SpanKey;

/// What a callee needs its callers to guarantee, plus the skip to install if
/// they all do. Index expressions are over the callee's PARAMETER names, which
/// is what makes call-site substitution meaningful.
struct Precondition {
    /// The callee's parameter names in declaration order, so a call site can
    /// map each name the obligation mentions back to a positional argument.
    param_order: Vec<String>,
    /// Position of the `ref Vec[T]` parameter — an index rather than a name
    /// because discharge reads the call site's positional argument list.
    vec_param: usize,
    /// `base + hi_init`. Obligation: `< v.len()` at every call site.
    max_index: BoundTerm,
    /// `base + lo_init`. Obligation: `>= 0`. `None` when the callee-side shape
    /// for the lower half did not hold, in which case only the upper half of
    /// the bounds check is skipped and the sign half is left for LLVM to fold
    /// from the monotone assumes — exactly as in the intra-function path.
    min_index: Option<BoundTerm>,
    /// Key and value for the `converging_skips` entry this unlocks.
    cond_key: SpanKey,
    skip: ConvergingSkip,
}

/// Analyse `program` and return, per free-function name, the converging skips
/// its body earns from a precondition every caller discharges.
///
/// The returned map is merged into `converging_skips` at `compile_function`; an
/// empty map — no candidate shape, or one undischarged site — leaves codegen
/// bit-for-bit unchanged, which is the common case for essentially every
/// program.
pub(crate) fn compute_interproc_converging_skips(
    program: &Program,
) -> HashMap<String, HashMap<SpanKey, ConvergingSkip>> {
    let mut out: HashMap<String, HashMap<SpanKey, ConvergingSkip>> = HashMap::new();

    // Candidate callees: free functions with a unique name. A duplicated name
    // means a call site cannot be attributed to one body, so BOTH lose
    // candidacy rather than the analysis picking one.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut dup: HashSet<&str> = HashSet::new();
    for item in &program.items {
        if let Item::Function(f) = item {
            if !seen.insert(f.name.as_str()) {
                dup.insert(f.name.as_str());
            }
        }
    }
    let mut cands: HashMap<String, Precondition> = HashMap::new();
    for item in &program.items {
        let Item::Function(f) = item else { continue };
        if dup.contains(f.name.as_str()) {
            continue;
        }
        if let Some(p) = infer_precondition(f) {
            cands.insert(f.name.clone(), p);
        }
    }
    if cands.is_empty() {
        return out;
    }

    // Every body in the program, including impl/trait methods — a call site in
    // any of them is a call site that must discharge.
    let bodies = all_bodies(program);

    // `upper` starts true and is cleared by the first site that cannot prove
    // it; `lower` additionally requires the callee-side shape to have supplied
    // a `min_index`. A candidate with NO call site keeps `upper == true`: the
    // body is dead, so the skip it earns is vacuous but not wrong.
    let mut upper_ok: HashMap<&str, bool> = cands.keys().map(|k| (k.as_str(), true)).collect();
    let mut lower_ok: HashMap<&str, bool> = cands
        .iter()
        .map(|(k, p)| (k.as_str(), p.min_index.is_some()))
        .collect();
    let mut value_used: HashSet<String> = HashSet::new();

    for body in &bodies {
        let consts = pin::int_const_bindings(body);
        let lbs: HashMap<String, BoundTerm> = pin::vec_length_lower_bounds(body)
            .into_iter()
            .map(|(v, b)| {
                let folded = pin::fold_consts(&b, &consts);
                (v, folded)
            })
            .collect();
        let cx = Cx {
            cands: &cands,
            lbs,
            consts,
            verdicts: RefCell::new(Vec::new()),
            value_uses: RefCell::new(HashSet::new()),
            opaque: std::cell::Cell::new(false),
        };
        walk_block(body, &LoopCtx::default(), &cx);
        if cx.opaque.get() {
            // A construct the walk does not model could hide a call site that
            // never has to discharge. Nothing in this program is provable.
            return out;
        }
        for (name, verdict) in cx.verdicts.into_inner() {
            if !verdict.upper {
                upper_ok.insert(cands.get_key_value(&name).unwrap().0.as_str(), false);
            }
            if !verdict.lower {
                lower_ok.insert(cands.get_key_value(&name).unwrap().0.as_str(), false);
            }
        }
        value_used.extend(cx.value_uses.into_inner());
    }

    for (name, p) in &cands {
        // Used as a bare value somewhere: a call could reach the body through
        // a site this analysis never inspected.
        if value_used.contains(name) {
            continue;
        }
        if !upper_ok.get(name.as_str()).copied().unwrap_or(false) {
            continue;
        }
        let mut skip = p.skip.clone();
        skip.lower_proven = lower_ok.get(name.as_str()).copied().unwrap_or(false);
        out.entry(name.clone())
            .or_default()
            .insert(p.cond_key, skip);
    }
    out
}

// ===================================================================
// Callee side — infer the precondition
// ===================================================================

/// Recognise the row-helper shape in `f` and record what it needs of its
/// callers, or `None` if any gate fails.
fn infer_precondition(f: &Function) -> Option<Precondition> {
    // A method can be reached through trait dispatch; a generic body is
    // emitted once per instantiation with different argument types; an
    // attributed function may be exported or otherwise externally callable;
    // and a `pub` function may be called from outside this program tree. Each
    // breaks the "every call site is in `all_bodies`" premise.
    if f.self_param.is_some() || f.generic_params.is_some() || f.is_pub || !f.attributes.is_empty()
    {
        return None;
    }
    // A destructuring parameter pattern has no single name to substitute for.
    let pnames: Vec<String> = f
        .params
        .iter()
        .map(|p| p.name().map(str::to_string))
        .collect::<Option<_>>()?;

    let body = &f.body;
    let consts = pin::int_const_bindings(body);
    let stmts = &body.stmts;
    for (pos, stmt) in stmts.iter().enumerate() {
        let StmtKind::Expr(e) = &stmt.kind else {
            continue;
        };
        let ExprKind::While {
            condition,
            body: wb,
            ..
        } = &e.kind
        else {
            continue;
        };
        let Some((lo, hi)) = pin::as_converging_guard(condition) else {
            continue;
        };
        if lo == hi {
            continue;
        }
        // Same monotonicity gates as the intra-function path: `hi` only ever
        // decreases, and `lo`'s increments all come after its uses, so at every
        // index site the guard `lo <= hi <= hi_init` bounds both indices.
        if !pin::only_monotone_decrement(wb, &hi) || !pin::increments_are_trailing(wb, &lo) {
            continue;
        }
        let Some(hi_init) = pin::sole_scalar_init(&stmts[..pos], &hi) else {
            continue;
        };
        let hi_init = pin::fold_consts(&hi_init, &consts);
        let lo_init =
            pin::sole_scalar_init(&stmts[..pos], &lo).map(|b| pin::fold_consts(&b, &consts));
        let lower_shape_ok = pin::decrements_are_trailing(wb, &hi) && lo_init.is_some();

        let idx_names = [lo.clone(), hi.clone()];
        let mut chosen: Option<(String, Vec<String>)> = None;
        for (v, base) in pin::collect_base_indexed(wb, &idx_names) {
            if base == lo || base == hi {
                continue;
            }
            // `base` must be a PARAMETER: that is the whole difference from the
            // intra-function path, which requires a local `sole_scalar_init`.
            if !pnames.contains(&base) {
                continue;
            }
            // The buffer must be a `ref Vec[..]` parameter whose length cannot
            // move under the loop.
            let Some(vi) = pnames.iter().position(|n| *n == v) else {
                continue;
            };
            if !is_ref_vec_param(&f.params[vi]) || !vec_len_stable(body, &v) {
                continue;
            }
            // `base` loop-invariant across the inner body, exactly as intra.
            if pin::stmt_touches_var(&wb.stmts, &wb.final_expr, &base) {
                continue;
            }
            // One `base` per inner loop keeps the emitted fact unambiguous; a
            // second distinct base disqualifies rather than guesses.
            match &mut chosen {
                None => chosen = Some((base, vec![v])),
                Some((b, vs)) if *b == base => vs.push(v),
                Some(_) => {
                    chosen = None;
                    break;
                }
            }
        }
        let Some((base_var, mut vec_vars)) = chosen else {
            continue;
        };
        vec_vars.sort();
        vec_vars.dedup();
        // A second buffer would need its own obligation against its own
        // caller-side pin. One buffer per helper is the shape; more than one
        // disqualifies rather than proving only the first.
        if vec_vars.len() != 1 {
            continue;
        }
        let Some(vec_param) = pnames.iter().position(|n| *n == vec_vars[0]) else {
            continue;
        };

        let max_index = BoundTerm::Bin(
            BoundOp::Add,
            Box::new(BoundTerm::Ident(base_var.clone())),
            Box::new(hi_init),
        );
        let min_index = lo_init.filter(|_| lower_shape_ok).map(|li| {
            BoundTerm::Bin(
                BoundOp::Add,
                Box::new(BoundTerm::Ident(base_var.clone())),
                Box::new(li),
            )
        });

        // Every identifier the obligation names must be a parameter that still
        // holds its argument value everywhere in the body — otherwise
        // substituting the call site's argument for it is not sound. `lo` and
        // `hi` are locals, so an init naming either one fails this check rather
        // than leaking a callee-local into a caller-side proof.
        let mut names = Vec::new();
        pin::bound_idents(&max_index, &mut names);
        if let Some(mi) = &min_index {
            pin::bound_idents(mi, &mut names);
        }
        names.sort();
        names.dedup();
        if !names.iter().all(|n| {
            pnames.iter().any(|p| p == n)
                && !pin::stmt_touches_var(&body.stmts, &body.final_expr, n)
        }) {
            continue;
        }

        let mut idx_vars = vec![lo, hi];
        idx_vars.sort();
        return Some(Precondition {
            param_order: pnames,
            vec_param,
            max_index,
            min_index,
            cond_key: SpanKey::from_span(&condition.span),
            skip: ConvergingSkip {
                base_var,
                idx_vars,
                vec_vars,
                // Settled once every call site has reported.
                lower_proven: false,
            },
        });
    }
    None
}

/// A `ref Vec[..]` parameter. `mut ref` is excluded — the callee could resize
/// the buffer under its own loop — and so is an owned `Vec[..]` parameter, for
/// the same reason.
fn is_ref_vec_param(p: &Param) -> bool {
    let TypeKind::Ref(inner) = &p.ty.kind else {
        return false;
    };
    let TypeKind::Path(path) = &inner.kind else {
        return false;
    };
    path.segments.last().map(String::as_str) == Some("Vec")
}

/// `v` is only ever read in `body`. `stmt_touches_var` covers assignment,
/// rebinding and mut-borrow; `block_mutates_vec` covers the method and
/// argument-forwarding routes to a resize.
fn vec_len_stable(body: &Block, v: &str) -> bool {
    !pin::stmt_touches_var(&body.stmts, &body.final_expr, v) && !block_mutates_vec(body, v)
}

/// Any method call on `v` that is not a known read-only query, or any use of
/// `v` as a call argument — it could be forwarded to something that resizes it,
/// which this pass deliberately does not chase.
fn block_mutates_vec(body: &Block, v: &str) -> bool {
    let bad = std::cell::Cell::new(false);
    let check = |e: &Expr| {
        match &e.kind {
            ExprKind::MethodCall { object, method, .. } => {
                if is_ident(object, v) && !is_read_only_vec_method(method) {
                    bad.set(true);
                }
            }
            ExprKind::Call { args, .. } => {
                if args.iter().any(|a| is_ident(&a.value, v)) {
                    bad.set(true);
                }
            }
            _ => {}
        }
        false
    };
    pin::body_any(body, &check);
    bad.get()
}

fn is_read_only_vec_method(m: &str) -> bool {
    matches!(
        m,
        "len" | "is_empty" | "get" | "first" | "last" | "contains"
    )
}

fn is_ident(e: &Expr, name: &str) -> bool {
    matches!(&e.kind, ExprKind::Identifier(n) if n == name)
}

// ===================================================================
// Caller side — discharge the precondition at every call site
// ===================================================================

/// Whether a single call site proved each half of its callee's obligation.
struct Verdict {
    upper: bool,
    lower: bool,
}

/// The innermost enclosing counted loop at a call site, when the counter is
/// still entry-valued there. An empty context means "no counter to substitute",
/// which `init_below_bound` handles natively: the placeholder counter's
/// coefficient is zero, so the obligation must hold unconditionally.
#[derive(Default, Clone)]
struct LoopCtx {
    counter: Option<String>,
    u_max: Option<BoundTerm>,
    u_min: Option<BoundTerm>,
}

/// Per-caller-body state for the site walk.
struct Cx<'a> {
    cands: &'a HashMap<String, Precondition>,
    lbs: HashMap<String, BoundTerm>,
    consts: HashMap<String, i64>,
    verdicts: RefCell<Vec<(String, Verdict)>>,
    value_uses: RefCell<HashSet<String>>,
    /// Set when the walk meets a construct it does not model. Poisons the whole
    /// program rather than the one body: a call site that is never visited
    /// cannot be distinguished from one that does not exist.
    opaque: std::cell::Cell<bool>,
}

/// Build the loop context for a counted loop's body, or the empty context when
/// the counter is not provably entry-valued at every point inside.
fn loop_ctx(e: &Expr, before: &[Stmt], cx: &Cx) -> LoopCtx {
    let Some((counter, u_max, enc_body)) = pin::as_enclosing_loop(e) else {
        return LoopCtx::default();
    };
    // Every write to the counter must be a top-level clean increment with no
    // mention of it afterwards. Then a call whose substituted obligation
    // mentions the counter necessarily sits before the step, where the loop
    // guard's `counter <= u_max` still holds. `increments_are_trailing`
    // recurses into nested blocks, so a buried write is caught.
    if !pin::increments_are_trailing(enc_body, &counter) {
        return LoopCtx::default();
    }
    LoopCtx {
        u_max: Some(pin::fold_consts(&u_max, &cx.consts)),
        u_min: pin::counter_min(e, before, &counter).map(|m| pin::fold_consts(&m, &cx.consts)),
        counter: Some(counter),
    }
}

/// Walk a block's statements, keeping `ctx` for everything that is not itself a
/// counted loop. Enumerates every `StmtKind` explicitly — see the module note
/// on walk totality.
fn walk_block(block: &Block, ctx: &LoopCtx, cx: &Cx) {
    for (pos, stmt) in block.stmts.iter().enumerate() {
        match &stmt.kind {
            StmtKind::Expr(e) => walk_expr(e, ctx, &block.stmts[..pos], cx),
            StmtKind::Let { value, .. } => walk_expr(value, ctx, &block.stmts[..pos], cx),
            StmtKind::LetUninit { .. } => {}
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                walk_expr(value, ctx, &block.stmts[..pos], cx);
                walk_block(else_block, ctx, cx);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                walk_expr(target, ctx, &block.stmts[..pos], cx);
                walk_expr(value, ctx, &block.stmts[..pos], cx);
            }
            // A deferred body runs at scope exit, by which point an enclosing
            // counter has already stepped — no counter fact survives into it.
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                walk_block(body, &LoopCtx::default(), cx)
            }
            // Desugared away before the resolver, so codegen never sees one.
            // Poison rather than assume, since the desugar is not this pass's
            // invariant to rely on.
            StmtKind::MultiAssign { .. } => cx.opaque.set(true),
        }
    }
    if let Some(e) = &block.final_expr {
        walk_expr(e, ctx, &block.stmts, cx);
    }
}

/// Walk an expression for call sites. Intercepts every block-bearing
/// `ExprKind` so nested blocks are routed through [`walk_block`] with the right
/// context, then delegates the remainder to `bce_length_pin`'s exhaustive
/// `expr_children_all` — which, for the variants that reach it, only ever
/// recurses through child *expressions*.
fn walk_expr(e: &Expr, ctx: &LoopCtx, before: &[Stmt], cx: &Cx) {
    match &e.kind {
        // ── The site itself ────────────────────────────────────────
        ExprKind::Call { callee, args } => {
            if let ExprKind::Identifier(name) = &callee.kind {
                if let Some(p) = cx.cands.get(name.as_str()) {
                    let v = discharge_call_site(p, args, ctx, cx);
                    cx.verdicts.borrow_mut().push((name.clone(), v));
                }
            } else {
                walk_expr(callee, ctx, before, cx);
            }
            for a in args {
                walk_expr(&a.value, ctx, before, cx);
            }
            return;
        }
        // A bare mention of a candidate's name is a value use — the function
        // could be called through a pointer whose site the walk never sees.
        ExprKind::Identifier(n) => {
            if cx.cands.contains_key(n.as_str()) {
                cx.value_uses.borrow_mut().insert(n.clone());
            }
            return;
        }

        // ── Counted loops: their body gets a fresh counter context ──
        ExprKind::While {
            condition, body, ..
        } => {
            // The condition is evaluated with the counter possibly AT its
            // bound (the iteration that fails the guard), so it keeps the
            // outer context, not the loop's own.
            walk_expr(condition, ctx, before, cx);
            walk_block(body, &loop_ctx(e, before, cx), cx);
            return;
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable, ctx, before, cx);
            walk_block(body, &loop_ctx(e, before, cx), cx);
            return;
        }
        // Uncounted loops carry no counter fact into their bodies.
        ExprKind::Loop { body, .. } => {
            walk_block(body, &LoopCtx::default(), cx);
            return;
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr(value, ctx, before, cx);
            walk_block(body, &LoopCtx::default(), cx);
            return;
        }

        // ── Other block-bearing forms: same context, own walk ───────
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            walk_block(b, ctx, cx);
            return;
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, ctx, before, cx);
            walk_block(then_block, ctx, cx);
            if let Some(eb) = else_branch {
                walk_expr(eb, ctx, before, cx);
            }
            return;
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(value, ctx, before, cx);
            walk_block(then_block, ctx, cx);
            if let Some(eb) = else_branch {
                walk_expr(eb, ctx, before, cx);
            }
            return;
        }
        ExprKind::Providers { bindings, body } => {
            for pb in bindings {
                walk_expr(&pb.value, ctx, before, cx);
            }
            walk_block(body, ctx, cx);
            return;
        }
        // A closure body may run long after the enclosing loop has stepped —
        // or never — so no counter fact survives into it.
        ExprKind::Closure { body, .. } => {
            walk_expr(body, &LoopCtx::default(), &[], cx);
            return;
        }
        _ => {}
    }
    // Everything else: `expr_children_all` is exhaustive over `ExprKind`, and
    // every variant that reaches here recurses only through child expressions,
    // so the `stmt_all` gap this module's totality note describes is
    // unreachable from this arm.
    pin::expr_children_all(e, |c| {
        walk_expr(c, ctx, before, cx);
        true
    });
}

/// Substitute this call site's arguments into the callee's obligation and
/// re-prove it with the caller's own facts.
fn discharge_call_site(p: &Precondition, args: &[CallArg], ctx: &LoopCtx, cx: &Cx) -> Verdict {
    let deny = Verdict {
        upper: false,
        lower: false,
    };
    // A named argument (or an arity the walk cannot line up) breaks the
    // positional mapping the substitution relies on.
    if args.len() != p.param_order.len() || args.iter().any(|a| a.label.is_some()) {
        return deny;
    }
    // The buffer argument must be a plain identifier the caller has a length
    // pin for — the pin is the `v.len()` side of the obligation.
    let ExprKind::Identifier(v) = &args[p.vec_param].value.kind else {
        return deny;
    };
    let Some(b_pin) = cx.lbs.get(v.as_str()) else {
        return deny;
    };
    let Some(subst) = build_subst(p, args, &cx.consts) else {
        return deny;
    };
    let Some(max) = substitute(&p.max_index, &subst) else {
        return deny;
    };
    let max = pin::fold_consts(&max, &cx.consts);

    // `init_below_bound` substitutes `counter := u_max` and checks the
    // difference from the pin is a positive constant. With no enclosing loop
    // the placeholder counter appears in nothing, and the check degenerates to
    // the unconditional `max < b_pin` — which is exactly right.
    let counter = ctx.counter.as_deref().unwrap_or("");
    let u_max = ctx.u_max.clone().unwrap_or(BoundTerm::Int(0));
    let upper = pin::init_below_bound(&max, counter, &u_max, b_pin);

    // The lower half evaluates the same sum at the counter's MINIMUM. With no
    // enclosing counter there is nothing to substitute, so `u_min = 0` against
    // a placeholder that appears in nothing gives the unconditional check.
    let u_min = match (&ctx.counter, &ctx.u_min) {
        (Some(_), Some(m)) => Some(m.clone()),
        (None, _) => Some(BoundTerm::Int(0)),
        // An enclosing counter whose minimum is unknown: the sign half cannot
        // be evaluated, so it goes unproven and the check keeps its sign half.
        (Some(_), None) => None,
    };
    let lower = upper
        && match (&p.min_index, &u_min) {
            (Some(mi), Some(um)) => substitute(mi, &subst)
                .map(|m| pin::fold_consts(&m, &cx.consts))
                // `sum_nonneg_at_min` takes the two halves of the sum
                // separately; the substituted term is already the whole sum, so
                // the second half is the identity `0`.
                .is_some_and(|m| pin::sum_nonneg_at_min(&m, &BoundTerm::Int(0), counter, um)),
            _ => false,
        };
    Verdict { upper, lower }
}

/// Map every parameter name the obligation mentions to this call site's
/// argument term. Returns `None` if any such argument is not pure arithmetic —
/// a call, an index, a field access: anything that could evaluate differently
/// than the obligation assumes, or carry a side effect.
fn build_subst(
    p: &Precondition,
    args: &[CallArg],
    consts: &HashMap<String, i64>,
) -> Option<HashMap<String, BoundTerm>> {
    let mut names = Vec::new();
    pin::bound_idents(&p.max_index, &mut names);
    if let Some(mi) = &p.min_index {
        pin::bound_idents(mi, &mut names);
    }
    names.sort();
    names.dedup();
    let mut out = HashMap::new();
    for (idx, name) in p.param_order.iter().enumerate() {
        if !names.contains(name) {
            continue;
        }
        let bt = pin::normalize_bound(&args.get(idx)?.value)?;
        out.insert(name.clone(), pin::fold_consts(&bt, consts));
    }
    // Every name the obligation mentions must have been mapped. A name that is
    // not a parameter never reaches here (`infer_precondition` rejects it), so
    // a miss means an argument that did not normalise.
    if names.iter().any(|n| !out.contains_key(n)) {
        return None;
    }
    Some(out)
}

/// Replace each `Ident` in `bt` by its substitution. An identifier absent from
/// the map fails closed rather than leaking a callee-local name into a
/// caller-side proof.
fn substitute(bt: &BoundTerm, subst: &HashMap<String, BoundTerm>) -> Option<BoundTerm> {
    Some(match bt {
        BoundTerm::Int(n) => BoundTerm::Int(*n),
        BoundTerm::Ident(s) => subst.get(s)?.clone(),
        BoundTerm::Bin(op, l, r) => BoundTerm::Bin(
            *op,
            Box::new(substitute(l, subst)?),
            Box::new(substitute(r, subst)?),
        ),
    })
}

/// Every function body in the program — free functions, impl methods, and
/// trait methods with a default body.
fn all_bodies(program: &Program) -> Vec<&Block> {
    let mut out = Vec::new();
    for item in &program.items {
        match item {
            Item::Function(f) => out.push(&f.body),
            Item::ImplBlock(b) => {
                for inner in &b.items {
                    if let ImplItem::Method(m) = inner {
                        out.push(&m.body);
                    }
                }
            }
            Item::TraitDef(t) => {
                for inner in &t.items {
                    if let TraitItem::Method(m) = inner {
                        if let Some(body) = &m.body {
                            out.push(body);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `src` and report the names of the functions that earned a skip.
    /// Mirrors `bce_length_pin`'s test harness: the analysis is pure AST, so
    /// no resolve/typecheck/lower pass is needed to exercise it.
    fn skipped_fns(src: &str) -> Vec<String> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let mut names: Vec<String> = compute_interproc_converging_skips(&parsed.program)
            .into_keys()
            .collect();
        names.sort();
        names
    }

    /// The row-helper callee, with `{SIG}` and `{CALLS}` substituted per test.
    const SRC: &str = r#"
fn {SIG} {
    let mut lo = 0i64;
    let mut hi = len - 1i64;
    let mut acc = 0i64;
    while lo <= hi {
        acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);
        lo = lo + 1i64;
        hi = hi - 1i64;
    }
    acc
}

fn driver() -> i64 {
    let n = 20i64;
    let len = 32i64;
    let v: Vec[u8] = Vec.filled(n * len, 48u8);
    let mut acc = 0i64;
    let mut i = 0i64;
    {CALLS}
    acc
}
"#;

    fn src(sig: &str, calls: &str) -> String {
        let out = SRC.replace("{SIG}", sig).replace("{CALLS}", calls);
        assert!(
            !out.contains("{SIG}") && !out.contains("{CALLS}"),
            "substitution anchor left behind"
        );
        out
    }

    const ROW_SIG: &str = "row_scan(v: ref Vec[u8], base: i64, len: i64) -> i64";
    const GOOD_LOOP: &str = "while i < n { acc = acc + row_scan(v, i * len, len); i = i + 1i64; }";

    #[test]
    fn discharges_the_canonical_row_helper() {
        assert_eq!(skipped_fns(&src(ROW_SIG, GOOD_LOOP)), vec!["row_scan"]);
    }

    #[test]
    fn refuses_when_a_second_call_site_is_out_of_range() {
        // One body, one fact: the good site does not get a specialised copy.
        let calls = format!("{GOOD_LOOP}\n    acc = acc + row_scan(v, n * len - 2i64, len);");
        assert!(skipped_fns(&src(ROW_SIG, &calls)).is_empty());
    }

    #[test]
    fn refuses_an_off_by_one_caller_bound() {
        let calls = "while i <= n { acc = acc + row_scan(v, i * len, len); i = i + 1i64; }";
        assert!(skipped_fns(&src(ROW_SIG, calls)).is_empty());
    }

    #[test]
    fn refuses_when_the_callee_is_taken_as_a_value() {
        // Reachable through a function pointer whose call site this walk never
        // inspects, so "every call site discharges" stops being establishable.
        // Only unit-testable: the same program does not survive codegen today
        // (fn-as-value with a `ref Vec` param mis-lowers — B-2026-08-05-15).
        let calls = format!("{GOOD_LOOP}\n    let f = row_scan;\n    acc = acc + f(v, 0i64, len);");
        assert!(skipped_fns(&src(ROW_SIG, &calls)).is_empty());
    }

    #[test]
    fn refuses_a_pub_callee() {
        // A `pub` function may be called from outside this program tree, so the
        // call-site census cannot be complete.
        let s = src(ROW_SIG, GOOD_LOOP).replace("fn row_scan(", "pub fn row_scan(");
        assert!(skipped_fns(&s).is_empty());
    }

    #[test]
    fn refuses_a_mut_ref_buffer_param() {
        // `mut ref` lets the callee resize the buffer under its own loop, so
        // the caller's length pin no longer describes it at the index sites.
        let sig = "row_scan(v: mut ref Vec[u8], base: i64, len: i64) -> i64";
        assert!(skipped_fns(&src(sig, GOOD_LOOP)).is_empty());
    }

    #[test]
    fn refuses_when_the_caller_has_no_length_pin() {
        // `v` is filled by an unrecognised route, so there is no `v.len()`
        // lower bound to discharge the obligation against.
        let s = src(ROW_SIG, GOOD_LOOP).replace(
            "let v: Vec[u8] = Vec.filled(n * len, 48u8);",
            "let mut v: Vec[u8] = Vec.new();\n    v.push(1u8);",
        );
        assert!(skipped_fns(&s).is_empty());
    }

    #[test]
    fn refuses_a_non_arithmetic_base_argument() {
        // `other.len()` could differ between the obligation's evaluation and
        // the call's, so it does not normalise to a `BoundTerm`.
        let calls = "let other: Vec[u8] = Vec.filled(4i64, 0u8);\n    \
                     while i < n { acc = acc + row_scan(v, other.len(), len); i = i + 1i64; }";
        assert!(skipped_fns(&src(ROW_SIG, calls)).is_empty());
    }

    #[test]
    fn refuses_a_duplicated_free_fn_name() {
        // Two bodies for one name: a call site cannot be attributed to one, so
        // BOTH lose candidacy rather than the analysis picking one.
        let s = format!(
            "{}\nfn row_scan(v: ref Vec[u8], base: i64, len: i64) -> i64 {{ 0i64 }}\n",
            src(ROW_SIG, GOOD_LOOP)
        );
        assert!(skipped_fns(&s).is_empty());
    }

    #[test]
    fn refuses_when_the_index_steps_before_its_use() {
        // The callee-side monotonicity gate, inherited from the intra-function
        // path: an ascending index stepped BEFORE the index site can exceed the
        // bound the `lo <= hi` guard established.
        let s = src(ROW_SIG, GOOD_LOOP).replace(
            "acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);\n        lo = lo + 1i64;",
            "lo = lo + 1i64;\n        acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);",
        );
        assert!(skipped_fns(&s).is_empty());
    }

    #[test]
    fn refuses_when_a_named_parameter_is_reassigned_in_the_body() {
        // `base` no longer holds its argument value at the index sites, so
        // substituting the call site's argument for it is not sound.
        let s = src(ROW_SIG, GOOD_LOOP).replace(
            "let mut acc = 0i64;\n    while lo <= hi {",
            "let mut acc = 0i64;\n    base = base + 1i64;\n    while lo <= hi {",
        );
        assert!(skipped_fns(&s).is_empty());
    }

    #[test]
    fn refuses_a_call_from_a_closure_body() {
        // A closure may run after the enclosing loop has stepped — or never —
        // so no counter fact survives into it and the site cannot discharge.
        let calls = "let f = |k: i64| { row_scan(v, k * len, len) };\n    \
                     while i < n { acc = acc + f(i); i = i + 1i64; }";
        assert!(skipped_fns(&src(ROW_SIG, calls)).is_empty());
    }

    #[test]
    fn a_callee_with_no_call_sites_is_vacuous_not_wrong() {
        // Dead body: every (zero) call site discharges. The skip it earns is
        // never executed, so it is vacuous rather than unsound — recorded so a
        // future reader does not mistake it for a missing gate.
        assert_eq!(skipped_fns(&src(ROW_SIG, "")), vec!["row_scan"]);
    }
}
