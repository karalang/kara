//! Bounded-accumulator overflow-check elision (B-2026-07-26-1).
//!
//! # What this removes and why it is worth removing
//!
//! Kāra traps on integer overflow by default, so `cnt = cnt + 1i64` lowers to
//! `llvm.sadd.with.overflow.i64` plus a branch to a panic block. That is
//! normally cheap. Inside a *guarded* counting loop it is not, because the
//! branch is what blocks the transform that matters:
//!
//! ```kara
//! while i < nums.len() {
//!     if ((nums[i] >> b) & 1i64) == 1i64 {
//!         cnt = cnt + 1i64;
//!     }
//!     i = i + 1i64;
//! }
//! ```
//!
//! A checked add can trap, so LLVM may not speculate it, so SimplifyCFG cannot
//! if-convert `if bit { cnt += 1 }` into the branchless `cnt += bit`. What
//! survives is a real conditional branch on a ~50%-random bit.
//!
//! Measured on kata #137 (four 120,001-element arrays, 40 punches), isolating
//! it in the Rust mirror one statement at a time:
//!
//! | build | mean |
//! |---|---|
//! | everything checked | 277 ms |
//! | only `cnt += 1` unchecked, all else still checked | **30 ms** |
//! | that same build with both LLVM vectorizers off | 40 ms |
//! | everything checked, but uniform (perfectly predicted) data | 45 ms |
//!
//! So the whole 7.9× is this one check; it is not vectorization (step 2); and
//! the mechanism is branch misprediction (step 3). Kāra measured 281 ms on the
//! same program against `clang -O3`'s 35 ms.
//!
//! # The soundness argument
//!
//! Eliding a trap is only legal if the trap provably cannot fire. The pattern
//! recognized here is deliberately narrow so that the proof is airtight and
//! needs no platform assumptions (no "allocations are smaller than 2^48", no
//! reasoning about `.len()`'s range):
//!
//! ```text
//! let mut acc = 0;              // integer literal ZERO
//! let mut i   = <lit >= 0>;     // non-negative integer literal
//! while i < <bound> {           // ascending guard, `bound` any i64 expression
//!     ...
//!     acc = acc + 1;            // EXACTLY ONE such site, no other write to acc
//!     ...
//!     i = i + <lit >= 1>;       // exactly one ascending step, no other write to i
//! }
//! ```
//!
//! Then:
//!
//! 1. `i` starts at `i0 >= 0` and strictly increases; the loop runs only while
//!    `i < bound`. So the trip count is at most `bound - i0 <= bound`.
//! 2. `bound` is an `i64` **value**, so `bound <= i64::MAX`, hence
//!    `trip <= i64::MAX`.
//! 3. `acc` starts at `0` and is incremented by exactly `1` at most once per
//!    iteration, so `acc <= trip <= i64::MAX` at every point — including after
//!    the final increment, which produces at most `i64::MAX` exactly.
//!
//! Therefore the overflow flag on that add is always false and the trap is
//! dead code. Note step 3 relies on the site being unique: two increment sites
//! would give `acc <= 2 * trip`, which can exceed `i64::MAX`, so the analysis
//! rejects that. It likewise relies on `acc`'s initializer being literally `0`
//! — a non-zero start would give `acc <= init + trip`, which can overflow.
//!
//! `i`'s own check is deliberately LEFT IN PLACE. It is not needed by the
//! argument above, and keeping it means the loop counter still traps on a
//! genuinely out-of-range bound rather than silently wrapping.
//!
//! # What is deliberately NOT handled
//!
//! * `acc += 1` (the compound form) — same shape, but the assign lowering it
//!   goes through is a different arm; folding it in is a follow-up, not a
//!   correctness question.
//! * Steps other than `1`, non-zero initializers, and multiple increment
//!   sites — each breaks a step of the proof above and would need the trip
//!   count bounded more tightly than `i64::MAX` to recover.
//! * `for` loops, and accumulators that are struct fields, array elements, or
//!   captured by a closure — all out of scope for a first cut; only a plain
//!   local declared in the same block is recognized.
//! * Any loop whose body writes `acc` or `i` through some other shape
//!   (`mut ref` argument, index-assign, nested closure). The scan fails closed
//!   on every construct it does not explicitly understand.

