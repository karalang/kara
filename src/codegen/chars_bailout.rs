//! Fail-closed allowlist deciding whether a `for ch in s.chars()` body may be
//! emitted TWICE — the precondition for the dual-region ASCII-bailout loop in
//! [`super::control_flow_for::Codegen::compile_for_string_chars_bailout_inner`]
//! (B-2026-07-28-2).
//!
//! # Why duplication is required at all
//!
//! Recovering stride-1 induction in the ASCII stretch means the ASCII scan has
//! to be its own inner loop, and an inner loop that runs the user body needs
//! its own copy of that body; the multibyte path keeps the other. There is no
//! single-copy arrangement that also yields stride 1 — merging the two regions
//! reintroduces exactly the offset PHI (`off+1` vs. the decoder's return) that
//! blocks it. So the cost is inherent, and the question is only *when* paying
//! it is safe and worthwhile.
//!
//! # What this predicate is protecting against
//!
//! Not the loop's control flow — that is correct by construction (each copy
//! gets its own [`super::state::LoopFrame`], so `break` and `continue` resolve
//! per region). What it protects against is codegen whose *side effects are
//! keyed by identity rather than by position*: emit it twice and you get two
//! registrations where the program has one construct. The clearest case is
//! `Codegen::record_spawn_site`, which pushes a row into the module-level
//! `KARAC_SPAWN_SITES` table and mints a fresh `par_id` per call.
//!
//! Hence the shape of this module: an EXHAUSTIVE `match` with no wildcard
//! arm, listing every AST variant explicitly. Anything not positively
//! classified as duplicable returns `false` and the caller keeps the existing
//! single-copy loop, which is always correct — just slower. A new AST variant
//! breaks the build here rather than silently joining the fast path, which is
//! the same fail-closed discipline [`super::ascii_const_chars`] uses.
//!
//! # The rules
//!
//! 1. **No nested loops** (`for` / `while` / `while let` / `loop`). This one
//!    rule does most of the work:
//!    - it removes the auto-parallelism surface wholesale — reduction
//!      lowering and `par` fan-out only ever attach to a loop, so no
//!      spawn-site row can be minted twice;
//!    - it makes duplication non-recursive. A nested duplicable `.chars()`
//!      loop would double *inside* an already-doubled body, so `n` levels
//!      would cost `2^n` copies. With no nested loops the factor is exactly
//!      2, once;
//!    - it costs nothing real. If a `.chars()` body contains a loop, the
//!      `.chars()` loop is not the hot inner loop, so the shape this
//!      predicate gates has nothing to win there anyway.
//! 2. **No constructs with their own emission identity**: closures (each
//!    emits a module-level function), `comptime`, `par` / `seq` / `lock` /
//!    `providers`, `unsafe`, `try`, and `defer` / `errdefer` (which register
//!    deferred actions).
//! 3. **A node budget**, so that even an allowlisted body cannot double an
//!    unbounded amount of IR. Rule 1 already keeps qualifying bodies small;
//!    this is the backstop for a straight-line body that is merely long.
//!
//! Set `KARAC_CHARS_ASCII_BAILOUT=0` to force every `.chars()` loop back to
//! the single-copy lowering — the escape hatch for bisecting a suspected
//! miscompile, matching `KARAC_RC_ELIDE_REF_PARAMS=0`.

use crate::ast::{Block, Expr, ExprKind, ParsedInterpolationPart, Stmt, StmtKind};

/// Maximum number of expression/statement nodes a body may contain and still
/// be emitted twice. Rule 1 (no nested loops) already bounds realistic
/// qualifying bodies well below this; the cap exists so a long straight-line
/// body cannot silently double a large amount of IR.
const MAX_NODES: usize = 96;

/// True when `body` may be safely and affordably emitted twice.
pub(super) fn body_is_duplicable(body: &Block) -> bool {
    if !enabled() {
        return false;
    }
    let mut budget = MAX_NODES;
    block_ok(body, &mut budget)
}

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("KARAC_CHARS_ASCII_BAILOUT").as_deref() != Ok("0"))
}

/// Charge one node against the budget. Returns false once it is exhausted,
/// which propagates up as "not duplicable".
fn spend(budget: &mut usize) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    true
}

fn block_ok(b: &Block, budget: &mut usize) -> bool {
    if !spend(budget) {
        return false;
    }
    b.stmts.iter().all(|s| stmt_ok(s, budget))
        && b.final_expr.as_ref().is_none_or(|e| expr_ok(e, budget))
}

