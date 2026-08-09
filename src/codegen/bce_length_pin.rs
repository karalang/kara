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
use std::collections::{BTreeMap, HashMap, HashSet};

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
pub(super) fn bound_idents(bt: &BoundTerm, out: &mut Vec<String>) {
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

pub(super) fn ident(e: &Expr) -> Option<String> {
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
                if let ParsedInterpolationPart::Expr(inner, _) = p {
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
                if let ParsedInterpolationPart::Expr(inner, _) = p {
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
/// or mut-marked arg), **including a write buried in a nested block**. Fails
/// open (true) on unanalyzed shapes.
///
/// The nested pre-check is load-bearing, not belt-and-braces (B-2026-08-04-13).
/// The structural match below inspects top-level targets and statement VALUES
/// only; it descends into inner blocks through `expr_writes_ident`, which sees
/// each nested statement's value but never its assignment TARGET. So
/// `if c { k = 100 }` read as "does not write `k`". Every caller uses this
/// answer to freeze a fact about `iv` across a region — a fill counter's
/// zero-ness (`counter_is_zero_before`), a fill bound's invariance
/// (`bound_invariant_in_block`), the +1-only stepping of a fill counter
/// (`writes_ident_other_than_plus_one_step`), an enclosing counter's guard
/// bound (`analyze_enclosing_body`), an index's init value
/// (`sole_scalar_init`) — and a missed write silently invalidates that fact,
/// which here means eliding a bounds check that was load-bearing. Three of
/// those five call sites were demonstrated to elide a real check and corrupt
/// the heap. `region_bindings` is this module's exhaustive, nesting-aware
/// collector (see its section comment, which names this exact gap); consulting
/// its assignment-target roots closes the class at the single point all five
/// callers share. Only `assigned` is consulted, not `rebound`: a nested `let`
/// shadows an inner name without writing the outer `iv`.
fn stmt_writes_ident(stmt: &Stmt, iv: &str) -> bool {
    if region_bindings(std::slice::from_ref(stmt), &None)
        .assigned
        .contains(iv)
    {
        return true;
    }
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
pub(super) fn expr_children_all<F: Fn(&Expr) -> bool + Copy>(e: &Expr, pred: F) -> bool {
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
            ParsedInterpolationPart::Expr(inner, _) => pred(inner),
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
pub(super) fn block_all<F: Fn(&Expr) -> bool + Copy>(block: &Block, pred: F) -> bool {
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

// ===================================================================
// Descending-loop bounds-check skip (B-2026-07-17-1)
// ===================================================================
//
// The ascending length-pin path above elides the upper-half check on
// `v[c]` when a dominating guard `while c < BOUND` proves `c < v.len()`
// (`BOUND == v.len()`). It does NOT reach the *rolling-1D-DP* idiom where
// the inner loop walks an index DOWNWARD:
//
// ```text
// let mut row = Vec.new();
// let mut j = 0; while j <= n { row.push(1); j = j + 1 }   // len == n + 1
// let mut i = 2;
// while i <= n {                       // enclosing counter: i <= n
//     let mut k = i - 1;               // k init = i - 1
//     while k >= 1 {                   // descending: k only decreases
//         row[k] = row[k] + row[k-1];  // <-- per-iteration bounds check
//         k = k - 1;
//     }
//     i = i + 1;
// }
// ```
//
// The descending guard `k >= 1` yields a LOWER bound only, so the upper
// half survives — a `cmp;ja` per iteration that LLVM cannot fold (it needs
// the RELATIONAL fact `k <= i-1 <= n-1 < len`, which its interval passes
// don't derive; confirmed empirically — an explicit `assume(k_init < len)`
// does not fold it either). Measured cost: ~1.20x vs equal-safety Rust on
// LeetCode #119 (bounds check is the whole gap).
//
// This pass recognises the shape and records, per inner descending loop, a
// **skip** telling codegen to push `UpperBound { idx_var: k, vec_var: v }`
// for that loop's body — routing through the SAME `asserted_index_bounds`
// channel the ascending path uses, so `emit_split_bounds_check` drops the
// upper half. The lower half (`k < 0`) is deliberately left in the IR;
// LLVM folds it trivially from the dominating `k >= LO` (LO >= 0) guard, so
// only the expensive check disappears.
//
// **Soundness.** The pushed fact `k < v.len()` must hold at every eval of
// `v[k]` in the body. The proof, entirely fail-closed:
//   1. A counted fill pins `v.len() >= B_pin` (`vec_length_lower_bounds`,
//      the same whole-function invariance gates as `compute_vec_length_pins`
//      plus the inclusive `<= BOUND` / `..=BOUND` forms → `B_pin == BOUND+1`).
//   2. `k` is monotone NON-INCREASING in the body: its ONLY writes are
//      top-level `k = k - C` / `k -= C` (C a positive int literal). So
//      `k <= k_init` holds at every point, regardless of ordering.
//   3. `k_init == E(i)` for a linear `E` non-decreasing in the enclosing
//      counter `i` (coefficient of `i` >= 0), and `i` is UNWRITTEN from the
//      enclosing loop's body-entry to the `k` init, so the enclosing guard
//      `i <= U` (or `i < U`) gives `i <= u_max` there.
//   4. The linear identity `B_pin - E[i := u_max] >= 1` holds — i.e.
//      `k_init <= E(u_max) = B_pin - (>=1) < B_pin <= v.len()`. Because the
//      subtraction cancels to a constant, every identifier of `E[i:=u_max]`
//      also appears in `B_pin` (identical coefficients), and `B_pin`'s
//      identifiers are proven invariant by (1) — so the relation holds at
//      runtime, not just symbolically. Overflow in evaluating `E` traps
//      (AOT), so a wrapped `k_init` never reaches the index (same footing
//      as the monotone-assume tier).
// A missed shape only keeps a bounds check; a wrongly-emitted skip would be
// an OOB read, so every recogniser fails closed on anything it does not
// fully understand.

/// A recognised descending-loop skip, keyed (in the returned map) by the
/// inner descending loop's condition span. Codegen pushes an
/// `UpperBound { idx_var, vec_var }` for each `vec_var` while compiling that
/// loop's body.
#[derive(Debug, Clone)]
pub(crate) struct DescendingSkip {
    pub idx_var: String,
    pub vec_vars: Vec<String>,
}

/// Analyse a function body and return the descending-loop skips it proves,
/// keyed by the inner loop's condition `SpanKey`.
pub(crate) fn compute_descending_skips(body: &Block) -> HashMap<SpanKey, DescendingSkip> {
    let lbs = vec_length_lower_bounds(body);
    let mut out = HashMap::new();
    if lbs.is_empty() {
        return out;
    }
    // Every loop is a candidate ENCLOSING loop; `for_each_block` visits the
    // block that lexically contains it, so each loop is seen as a statement of
    // its parent block.
    for_each_block(body, &mut |block| {
        scan_enclosing_loops(block, &lbs, &mut out)
    });
    out
}

/// For each direct loop statement in `block`, try to match it as the
/// enclosing loop of a descending-index rolling-DP inner loop.
fn scan_enclosing_loops(
    block: &Block,
    lbs: &HashMap<String, BoundTerm>,
    out: &mut HashMap<SpanKey, DescendingSkip>,
) {
    for stmt in &block.stmts {
        let StmtKind::Expr(e) = &stmt.kind else {
            continue;
        };
        let Some((counter, u_max, enc_body)) = as_enclosing_loop(e) else {
            continue;
        };
        analyze_enclosing_body(&counter, &u_max, enc_body, lbs, out);
    }
}

/// Scan an enclosing loop body for inner descending loops that qualify.
fn analyze_enclosing_body(
    counter: &str,
    u_max: &BoundTerm,
    enc_body: &Block,
    lbs: &HashMap<String, BoundTerm>,
    out: &mut HashMap<SpanKey, DescendingSkip>,
) {
    let stmts = &enc_body.stmts;
    for (pos, stmt) in stmts.iter().enumerate() {
        let StmtKind::Expr(e) = &stmt.kind else {
            continue;
        };
        let ExprKind::While {
            condition, body, ..
        } = &e.kind
        else {
            continue;
        };
        let Some(k) = as_descending_guard(condition) else {
            continue;
        };
        if k == counter {
            continue;
        }
        // The enclosing counter must be UNWRITTEN before this inner loop, so
        // the enclosing guard's `counter <= u_max` still holds at the `k` init.
        if stmts[..pos].iter().any(|s| stmt_writes_ident(s, counter)) {
            continue;
        }
        // `k` must be monotone non-increasing inside the inner loop.
        if !only_monotone_decrement(body, &k) {
            continue;
        }
        // `k`'s init must be a linear expression, its only pre-loop definition.
        let Some(e_bt) = sole_scalar_init(&stmts[..pos], &k) else {
            continue;
        };
        // Each pinned Vec indexed at `k` in the body whose length-lower-bound
        // beats the max init value gets its upper check skipped.
        let mut vec_vars: Vec<String> = lbs
            .iter()
            .filter(|(v, b_pin)| {
                block_indexes_vec_at(body, v, &k) && init_below_bound(&e_bt, counter, u_max, b_pin)
            })
            .map(|(v, _)| v.clone())
            .collect();
        if vec_vars.is_empty() {
            continue;
        }
        vec_vars.sort();
        out.insert(
            SpanKey::from_span(&condition.span),
            DescendingSkip {
                idx_var: k,
                vec_vars,
            },
        );
    }
}

/// Map each counted-fill Vec to a length LOWER bound `v.len() >= BOUND`.
/// Mirrors `compute_vec_length_pins`'s soundness gates but is keyed by Vec
/// name and additionally recognises the INCLUSIVE fill forms
/// (`while j <= BOUND` / `for i in 0..=BOUND` → `BOUND + 1`) that the
/// ascending pin path deliberately omits. Kept separate so the ascending
/// path stays byte-identical.
pub(super) fn vec_length_lower_bounds(body: &Block) -> HashMap<String, BoundTerm> {
    let whole = region_bindings(&body.stmts, &body.final_expr);
    let mut out = HashMap::new();
    for_each_block(body, &mut |block| {
        analyze_block_lbs(block, &whole, &mut out)
    });
    out
}

fn analyze_block_lbs(block: &Block, whole: &RegionBindings, out: &mut HashMap<String, BoundTerm>) {
    let stmts = &block.stmts;
    for (li, stmt) in stmts.iter().enumerate() {
        // `let v = Vec.filled(E, x)` states the length outright: `v.len() == E`,
        // so no fill loop needs to be found. The post-binding stability gates
        // below are shared with the counted-fill path.
        if let Some((v, size)) = filled_vec_binding(stmt) {
            if whole.rebound.get(&v) == Some(&1) {
                if let Some(bound) = normalize_bound(size) {
                    if lb_stable_after(block, li, &v, &bound) {
                        out.insert(v, bound);
                        continue;
                    }
                }
            }
            continue;
        }
        let Some(v) = empty_vec_binding(stmt) else {
            continue;
        };
        if whole.rebound.get(&v) != Some(&1) {
            continue;
        }
        let Some(fill) = find_counted_fill_lb(stmts, li, &v) else {
            continue;
        };
        let Some(bound) = normalize_bound(&fill.bound) else {
            continue;
        };
        if let Some(iv) = &fill.counter {
            if !counter_is_zero_before(stmts, fill.fi, iv) {
                continue;
            }
        }
        // Inclusive fill runs one extra iteration, so `len >= BOUND + 1`.
        let lb = if fill.inclusive {
            BoundTerm::Bin(
                BoundOp::Add,
                Box::new(bound.clone()),
                Box::new(BoundTerm::Int(1)),
            )
        } else {
            bound.clone()
        };
        if !lb_stable_after(block, fill.fi, &v, &bound) {
            continue;
        }
        out.insert(v, lb);
    }
}

/// Shared post-definition soundness gate for a length lower bound `v.len() >=
/// BOUND` established at `stmts[def_idx]`: from there to the end of the block,
/// `v`'s length must be stable and every identifier of `BOUND` unwritten, so
/// the relation cannot be falsified at any later index site.
fn lb_stable_after(block: &Block, def_idx: usize, v: &str, bound: &BoundTerm) -> bool {
    let after = &block.stmts[def_idx + 1..];
    if !vec_len_stable_after(after, &block.final_expr, v) {
        return false;
    }
    let mut idents = Vec::new();
    bound_idents(bound, &mut idents);
    if idents
        .iter()
        .any(|b| !var_unwritten_after(after, &block.final_expr, b))
    {
        return false;
    }
    let region = region_bindings(after, &block.final_expr);
    if region.is_rebound(v) {
        return false;
    }
    !idents
        .iter()
        .any(|b| region.is_rebound(b) || region.assigned.contains(b))
}

/// `let v = Vec.filled(SIZE, elem)` → `(v, &SIZE)`. `Vec.filled` produces a Vec
/// of exactly `SIZE` elements, so it pins the length directly rather than via a
/// counted fill loop. (A negative `SIZE` traps in the runtime constructor, so a
/// surviving binding always has `len == SIZE >= 0`.)
fn filled_vec_binding(stmt: &Stmt) -> Option<(String, &Expr)> {
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
    if segments.len() == 2 && segments[0] == "Vec" && segments[1] == "filled" && args.len() == 2 {
        Some((name.clone(), &args[0].value))
    } else {
        None
    }
}

/// Like `vec_readonly_after`, but additionally tolerates a bare-`v` TAIL
/// expression (returning the Vec). A tail move-out happens after every index
/// site and leaves the length unchanged, and a post-move index would not
/// type-check, so `len >= B_pin` still holds wherever `v[..]` is read/written.
/// The ascending pin path keeps the stricter `vec_readonly_after` (it never
/// needs to pin a returned Vec), so this relaxation is local to the
/// descending analysis.
fn vec_len_stable_after(stmts: &[Stmt], final_expr: &Option<Box<Expr>>, v: &str) -> bool {
    stmts.iter().all(|s| stmt_vec_len_stable(s, v))
        && match final_expr.as_deref() {
            None => true,
            Some(e) => is_ident_expr(e, v) || expr_vec_len_stable(e, v),
        }
}

/// `stmt_vec_readonly`'s twin over the relaxed expression rule below.
fn stmt_vec_len_stable(stmt: &Stmt, v: &str) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } => expr_vec_len_stable(value, v),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            expr_vec_len_stable(target, v) && expr_vec_len_stable(value, v)
        }
        StmtKind::Expr(e) => expr_vec_len_stable(e, v),
        StmtKind::LetUninit { name, .. } => name != v,
        // Unanalyzed statement kinds — fail closed.
        _ => false,
    }
}

/// Relaxed twin of `expr_vec_readonly` for the length LOWER-bound path: same
/// rules, plus `v` may be passed as an **unmarked argument to a FREE-FUNCTION
/// call**. That one relaxation is what lets the pin survive the overwhelmingly
/// common shape of handing a buffer to a helper that walks it — without it,
/// a single `is_strobogrammatic(corpus, base, len)` in the program disqualifies
/// every index site in the function (measured on kata #246: adding one
/// read-only `ref Vec[u8]` call to an otherwise-firing probe put both bounds
/// checks back).
///
/// **Soundness — why an unmarked free-function argument cannot change the
/// length.** design.md Feature 4 Part 1½: a free-function call writes `mut` on
/// any argument whose place-expression root is a fresh owned binding when the
/// callee's parameter is `mut ref T` / `mut Slice[T]`. Every `v` this path pins
/// is introduced by a `let v = Vec.new()/with_capacity()/filled()` in the same
/// function (see `empty_vec_binding` / `filled_vec_binding`), so it is exactly
/// such an owned binding and never a `mut ref` binding being forwarded — the
/// one case the spec lets through unmarked. So:
///   - callee takes `mut ref Vec` / `mut Slice` ⇒ the marker is mandatory here,
///     and we refuse marked arguments, so this case never reaches us;
///   - callee takes `ref Vec` ⇒ an immutable borrow cannot resize;
///   - callee takes `Vec` by value ⇒ the call MOVES `v`, and any later `v[..]`
///     would be a use-after-move the ownership checker already rejects, so in
///     any program that compiles there is no subsequent index site to protect.
///
/// **METHOD-call arguments are deliberately NOT relaxed.** Method calls never
/// write the `mut` marker (design.md, same section), so an unmarked argument to
/// a method proves nothing about the parameter's mode — the reasoning above
/// collapses entirely. `other.absorb(v)` must keep disqualifying.
fn expr_vec_len_stable(e: &Expr, v: &str) -> bool {
    match &e.kind {
        ExprKind::Identifier(n) => n != v,
        ExprKind::Index { object, index } => {
            let base_ok = match &object.kind {
                ExprKind::Identifier(n) if n == v => true,
                _ => expr_vec_len_stable(object, v),
            };
            base_ok && expr_vec_len_stable(index, v)
        }
        // Free-function call: an unmarked bare `v` argument is length-stable.
        ExprKind::Call { callee, args } => {
            expr_vec_len_stable(callee, v)
                && args.iter().all(|a| {
                    if !a.mut_marker && is_ident_expr(&a.value, v) {
                        return true;
                    }
                    expr_vec_len_stable(&a.value, v)
                })
        }
        // `v.len()` / `v.is_empty()` — read-only receiver methods. Arguments
        // are scanned with the STRICT rule (no marker is written for them).
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
            expr_vec_len_stable(object, v) && args.iter().all(|a| expr_vec_readonly(&a.value, v))
        }
        _ => expr_children_all(e, |c| expr_vec_len_stable(c, v)),
    }
}

/// One recognised counted fill for the lower-bound map (all four shapes).
struct FillLB {
    fi: usize,
    bound: Expr,
    counter: Option<String>,
    inclusive: bool,
}

/// Like `find_exact_fill_loop`, but also matches the inclusive `while j <=
/// BOUND` and `for i in 0..=BOUND` forms and reports which was found.
fn find_counted_fill_lb(stmts: &[Stmt], let_idx: usize, v: &str) -> Option<FillLB> {
    for (off, stmt) in stmts[let_idx + 1..].iter().enumerate() {
        let fi = let_idx + 1 + off;
        if let StmtKind::Expr(e) = &stmt.kind {
            if let ExprKind::While {
                condition, body, ..
            } = &e.kind
            {
                if let Some((iv, bound)) = as_strict_lt(condition) {
                    if iv != v && while_fill_body_ok(body, v, &iv, bound) {
                        return Some(FillLB {
                            fi,
                            bound: bound.clone(),
                            counter: Some(iv),
                            inclusive: false,
                        });
                    }
                }
                if let Some((iv, bound)) = as_le(condition) {
                    if iv != v && while_fill_body_ok(body, v, &iv, bound) {
                        return Some(FillLB {
                            fi,
                            bound: bound.clone(),
                            counter: Some(iv),
                            inclusive: true,
                        });
                    }
                }
                if block_mentions_ident(body, v) || expr_mentions_ident(condition, v) {
                    return None;
                }
                continue;
            }
            if let ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } = &e.kind
            {
                if let Some((bound, inclusive)) = for_zero_range_end_lb(pattern, iterable, v) {
                    if for_fill_body_ok(body, v, bound) {
                        return Some(FillLB {
                            fi,
                            bound: bound.clone(),
                            counter: None,
                            inclusive,
                        });
                    }
                }
                if block_mentions_ident(body, v) || expr_mentions_ident(iterable, v) {
                    return None;
                }
                continue;
            }
        }
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

/// `cond` as `(iv, &BOUND)` for an inclusive `iv <= BOUND` — surface
/// `Binary(LtEq)` or trait-lowered `Call { Path([ty,"le"]), [iv, BOUND] }`.
fn as_le(cond: &Expr) -> Option<(String, &Expr)> {
    match &cond.kind {
        ExprKind::Binary {
            op: BinOp::LtEq,
            left,
            right,
        } => Some((ident(left)?, right)),
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() == 2 && segments[1] == "le" {
                Some((ident(&args[0].value)?, &args[1].value))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `for I in 0..BOUND` / `0..=BOUND` (start `0` or omitted): `(&BOUND,
/// inclusive)` when the loop var is a plain binding distinct from `v`.
fn for_zero_range_end_lb<'a>(
    pattern: &Pattern,
    iterable: &'a Expr,
    v: &str,
) -> Option<(&'a Expr, bool)> {
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
    let start_zero = matches!(
        start.as_deref().map(|e| &e.kind),
        None | Some(ExprKind::Integer(0, _))
    );
    if !start_zero {
        return None;
    }
    Some((end.as_deref()?, *inclusive))
}

/// An enclosing loop whose counter has an upper bound: `(counter, u_max,
/// body)` where `u_max` is the counter's MAX value in the body as a
/// `BoundTerm` (`U` for `<= U` / `..=U`, `U - 1` for `< U` / `..U`).
pub(super) fn as_enclosing_loop(e: &Expr) -> Option<(String, BoundTerm, &Block)> {
    match &e.kind {
        ExprKind::While {
            condition, body, ..
        } => {
            let (counter, upper, inclusive) = as_counter_upper(condition)?;
            let u_bt = normalize_bound(upper)?;
            Some((counter, dec_if_exclusive(u_bt, inclusive), body))
        }
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            let PatternKind::Binding(i) = &pattern.kind else {
                return None;
            };
            let ExprKind::Range {
                end: Some(end),
                inclusive,
                ..
            } = &iterable.kind
            else {
                return None;
            };
            let u_bt = normalize_bound(end)?;
            Some((i.clone(), dec_if_exclusive(u_bt, *inclusive), body))
        }
        _ => None,
    }
}

/// `u_max` for an inclusive bound is `U`; for an exclusive bound it is
/// `U - 1` (the counter never reaches `U`).
fn dec_if_exclusive(u: BoundTerm, inclusive: bool) -> BoundTerm {
    if inclusive {
        u
    } else {
        BoundTerm::Bin(BoundOp::Sub, Box::new(u), Box::new(BoundTerm::Int(1)))
    }
}

/// A guard `counter <cmp> U`: `(counter, &U, inclusive)`. Matches `<=`/`<`
/// in surface `Binary` and trait-lowered `Call` forms.
fn as_counter_upper(cond: &Expr) -> Option<(String, &Expr, bool)> {
    if let Some((iv, u)) = as_le(cond) {
        return Some((iv, u, true));
    }
    if let Some((iv, u)) = as_strict_lt(cond) {
        return Some((iv, u, false));
    }
    None
}

/// A descending guard `k >= LO` / `k > LO` → `Some(k)`. `LO` is unconstrained
/// (the lower half of the check is left for LLVM to fold from this guard).
fn as_descending_guard(cond: &Expr) -> Option<String> {
    match &cond.kind {
        ExprKind::Binary {
            op: BinOp::GtEq | BinOp::Gt,
            left,
            ..
        } => ident(left),
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() == 2 && matches!(segments[1].as_str(), "ge" | "gt") {
                ident(&args[0].value)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Every write to `k` in `body` is a top-level `k = k - C` / `k -= C` (C a
/// positive int literal), and at least one such decrement exists. Any other
/// write form disqualifies — so `k` is provably non-increasing, hence
/// `k <= k_init` throughout.
///
/// Non-decrement writes are detected with `region_bindings` (exhaustive over
/// assignment-target roots and pattern binds, including NESTED ones) plus
/// `stmt_writes_bound` (the `mut`-marked-arg / receiver case region_bindings
/// does not track). `stmt_writes_ident` alone is insufficient: it is blind to
/// an assignment target buried in an inner block (e.g. `if c { k = i }`), which
/// would break monotonicity — see the region-binding collector's section
/// comment.
pub(super) fn only_monotone_decrement(body: &Block, k: &str) -> bool {
    let mut saw_decrement = false;
    for s in &body.stmts {
        if is_clean_decrement_stmt(s, k) {
            saw_decrement = true;
            continue;
        }
        if stmt_touches_var(std::slice::from_ref(s), &None, k) {
            return false;
        }
    }
    if body.final_expr.is_some() && stmt_touches_var(&[], &body.final_expr, k) {
        return false;
    }
    saw_decrement
}

/// Whether `k` is assigned (any nesting), rebound, or `mut`-written across the
/// given region.
pub(super) fn stmt_touches_var(stmts: &[Stmt], final_expr: &Option<Box<Expr>>, k: &str) -> bool {
    let rb = region_bindings(stmts, final_expr);
    if rb.assigned.contains(k) || rb.is_rebound(k) {
        return true;
    }
    stmts.iter().any(|s| stmt_writes_bound(s, k))
        || final_expr.as_ref().is_some_and(|e| expr_writes_bound(e, k))
}

fn is_clean_decrement_stmt(s: &Stmt, k: &str) -> bool {
    match &s.kind {
        StmtKind::Assign { target, value } if is_ident_expr(target, k) => {
            is_sub_pos_const(value, k)
        }
        StmtKind::CompoundAssign {
            target,
            op: CompoundOp::Sub,
            value,
        } if is_ident_expr(target, k) => is_pos_int_lit(value),
        _ => false,
    }
}

/// `value` is `k - C` (C a positive int literal) — surface `Binary(Sub)` or
/// trait-lowered `Call { Path([ty,"sub"]), [k, C] }`.
fn is_sub_pos_const(value: &Expr, k: &str) -> bool {
    match &value.kind {
        ExprKind::Binary {
            op: BinOp::Sub,
            left,
            right,
        } => is_ident_expr(left, k) && is_pos_int_lit(right),
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return false;
            };
            segments.len() == 2
                && segments[1] == "sub"
                && is_ident_expr(&args[0].value, k)
                && is_pos_int_lit(&args[1].value)
        }
        _ => false,
    }
}

fn is_ident_expr(e: &Expr, name: &str) -> bool {
    matches!(&e.kind, ExprKind::Identifier(n) if n == name)
}

fn is_pos_int_lit(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Integer(n, _) if *n >= 1)
}

/// `k`'s sole scalar definition among `stmts` (the statements preceding the
/// inner loop): the last top-level `let k = E` / `k = E` with `E` a
/// pure-arithmetic (linear-normalisable) expression, provided `k` is written
/// nowhere else (nested, compound, mut-arg). `None` on any other shape.
pub(super) fn sole_scalar_init(stmts: &[Stmt], k: &str) -> Option<BoundTerm> {
    let mut last: Option<BoundTerm> = None;
    for s in stmts {
        if let Some(rhs) = top_level_scalar_def(s, k) {
            // A non-normalisable init means we cannot reason about the value.
            last = Some(normalize_bound(rhs)?);
            continue;
        }
        if stmt_writes_ident(s, k) {
            return None;
        }
    }
    last
}

fn top_level_scalar_def<'a>(s: &'a Stmt, k: &str) -> Option<&'a Expr> {
    match &s.kind {
        StmtKind::Let { pattern, value, .. } if matches!(&pattern.kind, PatternKind::Binding(n) if n == k) => {
            Some(value)
        }
        StmtKind::Assign { target, value } if is_ident_expr(target, k) => Some(value),
        _ => None,
    }
}

/// Does `body` contain an index `v[k]` / `v[k ± c]` (root `v`, index var `k`)?
fn block_indexes_vec_at(body: &Block, v: &str, k: &str) -> bool {
    let pred = |e: &Expr| {
        if let ExprKind::Index { object, index } = &e.kind {
            if let ExprKind::Identifier(n) = &object.kind {
                return n == v && index_var_is(index, k);
            }
        }
        false
    };
    body_any(body, &pred)
}

/// The index expression's variable root is `k`, with an optional constant
/// offset: `k`, `k + c`, `c + k`, `k - c`.
fn index_var_is(index: &Expr, k: &str) -> bool {
    if is_ident_expr(index, k) {
        return true;
    }
    match &index.kind {
        ExprKind::Binary {
            op: BinOp::Add,
            left,
            right,
        } => {
            (is_ident_expr(left, k) && is_int_lit(right))
                || (is_int_lit(left) && is_ident_expr(right, k))
        }
        ExprKind::Binary {
            op: BinOp::Sub,
            left,
            right,
        } => is_ident_expr(left, k) && is_int_lit(right),
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return false;
            };
            if segments.len() != 2 {
                return false;
            }
            match segments[1].as_str() {
                "add" => {
                    (is_ident_expr(&args[0].value, k) && is_int_lit(&args[1].value))
                        || (is_int_lit(&args[0].value) && is_ident_expr(&args[1].value, k))
                }
                "sub" => is_ident_expr(&args[0].value, k) && is_int_lit(&args[1].value),
                _ => false,
            }
        }
        _ => false,
    }
}

fn is_int_lit(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Integer(_, _))
}