use crate::ast::{BinOp, Block, Expr, ExprKind, Pattern, PatternKind, Stmt, StmtKind};
use crate::resolver::SpanKey;
use crate::token::Span;

/// The `acc = acc + 1` assignment statements inside `block` whose overflow
/// check may be elided, keyed by the ASSIGNMENT STATEMENT's span.
///
/// Scans `block`'s statement list for the `let acc = 0; let i = <lit>; while
/// …` shape described in the module docs and returns one key per qualifying
/// increment site. A block with no qualifying loop returns an empty vector,
/// which is the overwhelmingly common case and costs one pass over the
/// statements.
pub(super) fn check_free_accumulator_sites(block: &Block) -> Vec<SpanKey> {
    let mut out = Vec::new();
    collect_sites(block, &mut out);
    out
}

/// Scan `block`'s own statement list, then recurse into every nested block.
///
/// The recursion matters: the shape this targets is usually an INNER loop
/// (#137's per-bit count sits inside a `while b < 32` outer loop), and whether
/// `compile_block` is invoked for a given nested block is a lowering detail
/// this analysis should not depend on. Scanning the whole tree from one entry
/// makes the hook placement irrelevant; re-inserting a span already seen is a
/// no-op because the caller collects into a set.
fn collect_sites(block: &Block, out: &mut Vec<SpanKey>) {
    scan_one_block(block, out);
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Expr(e) => collect_sites_in_expr(e, out),
            StmtKind::Let { value, .. } => collect_sites_in_expr(value, out),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                collect_sites_in_expr(value, out);
                collect_sites(else_block, out);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => collect_sites(body, out),
            _ => {}
        }
    }
    if let Some(e) = &block.final_expr {
        collect_sites_in_expr(e, out);
    }
}

/// Recurse through the block-bearing expression forms. Unlike the fail-closed
/// `expr_is_analyzable` gate — which decides whether a candidate loop may be
/// TRUSTED — this walk only decides where to LOOK, so an unrecognized form is
/// simply not descended into rather than being treated as disqualifying.
fn collect_sites_in_expr(e: &Expr, out: &mut Vec<SpanKey>) {
    match &e.kind {
        ExprKind::While { body, .. } | ExprKind::For { body, .. } => collect_sites(body, out),
        ExprKind::Block(b) => collect_sites(b, out),
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            collect_sites(then_block, out);
            if let Some(eb) = else_branch {
                collect_sites_in_expr(eb, out);
            }
        }
        _ => {}
    }
}

/// The single-block scan: find `let acc = 0; let i = <lit>; while …` in THIS
/// statement list and emit one key per qualifying increment site.
fn scan_one_block(block: &Block, out: &mut Vec<SpanKey>) {
    for (idx, stmt) in block.stmts.iter().enumerate() {
        let StmtKind::Expr(e) = &stmt.kind else {
            continue;
        };
        let ExprKind::While {
            condition, body, ..
        } = &e.kind
        else {
            continue;
        };
        // Fail closed on any expression shape this analysis does not model —
        // in particular a closure, which could capture and increment the
        // accumulator outside the statement-shaped scan below.
        if !body_is_analyzable(body) {
            continue;
        }
        let Some(counter) = ascending_guard_counter(condition) else {
            continue;
        };
        // The counter must start non-negative and advance by a positive
        // constant exactly once per iteration (proof steps 1-2).
        if literal_init_before(block, idx, &counter).is_none_or(|v| v < 0) {
            continue;
        }
        if !advances_by_positive_constant_once(body, &counter) {
            continue;
        }
        for name in candidate_accumulators(body) {
            // `acc` must be a local of this block initialized to literal 0
            // (proof step 3) and must not be the counter itself.
            if name == counter {
                continue;
            }
            if literal_init_before(block, idx, &name) != Some(0) {
                continue;
            }
            if let Some(span) = sole_increment_site(body, &name) {
                out.push(SpanKey::from_span(&span));
            }
        }
    }
}

