//! Loop-bound collection pre-sizing (phase-7-codegen.md § 7.3 lever #1).
//!
//! Rewrites `let mut v = Vec.new()` / `let mut s = ""` to a `with_capacity`
//! call when `v`/`s` is *immediately* filled by a simple counted loop whose
//! trip count is a known, in-scope expression. This turns an O(log n) chain of
//! grow-reallocs (cap 0 → 4 → 8 → … doubling, each a malloc+copy) into a single
//! up-front allocation. Measured 1.21× on the #43 multiply-strings bench.
//!
//! **Correctness margin.** `with_capacity` is only a capacity *hint* — the
//! collection still grows if the count is exceeded — so an inexact estimate can
//! never change program output, only memory use. To keep even that bounded, the
//! rewrite fires *only* under a conservative pattern (below) that guarantees the
//! reserved capacity is within a small factor of the real fill, and bails
//! (leaving `new()` untouched) on anything it cannot fully prove. It runs inside
//! `lowering` **before** operator lowering, so loop conditions / increments are
//! still raw `Binary` nodes — see [`crate::lowering::Lowerer::lower_block`].
//!
//! **Effect/ownership safety.** The rewrite only fires when the loop body
//! contains a `push`/`push_str` to the binding — which already allocates — so
//! swapping `new()` (no alloc) for `with_capacity` (allocates) introduces no
//! effect category the enclosing function did not already carry. Ownership is
//! unchanged: both yield a fresh owned collection.
//!
//! Firing pattern (ALL must hold):
//!   1. `let mut V = Vec.new()` / `String.new()` / `""` — fresh mutable binding.
//!   2. A following `while IV < BOUND` / `while IV <= BOUND` / `for I in LO..BOUND`
//!      loop whose body pushes to `V` exactly once, unconditionally, with no
//!      break/continue/return (so push-count == trip-count). The `<=` form runs
//!      one extra iteration, so it reserves `BOUND + 1`.
//!   3. Between the Let and the loop, `V` may only be *seed-pushed* (e.g.
//!      `fact.push(1)` / DP base cases); each such push adds 1 to the
//!      reservation. Any other mention there (move, reassign, index-write)
//!      bails. Inside the loop body `V` may be *read* (e.g. `V[i - 1]` /
//!      `V.len()` in a cumulative fill) but is pushed to exactly once — reads
//!      don't change the length, so the trip-count reservation stays exact.
//!   4. `BOUND` is fully analyzable, loop-invariant (none of its identifiers is
//!      assigned or method-called in the body), does not reference `IV` or `V`,
//!      and every identifier it names is in scope at the Let (i.e. not bound by
//!      a statement between the Let and the loop).
//!   5. `BOUND` is not a large integer literal (guards a pathological
//!      `while i < 1_000_000_000` from reserving gigabytes speculatively).

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::{IntSize, Type, TypeCheckResult, UIntSize};

/// Upper bound on a constant-literal capacity hint. A variable bound (`m + n`,
/// `xs.len()`) is sized by real program data and always allowed; only a *huge
/// constant* literal is refused, so a `while i < 2_000_000_000 { v.push(..) }`
/// can't be turned into a multi-GiB speculative reservation.
const MAX_LITERAL_BOUND: i64 = 1 << 20;

/// Collection kind a pre-sizable empty initializer denotes.
#[derive(Clone, Copy)]
enum Coll {
    Vec,
    String,
}

impl Coll {
    fn type_name(self) -> &'static str {
        match self {
            Coll::Vec => "Vec",
            Coll::String => "String",
        }
    }
}

/// Pre-size pre-sizable collections in one block's statement sequence. Does not
/// recurse — the lowering walk invokes this for every block, before that block's
/// operators are lowered.
pub fn presize_block(block: &mut Block, tc: &TypeCheckResult) {
    // Strongest rewrite first: a `Vec.new()` + EXACT counted `push(literal)` fill
    // collapses to a single `Vec.filled(BOUND, literal)` — the `vec![x; n]` shape
    // (one sized allocation + a straight fill, no per-element grow check). Removes
    // the fill loop, so the `with_capacity` pass below then sees a `Vec.filled`
    // binding it does not touch. Vecs that don't meet the exact-fill/literal gates
    // fall through to the capacity-hint rewrite (removes the realloc chain but
    // keeps the grow-checked push loop). See `fill_to_filled`. (B-2026-07-08-7.)
    fill_to_filled(block, tc);

    let n = block.stmts.len();
    for i in 0..n {
        let (v_name, coll) = match presizable_let(&block.stmts[i]) {
            Some(x) => x,
            None => continue,
        };
        if let Some(bound) = find_fill_bound(&block.stmts, i, &v_name) {
            rewrite_to_with_capacity(&mut block.stmts[i], coll, bound);
        }
    }
}

/// A recognised `Vec.new()` + exact-counted-`push(literal)` fill that can collapse
/// to `Vec.filled`. `let_idx`/`loop_idx` are statement indices in the enclosing
/// block; `bound`/`val` are the (cloned) fill count and element literal;
/// `counter` is `Some(IV)` for the `while` form (whose external counter must be
/// pinned to its post-loop value) and `None` for the `for` form.
struct FillPlan {
    let_idx: usize,
    loop_idx: usize,
    bound: Expr,
    val: Expr,
    counter: Option<String>,
}

