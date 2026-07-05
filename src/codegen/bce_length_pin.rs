//! Vec-length-pin analysis for bounds-check elision (kata #62 rolling-DP).
//!
//! The split-check elision in `control_flow_bce.rs` drops a `v[idx]` upper
//! bound when a dominating guard proves `idx < v.len()` — either directly
//! (`while idx < v.len()`) or via a `let n = v.len()` alias (`while idx < n`).
//! It does NOT reach the common rolling-DP idiom
//!
//! ```text
//! let mut dp = Vec.new();
//! let mut j = 0;  while j < cols { dp.push(1); j = j + 1 }   // dp.len() == cols
//! ...  while c < cols { dp[c] = dp[c] + dp[c - 1]; c = c + 1 }
//! ```
//!
//! where the loop bound `cols` is not spelled `dp.len()` but is *equal* to it,
//! because `dp` was filled to exactly `cols` elements by a counted push loop and
//! never resized after. This pass recognises that shape and records a **length
//! pin** `bound == dp.len()`, which codegen activates once the fill loop has been
//! emitted so a later `while c < bound` guard resolves `bound` back to `dp` and
//! elides the upper-half check on `dp[c]` / `dp[c - k]`. Measured ~3.0x on the
//! #62 seq bench (316ms → ~105ms, C parity) — the RMW inner scan's per-cell
//! bounds checks were the entire gap; once they clear, LLVM forwards the loads
//! itself.
//!
//! **Recognised fill shapes** (all establish `bound <= v.len()`):
//!   - `while iv < BOUND { v.push(x); iv = iv + 1 }` with `iv` proven to start at
//!     literal `0` and step by exactly `+1`.
//!   - `for i in 0..BOUND { v.push(x) }` (exclusive range, start `0` or omitted)
//!     — the range natively guarantees `BOUND` iterations from 0.
//!   - either form may be preceded by `v.push(..)` **seed** pushes (they only
//!     make `v` longer, so `bound <= v.len()` still holds).
//!
//! `BOUND` may be any **pure-arithmetic** expression over identifiers and integer
//! literals (`cols`, `n + 1`, `m * 2`, …) — normalised to a span-free `BoundTerm`
//! so the fill's bound and a later guard's bound match structurally regardless of
//! operator-lowering form.
//!
//! **Soundness.** A pin `(bound, v)` is emitted only when a *fail-closed*
//! whole-function scan proves `bound <= v.len()` holds from the fill loop to the
//! end of the function:
//!   1. `let mut v = Vec.new()` / `Vec.with_capacity(_)` — a fresh empty Vec.
//!   2. A recognised fill loop (above) with exactly one unconditional push and no
//!      early exit, so after the loop `v.len() == P + BOUND >= BOUND` (`P` = seed
//!      pushes ≥ 0). `BOUND` names no mutated identifier inside the body.
//!   3. Between the binding and the fill loop, `v` is only *seed-pushed* — no
//!      other mention — so its length is exactly the seeds plus the trip count.
//!   4. After the fill loop, `v` appears ONLY as an index base (`v[..]`, read or
//!      assign-target) or a read-only `.len()` / `.is_empty()` receiver, and every
//!      identifier in `BOUND` is never written (assigned, shadowed, mut-borrowed,
//!      or method-mutated). So `v.len()` and `BOUND` are both constant from the
//!      fill loop onward and the `bound <= v.len()` relation cannot be falsified.
//!
//! The pin is a pure capacity-of-safety fact fed to the *existing* split-check
//! elision — no new IR shape — so a missed pin only keeps a bounds check, and a
//! wrongly-emitted one would be an OOB. Every recognition/scan helper therefore
//! fails closed (returns "no pin" / "unsafe") on any shape it does not fully
//! understand, mirroring `borrow_elision.rs` and `presize.rs`.

use crate::ast::*;
use crate::resolver::SpanKey;
use std::collections::{HashMap, HashSet};

/// A pure-arithmetic loop bound, normalised to a span-free canonical form so the
/// fill loop's bound and a later guard's bound compare structurally even across
/// operator-lowering forms (surface `Binary` vs trait-lowered `Call`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoundTerm {
    Ident(String),
    Int(i64),
    Bin(BoundOp, Box<BoundTerm>, Box<BoundTerm>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// Normalise a loop-bound expression to a `BoundTerm`, or `None` if it is not a
/// pure-arithmetic expression over identifiers and integer literals. Method
/// calls, function calls, indexing, and field access all return `None` (they
/// could vary between the fill and the use, or carry side effects), keeping the
/// pinned bound a deterministic function of its identifiers.
pub(crate) fn normalize_bound(expr: &Expr) -> Option<BoundTerm> {
    match &expr.kind {
        ExprKind::Identifier(n) => Some(BoundTerm::Ident(n.clone())),
        ExprKind::Integer(k, _) => Some(BoundTerm::Int(*k)),
        ExprKind::Binary { op, left, right } => {
            let bop = surface_bound_op(op)?;
            Some(BoundTerm::Bin(
                bop,
                Box::new(normalize_bound(left)?),
                Box::new(normalize_bound(right)?),
            ))
        }
        // Trait-lowered `Call { Path([ty, "add"|"sub"|…]), [a, b] }`.
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 {
                return None;
            }
            let bop = method_bound_op(segments[1].as_str())?;
            Some(BoundTerm::Bin(
                bop,
                Box::new(normalize_bound(&args[0].value)?),
                Box::new(normalize_bound(&args[1].value)?),
            ))
        }
        _ => None,
    }
}

fn surface_bound_op(op: &BinOp) -> Option<BoundOp> {
    match op {
        BinOp::Add => Some(BoundOp::Add),
        BinOp::Sub => Some(BoundOp::Sub),
        BinOp::Mul => Some(BoundOp::Mul),
        BinOp::Div => Some(BoundOp::Div),
        BinOp::Mod => Some(BoundOp::Rem),
        _ => None,
    }
}

fn method_bound_op(name: &str) -> Option<BoundOp> {
    match name {
        "add" => Some(BoundOp::Add),
        "sub" => Some(BoundOp::Sub),
        "mul" => Some(BoundOp::Mul),
        "div" => Some(BoundOp::Div),
        "rem" => Some(BoundOp::Rem),
        _ => None,
    }
}

/// Every identifier named by a `BoundTerm`.
fn bound_idents(bt: &BoundTerm, out: &mut Vec<String>) {
    match bt {
        BoundTerm::Ident(n) => out.push(n.clone()),
        BoundTerm::Int(_) => {}
        BoundTerm::Bin(_, l, r) => {
            bound_idents(l, out);
            bound_idents(r, out);
        }
    }
}

/// One recognised length pin: after the fill loop identified by its key span,
/// the Vec `vec_var` has length `>= bound` and both are invariant, so a
/// `while idx < bound` guard proves `idx < vec_var.len()`.
#[derive(Debug, Clone)]
pub(crate) struct VecLengthPin {
    pub bound: BoundTerm,
    pub vec_var: String,
}

/// One recognised fill loop.
struct Fill {
    /// Statement index of the fill loop in the enclosing block.
    fi: usize,
    /// Key span codegen matches to activate the pin — the `while` condition's
    /// span, or the `for`-range end expression's span.
    key_span: crate::token::Span,
    /// Raw bound expression (`BOUND` in `iv < BOUND` / `0..BOUND`).
    bound: Expr,
    /// `Some(iv)` for the `while` form (needs the zero-start proof); `None` for
    /// the `for 0..BOUND` form (start is structurally 0).
    counter: Option<String>,
}