/// Deep "some sub-expression satisfies `pred`" over a block.
pub(super) fn body_any(block: &Block, pred: &dyn Fn(&Expr) -> bool) -> bool {
    !block_all(block, |c| !expr_contains(c, pred))
}

fn expr_contains(e: &Expr, pred: &dyn Fn(&Expr) -> bool) -> bool {
    pred(e) || !expr_children_all(e, |c| !expr_contains(c, pred))
}

// ── Linear normal form for the relational `k_init < B_pin` proof ────

/// A linear combination of identifiers plus a constant. Used to prove the
/// symbolic identity `B_pin - E[i := u_max] = const >= 1`.
#[derive(Clone, Default)]
struct Linear {
    terms: BTreeMap<String, i64>,
    konst: i64,
}

/// Normalise a `BoundTerm` to `Linear`, or `None` if it is not affine
/// (multiplication of two non-constants, any division/remainder, or an
/// arithmetic overflow in the coefficients — all fail closed).
fn bound_to_linear(bt: &BoundTerm) -> Option<Linear> {
    match bt {
        BoundTerm::Int(n) => Some(Linear {
            terms: BTreeMap::new(),
            konst: *n,
        }),
        BoundTerm::Ident(s) => {
            let mut terms = BTreeMap::new();
            terms.insert(s.clone(), 1);
            Some(Linear { terms, konst: 0 })
        }
        BoundTerm::Bin(op, l, r) => {
            let a = bound_to_linear(l)?;
            let b = bound_to_linear(r)?;
            match op {
                BoundOp::Add => lin_add(&a, &b),
                BoundOp::Sub => {
                    let neg = lin_scale(&b, -1)?;
                    lin_add(&a, &neg)
                }
                BoundOp::Mul => {
                    if a.terms.is_empty() {
                        lin_scale(&b, a.konst)
                    } else if b.terms.is_empty() {
                        lin_scale(&a, b.konst)
                    } else {
                        None
                    }
                }
                BoundOp::Div | BoundOp::Rem => None,
            }
        }
    }
}