/// `var` when `cond` is an ASCENDING upper-bound guard on a bare identifier:
/// `v < X` / `v <= X` / `X > v` / `X >= v`, with `X` any expression. The
/// descending forms are rejected — the proof needs `i` to increase toward a
/// bound it starts below.
fn ascending_guard_counter(cond: &Expr) -> Option<String> {
    if let ExprKind::Binary { op, left, right } = &cond.kind {
        return match (op, &left.kind, &right.kind) {
            (BinOp::Lt | BinOp::LtEq, ExprKind::Identifier(v), _) => Some(v.clone()),
            (BinOp::Gt | BinOp::GtEq, _, ExprKind::Identifier(v)) => Some(v.clone()),
            _ => None,
        };
    }
    // Codegen runs on the LOWERED AST, where a comparison against a
    // trait-implementing operand is rewritten into a two-argument
    // `Ord::lt`-style `Call`. `guard_counter_var` in control_flow_bce.rs
    // recognizes the same pair of shapes for the unroll gates; missing this
    // arm is why the first cut of this analysis silently never fired.
    if let ExprKind::Call { callee, args } = &cond.kind {
        let ExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        if segments.len() != 2 || args.len() != 2 {
            return None;
        }
        return match (
            segments[1].as_str(),
            &args[0].value.kind,
            &args[1].value.kind,
        ) {
            ("lt" | "le", ExprKind::Identifier(v), _) => Some(v.clone()),
            ("gt" | "ge", _, ExprKind::Identifier(v)) => Some(v.clone()),
            _ => None,
        };
    }
    None
}

/// The integer literal `name` is initialized to by a `let` in `block` strictly
/// before index `before`, or `None` if it is not a local of this block, is not
/// initialized from a bare integer literal, or is written again between its
/// declaration and `before`.
fn literal_init_before(block: &Block, before: usize, name: &str) -> Option<i64> {
    let mut init = None;
    for stmt in block.stmts.iter().take(before) {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } if binds_identifier(pattern) == Some(name) => {
                init = integer_literal(value);
            }
            StmtKind::Let { .. } => {}
            // A write between the declaration and the loop invalidates the
            // literal we recorded — fail closed rather than tracking it.
            StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
                if matches!(&target.kind, ExprKind::Identifier(t) if t == name) {
                    init = None;
                }
            }
            _ => {}
        }
    }
    init
}

/// The single identifier a `let` pattern binds, when it is the simple
/// (non-destructuring) form.
fn binds_identifier(pattern: &Pattern) -> Option<&str> {
    match &pattern.kind {
        PatternKind::Binding(name) => Some(name.as_str()),
        _ => None,
    }
}

/// The `i64` value of a bare integer literal expression.
fn integer_literal(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::Integer(v, _) => Some(*v),
        _ => None,
    }
}

/// Whether `body` advances `var` by a positive integer constant at exactly one
/// TOP-LEVEL site and never writes it anywhere else (including nested).
fn advances_by_positive_constant_once(body: &Block, var: &str) -> bool {
    let mut top_level_steps = 0usize;
    for stmt in &body.stmts {
        if let StmtKind::Assign { target, value } = &stmt.kind {
            if matches!(&target.kind, ExprKind::Identifier(t) if t == var) {
                match add_of_identifier_and_literal(value, var) {
                    Some(k) if k >= 1 => top_level_steps += 1,
                    _ => return false,
                }
            }
        }
    }
    // No OTHER write to `var` anywhere in the body — a nested `i = …` inside
    // an `if` would break the monotone-step assumption.
    top_level_steps == 1 && count_writes(body, var) == 1
}

/// Names assigned somewhere in `body` via `name = name + 1`. A superset of the
/// qualifying accumulators; each is then validated by [`sole_increment_site`]
/// and its initializer.
fn candidate_accumulators(body: &Block) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    walk_stmts(body, &mut |stmt| {
        if let StmtKind::Assign { target, value } = &stmt.kind {
            if let ExprKind::Identifier(t) = &target.kind {
                if add_of_identifier_and_literal(value, t) == Some(1) && !names.contains(t) {
                    names.push(t.clone());
                }
            }
        }
    });
    names
}