/// Analyse a function body and return the length pins it establishes, keyed by
/// the fill loop's key span (`SpanKey`). Codegen activates a pin when it finishes
/// emitting the matching loop, so the pin is live exactly for the code lexically
/// after the fill loop.
pub(crate) fn compute_vec_length_pins(body: &Block) -> HashMap<SpanKey, VecLengthPin> {
    let mut out = HashMap::new();
    // Whole-function binding counts. A pin is name-keyed on its Vec and stays
    // active to end of function, so it is only sound when that Vec name is bound
    // EXACTLY ONCE in the whole function — otherwise the pin could match a
    // different, same-named Vec in a sibling / outer scope (which was never
    // proven long enough) and elide a genuine OOB. This is what makes recursing
    // into nested blocks below safe (a nested `let dp` fill whose name also
    // appears at the outer level is bound twice → no pin).
    let whole = region_bindings(&body.stmts, &body.final_expr);
    // Recognise fills in the function body AND every nested block (per-iteration
    // rebuilds `while k { let mut dp = …; fill; use }`, DP inside an `if` arm,
    // …). Each block is analysed on its OWN statement list; a Vec's scope — and
    // thus the invariance region — is the rest of its declaring block.
    for_each_block(body, &mut |block| {
        analyze_block_pins(block, &whole, &mut out)
    });
    out
}

/// Run the per-block fill recognition on ONE block's statement list (no
/// recursion — `for_each_block` drives descent). Records a pin for every
/// counted-fill Vec declared directly in `block` that passes all soundness
/// gates.
fn analyze_block_pins(
    block: &Block,
    whole: &RegionBindings,
    out: &mut HashMap<SpanKey, VecLengthPin>,
) {
    let stmts = &block.stmts;
    for (li, stmt) in stmts.iter().enumerate() {
        let Some(v) = empty_vec_binding(stmt) else {
            continue;
        };
        // The Vec name must be bound exactly once in the whole function.
        if whole.rebound.get(&v) != Some(&1) {
            continue;
        }
        let Some(fill) = find_exact_fill_loop(stmts, li, &v) else {
            continue;
        };
        // The bound must be a pure-arithmetic expression to pin.
        let Some(bound) = normalize_bound(&fill.bound) else {
            continue;
        };
        // `while` counter must be provably zero at loop entry (so the trip count
        // is exactly `BOUND`). The `for 0..BOUND` form guarantees this natively.
        if let Some(iv) = &fill.counter {
            if !counter_is_zero_before(stmts, fill.fi, iv) {
                continue;
            }
        }
        // After the fill loop, `v` must stay length-stable and every identifier
        // in `BOUND` must stay unwritten — the invariance over `v`'s scope (the
        // rest of THIS block) that makes the pin sound.
        let after = &stmts[fill.fi + 1..];
        if !vec_readonly_after(after, &block.final_expr, &v) {
            continue;
        }
        let mut idents = Vec::new();
        bound_idents(&bound, &mut idents);
        if idents
            .iter()
            .any(|b| !var_unwritten_after(after, &block.final_expr, b))
        {
            continue;
        }
        // Shadow / nested-reassignment soundness (the scans above are pattern-
        // blind in NESTED blocks — they only see statement VALUES, not `let`
        // patterns or assignment targets buried in an inner block). Collect every
        // name rebound-by-pattern or assigned anywhere in `v`'s after-fill scope:
        //   - a rebind of `v` (`let v = <shorter Vec>` in an inner scope) would
        //     make the name-keyed pin apply to a DIFFERENT, unproven Vec — an OOB
        //     read on the shadow (also caught by the exactly-once gate, but kept
        //     as defence in depth);
        //   - a rebind OR reassignment of a bound identifier (`let n = 20` /
        //     `n = 20` in an inner scope) changes the bound's value out from under
        //     the fill, so the guard no longer implies `idx < v.len()`.
        // Either is memory-unsafety; bail. (`v` as an assignment ROOT is fine —
        // that's an index store `v[i] = x`, already vetted by `vec_readonly_after`
        // — so `v` is checked against `rebound` only, not `assigned`.)
        let region = region_bindings(after, &block.final_expr);
        if region.is_rebound(&v)
            || idents
                .iter()
                .any(|b| region.is_rebound(b) || region.assigned.contains(b))
        {
            continue;
        }
        out.insert(
            SpanKey::from_span(&fill.key_span),
            VecLengthPin { bound, vec_var: v },
        );
    }
}

/// `let mut V = Vec.new()` / `Vec.with_capacity(_)` → `Some(V)`. Both start
/// empty (length 0); the fill loop is what gives `V` its length.
fn empty_vec_binding(stmt: &Stmt) -> Option<String> {
    let StmtKind::Let { pattern, value, .. } = &stmt.kind else {
        return None;
    };
    let PatternKind::Binding(name) = &pattern.kind else {
        return None;
    };
    let ExprKind::Call { callee, args } = &value.kind else {
        return None;
    };
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return None;
    };
    let is_empty_vec = segments.len() == 2
        && segments[0] == "Vec"
        && ((segments[1] == "new" && args.is_empty())
            || (segments[1] == "with_capacity" && args.len() == 1));
    if is_empty_vec {
        Some(name.clone())
    } else {
        None
    }
}

/// Find `v`'s exact counted-fill loop among `stmts[let_idx+1..]`. Statements that
/// don't touch `v` are stepped over; `v.push(..)` **seed** pushes are allowed
/// (they only lengthen `v`); the first *other* mention of `v` bails (so `v`'s
/// length before the fill is exactly the seed count).
fn find_exact_fill_loop(stmts: &[Stmt], let_idx: usize, v: &str) -> Option<Fill> {
    for (off, stmt) in stmts[let_idx + 1..].iter().enumerate() {
        let fi = let_idx + 1 + off;
        if let StmtKind::Expr(e) = &stmt.kind {
            // `while iv < BOUND { v.push(x) once; iv = iv + 1 }`.
            if let ExprKind::While {
                condition, body, ..
            } = &e.kind
            {
                if let Some((iv, bound)) = as_strict_lt(condition) {
                    if iv != v && while_fill_body_ok(body, v, &iv, bound) {
                        return Some(Fill {
                            fi,
                            key_span: condition.span.clone(),
                            bound: bound.clone(),
                            counter: Some(iv),
                        });
                    }
                }
                if block_mentions_ident(body, v) || expr_mentions_ident(condition, v) {
                    return None;
                }
                continue;
            }
            // `for i in 0..BOUND { v.push(x) once }`.
            if let ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } = &e.kind
            {
                if let Some(bound) = for_zero_range_end(pattern, iterable, v) {
                    if for_fill_body_ok(body, v, bound) {
                        return Some(Fill {
                            fi,
                            key_span: bound.span.clone(),
                            bound: bound.clone(),
                            counter: None,
                        });
                    }
                }
                if block_mentions_ident(body, v) || expr_mentions_ident(iterable, v) {
                    return None;
                }
                continue;
            }
        }
        // A `v.push(..)` seed push before the fill is allowed (lengthens `v`);
        // any OTHER mention of `v` means we can't account for its length — bail.
        if let StmtKind::Expr(e) = &stmt.kind {
            if is_push_to(e, v) {
                continue;
            }
        }
        if stmt_mentions_ident(stmt, v) {
            return None;
        }
    }
    None
}

/// `for I in 0..BOUND` (exclusive, start `0` or omitted): return `&BOUND` when
/// the loop var is a plain binding distinct from `v`. `None` for any other range
/// shape (inclusive, non-zero start, unbounded).
fn for_zero_range_end<'a>(pattern: &Pattern, iterable: &'a Expr, v: &str) -> Option<&'a Expr> {
    let PatternKind::Binding(i) = &pattern.kind else {
        return None;
    };
    if i == v {
        return None;
    }
    let ExprKind::Range {
        start,
        end,
        inclusive,
    } = &iterable.kind
    else {
        return None;
    };
    if *inclusive {
        return None;
    }
    // Start must be absent or literal 0.
    let start_zero = matches!(
        start.as_deref().map(|e| &e.kind),
        None | Some(ExprKind::Integer(0, _))
    );
    if !start_zero {
        return None;
    }
    end.as_deref()
}