/// Rewrite `let mut V = Vec.new(); <exact counted push loop>` to
/// `let mut V = Vec.filled(BOUND, LIT)`, deleting the loop. This is the `vec![x;
/// n]` lowering. Unlike the `with_capacity` pre-size (which removes the realloc
/// chain but leaves a per-element `len == cap` grow check the optimizer cannot
/// prove dead — the self-referential capacity phi defeats SCEV, B-2026-07-08-7),
/// `Vec.filled` lowers to one sized allocation + a straight fill, no grow check.
///
/// Fires only under gates that make the collapse output-preserving AND
/// memory-safe:
///   * count is EXACT — `for I in 0..BOUND` (start absent/0), or `while IV <
///     BOUND` with `IV` proven literal-`0` at entry and stepped by exactly one
///     `+ 1` — so the loop yields exactly `BOUND` elements, matching
///     `Vec.filled(BOUND, _)`.
///   * exactly ONE unconditional `V.push(LIT)`, no early exit, and no other body
///     statement beyond the counter step — nothing whose removal drops a
///     side effect or read.
///   * `LIT` is an integer LITERAL whose checked type is 64-bit (`i64` / `u64` /
///     `usize`). `Vec.filled` sizes its buffer from the *compiled value* type,
///     and a bare integer literal compiles to `i64`; gating on a 64-bit element
///     keeps buffer stride == element stride. A narrow `Vec[i32]` fill would
///     mis-size, so it stays on the `with_capacity` path.
///   * `V` is untouched between its `Vec.new()` and the loop (no seed pushes), so
///     the loop's trip count is the whole length.
///   * `BOUND` is pure arithmetic over in-scope, loop-invariant identifiers that
///     name neither `V` nor the counter, and unchanged between the `let` and the
///     loop (the `filled` call evaluates it at the `let` position).
///
/// The `while` form's external counter `IV` is pinned to its exact post-loop
/// value by REPLACING the loop with `IV = BOUND` (from `IV = 0` stepping `+1`
/// until `IV >= BOUND`, `IV` ends at `BOUND`) — so any later read of `IV`
/// observes the same value it would have after the real loop, with no need to
/// prove `IV` dead. The `for` form's counter is loop-scoped, so its loop is
/// simply deleted. The fixup is a dead store (DCE'd) when `IV` is unused.
///
/// Effect/ownership safety: the loop's only effect is `allocates` (the `push`),
/// which `Vec.filled` reproduces; the counter arithmetic (`+`) carries no effect
/// (`panics` comes from indexing/division/unwrap, not `+`). Both forms yield a
/// fresh owned `Vec`. So the rewrite is effect- and ownership-neutral — it runs
/// before `effectcheck`/`ownershipcheck` (see `cli.rs` pipeline order).
fn fill_to_filled(block: &mut Block, tc: &TypeCheckResult) {
    let mut plans: Vec<FillPlan> = Vec::new();
    for i in 0..block.stmts.len() {
        let Some(v) = fresh_empty_vec_new(&block.stmts[i]) else {
            continue;
        };
        if let Some(plan) = plan_filled_rewrite(&block.stmts, i, &v, tc) {
            plans.push(plan);
        }
    }
    if plans.is_empty() {
        return;
    }
    // Two in-place passes then a deletion pass — none of which shifts an index
    // the next relies on. `let_idx < loop_idx` always, and distinct plans target
    // distinct Vecs (distinct loops), so the passes don't interfere.
    //   1. rewrite each `let` init to `Vec.filled` (in place),
    //   2. for `while` plans, replace the loop with `IV = BOUND` (in place),
    //   3. delete `for` plans' loops high-index-first (they have no counter fixup).
    let mut for_loop_idxs: Vec<usize> = Vec::new();
    for p in &plans {
        rewrite_let_to_filled(&mut block.stmts[p.let_idx], p.bound.clone(), p.val.clone());
        match &p.counter {
            Some(iv) => {
                replace_loop_with_counter_fixup(&mut block.stmts[p.loop_idx], iv, p.bound.clone())
            }
            None => for_loop_idxs.push(p.loop_idx),
        }
    }
    for_loop_idxs.sort_unstable();
    for idx in for_loop_idxs.into_iter().rev() {
        block.stmts.remove(idx);
    }
}

/// `let mut V = Vec.new()` (fresh, mutable, no args) → `Some(V)`.
fn fresh_empty_vec_new(stmt: &Stmt) -> Option<String> {
    let StmtKind::Let {
        is_mut: true,
        pattern,
        value,
        ..
    } = &stmt.kind
    else {
        return None;
    };
    let PatternKind::Binding(name) = &pattern.kind else {
        return None;
    };
    let ExprKind::Call { callee, args } = &value.kind else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return None;
    };
    if segments.len() == 2 && segments[0] == "Vec" && segments[1] == "new" {
        Some(name.clone())
    } else {
        None
    }
}

/// Find `v`'s exact counted-`push(literal)` fill loop after `let_idx` and, if all
/// gates hold, return the plan to collapse it to `Vec.filled`. `v` must not be
/// mentioned between its `let` and the loop (no seed pushes); the loop must be the
/// first statement touching `v`.
fn plan_filled_rewrite(
    stmts: &[Stmt],
    let_idx: usize,
    v: &str,
    tc: &TypeCheckResult,
) -> Option<FillPlan> {
    // Names bound between the `let` and the loop — the BOUND must not reference
    // any (they aren't in scope at the `let`, so `Vec.filled(BOUND, _)` placed at
    // the `let` couldn't see them).
    // The `Vec.new()` initializer expression — its resolved type carries `v`'s
    // element type, the stride-safety oracle for the `Vec.filled` rewrite.
    let StmtKind::Let { value: vecnew, .. } = &stmts[let_idx].kind else {
        return None;
    };
    let mut bound_between: Vec<String> = Vec::new();
    for (off, stmt) in stmts[let_idx + 1..].iter().enumerate() {
        let loop_idx = let_idx + 1 + off;
        // The fill loop is the first statement that mentions `v`.
        if let StmtKind::Expr(e) = &stmt.kind {
            if let ExprKind::While {
                condition, body, ..
            } = &e.kind
            {
                let (iv, bound) = as_strict_lt(condition)?;
                if iv == v {
                    return None;
                }
                let val = while_fill_value(body, v, &iv)?;
                if !counter_zero_before(stmts, loop_idx, &iv) {
                    return None;
                }
                let pre_loop = &stmts[let_idx + 1..loop_idx];
                return finish_plan(
                    let_idx,
                    loop_idx,
                    bound,
                    val,
                    v,
                    Some(&iv),
                    &bound_between,
                    pre_loop,
                    vecnew,
                    tc,
                );
            }
            if let ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } = &e.kind
            {
                let bound = for_zero_range_end(pattern, iterable, v)?;
                let val = for_fill_value(body, v)?;
                let pre_loop = &stmts[let_idx + 1..loop_idx];
                return finish_plan(
                    let_idx,
                    loop_idx,
                    bound,
                    val,
                    v,
                    None,
                    &bound_between,
                    pre_loop,
                    vecnew,
                    tc,
                );
            }
        }
        // Not the fill loop. If it touches `v` at all (a seed push, a move, …) we
        // can't account for `v`'s length — bail. Otherwise record any names it
        // binds and keep scanning.
        if stmt_mentions_ident(stmt, v) {
            return None;
        }
        collect_let_bound_names(stmt, &mut bound_between);
    }
    None
}

/// Shared final validation for both loop forms: `BOUND` purity/scope/invariance
/// and the element-literal width gate. Returns the plan on success.
#[allow(clippy::too_many_arguments)]
fn finish_plan(
    let_idx: usize,
    loop_idx: usize,
    bound: &Expr,
    val: &Expr,
    v: &str,
    iv: Option<&str>,
    bound_between: &[String],
    pre_loop: &[Stmt],
    vecnew: &Expr,
    tc: &TypeCheckResult,
) -> Option<FillPlan> {
    // Stride safety: the fill value must be an integer LITERAL (it compiles to
    // `i64`) AND `v`'s element type must be 64-bit (`i64` / `u64` / `usize`), so
    // `Vec.filled`'s buffer stride (sized from the compiled value) equals the
    // element stride. A narrow `Vec[i32]` fill would mis-size; it stays on the
    // `with_capacity` path. The element type is read from the resolved type of the
    // `Vec.new()` initializer — the literal's OWN checked type is unreliable here
    // (a bare `0` records as `i64` even when coerced to a narrower element).
    if !matches!(&val.kind, ExprKind::Integer(..)) {
        return None;
    }
    if !vec_elem_is_64bit_int(vecnew, tc) {
        return None;
    }
    // BOUND: fully analyzable, small-or-variable, in scope at the `let`, naming
    // neither `v` nor the counter. Loop-body invariance is guaranteed
    // structurally (the body is exactly the push + `+1` step, touching only `v`
    // and the counter — both excluded from BOUND here).
    let mut idents = Vec::new();
    if !collect_idents(bound, &mut idents) {
        return None;
    }
    if idents.iter().any(|n| n == v || Some(n.as_str()) == iv) {
        return None;
    }
    if let ExprKind::Integer(k, _) = &bound.kind {
        if *k < 0 || *k > MAX_LITERAL_BOUND {
            return None;
        }
    }
    if idents.iter().any(|n| bound_between.contains(n)) {
        return None;
    }
    // BOUND must also be invariant across the statements BETWEEN the `let` and the
    // loop: the rewritten `Vec.filled(BOUND, _)` sits at the `let` position and
    // evaluates BOUND there, whereas the loop evaluated it later. If any of those
    // statements writes a BOUND identifier (`xs.push(..)` before a `while j <
    // xs.len()` fill), the two evaluations differ → miscount. (`with_capacity`
    // tolerates this — capacity is a hint — but an exact `filled` cannot.)
    if idents
        .iter()
        .any(|n| pre_loop.iter().any(|s| stmt_mutates_ident(s, n)))
    {
        return None;
    }
    Some(FillPlan {
        let_idx,
        loop_idx,
        bound: bound.clone(),
        val: val.clone(),
        counter: iv.map(str::to_string),
    })
}