/// The span of the ONLY write to `name` in `body`, when that write is exactly
/// `name = name + 1`. `None` when there is no such site or more than one write
/// of any kind (the uniqueness the `acc <= trip` bound depends on).
fn sole_increment_site(body: &Block, name: &str) -> Option<Span> {
    if count_writes(body, name) != 1 {
        return None;
    }
    let mut found = None;
    walk_stmts(body, &mut |stmt| {
        if let StmtKind::Assign { target, value } = &stmt.kind {
            if matches!(&target.kind, ExprKind::Identifier(t) if t == name)
                && add_of_identifier_and_literal(value, name) == Some(1)
            {
                found = Some(stmt.span.clone());
            }
        }
    });
    found
}

/// Count of writes to `name` anywhere in `body` — plain assignment, compound
/// assignment, or a `let` that shadows it. A shadowing `let` counts as a write
/// so the analysis fails closed rather than reasoning about scopes.
fn count_writes(body: &Block, name: &str) -> usize {
    let mut n = 0usize;
    walk_stmts(body, &mut |stmt| match &stmt.kind {
        StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
            if matches!(&target.kind, ExprKind::Identifier(t) if t == name) {
                n += 1;
            }
        }
        StmtKind::MultiAssign { targets, .. } => {
            for t in targets {
                if matches!(&t.kind, ExprKind::Identifier(x) if x == name) {
                    n += 1;
                }
            }
        }
        StmtKind::Let { pattern, .. } if binds_identifier(pattern) == Some(name) => {
            n += 1;
        }
        _ => {}
    });
    n
}

/// `K` when `e` is exactly `<var> + <integer literal K>` (or the commuted
/// `K + <var>`). Any other shape — a different left operand, a non-literal
/// right operand, a nested expression — yields `None`.
fn add_of_identifier_and_literal(e: &Expr, var: &str) -> Option<i64> {
    let (left, right) = match &e.kind {
        ExprKind::Binary {
            op: BinOp::Add,
            left,
            right,
        } => (left.as_ref(), right.as_ref()),
        // Lowered `Add::add(a, b)` form — the same rewrite that turns `i < n`
        // into a `Call` (see `ascending_guard_counter`).
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 || segments[1] != "add" || args.len() != 2 {
                return None;
            }
            (&args[0].value, &args[1].value)
        }
        _ => return None,
    };
    if matches!(&left.kind, ExprKind::Identifier(v) if v == var) {
        integer_literal(right)
    } else if matches!(&right.kind, ExprKind::Identifier(v) if v == var) {
        integer_literal(left)
    } else {
        None
    }
}

/// Visit every statement in `body`, descending into the statement-position
/// `if` / `while` / `for` / block expressions so a write hidden one level down
/// is still counted. Closures are handled separately and conservatively by
/// [`block_contains_closure`], which disqualifies the whole loop.
fn walk_stmts(body: &Block, f: &mut impl FnMut(&Stmt)) {
    for stmt in &body.stmts {
        f(stmt);
        if let StmtKind::Expr(e) = &stmt.kind {
            walk_expr_blocks(e, f);
        }
    }
}

/// Descend into every `Block` reachable from a statement-position expression.
fn walk_expr_blocks(e: &Expr, f: &mut impl FnMut(&Stmt)) {
    match &e.kind {
        ExprKind::While { body, .. } => walk_stmts(body, f),
        ExprKind::Block(b) => walk_stmts(b, f),
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            walk_stmts(then_block, f);
            if let Some(eb) = else_branch {
                walk_expr_blocks(eb, f);
            }
        }
        _ => {}
    }
}

/// Fail-closed shape gate: whether every expression reachable from `body` is
/// one this analysis understands well enough to guarantee it contains no
/// closure and no hidden write.
///
/// The default arm returns `false` (NOT analyzable), so any expression kind
/// added to the AST later automatically disqualifies the loop instead of
/// silently widening the elision. That matters because the one hole a
/// statement-shaped walk cannot see is an assignment inside an
/// expression-position closure — `let f = |_| { acc = acc + 1 };` — which
/// would break the one-increment-per-iteration bound the proof rests on.
fn body_is_analyzable(body: &Block) -> bool {
    body.stmts.iter().all(stmt_is_analyzable)
        && body
            .final_expr
            .as_ref()
            .is_none_or(|e| expr_is_analyzable(e))
}