/// `while` fill body: exactly one unconditional `v.push(..)`, `iv` stepped by
/// exactly one `+1` and never otherwise written, no early exit, and no bound
/// identifier written. Reads of `v` are allowed.
fn while_fill_body_ok(body: &Block, v: &str, iv: &str, bound: &Expr) -> bool {
    if block_has_early_exit(body) {
        return false;
    }
    if count_pushes_shallow(body, v) != Some(1) {
        return false;
    }
    if count_plus_one_steps(body, iv) != 1 {
        return false;
    }
    if writes_ident_other_than_plus_one_step(body, iv) {
        return false;
    }
    bound_invariant_in_block(body, bound)
}

/// `for` fill body: exactly one unconditional `v.push(..)`, no early exit, no
/// bound identifier written. The range construct owns the counter, so there is
/// no `iv` to validate.
fn for_fill_body_ok(body: &Block, v: &str, bound: &Expr) -> bool {
    if block_has_early_exit(body) {
        return false;
    }
    if count_pushes_shallow(body, v) != Some(1) {
        return false;
    }
    bound_invariant_in_block(body, bound)
}

/// No identifier of `bound` is written anywhere in `body` (the bound is
/// loop-invariant). `None`-normalising bounds are treated as non-invariant
/// (fail closed) so a later `normalize_bound` bail is never reached with a
/// mutated bound.
fn bound_invariant_in_block(block: &Block, bound: &Expr) -> bool {
    let Some(bt) = normalize_bound(bound) else {
        return false;
    };
    let mut idents = Vec::new();
    bound_idents(&bt, &mut idents);
    !idents.iter().any(|n| var_written_in_block(block, n))
}

/// `cond` as `(iv, &BOUND)` for a strict `iv < BOUND` — surface `Binary(Lt)` or
/// trait-lowered `Call { Path([ty,"lt"]), [iv, BOUND] }`. `iv` must be a bare
/// identifier; `BOUND` is returned raw for normalisation.
fn as_strict_lt(cond: &Expr) -> Option<(String, &Expr)> {
    match &cond.kind {
        ExprKind::Binary {
            op: BinOp::Lt,
            left,
            right,
        } => Some((ident(left)?, right)),
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() == 2 && segments[1] == "lt" {
                Some((ident(&args[0].value)?, &args[1].value))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn ident(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        _ => None,
    }
}

/// Count top-level `v.push(..)` statements in `body`. `None` if the body has any
/// nested control flow (which could hide a conditional push) — fail closed so a
/// surviving body is straight-line and the count is exact.
fn count_pushes_shallow(body: &Block, v: &str) -> Option<usize> {
    let mut pushes = 0;
    for s in &body.stmts {
        match &s.kind {
            StmtKind::Expr(e) => {
                if is_push_to(e, v) {
                    pushes += 1;
                } else if expr_has_nested_control_flow(e) {
                    return None;
                }
            }
            StmtKind::Assign { value, .. } | StmtKind::CompoundAssign { value, .. } => {
                if expr_has_nested_control_flow(value) {
                    return None;
                }
            }
            StmtKind::Let { value, .. } => {
                if expr_has_nested_control_flow(value) {
                    return None;
                }
            }
            // Any other statement kind (defer, let-else, uninit, …) — bail.
            _ => return None,
        }
    }
    if let Some(fe) = &body.final_expr {
        if expr_has_nested_control_flow(fe) {
            return None;
        }
    }
    Some(pushes)
}

fn is_push_to(e: &Expr, v: &str) -> bool {
    matches!(&e.kind, ExprKind::MethodCall { object, method, .. }
        if matches!(&object.kind, ExprKind::Identifier(n) if n == v)
            && matches!(method.as_str(), "push" | "push_back"))
}

/// Count top-level `iv = iv + 1` / `iv += 1` steps in `body`.
fn count_plus_one_steps(body: &Block, iv: &str) -> usize {
    body.stmts
        .iter()
        .filter(|s| is_plus_one_step(s, iv))
        .count()
}

fn is_plus_one_step(stmt: &Stmt, iv: &str) -> bool {
    match &stmt.kind {
        StmtKind::Assign { target, value } => {
            assign_root_is(target, iv) && is_iv_plus_one(value, iv)
        }
        StmtKind::CompoundAssign {
            target,
            op: CompoundOp::Add,
            value,
        } => assign_root_is(target, iv) && matches!(&value.kind, ExprKind::Integer(1, _)),
        _ => false,
    }
}

/// `iv + 1` / `1 + iv` — surface `Binary(Add)` or trait-lowered `Call`.
fn is_iv_plus_one(value: &Expr, iv: &str) -> bool {
    let is_iv = |e: &Expr| matches!(&e.kind, ExprKind::Identifier(n) if n == iv);
    let is_one = |e: &Expr| matches!(&e.kind, ExprKind::Integer(1, _));
    match &value.kind {
        ExprKind::Binary {
            op: BinOp::Add,
            left,
            right,
        } => (is_iv(left) && is_one(right)) || (is_one(left) && is_iv(right)),
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return false;
            };
            segments.len() == 2
                && segments[1] == "add"
                && args.len() == 2
                && ((is_iv(&args[0].value) && is_one(&args[1].value))
                    || (is_one(&args[0].value) && is_iv(&args[1].value)))
        }
        _ => false,
    }
}

/// True if `iv` is written anywhere in `body` by anything OTHER than a
/// top-level `+1` step — a second step, a reset, an index/field store rooted at
/// `iv`, a mut-borrow, or any write buried in nested control flow. Fails open
/// (returns true) on unanalyzable shapes, disqualifying the fill.
fn writes_ident_other_than_plus_one_step(body: &Block, iv: &str) -> bool {
    for s in &body.stmts {
        if is_plus_one_step(s, iv) {
            continue;
        }
        if stmt_writes_ident(s, iv) {
            return true;
        }
    }
    if let Some(fe) = &body.final_expr {
        if expr_writes_ident(fe, iv) {
            return true;
        }
    }
    false
}

// ── After-fill invariance scans ─────────────────────────────────────
//
// These decide whether the pin stays valid for the rest of the function. Both
// are whitelist / fail-closed: any use they don't recognise as safe returns
// "unsafe", keeping the bounds check.

/// Every occurrence of `v` in the post-fill region is a *safe read*: `v` as the
/// base of an index expression (`v[..]`, including an assign target `v[..] = x`)
/// or the receiver of `.len()` / `.is_empty()`. Anything else — a push/pop or
/// other mutating method, a reassignment, a shadow, a move into a call, a
/// closure capture — makes `v.len()` potentially change and disqualifies.
fn vec_readonly_after(stmts: &[Stmt], final_expr: &Option<Box<Expr>>, v: &str) -> bool {
    stmts.iter().all(|s| stmt_vec_readonly(s, v))
        && final_expr
            .as_ref()
            .map(|e| expr_vec_readonly(e, v))
            .unwrap_or(true)
}

fn stmt_vec_readonly(stmt: &Stmt, v: &str) -> bool {
    match &stmt.kind {
        StmtKind::Let { pattern, value, .. } if pattern_binds(pattern, v) => {
            // `let v = ..` shadows the tracked binding — unsafe (a new `v`).
            let _ = value;
            false
        }
        StmtKind::Let { value, .. } => expr_vec_readonly(value, v),
        StmtKind::LetUninit { name, .. } => name != v,
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            expr_vec_readonly(target, v) && expr_vec_readonly(value, v)
        }
        StmtKind::Expr(e) => expr_vec_readonly(e, v),
        // Unanalyzed statement kinds (defer / let-else / …) — fail closed.
        _ => false,
    }
}