/// `while IV < BOUND` body that is EXACTLY one `v.push(LIT)` plus one `IV = IV +
/// 1` / `IV += 1` step (in either order), no early exit, no other statement, no
/// tail expr → `Some(&LIT)`. This straight-line, two-statement shape guarantees
/// the push is unconditional and the trip count is `BOUND` (with the caller's
/// zero-start proof), and that removing the loop drops nothing else.
fn while_fill_value<'a>(body: &'a Block, v: &str, iv: &str) -> Option<&'a Expr> {
    if body.final_expr.is_some() || body.stmts.len() != 2 {
        return None;
    }
    let mut push_val: Option<&Expr> = None;
    let mut steps = 0;
    for s in &body.stmts {
        if let StmtKind::Expr(e) = &s.kind {
            if let Some(val) = push_value(e, v) {
                if push_val.is_some() {
                    return None;
                }
                push_val = Some(val);
                continue;
            }
        }
        if is_plus_one_step(s, iv) {
            steps += 1;
            continue;
        }
        return None;
    }
    if steps != 1 {
        return None;
    }
    push_val
}

/// `for I in 0..BOUND` body that is EXACTLY one `v.push(LIT)`, no early exit, no
/// other statement, no tail expr → `Some(&LIT)`. The range owns the counter, so
/// there is nothing else to validate.
fn for_fill_value<'a>(body: &'a Block, v: &str) -> Option<&'a Expr> {
    if body.final_expr.is_some() || body.stmts.len() != 1 {
        return None;
    }
    let StmtKind::Expr(e) = &body.stmts[0].kind else {
        return None;
    };
    push_value(e, v)
}

/// `v.push(VAL)` / `v.push_back(VAL)` with a single arg → `Some(&VAL)`.
fn push_value<'a>(e: &'a Expr, v: &str) -> Option<&'a Expr> {
    let ExprKind::MethodCall {
        object,
        method,
        args,
        ..
    } = &e.kind
    else {
        return None;
    };
    if !matches!(&object.kind, ExprKind::Identifier(n) if n == v) {
        return None;
    }
    if !matches!(method.as_str(), "push" | "push_back") || args.len() != 1 {
        return None;
    }
    Some(&args[0].value)
}

/// `IV = IV + 1` / `IV = 1 + IV` (Assign) or `IV += 1` (CompoundAssign).
fn is_plus_one_step(stmt: &Stmt, iv: &str) -> bool {
    match &stmt.kind {
        StmtKind::Assign { target, value } => {
            if !matches!(&target.kind, ExprKind::Identifier(n) if n == iv) {
                return false;
            }
            let ExprKind::Binary {
                op: BinOp::Add,
                left,
                right,
            } = &value.kind
            else {
                return false;
            };
            let is_iv = |e: &Expr| matches!(&e.kind, ExprKind::Identifier(n) if n == iv);
            let is_one = |e: &Expr| matches!(&e.kind, ExprKind::Integer(1, _));
            (is_iv(left) && is_one(right)) || (is_one(left) && is_iv(right))
        }
        StmtKind::CompoundAssign { target, op, value } => {
            matches!(&target.kind, ExprKind::Identifier(n) if n == iv)
                && matches!(op, CompoundOp::Add)
                && matches!(&value.kind, ExprKind::Integer(1, _))
        }
        _ => false,
    }
}

/// `while IV < BOUND` header → `(IV, &BOUND)` for a strict `<` with a bare-ident
/// left operand. Runs pre-operator-lowering, so the condition is a raw `Binary`.
fn as_strict_lt(cond: &Expr) -> Option<(String, &Expr)> {
    let ExprKind::Binary {
        op: BinOp::Lt,
        left,
        right,
    } = &cond.kind
    else {
        return None;
    };
    let ExprKind::Identifier(iv) = &left.kind else {
        return None;
    };
    Some((iv.clone(), right))
}

/// `for I in 0..BOUND` (exclusive, start absent or literal `0`, `I != v`) →
/// `Some(&BOUND)`.
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
    let start_zero = matches!(
        start.as_deref().map(|e| &e.kind),
        None | Some(ExprKind::Integer(0, _))
    );
    if !start_zero {
        return None;
    }
    end.as_deref()
}

/// `IV` is bound `let mut IV = 0` before `loop_idx` and never assigned/compound-
/// assigned before the loop, so it is provably `0` at loop entry (exact trip
/// count `BOUND`). Fails closed if the init isn't a literal `0` or a pre-loop
/// write is seen.
fn counter_zero_before(stmts: &[Stmt], loop_idx: usize, iv: &str) -> bool {
    let mut seen_zero_init = false;
    for stmt in &stmts[..loop_idx] {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                if matches!(&pattern.kind, PatternKind::Binding(n) if n == iv) {
                    if matches!(&value.kind, ExprKind::Integer(0, _)) {
                        seen_zero_init = true;
                    } else {
                        return false;
                    }
                }
            }
            StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
                if matches!(&target.kind, ExprKind::Identifier(n) if n == iv) {
                    return false;
                }
            }
            _ => {}
        }
    }
    seen_zero_init
}

/// The `Vec.new()` initializer `vecnew` has a resolved element type that is a
/// 64-bit integer (`i64` / `u64` / `usize`) — the widths a bare integer literal
/// compiles to, keeping `Vec.filled`'s buffer stride equal to the element stride.
/// Reads `v`'s element from the initializer's checked type (`Vec[T]` →
/// `Type::Named { name: "Vec", args: [T] }`). Fails closed when the type is
/// unrecorded or not a concrete 64-bit-element `Vec` (e.g. an un-annotated
/// binding whose element is still a type variable at the initializer, or a narrow
/// element) — those keep the `with_capacity` path.
fn vec_elem_is_64bit_int(vecnew: &Expr, tc: &TypeCheckResult) -> bool {
    let Some(Type::Named { name, args }) = tc.expr_types.get(&SpanKey::from_span(&vecnew.span))
    else {
        return false;
    };
    if (name != "Vec" && name != "VecDeque") || args.len() != 1 {
        return false;
    }
    matches!(
        args[0],
        Type::Int(IntSize::I64) | Type::UInt(UIntSize::U64 | UIntSize::Usize)
    )
}