fn lin_add(a: &Linear, b: &Linear) -> Option<Linear> {
    let mut terms = a.terms.clone();
    for (k, v) in &b.terms {
        let e = terms.entry(k.clone()).or_insert(0);
        *e = e.checked_add(*v)?;
    }
    terms.retain(|_, v| *v != 0);
    Some(Linear {
        terms,
        konst: a.konst.checked_add(b.konst)?,
    })
}

fn lin_scale(a: &Linear, c: i64) -> Option<Linear> {
    if c == 0 {
        return Some(Linear::default());
    }
    let mut terms = BTreeMap::new();
    for (k, v) in &a.terms {
        terms.insert(k.clone(), v.checked_mul(c)?);
    }
    Some(Linear {
        terms,
        konst: a.konst.checked_mul(c)?,
    })
}

/// Prove `k_init < B_pin` where `k_init == E(counter)`: substitute
/// `counter := u_max` in `E` (valid because `E` is non-decreasing in the
/// counter and `counter <= u_max`), then check `B_pin - E[counter:=u_max]`
/// is a positive constant.
pub(super) fn init_below_bound(
    e_bt: &BoundTerm,
    counter: &str,
    u_max: &BoundTerm,
    b_pin: &BoundTerm,
) -> bool {
    let (Some(e_lin), Some(u_lin), Some(pin_lin)) = (
        bound_to_linear(e_bt),
        bound_to_linear(u_max),
        bound_to_linear(b_pin),
    ) else {
        return false;
    };
    // `E` must be non-decreasing in the counter, so its max over `counter <=
    // u_max` is at `counter == u_max`.
    let a = *e_lin.terms.get(counter).unwrap_or(&0);
    if a < 0 {
        return false;
    }
    // init_max = (E without the counter term) + a * u_max
    let mut e_without = e_lin.clone();
    e_without.terms.remove(counter);
    let (Some(scaled_u), ..) = (lin_scale(&u_lin, a),) else {
        return false;
    };
    let Some(init_max) = lin_add(&e_without, &scaled_u) else {
        return false;
    };
    // diff = B_pin - init_max must be a strictly positive constant.
    let Some(neg_init) = lin_scale(&init_max, -1) else {
        return false;
    };
    let Some(diff) = lin_add(&pin_lin, &neg_init) else {
        return false;
    };
    diff.terms.is_empty() && diff.konst >= 1
}