/// Whether every occurrence of `v` in `e` is a safe read (see `vec_readonly_after`).
fn expr_vec_readonly(e: &Expr, v: &str) -> bool {
    match &e.kind {
        // A bare `v` not captured by an index/len parent below is a disallowed
        // use (move, comparison, argument, …).
        ExprKind::Identifier(n) => n != v,
        // `v[index]` — the base may be `v` (allowed, don't descend into the
        // Identifier); the index sub-expression is still scanned.
        ExprKind::Index { object, index } => {
            let base_ok = match &object.kind {
                ExprKind::Identifier(n) if n == v => true,
                _ => expr_vec_readonly(object, v),
            };
            base_ok && expr_vec_readonly(index, v)
        }
        // `v.len()` / `v.is_empty()` — read-only receiver methods.
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            let recv_is_v = matches!(&object.kind, ExprKind::Identifier(n) if n == v);
            if recv_is_v && is_read_only_vec_method(method) && args.is_empty() {
                return true;
            }
            expr_vec_readonly(object, v) && args.iter().all(|a| expr_vec_readonly(&a.value, v))
        }
        // Everything else: recurse into all children; a bare `v` anywhere is
        // caught by the Identifier arm.
        _ => expr_children_all(e, |c| expr_vec_readonly(c, v)),
    }
}

fn is_read_only_vec_method(method: &str) -> bool {
    matches!(method, "len" | "is_empty")
}

/// `b` is never *written* in the post-fill region: not an assignment target,
/// not shadowed, not passed as a `mut` argument, not a method-call receiver
/// (a scalar bound has no in-place mutators, but any receiver use is refused
/// conservatively). Reads are fine. Fails closed on unanalyzed shapes.
fn var_unwritten_after(stmts: &[Stmt], final_expr: &Option<Box<Expr>>, b: &str) -> bool {
    stmts.iter().all(|s| !stmt_writes_bound(s, b))
        && final_expr
            .as_ref()
            .map(|e| !expr_writes_bound(e, b))
            .unwrap_or(true)
}

fn stmt_writes_bound(stmt: &Stmt, b: &str) -> bool {
    match &stmt.kind {
        StmtKind::Let { pattern, value, .. } => {
            pattern_binds(pattern, b) || expr_writes_bound(value, b)
        }
        StmtKind::LetUninit { name, .. } => name == b,
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            assign_root_is(target, b) || expr_writes_bound(target, b) || expr_writes_bound(value, b)
        }
        StmtKind::Expr(e) => expr_writes_bound(e, b),
        // Unanalyzed statement kinds — fail closed (treat as a write).
        _ => true,
    }
}

/// Whether `b` is written anywhere in `e`: as a `mut`-marked argument, a
/// method-call receiver, or (recursively) inside any sub-expression. Reads
/// (bare identifier, arithmetic operands) are NOT writes.
fn expr_writes_bound(e: &Expr, b: &str) -> bool {
    match &e.kind {
        ExprKind::Identifier(_) => false,
        ExprKind::MethodCall { object, args, .. } => {
            // A method may take `mut ref self` — any receiver use of `b` refused.
            matches!(&object.kind, ExprKind::Identifier(n) if n == b)
                || expr_writes_bound(object, b)
                || args.iter().any(|a| call_arg_writes_bound(a, b))
        }
        ExprKind::Call { callee, args } => {
            expr_writes_bound(callee, b) || args.iter().any(|a| call_arg_writes_bound(a, b))
        }
        _ => !expr_children_all(e, |c| !expr_writes_bound(c, b)),
    }
}

fn call_arg_writes_bound(a: &CallArg, b: &str) -> bool {
    if a.mut_marker {
        if let Some(root) = place_root(&a.value) {
            if root == b {
                return true;
            }
        }
    }
    expr_writes_bound(&a.value, b)
}

// ── Region binding/assignment collector (shadow soundness) ──────────
//
// The read-only / unwritten scans above recurse into nested blocks through the
// generic `expr_children_all`, which only sees statement VALUES — it is blind to
// `let` patterns and assignment targets buried in an inner block. This collector
// closes that gap: it walks a region (statements + all nested blocks/exprs) and
// records every name bound by a pattern (`rebound`) and every assignment target
// ROOT (`assigned`). Both walkers are EXHAUSTIVE over `StmtKind` / `ExprKind`
// (no wildcard) so a new AST variant is a compile error rather than a silent
// hole. Over-collection is safe — it only makes a pin bail.

#[derive(Default)]
struct RegionBindings {
    /// name → number of pattern-binding sites in the region. Counts (not a set)
    /// so a whole-function scan can enforce "the pinned Vec is bound exactly
    /// once" (the guard against a name-keyed pin matching a same-named Vec in a
    /// sibling / outer scope).
    rebound: HashMap<String, usize>,
    assigned: HashSet<String>,
}

impl RegionBindings {
    fn bind(&mut self, name: String) {
        *self.rebound.entry(name).or_insert(0) += 1;
    }
    fn is_rebound(&self, name: &str) -> bool {
        self.rebound.contains_key(name)
    }
}

fn region_bindings(stmts: &[Stmt], final_expr: &Option<Box<Expr>>) -> RegionBindings {
    let mut rb = RegionBindings::default();
    for s in stmts {
        collect_stmt_bindings(s, &mut rb);
    }
    if let Some(e) = final_expr {
        collect_expr_bindings(e, &mut rb);
    }
    rb
}

fn collect_block_bindings(b: &Block, rb: &mut RegionBindings) {
    for s in &b.stmts {
        collect_stmt_bindings(s, rb);
    }
    if let Some(e) = &b.final_expr {
        collect_expr_bindings(e, rb);
    }
}

fn collect_stmt_bindings(s: &Stmt, rb: &mut RegionBindings) {
    match &s.kind {
        StmtKind::Let { pattern, value, .. } => {
            for n in pattern.binding_names() {
                rb.bind(n);
            }
            collect_expr_bindings(value, rb);
        }
        StmtKind::LetElse {
            pattern,
            value,
            else_block,
            ..
        } => {
            for n in pattern.binding_names() {
                rb.bind(n);
            }
            collect_expr_bindings(value, rb);
            collect_block_bindings(else_block, rb);
        }
        StmtKind::LetUninit { name, .. } => {
            rb.bind(name.clone());
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            if let Some(root) = place_root(target) {
                rb.assigned.insert(root.to_string());
            }
            collect_expr_bindings(target, rb);
            collect_expr_bindings(value, rb);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                if let Some(root) = place_root(t) {
                    rb.assigned.insert(root.to_string());
                }
                collect_expr_bindings(t, rb);
            }
            for v in values {
                collect_expr_bindings(v, rb);
            }
        }
        StmtKind::Expr(e) => collect_expr_bindings(e, rb),
        StmtKind::Defer { body } => collect_block_bindings(body, rb),
        StmtKind::ErrDefer { binding, body } => {
            if let Some(b) = binding {
                rb.bind(b.clone());
            }
            collect_block_bindings(body, rb);
        }
    }
}