/// Rewrite `let mut V = Vec.new()` to `let mut V = Vec.filled(bound, val)`. The
/// binding's type annotation (if any) is preserved, so the element type is
/// unchanged.
fn rewrite_let_to_filled(stmt: &mut Stmt, bound: Expr, val: Expr) {
    let StmtKind::Let { value, .. } = &mut stmt.kind else {
        return;
    };
    let span = value.span.clone();
    let callee = Expr {
        kind: ExprKind::Path {
            segments: vec!["Vec".to_string(), "filled".to_string()],
            generic_args: None,
        },
        span: span.clone(),
    };
    let mk_arg = |value: Expr, span: Span| CallArg {
        label: None,
        mut_marker: false,
        value,
        span,
    };
    *value = Expr {
        kind: ExprKind::Call {
            callee: Box::new(callee),
            args: vec![mk_arg(bound, span.clone()), mk_arg(val, span.clone())],
        },
        span,
    };
}

/// Replace the `while IV < BOUND { … }` fill-loop statement with `IV = BOUND` —
/// the counter's exact value after the real loop (`0`, stepping `+1`, exits at
/// `IV >= BOUND`, so `IV == BOUND`). Keeps any later read of `IV` correct without
/// a liveness proof; DCE removes the store when `IV` is unused.
fn replace_loop_with_counter_fixup(stmt: &mut Stmt, iv: &str, bound: Expr) {
    let span = bound.span.clone();
    stmt.kind = StmtKind::Assign {
        target: Expr {
            kind: ExprKind::Identifier(iv.to_string()),
            span: span.clone(),
        },
        value: bound,
    };
}

/// `let mut V = <empty Vec/String>` → `Some((V, kind))`.
fn presizable_let(stmt: &Stmt) -> Option<(String, Coll)> {
    let StmtKind::Let {
        is_mut: true,
        pattern,
        value,
        ..
    } = &stmt.kind
    else {
        return None;
    };
    let PatternKind::Binding(name) = &pattern.kind else {
        return None;
    };
    let coll = match &value.kind {
        ExprKind::StringLit(s) if s.is_empty() => Coll::String,
        ExprKind::Call { callee, args } if args.is_empty() => match &callee.kind {
            ExprKind::Path { segments, .. } if segments.len() == 2 && segments[1] == "new" => {
                match segments[0].as_str() {
                    "Vec" => Coll::Vec,
                    "String" => Coll::String,
                    _ => return None,
                }
            }
            _ => return None,
        },
        _ => return None,
    };
    Some((name.clone(), coll))
}

/// Outcome of inspecting one loop while searching for `v`'s fill loop.
enum LoopVerdict {
    /// The loop doesn't touch `v` — keep scanning later statements.
    Skip,
    /// The loop touches `v` but not as a clean counted fill — give up on `v`.
    Bail,
    /// The loop is a clean counted fill; pre-size `v` to this bound. Boxed to
    /// keep the verdict enum small (the other variants are unit).
    Presize(Box<Expr>),
}

/// Find a counted-fill loop for `v` after `let_idx`, returning its trip-count
/// bound expression if the conservative pattern holds. Loops that don't touch
/// `v` (e.g. a preceding skip/scan loop) are stepped over; the first sign of an
/// unhandled use of `v` aborts.
fn find_fill_bound(stmts: &[Stmt], let_idx: usize, v: &str) -> Option<Expr> {
    let mut bound_between: Vec<String> = Vec::new();
    // Pre-loop seed pushes to `v` (e.g. `fact.push(1)` before the counted fill,
    // or base cases seeding a DP table). Each adds one element on top of the
    // loop's trip count, so they're folded into the reservation below.
    let mut prelude_pushes: i64 = 0;
    for stmt in &stmts[let_idx + 1..] {
        let verdict = match &stmt.kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::While {
                    condition, body, ..
                } => analyze_while(condition, body, v, &bound_between),
                ExprKind::For {
                    pattern,
                    iterable,
                    body,
                    ..
                } => analyze_for(pattern, iterable, body, v, &bound_between),
                // A pre-loop seed push to `v` — count it rather than bail; the
                // reservation is the loop trip count plus these seeds.
                _ if is_push_to(e, v) => {
                    prelude_pushes += 1;
                    LoopVerdict::Skip
                }
                // Any OTHER non-loop statement that touches `v` (a move into a
                // call, an index-write, a non-push method, …) — we don't
                // understand `v`'s use; bail.
                _ if mentions_ident(e, v) => LoopVerdict::Bail,
                _ => LoopVerdict::Skip,
            },
            // A non-expr statement between the Let and the loop. If it touches
            // `v`, bail (v may be moved/reassigned). Otherwise record the names
            // it binds — the bound must not reference any (not in scope at Let).
            _ => {
                if stmt_mentions_ident(stmt, v) {
                    return None;
                }
                collect_let_bound_names(stmt, &mut bound_between);
                LoopVerdict::Skip
            }
        };
        match verdict {
            LoopVerdict::Presize(bound) => {
                return Some(if prelude_pushes > 0 {
                    add_const(&bound, prelude_pushes)
                } else {
                    *bound
                });
            }
            LoopVerdict::Bail => return None,
            LoopVerdict::Skip => {}
        }
    }
    None
}

/// `while IV < BOUND { body }` or `while IV <= BOUND { body }` — classify for
/// the fill search. The `<=` form runs one extra iteration, so its reservation
/// is `BOUND + 1`; the raw `BOUND` is still what `fill_loop_ok` validates (so
/// the giant-literal guard sees the literal, not a `+ 1` wrapper hiding it), and
/// the `+ 1` only wraps the accepted bound. Over-reserving by `IV`'s start value
/// is harmless — capacity is a hint, never an output-affecting quantity.
fn analyze_while(condition: &Expr, body: &Block, v: &str, bound_between: &[String]) -> LoopVerdict {
    if let ExprKind::Binary { op, left, right } = &condition.kind {
        let inclusive = match op {
            BinOp::Lt => Some(false),
            BinOp::LtEq => Some(true),
            _ => None,
        };
        if let Some(inclusive) = inclusive {
            if let ExprKind::Identifier(iv) = &left.kind {
                if iv != v && fill_loop_ok(body, v, Some(iv), right, bound_between) {
                    let bound = if inclusive {
                        add_const(right, 1)
                    } else {
                        (**right).clone()
                    };
                    return LoopVerdict::Presize(Box::new(bound));
                }
            }
        }
    }
    loop_touches_v_verdict(Some(condition), body, v)
}

/// Build the expression `<e> + k` (`k >= 1`) — a larger reservation than the
/// raw bound. An inclusive `<=` loop adds one iteration; each pre-loop seed
/// push adds one element. Spans are inherited from `e` — synthesized nodes
/// never surface in diagnostics, they only feed the `with_capacity` argument.
fn add_const(e: &Expr, k: i64) -> Expr {
    let span = e.span.clone();
    Expr {
        kind: ExprKind::Binary {
            op: BinOp::Add,
            left: Box::new(e.clone()),
            right: Box::new(Expr {
                kind: ExprKind::Integer(k, None),
                span: span.clone(),
            }),
        },
        span,
    }
}

