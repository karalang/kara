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
//!   3. `V` is not mentioned between the Let and the loop. Inside the loop body
//!      `V` may be *read* (e.g. `V[i - 1]` / `V.len()` in a cumulative fill) but
//!      is pushed to exactly once — reads don't change the length, so the
//!      trip-count reservation stays exact.
//!   4. `BOUND` is fully analyzable, loop-invariant (none of its identifiers is
//!      assigned or method-called in the body), does not reference `IV` or `V`,
//!      and every identifier it names is in scope at the Let (i.e. not bound by
//!      a statement between the Let and the loop).
//!   5. `BOUND` is not a large integer literal (guards a pathological
//!      `while i < 1_000_000_000` from reserving gigabytes speculatively).

use crate::ast::*;

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
pub fn presize_block(block: &mut Block) {
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
                // A non-loop expression statement that touches `v` (a move into a
                // call, an early read, …) — we don't understand `v`'s use; bail.
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
            LoopVerdict::Presize(bound) => return Some(*bound),
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
                        plus_one(right)
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

/// Build the expression `<e> + 1` (turns an inclusive `<=` bound into its
/// trip count). Spans are inherited from `e` — synthesized nodes never surface
/// in diagnostics, they only feed the `with_capacity` argument.
fn plus_one(e: &Expr) -> Expr {
    let span = e.span.clone();
    Expr {
        kind: ExprKind::Binary {
            op: BinOp::Add,
            left: Box::new(e.clone()),
            right: Box::new(Expr {
                kind: ExprKind::Integer(1, None),
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

    /// Parse `src`, run pre-sizing on the first function's body, and report
    /// whether the binding `name` ended up as a `*.with_capacity(...)` init.
    fn fires_for(src: &str, name: &str) -> bool {
        let parsed = crate::parse(src);
        let mut body = parsed
            .program
            .items
            .into_iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f.body),
                _ => None,
            })
            .expect("a function");
        presize_block(&mut body);
        body.stmts.iter().any(|s| match &s.kind {
            StmtKind::Let { pattern, value, .. } => {
                matches!(&pattern.kind, PatternKind::Binding(n) if n == name)
                    && matches!(&value.kind, ExprKind::Call { callee, .. }
                        if matches!(&callee.kind, ExprKind::Path { segments, .. }
                            if segments.len() == 2 && segments[1] == "with_capacity"))
            }
            _ => false,
        })
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
}