/// EXHAUSTIVE over `ExprKind`. Collects patterns introduced by expression-level
/// binders (`if let` / `while let` / `for` / `match` / closures) and recurses
/// into every nested block via `collect_block_bindings` (so inner-block `let`s
/// and assignments are seen) and every sub-expression.
fn collect_expr_bindings(e: &Expr, rb: &mut RegionBindings) {
    match &e.kind {
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
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
                if let ParsedInterpolationPart::Expr(inner) = p {
                    collect_expr_bindings(inner, rb);
                }
            }
        }
        ExprKind::Binary { left, right, .. } => {
            collect_expr_bindings(left, rb);
            collect_expr_bindings(right, rb);
        }
        ExprKind::Unary { operand, .. } => collect_expr_bindings(operand, rb),
        ExprKind::Question(inner) => collect_expr_bindings(inner, rb),
        ExprKind::OptionalChain { object, args, .. } => {
            collect_expr_bindings(object, rb);
            if let Some(a) = args {
                for arg in a {
                    collect_expr_bindings(&arg.value, rb);
                }
            }
        }
        ExprKind::NilCoalesce { left, right } => {
            collect_expr_bindings(left, rb);
            collect_expr_bindings(right, rb);
        }
        ExprKind::Call { callee, args } => {
            collect_expr_bindings(callee, rb);
            for a in args {
                collect_expr_bindings(&a.value, rb);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_expr_bindings(object, rb);
            for a in args {
                collect_expr_bindings(&a.value, rb);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            collect_expr_bindings(object, rb)
        }
        ExprKind::Index { object, index } => {
            collect_expr_bindings(object, rb);
            collect_expr_bindings(index, rb);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => collect_block_bindings(b, rb),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_expr_bindings(condition, rb);
            collect_block_bindings(then_block, rb);
            if let Some(e) = else_branch {
                collect_expr_bindings(e, rb);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            for n in pattern.binding_names() {
                rb.bind(n);
            }
            collect_expr_bindings(value, rb);
            collect_block_bindings(then_block, rb);
            if let Some(e) = else_branch {
                collect_expr_bindings(e, rb);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_expr_bindings(scrutinee, rb);
            for arm in arms {
                for n in arm.pattern.binding_names() {
                    rb.bind(n);
                }
                if let Some(g) = &arm.guard {
                    collect_expr_bindings(g, rb);
                }
                collect_expr_bindings(&arm.body, rb);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_expr_bindings(condition, rb);
            collect_block_bindings(body, rb);
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            for n in pattern.binding_names() {
                rb.bind(n);
            }
            collect_expr_bindings(value, rb);
            collect_block_bindings(body, rb);
        }
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            for n in pattern.binding_names() {
                rb.bind(n);
            }
            collect_expr_bindings(iterable, rb);
            collect_block_bindings(body, rb);
        }
        ExprKind::Loop { body, .. } => collect_block_bindings(body, rb),
        ExprKind::LabeledBlock { body, .. } => collect_block_bindings(body, rb),
        ExprKind::Closure { params, body, .. } => {
            for cp in params {
                for n in cp.pattern.binding_names() {
                    rb.bind(n);
                }
            }
            collect_expr_bindings(body, rb);
        }
        ExprKind::Return(opt) => {
            if let Some(inner) = opt {
                collect_expr_bindings(inner, rb);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                collect_expr_bindings(v, rb);
            }
        }
        ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
            for x in exprs {
                collect_expr_bindings(x, rb);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items {
                collect_expr_bindings(x, rb);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            collect_expr_bindings(value, rb);
            collect_expr_bindings(count, rb);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                collect_expr_bindings(k, rb);
                collect_expr_bindings(v, rb);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                collect_expr_bindings(&f.value, rb);
            }
            if let Some(sp) = spread {
                collect_expr_bindings(sp, rb);
            }
        }
        ExprKind::Pipe { left, right } => {
            collect_expr_bindings(left, rb);
            collect_expr_bindings(right, rb);
        }
        ExprKind::Cast { expr, .. } => collect_expr_bindings(expr, rb),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_expr_bindings(s, rb);
            }
            if let Some(e) = end {
                collect_expr_bindings(e, rb);
            }
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            collect_block_bindings(b, rb)
        }
        ExprKind::Lock { body, .. } => collect_block_bindings(body, rb),
        ExprKind::Providers { bindings, body } => {
            for pb in bindings {
                collect_expr_bindings(&pb.value, rb);
            }
            collect_block_bindings(body, rb);
        }
    }
}

// ── Block visitor (drives per-block fill recognition) ───────────────
//
// Invokes `f` on `block` and on every block nested inside it (loop / if /
// match / closure / … bodies). EXHAUSTIVE over `StmtKind` / `ExprKind` so a new
// AST variant is a compile error rather than an un-visited block.

fn for_each_block<'a>(block: &'a Block, f: &mut impl FnMut(&'a Block)) {
    f(block);
    for s in &block.stmts {
        for_each_block_in_stmt(s, f);
    }
    if let Some(e) = &block.final_expr {
        for_each_block_in_expr(e, f);
    }
}

fn for_each_block_in_stmt<'a>(s: &'a Stmt, f: &mut impl FnMut(&'a Block)) {
    match &s.kind {
        StmtKind::Let { value, .. } => for_each_block_in_expr(value, f),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            for_each_block_in_expr(value, f);
            for_each_block(else_block, f);
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            for_each_block_in_expr(target, f);
            for_each_block_in_expr(value, f);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                for_each_block_in_expr(t, f);
            }
            for v in values {
                for_each_block_in_expr(v, f);
            }
        }
        StmtKind::Expr(e) => for_each_block_in_expr(e, f),
        StmtKind::Defer { body } => for_each_block(body, f),
        StmtKind::ErrDefer { body, .. } => for_each_block(body, f),
    }
}

fn for_each_block_in_expr<'a>(e: &'a Expr, f: &mut impl FnMut(&'a Block)) {
    match &e.kind {
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
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
                if let ParsedInterpolationPart::Expr(inner) = p {
                    for_each_block_in_expr(inner, f);
                }
            }
        }
        ExprKind::Binary { left, right, .. } => {
            for_each_block_in_expr(left, f);
            for_each_block_in_expr(right, f);
        }
        ExprKind::Unary { operand, .. } => for_each_block_in_expr(operand, f),
        ExprKind::Question(inner) => for_each_block_in_expr(inner, f),
        ExprKind::OptionalChain { object, args, .. } => {
            for_each_block_in_expr(object, f);
            if let Some(a) = args {
                for arg in a {
                    for_each_block_in_expr(&arg.value, f);
                }
            }
        }
        ExprKind::NilCoalesce { left, right } => {
            for_each_block_in_expr(left, f);
            for_each_block_in_expr(right, f);
        }
        ExprKind::Call { callee, args } => {
            for_each_block_in_expr(callee, f);
            for a in args {
                for_each_block_in_expr(&a.value, f);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            for_each_block_in_expr(object, f);
            for a in args {
                for_each_block_in_expr(&a.value, f);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            for_each_block_in_expr(object, f)
        }
        ExprKind::Index { object, index } => {
            for_each_block_in_expr(object, f);
            for_each_block_in_expr(index, f);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => for_each_block(b, f),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            for_each_block_in_expr(condition, f);
            for_each_block(then_block, f);
            if let Some(e) = else_branch {
                for_each_block_in_expr(e, f);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            for_each_block_in_expr(value, f);
            for_each_block(then_block, f);
            if let Some(e) = else_branch {
                for_each_block_in_expr(e, f);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            for_each_block_in_expr(scrutinee, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    for_each_block_in_expr(g, f);
                }
                for_each_block_in_expr(&arm.body, f);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            for_each_block_in_expr(condition, f);
            for_each_block(body, f);
        }
        ExprKind::WhileLet { value, body, .. } => {
            for_each_block_in_expr(value, f);
            for_each_block(body, f);
        }
        ExprKind::For { iterable, body, .. } => {
            for_each_block_in_expr(iterable, f);
            for_each_block(body, f);
        }
        ExprKind::Loop { body, .. } => for_each_block(body, f),
        ExprKind::LabeledBlock { body, .. } => for_each_block(body, f),
        ExprKind::Closure { body, .. } => for_each_block_in_expr(body, f),
        ExprKind::Return(opt) => {
            if let Some(inner) = opt {
                for_each_block_in_expr(inner, f);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                for_each_block_in_expr(v, f);
            }
        }
        ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
            for x in exprs {
                for_each_block_in_expr(x, f);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items {
                for_each_block_in_expr(x, f);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            for_each_block_in_expr(value, f);
            for_each_block_in_expr(count, f);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                for_each_block_in_expr(k, f);
                for_each_block_in_expr(v, f);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for fld in fields {
                for_each_block_in_expr(&fld.value, f);
            }
            if let Some(sp) = spread {
                for_each_block_in_expr(sp, f);
            }
        }
        ExprKind::Pipe { left, right } => {
            for_each_block_in_expr(left, f);
            for_each_block_in_expr(right, f);
        }
        ExprKind::Cast { expr, .. } => for_each_block_in_expr(expr, f),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                for_each_block_in_expr(s, f);
            }
            if let Some(e) = end {
                for_each_block_in_expr(e, f);
            }
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            for_each_block(b, f)
        }
        ExprKind::Lock { body, .. } => for_each_block(body, f),
        ExprKind::Providers { bindings, body } => {
            for pb in bindings {
                for_each_block_in_expr(&pb.value, f);
            }
            for_each_block(body, f);
        }
    }
}

// ── Generic AST helpers (whitelist / fail-closed) ───────────────────

fn pattern_binds(pattern: &Pattern, name: &str) -> bool {
    pattern.binding_names().iter().any(|n| n == name)
}

/// Root identifier of an assignment target place expression.
fn assign_root_is(target: &Expr, name: &str) -> bool {
    match &target.kind {
        ExprKind::Identifier(n) => n == name,
        ExprKind::Index { object, .. }
        | ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. } => assign_root_is(object, name),
        ExprKind::Unary { operand, .. } => assign_root_is(operand, name),
        _ => false,
    }
}