/// `for I in LO..BOUND { body }` (exclusive or inclusive). Bound is the range's
/// upper expr; the (≤ LO) over-reservation is harmless.
fn analyze_for(
    pattern: &Pattern,
    iterable: &Expr,
    body: &Block,
    v: &str,
    bound_between: &[String],
) -> LoopVerdict {
    let iv = match &pattern.kind {
        PatternKind::Binding(n) => Some(n.as_str()),
        _ => None,
    };
    if let ExprKind::Range { end: Some(end), .. } = &iterable.kind {
        if fill_loop_ok(body, v, iv, end, bound_between) {
            return LoopVerdict::Presize(Box::new((**end).clone()));
        }
    }
    loop_touches_v_verdict(Some(iterable), body, v)
}

/// A loop that wasn't a clean fill: `Bail` if it mentions `v` anywhere (we don't
/// understand the use), else `Skip` (it's unrelated to `v`).
fn loop_touches_v_verdict(header: Option<&Expr>, body: &Block, v: &str) -> LoopVerdict {
    let touches = header.is_some_and(|h| mentions_ident(h, v)) || block_mentions_ident(body, v);
    if touches {
        LoopVerdict::Bail
    } else {
        LoopVerdict::Skip
    }
}

fn block_mentions_ident(block: &Block, name: &str) -> bool {
    block.stmts.iter().any(|s| stmt_mentions_ident(s, name))
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| mentions_ident(e, name))
}

/// Shared body/bound validation for both loop forms.
fn fill_loop_ok(
    body: &Block,
    v: &str,
    iv: Option<&str>,
    bound: &Expr,
    bound_between: &[String],
) -> bool {
    // (a) Body must fill `v` exactly once, unconditionally, with no early exit.
    if !body_fills_once(body, v) {
        return false;
    }
    // (b) Bound must be fully analyzable and not reference `v` or the counter.
    let mut bound_idents = Vec::new();
    if !collect_idents(bound, &mut bound_idents) {
        return false;
    }
    if bound_idents
        .iter()
        .any(|n| n == v || Some(n.as_str()) == iv)
    {
        return false;
    }
    // (c) Bound must be a small-or-variable quantity (no giant literal).
    if let ExprKind::Integer(k, _) = &bound.kind {
        if *k < 0 || *k > MAX_LITERAL_BOUND {
            return false;
        }
    }
    // (d) Every bound identifier must be in scope at the Let.
    if bound_idents.iter().any(|n| bound_between.contains(n)) {
        return false;
    }
    // (e) Bound must be loop-invariant: none of its identifiers is assigned or
    //     method-called (potential mutation) anywhere in the body.
    if bound_idents.iter().any(|n| ident_mutated_in_block(body, n)) {
        return false;
    }
    true
}

/// The body pushes to `v` exactly once, unconditionally, with no
/// break/continue/return. Read-only mentions of `v` elsewhere in the body — e.g.
/// `v.len()` or `v[i - 1]` in a cumulative fill `v.push(v[i - 1] + x)` — are
/// allowed: a read never changes the length, so push-count still equals
/// trip-count.
///
/// Correctness rests on the complete-or-bail ident analysis: `count_ident_in_
/// block` returns `None` on any node it can't fully enumerate (nested `if`/loop
/// bodies, or `v` moved into an un-walked position), so a body that survives it
/// is straight-line — which makes the single top-level push the *only* push
/// (i.e. unconditional). And pre-sizing is a pure capacity hint, so even an
/// imperfect estimate from an allowed read can never change program output.
fn body_fills_once(body: &Block, v: &str) -> bool {
    if block_has_early_exit(body) {
        return false;
    }
    // Fail closed on any body we cannot fully analyze (nested control flow, or
    // `v` in an un-walked position) — guarantees the body is straight-line, so
    // every push to `v` is a top-level statement counted below.
    if count_ident_in_block(body, v).is_none() {
        return false;
    }
    // Exactly one (necessarily top-level, hence unconditional) push to `v`.
    body.stmts
        .iter()
        .filter(|s| matches!(&s.kind, StmtKind::Expr(e) if is_push_to(e, v)))
        .count()
        == 1
}

fn is_push_to(e: &Expr, v: &str) -> bool {
    matches!(&e.kind, ExprKind::MethodCall { object, method, .. }
        if matches!(&object.kind, ExprKind::Identifier(n) if n == v)
            && matches!(method.as_str(), "push" | "push_str" | "push_back"))
}

/// Rewrite `let mut V = new()/""` to `let mut V = <Coll>.with_capacity(bound)`.
fn rewrite_to_with_capacity(stmt: &mut Stmt, coll: Coll, bound: Expr) {
    let StmtKind::Let { value, .. } = &mut stmt.kind else {
        return;
    };
    let span = value.span.clone();
    let callee = Expr {
        kind: ExprKind::Path {
            segments: vec![coll.type_name().to_string(), "with_capacity".to_string()],
            generic_args: None,
        },
        span: span.clone(),
    };
    let arg = CallArg {
        label: None,
        mut_marker: false,
        value: bound,
        span: span.clone(),
    };
    *value = Expr {
        kind: ExprKind::Call {
            callee: Box::new(callee),
            args: vec![arg],
        },
        span,
    };
}

// ── AST analysis helpers ─────────────────────────────────────────
//
// `collect_idents` / `count_ident_*` are *complete-or-bail*: they return `false`
// / `None` on any `ExprKind` they don't explicitly understand, so a candidate is
// only pre-sized when every relevant expression was fully analyzed. Under-
// counting identifiers would be unsafe (it could hide an out-of-scope or mutated
// bound), so the unhandled arm must fail closed.

/// Collect all identifier names in `e`. Returns false if `e` (or a child)
/// contains an `ExprKind` not understood here — caller must then bail.
fn collect_idents(e: &Expr, out: &mut Vec<String>) -> bool {
    match &e.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::Bool(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..)
        | ExprKind::StringLit(..) => true,
        ExprKind::Identifier(n) => {
            out.push(n.clone());
            true
        }
        ExprKind::Binary { left, right, .. } => {
            collect_idents(left, out) && collect_idents(right, out)
        }
        ExprKind::Unary { operand, .. } => collect_idents(operand, out),
        ExprKind::Index { object, index } => {
            collect_idents(object, out) && collect_idents(index, out)
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            collect_idents(object, out)
        }
        ExprKind::MethodCall { object, args, .. } => {
            if !collect_idents(object, out) {
                return false;
            }
            args.iter().all(|a| collect_idents(&a.value, out))
        }
        ExprKind::Call { callee, args } => {
            if !collect_idents(callee, out) {
                return false;
            }
            args.iter().all(|a| collect_idents(&a.value, out))
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                if !collect_idents(s, out) {
                    return false;
                }
            }
            match end {
                Some(e) => collect_idents(e, out),
                None => true,
            }
        }
        // A 2-segment `Type.const` path names no local; a bare path is rare in a
        // numeric bound. Treat the root segment as a referenced name (safe
        // over-approximation) for single-segment paths; bail on longer ones.
        ExprKind::Path { segments, .. } if segments.len() == 1 => {
            out.push(segments[0].clone());
            true
        }
        _ => false,
    }
}

/// Count identifier occurrences of `name` across a whole block (stmts + final
/// expr). `None` if any node is unanalyzable.
fn count_ident_in_block(block: &Block, name: &str) -> Option<usize> {
    let mut total = 0;
    for s in &block.stmts {
        total += count_ident_in_stmt(s, name)?;
    }
    if let Some(e) = &block.final_expr {
        total += count_ident_in_expr(e, name)?;
    }
    Some(total)
}