// ===================================================================
// Converging two-pointer + base-offset skip (B-2026-08-04-8)
// ===================================================================
//
// Neither the ascending pin path nor the descending skip above reaches the
// **row-major two-pointer** shape — a flat `N * STRIDE` buffer walked from
// both ends within each row:
//
// ```text
// let mut v = Vec.filled(n * len, 0u8);      // len == n * len
// let mut i = 0;
// while i < n {
//     let base = i * len;                    // row offset
//     let mut lo = 0;
//     let mut hi = len - 1;
//     while lo <= hi {                       // CONVERGING: two moving indices
//         ... v[base + lo] ... v[base + hi];  // <-- two bounds checks/iteration
//         lo = lo + 1;
//         hi = hi - 1;
//     }
//     i = i + 1;
// }
// ```
//
// Measured on LeetCode #246: neither ingredient alone defeats LLVM —
// `base + ascending`, `base + descending`, converging-without-base, and
// both-ends-from-a-single-ascending-IV all elide cleanly. Only a base offset
// **combined with** a converging two-variable guard survives, because the
// index is the SUM `base + j` (so the existing bare-index/constant-offset
// matcher does not apply) and `j`'s range is fixed jointly by the two
// co-moving variables rather than by a single affine IV. Cost: 1.93x on a
// work-identical probe that differs only in the guard shape.
//
// **Soundness.** The pushed fact is `base + j < v.len()` at every eval of
// `v[base + j]` in the inner body. The proof, entirely fail-closed:
//   1. A `Vec.filled(E, _)` or counted fill pins `v.len() >= B_pin`
//      (`vec_length_lower_bounds`, whole-function invariance gates).
//   2. `hi` is monotone NON-INCREASING in the inner body (only top-level
//      `hi = hi - C` / `hi -= C`), so `hi <= hi_init` at every point.
//   3. `lo`'s only writes are top-level `lo = lo + C` / `lo += C` that come
//      AFTER every other mention of `lo` in the body. So at each `v[base+lo]`,
//      `lo` still holds its body-entry value, where the loop guard gives
//      `lo <= hi` (or `lo < hi`) and hence `lo <= hi <= hi_init`. The
//      ordering gate is load-bearing: unlike the descending case, an
//      ascending index that is stepped BEFORE its use can exceed the bound
//      the guard established.
//   4. So BOTH indices are `<= hi_init`, and it suffices to prove
//      `base + hi_init < B_pin`. `base` is unwritten in the inner body
//      (loop-invariant) and its sole init is a linear `E_base(i)`; `i` is
//      unwritten from the enclosing body-entry to that init, so the enclosing
//      guard `i <= u_max` holds there.
//   5. `init_below_bound` proves `B_pin - (E_base + hi_init)[i := u_max] >= 1`
//      as a linear identity — the same cancellation the descending path uses,
//      so every identifier of the substituted sum also appears in `B_pin`
//      with an identical coefficient and the relation holds at runtime.
//
// Because the row extent (`n`, `len`) is typically a literal binding rather
// than a symbolic parameter, and `n * len` is NOT affine in `{n, len}`, the
// proof first substitutes immutable integer-literal `let`s (`fold_consts`).
// Without that step the identity is a product of two symbolic terms and
// `bound_to_linear` fails closed.
//
// Only the UPPER half is skipped; the lower (`idx < 0`) half stays for LLVM
// to fold from the monotone assumes, exactly as in the descending path.