fn stmt_is_analyzable(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } => expr_is_analyzable(value),
        StmtKind::Assign { target, value } => {
            expr_is_analyzable(target) && expr_is_analyzable(value)
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            expr_is_analyzable(target) && expr_is_analyzable(value)
        }
        StmtKind::Expr(e) => expr_is_analyzable(e),
        _ => false,
    }
}

fn expr_is_analyzable(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::Bool(_)
        | ExprKind::Identifier(_)
        | ExprKind::Path { .. } => true,
        ExprKind::Binary { left, right, .. } => {
            expr_is_analyzable(left) && expr_is_analyzable(right)
        }
        ExprKind::Unary { operand, .. } => expr_is_analyzable(operand),
        ExprKind::Cast { expr, .. } => expr_is_analyzable(expr),
        ExprKind::Index { object, index } => {
            expr_is_analyzable(object) && expr_is_analyzable(index)
        }
        ExprKind::FieldAccess { object, .. } => expr_is_analyzable(object),
        ExprKind::MethodCall { object, args, .. } => {
            expr_is_analyzable(object) && args.iter().all(|a| expr_is_analyzable(&a.value))
        }
        ExprKind::Call { callee, args } => {
            expr_is_analyzable(callee) && args.iter().all(|a| expr_is_analyzable(&a.value))
        }
        ExprKind::Block(b) => body_is_analyzable(b),
        ExprKind::While {
            condition, body, ..
        } => expr_is_analyzable(condition) && body_is_analyzable(body),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            expr_is_analyzable(condition)
                && body_is_analyzable(then_block)
                && else_branch.as_ref().is_none_or(|b| expr_is_analyzable(b))
        }
        // Everything else — closures above all, but also `for`, `match`,
        // `try`, spawn/par forms — is not modelled here. Fail closed.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Item;

    /// Parse `src` and return the body block of the single named function.
    fn fn_body(src: &str, name: &str) -> Block {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        for item in &parsed.program.items {
            if let Item::Function(f) = item {
                if f.name == name {
                    return f.body.clone();
                }
            }
        }
        panic!("fn {name} not found");
    }

    const COUNT_LOOP: &str = "\
fn count(nums: ref Vec[i64], b: i64) -> i64 {
    let mut cnt = 0i64;
    let mut i = 0i64;
    while i < nums.len() {
        if ((nums[i] >> b) & 1i64) == 1i64 {
            cnt = cnt + 1i64;
        }
        i = i + 1i64;
    }
    cnt
}
";

    #[test]
    fn recognizes_the_guarded_counting_loop() {
        let body = fn_body(COUNT_LOOP, "count");
        assert_eq!(
            check_free_accumulator_sites(&body).len(),
            1,
            "the #137 counting shape must yield exactly one elidable site"
        );
    }

    #[test]
    fn rejects_nonzero_accumulator_init() {
        let src = COUNT_LOOP.replace("let mut cnt = 0i64;", "let mut cnt = 1i64;");
        let body = fn_body(&src, "count");
        assert!(check_free_accumulator_sites(&body).is_empty());
    }

    #[test]
    fn rejects_two_increment_sites() {
        let src = COUNT_LOOP.replace(
            "            cnt = cnt + 1i64;\n",
            "            cnt = cnt + 1i64;\n        } else {\n            cnt = cnt + 1i64;\n",
        );
        let body = fn_body(&src, "count");
        assert!(
            check_free_accumulator_sites(&body).is_empty(),
            "two sites give acc <= 2*trip, which can overflow"
        );
    }

    #[test]
    fn rejects_step_larger_than_one() {
        let src = COUNT_LOOP.replace("cnt = cnt + 1i64;", "cnt = cnt + 2i64;");
        let body = fn_body(&src, "count");
        assert!(check_free_accumulator_sites(&body).is_empty());
    }

    #[test]
    fn rejects_negative_counter_start() {
        let src = COUNT_LOOP.replace("let mut i = 0i64;", "let mut i = -5i64;");
        let body = fn_body(&src, "count");
        assert!(check_free_accumulator_sites(&body).is_empty());
    }
}