fn count_ident_in_stmt(stmt: &Stmt, name: &str) -> Option<usize> {
    match &stmt.kind {
        StmtKind::Let { value, .. } => count_ident_in_expr(value, name),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            Some(count_ident_in_expr(target, name)? + count_ident_in_expr(value, name)?)
        }
        StmtKind::Expr(e) => count_ident_in_expr(e, name),
        // Uninit/defer/let-else introduce nesting we don't analyze — fail closed.
        _ => None,
    }
}

fn count_ident_in_expr(e: &Expr, name: &str) -> Option<usize> {
    let mut idents = Vec::new();
    // Reuse the complete-or-bail collector, then count matches. A loop body's
    // inner control flow (if/while/…) is not enumerated by `collect_idents`, so
    // it returns false there — which is exactly the fail-closed behavior we want
    // (a body with nested control flow won't pre-size, but stays correct).
    if !collect_idents(e, &mut idents) {
        return None;
    }
    Some(idents.iter().filter(|n| n.as_str() == name).count())
}

/// True if `name` is assigned to, or used as a method-call receiver (a possible
/// mutation), anywhere in `block`. Used for the bound-invariance check. Fails
/// *open* (returns true → bail the optimization) on unanalyzable nesting.
fn ident_mutated_in_block(block: &Block, name: &str) -> bool {
    block.stmts.iter().any(|s| stmt_mutates_ident(s, name))
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_mutates_ident(e, name))
}

fn stmt_mutates_ident(stmt: &Stmt, name: &str) -> bool {
    match &stmt.kind {
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            assign_target_root_is(target, name)
                || expr_mutates_ident(target, name)
                || expr_mutates_ident(value, name)
        }
        StmtKind::Let { value, .. } => expr_mutates_ident(value, name),
        StmtKind::Expr(e) => expr_mutates_ident(e, name),
        // Unanalyzed statement kinds: assume they might mutate → bail.
        _ => true,
    }
}

fn expr_mutates_ident(e: &Expr, name: &str) -> bool {
    match &e.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::Bool(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..)
        | ExprKind::StringLit(..)
        | ExprKind::Identifier(..)
        | ExprKind::Path { .. } => false,
        ExprKind::Binary { left, right, .. } => {
            expr_mutates_ident(left, name) || expr_mutates_ident(right, name)
        }
        ExprKind::Unary { operand, .. } => expr_mutates_ident(operand, name),
        ExprKind::Index { object, index } => {
            expr_mutates_ident(object, name) || expr_mutates_ident(index, name)
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            expr_mutates_ident(object, name)
        }
        ExprKind::MethodCall { object, args, .. } => {
            // A method call on `name` may mutate it (push/clear/…). Treat any
            // receiver match as a mutation.
            matches!(&object.kind, ExprKind::Identifier(n) if n == name)
                || expr_mutates_ident(object, name)
                || args.iter().any(|a| expr_mutates_ident(&a.value, name))
        }
        ExprKind::Call { callee, args } => {
            expr_mutates_ident(callee, name)
                || args.iter().any(|a| expr_mutates_ident(&a.value, name))
        }
        ExprKind::Range { start, end, .. } => {
            start.as_ref().is_some_and(|s| expr_mutates_ident(s, name))
                || end.as_ref().is_some_and(|e| expr_mutates_ident(e, name))
        }
        // Unknown nesting (closures, if/match in expr position, …): fail open.
        _ => true,
    }
}

fn assign_target_root_is(target: &Expr, name: &str) -> bool {
    match &target.kind {
        ExprKind::Identifier(n) => n == name,
        ExprKind::Index { object, .. }
        | ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. } => assign_target_root_is(object, name),
        _ => false,
    }
}

/// Any break/continue/return anywhere in the block (so we can't trust the trip
/// count). Conservative: recurses only into the shapes it knows; an unknown
/// nested form makes the enclosing `count_ident_in_block` bail anyway.
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

// ── "mentions identifier" (looser; used for the between-Let-and-loop guard) ──

fn stmt_mentions_ident(stmt: &Stmt, name: &str) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } => mentions_ident(value, name),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            mentions_ident(target, name) || mentions_ident(value, name)
        }
        StmtKind::Expr(e) => mentions_ident(e, name),
        // Conservatively assume an unanalyzed statement kind touches the name.
        _ => true,
    }
}

/// Whether `name` appears anywhere in `e`. Fails *open* (returns true on unknown
/// shapes) so the between-Let-and-loop guard can't miss a hidden use of `v`.
fn mentions_ident(e: &Expr, name: &str) -> bool {
    match &e.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::Bool(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..)
        | ExprKind::StringLit(..) => false,
        ExprKind::Identifier(n) => n == name,
        ExprKind::Path { segments, .. } => segments.first().is_some_and(|s| s == name),
        ExprKind::Binary { left, right, .. } => {
            mentions_ident(left, name) || mentions_ident(right, name)
        }
        ExprKind::Unary { operand, .. } => mentions_ident(operand, name),
        ExprKind::Index { object, index } => {
            mentions_ident(object, name) || mentions_ident(index, name)
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            mentions_ident(object, name)
        }
        ExprKind::MethodCall { object, args, .. } => {
            mentions_ident(object, name) || args.iter().any(|a| mentions_ident(&a.value, name))
        }
        ExprKind::Call { callee, args } => {
            mentions_ident(callee, name) || args.iter().any(|a| mentions_ident(&a.value, name))
        }
        ExprKind::Range { start, end, .. } => {
            start.as_ref().is_some_and(|s| mentions_ident(s, name))
                || end.as_ref().is_some_and(|e| mentions_ident(e, name))
        }
        // Unknown nesting: fail open (treat as a use), so we never pre-size past
        // a hidden move/escape of `v`.
        _ => true,
    }
}