fn stmt_ok(s: &Stmt, budget: &mut usize) -> bool {
    if !spend(budget) {
        return false;
    }
    match &s.kind {
        StmtKind::Let { value, .. } => expr_ok(value, budget),
        StmtKind::LetUninit { .. } => true,
        StmtKind::LetElse {
            value, else_block, ..
        } => expr_ok(value, budget) && block_ok(else_block, budget),
        StmtKind::Assign { target, value } => expr_ok(target, budget) && expr_ok(value, budget),
        StmtKind::MultiAssign { targets, values } => {
            targets.iter().all(|t| expr_ok(t, budget)) && values.iter().all(|v| expr_ok(v, budget))
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            expr_ok(target, budget) && expr_ok(value, budget)
        }
        StmtKind::Expr(e) => expr_ok(e, budget),

        // Rule 2 — deferred actions are registered as codegen side effects,
        // so two copies would register two.
        StmtKind::Defer { .. } | StmtKind::ErrDefer { .. } => false,
    }
}

fn expr_ok(e: &Expr, budget: &mut usize) -> bool {
    if !spend(budget) {
        return false;
    }
    match &e.kind {
        // ── Leaves ────────────────────────────────────────────────────
        ExprKind::Integer(..)
        | ExprKind::Float(..)
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
        | ExprKind::OffsetOf { .. } => true,

        ExprKind::InterpolatedStringLit(parts) => parts.iter().all(|p| match p {
            ParsedInterpolationPart::Text(_) => true,
            ParsedInterpolationPart::Expr(inner, _) => expr_ok(inner, budget),
        }),

        // ── Pure operator / access trees ──────────────────────────────
        ExprKind::Binary { left, right, .. } => expr_ok(left, budget) && expr_ok(right, budget),
        ExprKind::Unary { operand, .. } => expr_ok(operand, budget),
        ExprKind::Question(inner) => expr_ok(inner, budget),
        ExprKind::NilCoalesce { left, right } => expr_ok(left, budget) && expr_ok(right, budget),
        ExprKind::Pipe { left, right } => expr_ok(left, budget) && expr_ok(right, budget),
        ExprKind::Cast { expr, .. } => expr_ok(expr, budget),
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            expr_ok(object, budget)
        }
        ExprKind::Index { object, index } => expr_ok(object, budget) && expr_ok(index, budget),
        ExprKind::Range { start, end, .. } => {
            start.as_ref().is_none_or(|s| expr_ok(s, budget))
                && end.as_ref().is_none_or(|s| expr_ok(s, budget))
        }

        // ── Calls ─────────────────────────────────────────────────────
        // A call emits a call instruction; the callee body is emitted once
        // for the whole module regardless of how many call sites reach it.
        ExprKind::Call { callee, args } => {
            expr_ok(callee, budget) && args.iter().all(|a| expr_ok(&a.value, budget))
        }
        ExprKind::MethodCall { object, args, .. } => {
            expr_ok(object, budget) && args.iter().all(|a| expr_ok(&a.value, budget))
        }
        ExprKind::OptionalChain { object, args, .. } => {
            expr_ok(object, budget)
                && args
                    .as_ref()
                    .is_none_or(|v| v.iter().all(|a| expr_ok(&a.value, budget)))
        }

        // ── Branching control flow (no loops — see rule 1) ────────────
        ExprKind::Block(b) => block_ok(b, budget),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            expr_ok(condition, budget)
                && block_ok(then_block, budget)
                && else_branch.as_ref().is_none_or(|e| expr_ok(e, budget))
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            expr_ok(value, budget)
                && block_ok(then_block, budget)
                && else_branch.as_ref().is_none_or(|e| expr_ok(e, budget))
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_ok(scrutinee, budget)
                && arms.iter().all(|a| {
                    spend(budget)
                        && a.guard.as_ref().is_none_or(|g| expr_ok(g, budget))
                        && expr_ok(&a.body, budget)
                })
        }
        ExprKind::LabeledBlock { body, .. } => block_ok(body, budget),

        // ── Jumps ─────────────────────────────────────────────────────
        // `break` / `continue` resolve against whichever region's LoopFrame
        // is live, which is exactly the per-copy retargeting the caller sets
        // up; `return` leaves the function from either copy.
        ExprKind::Return(v) => v.as_ref().is_none_or(|e| expr_ok(e, budget)),
        ExprKind::Break { value, .. } => value.as_ref().is_none_or(|e| expr_ok(e, budget)),
        ExprKind::Continue { .. } => true,

        // ── Composite literals ────────────────────────────────────────
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            items.iter().all(|i| expr_ok(i, budget))
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => items.iter().all(|i| expr_ok(i, budget)),
        ExprKind::RepeatLiteral { value, count, .. } => {
            expr_ok(value, budget) && expr_ok(count, budget)
        }
        ExprKind::MapLiteral(pairs) => pairs
            .iter()
            .all(|(k, v)| expr_ok(k, budget) && expr_ok(v, budget)),
        ExprKind::StructLiteral { fields, spread, .. } => {
            fields.iter().all(|f| expr_ok(&f.value, budget))
                && spread.as_ref().is_none_or(|s| expr_ok(s, budget))
        }

        // ── Rule 1: nested loops disqualify ───────────────────────────
        // Keeps auto-par / `par` reduction lowering (the spawn-site minting
        // surface) out of duplicated code, and keeps duplication from
        // compounding through nested `.chars()` loops. Costs nothing: a
        // `.chars()` loop containing a loop is not the hot inner loop.
        ExprKind::For { .. }
        | ExprKind::While { .. }
        | ExprKind::WhileLet { .. }
        | ExprKind::Loop { .. } => false,

        // ── Rule 2: constructs with their own emission identity ───────
        // Closures emit a module-level function per occurrence; `par` / `seq`
        // mint spawn sites; `comptime` runs the compile-time evaluator;
        // `lock` / `providers` / `unsafe` / `try` open regions whose lowering
        // is not worth reasoning about twice for a perf win.
        ExprKind::Closure { .. }
        | ExprKind::Comptime(_)
        | ExprKind::Unsafe(_)
        | ExprKind::Try(_)
        | ExprKind::Seq(_)
        | ExprKind::Par(_)
        | ExprKind::Lock { .. }
        | ExprKind::Providers { .. } => false,

        // Parse-error placeholder — codegen never reaches a valid program
        // through here, but fail closed rather than assume.
        ExprKind::Error => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(src: &str) -> Block {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "probe must parse: {:?}",
            parsed.errors
        );
        // The probe shape is always `fn main() { for ch in s.chars() { .. } }`;
        // dig out the for-loop's body.
        for item in &parsed.program.items {
            if let crate::ast::Item::Function(f) = item {
                for stmt in &f.body.stmts {
                    if let StmtKind::Expr(e) = &stmt.kind {
                        if let ExprKind::For { body, .. } = &e.kind {
                            return body.clone();
                        }
                    }
                }
            }
        }
        panic!("probe program has no top-level for loop");
    }

    fn duplicable(loop_body: &str) -> bool {
        body_is_duplicable(&body_of(&format!(
            "fn main() {{\n    let s: String = \"hi\";\n    let mut n = 0i64;\n    \
             for ch in s.chars() {{\n{loop_body}\n    }}\n    print(n);\n}}\n"
        )))
    }

    #[test]
    fn ordinary_hot_bodies_qualify() {
        assert!(duplicable("        n = n + 1i64;"));
        assert!(duplicable(
            "        if ch == 'a' { n = n + 1i64; } else { n = n - 1i64; }"
        ));
        assert!(duplicable(
            "        let mut out: String = \"\"; out.push(ch);"
        ));
        assert!(duplicable(
            "        match ch { 'a' => { n = n + 1i64; }, _ => { n = n + 2i64; } }"
        ));
        // break / continue / labeled continue are the retargeting cases the
        // dual-region lowering exists to handle, so they must NOT disqualify.
        assert!(duplicable(
            "        if ch == 'a' { continue; } n = n + 1i64;"
        ));
        assert!(duplicable("        if ch == 'a' { break; } n = n + 1i64;"));
        assert!(duplicable("        if ch == 'a' { return; } n = n + 1i64;"));
    }

    #[test]
    fn rule_1_nested_loops_disqualify() {
        // Not a correctness cliff — the single-copy loop is always valid — but
        // this is what keeps spawn-site minting out of duplicated code and
        // stops duplication compounding to 2^depth.
        assert!(!duplicable(
            "        let mut i = 0i64; while i < 3i64 { n = n + 1i64; i = i + 1i64; }"
        ));
        assert!(!duplicable("        for c2 in s.chars() { n = n + 1i64; }"));
        assert!(!duplicable("        for i in 0i64..3i64 { n = n + 1i64; }"));
    }

    #[test]
    fn rule_2_identity_bearing_constructs_disqualify() {
        assert!(!duplicable("        let f = |x: i64| x + 1i64; n = f(n);"));
        assert!(!duplicable("        unsafe { n = n + 1i64; }"));
        assert!(!duplicable("        seq { n = n + 1i64; }"));
        assert!(!duplicable("        defer { n = n + 1i64; }"));
    }

    #[test]
    fn rule_3_the_node_budget_is_a_real_backstop() {
        // A straight-line body long enough to blow the cap must fall back even
        // though every individual node is allowlisted.
        let long = "        n = n + 1i64;\n".repeat(MAX_NODES);
        assert!(!duplicable(&long));
        // ...and something comfortably under it must not.
        assert!(duplicable(&"        n = n + 1i64;\n".repeat(4)));
    }

    #[test]
    fn the_budget_cannot_be_evaded_by_nesting() {
        // Depth, not just statement count, has to be charged — otherwise ONE
        // statement holding a huge expression tree would duplicate unbounded
        // IR while looking tiny by statement count. Charging per node also
        // bounds this walker's own recursion depth by `MAX_NODES`, so it
        // cannot overflow the stack on a pathological input.
        let wide = format!("        n = 0i64{};", " + 1i64".repeat(MAX_NODES * 2));
        assert!(!duplicable(&wide));
        assert!(duplicable("        n = 0i64 + 1i64 + 1i64 + 1i64;"));
    }
}