/// A recognised converging two-pointer skip, keyed (in the returned map) by
/// the inner loop's condition span. Codegen pushes an `UpperBoundSum` fact per
/// (`idx_var`, `vec_var`) pair while compiling that loop's body.
#[derive(Debug, Clone)]
pub(crate) struct ConvergingSkip {
    /// The loop-invariant row offset (`base` in `v[base + lo]`).
    pub base_var: String,
    /// The converging index variables (`lo` and `hi`), both proven
    /// `base + idx < v.len()`.
    pub idx_vars: Vec<String>,
    /// The Vecs those facts apply to.
    pub vec_vars: Vec<String>,
    /// Whether `base + idx >= 0` was also proven, so the bounds check can be
    /// dropped ENTIRELY rather than leaving the sign half behind.
    pub lower_proven: bool,
}

/// Analyse a function body and return the converging two-pointer skips it
/// proves, keyed by the inner loop's condition `SpanKey`.
pub(crate) fn compute_converging_skips(body: &Block) -> HashMap<SpanKey, ConvergingSkip> {
    let mut out = HashMap::new();
    let lbs = vec_length_lower_bounds(body);
    if lbs.is_empty() {
        return out;
    }
    let consts = int_const_bindings(body);
    let lbs: HashMap<String, BoundTerm> = lbs
        .into_iter()
        .map(|(v, b)| {
            let folded = fold_consts(&b, &consts);
            (v, folded)
        })
        .collect();
    for_each_block(body, &mut |block| {
        scan_enclosing_loops_conv(block, &lbs, &consts, &mut out)
    });
    out
}

fn scan_enclosing_loops_conv(
    block: &Block,
    lbs: &HashMap<String, BoundTerm>,
    consts: &HashMap<String, i64>,
    out: &mut HashMap<SpanKey, ConvergingSkip>,
) {
    for (pos, stmt) in block.stmts.iter().enumerate() {
        let StmtKind::Expr(e) = &stmt.kind else {
            continue;
        };
        let Some((counter, u_max, enc_body)) = as_enclosing_loop(e) else {
            continue;
        };
        let u_max = fold_consts(&u_max, consts);
        // The counter's MINIMUM, used only for the optional lower-half proof:
        // a `for` range states it outright, a `while` takes it from the
        // counter's sole definition before the loop. `None` simply means the
        // lower half is not proven.
        let u_min = counter_min(e, &block.stmts[..pos], &counter).map(|m| fold_consts(&m, consts));
        analyze_enclosing_body_conv(&counter, &u_max, u_min.as_ref(), enc_body, lbs, consts, out);
    }
}

/// The enclosing counter's minimum value: the `for` range's start (`0` when
/// omitted), or the `while` counter's sole scalar definition among the
/// statements preceding the loop.
pub(super) fn counter_min(e: &Expr, before: &[Stmt], counter: &str) -> Option<BoundTerm> {
    match &e.kind {
        ExprKind::For { iterable, .. } => {
            let ExprKind::Range { start, .. } = &iterable.kind else {
                return None;
            };
            match start.as_deref() {
                None => Some(BoundTerm::Int(0)),
                Some(st) => normalize_bound(st),
            }
        }
        _ => sole_scalar_init(before, counter),
    }
}