/// Root identifier of a place expression, else `None` (literals, calls, …).
fn place_root(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n.as_str()),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root(object),
        ExprKind::Unary { operand, .. } => place_root(operand),
        _ => None,
    }
}

/// Whether `stmt` writes `iv` (assign target, compound-assign target, shadow,
/// or mut-marked arg). Fails open (true) on unanalyzed shapes.
fn stmt_writes_ident(stmt: &Stmt, iv: &str) -> bool {
    match &stmt.kind {
        StmtKind::Let { pattern, value, .. } => {
            pattern_binds(pattern, iv) || expr_writes_ident(value, iv)
        }
        StmtKind::LetUninit { name, .. } => name == iv,
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            assign_root_is(target, iv)
                || expr_writes_ident(target, iv)
                || expr_writes_ident(value, iv)
        }
        StmtKind::Expr(e) => expr_writes_ident(e, iv),
        _ => true,
    }
}

fn expr_writes_ident(e: &Expr, iv: &str) -> bool {
    match &e.kind {
        ExprKind::Identifier(_) => false,
        ExprKind::MethodCall { object, args, .. } => {
            matches!(&object.kind, ExprKind::Identifier(n) if n == iv)
                || expr_writes_ident(object, iv)
                || args.iter().any(|a| {
                    (a.mut_marker && place_root(&a.value) == Some(iv))
                        || expr_writes_ident(&a.value, iv)
                })
        }
        ExprKind::Call { callee, args } => {
            expr_writes_ident(callee, iv)
                || args.iter().any(|a| {
                    (a.mut_marker && place_root(&a.value) == Some(iv))
                        || expr_writes_ident(&a.value, iv)
                })
        }
        _ => !expr_children_all(e, |c| !expr_writes_ident(c, iv)),
    }
}

fn var_written_in_block(block: &Block, name: &str) -> bool {
    block.stmts.iter().any(|s| stmt_writes_ident(s, name))
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_writes_ident(e, name))
}

fn counter_is_zero_before(stmts: &[Stmt], fill_idx: usize, iv: &str) -> bool {
    // Walk every statement before the fill loop, tracking whether `iv`'s last
    // binding/assignment set it to literal 0. Any non-zero or unanalyzable
    // write to `iv` clears the knowledge (fail closed).
    let mut known_zero = false;
    for stmt in &stmts[..fill_idx] {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } if pattern_binds(pattern, iv) => {
                known_zero = matches!(&value.kind, ExprKind::Integer(0, _));
            }
            StmtKind::Assign { target, value } if assign_root_is(target, iv) => {
                known_zero = matches!(target.kind, ExprKind::Identifier(_))
                    && matches!(&value.kind, ExprKind::Integer(0, _));
            }
            // Any other write to `iv` (compound assign, mut-borrow, nested) —
            // conservatively treat as unknown.
            _ => {
                if stmt_writes_ident(stmt, iv) {
                    known_zero = false;
                }
            }
        }
    }
    known_zero
}

fn block_has_early_exit(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_early_exit)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_early_exit(e))
}

fn stmt_has_early_exit(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Expr(e) => expr_has_early_exit(e),
        StmtKind::Let { value, .. } => expr_has_early_exit(value),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            expr_has_early_exit(target) || expr_has_early_exit(value)
        }
        _ => false,
    }
}

fn expr_has_early_exit(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::Break { .. } | ExprKind::Continue { .. } | ExprKind::Return(..)
    )
}

/// True if `e` contains a nested control-flow expression (if / match / loop /
/// while / for / closure / block). Used to guarantee a fill-loop body is
/// straight-line before counting its pushes.
fn expr_has_nested_control_flow(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::If { .. }
            | ExprKind::IfLet { .. }
            | ExprKind::Match { .. }
            | ExprKind::While { .. }
            | ExprKind::WhileLet { .. }
            | ExprKind::For { .. }
            | ExprKind::Loop { .. }
            | ExprKind::Closure { .. }
            | ExprKind::Block(_)
            | ExprKind::LabeledBlock { .. }
    )
}

// ── "mentions identifier" (used for the pre-fill emptiness guard) ──

fn stmt_mentions_ident(stmt: &Stmt, name: &str) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } => expr_mentions_ident(value, name),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            expr_mentions_ident(target, name) || expr_mentions_ident(value, name)
        }
        StmtKind::Expr(e) => expr_mentions_ident(e, name),
        // Unanalyzed statement kind — conservatively assume it touches the name.
        _ => true,
    }
}

fn block_mentions_ident(block: &Block, name: &str) -> bool {
    block.stmts.iter().any(|s| stmt_mentions_ident(s, name))
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_mentions_ident(e, name))
}

fn expr_mentions_ident(e: &Expr, name: &str) -> bool {
    match &e.kind {
        ExprKind::Identifier(n) => n == name,
        ExprKind::Path { segments, .. } => segments.first().is_some_and(|s| s == name),
        _ => !expr_children_all(e, |c| !expr_mentions_ident(c, name)),
    }
}