/// Record the names a between-Let-and-loop statement binds (so the bound can't
/// reference something declared after the Let).
fn collect_let_bound_names(stmt: &Stmt, out: &mut Vec<String>) {
    match &stmt.kind {
        StmtKind::Let { pattern, .. } => {
            if let PatternKind::Binding(n) = &pattern.kind {
                out.push(n.clone());
            }
        }
        StmtKind::LetUninit { name, .. } => out.push(name.clone()),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse + typecheck `src`, run pre-sizing on the first function's body, and
    /// return the body so a test can inspect how `name`'s init was rewritten.
    /// Typecheck is required so `fill_to_filled` can consult `expr_types` for the
    /// element-width gate (the `with_capacity` pass ignores `tc`).
    fn presize_first_fn(src: &str) -> Block {
        let parsed = crate::parse(src);
        let rr = crate::resolve(&parsed.program);
        let tc = crate::typecheck(&parsed.program, &rr);
        let mut body = parsed
            .program
            .items
            .into_iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f.body),
                _ => None,
            })
            .expect("a function");
        presize_block(&mut body, &tc);
        body
    }

    /// Whether `name`'s init became a `Vec.<method>(...)` call after pre-sizing.
    fn init_is(body: &Block, name: &str, method: &str) -> bool {
        body.stmts.iter().any(|s| match &s.kind {
            StmtKind::Let { pattern, value, .. } => {
                matches!(&pattern.kind, PatternKind::Binding(n) if n == name)
                    && matches!(&value.kind, ExprKind::Call { callee, .. }
                        if matches!(&callee.kind, ExprKind::Path { segments, .. }
                            if segments.len() == 2 && segments[1] == method))
            }
            _ => false,
        })
    }

    /// Whether `name` ended up as a `*.with_capacity(...)` init.
    fn fires_for(src: &str, name: &str) -> bool {
        init_is(&presize_first_fn(src), name, "with_capacity")
    }

    /// Whether `name` ended up as a `Vec.filled(...)` init (the strongest rewrite)
    /// AND the fill loop was deleted (statement count is what remains).
    fn filled_fires_for(src: &str, name: &str) -> bool {
        init_is(&presize_first_fn(src), name, "filled")
    }

    /// Count `while` + `for` loop statements left in the first function's body —
    /// a `fill_to_filled` firing must delete the fill loop.
    fn loop_count(src: &str) -> usize {
        presize_first_fn(src)
            .stmts
            .iter()
            .filter(|s| {
                matches!(&s.kind, StmtKind::Expr(e)
                    if matches!(&e.kind, ExprKind::While { .. } | ExprKind::For { .. }))
            })
            .count()
    }

    #[test]
    fn fires_on_counted_while_vec() {
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut i = 0i64;\n  while i < n {\n    v.push(i);\n    i = i + 1i64;\n  }\n}\n";
        assert!(fires_for(src, "v"));
    }

    #[test]
    fn fires_on_counted_while_string_empty() {
        let src = "fn f(xs: ref Vec[u8]) {\n  let mut s: String = \"\";\n  let mut i = 0i64;\n  while i < xs.len() {\n    s.push_str(\"x\");\n    i = i + 1i64;\n  }\n}\n";
        assert!(fires_for(src, "s"));
    }

    #[test]
    fn fires_on_for_range() {
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  for i in 0i64..n {\n    v.push(i);\n  }\n}\n";
        assert!(fires_for(src, "v"));
    }

    #[test]
    fn skips_unrelated_loop_before_fill() {
        // A scan loop that doesn't touch `v` precedes the fill loop (the #43
        // multiply `out` shape) — must step over it and still pre-size.
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut k = 0i64;\n  while k < n {\n    k = k + 1i64;\n  }\n  let mut j = 0i64;\n  while j < n {\n    v.push(j);\n    j = j + 1i64;\n  }\n}\n";
        assert!(fires_for(src, "v"));
    }

    #[test]
    fn no_fire_on_conditional_push() {
        // Push nested in an `if` — not unconditional, count bails.
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut i = 0i64;\n  while i < n {\n    if i > 0i64 { v.push(i); }\n    i = i + 1i64;\n  }\n}\n";
        assert!(!fires_for(src, "v"));
    }

    #[test]
    fn no_fire_on_two_pushes() {
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut i = 0i64;\n  while i < n {\n    v.push(i);\n    v.push(i);\n    i = i + 1i64;\n  }\n}\n";
        assert!(!fires_for(src, "v"));
    }

    #[test]
    fn no_fire_when_bound_declared_after_let() {
        // `hi` is bound between the Let and the loop → not in scope at the Let.
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut i = 0i64;\n  let hi = n + 1i64;\n  while i < hi {\n    v.push(i);\n    i = i + 1i64;\n  }\n}\n";
        assert!(!fires_for(src, "v"));
    }

    #[test]
    fn no_fire_when_bound_mutated_in_body() {
        // The bound `xs.len()` changes because the body also pushes to `xs`.
        let src = "fn f(xs: mut ref Vec[i64]) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut i = 0i64;\n  while i < xs.len() {\n    xs.push(0i64);\n    v.push(i);\n    i = i + 1i64;\n  }\n}\n";
        assert!(!fires_for(src, "v"));
    }

    #[test]
    fn no_fire_on_giant_literal_bound() {
        let src = "fn f() {\n  let mut v: Vec[i64] = Vec.new();\n  let mut i = 0i64;\n  while i < 2000000000i64 {\n    v.push(i);\n    i = i + 1i64;\n  }\n}\n";
        assert!(!fires_for(src, "v"));
    }

    #[test]
    fn no_fire_on_nonempty_string() {
        let src = "fn f(n: i64) {\n  let mut s: String = \"seed\";\n  let mut i = 0i64;\n  while i < n {\n    s.push_str(\"x\");\n    i = i + 1i64;\n  }\n}\n";
        assert!(!fires_for(src, "s"));
    }

    #[test]
    fn fires_on_counted_while_le() {
        // Inclusive `<=` fill (`while d <= n`) — one iteration more than `<`;
        // reserve BOUND + 1. This is the common counted-loop idiom the pass
        // previously missed (measured 1.36x on the kata-60 factorial `digits`).
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut d = 1i64;\n  while d <= n {\n    v.push(d);\n    d = d + 1i64;\n  }\n}\n";
        assert!(fires_for(src, "v"));
    }

    #[test]
    fn fires_when_body_reads_v() {
        // A cumulative fill reads `v` (here `v.len()`) alongside the single
        // push. A read never changes the length, so push-count == trip-count —
        // the pass must still pre-size. (The old `count == 1` rule rejected any
        // read of `v`.)
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut i = 0i64;\n  while i < n {\n    v.push(v.len());\n    i = i + 1i64;\n  }\n}\n";
        assert!(fires_for(src, "v"));
    }

    #[test]
    fn no_fire_on_two_pushes_with_read() {
        // A read of `v` does not license a second push: two pushes still means
        // push-count != trip-count, so no pre-size (guards the read relaxation
        // from over-firing).
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut i = 0i64;\n  while i < n {\n    v.push(v.len());\n    v.push(i);\n    i = i + 1i64;\n  }\n}\n";
        assert!(!fires_for(src, "v"));
    }

    #[test]
    fn fires_on_seed_push_then_cumulative_le_fill() {
        // The kata-60 factorial `fact` shape: a pre-loop seed push, then a
        // cumulative `<=` fill that reads `fact[i-1]`. Combines all three
        // relaxations (seed push + `<=` + read-in-body); reserves BOUND + 1
        // (the `<=`) + 1 (the seed).
        let src = "fn f(n: i64) {\n  let mut fact: Vec[i64] = Vec.new();\n  fact.push(1i64);\n  let mut i = 1i64;\n  while i <= n {\n    fact.push(fact[i - 1i64] * i);\n    i = i + 1i64;\n  }\n}\n";
        assert!(fires_for(src, "fact"));
    }

    #[test]
    fn no_fire_on_non_push_prelude_mention() {
        // A pre-loop statement that mentions `v` other than by pushing (here `v`
        // moved into a call) is not understood → bail. Guards the seed-push
        // relaxation from swallowing moves/reassigns.
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  sink(v);\n  let mut i = 0i64;\n  while i < n {\n    v.push(i);\n    i = i + 1i64;\n  }\n}\n";
        assert!(!fires_for(src, "v"));
    }

    // ── fill_to_filled (Vec.new()+counted push(literal) → Vec.filled) ────────

    #[test]
    fn filled_fires_on_while_zero_start_literal() {
        // The kata #63 dp-fill shape: `while j < cols { dp.push(0); j = j + 1 }`
        // over `j` proven 0 → collapses to `Vec.filled(cols, 0)`, loop deleted.
        let src = "fn f(cols: i64) {\n  let mut dp: Vec[i64] = Vec.new();\n  let mut j = 0i64;\n  while j < cols {\n    dp.push(0i64);\n    j = j + 1i64;\n  }\n}\n";
        assert!(filled_fires_for(src, "dp"));
        assert_eq!(loop_count(src), 0, "fill loop must be deleted");
    }

    #[test]
    fn filled_fires_on_while_compound_step() {
        // `j += 1` (CompoundAssign) is an accepted `+1` step form.
        let src = "fn f(cols: i64) {\n  let mut dp: Vec[i64] = Vec.new();\n  let mut j = 0i64;\n  while j < cols {\n    dp.push(0i64);\n    j += 1i64;\n  }\n}\n";
        assert!(filled_fires_for(src, "dp"));
        assert_eq!(loop_count(src), 0);
    }

    #[test]
    fn filled_fires_on_for_zero_range() {
        // `for i in 0..n { v.push(7) }` → `Vec.filled(n, 7)`.
        let src = "fn f(n: i64) {\n  let mut v: Vec[i64] = Vec.new();\n  for i in 0i64..n {\n    v.push(7i64);\n  }\n}\n";
        assert!(filled_fires_for(src, "v"));
        assert_eq!(loop_count(src), 0);
    }

    #[test]
    fn filled_no_fire_on_narrow_element() {
        // A `Vec[i32]` fill: the bare literal compiles to i64 but the element is
        // i32, so `Vec.filled` would mis-size the buffer — must NOT collapse.
        // Falls through to `with_capacity` instead (still correct).
        let src = "fn f(cols: i64) {\n  let mut dp: Vec[i32] = Vec.new();\n  let mut j = 0i64;\n  while j < cols {\n    dp.push(0);\n    j = j + 1i64;\n  }\n}\n";
        assert!(!filled_fires_for(src, "dp"));
        assert!(
            fires_for(src, "dp"),
            "narrow element still gets with_capacity"
        );
    }

    #[test]
    fn filled_no_fire_on_nonliteral_value() {
        // `dp.push(j)` fills with the counter, not a literal — `Vec.filled` needs
        // a single fixed value. Stays on the `with_capacity` path.
        let src = "fn f(cols: i64) {\n  let mut dp: Vec[i64] = Vec.new();\n  let mut j = 0i64;\n  while j < cols {\n    dp.push(j);\n    j = j + 1i64;\n  }\n}\n";
        assert!(!filled_fires_for(src, "dp"));
        assert!(fires_for(src, "dp"));
    }

    #[test]
    fn filled_no_fire_on_nonzero_counter_start() {
        // `j` starts at 1, so the loop runs `cols - 1` times — NOT `cols`. An
        // exact `Vec.filled(cols, _)` would over-count; must not fire.
        let src = "fn f(cols: i64) {\n  let mut dp: Vec[i64] = Vec.new();\n  let mut j = 1i64;\n  while j < cols {\n    dp.push(0i64);\n    j = j + 1i64;\n  }\n}\n";
        assert!(!filled_fires_for(src, "dp"));
    }

    #[test]
    fn filled_fires_when_counter_used_after_via_fixup() {
        // `j` is read after the loop. The rewrite still fires: the `while` is
        // replaced by `j = cols` (its exact post-loop value), so the later read
        // is preserved. The fill loop is gone (replaced by an assignment).
        let src = "fn f(cols: i64) -> i64 {\n  let mut dp: Vec[i64] = Vec.new();\n  let mut j = 0i64;\n  while j < cols {\n    dp.push(0i64);\n    j = j + 1i64;\n  }\n  return j;\n}\n";
        assert!(filled_fires_for(src, "dp"));
        assert_eq!(loop_count(src), 0, "while loop replaced by counter fixup");
        // The counter fixup `j = cols` must be present so the `return j` is correct.
        let body = presize_first_fn(src);
        assert!(
            body.stmts.iter().any(|s| matches!(&s.kind,
                StmtKind::Assign { target, value }
                    if matches!(&target.kind, ExprKind::Identifier(n) if n == "j")
                        && matches!(&value.kind, ExprKind::Identifier(n) if n == "cols"))),
            "counter pinned to bound"
        );
    }

    #[test]
    fn filled_no_fire_on_seed_push() {
        // A pre-loop seed push makes the total length `1 + cols`, not `cols` —
        // `Vec.filled(cols, _)` would under-count. Must not fire.
        let src = "fn f(cols: i64) {\n  let mut dp: Vec[i64] = Vec.new();\n  dp.push(9i64);\n  let mut j = 0i64;\n  while j < cols {\n    dp.push(0i64);\n    j = j + 1i64;\n  }\n}\n";
        assert!(!filled_fires_for(src, "dp"));
    }

    #[test]
    fn filled_no_fire_on_inclusive_while() {
        // `while j <= cols` runs `cols + 1` times, not `cols`. Not a `<` header,
        // so `as_strict_lt` rejects it — must not fire.
        let src = "fn f(cols: i64) {\n  let mut dp: Vec[i64] = Vec.new();\n  let mut j = 0i64;\n  while j <= cols {\n    dp.push(0i64);\n    j = j + 1i64;\n  }\n}\n";
        assert!(!filled_fires_for(src, "dp"));
    }

    #[test]
    fn filled_no_fire_on_extra_body_stmt() {
        // A body with more than the push + step (here an extra `dp[0] = 1`) is not
        // a clean count-preserving fill — must not collapse (removal would drop
        // the extra statement).
        let src = "fn f(cols: i64) {\n  let mut dp: Vec[i64] = Vec.new();\n  let mut j = 0i64;\n  while j < cols {\n    dp.push(0i64);\n    dp[0i64] = 1i64;\n    j = j + 1i64;\n  }\n}\n";
        assert!(!filled_fires_for(src, "dp"));
    }

    #[test]
    fn filled_fires_on_len_bound() {
        // `xs.len()` is a valid fill count: invariant across the loop (the body
        // never touches `xs`) and evaluated once by `Vec.filled`.
        let src = "fn f(xs: ref Vec[i64]) {\n  let mut v: Vec[i64] = Vec.new();\n  let mut j = 0i64;\n  while j < xs.len() {\n    v.push(0i64);\n    j = j + 1i64;\n  }\n}\n";
        assert!(filled_fires_for(src, "v"));
        assert_eq!(loop_count(src), 0);
    }

    #[test]
    fn filled_no_fire_when_bound_mutated_before_loop() {
        // `xs` is pushed BETWEEN the `let` and the loop, so `xs.len()` at the
        // `let` position (where `Vec.filled` evaluates it) differs from its value
        // at the loop — a miscount. Must not fire.
        let src = "fn f(xs: mut ref Vec[i64]) {\n  let mut v: Vec[i64] = Vec.new();\n  xs.push(1i64);\n  let mut j = 0i64;\n  while j < xs.len() {\n    v.push(0i64);\n    j = j + 1i64;\n  }\n}\n";
        assert!(!filled_fires_for(src, "v"));
    }
}