/// Scan an enclosing loop body for inner converging loops that qualify.
#[allow(clippy::too_many_arguments)]
fn analyze_enclosing_body_conv(
    counter: &str,
    u_max: &BoundTerm,
    u_min: Option<&BoundTerm>,
    enc_body: &Block,
    lbs: &HashMap<String, BoundTerm>,
    consts: &HashMap<String, i64>,
    out: &mut HashMap<SpanKey, ConvergingSkip>,
) {
    let stmts = &enc_body.stmts;
    for (pos, stmt) in stmts.iter().enumerate() {
        let StmtKind::Expr(e) = &stmt.kind else {
            continue;
        };
        let ExprKind::While {
            condition, body, ..
        } = &e.kind
        else {
            continue;
        };
        let Some((lo, hi)) = as_converging_guard(condition) else {
            continue;
        };
        if lo == hi || lo == counter || hi == counter {
            continue;
        }
        // The enclosing counter must be UNWRITTEN before this inner loop, so
        // the enclosing guard's `counter <= u_max` still holds at the inits.
        if stmts[..pos].iter().any(|s| stmt_writes_ident(s, counter)) {
            continue;
        }
        // `hi` non-increasing everywhere; `lo`'s steps strictly after its uses.
        if !only_monotone_decrement(body, &hi) || !increments_are_trailing(body, &lo) {
            continue;
        }
        let Some(hi_init) = sole_scalar_init(&stmts[..pos], &hi) else {
            continue;
        };
        let hi_init = fold_consts(&hi_init, consts);
        // Lower-half prerequisites, all optional: both indices must still hold
        // their body-entry values at every use (so the guard's `lo_init <= lo
        // <= hi` chain applies to `hi` too), and `lo`'s init must be placeable.
        let lo_init = sole_scalar_init(&stmts[..pos], &lo).map(|b| fold_consts(&b, consts));
        let lower_shape_ok = decrements_are_trailing(body, &hi) && lo_init.is_some();

        // Candidate `(vec, base)` pairs actually indexed as `v[base + lo/hi]`.
        let idx_names = [lo.clone(), hi.clone()];
        let mut chosen: Option<(String, Vec<String>)> = None;
        // A single skip covers every (idx, vec) pair it names, so the lower
        // half is claimed only if it holds for ALL of them.
        let mut lower_all = true;
        for (v, base) in collect_base_indexed(body, &idx_names) {
            if base == lo || base == hi || !lbs.contains_key(&v) {
                continue;
            }
            // `base` must be loop-invariant across the inner body.
            if stmt_touches_var(&body.stmts, &body.final_expr, &base) {
                continue;
            }
            let Some(e_base) = sole_scalar_init(&stmts[..pos], &base) else {
                continue;
            };
            let e_base_kept = fold_consts(&e_base, consts);
            let max_idx = BoundTerm::Bin(
                BoundOp::Add,
                Box::new(e_base_kept.clone()),
                Box::new(hi_init.clone()),
            );
            if !init_below_bound(&max_idx, counter, u_max, &lbs[&v]) {
                continue;
            }
            // `base + lo_init >= 0` with the counter at its minimum proves the
            // sign half for BOTH indices (each is `>= lo_init` at its use).
            let lower_ok = lower_shape_ok
                && match (u_min, &lo_init) {
                    (Some(m), Some(li)) => sum_nonneg_at_min(&e_base_kept, li, counter, m),
                    _ => false,
                };
            lower_all &= lower_ok;
            // One `base` per inner loop keeps the emitted fact unambiguous;
            // a second distinct base disqualifies rather than guesses.
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
        let mut idx_vars = vec![lo, hi];
        idx_vars.sort();
        out.insert(
            SpanKey::from_span(&condition.span),
            ConvergingSkip {
                base_var,
                idx_vars,
                vec_vars,
                lower_proven: lower_all,
            },
        );
    }
}

/// A converging guard `lo <= hi` / `lo < hi` with both sides bare identifiers.
pub(super) fn as_converging_guard(cond: &Expr) -> Option<(String, String)> {
    match &cond.kind {
        ExprKind::Binary {
            op: BinOp::LtEq | BinOp::Lt,
            left,
            right,
        } => Some((ident(left)?, ident(right)?)),
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() == 2 && matches!(segments[1].as_str(), "le" | "lt") {
                Some((ident(&args[0].value)?, ident(&args[1].value)?))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Every write to `lo` in `body` is a top-level `lo = lo + C` / `lo += C` (C a
/// positive int literal), and `lo` is not mentioned at all after the first such
/// step. So at every OTHER mention of `lo` — in particular every `v[base + lo]`
/// index — `lo` still holds its body-entry value, which the loop guard bounds.
///
/// Zero steps is accepted (an unchanged `lo` trivially keeps its entry value);
/// what must be excluded is a step that happens BEFORE a use, or any write
/// form other than a clean increment.
pub(super) fn increments_are_trailing(body: &Block, lo: &str) -> bool {
    let mut stepped = false;
    for s in &body.stmts {
        if is_clean_increment_stmt(s, lo) {
            stepped = true;
            continue;
        }
        if stepped {
            if stmt_mentions_ident(s, lo) {
                return false;
            }
        } else if stmt_touches_var(std::slice::from_ref(s), &None, lo) {
            return false;
        }
    }
    let Some(e) = &body.final_expr else {
        return true;
    };
    if stmt_touches_var(&[], &body.final_expr, lo) {
        return false;
    }
    !(stepped && expr_mentions_ident(e, lo))
}

/// Mirror of `increments_are_trailing` for the DEcrementing index: every write
/// to `hi` is a top-level clean decrement and `hi` is not mentioned after the
/// first one. Needed only for the LOWER half — the upper half rides on plain
/// monotonicity (`hi <= hi_init` regardless of ordering), but a lower bound on
/// a shrinking value is only valid before it shrinks.
pub(super) fn decrements_are_trailing(body: &Block, hi: &str) -> bool {
    let mut stepped = false;
    for s in &body.stmts {
        if is_clean_decrement_stmt(s, hi) {
            stepped = true;
            continue;
        }
        if stepped {
            if stmt_mentions_ident(s, hi) {
                return false;
            }
        } else if stmt_touches_var(std::slice::from_ref(s), &None, hi) {
            return false;
        }
    }
    let Some(e) = &body.final_expr else {
        return true;
    };
    if stmt_touches_var(&[], &body.final_expr, hi) {
        return false;
    }
    !(stepped && expr_mentions_ident(e, hi))
}

/// Prove `E_base + lo_init >= 0` by evaluating at the counter's MINIMUM:
/// substitute `counter := u_min` (valid because `E_base` is non-decreasing in
/// the counter, so its minimum over `counter >= u_min` is there) and check the
/// result is a non-negative constant. Mirrors `init_below_bound`, which does
/// the same substitution at the maximum for the upper half.
pub(super) fn sum_nonneg_at_min(
    e_base: &BoundTerm,
    lo_init: &BoundTerm,
    counter: &str,
    u_min: &BoundTerm,
) -> bool {
    let sum = BoundTerm::Bin(
        BoundOp::Add,
        Box::new(e_base.clone()),
        Box::new(lo_init.clone()),
    );
    let (Some(lin), Some(min_lin)) = (bound_to_linear(&sum), bound_to_linear(u_min)) else {
        return false;
    };
    let a = *lin.terms.get(counter).unwrap_or(&0);
    if a < 0 {
        return false;
    }
    let mut without = lin.clone();
    without.terms.remove(counter);
    let Some(scaled) = lin_scale(&min_lin, a) else {
        return false;
    };
    let Some(at_min) = lin_add(&without, &scaled) else {
        return false;
    };
    at_min.terms.is_empty() && at_min.konst >= 0
}

fn is_clean_increment_stmt(s: &Stmt, lo: &str) -> bool {
    match &s.kind {
        StmtKind::Assign { target, value } if is_ident_expr(target, lo) => {
            is_add_pos_const(value, lo)
        }
        StmtKind::CompoundAssign {
            target,
            op: CompoundOp::Add,
            value,
        } if is_ident_expr(target, lo) => is_pos_int_lit(value),
        _ => false,
    }
}

/// `value` is `lo + C` / `C + lo` (C a positive int literal) — surface
/// `Binary(Add)` or trait-lowered `Call { Path([ty,"add"]), .. }`.
fn is_add_pos_const(value: &Expr, lo: &str) -> bool {
    match &value.kind {
        ExprKind::Binary {
            op: BinOp::Add,
            left,
            right,
        } => {
            (is_ident_expr(left, lo) && is_pos_int_lit(right))
                || (is_pos_int_lit(left) && is_ident_expr(right, lo))
        }
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return false;
            };
            segments.len() == 2
                && segments[1] == "add"
                && ((is_ident_expr(&args[0].value, lo) && is_pos_int_lit(&args[1].value))
                    || (is_pos_int_lit(&args[0].value) && is_ident_expr(&args[1].value, lo)))
        }
        _ => false,
    }
}

/// Collect `(vec_var, base_var)` for every `v[base + idx]` / `v[idx + base]` in
/// `body` where `idx` is one of `idx_names` and both `base` and `v` are bare
/// identifiers. Sorted+deduped so the emitted fact set is deterministic.
pub(super) fn collect_base_indexed(body: &Block, idx_names: &[String]) -> Vec<(String, String)> {
    // The predicate always reports `false` so `body_any`'s short-circuit never
    // fires and the walk visits every sub-expression; hits accumulate in the
    // cell instead of the return value.
    let cell: std::cell::RefCell<Vec<(String, String)>> = std::cell::RefCell::new(Vec::new());
    let collect = |e: &Expr| {
        if let ExprKind::Index { object, index } = &e.kind {
            if let ExprKind::Identifier(v) = &object.kind {
                if let Some(base) = sum_with_index_var(index, idx_names) {
                    cell.borrow_mut().push((v.clone(), base));
                }
            }
        }
        false
    };
    body_any(body, &collect);
    let mut out = cell.into_inner();
    out.sort();
    out.dedup();
    out
}

/// `index` is `base + idx` or `idx + base` where `idx` is one of `idx_names`
/// and `base` is a bare identifier → `Some(base)`. Handles the surface
/// `Binary(Add)` and the trait-lowered `Call { Path([ty,"add"]), .. }` forms.
fn sum_with_index_var(index: &Expr, idx_names: &[String]) -> Option<String> {
    let (l, r) = match &index.kind {
        ExprKind::Binary {
            op: BinOp::Add,
            left,
            right,
        } => (left.as_ref(), right.as_ref()),
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 || segments[1] != "add" {
                return None;
            }
            (&args[0].value, &args[1].value)
        }
        _ => return None,
    };
    let (ln, rn) = (ident(l)?, ident(r)?);
    if idx_names.contains(&rn) && !idx_names.contains(&ln) {
        Some(ln)
    } else if idx_names.contains(&ln) && !idx_names.contains(&rn) {
        Some(rn)
    } else {
        None
    }
}

/// Immutable integer-literal `let` bindings of the whole function: `x` bound
/// exactly once to an integer literal and never assigned, shadowed, or
/// mut-written. Substituting these is what makes the `n * len` row-extent
/// products affine (see the section comment).
pub(super) fn int_const_bindings(body: &Block) -> HashMap<String, i64> {
    let whole = region_bindings(&body.stmts, &body.final_expr);
    let mut cand: HashMap<String, i64> = HashMap::new();
    for_each_block(body, &mut |block| {
        for s in &block.stmts {
            if let StmtKind::Let { pattern, value, .. } = &s.kind {
                if let PatternKind::Binding(n) = &pattern.kind {
                    if let ExprKind::Integer(v, _) = &value.kind {
                        cand.insert(n.clone(), *v);
                    }
                }
            }
        }
    });
    cand.retain(|n, _| {
        whole.rebound.get(n) == Some(&1)
            && !whole.assigned.contains(n)
            && !mut_used_anywhere(body, n)
    });
    cand
}

/// Whether `name` is ever passed as a `mut`-marked argument or used as a
/// method-call receiver anywhere in `body`. Deliberately scans statement
/// *values* only: `stmt_writes_bound` reports a `let name = ..` binding as a
/// write to `name`, which is correct for its "unwritten AFTER this point" use
/// but would reject every candidate here (each is introduced by exactly such a
/// binding). Rebinding and assignment are covered separately by
/// `region_bindings`. `expr_writes_bound` recurses through nested blocks, so
/// the top-level scan is exhaustive.
fn mut_used_anywhere(body: &Block, name: &str) -> bool {
    let stmt_hit = |s: &Stmt| match &s.kind {
        StmtKind::Let { value, .. } => expr_writes_bound(value, name),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            expr_writes_bound(target, name) || expr_writes_bound(value, name)
        }
        StmtKind::Expr(e) => expr_writes_bound(e, name),
        StmtKind::LetUninit { .. } => false,
        // Unanalyzed statement kinds — fail closed.
        _ => true,
    };
    body.stmts.iter().any(stmt_hit)
        || body
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_writes_bound(e, name))
}