/// Apply `pred` to every direct sub-expression of `e`, returning true iff `pred`
/// holds for all of them. EXHAUSTIVE over `ExprKind` (no wildcard) so a new AST
/// variant breaks this at compile time rather than silently escaping the scans
/// above — the same fail-closed discipline as `control_flow_bce.rs`'s
/// monotone walk. Leaf expressions have no children → vacuously true.
fn expr_children_all<F: Fn(&Expr) -> bool + Copy>(e: &Expr, pred: F) -> bool {
    match &e.kind {
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => true,
        ExprKind::InterpolatedStringLit(parts) => parts.iter().all(|p| match p {
            ParsedInterpolationPart::Expr(inner) => pred(inner),
            _ => true,
        }),
        ExprKind::Binary { left, right, .. } => pred(left) && pred(right),
        ExprKind::Unary { operand, .. } => pred(operand),
        ExprKind::Question(inner) => pred(inner),
        ExprKind::OptionalChain { object, args, .. } => {
            pred(object)
                && args
                    .as_ref()
                    .map(|a| a.iter().all(|x| pred(&x.value)))
                    .unwrap_or(true)
        }
        ExprKind::NilCoalesce { left, right } => pred(left) && pred(right),
        ExprKind::Call { callee, args } => pred(callee) && args.iter().all(|a| pred(&a.value)),
        ExprKind::MethodCall { object, args, .. } => {
            pred(object) && args.iter().all(|a| pred(&a.value))
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => pred(object),
        ExprKind::Index { object, index } => pred(object) && pred(index),
        ExprKind::Block(b) | ExprKind::Comptime(b) => block_all(b, pred),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            pred(condition)
                && block_all(then_block, pred)
                && else_branch.as_ref().map(|e| pred(e)).unwrap_or(true)
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            pred(value)
                && block_all(then_block, pred)
                && else_branch.as_ref().map(|e| pred(e)).unwrap_or(true)
        }
        ExprKind::Match { scrutinee, arms } => {
            // `arm.guard` is an un-boxed `Option<Expr>`, so `map(&pred)` needs no
            // deref-coercing closure (unlike the `Option<Box<Expr>>` arms).
            pred(scrutinee)
                && arms
                    .iter()
                    .all(|arm| arm.guard.as_ref().map(&pred).unwrap_or(true) && pred(&arm.body))
        }
        ExprKind::While {
            condition, body, ..
        } => pred(condition) && block_all(body, pred),
        ExprKind::WhileLet { value, body, .. } => pred(value) && block_all(body, pred),
        ExprKind::For { iterable, body, .. } => pred(iterable) && block_all(body, pred),
        ExprKind::Loop { body, .. } => block_all(body, pred),
        ExprKind::LabeledBlock { body, .. } => block_all(body, pred),
        ExprKind::Closure { body, .. } => pred(body),
        ExprKind::Return(opt) => opt.as_ref().map(|e| pred(e)).unwrap_or(true),
        ExprKind::Break { value, .. } => value.as_ref().map(|e| pred(e)).unwrap_or(true),
        ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => exprs.iter().all(pred),
        ExprKind::PrefixCollectionLiteral { items, .. } => items.iter().all(pred),
        ExprKind::RepeatLiteral { value, count, .. } => pred(value) && pred(count),
        ExprKind::MapLiteral(pairs) => pairs.iter().all(|(k, v)| pred(k) && pred(v)),
        ExprKind::StructLiteral { fields, spread, .. } => {
            fields.iter().all(|f| pred(&f.value))
                && spread.as_ref().map(|s| pred(s)).unwrap_or(true)
        }
        ExprKind::Pipe { left, right } => pred(left) && pred(right),
        ExprKind::Cast { expr, .. } => pred(expr),
        ExprKind::Range { start, end, .. } => {
            start.as_ref().map(|s| pred(s)).unwrap_or(true)
                && end.as_ref().map(|e| pred(e)).unwrap_or(true)
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            block_all(b, pred)
        }
        ExprKind::Lock { body, .. } => block_all(body, pred),
        ExprKind::Providers { bindings, body } => {
            bindings.iter().all(|pb| pred(&pb.value)) && block_all(body, pred)
        }
    }
}

/// `pred` holds for every statement value and the final expr of `block`.
fn block_all<F: Fn(&Expr) -> bool + Copy>(block: &Block, pred: F) -> bool {
    block.stmts.iter().all(|s| stmt_all(s, pred))
        && block.final_expr.as_ref().map(|e| pred(e)).unwrap_or(true)
}