/// Replace every identifier of `bt` that has an immutable integer-literal
/// binding with that literal. Pure rewriting — the resulting term denotes the
/// same runtime value.
pub(super) fn fold_consts(bt: &BoundTerm, consts: &HashMap<String, i64>) -> BoundTerm {
    match bt {
        BoundTerm::Ident(s) => match consts.get(s) {
            Some(v) => BoundTerm::Int(*v),
            None => bt.clone(),
        },
        BoundTerm::Int(_) => bt.clone(),
        BoundTerm::Bin(op, l, r) => BoundTerm::Bin(
            *op,
            Box::new(fold_consts(l, consts)),
            Box::new(fold_consts(r, consts)),
        ),
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

    // ── Converging two-pointer skip (B-2026-08-04-8) ────────────────

    /// Parse `src` and return the converging skips for the first function as a
    /// sorted `Vec<(base_var, idx_vars, vec_vars)>`.
    fn conv_skips(src: &str) -> Vec<(String, Vec<String>, Vec<String>)> {
        let parsed = crate::parse(src);
        // A mutated source that does not PARSE yields no skips, which would
        // make every negative below a vacuous pass. Fail loudly instead.
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let body = parsed
            .program
            .items
            .into_iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f.body),
                _ => None,
            })
            .expect("a function");
        let mut out: Vec<(String, Vec<String>, Vec<String>)> = compute_converging_skips(&body)
            .into_values()
            .map(|s| (s.base_var, s.idx_vars, s.vec_vars))
            .collect();
        out.sort();
        out
    }

    /// `ROW_MAJOR` with `from` replaced by `to`, asserting the anchor actually
    /// matched. A silently-missed replacement would leave the ORIGINAL (firing)
    /// program behind and turn a negative test into a vacuous pass — which is
    /// exactly how the first draft of these gates reported green.
    fn mutate(from: &str, to: &str) -> String {
        assert!(
            ROW_MAJOR.contains(from),
            "anchor not found in ROW_MAJOR: {from:?}"
        );
        ROW_MAJOR.replace(from, to)
    }

    /// The canonical row-major two-pointer: a `Vec.filled(n * len, _)` corpus
    /// walked from both ends within each row.
    const ROW_MAJOR: &str = "fn f() -> i64 {\n\
            let n = 20i64;\n\
            let len = 32i64;\n\
            let mut v: Vec[u8] = Vec.filled(n * len, 48u8);\n\
            let mut acc = 0i64;\n\
            let mut i = 0i64;\n\
            while i < n {\n\
              let base = i * len;\n\
              let mut lo = 0i64;\n\
              let mut hi = len - 1i64;\n\
              while lo <= hi {\n\
                acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);\n\
                lo = lo + 1i64;\n\
                hi = hi - 1i64;\n\
              }\n\
              i = i + 1i64;\n\
            }\n\
            acc\n\
        }";

    #[test]
    fn fires_on_row_major_two_pointer() {
        assert_eq!(
            conv_skips(ROW_MAJOR),
            vec![(
                "base".to_string(),
                vec!["hi".to_string(), "lo".to_string()],
                vec!["v".to_string()]
            )]
        );
    }

    /// The single skip's `lower_proven` flag, or `None` if nothing fired.
    fn conv_lower(src: &str) -> Option<bool> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let body = parsed
            .program
            .items
            .into_iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f.body),
                _ => None,
            })
            .expect("a function");
        let m = compute_converging_skips(&body);
        assert!(m.len() <= 1, "expected at most one skip, got {}", m.len());
        m.into_values().next().map(|s| s.lower_proven)
    }

    #[test]
    fn proves_the_lower_half_for_the_canonical_shape() {
        // `base = i * len` with `i >= 0` and `lo` starting at 0 puts every
        // index at or above zero, so the check disappears ENTIRELY.
        assert_eq!(conv_lower(ROW_MAJOR), Some(true));
    }

    #[test]
    fn keeps_the_lower_half_when_hi_steps_before_its_use() {
        // `hi = hi - 1` BEFORE the index still bounds `hi` ABOVE (it only
        // shrinks), so the upper half is still proven — but `hi` may now be
        // below `lo_init`, so the sign half must survive. This pins the two
        // halves as independently gated.
        let src = mutate(
            "acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);\nlo = lo + 1i64;\nhi = hi - 1i64;",
            "hi = hi - 1i64;\nacc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);\nlo = lo + 1i64;",
        );
        assert_eq!(conv_lower(&src), Some(false));
    }

    #[test]
    fn keeps_the_lower_half_when_the_row_origin_can_be_negative() {
        // The enclosing counter starts below zero, so `base = i * len` can be
        // negative even though the upper bound still holds.
        let src = mutate("let mut i = 0i64;", "let mut i = -4i64;");
        assert_eq!(conv_lower(&src), Some(false));
    }

    #[test]
    fn fires_when_the_vec_is_passed_to_a_free_function_unmarked() {
        // The #246 shape: the buffer is also handed to a helper that walks it.
        // An unmarked free-function argument cannot be `mut ref` (the marker
        // would be mandatory), so the length is still pinned.
        let src = mutate(
            "acc\n}",
            "acc + peek(v)\n}\nfn peek(c: ref Vec[u8]) -> i64 { return c.len() as i64; }",
        );
        assert_eq!(conv_skips(&src).len(), 1);
    }

    #[test]
    fn no_fire_when_the_vec_is_passed_mut_marked() {
        // `mut v` says the callee takes `mut ref Vec` and may resize it.
        let src = mutate(
            "acc\n}",
            "acc + grow(mut v)\n}\nfn grow(c: mut ref Vec[u8]) -> i64 { c.push(1u8); return 0i64; }",
        );
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn no_fire_when_the_vec_is_a_method_argument() {
        // Method calls never write the `mut` marker, so an unmarked method
        // argument proves nothing about the parameter's mode.
        let src = mutate("acc\n}", "acc + sink.absorb(v)\n}");
        let src = src.replace(
            "let mut acc = 0i64;",
            "let mut acc = 0i64;\nlet mut sink: Vec[u8] = Vec.new();",
        );
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn fires_with_strict_converging_guard() {
        // `while lo < hi` bounds `lo` by `hi` just as `<=` does.
        let src = mutate("while lo <= hi", "while lo < hi");
        assert_eq!(conv_skips(&src).len(), 1);
    }

    #[test]
    fn fires_for_index_assign_targets() {
        // The construction half of the shape: both ends are ASSIGNED, not read.
        let src = mutate(
            "acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);",
            "v[base + lo] = 48u8; v[base + hi] = 48u8;",
        );
        assert_eq!(conv_skips(&src).len(), 1);
    }

    // ── Negatives: each soundness gate, one at a time ────────────────

    #[test]
    fn no_fire_when_ascending_index_steps_before_its_use() {
        // `lo = lo + 1` BEFORE the index: at the use, `lo` may already exceed
        // the `lo <= hi <= hi_init` bound the guard established. Skipping the
        // check here would be an OOB read.
        let src = mutate(
            "acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);\nlo = lo + 1i64;",
            "lo = lo + 1i64;\nacc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);",
        );
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn no_fire_when_hi_is_not_monotone() {
        // `hi` reset upward inside the body breaks `hi <= hi_init`.
        let src = mutate("hi = hi - 1i64;", "hi = hi + 1i64;");
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn no_fire_when_base_is_written_in_the_inner_body() {
        let src = mutate("lo = lo + 1i64;", "base = base + 1i64; lo = lo + 1i64;");
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn no_fire_when_the_row_does_not_fit_the_pinned_length() {
        // `base = i * len` but rows are `len + 1` wide: the last row runs off
        // the end, and the linear identity must refuse it.
        let src = mutate("let mut hi = len - 1i64;", "let mut hi = len;");
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn no_fire_when_the_enclosing_counter_is_written_before_the_inner_loop() {
        let src = mutate("let base = i * len;", "i = i + 1i64; let base = i * len;");
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn no_fire_when_the_vec_length_is_not_pinned() {
        // A bare `Vec.new()` with no counted fill: nothing bounds `v.len()`.
        let src = mutate("Vec.filled(n * len, 48u8)", "Vec.new()");
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn no_fire_when_the_vec_is_resized_after_the_pin() {
        let src = mutate("let mut acc = 0i64;", "let mut acc = 0i64; v.push(1u8);");
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn no_fire_when_a_bound_identifier_is_mutated_after_the_pin() {
        // `len` written after the pin: `n * len` no longer describes v.len().
        let src = mutate("let len = 32i64;", "let mut len = 32i64;")
            .replace("let mut acc = 0i64;", "let mut acc = 0i64; len = 64i64;");
        assert!(conv_skips(&src).is_empty());
    }

    #[test]
    fn no_fire_for_a_plain_single_index_loop() {
        // No sum index: the existing bare-index paths own this shape.
        let src = mutate("v[base + lo]", "v[lo]").replace("v[base + hi]", "v[hi]");
        assert!(conv_skips(&src).is_empty());
    }

    // ── Descending-loop bounds-check skip (B-2026-07-17-1) ──────────

    /// Parse `src` and return the descending skips for the first function as a
    /// sorted `Vec<(idx_var, vec_vars)>`.
    fn desc_skips(src: &str) -> Vec<(String, Vec<String>)> {
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
        let mut out: Vec<(String, Vec<String>)> = compute_descending_skips(&body)
            .into_values()
            .map(|s| (s.idx_var, s.vec_vars))
            .collect();
        out.sort();
        out
    }

    /// The canonical LeetCode #119 in-place rolling-row shape: inclusive fill
    /// `while j <= n`, enclosing `while i <= n`, inner descending `while k >= 1`
    /// updating `row[k] = row[k] + row[k-1]`, returning `row`.
    #[test]
    fn desc_fires_on_pascal_row_shape() {
        let src = "fn get_row(n: i64) -> Vec[i64] {\n\
            let mut row: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j <= n { row.push(1i64); j = j + 1i64; }\n\
            let mut i = 2i64;\n\
            while i <= n {\n\
              let mut k = i - 1i64;\n\
              while k >= 1i64 { row[k] = row[k] + row[k - 1i64]; k = k - 1i64; }\n\
              i = i + 1i64;\n\
            }\n\
            row\n\
        }\n";
        assert_eq!(
            desc_skips(src),
            vec![("k".to_string(), vec!["row".to_string()])]
        );
    }

    /// Exclusive enclosing bound + exclusive fill: `while j < n` (len == n),
    /// `for i in 1..n` (i <= n-1), `k = i` init, `while k >= 1`. Proof:
    /// `k <= i <= n-1 < n == len`.
    #[test]
    fn desc_fires_on_exclusive_bounds() {
        let src = "fn f(n: i64) -> Vec[i64] {\n\
            let mut v: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < n { v.push(1i64); j = j + 1i64; }\n\
            for i in 1i64..n {\n\
              let mut k = i;\n\
              while k >= 1i64 { v[k] = v[k] + v[k - 1i64]; k = k - 1i64; }\n\
            }\n\
            v\n\
        }\n";
        assert_eq!(
            desc_skips(src),
            vec![("k".to_string(), vec!["v".to_string()])]
        );
    }

    /// Negative: init `k = i + 1` can reach `len` (`i <= n-1` ⇒ `k <= n == len`,
    /// NOT `< len`), so the proof must FAIL closed.
    #[test]
    fn desc_no_fire_when_init_reaches_len() {
        let src = "fn f(n: i64) -> Vec[i64] {\n\
            let mut v: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j < n { v.push(1i64); j = j + 1i64; }\n\
            for i in 1i64..n {\n\
              let mut k = i + 1i64;\n\
              while k >= 1i64 { v[k] = v[k] + v[k - 1i64]; k = k - 1i64; }\n\
            }\n\
            v\n\
        }\n";
        assert!(desc_skips(src).is_empty());
    }

    /// Negative: the inner loop ASCENDS (`k = k + 1`) — not monotone
    /// non-increasing, so `k <= k_init` does not hold; must not fire.
    #[test]
    fn desc_no_fire_on_ascending_inner() {
        let src = "fn f(n: i64) -> Vec[i64] {\n\
            let mut v: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j <= n { v.push(1i64); j = j + 1i64; }\n\
            let mut i = 2i64;\n\
            while i <= n {\n\
              let mut k = 1i64;\n\
              while k >= 1i64 { v[k] = v[k] + 1i64; k = k + 1i64; }\n\
              i = i + 1i64;\n\
            }\n\
            v\n\
        }\n";
        assert!(desc_skips(src).is_empty());
    }

    /// Negative: no length pin (the Vec is grown INSIDE the enclosing loop, so
    /// its length is not fixed by a counted fill) — nothing to bound against.
    #[test]
    fn desc_no_fire_without_pin() {
        let src = "fn f(n: i64) -> Vec[i64] {\n\
            let mut v: Vec[i64] = Vec.new();\n\
            let mut i = 2i64;\n\
            while i <= n {\n\
              v.push(0i64);\n\
              let mut k = i - 1i64;\n\
              while k >= 1i64 { v[k] = v[k] + v[k - 1i64]; k = k - 1i64; }\n\
              i = i + 1i64;\n\
            }\n\
            v\n\
        }\n";
        assert!(desc_skips(src).is_empty());
    }

    /// Negative: the enclosing counter is REWRITTEN before the inner loop, so
    /// `i <= u_max` no longer holds at the `k` init; must fail closed.
    #[test]
    fn desc_no_fire_when_counter_rewritten() {
        let src = "fn f(n: i64) -> Vec[i64] {\n\
            let mut v: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j <= n { v.push(1i64); j = j + 1i64; }\n\
            let mut i = 2i64;\n\
            while i <= n {\n\
              i = i + 5i64;\n\
              let mut k = i - 1i64;\n\
              while k >= 1i64 { v[k] = v[k] + v[k - 1i64]; k = k - 1i64; }\n\
            }\n\
            v\n\
        }\n";
        assert!(desc_skips(src).is_empty());
    }

    /// Negative: `k` is mutated by a non-decrement write inside the inner loop
    /// (a nested reset), so monotonicity is not provable; must not fire.
    #[test]
    fn desc_no_fire_on_nonmonotone_reset() {
        let src = "fn f(n: i64) -> Vec[i64] {\n\
            let mut v: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            while j <= n { v.push(1i64); j = j + 1i64; }\n\
            let mut i = 2i64;\n\
            while i <= n {\n\
              let mut k = i - 1i64;\n\
              while k >= 1i64 { v[k] = v[k] + v[k - 1i64]; if k < 0i64 { k = i; } k = k - 1i64; }\n\
              i = i + 1i64;\n\
            }\n\
            v\n\
        }\n";
        assert!(desc_skips(src).is_empty());
    }

    // ── Nested writes to the values the proof freezes (B-2026-08-04-13) ──
    //
    // Each fact the descending skip rests on is read off a region scan:
    // the fill counter starts at 0, the fill bound is invariant, the
    // enclosing counter still satisfies its guard, the index's init is the
    // one written before the loop. `stmt_writes_ident` answered all four and
    // saw only TOP-LEVEL assignment targets, so burying the write one block
    // deep (`if c { i = 50 }`) left every fact reading "unchanged" while the
    // program changed it. The skip then elided a load-bearing check and the
    // store ran past the end of the buffer — measured as glibc heap
    // corruption, not a clean panic. The three cases below are the same
    // program with the write at the three different levers.

    /// The shared base for the nested-write cases. Each slot is empty in the
    /// control and holds one nested write in a negative, so those tests differ
    /// from a FIRING program only in the nesting — not in some extra statement
    /// that trips an unrelated gate.
    fn nested_write_case(pre_fill: &str, enc_head: &str, post_init: &str) -> String {
        format!(
            "fn f(n: i64) -> Vec[i64] {{\n\
             let mut v: Vec[i64] = Vec.new();\n\
             let mut j = 0i64;\n\
             let flag = 1i64;\n\
             {pre_fill}\
             while j <= n {{ v.push(1i64); j = j + 1i64; }}\n\
             let mut i = 2i64;\n\
             while i <= n {{\n\
               {enc_head}\
               let mut k = i - 1i64;\n\
               {post_init}\
               while k >= 1i64 {{ v[k] = v[k] + v[k - 1i64]; k = k - 1i64; }}\n\
               i = i + 1i64;\n\
             }}\n\
             v\n\
            }}\n"
        )
    }

    /// Control: with every slot empty the skip DOES fire, so the three
    /// negatives below are pinned to the nested write itself.
    #[test]
    fn desc_fires_on_the_base_of_the_nested_write_cases() {
        assert_eq!(
            desc_skips(&nested_write_case("", "", "")),
            vec![("k".to_string(), vec!["v".to_string()])]
        );
    }

    /// Negative: the ENCLOSING COUNTER is rewritten in a nested block before
    /// the inner loop, so `i <= u_max` is false where the proof substitutes
    /// it. The top-level twin of this is `desc_no_fire_when_counter_rewritten`,
    /// which passed all along — only the nested spelling escaped.
    #[test]
    fn desc_no_fire_when_counter_rewritten_in_a_nested_block() {
        let src = nested_write_case("", "if flag == 1i64 { i = 50i64; }\n", "");
        assert!(desc_skips(&src).is_empty());
    }

    /// Negative: the INDEX'S INIT is rewritten in a nested block between its
    /// `let` and the inner loop, so `sole_scalar_init` reports a value `k` no
    /// longer holds and `k <= k_init` bounds the wrong number.
    #[test]
    fn desc_no_fire_when_index_init_rewritten_in_a_nested_block() {
        let src = nested_write_case("", "", "if flag == 1i64 { k = 50i64; }\n");
        assert!(desc_skips(&src).is_empty());
    }

    /// Negative: the FILL COUNTER is preset in a nested block, so the fill runs
    /// fewer iterations than its bound and the length pin overstates
    /// `v.len()`. This one corrupts through the pin rather than the index: the
    /// skip's arithmetic is right, its premise is not.
    #[test]
    fn desc_no_fire_when_fill_counter_preset_in_a_nested_block() {
        let src = nested_write_case("if flag == 1i64 { j = 5i64; }\n", "", "");
        assert!(desc_skips(&src).is_empty());
    }

    /// The same fill-counter preset seen through the ASCENDING pin path, which
    /// shares `counter_is_zero_before` — the gate is one function, so both
    /// consumers must fail closed on it.
    #[test]
    fn no_fire_when_fill_counter_preset_in_a_nested_block() {
        let with_preset = "fn f(cols: i64) {\n\
            let mut dp: Vec[i64] = Vec.new();\n\
            let mut j = 0i64;\n\
            let flag = 1i64;\n\
            if flag == 1i64 { j = 5i64; }\n\
            while j < cols { dp.push(1i64); j = j + 1i64; }\n\
            let mut c = 0i64;\n\
            while c < cols { dp[c] = dp[c] + 1i64; c = c + 1i64; }\n\
        }\n";
        assert!(!pins_vec(with_preset, "dp"));
        // Control: the identical program without the nested preset pins, so
        // the assertion above is about the preset and not the added `flag`.
        let without_preset = with_preset.replace("if flag == 1i64 { j = 5i64; }\n", "");
        assert!(pins_vec(&without_preset, "dp"));
    }
}