fn stmt_all<F: Fn(&Expr) -> bool + Copy>(stmt: &Stmt, pred: F) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } => pred(value),
        StmtKind::LetUninit { .. } => true,
        StmtKind::LetElse {
            value, else_block, ..
        } => pred(value) && block_all(else_block, pred),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            pred(target) && pred(value)
        }
        StmtKind::Expr(e) => pred(e),
        StmtKind::Defer { body } => block_all(body, pred),
        StmtKind::ErrDefer { body, .. } => block_all(body, pred),
        StmtKind::MultiAssign { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `src`, run the pin analysis on the first function's body, and
    /// report whether some pin binds the Vec named `vec`.
    fn pins_vec(src: &str, vec: &str) -> bool {
        let parsed = crate::parse(src);
        let body = parsed
            .program
            .items
            .into_iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f.body),
                _ => None,
            })
            .expect("a function");
        compute_vec_length_pins(&body)
            .values()
            .any(|p| p.vec_var == vec)
    }

    // ── Positive: the kata shape and the new fill/bound forms ────

    #[test]
    fn fires_on_rolling_dp_shape() {
        let src = "fn f(rows: i64, cols: i64) -> i64 {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            let mut i = 1i64;\n\
            while i < rows {\n\
              let mut c = 1i64;\n\
              while c < cols { dp[c] = dp[c] + dp[c - 1i64]; c = c + 1i64; }\n\
              i = i + 1i64;\n\
            }\n\
            dp[cols - 1i64]\n\
        }\n";
        assert!(pins_vec(src, "dp"));
    }

    #[test]
    fn fires_with_capacity_init() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.with_capacity(cols);\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(pins_vec(src, "dp"));
    }

    #[test]
    fn fires_on_for_range_fill() {
        // Follow-up (1): `for i in 0..n { v.push(..) }` establishes the same
        // `len == n` fact as the counted while loop.
        let src = "fn f(n: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            for i in 0i64..n { dp.push(1i64); }\n\
            let mut c = 0i64;\n\
            while c < n { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(pins_vec(src, "dp"));
    }

    #[test]
    fn fires_on_for_range_fill_omitted_start() {
        let src = "fn f(n: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            for i in 0i64..n { dp.push(0i64); }\n\
            let mut c = 0i64;\n\
            while c < n { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(pins_vec(src, "dp"));
    }

    #[test]
    fn fires_with_prelude_seed_push() {
        // Follow-up (2): a seed push before the counted fill only lengthens `dp`,
        // so `cols <= dp.len()` still holds — the pin now fires.
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            dp.push(7i64);\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(pins_vec(src, "dp"));
    }

    #[test]
    fn fires_on_nonbare_bound_plus_one() {
        // Follow-up (3): a `cols + 1` arithmetic bound, filled and indexed under
        // the identical expression, pins by structural (normalised) match.
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols + 1i64 { dp.push(1i64); j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols + 1i64 { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(pins_vec(src, "dp"));
    }

    // ── Negative: unsound-if-fired shapes MUST NOT pin ───────────

    #[test]
    fn no_fire_counter_starts_nonzero() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 1i64;\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_for_range_nonzero_start() {
        // `for i in 1..n` pushes only `n-1` times ⇒ dp.len() < n.
        let src = "fn f(n: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            for i in 1i64..n { dp.push(1i64); }\n\
            let mut c = 0i64;\n\
            while c < n { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_for_range_inclusive() {
        // `for i in 0..=n` is a different trip count than the `< n` guard; only
        // the exclusive form is recognised.
        let src = "fn f(n: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            for i in 0i64..=n { dp.push(1i64); }\n\
            let mut c = 0i64;\n\
            while c < n { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_bound_reassigned_after_fill() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            cols = cols + 5i64;\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_nonbare_bound_ident_reassigned_after_fill() {
        // `cols + 1` bound but `cols` is rewritten after the fill ⇒ the fill-time
        // and use-time bounds differ. Must not pin.
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols + 1i64 { dp.push(1i64); j = j + 1i64; }\n\
            cols = cols + 3i64;\n\
            let mut c = 0i64;\n\
            while c < cols + 1i64 { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_vec_popped_after_fill() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            dp.pop();\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_vec_pushed_after_fill() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            dp.push(9i64);\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_conditional_push() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols { if j > 0i64 { dp.push(1i64); } j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_two_pushes() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); dp.push(2i64); j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_inclusive_while_fill() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j <= cols { dp.push(1i64); j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_step_of_two() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = j + 2i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_vec_moved_into_call_after_fill() {
        let src = "fn sink(v: Vec[i64]) {}\n\
        fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            sink(dp);\n\
            let mut c = 0i64;\n\
            while c < cols { c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_non_push_use_between_binding_and_fill() {
        // A non-push mention of `dp` before the fill (here a `.len()` read) means
        // its length before the fill isn't provably the seed count — bail.
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let x = dp.len();\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_counter_reset_in_body() {
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols { dp.push(1i64); j = 0i64; j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    // ── Shadow / nested-reassignment soundness (region_bindings) ──

    #[test]
    fn no_fire_nested_shadow_of_vec() {
        // A nested block re-binds `dp` (to a shorter/empty Vec) and indexes it.
        // The name-keyed pin must NOT fire, or the shadow `dp[d]` reads OOB.
        let src = "fn f(n: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < n { dp.push(1i64); j = j + 1i64; }\n\
            let mut once = 1i64;\n\
            while once > 0i64 {\n\
              let mut dp: Vec[i64] = Vec.new();\n\
              let mut d = 0i64;\n\
              while d < n { let x = dp[d]; d = d + 1i64; }\n\
              once = 0i64;\n\
            }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_nested_shadow_of_bound_var() {
        // A nested block re-binds the BOUND var `n` to a larger value; `dp[c]`
        // under `while c < n` would then read past `dp.len()`. Must not pin.
        let src = "fn f(n: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < n { dp.push(1i64); j = j + 1i64; }\n\
            let mut once = 1i64;\n\
            while once > 0i64 {\n\
              let n = 20i64;\n\
              let mut c = 0i64;\n\
              while c < n { let x = dp[c]; c = c + 1i64; }\n\
              once = 0i64;\n\
            }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_nested_reassign_of_bound_var() {
        // A nested-block ASSIGNMENT (not rebind) of the bound var also changes
        // its value out from under the fill.
        let src = "fn f(m: i64) {\n\
            let mut n = m;\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < n { dp.push(1i64); j = j + 1i64; }\n\
            let mut once = 1i64;\n\
            while once > 0i64 {\n\
              n = 20i64;\n\
              let mut c = 0i64;\n\
              while c < n { let x = dp[c]; c = c + 1i64; }\n\
              once = 0i64;\n\
            }\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    // ── Nested-block fill recognition ───────────────────────────

    #[test]
    fn fires_on_per_iteration_rebuild() {
        // The DP buffer is built fresh inside an outer loop's body block — the
        // common "rebuild each iteration" shape. `dp` is bound once (one AST
        // site), so the pin is sound.
        let src = "fn f(total: i64, n: i64) -> i64 {\n\
            let mut acc = 0i64;\n\
            let mut k = 0i64;\n\
            while k < total {\n\
              let mut dp: Vec[i64] = Vec.new();\n\
              let mut j = 0i64;\n\
              while j < n { dp.push(1i64); j = j + 1i64; }\n\
              let mut c = 1i64;\n\
              while c < n { dp[c] = dp[c] + dp[c - 1i64]; c = c + 1i64; }\n\
              acc = acc + dp[n - 1i64];\n\
              k = k + 1i64;\n\
            }\n\
            acc\n\
        }\n";
        assert!(pins_vec(src, "dp"));
    }

    #[test]
    fn fires_on_fill_inside_if_block() {
        let src = "fn f(n: i64, flag: bool) -> i64 {\n\
            let mut acc = 0i64;\n\
            if flag {\n\
              let mut dp: Vec[i64] = Vec.new();\n\
              let mut j = 0i64;\n\
              while j < n { dp.push(1i64); j = j + 1i64; }\n\
              let mut c = 0i64;\n\
              while c < n { acc = acc + dp[c]; c = c + 1i64; }\n\
            }\n\
            acc\n\
        }\n";
        assert!(pins_vec(src, "dp"));
    }

    #[test]
    fn no_fire_two_vecs_same_name_siblings() {
        // Two sibling blocks each bind `dp` (two binding sites). A pin from the
        // first would linger (kept to end of function) and wrongly match the
        // second, shorter `dp`. The exactly-once gate refuses BOTH.
        let src = "fn f(n: i64) -> i64 {\n\
            let mut acc = 0i64;\n\
            let mut a = 1i64;\n\
            while a > 0i64 {\n\
              let mut dp: Vec[i64] = Vec.new();\n\
              let mut j = 0i64;\n\
              while j < n { dp.push(1i64); j = j + 1i64; }\n\
              let mut c = 0i64;\n\
              while c < n { acc = acc + dp[c]; c = c + 1i64; }\n\
              a = 0i64;\n\
            }\n\
            let mut b = 1i64;\n\
            while b > 0i64 {\n\
              let mut dp: Vec[i64] = Vec.new();\n\
              dp.push(1i64);\n\
              let mut c = 0i64;\n\
              while c < n { acc = acc + dp[c]; c = c + 1i64; }\n\
              b = 0i64;\n\
            }\n\
            acc\n\
        }\n";
        assert!(!pins_vec(src, "dp"));
    }

    // ── BoundTerm normalisation unit coverage ────────────────────

    #[test]
    fn normalize_matches_across_operand_forms() {
        use crate::token::Span;
        fn id(n: &str) -> Expr {
            Expr {
                kind: ExprKind::Identifier(n.to_string()),
                span: Span::default(),
            }
        }
        fn int(k: i64) -> Expr {
            Expr {
                kind: ExprKind::Integer(k, None),
                span: Span::default(),
            }
        }
        // Two structurally-identical `cols + 1` at different spans normalise equal.
        let a = Expr {
            kind: ExprKind::Binary {
                op: BinOp::Add,
                left: Box::new(id("cols")),
                right: Box::new(int(1)),
            },
            span: Span {
                offset: 10,
                ..Span::default()
            },
        };
        let b = Expr {
            kind: ExprKind::Binary {
                op: BinOp::Add,
                left: Box::new(id("cols")),
                right: Box::new(int(1)),
            },
            span: Span {
                offset: 99,
                ..Span::default()
            },
        };
        assert_eq!(normalize_bound(&a), normalize_bound(&b));
        // `cols` and `cols + 1` differ.
        assert_ne!(normalize_bound(&id("cols")), normalize_bound(&a));
        // Bare forms normalise to the expected terms.
        assert_eq!(
            normalize_bound(&id("n")),
            Some(BoundTerm::Ident("n".into()))
        );
        assert_eq!(normalize_bound(&int(5)), Some(BoundTerm::Int(5)));
    }

    #[test]
    fn nonbare_bound_not_matching_the_guard_does_not_fire() {
        // Fill bound `cols + 1` but the using guard is `c < cols` (a DIFFERENT
        // expression) — the normalised bounds differ, so no elision fires for a
        // `dp[c]` that could reach `cols` (== dp.len() here, in bounds, but the
        // point is the pin must key on the exact bound, not fire loosely).
        let src = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < cols + 1i64 { dp.push(1i64); j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        // The pin still EXISTS (keyed on `cols + 1`), but a `while c < cols`
        // guard won't resolve to it — that match happens in resolve_len_origin,
        // exercised by the E2E tests. Here we just confirm the pin is recorded
        // for the `cols + 1` bound (fill recognised).
        assert!(pins_vec(src, "dp"));
    }
}
