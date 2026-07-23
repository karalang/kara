// src/concurrency.rs

//! Concurrency analysis pass for the Kāra language.
//!
//! Analyzes function bodies to identify which statements can safely run in
//! parallel by building a dual-analysis dependency graph:
//! 1. **Data dependency**: if statement B reads a variable that A defines, B depends on A
//! 2. **Effect conflict**: if A and B have conflicting effects on the same resource, they
//!    must serialize
//!
//! Only when BOTH analyses find no dependency can statements be parallelized.

use crate::ast::*;
use crate::effectchecker::{DeclaredEffects, EffectCheckResult, EffectSet};
use crate::resolver::SpanKey;
use crate::typechecker::TypeCheckResult;
use std::collections::{HashMap, HashSet};

/// True when an `EffectSet` contains any verb that implies side effects
/// beyond a pure read — used by `method_effects_imply_receiver_mutation`
/// to decide whether a method call should mark its receiver as written
/// for data-dependency reasoning.
fn effect_set_has_nonpure_verb(set: &EffectSet) -> bool {
    use EffectVerbKind::*;
    set.effects.iter().any(|te| {
        matches!(
            te.effect.verb,
            Writes | Allocates | Sends | Receives | Panics | UserDefined(_)
        )
    })
}

/// `true` iff this statement does ~zero work — a `let`/`assign` whose
/// RHS is a literal or bare identifier, or a `let uninit` (which only
/// allocates an empty stack slot). The classification is structural
/// (not effect-based) so a side-effecting RHS like `let x = call()`
/// is NOT considered constant-init even when `call()` is pure.
///
/// Used by `find_parallel_groups`'s cost-model gate: a parallel
/// group where N−1 of N stmts are constant-init can produce no
/// parallelism (one branch holds all the work, the others idle) so
/// the `karac_par_run` spawn cost is pure overhead. Marking those
/// groups trivial routes them through sequential codegen instead.
/// See `StmtInfo::is_constant_init` for the failure-mode this
/// closes.
fn stmt_is_constant_init(stmt: &Stmt) -> bool {
    let value = match &stmt.kind {
        StmtKind::Let { value, .. } => value,
        StmtKind::Assign { target: _, value } => value,
        StmtKind::LetUninit { .. } => return true,
        _ => return false,
    };
    expr_is_constant_init(value)
}

/// `true` iff `expr` is a literal-init form that does ~zero work — a scalar
/// literal, an identifier read, or a **source-bounded** composite literal
/// (`[a, b, c]`, `Vec[..]`, `(a, b)`, `{k: v}`) whose every element is itself
/// constant-init. The trivial-group filter in `find_parallel_groups` uses this
/// to recognize sibling stmts that wouldn't benefit from `karac_par_run`'s
/// ~70μs spawn cost.
///
/// Surfaced 2026-05-22 by the kata-91 bench: `let zero: u8 = b'0';` was
/// mis-classified as non-constant because `ByteLit` was missing, pushing
/// `non_constant_count` over the `<= 1` threshold and emitting a par-block for
/// a (`let l = N; let zero = b'0'; let buf = Vec.new(); let j = 0;`) prologue —
/// the captured `l` then became an opaque load and LLVM lost the const-prop
/// into `k % l` (~47ms on a 10M-iter hot loop). `MultiStringLit` is parity with
/// `StringLit`.
///
/// The composite-literal recursion was added 2026-06-14 (auto-par ordered-
/// output corpus probe): once output suppression was removed, test-harness
/// mains shaped `report("ex1", ex1); let ex2 = ["..", ".."]; report("ex2", ex2);`
/// fanned out a par-block per (`report`, `let exN = [literals]`) pair — but a
/// literal array build is ~zero work, so the group held only ONE substantial
/// branch (the `report` call) and the fan-out bought no speedup, just spawn
/// overhead + binary growth. Recognizing the literal array as constant-init
/// drops `non_constant_count` to 1 → the group is trivial → inlined. A
/// collection literal whose elements DO work (`[f(), g()]`) recurses to
/// non-constant and stays parallelizable; `RepeatLiteral` (`[v; n]`) is
/// deliberately excluded — its count is an expression, so it can be an O(n)
/// fill worth overlapping with a sibling computation.
fn expr_is_constant_init(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Identifier(_) => true,
        ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
            elems.iter().all(expr_is_constant_init)
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => items.iter().all(expr_is_constant_init),
        ExprKind::MapLiteral(pairs) => pairs
            .iter()
            .all(|(k, v)| expr_is_constant_init(k) && expr_is_constant_init(v)),
        // Empty-collection constructors — `Vec.new()` / `String.new()` — do
        // ~zero work: they materialize an empty `{ptr, len, cap}` descriptor
        // with NO heap allocation (the first `push`/grow is what allocates).
        // The `allocates(Heap)` effect the constructor carries is conservative
        // for that *potential later* growth, not for the constructor itself, so
        // for the cost model an empty `Vec.new()` is constant-init exactly like
        // a literal. Without this, a hot prologue like
        // `let n = x & M; let buf = Vec.new();` counts TWO non-constant stmts,
        // clears the `<= 1` trivial gate in `find_parallel_groups`, and fans out
        // a ~70μs-spawn par group *per call* — e.g. kata #405 `to_hex`, whose
        // default (auto-par) build blew up 40–66× instructions (and
        // non-deterministically) vs its `KARAC_AUTO_PAR=0` seq lane
        // (B-2026-07-09-14). Matches this filter's case-2 rationale: an empty
        // constructor is never the "work" branch, so overlapping it with a
        // sibling computation buys only spawn overhead, never speedup. Only the
        // zero-arg `new` of the two genuinely lazy/empty collections is
        // recognized — `Map.new()` / `Set.new()` may allocate an initial table,
        // so they are deliberately excluded (conservative).
        ExprKind::Call { callee, args } if args.is_empty() => matches!(
            &callee.kind,
            ExprKind::Path { segments, .. }
                if segments.len() == 2
                    && segments[1] == "new"
                    && matches!(segments[0].as_str(), "Vec" | "String")
        ),
        _ => false,
    }
}

/// `true` iff this statement contains a `return`, `break`, or
/// `continue` that escapes a directly-nested expression's control flow
/// — i.e., that would, at codegen time, emit a `ret X` (or branch to a
/// loop's exit edge) bypassing the statement's "fall through" exit.
/// Used by `find_parallel_groups` to keep such statements out of
/// par groups; a par branch is lowered to a standalone `void` LLVM
/// function and an embedded `return X` from the original body would
/// produce `ret <T> X` inside the void branch and fail LLVM module
/// verification.
fn stmt_has_early_exit(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. }
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. }
        | StmtKind::Expr(value) => expr_has_early_exit(value),
        StmtKind::LetElse {
            value, else_block, ..
        } => expr_has_early_exit(value) || block_has_early_exit(else_block),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => block_has_early_exit(body),
        StmtKind::LetUninit { .. } => false,
    }
}

/// True when `block` contains a `return` / `break` / `continue` that would
/// transfer control out of it. Used (via `stmt_has_early_exit`) by
/// `find_parallel_groups` to keep such statements out of par groups.
fn block_has_early_exit(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_early_exit)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_early_exit(e))
}

fn expr_has_early_exit(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Return(_) => true,
        ExprKind::Break { .. } => true,
        ExprKind::Continue { .. } => true,
        ExprKind::Block(b) => block_has_early_exit(b),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            expr_has_early_exit(condition)
                || block_has_early_exit(then_block)
                || else_branch.as_ref().is_some_and(|e| expr_has_early_exit(e))
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            expr_has_early_exit(value)
                || block_has_early_exit(then_block)
                || else_branch.as_ref().is_some_and(|e| expr_has_early_exit(e))
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_has_early_exit(scrutinee) || arms.iter().any(|a| expr_has_early_exit(&a.body))
        }
        ExprKind::While {
            condition, body, ..
        } => expr_has_early_exit(condition) || block_has_early_exit(body),
        ExprKind::For { iterable, body, .. } => {
            expr_has_early_exit(iterable) || block_has_early_exit(body)
        }
        ExprKind::Loop { body, .. } => block_has_early_exit(body),
        ExprKind::Binary { left, right, .. }
        | ExprKind::Pipe { left, right }
        | ExprKind::NilCoalesce { left, right } => {
            expr_has_early_exit(left) || expr_has_early_exit(right)
        }
        ExprKind::Unary { operand, .. } => expr_has_early_exit(operand),
        ExprKind::Call { callee, args } => {
            expr_has_early_exit(callee) || args.iter().any(|a| expr_has_early_exit(&a.value))
        }
        ExprKind::MethodCall { object, args, .. } => {
            expr_has_early_exit(object) || args.iter().any(|a| expr_has_early_exit(&a.value))
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            expr_has_early_exit(object)
        }
        ExprKind::Index { object, index } => {
            expr_has_early_exit(object) || expr_has_early_exit(index)
        }
        ExprKind::Tuple(elems) => elems.iter().any(expr_has_early_exit),
        _ => false,
    }
}

/// `true` iff the function body contains a user `defer` / `errdefer`
/// statement at ANY nesting depth. Used by [`ConcurrencyAnalyzer::analyze_function`]
/// to BAIL the whole function's auto-parallelization to sequential codegen
/// (B-2026-07-16-10).
///
/// Rationale: user `defer` semantics — reverse-declaration-order (LIFO) at
/// scope exit, design.md § *defer* — are NOT preserved by the auto-par
/// whole-function lowering. When any statement in the body forms a parallel
/// group (or a reduction), the entire body is lowered through the `par_run`
/// wrapper, and function-scope `defer` blocks are then materialized in-place
/// (FIFO, at their declaration point) instead of being registered on the true
/// function-scope cleanup frame — so they run before the sequential remainder
/// of the body and in the wrong order (a use-after-cleanup hazard for a
/// resource-releasing defer). Auto-par is only an optimization: falling back
/// to the sequential lowering (which drains defers LIFO correctly) is always
/// sound. Explicit `par {}` (`compile_par_block`) is a separate path and is
/// unaffected — it drains defers correctly and is not gated by this analysis.
fn block_has_user_defer(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_user_defer)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_user_defer(e))
}

fn stmt_has_user_defer(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Defer { .. } | StmtKind::ErrDefer { .. } => true,
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. }
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. }
        | StmtKind::Expr(value) => expr_has_user_defer(value),
        StmtKind::LetElse {
            value, else_block, ..
        } => expr_has_user_defer(value) || block_has_user_defer(else_block),
        StmtKind::LetUninit { .. } => false,
    }
}

fn expr_has_user_defer(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Block(b) => block_has_user_defer(b),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            expr_has_user_defer(condition)
                || block_has_user_defer(then_block)
                || else_branch.as_ref().is_some_and(|e| expr_has_user_defer(e))
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            expr_has_user_defer(value)
                || block_has_user_defer(then_block)
                || else_branch.as_ref().is_some_and(|e| expr_has_user_defer(e))
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_has_user_defer(scrutinee) || arms.iter().any(|a| expr_has_user_defer(&a.body))
        }
        ExprKind::While {
            condition, body, ..
        } => expr_has_user_defer(condition) || block_has_user_defer(body),
        ExprKind::For { iterable, body, .. } => {
            expr_has_user_defer(iterable) || block_has_user_defer(body)
        }
        ExprKind::Loop { body, .. } => block_has_user_defer(body),
        ExprKind::Binary { left, right, .. }
        | ExprKind::Pipe { left, right }
        | ExprKind::NilCoalesce { left, right } => {
            expr_has_user_defer(left) || expr_has_user_defer(right)
        }
        ExprKind::Unary { operand, .. } => expr_has_user_defer(operand),
        ExprKind::Call { callee, args } => {
            expr_has_user_defer(callee) || args.iter().any(|a| expr_has_user_defer(&a.value))
        }
        ExprKind::MethodCall { object, args, .. } => {
            expr_has_user_defer(object) || args.iter().any(|a| expr_has_user_defer(&a.value))
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            expr_has_user_defer(object)
        }
        ExprKind::Index { object, index } => {
            expr_has_user_defer(object) || expr_has_user_defer(index)
        }
        ExprKind::Tuple(elems) => elems.iter().any(expr_has_user_defer),
        _ => false,
    }
}

/// `true` iff this statement performs a channel operation — `Channel.new()`,
/// or a `Sender.send` / `Receiver.recv` / `Receiver.try_recv` method call
/// anywhere in its expression tree. Used by `find_parallel_groups` to keep
/// channel-bearing statements out of auto-par groups.
///
/// Channels are explicit concurrency/communication primitives: a `send` must
/// happen-before the matching `recv` for the value to transfer, but `send`
/// (`allocates(Heap)`) and `recv` (`suspends`) carry no mutually-conflicting
/// resource effect, so the effect-conflict gate treats them as independent
/// and would fan them into separate `__par_branch` workers — reordering the
/// communication (the non-blocking floor's `recv` would observe an empty
/// queue) AND isolating the channel-end bindings into the branch's captured
/// variable scope. Auto-par is a compute optimization; it must never relocate
/// a channel op. This AST-level guard catches the cases the effect-based
/// `effects_mark_coroutine_boundary` (`suspends`) misses — `send`'s
/// `allocates`-only effect, and a `recv` whose method-call effect didn't
/// resolve (e.g. nested inside `println(rx.recv())`).
fn stmt_has_channel_op(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. }
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. }
        | StmtKind::Expr(value) => expr_has_channel_op(value),
        StmtKind::LetElse {
            value, else_block, ..
        } => expr_has_channel_op(value) || block_has_channel_op(else_block),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => block_has_channel_op(body),
        StmtKind::LetUninit { .. } => false,
    }
}

fn block_has_channel_op(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_channel_op)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_channel_op(e))
}

fn expr_has_channel_op(expr: &Expr) -> bool {
    match &expr.kind {
        // `Channel.new()` — the constructor (a 2-segment `Channel.new` path
        // callee).
        ExprKind::Call { callee, args } => {
            let is_channel_new = matches!(
                &callee.kind,
                ExprKind::Path { segments, .. }
                    if segments.len() == 2 && segments[0] == "Channel" && segments[1] == "new"
            );
            is_channel_new
                || expr_has_channel_op(callee)
                || args.iter().any(|a| expr_has_channel_op(&a.value))
        }
        // `tx.send(..)` / `rx.recv()` / `rx.try_recv()`. The bare method
        // names are channel-specific (network types use `send_text` /
        // `recv_text`); even if a user type reused one, excluding its
        // statement from auto-par only forfeits a compute optimization.
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            matches!(method.as_str(), "send" | "recv" | "try_recv")
                || expr_has_channel_op(object)
                || args.iter().any(|a| expr_has_channel_op(&a.value))
        }
        ExprKind::Block(b) => block_has_channel_op(b),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            expr_has_channel_op(condition)
                || block_has_channel_op(then_block)
                || else_branch.as_ref().is_some_and(|e| expr_has_channel_op(e))
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            expr_has_channel_op(value)
                || block_has_channel_op(then_block)
                || else_branch.as_ref().is_some_and(|e| expr_has_channel_op(e))
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_has_channel_op(scrutinee) || arms.iter().any(|a| expr_has_channel_op(&a.body))
        }
        ExprKind::While {
            condition, body, ..
        } => expr_has_channel_op(condition) || block_has_channel_op(body),
        ExprKind::For { iterable, body, .. } => {
            expr_has_channel_op(iterable) || block_has_channel_op(body)
        }
        ExprKind::Loop { body, .. } => block_has_channel_op(body),
        ExprKind::Binary { left, right, .. }
        | ExprKind::Pipe { left, right }
        | ExprKind::NilCoalesce { left, right } => {
            expr_has_channel_op(left) || expr_has_channel_op(right)
        }
        ExprKind::Unary { operand, .. } => expr_has_channel_op(operand),
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            expr_has_channel_op(object)
        }
        ExprKind::Index { object, index } => {
            expr_has_channel_op(object) || expr_has_channel_op(index)
        }
        ExprKind::Tuple(elems) => elems.iter().any(expr_has_channel_op),
        _ => false,
    }
}

/// True iff `stmt` *syntactically* performs console output (`println` /
/// `print` / `eprintln` / `eprint`) at its own expression level. Used only
/// to keep such statements out of the reorder-opportunity advisory:
/// relocating a console write changes observable output order, which
/// `query effects` would not catch (console output is resourceless by
/// design — see the auto-par ordered-output note in `find_parallel_groups`).
///
/// This is a best-effort **local** filter, not a soundness guarantee — it
/// detects a direct console call in the statement's own expression tree but
/// not output emitted transitively inside a called function (the same
/// resourceless-console limitation the rest of the pass carries). The
/// reorder advisory is scoped to data + resource-effect dependencies; the
/// agent's verify loop is the backstop for observable-order changes. See the
/// reorder-opportunity entry in phase-5-diagnostics.md.
fn stmt_has_console_output(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. }
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. }
        | StmtKind::Expr(value) => expr_has_console_output(value),
        StmtKind::LetElse {
            value, else_block, ..
        } => expr_has_console_output(value) || block_has_console_output(else_block),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            block_has_console_output(body)
        }
        StmtKind::LetUninit { .. } => false,
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
    }
}

fn block_has_console_output(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_console_output)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_console_output(e))
}

fn expr_has_console_output(expr: &Expr) -> bool {
    /// `println` / `print` / `eprintln` / `eprint` — the console-writing
    /// builtins whose call ordering is observable. A bare free-function
    /// callee parses as either an `Identifier` or a single-segment `Path`.
    fn is_console_callee(callee: &Expr) -> bool {
        let name = match &callee.kind {
            ExprKind::Identifier(name) => Some(name.as_str()),
            ExprKind::Path { segments, .. } if segments.len() == 1 => Some(segments[0].as_str()),
            _ => None,
        };
        matches!(name, Some("println" | "print" | "eprintln" | "eprint"))
    }

    match &expr.kind {
        ExprKind::Call { callee, args } => {
            is_console_callee(callee)
                || expr_has_console_output(callee)
                || args.iter().any(|a| expr_has_console_output(&a.value))
        }
        ExprKind::MethodCall { object, args, .. } => {
            expr_has_console_output(object)
                || args.iter().any(|a| expr_has_console_output(&a.value))
        }
        ExprKind::Block(b) => block_has_console_output(b),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            expr_has_console_output(condition)
                || block_has_console_output(then_block)
                || else_branch
                    .as_ref()
                    .is_some_and(|e| expr_has_console_output(e))
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            expr_has_console_output(value)
                || block_has_console_output(then_block)
                || else_branch
                    .as_ref()
                    .is_some_and(|e| expr_has_console_output(e))
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_has_console_output(scrutinee)
                || arms.iter().any(|a| expr_has_console_output(&a.body))
        }
        ExprKind::While {
            condition, body, ..
        } => expr_has_console_output(condition) || block_has_console_output(body),
        ExprKind::For { iterable, body, .. } => {
            expr_has_console_output(iterable) || block_has_console_output(body)
        }
        ExprKind::Loop { body, .. } => block_has_console_output(body),
        ExprKind::Binary { left, right, .. }
        | ExprKind::Pipe { left, right }
        | ExprKind::NilCoalesce { left, right } => {
            expr_has_console_output(left) || expr_has_console_output(right)
        }
        ExprKind::Unary { operand, .. } => expr_has_console_output(operand),
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            expr_has_console_output(object)
        }
        ExprKind::Index { object, index } => {
            expr_has_console_output(object) || expr_has_console_output(index)
        }
        ExprKind::Tuple(elems) => elems.iter().any(expr_has_console_output),
        _ => false,
    }
}

/// Whether a statement may participate in the reorder-opportunity advisory —
/// the same parallel-eligibility guards `find_parallel_groups` applies to a
/// group seed, plus a console-output exclusion (a console write must not be
/// proposed as a mover; relocating it reorders observable output). A
/// statement failing any guard can never auto-parallelize, so co-locating it
/// with a sibling would be pointless. See
/// [`ConcurrencyChecker::find_reorder_opportunities`].
fn reorder_eligible(info: &StmtInfo) -> bool {
    !info.has_early_exit
        && !info.has_channel_op
        && !info.has_console_output
        && !info.is_seq
        && (!effects_mark_coroutine_boundary(&info.effects)
            || info.is_timer_suspend
            || info.is_safe_network_fanout)
}

// ── Result Types ───────────────────────────────────────────────

/// The full result of concurrency analysis across all functions.
#[derive(Debug, Clone)]
pub struct ConcurrencyAnalysis {
    /// Per-function parallelization decisions.
    pub function_decisions: HashMap<String, FunctionConcurrency>,
    /// Phase-8 stdlib-floor § Compiler queries channel sub-item 2.
    /// Empty in v1; future P1.6 catalogue entry (auto-concurrency
    /// fork threshold) pushes `CompilerQuery` values here.
    pub queries: Vec<crate::queries::CompilerQuery>,
}

/// Parallelization analysis for a single function.
#[derive(Debug, Clone)]
pub struct FunctionConcurrency {
    /// Groups of statement indices that can run in parallel.
    pub parallel_groups: Vec<ParallelGroup>,
    /// Total statements analyzed.
    pub total_statements: usize,
    /// Source span of each top-level body statement, indexed by the same
    /// ordinal used in `parallel_groups[].statement_indices` and
    /// `serialization_points[].statement_indices` (so `statement_spans[i]`
    /// locates statement `i`). Length is always `total_statements`. The
    /// ordinal stays the stable key for agents/diffs; this array makes the
    /// machine surface self-locating for IDE/LSP decoration and human
    /// reports without re-deriving positions by counting statements. See
    /// phase-5-diagnostics.md "Self-locating query output".
    pub statement_spans: Vec<crate::token::Span>,
    /// Top-level loops in the function body whose only loop-carried write
    /// is a reduction over an outer-scope accumulator with an op in the
    /// associative + commutative allow-list. Codegen consumes this list
    /// to lower the loop as a fan-out + reduce: each worker processes a
    /// contiguous slice of the iteration space into a per-thread partial,
    /// then a final serial pass combines the partials with the same op.
    /// See `docs/implementation_checklist/phase-7-codegen.md` — "Auto-par
    /// reduction recognition" — for the policy and slicing plan.
    pub loop_reductions: Vec<LoopReduction>,
    /// The statement pairs that *can't* run in parallel, and why — the
    /// inverse of `parallel_groups`. Each records the conflicting
    /// statement indices, a human reason, the resource at issue (empty
    /// for a data/ordering conflict), and — for an effect conflict — the
    /// callees whose effect on that resource forced the serialization
    /// (`blocking_callees`). Inverting `blocking_callees` across all
    /// functions answers "which callers does function `f` block, and on
    /// what resource" — the Cartographer attribution view.
    pub serialization_points: Vec<SerializationPoint>,
    /// Independent statement pairs the contiguous-only grouper could not
    /// co-group *only because they are non-adjacent in source order* — a
    /// legal reorder (permitted by the data + effect dependency graph)
    /// would make them adjacent and let them parallelize. Each names the two
    /// ordinals and which one can slide. This is the deterministic "a better
    /// order exists" signal for the agent-driven reorder loop (option 1):
    /// the agent acts on a sound dependency signal instead of guessing, then
    /// re-runs `check` / `query` to confirm it helped and broke nothing. See
    /// phase-5-diagnostics.md "Contiguous-greedy grouping is suboptimal".
    pub reorder_opportunities: Vec<ReorderOpportunity>,
}

/// A pair of independent statements left unparallelized only by source
/// ordering, surfaced by [`ConcurrencyChecker::find_reorder_opportunities`].
/// See [`FunctionConcurrency::reorder_opportunities`].
#[derive(Debug, Clone)]
pub struct ReorderOpportunity {
    /// The two independent statement ordinals, ascending. Index into
    /// `statement_spans` to locate them.
    pub statement_indices: Vec<usize>,
    /// The ordinal (one of `statement_indices`) that can legally slide
    /// adjacent to its partner — every statement it passes over is
    /// dependency-independent of it, so the move preserves data + effect
    /// ordering. The advisory reports the move but does not apply it.
    pub movable_statement: usize,
    /// Human-readable explanation, e.g. ``statements 0 and 2 are
    /// independent but separated by statement 1; moving statement 2 adjacent
    /// would let them parallelize``.
    pub reason: String,
}

/// One reason two statements in a function body can't run in parallel —
/// the inverse of a [`ParallelGroup`]. See
/// [`FunctionConcurrency::serialization_points`].
#[derive(Debug, Clone)]
pub struct SerializationPoint {
    /// The two conflicting statement indices, ascending.
    pub statement_indices: Vec<usize>,
    /// Human-readable cause, e.g. `"writes(AuditLog) conflicts with
    /// writes(AuditLog)"`, `"data dependency on `x`"`, `"explicit seq
    /// ordering"`.
    pub reason: String,
    /// The resource at issue for an effect conflict (e.g. `"AuditLog"`);
    /// empty for a data-dependency / write-write / ordering conflict.
    pub resource: String,
    /// For an effect conflict: the callee keys (`fn` / `Type.method`)
    /// whose effect on `resource` caused the conflict. Empty for
    /// non-effect conflicts. Sorted + deduped.
    pub blocking_callees: Vec<String>,
    /// Structured, machine-readable counterpart to `reason`: *which axis*
    /// forced this serialization. Lets a consumer branch on the conflict
    /// class without parsing the prose `reason` — a data dependency and an
    /// effect conflict imply different fixes (break the dataflow vs split
    /// the resource), and the human string alone hides the distinction
    /// when two pairs read byte-identical on the effect surface. See
    /// phase-5-diagnostics.md "Per-statement exclusion-reason attribution".
    pub cause: SerializationCause,
}

/// Structured attribution of *which axis* serialized a statement pair —
/// the discriminated counterpart to [`SerializationPoint::reason`].
#[derive(Debug, Clone)]
pub enum SerializationCause {
    /// One of the two statements is inside a `seq {}` block — explicit
    /// user-requested ordering, not a discovered dependency.
    SeqOrdering,
    /// A local-binding dependency between the two statements. `vars` lists
    /// the bindings at issue (sorted, deduped); `kind` records the
    /// dependency direction.
    DataDependency {
        kind: DataDepKind,
        vars: Vec<String>,
    },
    /// A `with _` polymorphic-effect call whose effects are unknown at
    /// analysis time, forcing a conservative serialization.
    PolymorphicEffect,
    /// A resource-level effect conflict: both `verbs` act on `resource`.
    EffectConflict {
        resource: String,
        verbs: (EffectVerbKind, EffectVerbKind),
    },
}

/// Direction of a [`SerializationCause::DataDependency`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataDepKind {
    /// Read-after-write: the later statement reads a binding the earlier
    /// one writes — a true (flow) dependency.
    Raw,
    /// Write-after-read: the later statement writes a binding the earlier
    /// one reads — an anti-dependency.
    War,
    /// Both statements write the same binding — an output dependency.
    WriteWrite,
}

impl DataDepKind {
    /// Lowercase wire tag used in the structured query output.
    pub fn as_str(&self) -> &'static str {
        match self {
            DataDepKind::Raw => "raw",
            DataDepKind::War => "war",
            DataDepKind::WriteWrite => "ww",
        }
    }
}

/// An associative + commutative reduction operator recognized at v1.
/// Int-only allow-list per the roadmap entry; float `+`/`*` are deferred
/// to v1.x behind an `#[fp_reassoc]` opt-in because IEEE-754 addition is
/// not associative and per-thread combine order would break determinism.
///
/// `Collect` is a different reduction kind from the scalar ops: it
/// represents a Vec/String/Buffer accumulator that *collects* per-iter
/// contributions via `acc.push(x)` rather than scalar-folding. The
/// combine model concatenates per-worker partial buffers, which produces
/// worker-order output (not iteration-order). For this reason the
/// analyzer only recognizes `Collect` when the enclosing loop carries
/// the `#[par_unordered]` attribute — an explicit user opt-in to the
/// unordered-output property. See `phase-7-codegen.md` collect-style
/// reduction entry for the full design + slice plan. Codegen lowering
/// is Phase 3 and not yet implemented; for now `try_emit_reduction_lowering`
/// returns `Ok(None)` on a `Collect` reduction and the loop falls back
/// to sequential codegen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReductionOp {
    Add,
    Mul,
    BitOr,
    BitAnd,
    BitXor,
    Min,
    Max,
    Collect,
}

impl ReductionOp {
    /// Source-level glyph for the op, used in `--concurrency-report`
    /// output and in diagnostic messages.
    pub fn symbol(&self) -> &'static str {
        match self {
            ReductionOp::Add => "+",
            ReductionOp::Mul => "*",
            ReductionOp::BitOr => "|",
            ReductionOp::BitAnd => "&",
            ReductionOp::BitXor => "^",
            ReductionOp::Min => "min",
            ReductionOp::Max => "max",
            ReductionOp::Collect => "collect",
        }
    }

    fn from_bin_op(op: &BinOp) -> Option<Self> {
        match op {
            BinOp::Add => Some(ReductionOp::Add),
            BinOp::Mul => Some(ReductionOp::Mul),
            BinOp::BitOr => Some(ReductionOp::BitOr),
            BinOp::BitAnd => Some(ReductionOp::BitAnd),
            BinOp::BitXor => Some(ReductionOp::BitXor),
            _ => None,
        }
    }

    fn from_compound_op(op: &CompoundOp) -> Option<Self> {
        match op {
            CompoundOp::Add => Some(ReductionOp::Add),
            CompoundOp::Mul => Some(ReductionOp::Mul),
            CompoundOp::BitOr => Some(ReductionOp::BitOr),
            CompoundOp::BitAnd => Some(ReductionOp::BitAnd),
            CompoundOp::BitXor => Some(ReductionOp::BitXor),
            _ => None,
        }
    }
}

/// A loop body recognized as a reduction over a single accumulator.
/// `stmt_index` identifies the top-level loop statement in the
/// enclosing function's body; `loop_line` is the loop expression's
/// 1-indexed source line, suitable for the report's user-facing text.
#[derive(Debug, Clone)]
pub struct LoopReduction {
    pub accumulator: String,
    pub op: ReductionOp,
    pub stmt_index: usize,
    pub loop_line: usize,
    /// Collect-only: the body pushes EXACTLY one element per iteration,
    /// unconditionally, and mentions the accumulator nowhere else. This
    /// licenses the tabulate lowering — output length is exactly
    /// `iter_total` and iteration `i` owns output slot `i`, so workers
    /// write elements in place into one presized shared buffer (no
    /// per-worker partial Vecs, no combine memcpy). The gate must be
    /// exact: an extra or skipped push under tabulate overflows a
    /// worker's chunk view and the push grow-path would free an interior
    /// pointer. See `collect_is_tabulate_shape`.
    pub collect_tabulate: bool,
    /// SEQUENTIAL tabulate (no `#[par_unordered]`): the same
    /// tabulate-shape guarantee, lowered inline — reserve the exact
    /// capacity once, store each element in place, bump `len` after the
    /// loop. No parallel dispatch, no reordering license needed; the
    /// win is removing the per-iteration push grow-branch + realloc
    /// call, which is what blocks LLVM's loop vectorizer on the
    /// canonical `out.push(f(v[i]))` map loop (see the phase-10
    /// CPU-codegen-gap entry, 2026-07-16 forensics). Only ever true
    /// with `op == Collect && collect_tabulate`.
    pub seq: bool,
}

/// A set of statements that can safely run in parallel.
#[derive(Debug, Clone)]
pub struct ParallelGroup {
    /// Indices of statements in this parallel group.
    pub statement_indices: Vec<usize>,
    /// Why these can be parallelized.
    pub reason: String,
    /// True if the group is too cheap to justify thread dispatch
    /// (pure arithmetic, simple variable access, no I/O or function calls with effects).
    /// Codegen should run trivial groups inline instead of spawning tasks.
    pub is_trivial: bool,
    /// Names of *captured* (pre-existing) locals that some stmt in this
    /// group mutates without introducing them as a fresh let-binding —
    /// e.g., `v.push(3)` mutates the captured `v`, `cap = max` mutates
    /// the captured `cap`. The auto-par codegen captures locals by
    /// value into the per-branch env struct, so these mutations live
    /// on the branch's local copy and are lost at join time. Codegen
    /// (`compute_return_slots_checked`) consults this set: if any name
    /// in it is read outside the group, the par-group is dropped and
    /// the stmts run sequentially. Names freshly introduced by
    /// `let`/`let-uninit`/`let-else` patterns within the group itself
    /// are excluded — those flow through the return-slot mechanism
    /// already.
    pub captured_mutations: HashSet<String>,
    /// The subset of `captured_mutations` naming HEAP-OWNING CONTAINER
    /// locals (`Vec` / `String` / `Map` / `Set` / sorted variants). A lost
    /// branch-local mutation of one of these is never a dead write even when
    /// no later statement reads the name: the parent's scope-exit drop reads
    /// the container header, and the branch's realloc'd buffer + pushed
    /// elements are orphaned (B-2026-07-15-2 — the write-only single-push
    /// `Vec[shared]` leak). Codegen falls back to sequential whenever this
    /// set is non-empty, independent of the outside-reads check.
    pub captured_container_mutations: HashSet<String>,
}

// ── Internal: Per-statement metadata ───────────────────────────

/// Metadata extracted from a single statement for dependency analysis.
#[derive(Debug, Clone, Default)]
struct StmtInfo {
    /// Variables defined (written) by this statement.
    defines: HashSet<String>,
    /// Names freshly introduced by `let`/`let-uninit`/`let-else`
    /// patterns in this statement (subset of `defines`). The complement
    /// `defines − let_introduced` is the set of *captured* names this
    /// statement mutates — needed by the auto-par codegen to decide
    /// whether a multi-stmt group can safely run in parallel given
    /// that captures are bit-copied into per-branch envs.
    let_introduced: HashSet<String>,
    /// Variables read by this statement.
    reads: HashSet<String>,
    /// Bare names of functions this statement calls (free-fn callee names
    /// and method names, transitively through the statement's expression
    /// tree). Drives the SELF-RECURSION par gate (B-2026-07-15-4): a group
    /// whose statement calls the enclosing function is a recursive
    /// divide-and-conquer — spawning it costs ~70µs per dispatch and O(nodes)
    /// dispatches per top-level call (each recursion level re-spawns), which
    /// no bounded top-level win can amortize without a work-stealing
    /// sequential-cutoff scheduler. Measured 175x wall-time regression on a
    /// 15-node tree build at 20k reps before the gate.
    called_fn_names: HashSet<String>,
    /// Effects produced by this statement (from called functions).
    effects: Vec<StmtEffect>,
    /// Whether this statement (transitively) calls a function with polymorphic
    /// effects (`with _`). Its effects are unknown at analysis time, so it must
    /// serialize conservatively against any other stmt with visible effects.
    calls_polymorphic: bool,
    /// Whether this statement is inside a seq {} block.
    is_seq: bool,
    /// Whether this statement may exit the enclosing function abnormally
    /// (an `if` body / loop body / match arm reachable through this stmt
    /// contains `return`, `break`, or `continue`). Such statements
    /// cannot share a parallel group with siblings — par branches are
    /// emitted as standalone `void` LLVM functions and a raw `ret X`
    /// from inside the branch produces invalid IR ("return instr that
    /// returns non-void in Function of void return type").
    has_early_exit: bool,
    /// Whether this statement performs a channel operation (`Channel.new()`
    /// / `Sender.send` / `Receiver.recv` / `Receiver.try_recv`). Such
    /// statements are kept out of auto-par groups — channels are explicit
    /// communication primitives whose ordering auto-par must not disturb.
    /// See `stmt_has_channel_op` and the `find_parallel_groups` guards.
    has_channel_op: bool,
    /// Whether this statement *syntactically* performs console output
    /// (`println` / `print` / `eprintln` / `eprint`). Used only by the
    /// reorder-opportunity advisory to exclude such statements as movers —
    /// relocating a console write reorders observable output, which the
    /// effect surface (console output is resourceless) would not flag. A
    /// best-effort local check, not interprocedural. See
    /// `stmt_has_console_output`.
    has_console_output: bool,
    /// Whether this statement is a direct, pure `sleep_ms(...)` timer-park
    /// call — the ONLY `suspends` form the auto-parallelizer overlaps (A2b).
    /// `suspends` is an execution verb (placement, not conflict — design.md
    /// :5907), but at the effect level a timer wait and a channel `recv` are
    /// indistinguishable (both seed a bare `suspends`), and a channel recv is
    /// NOT independent — it has a happens-before with its producer, so
    /// relocating it into a `__par_branch` worker deadlocks. So the boundary
    /// gate keeps *every* `suspends` stmt serial (conservative default) and
    /// exempts only the ones proven to be a standalone timer park here. See
    /// `stmt_is_timer_suspend` and the `find_parallel_groups` boundary guards.
    is_timer_suspend: bool,
    /// A2b-2: whether this statement is a network-boundary call the auto-par
    /// fan-out can safely overlap — a direct free-function `Call` (or its
    /// `let`) whose arguments move in NO owned heap/`Drop` binding, so the
    /// coroutine-owned-param double-drop (the `__par_branch` suppression-scope
    /// gap) cannot fire. Like `is_timer_suspend`, it exempts the statement from
    /// the `effects_mark_coroutine_boundary` gate — the conflict model then
    /// keeps same-resource network calls (`sends`/`receives` on `Network`)
    /// serial and overlaps only independent ones (e.g. two `reads(Network)`
    /// fetches). Fail-closed: proven purely from AST shape (no type info in
    /// this pass), so it admits literal/const-arg calls only; variable-arg
    /// fan-out (Copy/borrow args) awaits threading ownership info through and
    /// is the A2b-2 follow-up. Set in `analyze_stmt` as `stmt_fanout_args_safe`
    /// (arg-safety) AND a `Network`-resource-effect check.
    is_safe_network_fanout: bool,
    /// A2b-2 Phase 1: whether this statement is an *ephemeral* network
    /// fan-out — a safe network fan-out (`is_safe_network_fanout`) whose
    /// callee declares NO borrow parameter (`ref`/`mut ref`/`mut Slice`). No
    /// borrow param means the callee cannot be handed a shared connection
    /// object; it must open its own connection internally (the
    /// `http_get(url: String)` shape), so two such calls touch disjoint,
    /// freshly-created OS connection state. That is what makes it *sound to
    /// relax the `Network`-resource conflict* between two of them
    /// (`(Sends,Sends)`/`(Receives,Receives)` on `Network`) in
    /// `statements_conflict`, letting `http_get("a"); http_get("b")` fan out
    /// with their real `sends`/`receives` effects. A call that borrows an
    /// argument (`send_on(ref conn, ...)`) is deliberately excluded: two ops
    /// on the same borrowed connection would race if overlapped, and this
    /// pass has no connection-identity info to tell same-conn from
    /// different-conn apart — that is the Phase 2 parameterized-`Network`
    /// follow-up (docs/spikes/network-resource-granularity.md). Any *other*
    /// shared resource a callee touches (a pool checkout `writes(Pool)`, a DB
    /// `writes(Db)`) still surfaces as a non-`Network` effect and still
    /// serializes — the relaxation only ever skips `Network`↔`Network` pairs.
    /// Set in `analyze_stmt` as `is_safe_network_fanout` AND
    /// `stmt_callee_has_no_borrow_params`.
    is_ephemeral_network_fanout: bool,
    /// A2b-2 Phase 2 Slice 2: for a method-call network fan-out CANDIDATE
    /// (`obj.method(args)` touching `Network`, borrowed `ref`/`mut ref self`,
    /// plain-identifier receiver that is neither a `ref` param nor a `shared`
    /// (RC) type, args fan-out-safe), the receiver ROOT identifier; `None`
    /// otherwise. Two such statements with DIFFERENT roots have provably
    /// distinct, non-aliasing receivers — distinct connections — so
    /// `statements_conflict` relaxes their `Network`↔`Network` conflict. Same
    /// root is already serialized by the write-write data dependency (a
    /// `mut ref self` method defines its receiver), and a shared-type / ref-param
    /// receiver (which could alias under a different name) is excluded here.
    /// Requires type info (`method_callee_types`); `None` without it
    /// (fail-closed). Computed in `analyze_stmt` via `classify_method_fanout`.
    method_fanout_receiver_root: Option<String>,
    /// Whether this statement is a constant-cost initializer — a
    /// `let`/`assign` of a literal or bare identifier, or a `let
    /// uninit`. These are O(1) and run in ~zero time. Used by the
    /// cost-model gate in `find_parallel_groups`: a parallel group
    /// where N−1 of N stmts are constant-init has zero structural
    /// parallelism (one branch does all the work, others idle) and is
    /// marked trivial so codegen skips the `karac_par_run` dispatch.
    /// Without this, the auto-parallelizer pays per-spawn cost (~70μs
    /// on macOS) for groups that can produce no speedup — the
    /// dominant hot-path overhead surfaced by the kata 6 zigzag bench
    /// (2.5× slowdown vs sequential codegen, 2026-05-17).
    is_constant_init: bool,
}

/// Human label for an effect verb, used in serialization-point reasons.
fn effect_verb_label(v: &EffectVerbKind) -> &str {
    match v {
        EffectVerbKind::Reads => "reads",
        EffectVerbKind::Writes => "writes",
        EffectVerbKind::Sends => "sends",
        EffectVerbKind::Receives => "receives",
        EffectVerbKind::Allocates => "allocates",
        EffectVerbKind::Panics => "panics",
        EffectVerbKind::Blocks => "blocks",
        EffectVerbKind::Suspends => "suspends",
        EffectVerbKind::UserDefined(s) => s.as_str(),
    }
}

/// An effect associated with a statement.
#[derive(Debug, Clone)]
struct StmtEffect {
    verb: EffectVerbKind,
    resource: String,
    /// The callee whose effect this is — the function/method key
    /// (`fn` name or `Type.method`) that contributed this effect to the
    /// statement, or `None` for an effect the statement performs
    /// directly. Used to attribute a serialization point to the specific
    /// callee responsible (`SerializationPoint::blocking_callees`).
    source_callee: Option<String>,
    /// A2b-2 Phase 2 Slice 3 (parameterized resources): the **partition key**
    /// for a parameterized resource (`writes(Db[id])`), when it resolves to a
    /// compile-time LITERAL at this call site — the callee's declared param
    /// substituted with the actual argument (`update(5)` on `writes(Db[id])` →
    /// `Some("5")`). `None` for an unparameterized resource OR a param that does
    /// not reduce to a literal here (a variable arg — fail-closed to "unproven",
    /// so it conservatively conflicts). Two same-resource effects with distinct
    /// `Some` keys touch DIFFERENT partitions and never conflict
    /// (`design.md § Parameterized Resources`, proven-disjoint case).
    key: Option<String>,
}

/// True iff a statement's effect set marks it as a **coroutine network-boundary
/// call** — one that the A2 coroutine transform (`build_state_struct_layouts`,
/// keyed off `sends(Network)`/`receives(Network)`) compiles into a dispatcher-
/// driven LLVM coroutine, or a `suspends` park (e.g. `Receiver.recv`). Such a
/// statement must not be auto-parallelized: a coroutine owns + drops its
/// by-value params at completion while auto-par captures are shared-with-write-
/// back (the parent keeps drop ownership), so lifting the call into a
/// `__par_branch` worker double-drops any owned user-`Drop` arg (an fd
/// double-close for a `WebSocket`), and the ramp+wait belongs to the async
/// dispatcher, not the `karac_par_run` pool. See `find_parallel_groups`.
///
/// **`suspends` stays gated, except a standalone timer park (A2b, 2026-06-10).**
/// At the effect level a channel `recv`, a network park, and `sleep_ms` all
/// seed a bare `suspends` — indistinguishable here. A channel recv is NOT
/// independent (it has a happens-before with its producer; relocating it into a
/// `__par_branch` worker deadlocks — regression-pinned by
/// `e2e_auto_par_channel_consumer_terminates`), so the conservative default is
/// to keep every `suspends` stmt serial. The `find_parallel_groups` boundary
/// guards then exempt only the stmts `stmt_is_timer_suspend` proves to be a
/// standalone `sleep_ms` call — the one `suspends` form known to be independent
/// (a bare timer wait, no by-value `Drop` params). The harder *network*
/// coroutine fan-out (design.md:9044 `http_get` — true double-drop, wants
/// dispatcher routing) stays gated pending A2b-2.
fn effects_mark_coroutine_boundary(effects: &[StmtEffect]) -> bool {
    effects.iter().any(|e| {
        matches!(e.verb, EffectVerbKind::Suspends)
            || (matches!(e.verb, EffectVerbKind::Sends | EffectVerbKind::Receives)
                && e.resource == "Network")
    })
}

/// True iff `stmt` is a direct, pure `sleep_ms(...)` timer-park call — the only
/// `suspends` form the auto-parallelizer overlaps (A2b). `find_parallel_groups`
/// exempts such statements from the `effects_mark_coroutine_boundary` gate so
/// two independent timer waits overlap via the `karac_par_run` thread-block
/// path, exactly like `blocks` (A1). It is deliberately conservative: the stmt
/// must be exactly a `sleep_ms` call whose args contain no further call or
/// method (which could itself suspend or touch a channel) — anything richer
/// stays serial. A `sleep_ms` wrapper fn (`fn nap() { sleep_ms(..) }`) does NOT
/// qualify (the call site sees only the wrapper's propagated `suspends`, not
/// that it is timer-pure); supporting wrappers would need provenance on the
/// effect and is left to A2b-2.
fn stmt_is_timer_suspend(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => expr_is_pure_sleep_ms_call(value),
        _ => false,
    }
}

/// `sleep_ms(<call-free args>)` — a `Call` to the bare `sleep_ms` path whose
/// every argument is itself free of any nested call/method.
fn expr_is_pure_sleep_ms_call(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            // A bare free-function callee is either an `Identifier` or a
            // single-segment `Path`, depending on parse context.
            let is_sleep_ms = match &callee.kind {
                ExprKind::Identifier(name) => name == "sleep_ms",
                ExprKind::Path { segments, .. } => segments.len() == 1 && segments[0] == "sleep_ms",
                _ => false,
            };
            is_sleep_ms && args.iter().all(|a| expr_is_call_free(&a.value))
        }
        _ => false,
    }
}

/// True iff `expr` contains no `Call` and no `MethodCall` anywhere — used to
/// confirm a `sleep_ms` argument cannot itself suspend or touch a channel.
fn expr_is_call_free(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::Closure { .. } => false,
        ExprKind::Binary { left, right, .. } => expr_is_call_free(left) && expr_is_call_free(right),
        ExprKind::Unary { operand, .. } => expr_is_call_free(operand),
        ExprKind::Index { object, index } => expr_is_call_free(object) && expr_is_call_free(index),
        ExprKind::FieldAccess { object, .. } => expr_is_call_free(object),
        ExprKind::Cast { expr, .. } => expr_is_call_free(expr),
        // Literals, paths, and other leaf forms carry no call.
        _ => true,
    }
}

/// A2b-2 (arg-safety half): true iff `stmt` is a direct free-function `Call`
/// (or `let x = Call(...)`) whose every argument is BOTH call-free (no nested
/// call/method that could itself suspend or touch a channel) AND binding-free
/// (references no name, so nothing owned is moved into the coroutine). The
/// coroutine-owned-param double-drop
/// (docs/spikes/network-async-coroutine-transform.md; the `__par_branch`
/// suppression-scope gap in `call_dispatch.rs`) fires ONLY when a coroutine
/// call moves an owned parent `Drop`/heap binding into itself via an
/// `Identifier` argument; a literal / const-expression argument names no
/// binding, so the caller's drop-suppression has nothing to cancel and the
/// value drops exactly once (inside the coroutine). Deliberately conservative:
/// it admits the flagship two-`http_get("...")`-to-different-hosts shape and
/// leaves variable-arg fan-out (Copy/borrow args — safe, but indistinguishable
/// from an owned move without type info this pass does not carry) to the A2b-2
/// follow-up. This is only the ARG-safety half; `analyze_stmt` combines it with
/// a Network-resource-effect check so the exemption fires for network calls
/// only (a non-network user `with suspends` fn stays serial), and the conflict
/// model still serializes same-resource network calls
/// (`(Sends,Sends)`/`(Receives,Receives)` on `Network`) — the exemption ONLY
/// lifts the blanket coroutine-boundary EXCLUSION so two *independent*
/// (disjoint-resource) network calls can group.
fn stmt_fanout_args_safe(
    stmt: &Stmt,
    function_bodies: &HashMap<String, &Function>,
    method_bodies: &HashMap<String, &Function>,
) -> bool {
    let value = match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => value,
        _ => return false,
    };
    let ExprKind::Call { callee, args } = &value.kind else {
        return false;
    };
    // Resolve the callee's params. Admitted callee shapes: a bare free function
    // (`Identifier` / 1-segment `Path`) OR a 2-segment ASSOCIATED-function path
    // (`Type.connect(...)`, no `self` receiver — A2b-2 Phase 2 Slice 1). Neither
    // has a receiver to move into the coroutine or to share between two calls,
    // so both fit the double-drop reasoning below. A 2-segment path that is a
    // METHOD (has `self` — its receiver IS a connection, e.g. `stream.read`) or
    // is unresolvable (extern, associated-vs-method unknown) is rejected via
    // `resolve_assoc_callee`, and a computed callee is outside the shape.
    //
    // For a free function the params may be absent (extern) — a literal argument
    // is still safe, so the shape is admitted with `None` params (`param_is_borrow`
    // is `false` on `None`, so any `Identifier` argument then fails, leaving only
    // literal-arg extern calls). When present, the params tell us which positions
    // BORROW their argument (`ref`/`mut ref`/`mut Slice` — not moved): an
    // `Identifier` at a borrow position moves no owned binding into the coroutine,
    // so it is fan-out-safe even though it names a binding (verified
    // double-free-clean by
    // `tests/memory_sanitizer.rs::asan_par_ref_string_arg_network_call_no_double_free`).
    let callee_params: Option<&[Param]> = match &callee.kind {
        ExprKind::Identifier(n) => function_bodies.get(n).map(|f| f.params.as_slice()),
        ExprKind::Path { segments, .. } if segments.len() == 1 => function_bodies
            .get(&segments[0])
            .map(|f| f.params.as_slice()),
        ExprKind::Path { segments, .. } if segments.len() == 2 => {
            match resolve_assoc_callee(segments, method_bodies) {
                Some(f) => Some(f.params.as_slice()),
                None => return false,
            }
        }
        _ => return false,
    };
    args.iter().enumerate().all(|(i, a)| {
        expr_is_call_free(&a.value)
            && (expr_is_binding_free(&a.value)
                || (matches!(a.value.kind, ExprKind::Identifier(_))
                    && param_is_borrow(callee_params, i)))
    })
}

/// True iff the callee's parameter at position `i` is a borrow form
/// (`ref T` / `mut ref T` / `mut Slice[T]`) — an argument passed there is
/// borrowed, never moved, so an owned binding at that position is not
/// double-dropped when the call is lifted into a par branch. `None` params
/// (callee body not in this program) → `false` (fail-closed).
fn param_is_borrow(params: Option<&[Param]>, i: usize) -> bool {
    params.and_then(|ps| ps.get(i)).is_some_and(|p| {
        matches!(
            p.ty.kind,
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
        )
    })
}

/// A2b-2 Phase 2 Slice 3: resolve a parameterized-resource key expression
/// (`Db[<param>]`) to a compile-time-LITERAL partition key at a call site, or
/// `None` if it does not reduce to a literal here. The declared key `param` is
/// relative to the callee's `params`: a bare identifier names a callee
/// parameter, substituted with the actual `args` at the same position; a
/// literal in the declaration itself is taken verbatim. A non-literal resolved
/// argument (a variable) yields `None` — deliberately "unproven", so two such
/// calls conservatively conflict. Integer keys normalize to their numeric value
/// (so `5` and `5u64` are the same partition), keeping distinctness sound.
fn resolve_param_key(param: &Expr, params: &[Param], args: &[CallArg]) -> Option<String> {
    match &param.kind {
        ExprKind::Identifier(pname) => {
            let idx = params
                .iter()
                .position(|p| matches!(&p.pattern.kind, PatternKind::Binding(n) if n == pname))?;
            literal_key(&args.get(idx)?.value)
        }
        _ => literal_key(param),
    }
}

/// The compile-time-literal partition key of `expr` (its normalized value), or
/// `None` if `expr` is not an integer/string literal.
fn literal_key(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Integer(n, _) => Some(n.to_string()),
        ExprKind::StringLit(s) => Some(s.clone()),
        _ => None,
    }
}

/// A2b-2 Phase 2 (Slice 1): resolve a 2-segment `Type.method` callee to its
/// body IFF it is an ASSOCIATED function — one with NO `self` receiver
/// (`self_param.is_none()`) — present in this program's `method_bodies`. These
/// are the receiver-less connection *openers* (`TcpStream.connect`,
/// `TlsStream.connect`): structurally identical to a free function, since there
/// is no receiver to move into the coroutine or to share between two calls. A
/// 2-segment path that resolves to a METHOD (`self_param.is_some()` — its
/// receiver IS a live connection/listener, e.g. `stream.read`, `listener.accept`)
/// is deliberately NOT admitted (returns `None`): overlapping two ops on one
/// shared receiver would race. An unresolvable callee (extern — associated
/// vs. method is unknown) also returns `None`, fail-closed. Returns `None` for a
/// non-2-segment path so callers can branch on it uniformly.
fn resolve_assoc_callee<'a>(
    segments: &[String],
    method_bodies: &HashMap<String, &'a Function>,
) -> Option<&'a Function> {
    if segments.len() != 2 {
        return None;
    }
    let key = format!("{}.{}", segments[0], segments[1]);
    method_bodies
        .get(&key)
        .copied()
        .filter(|f| f.self_param.is_none())
}

/// A2b-2 Phase 1 companion to [`stmt_fanout_args_safe`]: true iff the
/// statement's callee (resolved by the same free-fn / associated-fn rule) is in
/// this program and declares NO borrow parameter (`ref`/`mut ref`/`mut Slice`).
/// Combined with `is_safe_network_fanout` in `analyze_stmt` it yields
/// `is_ephemeral_network_fanout` — see that field's doc for why a borrow-free
/// callee proves two network calls use disjoint, freshly-opened connections
/// and may overlap. Fail-closed: a computed callee, a non-`Call` statement, or
/// an extern callee (body not in this program, so its param modes are unknown)
/// all return `false`.
fn stmt_callee_has_no_borrow_params(
    stmt: &Stmt,
    function_bodies: &HashMap<String, &Function>,
    method_bodies: &HashMap<String, &Function>,
) -> bool {
    let value = match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => value,
        _ => return false,
    };
    let ExprKind::Call { callee, .. } = &value.kind else {
        return false;
    };
    // Mirror `stmt_fanout_args_safe`'s callee resolution: a bare free function
    // or a 2-segment ASSOCIATED-function path (`Type.connect`, no `self`). Unlike
    // that function, this one needs the params to exist — an extern callee (body
    // absent) is fail-closed `false`, since its param modes are unknown.
    let params: &[Param] = match &callee.kind {
        ExprKind::Identifier(n) => match function_bodies.get(n) {
            Some(f) => &f.params,
            None => return false,
        },
        ExprKind::Path { segments, .. } if segments.len() == 1 => {
            match function_bodies.get(&segments[0]) {
                Some(f) => &f.params,
                None => return false,
            }
        }
        ExprKind::Path { segments, .. } if segments.len() == 2 => {
            match resolve_assoc_callee(segments, method_bodies) {
                Some(f) => &f.params,
                None => return false,
            }
        }
        _ => return false,
    };
    !params.iter().any(|p| {
        matches!(
            p.ty.kind,
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
        )
    })
}

/// True iff `expr` references no binding — used by `stmt_fanout_args_safe`
/// to prove a network call's arguments move no owned parent binding into the
/// coroutine. FAIL-CLOSED: only pure-value literals and arithmetic/cast over
/// them are binding-free; ANY `Identifier`/`Path`, and every richer form
/// (struct/array/map literal, interpolated string, index, field access, call,
/// closure, …) that could carry a name, disqualifies. This pass has no type
/// info, so it cannot tell an owned heap binding from a Copy scalar and
/// conservatively excludes all names.
fn expr_is_binding_free(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..)
        | ExprKind::StringLit(..)
        | ExprKind::MultiStringLit(..)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(..) => true,
        ExprKind::Binary { left, right, .. } => {
            expr_is_binding_free(left) && expr_is_binding_free(right)
        }
        ExprKind::Unary { operand, .. } => expr_is_binding_free(operand),
        ExprKind::Cast { expr, .. } => expr_is_binding_free(expr),
        // Identifier / Path / interpolated string / struct-array-map literals /
        // index / field access / call / closure / everything else: fail closed.
        _ => false,
    }
}

/// Sparse statement-conflict graph.
///
/// Replaces the former dense `Vec<Vec<bool>>` adjacency matrix (which was
/// `O(n²)` memory — a 49K-statement function alone allocated ~2.4 GB of
/// bools — and was filled by an all-pairs `O(n²)` scan). Two statements can
/// only conflict if they share a *binding* (dataflow), a *resource*
/// (effect), a *polymorphic-effect* linkage, or a `seq` ordering — see
/// [`ConcurrencyChecker::statements_conflict`]. So an inverted index over
/// those keys enumerates every real edge in ~`O(edges)` work, with no
/// quadratic allocation or all-pairs conflict check. See
/// phase-5-diagnostics.md.
struct ConflictGraph {
    /// `neighbors[i]` = the set of statements that conflict with statement `i`.
    neighbors: Vec<HashSet<usize>>,
}

impl ConflictGraph {
    /// Do statements `i` and `j` conflict (must serialize)? Symmetric.
    fn conflicts(&self, i: usize, j: usize) -> bool {
        self.neighbors[i].contains(&j)
    }
}

// ── Checker ────────────────────────────────────────────────────

pub struct ConcurrencyChecker<'a> {
    program: &'a Program,
    effects: &'a EffectCheckResult,
    /// Function bodies collected from the program, keyed by function name.
    function_bodies: HashMap<String, &'a Function>,
    /// Impl method bodies: "TypeName.method" -> &Function.
    method_bodies: HashMap<String, &'a Function>,
    /// Type info (when available). Its `method_callee_types` map (receiver type
    /// name per method-call span) drives method-receiver classification for
    /// A2b-2 Phase 2 Slice 2 (method-call network fan-out). `None` disables it.
    types: Option<&'a TypeCheckResult>,
    /// Names of `shared struct` / `shared enum` (RC) types declared in this
    /// program. A2b-2 Phase 2 Slice 2: a method receiver of a shared type can
    /// ALIAS another binding (`let b = a` clones the RC handle), so two method
    /// calls on distinct-named shared receivers may still hit the same object —
    /// they are excluded from method-call fan-out.
    shared_type_names: HashSet<String>,
}

impl<'a> ConcurrencyChecker<'a> {
    pub fn new(
        program: &'a Program,
        effects: &'a EffectCheckResult,
        types: Option<&'a TypeCheckResult>,
    ) -> Self {
        let shared_type_names = program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::StructDef(s) if s.is_shared => Some(s.name.clone()),
                Item::EnumDef(e) if e.is_shared => Some(e.name.clone()),
                _ => None,
            })
            .collect();
        let mut checker = ConcurrencyChecker {
            program,
            effects,
            function_bodies: HashMap::new(),
            method_bodies: HashMap::new(),
            types,
            shared_type_names,
        };
        checker.collect_functions();
        checker
    }

    fn collect_functions(&mut self) {
        for item in &self.program.items {
            match item {
                Item::Function(f) => {
                    self.function_bodies.insert(f.name.clone(), f);
                }
                Item::ImplBlock(imp) => {
                    let type_name = match &imp.target_type.kind {
                        TypeKind::Path(p) => match p.segments.last().cloned() {
                            Some(name) => name,
                            None => continue,
                        },
                        _ => continue,
                    };
                    for item in &imp.items {
                        if let ImplItem::Method(method) = item {
                            let key = format!("{}.{}", type_name, method.name);
                            self.method_bodies.insert(key, method);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub fn analyze(self) -> ConcurrencyAnalysis {
        let mut decisions = HashMap::new();

        for item in &self.program.items {
            if let Item::Function(f) = item {
                let fc = self.analyze_function(f);
                decisions.insert(f.name.clone(), fc);
            }
        }

        // Also analyze impl methods
        for item in &self.program.items {
            if let Item::ImplBlock(imp) = item {
                let type_name = match &imp.target_type.kind {
                    TypeKind::Path(p) => match p.segments.last().cloned() {
                        Some(name) => name,
                        None => continue,
                    },
                    _ => continue,
                };
                for impl_item in &imp.items {
                    if let ImplItem::Method(method) = impl_item {
                        let key = format!("{}.{}", type_name, method.name);
                        let fc = self.analyze_function(method);
                        decisions.insert(key, fc);
                    }
                }
            }
        }

        ConcurrencyAnalysis {
            function_decisions: decisions,
            queries: Vec::new(),
        }
    }

    fn analyze_function(&self, func: &Function) -> FunctionConcurrency {
        let stmts = &func.body.stmts;
        let total_statements = stmts.len();

        // B-2026-07-16-10: a function containing any user `defer` / `errdefer`
        // is not auto-parallelized — the par_run whole-function lowering does
        // not preserve LIFO-at-scope-exit defer semantics (it emits function-
        // scope defers FIFO-inline). Return an empty decision so codegen falls
        // back to the sequential lowering, which drains defers correctly. See
        // `block_has_user_defer` for the full rationale. (Explicit `par {}` is
        // a separate codegen path and is unaffected.)
        if total_statements == 0 || block_has_user_defer(&func.body) {
            return FunctionConcurrency {
                parallel_groups: Vec::new(),
                total_statements,
                statement_spans: Vec::new(),
                loop_reductions: Vec::new(),
                serialization_points: Vec::new(),
                reorder_opportunities: Vec::new(),
            };
        }

        // The enclosing fn's `ref`/`mut ref` parameter names — a method-call
        // receiver rooted at one may be caller-aliased, so it is excluded from
        // method-call network fan-out (Slice 2). `mut Slice` params are borrows
        // too but never name a method receiver of interest; included for parity
        // with `param_is_borrow`.
        let mut ref_params: HashSet<String> = HashSet::new();
        for p in &func.params {
            if matches!(
                p.ty.kind,
                TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
            ) {
                self.collect_pattern_bindings(&p.pattern, &mut ref_params);
            }
        }

        // Step 1: Extract metadata for each statement
        let stmt_infos: Vec<StmtInfo> = stmts
            .iter()
            .map(|s| self.analyze_stmt(s, false, &ref_params))
            .collect();

        // Step 2: Build the conflict graph + the serialization-point list
        // (the inverse of the parallel groups: for every conflicting pair,
        // *why* they can't parallelize + which callee's effect is to blame).
        // Uses a sparse inverted index rather than a dense O(n²) matrix — see
        // `build_conflict_graph` / [`ConflictGraph`].
        let (graph, serialization_points) = self.build_conflict_graph(&stmt_infos);

        // Step 3: Find maximal independent sets (greedy graph coloring approach)
        // We group statements that have no edges between them.
        // Names of locals whose declared/recorded type is a heap-owning
        // container — feeds `captured_container_mutations` (B-2026-07-15-2).
        let container_locals = self.collect_container_locals(&func.body);
        // B-2026-07-16-19: per-stmt consuming reads of move-hazard locals.
        // A statement that MOVES heap ownership out of a binding it captured
        // (a `match r { Some(w) => .. }` on an `Option[String]`, a bare owned
        // heap arg to a consuming callee, `let y = s;`) must not enter a par
        // group: the branch env bit-copies the binding, the branch's move
        // machinery suppresses/frees only its LOCAL copy, and the parent's
        // scope-exit cleanup still fires on the original — a double-free the
        // stmt-vs-stmt conflict graph cannot see (the hazard is stmt-vs-
        // scope-exit, not stmt-vs-stmt).
        let move_hazards = self.collect_move_hazard_locals(&func.body);
        let consuming_hazard_reads: Vec<HashSet<String>> = stmts
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut set = self.stmt_consuming_hazard_reads(s, &move_hazards, true);
                // Names the stmt itself introduces are its own to consume —
                // the branch-local move machinery is complete for those.
                for n in &stmt_infos[i].let_introduced {
                    set.remove(n);
                }
                set
            })
            .collect();
        // B-2026-07-22-9 producer guard uses a NARROWER set: wrapper-combinator
        // consumption (`a.unwrap_or(..)`) of a published slot is round-trip-safe
        // (B-2026-07-17-4), so it must not de-parallelize the PRODUCER of that
        // binding — only genuine MOVES do. (The consumer of such a call is still
        // gated by `consuming_hazard_reads` above.)
        let moving_hazard_reads: Vec<HashSet<String>> = stmts
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut set = self.stmt_consuming_hazard_reads(s, &move_hazards, false);
                for n in &stmt_infos[i].let_introduced {
                    set.remove(n);
                }
                set
            })
            .collect();
        let parallel_groups = self.find_parallel_groups(
            &stmt_infos,
            &graph,
            total_statements,
            &container_locals,
            &consuming_hazard_reads,
            &moving_hazard_reads,
            &func.name,
        );

        // Step 4: Recognize reductions in top-level loops. Independent of
        // the parallel-group / dependency machinery — a reduction loop
        // has a loop-carried dependency that the parallel-group analysis
        // correctly serializes, but the loop's iteration space can still
        // be split across workers when the op is associative + commutative.
        let loop_reductions = self.recognize_reductions(func);

        // Step 5: Flag parallelism left on the table purely by source
        // ordering — independent statements the contiguous-only grouper
        // could not co-group because they are non-adjacent, but a legal
        // reorder would. Advisory only; consumes the same dependency graph.
        let reorder_opportunities = self.find_reorder_opportunities(
            &stmt_infos,
            &graph,
            total_statements,
            &parallel_groups,
        );

        let statement_spans = stmts.iter().map(|s| s.span.clone()).collect();

        FunctionConcurrency {
            parallel_groups,
            total_statements,
            statement_spans,
            loop_reductions,
            serialization_points,
            reorder_opportunities,
        }
    }

    /// Build the sparse [`ConflictGraph`] plus the ordered
    /// serialization-point list for a function body.
    ///
    /// Instead of the former dense `O(n²)` all-pairs scan, this enumerates
    /// only *candidate* pairs — pairs that share a binding, a resource, a
    /// polymorphic-effect linkage, or a `seq` ordering — via inverted
    /// indices, since [`Self::statements_conflict`] can only return `true`
    /// for such pairs. Every candidate is then run through the exact same
    /// `statements_conflict` / `conflict_detail` predicates, so the produced
    /// edge set and serialization points are identical to the old dense
    /// build (the serialization points are re-sorted into the old
    /// outer-`i` / inner-`j` emission order for byte-stable diagnostics).
    fn build_conflict_graph(&self, infos: &[StmtInfo]) -> (ConflictGraph, Vec<SerializationPoint>) {
        let n = infos.len();

        // Inverted indices. Only pairs colliding on one of these keys can
        // ever conflict, so they bound the candidate set.
        let mut var_definers: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut var_readers: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut resource_stmts: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut seq_stmts: Vec<usize> = Vec::new();
        let mut poly_stmts: Vec<usize> = Vec::new();
        let mut effectful_stmts: Vec<usize> = Vec::new();

        for (i, info) in infos.iter().enumerate() {
            if info.is_seq {
                seq_stmts.push(i);
            }
            if info.calls_polymorphic {
                poly_stmts.push(i);
            }
            if !info.effects.is_empty() {
                effectful_stmts.push(i);
            }
            for v in &info.defines {
                var_definers.entry(v.as_str()).or_default().push(i);
            }
            for v in &info.reads {
                var_readers.entry(v.as_str()).or_default().push(i);
            }
            for e in &info.effects {
                resource_stmts
                    .entry(e.resource.as_str())
                    .or_default()
                    .push(i);
            }
        }

        // Candidate unordered pairs, stored `(lo, hi)` with `lo < hi`.
        let mut candidates: HashSet<(usize, usize)> = HashSet::new();
        let mut add = |a: usize, b: usize| {
            if a != b {
                candidates.insert((a.min(b), a.max(b)));
            }
        };

        // A `seq` statement force-serializes against *every* other statement.
        for &s in &seq_stmts {
            for other in 0..n {
                add(s, other);
            }
        }

        // Dataflow: a conflict via binding `v` requires at least one *definer*
        // of `v` (two pure readers never conflict). So pair each definer with
        // every other definer and every reader of the same binding.
        for (v, definers) in &var_definers {
            for a in 0..definers.len() {
                for b in (a + 1)..definers.len() {
                    add(definers[a], definers[b]);
                }
            }
            if let Some(readers) = var_readers.get(v) {
                for &d in definers {
                    for &r in readers {
                        add(d, r);
                    }
                }
            }
        }

        // Polymorphic calls have unknown effects: each conflicts with any
        // other polymorphic *or* effect-bearing statement.
        for a in 0..poly_stmts.len() {
            for b in (a + 1)..poly_stmts.len() {
                add(poly_stmts[a], poly_stmts[b]);
            }
        }
        for &p in &poly_stmts {
            for &e in &effectful_stmts {
                add(p, e);
            }
        }

        // Effect conflicts only arise between statements touching the *same*
        // resource (`two_effects_conflict` short-circuits on differing
        // resources).
        for stmts in resource_stmts.values() {
            for a in 0..stmts.len() {
                for b in (a + 1)..stmts.len() {
                    add(stmts[a], stmts[b]);
                }
            }
        }

        // Confirm each candidate against the exact predicate and record edges.
        let mut neighbors = vec![HashSet::new(); n];
        let mut edges: Vec<(usize, usize)> = Vec::new();
        for &(lo, hi) in &candidates {
            if self.statements_conflict(&infos[lo], &infos[hi]) {
                neighbors[lo].insert(hi);
                neighbors[hi].insert(lo);
                edges.push((lo, hi));
            }
        }

        // Reproduce the old emission order (outer index ascending, inner index
        // ascending) so serialization-point diagnostics stay byte-stable.
        edges.sort_unstable_by_key(|&(lo, hi)| (hi, lo));
        let mut serialization_points: Vec<SerializationPoint> = Vec::new();
        for (lo, hi) in edges {
            if let Some(mut sp) = self.conflict_detail(&infos[lo], &infos[hi]) {
                sp.statement_indices = vec![lo, hi];
                serialization_points.push(sp);
            }
        }

        (ConflictGraph { neighbors }, serialization_points)
    }

    /// Explain a single conflicting statement pair: the cause, the
    /// resource at issue, and (for an effect conflict) the callees whose
    /// effect on that resource forced the serialization. Mirrors the
    /// decision order of [`Self::statements_conflict`] and returns the
    /// first cause found. `statement_indices` is filled in by the caller.
    fn conflict_detail(&self, a: &StmtInfo, b: &StmtInfo) -> Option<SerializationPoint> {
        let mk = |reason: String,
                  resource: String,
                  blocking_callees: Vec<String>,
                  cause: SerializationCause| {
            Some(SerializationPoint {
                statement_indices: Vec::new(),
                reason,
                resource,
                blocking_callees,
                cause,
            })
        };

        if a.is_seq || b.is_seq {
            return mk(
                "explicit seq ordering".to_string(),
                String::new(),
                Vec::new(),
                SerializationCause::SeqOrdering,
            );
        }

        // Data dependency: one reads a binding the other defines. `a` is the
        // earlier statement, `b` the later (the caller passes ascending
        // indices), so `a.defines ∩ b.reads` is read-after-write (a true flow
        // dependency) and `b.defines ∩ a.reads` is write-after-read (an
        // anti-dependency). RAW dominates the `kind` tag when both are present.
        let raw_present = a.defines.intersection(&b.reads).next().is_some();
        let mut dep: Vec<&String> = a.defines.intersection(&b.reads).collect();
        dep.extend(b.defines.intersection(&a.reads));
        if !dep.is_empty() {
            dep.sort();
            dep.dedup();
            let names = dep
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let vars = dep.iter().map(|s| (*s).clone()).collect();
            let kind = if raw_present {
                DataDepKind::Raw
            } else {
                DataDepKind::War
            };
            return mk(
                format!("data dependency on {names}"),
                String::new(),
                Vec::new(),
                SerializationCause::DataDependency { kind, vars },
            );
        }

        // Write-write on the same binding.
        let mut ww: Vec<&String> = a.defines.intersection(&b.defines).collect();
        if !ww.is_empty() {
            ww.sort();
            let names = ww
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let vars = ww.iter().map(|s| (*s).clone()).collect();
            return mk(
                format!("both assign {names}"),
                String::new(),
                Vec::new(),
                SerializationCause::DataDependency {
                    kind: DataDepKind::WriteWrite,
                    vars,
                },
            );
        }

        // Polymorphic call: effects unknown at analysis time.
        if (a.calls_polymorphic && (b.calls_polymorphic || !b.effects.is_empty()))
            || (b.calls_polymorphic && !a.effects.is_empty())
        {
            return mk(
                "polymorphic-effect call — effects unknown at analysis time".to_string(),
                String::new(),
                Vec::new(),
                SerializationCause::PolymorphicEffect,
            );
        }

        // Effect conflict: find the conflicting effect pairs and attribute
        // them to the callees that contributed them.
        //
        // A2b-2 Phase 1/2: mirror `statements_conflict`'s network relaxations so
        // a reported cause is never a `Network`↔`Network` pair the grouper
        // actually treated as non-conflicting. For two ephemeral network
        // fan-outs, OR two method-call fan-outs on distinct receivers, the edge
        // that reached here must be a *non-Network* conflict, so skip
        // `Network`↔`Network` pairs and attribute the true cause.
        let distinct_method_fanout = match (
            &a.method_fanout_receiver_root,
            &b.method_fanout_receiver_root,
        ) {
            (Some(ra), Some(rb)) => ra != rb,
            _ => false,
        };
        let skip_network = (a.is_ephemeral_network_fanout && b.is_ephemeral_network_fanout)
            || distinct_method_fanout;
        let mut resource = String::new();
        let mut verbs: Option<(EffectVerbKind, EffectVerbKind)> = None;
        let mut callees: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for ae in &a.effects {
            for be in &b.effects {
                if skip_network && ae.resource == "Network" && be.resource == "Network" {
                    continue;
                }
                if self.two_effects_conflict(ae, be) {
                    if verbs.is_none() {
                        resource = ae.resource.clone();
                        verbs = Some((ae.verb.clone(), be.verb.clone()));
                    }
                    if ae.resource == be.resource && ae.resource == resource {
                        if let Some(c) = &ae.source_callee {
                            callees.insert(c.clone());
                        }
                        if let Some(c) = &be.source_callee {
                            callees.insert(c.clone());
                        }
                    }
                }
            }
        }
        if let Some((va, vb)) = verbs {
            let reason = format!(
                "{}({}) conflicts with {}({})",
                effect_verb_label(&va),
                resource,
                effect_verb_label(&vb),
                resource,
            );
            let cause = SerializationCause::EffectConflict {
                resource: resource.clone(),
                verbs: (va, vb),
            };
            return mk(reason, resource, callees.into_iter().collect(), cause);
        }

        None
    }

    /// Walk top-level statements in `func.body`; for each loop expression
    /// (`for` / `while` / `loop`), attempt to classify its body as a
    /// reduction over a single outer-scope accumulator. The classifier
    /// is intentionally conservative — anything outside the strict
    /// `acc = acc <op> expr` / `acc op= expr` shape (with op in the
    /// allow-list) returns no recognition. Codegen will re-validate the
    /// shape against type information before emitting the fan-out.
    fn recognize_reductions(&self, func: &Function) -> Vec<LoopReduction> {
        let mut out = Vec::new();
        self.recognize_reductions_in_block(&func.body, &mut out);
        out
    }

    /// Walk one block's statements for reduction-shaped loops, recursing
    /// into nested loop bodies and if-arms. Recursion (2026-07-15) is what
    /// lets a `#[par_unordered]` collect loop nested inside an outer
    /// sequential loop fan out — the LBM-substep shape (`while s < steps {
    /// … #[par_unordered] while c < n { out.push(f(grid[c])) } … }`),
    /// which the previous top-level-only walk silently left sequential.
    /// A `LoopReduction`'s `stmt_index` is the loop's index within ITS OWN
    /// block; codegen's lookup disambiguates by (stmt_index, loop_line),
    /// so equal indices across sibling blocks can't cross-match. Recursing
    /// into a body that is itself reduction-classified is deliberate: the
    /// runtime's fork-depth cap (`KARAC_PAR_MAX_FORK_DEPTH`) already makes
    /// inner regions run sequentially inline (see the recursion note
    /// below), so nested tags are safe.
    fn recognize_reductions_in_block(&self, block: &Block, out: &mut Vec<LoopReduction>) {
        for (idx, stmt) in block.stmts.iter().enumerate() {
            let StmtKind::Expr(expr) = &stmt.kind else {
                continue;
            };
            match &expr.kind {
                ExprKind::If {
                    then_block,
                    else_branch,
                    ..
                } => {
                    self.recognize_reductions_in_block(then_block, out);
                    if let Some(else_expr) = else_branch {
                        if let ExprKind::Block(else_block) = &else_expr.kind {
                            self.recognize_reductions_in_block(else_block, out);
                        }
                    }
                    continue;
                }
                ExprKind::For { .. } | ExprKind::While { .. } | ExprKind::Loop { .. } => {}
                _ => continue,
            }
            let (body, attributes) = match &expr.kind {
                ExprKind::For {
                    body, attributes, ..
                }
                | ExprKind::While {
                    body, attributes, ..
                }
                | ExprKind::Loop {
                    body, attributes, ..
                } => (body, attributes.as_slice()),
                _ => unreachable!("filtered above"),
            };
            self.recognize_reductions_in_block(body, out);
            if let Some((accumulator, op)) = self.classify_loop_body(body, attributes) {
                // B-2026-07-16-6 soundness gate: the reduction lowering runs
                // this body on MULTIPLE worker threads, so any value the body
                // touches that is reachable from outside one iteration is
                // visible to all workers. A plain `shared` (non-`par`) handle
                // carries a NON-ATOMIC refcount header — one racing
                // rc-inc/rc-dec pair across workers is a lost update that
                // under-counts the header and frees a still-referenced object
                // (use-after-free / double-free / heap corruption). The body
                // must therefore satisfy the same cross-task-safe predicate an
                // explicit `spawn` capture does; decline the reduction (the
                // loop lowers sequentially) when it doesn't.
                if !self.loop_body_types_cross_task_safe(body) {
                    continue;
                }
                // A reduction whose per-iteration delta recurses into the
                // enclosing function (e.g. a backtracking counter
                // `if legal { total = total + count(...deeper...) }`) is
                // recognized and lowered like any other. It used to be declined
                // here (B-2026-07-03-14) because parallelizing every recursion
                // level nested a parallel region per depth and exhausted the
                // stack — but the runtime now caps reduction fan-out depth
                // (`KARAC_PAR_MAX_FORK_DEPTH`, default 1, in `karac_par_reduce`),
                // so only the OUTERMOST level parallelizes and every deeper
                // level runs sequentially inline. That bounds nesting to a
                // constant and turns the crash into the useful case: a
                // backtracking search parallelized at its independent top-level
                // branches. The cost/shape gates in codegen still apply.
                let collect_tabulate = op == ReductionOp::Collect
                    && self.collect_is_tabulate_shape(body, &accumulator);
                out.push(LoopReduction {
                    accumulator,
                    op,
                    stmt_index: idx,
                    loop_line: expr.span.line,
                    collect_tabulate,
                    seq: false,
                });
            } else if !attributes.iter().any(|a| a.is_bare("par_unordered")) {
                // No reduction classified and no par opt-in: try the
                // SEQUENTIAL collect-tabulate shape. Unlike the par
                // classifier, other loop-carried writes (a scalar
                // accumulation alongside the push, extra counters) are
                // fine — the lowering compiles every non-push statement
                // inline in source order; only the push itself is
                // rewritten into an in-place store. The tabulate shape
                // check guarantees the accumulator appears exactly once,
                // as the receiver of one unconditional top-level push.
                if let Some(acc) = self.classify_seq_collect_tabulate(body, expr) {
                    out.push(LoopReduction {
                        accumulator: acc,
                        op: ReductionOp::Collect,
                        stmt_index: idx,
                        loop_line: expr.span.line,
                        collect_tabulate: true,
                        seq: true,
                    });
                }
            }
        }
    }

    /// Find the single outer-scope Vec accumulator of a sequential
    /// tabulate loop, if the body has that shape: exactly one top-level
    /// unconditional `acc.push(EXPR)` where `acc` is an outer binding
    /// mentioned nowhere else in the body (`collect_is_tabulate_shape`
    /// does the exactness check). Candidate discovery scans top-level
    /// bare pushes; two pushes to DIFFERENT accumulators is declined
    /// (each would fail the other's mention check anyway).
    ///
    /// LOOP-CONTROL immutability (B-2026-07-16-7): the tabulate lowering
    /// precomputes the trip count, so the body must not be able to
    /// change how many iterations the SOURCE loop would run. For a
    /// while-loop, any body write to a variable the condition reads
    /// (the counter itself, the bound, a `.len()` receiver) — other
    /// than the terminal step-one increment the codegen strips — makes
    /// the source trip count body-dependent: DECLINE. (The self-hosted
    /// lexer's `if escaped { i = i + 1 }` skip-advance inside a push
    /// loop is the live shape that miscompiled.) For a for-range loop
    /// the range is evaluated once up front in source semantics too, so
    /// bound writes are harmless — but a body write to the LOOP VAR
    /// still diverges (source rebinds it fresh each iteration; the
    /// lowering persists one alloca): DECLINE that as well.
    fn classify_seq_collect_tabulate(&self, body: &Block, loop_expr: &Expr) -> Option<String> {
        let mut candidate: Option<String> = None;
        let consider = |name: Option<String>, candidate: &mut Option<String>| -> bool {
            let Some(n) = name else { return true };
            match candidate {
                None => {
                    *candidate = Some(n);
                    true
                }
                Some(existing) => *existing == n,
            }
        };
        for stmt in &body.stmts {
            if let StmtKind::Expr(e) = &stmt.kind {
                if !consider(collect_push_shape(e), &mut candidate) {
                    return None;
                }
            }
        }
        if let Some(e) = &body.final_expr {
            if !consider(collect_push_shape(e), &mut candidate) {
                return None;
            }
        }
        let acc = candidate?;
        if !self.collect_is_tabulate_shape(body, &acc) {
            return None;
        }

        // ── Loop-control immutability gate. ──
        // Names the trip count depends on:
        let mut control_reads: HashSet<String> = HashSet::new();
        match &loop_expr.kind {
            ExprKind::While { condition, .. } => {
                self.collect_expr_reads(condition, &mut control_reads);
            }
            ExprKind::For {
                pattern, iterable, ..
            } => {
                // Range bounds are pre-evaluated in source semantics; only
                // the loop variable itself is control state.
                let _ = iterable;
                if let PatternKind::Binding(name) = &pattern.kind {
                    control_reads.insert(name.clone());
                }
            }
            _ => return None,
        }
        if control_reads.is_empty() {
            return Some(acc);
        }

        // Names the body writes — Assign/CompoundAssign targets plus
        // nested writes (if-arms, inner loops, mutating method
        // receivers) via the same walker the auto-par dependency check
        // trusts. Body-local rebindings are not loop-carried; the
        // while-form's TERMINAL step-one increment is the one exempted
        // write (extract_loop_shape strips it before codegen).
        let mut let_introduced: HashSet<String> = HashSet::new();
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    self.collect_pattern_bindings(pattern, &mut let_introduced);
                }
                StmtKind::LetUninit { name, .. } => {
                    let_introduced.insert(name.clone());
                }
                _ => {}
            }
        }
        let is_while = matches!(loop_expr.kind, ExprKind::While { .. });
        let last_idx = body.stmts.len().saturating_sub(1);
        let mut written: HashSet<String> = HashSet::new();
        for (i, stmt) in body.stmts.iter().enumerate() {
            match &stmt.kind {
                StmtKind::Assign { target, value } => {
                    if is_while && i == last_idx && body.final_expr.is_none() {
                        if let Some(name) = identifier_name(target) {
                            if induction_step_via_assign(value, &name) {
                                // The terminal counter step — stripped by
                                // extract_loop_shape, exempt here.
                                continue;
                            }
                        }
                    }
                    self.collect_assign_target_defines(target, &mut written);
                    self.collect_expr_inner_writes(value, &mut written);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.collect_assign_target_defines(target, &mut written);
                    self.collect_expr_inner_writes(value, &mut written);
                }
                StmtKind::Let { value, .. } => {
                    self.collect_expr_inner_writes(value, &mut written);
                }
                StmtKind::Expr(e) => {
                    self.collect_expr_inner_writes(e, &mut written);
                }
                _ => {}
            }
        }
        if let Some(e) = &body.final_expr {
            self.collect_expr_inner_writes(e, &mut written);
        }
        if written
            .iter()
            .any(|w| !let_introduced.contains(w) && control_reads.contains(w))
        {
            return None;
        }
        Some(acc)
    }

    /// B-2026-07-16-6: true when every typed expression inside `body`
    /// satisfies [`crate::cross_task_safe::is_cross_task_safe`] — the
    /// same predicate enforced on explicit `spawn` / `par {}` captures.
    ///
    /// Implementation is a span sweep over `expr_types` (every entry
    /// whose span lies inside the body block), NOT an AST walk: a walk
    /// has to enumerate every `ExprKind` and a missed variant silently
    /// reopens the soundness hole, while the sweep is shape-blind and
    /// stays exhaustive as the language grows. Deliberately conservative
    /// in two ways: a body-local FRESH `shared` object (thread-local for
    /// its whole life, so technically race-free) still declines, and a
    /// body expression with no `expr_types` entry contributes nothing
    /// (the racing values — reads of outer bindings and their
    /// projections — are bread-and-butter typed expressions). The cost
    /// of a false decline is a sequential loop, never a miscompile.
    ///
    /// Without type info (`self.types` is `None` — the untyped
    /// `concurrency_analyze` convenience entry used by analysis-only
    /// tests), recognition is left unchanged: every path that LOWERS a
    /// reduction (cli.rs `concurrencycheck`) runs the typed form.
    fn loop_body_types_cross_task_safe(&self, body: &Block) -> bool {
        let Some(tc) = self.types else {
            return true;
        };
        let lo = body.span.offset;
        let hi = body.span.offset + body.span.length;
        for (key, ty) in &tc.expr_types {
            let SpanKey(offset, length) = *key;
            if offset >= lo
                && offset + length <= hi
                && crate::cross_task_safe::is_cross_task_safe(ty, tc).is_err()
            {
                return false;
            }
        }
        true
    }

    /// Classify a loop body as a reduction over a single outer-scope
    /// accumulator. Returns `Some((name, op))` if every top-level
    /// loop-carried write to an outer-scope name is reduction-shaped
    /// against the same accumulator with the same op (with induction-
    /// shape writes — `i = i + const_lit`, `i += const_lit` — allowed
    /// alongside as loop-counter steps). Returns `None` for any other
    /// shape: multiple distinct accumulators, mixed ops, non-reduction
    /// writes, or writes nested inside `if`/`else`/inner-loop branches.
    fn classify_loop_body(
        &self,
        body: &Block,
        attributes: &[Attribute],
    ) -> Option<(String, ReductionOp)> {
        // `#[par_unordered]` opts into the collect-shape recognizer
        // (`acc.push(x)` and `if cond { acc.push(x); }`). Other loops
        // see only the scalar-reduction shapes. See
        // `phase-7-codegen.md` collect-style follow-on for the design.
        let par_unordered = attributes.iter().any(|a| a.is_bare("par_unordered"));
        // Names freshly introduced inside the loop body. Writes to these
        // are body-scoped and not loop-carried.
        let mut let_introduced: HashSet<String> = HashSet::new();
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    self.collect_pattern_bindings(pattern, &mut let_introduced);
                }
                StmtKind::LetUninit { name, .. } => {
                    let_introduced.insert(name.clone());
                }
                _ => {}
            }
        }

        let mut reduction: Option<(String, ReductionOp)> = None;
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::MultiAssign { .. } => unreachable!(
                    "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
                ),
                StmtKind::Assign { target, value } => {
                    let name = identifier_name(target)?;
                    if let_introduced.contains(&name) {
                        // Assign to a body-local name (re-bound after let).
                        // Not loop-carried; ignored.
                        continue;
                    }
                    // Induction shape is a strict subset of reduction shape
                    // (`i = i + 1` matches the `+` reduction check too) — so
                    // check induction first and short-circuit, otherwise an
                    // explicit `while`-loop counter would be tagged as the
                    // reduction accumulator and fight whichever real
                    // accumulator the loop also writes to.
                    if induction_step_via_assign(value, &name) {
                        // i = i + const_lit — loop-counter step; ignored.
                    } else {
                        let op = reduction_binary_shape(value, &name)?;
                        match reduction {
                            None => reduction = Some((name, op)),
                            Some((ref existing_name, existing_op)) => {
                                if existing_name != &name || existing_op != op {
                                    return None;
                                }
                            }
                        }
                    }
                }
                StmtKind::CompoundAssign { target, op, value } => {
                    let name = identifier_name(target)?;
                    if let_introduced.contains(&name) {
                        continue;
                    }
                    let Some(red_op) = ReductionOp::from_compound_op(op) else {
                        // Sub / Div / Mod / Shl / Shr — not in the
                        // associative + commutative allow-list.
                        return None;
                    };
                    // Mirror of the Assign-branch induction-first rule:
                    // `i += 1` matches the `+` reduction shape, so check
                    // for the counter-step shape first.
                    if red_op == ReductionOp::Add && is_int_literal(value) {
                        // i += const_lit — loop-counter step; ignored.
                        continue;
                    }
                    match reduction {
                        None => reduction = Some((name, red_op)),
                        Some((ref existing_name, existing_op)) => {
                            if existing_name != &name || existing_op != red_op {
                                return None;
                            }
                        }
                    }
                }
                StmtKind::Let { .. } | StmtKind::LetElse { .. } | StmtKind::LetUninit { .. } => {
                    // Fresh body bindings; not loop-carried.
                }
                StmtKind::Expr(expr) => {
                    // First: try the conditional-assign Min/Max desugar —
                    // `if x < acc { acc = x; }` and friends shape a
                    // recognized reduction step even though the inner-write
                    // check below would otherwise reject any if-stmt that
                    // writes an outer-scope name.
                    if let Some((name, op)) = conditional_minmax_shape(expr) {
                        if let_introduced.contains(&name) {
                            // Body-local accumulator; ignore.
                            continue;
                        }
                        match reduction {
                            None => reduction = Some((name, op)),
                            Some((ref existing_name, existing_op)) => {
                                if existing_name != &name || existing_op != op {
                                    return None;
                                }
                            }
                        }
                        continue;
                    }
                    // Next: conditional accumulator-update shape —
                    // `if cond { acc = acc + delta; }` (and the OP=
                    // form). Semantically equivalent to
                    // `acc = acc + (if cond { delta } else { 0 })`,
                    // so reducible under the same associative+commutative
                    // op as the unconditional form. The condition must
                    // not read the accumulator (order-dependent), which
                    // the helper verifies.
                    if let Some((name, op)) = self.conditional_acc_update_shape(expr) {
                        if let_introduced.contains(&name) {
                            continue;
                        }
                        match reduction {
                            None => reduction = Some((name, op)),
                            Some((ref existing_name, existing_op)) => {
                                if existing_name != &name || existing_op != op {
                                    return None;
                                }
                            }
                        }
                        continue;
                    }
                    // Collect-style recognition (Phase 2 — gated on
                    // `#[par_unordered]`). Two shapes:
                    //   acc.push(EXPR)                                  (bare)
                    //   if cond { acc.push(EXPR); }                     (conditional)
                    // The combine model is per-worker partial Vecs
                    // concat'd in worker-order, so the output ordering
                    // differs from iteration-order — the attribute is
                    // the user's explicit opt-in to that property. Push
                    // arg expressions are accepted as-is (no acc-read
                    // restriction inside them is needed for correctness:
                    // the arg is per-iter data, evaluated within the
                    // worker's slice, never folded with sibling workers'
                    // partials before final concat).
                    if par_unordered {
                        if let Some(name) = collect_push_shape(expr) {
                            if let_introduced.contains(&name) {
                                continue;
                            }
                            match reduction {
                                None => reduction = Some((name, ReductionOp::Collect)),
                                Some((ref existing_name, existing_op)) => {
                                    if existing_name != &name || existing_op != ReductionOp::Collect
                                    {
                                        return None;
                                    }
                                }
                            }
                            continue;
                        }
                        if let Some(name) = self.conditional_collect_shape(expr) {
                            if let_introduced.contains(&name) {
                                continue;
                            }
                            match reduction {
                                None => reduction = Some((name, ReductionOp::Collect)),
                                Some((ref existing_name, existing_op)) => {
                                    if existing_name != &name || existing_op != ReductionOp::Collect
                                    {
                                        return None;
                                    }
                                }
                            }
                            continue;
                        }
                    }
                    // Else: any inner write to an outer-scope name (via
                    // nested if/else or inner loop) breaks the simple-
                    // reduction recognition; defer multi-write loops to a
                    // later slice.
                    let mut inner_writes = HashSet::new();
                    self.collect_expr_inner_writes(expr, &mut inner_writes);
                    for w in &inner_writes {
                        if !let_introduced.contains(w) {
                            return None;
                        }
                    }
                }
                StmtKind::Defer { .. } | StmtKind::ErrDefer { .. } => {
                    // Defers run at scope exit, not per-iteration; treat
                    // conservatively as a rejection signal — a defer with
                    // a captured-write reads its surrounding loop's
                    // accumulator state in a way the fan-out / combine
                    // model doesn't preserve.
                    return None;
                }
            }
        }

        // Same audit on the block's trailing expression. A loop body that
        // ends with `if x < acc { acc = x; }` (no trailing semicolon)
        // parses the if as `final_expr` rather than `Stmt::Expr`; the
        // conditional-assign recognizer must fire here too or the kata-153
        // shape (`for i in 1..n { let x = nums[i]; if x < m { m = x; } }`)
        // silently falls back to sequential.
        if let Some(e) = &body.final_expr {
            if let Some((name, op)) = conditional_minmax_shape(e) {
                if !let_introduced.contains(&name) {
                    match reduction {
                        None => reduction = Some((name, op)),
                        Some((ref existing_name, existing_op)) => {
                            if existing_name != &name || existing_op != op {
                                return None;
                            }
                        }
                    }
                }
            } else if let Some((name, op)) = self.conditional_acc_update_shape(e) {
                if !let_introduced.contains(&name) {
                    match reduction {
                        None => reduction = Some((name, op)),
                        Some((ref existing_name, existing_op)) => {
                            if existing_name != &name || existing_op != op {
                                return None;
                            }
                        }
                    }
                }
            } else if par_unordered {
                // Mirror of the StmtKind::Expr collect-shape arm above.
                // Trailing-expression position (no semicolon on the last
                // collect step) — analogous to `conditional_minmax_shape`
                // landing in both stmt + final_expr positions.
                if let Some(name) =
                    collect_push_shape(e).or_else(|| self.conditional_collect_shape(e))
                {
                    if !let_introduced.contains(&name) {
                        match reduction {
                            None => reduction = Some((name, ReductionOp::Collect)),
                            Some((ref existing_name, existing_op)) => {
                                if existing_name != &name || existing_op != ReductionOp::Collect {
                                    return None;
                                }
                            }
                        }
                    }
                } else {
                    let mut inner_writes = HashSet::new();
                    self.collect_expr_inner_writes(e, &mut inner_writes);
                    for w in &inner_writes {
                        if !let_introduced.contains(w) {
                            return None;
                        }
                    }
                }
            } else {
                let mut inner_writes = HashSet::new();
                self.collect_expr_inner_writes(e, &mut inner_writes);
                for w in &inner_writes {
                    if !let_introduced.contains(w) {
                        return None;
                    }
                }
            }
        }

        reduction
    }

    /// Recognize the conditional collect shape:
    ///
    ///   if cond { acc.push(EXPR); }
    ///   if cond { acc.push(EXPR); } else { /* empty */ }
    ///
    /// Returns `Some(acc_name)` when the if-stmt wraps a single
    /// `acc.push(_)` method call. Like the conditional-acc-update
    /// helper, the else-branch must be absent OR an empty block; a
    /// two-arm version (push different values in each arm) is left to
    /// a follow-on if a workload surfaces it. The condition is **not**
    /// required to be acc-free here — `acc.len()` queries inside the
    /// condition are workload-relative but never read partial state
    /// across workers, since each worker's local Vec is independent
    /// until the final concat. The combine model treats every push as
    /// contributing one element to the parent's Vec; ordering is
    /// already worker-driven, so the condition's per-iter timing
    /// doesn't add an extra ordering hazard.
    fn conditional_collect_shape(&self, expr: &Expr) -> Option<String> {
        let ExprKind::If {
            condition: _,
            then_block,
            else_branch,
        } = &expr.kind
        else {
            return None;
        };
        if let Some(else_expr) = else_branch {
            let ExprKind::Block(b) = &else_expr.kind else {
                return None;
            };
            if !b.stmts.is_empty() || b.final_expr.is_some() {
                return None;
            }
        }
        if then_block.stmts.len() != 1 || then_block.final_expr.is_some() {
            return None;
        }
        let StmtKind::Expr(inner) = &then_block.stmts[0].kind else {
            return None;
        };
        collect_push_shape(inner)
    }

    /// Is a Collect-classified loop body **tabulate-shaped**: exactly one
    /// top-level bare `acc.push(EXPR)` per iteration — no conditional
    /// pushes, no second push, and `acc` mentioned nowhere else in the
    /// body (including `let` initializers and the push's own argument)?
    ///
    /// Tabulate lets workers write elements directly into a shared
    /// presized buffer at their global iteration index, so the invariant
    /// "iteration i produces exactly output element i" must be airtight:
    /// a body that could push more than once per iteration overflows its
    /// chunk view (and the push grow-path would `free` an interior
    /// pointer), and one that could skip a push leaves garbage holes.
    /// Skips can't happen — `continue` anywhere in the body already
    /// rejects the whole lowering via `block_has_early_exit` — so this
    /// check only has to bound the push count from above, which it does
    /// by requiring the single bare push to be the ONLY mention of `acc`.
    /// Mention-detection over-approximates via `collect_expr_reads` ∪
    /// `collect_expr_inner_writes` (an `Identifier(acc)` anywhere,
    /// receiver positions included, registers as a read). Any shape this
    /// declines still lowers through the partial-Vecs path — declining
    /// costs performance, never correctness.
    fn collect_is_tabulate_shape(&self, body: &Block, acc: &str) -> bool {
        // A body-local rebinding of the accumulator name makes every
        // later mention ambiguous between the two; decline outright.
        let mut let_introduced: HashSet<String> = HashSet::new();
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    self.collect_pattern_bindings(pattern, &mut let_introduced);
                }
                StmtKind::LetUninit { name, .. } => {
                    let_introduced.insert(name.clone());
                }
                _ => {}
            }
        }
        if let_introduced.contains(acc) {
            return false;
        }

        let mentions_acc = |e: &Expr| -> bool {
            let mut names = HashSet::new();
            self.collect_expr_reads(e, &mut names);
            self.collect_expr_inner_writes(e, &mut names);
            names.contains(acc)
        };

        let mut bare_pushes = 0usize;
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::Expr(expr) => {
                    if collect_push_shape(expr).as_deref() == Some(acc) {
                        bare_pushes += 1;
                        let ExprKind::MethodCall { args, .. } = &expr.kind else {
                            return false;
                        };
                        if mentions_acc(&args[0].value) {
                            return false;
                        }
                        continue;
                    }
                    // A conditional push means a variable per-iter count.
                    if self.conditional_collect_shape(expr).as_deref() == Some(acc) {
                        return false;
                    }
                    if mentions_acc(expr) {
                        return false;
                    }
                }
                StmtKind::Let { value, .. } => {
                    if mentions_acc(value) {
                        return false;
                    }
                }
                StmtKind::Assign { target, value }
                | StmtKind::CompoundAssign { target, value, .. } => {
                    if mentions_acc(target) || mentions_acc(value) {
                        return false;
                    }
                }
                // LetElse's else-block diverges (break/return), which
                // `block_has_early_exit` rejects downstream anyway;
                // Defer never reaches here (classify_loop_body returns
                // None); MultiAssign is desugared away. Decline all
                // three defensively rather than reasoning about them.
                StmtKind::LetElse { .. }
                | StmtKind::LetUninit { .. }
                | StmtKind::Defer { .. }
                | StmtKind::ErrDefer { .. }
                | StmtKind::MultiAssign { .. } => return false,
            }
        }
        if let Some(e) = &body.final_expr {
            if collect_push_shape(e).as_deref() == Some(acc) {
                bare_pushes += 1;
                let ExprKind::MethodCall { args, .. } = &e.kind else {
                    return false;
                };
                if mentions_acc(&args[0].value) {
                    return false;
                }
            } else if self.conditional_collect_shape(e).as_deref() == Some(acc) || mentions_acc(e) {
                // A conditional push (variable count) or any other
                // accumulator mention — decline.
                return false;
            }
        }
        bare_pushes == 1
    }

    /// Recognize the conditional-accumulator-update shape:
    ///
    ///   if cond { acc = acc + delta; }                              (1-arm)
    ///   if cond { acc OP= delta; }                                  (1-arm CompoundAssign)
    ///   if cond { acc = acc + delta; } else { /* empty */ }         (1-arm + empty else)
    ///   if cond { acc = acc + a; } else { acc = acc + b; }          (2-arm — added 2026-05-20)
    ///   if cond { acc OP= a; }     else { acc OP= b; }              (2-arm CompoundAssign)
    ///
    /// Returns `Some((acc_name, op))` when both arms (or the single
    /// then-arm with absent/empty else) update the same outer-scope
    /// accumulator with the same op. The transformation that justifies
    /// recognizing this as a reduction is:
    ///
    ///   1-arm: if cond { acc = acc + d }      ≡  acc = acc + (if cond { d } else { 0 })
    ///   2-arm: if cond { acc = acc + a }
    ///          else     { acc = acc + b }     ≡  acc = acc + (if cond { a } else { b })
    ///
    /// In both cases the per-iteration contribution is order-independent
    /// for any associative+commutative op with a known identity, so the
    /// par-reduce fan-out + combine model preserves the final value.
    ///
    /// Constraints checked:
    /// - The then-block is exactly one statement of the recognized
    ///   accumulator-update shape (Assign with `reduction_binary_shape`
    ///   match, or CompoundAssign with an allow-listed op).
    /// - The else-branch, if present, is either empty (1-arm shape) or
    ///   exactly one statement of the same update shape, writing the
    ///   *same* accumulator name with the *same* op (mixed ops like
    ///   `if c { acc += 1 } else { acc *= 2 }` are rejected — combine
    ///   ordering only commutes within one op).
    /// - The condition expression does NOT read the accumulator —
    ///   otherwise the per-iter decision depends on accumulator state
    ///   produced by earlier iterations, which is order-dependent and
    ///   not preserved by the fan-out / combine model. Delta expressions
    ///   are guarded transitively via `reduction_binary_shape` (which
    ///   requires acc to appear exactly once on the RHS, so the
    ///   non-acc operand is acc-free by construction) and via the
    ///   CompoundAssign arm's no-self-reference assumption.
    fn conditional_acc_update_shape(&self, expr: &Expr) -> Option<(String, ReductionOp)> {
        let ExprKind::If {
            condition,
            then_block,
            else_branch,
        } = &expr.kind
        else {
            return None;
        };
        // The then-block must be exactly one accumulator-update stmt.
        let (acc_name, op) = single_stmt_block_as_acc_update(then_block)?;
        // The else-branch, when present, may be empty (1-arm shape) or a
        // single matching update for the same (acc, op).
        if let Some(else_expr) = else_branch {
            let ExprKind::Block(b) = &else_expr.kind else {
                return None;
            };
            if b.final_expr.is_some() {
                return None;
            }
            match b.stmts.len() {
                0 => { /* empty else — 1-arm shape with explicit empty else */ }
                1 => {
                    let (else_acc, else_op) = single_stmt_as_acc_update(&b.stmts[0])?;
                    if else_acc != acc_name || else_op != op {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        // Final guard: condition must not reference the accumulator.
        let mut cond_reads: HashSet<String> = HashSet::new();
        self.collect_expr_reads(condition, &mut cond_reads);
        if cond_reads.contains(&acc_name) {
            return None;
        }
        Some((acc_name, op))
    }

    /// Analyze a single statement to extract defines, reads, and effects.
    /// `ref_params` is the set of `ref`/`mut ref` parameter names of the
    /// enclosing function (a receiver rooted at one may be caller-aliased, so
    /// it is excluded from method-call network fan-out — Slice 2).
    fn analyze_stmt(&self, stmt: &Stmt, is_seq: bool, ref_params: &HashSet<String>) -> StmtInfo {
        let mut info = StmtInfo {
            defines: HashSet::new(),
            let_introduced: HashSet::new(),
            reads: HashSet::new(),
            called_fn_names: HashSet::new(),
            effects: Vec::new(),
            calls_polymorphic: false,
            is_seq,
            has_early_exit: stmt_has_early_exit(stmt),
            has_channel_op: stmt_has_channel_op(stmt),
            has_console_output: stmt_has_console_output(stmt),
            is_timer_suspend: stmt_is_timer_suspend(stmt),
            // Set below, once `effects` is populated — it needs the effect set
            // to confirm the call touches the `Network` resource.
            is_safe_network_fanout: false,
            is_ephemeral_network_fanout: false,
            method_fanout_receiver_root: None,
            is_constant_init: stmt_is_constant_init(stmt),
        };

        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { pattern, value, .. } => {
                // The pattern defines variables
                self.collect_pattern_bindings(pattern, &mut info.defines);
                self.collect_pattern_bindings(pattern, &mut info.let_introduced);
                // The value expression may read variables and call functions
                self.collect_expr_reads(value, &mut info.reads);
                self.collect_expr_effects(value, &mut info);
                // The RHS may also WRITE outer state as a side effect — a
                // `mut ref self` / `mut ref T` call mutates its receiver / a
                // `mut`-passed argument. Record those writes so a later stmt
                // that reads (or writes) the same place serializes against
                // this one. Without this, `let then_block = self.parse_block()`
                // recorded no write on `self`, so three sequential
                // cursor-advancing `self.parse_*()` calls looked independent
                // and the auto-parallelizer raced them (B-2026-07-09-12).
                // Mirrors the `StmtKind::Expr` arm's inner-write collection.
                self.collect_expr_inner_writes(value, &mut info.defines);
            }
            StmtKind::LetUninit { name, .. } => {
                info.defines.insert(name.clone());
                info.let_introduced.insert(name.clone());
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.collect_pattern_bindings(pattern, &mut info.defines);
                self.collect_pattern_bindings(pattern, &mut info.let_introduced);
                self.collect_expr_reads(value, &mut info.reads);
                self.collect_expr_effects(value, &mut info);
                // RHS side-effect writes (mut-ref receiver / mut arg) — see the
                // `StmtKind::Let` arm (B-2026-07-09-12).
                self.collect_expr_inner_writes(value, &mut info.defines);
                self.collect_block_reads(else_block, &mut info.reads);
                self.collect_block_effects(else_block, &mut info);
                self.collect_block_inner_writes(else_block, &mut info.defines);
            }
            StmtKind::Assign { target, value } => {
                // The target is being written to
                self.collect_assign_target_defines(target, &mut info.defines);
                // But the target may also read (e.g. array[idx] = val reads idx)
                self.collect_assign_target_reads(target, &mut info.reads);
                self.collect_expr_reads(value, &mut info.reads);
                self.collect_expr_effects(value, &mut info);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.collect_assign_target_defines(target, &mut info.defines);
                self.collect_assign_target_reads(target, &mut info.reads);
                // Compound assign also reads the target
                self.collect_expr_reads(target, &mut info.reads);
                self.collect_expr_reads(value, &mut info.reads);
                self.collect_expr_effects(value, &mut info);
            }
            StmtKind::Expr(expr) => {
                self.collect_expr_reads(expr, &mut info.reads);
                self.collect_expr_effects(expr, &mut info);
                // Nested Assigns (e.g. inside a `for v in nums.iter() {
                // if v > cap { cap = v; } }`) write to outer-scope
                // names — record them in `info.defines` so subsequent
                // stmts that read those names create a data dependency
                // and serialize against this stmt. Without this, a
                // for-loop body's `cap = v` is invisible to
                // `statements_conflict` and the analyzer groups stmts
                // that should be sequential.
                self.collect_expr_inner_writes(expr, &mut info.defines);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.collect_block_reads(body, &mut info.reads);
                self.collect_block_effects(body, &mut info);
                self.collect_block_inner_writes(body, &mut info.defines);
            }
        }

        // A2b-2: a network call (touches the `Network` resource) whose args
        // move in no owned binding is exempt from the coroutine-boundary gate.
        // Both halves are required: the arg-safety proves no double-drop, and
        // the Network-resource check keeps a non-network user `with suspends`
        // fn serial (its independence isn't established here). Now that
        // `info.effects` is populated, combine them.
        info.is_safe_network_fanout =
            stmt_fanout_args_safe(stmt, &self.function_bodies, &self.method_bodies)
                && info.effects.iter().any(|e| e.resource == "Network");

        // A2b-2 Phase 1: an ephemeral network fan-out is a safe network fan-out
        // whose callee borrows nothing — so it cannot share a connection object
        // with a sibling call, and its `Network` ops touch a freshly-opened,
        // private connection. Two of them may overlap; `statements_conflict`
        // relaxes the `Network`↔`Network` conflict for such pairs.
        info.is_ephemeral_network_fanout = info.is_safe_network_fanout
            && stmt_callee_has_no_borrow_params(stmt, &self.function_bodies, &self.method_bodies);

        // A2b-2 Phase 2 Slice 2: method-call network fan-out. A method call
        // `obj.method(args)` touching `Network` with a borrowed receiver of a
        // distinct-provable (non-shared, non-ref-param) local is a candidate;
        // record its receiver root for the distinct-receiver conflict relaxation.
        if info.effects.iter().any(|e| e.resource == "Network") {
            info.method_fanout_receiver_root = self.classify_method_fanout(stmt, ref_params);
        }

        info
    }

    /// A2b-2 Phase 2 Slice 2: classify a statement as a method-call network
    /// fan-out CANDIDATE, returning its receiver ROOT identifier if admissible.
    /// Requires (all fail-closed to `None`): the statement is `obj.method(args)`
    /// whose (1) receiver `obj` is a plain identifier that is NOT a `ref`/`mut ref`
    /// parameter of the enclosing fn (a ref param may be caller-aliased); (2)
    /// receiver type — from `method_callee_types` — is NOT a `shared` (RC) type
    /// (which can alias via `let b = a`); (3) resolved method BORROWS its receiver
    /// (`ref self`/`mut ref self`, never a consuming `own self` that would move it
    /// into the coroutine and double-drop); and (4) args are fan-out-safe (same
    /// rule as `stmt_fanout_args_safe`). The Network-resource check is applied by
    /// the caller. Needs type info; `None` without it.
    fn classify_method_fanout(&self, stmt: &Stmt, ref_params: &HashSet<String>) -> Option<String> {
        let value = match &stmt.kind {
            StmtKind::Let { value, .. } | StmtKind::Expr(value) => value,
            _ => return None,
        };
        let ExprKind::MethodCall {
            object,
            args,
            method,
            ..
        } = &value.kind
        else {
            return None;
        };
        // (1) Receiver is a plain local identifier, not a ref/mut-ref param.
        let ExprKind::Identifier(root) = &object.kind else {
            return None;
        };
        if ref_params.contains(root) {
            return None;
        }
        // (2) Receiver type is known (from `method_callee_types`, keyed by the
        // method-call span — which equals the receiver span) and NOT shared.
        // The stored value is the full `"TypeName.method"` key.
        let key = self
            .types?
            .method_callee_types
            .get(&SpanKey::from_span(&value.span))?;
        let recv_ty = key.rsplit_once('.').map(|(t, _)| t)?;
        if self.shared_type_names.contains(recv_ty) {
            return None;
        }
        let _ = method; // method identity is carried by `key`; kept for clarity
                        // (3) Resolved method borrows its receiver (not consuming).
        let func = self.method_bodies.get(key)?;
        if !matches!(
            func.self_param,
            Some(SelfParam::Ref) | Some(SelfParam::MutRef)
        ) {
            return None;
        }
        // (4) Args fan-out-safe: literal / const, or an identifier at a borrow
        // parameter position (mirrors `stmt_fanout_args_safe`).
        let callee_params = Some(func.params.as_slice());
        let args_safe = args.iter().enumerate().all(|(i, a)| {
            expr_is_call_free(&a.value)
                && (expr_is_binding_free(&a.value)
                    || (matches!(a.value.kind, ExprKind::Identifier(_))
                        && param_is_borrow(callee_params, i)))
        });
        if !args_safe {
            return None;
        }
        Some(root.clone())
    }

    /// Check if two statements conflict (have a dependency requiring serialization).
    fn statements_conflict(&self, a: &StmtInfo, b: &StmtInfo) -> bool {
        // If either is in a seq block, force serialization
        if a.is_seq || b.is_seq {
            return true;
        }

        // Data dependency: B reads something A defines, or A reads something B defines
        if !a.defines.is_disjoint(&b.reads) || !b.defines.is_disjoint(&a.reads) {
            return true;
        }

        // Write-write conflict on same variable
        if !a.defines.is_disjoint(&b.defines) {
            return true;
        }

        // Polymorphic calls have unknown effects at analysis time — conflict
        // with any other stmt that has effect activity.
        if a.calls_polymorphic && (b.calls_polymorphic || !b.effects.is_empty()) {
            return true;
        }
        if b.calls_polymorphic && !a.effects.is_empty() {
            return true;
        }

        // A2b-2 Phase 1: two *ephemeral* network fan-outs (borrow-free free-fn
        // network calls — e.g. `http_get("a"); http_get("b")`) open disjoint,
        // freshly-created connections, so their `Network`-resource effects
        // (`sends`/`receives`) do not conflict. Skip only `Network`↔`Network`
        // pairs; any *other* shared resource a callee touches still serializes
        // through `two_effects_conflict` (a data dependency was already ruled
        // out above). See `is_ephemeral_network_fanout` and
        // docs/spikes/network-resource-granularity.md.
        if a.is_ephemeral_network_fanout && b.is_ephemeral_network_fanout {
            return self.effects_conflict_excluding_network(&a.effects, &b.effects);
        }

        // A2b-2 Phase 2 Slice 2: two method-call network fan-outs on DISTINCT,
        // provably-non-aliasing receivers (`s1.read(); s2.read()`) touch distinct
        // connections, so their `Network` effects do not conflict. Same-root
        // calls never reach this relaxation: a `mut ref self` method defines its
        // receiver, so the write-write check above already serialized them, and a
        // `ref self` same-root pair is excluded by the `ra != rb` guard here.
        // Shared-type / `ref`-param receivers (which could alias under a distinct
        // name) are excluded from candidacy in `classify_method_fanout`.
        if let (Some(ra), Some(rb)) = (
            &a.method_fanout_receiver_root,
            &b.method_fanout_receiver_root,
        ) {
            if ra != rb {
                return self.effects_conflict_excluding_network(&a.effects, &b.effects);
            }
        }

        // Effect conflict
        self.effects_conflict(&a.effects, &b.effects)
    }

    /// Like [`Self::effects_conflict`] but ignores every effect pair where
    /// BOTH sides touch the `Network` resource. Used only for two ephemeral
    /// network fan-outs (see [`Self::statements_conflict`]): their network I/O
    /// is on disjoint fresh connections, so `Network`↔`Network` is safe, while
    /// any non-`Network` resource conflict they carry is still honored.
    fn effects_conflict_excluding_network(
        &self,
        a_effects: &[StmtEffect],
        b_effects: &[StmtEffect],
    ) -> bool {
        for a in a_effects {
            for b in b_effects {
                if a.resource == "Network" && b.resource == "Network" {
                    continue;
                }
                if self.two_effects_conflict(a, b) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if two sets of effects have a conflict.
    fn effects_conflict(&self, a_effects: &[StmtEffect], b_effects: &[StmtEffect]) -> bool {
        for a in a_effects {
            for b in b_effects {
                if self.two_effects_conflict(a, b) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if two individual effects conflict.
    ///
    /// Conflict rules:
    /// - Same resource:
    ///   - reads + reads = NO conflict
    ///   - reads + writes = CONFLICT
    ///   - writes + writes = CONFLICT
    ///   - sends + sends = CONFLICT
    ///   - receives + receives = CONFLICT
    ///   - allocates + allocates = CONFLICT (same resource)
    ///   - panics + panics = CONFLICT
    ///   - blocks + blocks = NO conflict — execution verb drives placement, not
    ///     conflict (A1, 2026-06-10; design.md:5907/:5920)
    ///   - suspends + suspends = NO conflict — execution verb, same as blocks
    ///     (design.md:5907/:5920). Only matters for stmts the boundary gate lets
    ///     through, i.e. standalone `sleep_ms` timer waits (A2b, 2026-06-10):
    ///     two overlap via the par thread-block path. Channel `recv` / network
    ///     parks also carry `suspends` but are excluded upstream by
    ///     `effects_mark_coroutine_boundary` before this check is reached.
    ///   - Cross-category (e.g. reads + sends) = NO conflict even on same resource
    /// - Different resources = NO conflict regardless of verbs
    fn two_effects_conflict(&self, a: &StmtEffect, b: &StmtEffect) -> bool {
        // Different resources never conflict
        if a.resource != b.resource {
            return false;
        }

        // A2b-2 Phase 2 Slice 3: parameterized-resource PARTITION KEYS. Two
        // accesses to the same resource with DISTINCT compile-time-literal keys
        // touch different partitions (`writes(Db[1])` vs `writes(Db[2])`) and
        // never conflict — the `design.md § Parameterized Resources`
        // proven-disjoint case. Any other combination (equal keys =
        // proven-identical; a `None` key = unparameterized or a non-literal
        // "unproven" arg) falls through to the verb-based check below, so it
        // conservatively conflicts — "silent under-serialization is never
        // accepted". `key` is only ever `Some` for a resource declared with a
        // `[param]`, so unparameterized effects are unaffected (additive).
        if let (Some(ka), Some(kb)) = (&a.key, &b.key) {
            if ka != kb {
                return false;
            }
        }

        // Same resource: check verb categories
        use EffectVerbKind::*;

        // Group 1: reads/writes — same category
        // Group 2: sends/receives — same category
        // Group 3: allocates — informational, NOT a conflict (A3a; design.md)
        // Group 4: panics — informational, NOT a conflict (A3b; design.md)
        // Group 5: blocks — execution verb, NOT a conflict (A1; design.md:5907)
        // Group 6: suspends — execution verb, NOT a conflict (A2b; the
        //          `(Suspends,Suspends) => false` arm below). General
        //          suspends/network still serialize, but via the upstream
        //          `effects_mark_coroutine_boundary` gate in
        //          `find_parallel_groups`, not this conflict arm — only
        //          `sleep_ms` timer waits (which clear the gate) reach here.
        // Cross-group: no conflict

        match (&a.verb, &b.verb) {
            // reads + reads = safe
            (Reads, Reads) => false,
            // reads + writes or writes + reads = CONFLICT
            (Reads, Writes) | (Writes, Reads) => true,
            // writes + writes = CONFLICT
            (Writes, Writes) => true,

            // sends + sends = CONFLICT
            (Sends, Sends) => true,
            // receives + receives = CONFLICT
            (Receives, Receives) => true,
            // sends + receives = safe (same resource, different direction)
            (Sends, Receives) | (Receives, Sends) => false,

            // allocates + allocates = NO conflict. `allocates` is an
            // *informational* resource verb (design.md: only reads/writes +
            // sends/receives drive conflict) — the heap allocator is
            // thread-safe, so two independent allocating statements may run
            // concurrently. The diagnostics-side `effectchecker.rs::two_effects_conflict`
            // already returns `false` here ("allocates, panics are
            // informational"); this aligns the auto-par conflict model with it.
            // Unlike `suspends`/network, `allocates` is NOT a coroutine
            // boundary (`effects_mark_coroutine_boundary` excludes it), so the
            // by-value double-drop hazard does not apply — the same reasoning
            // that made the A1 `blocks` flip safe. Lifted in A3 (2026-06-19);
            // see phase-5-diagnostics.md.
            (Allocates, Allocates) => false,
            // panics + panics = NO conflict. `panics` is *informational* too
            // (design.md: only reads/writes + sends/receives drive conflict),
            // and `effectchecker.rs::effects_conflict` already treats it as
            // non-conflicting. This unblocks auto-par for ordinary arithmetic:
            // `/` and `%` infer `panics` (the div/rem-by-zero guard), which is
            // why `examples/parallax_lite` had to avoid them to keep its groups.
            // Safe because a Kāra panic lowers to `emit_panic` = `printf` +
            // `exit(1)` (`src/codegen/runtime.rs`), a direct process exit — NOT
            // a Rust unwind. So a panic inside a `par_run` worker terminates the
            // whole process fail-fast (identical to a sequential panic: the
            // release runtime is built `panic = "abort"`, and worker-panic →
            // process-abort is the documented intended `par {}` semantics, see
            // the `[profile.release]` comment in Cargo.toml). No unwinding means
            // no double-drop and nothing to "propagate" — the same worker-exit
            // path already runs for explicit `par {}` and the A1/A3a groups.
            // Like `allocates`, `panics` is NOT a coroutine boundary. The
            // common case — a `/`/`%` that does not actually divide by zero —
            // simply computes concurrently. Lifted in A3b (2026-06-19);
            // see phase-5-diagnostics.md.
            (Panics, Panics) => false,
            // blocks + blocks = NO conflict. Execution verbs answer PLACEMENT,
            // not conflict (design.md:5907/:5920) — two independent blocking
            // calls overlap on the blocking pool via the same `emit_par_run`
            // fan-out that explicit `par {}` uses. Lifted in A1 (2026-06-10);
            // see phase-5-diagnostics.md and bench/auto_par_io/.
            (Blocks, Blocks) => false,
            // suspends + suspends = NO conflict. Like `blocks`, `suspends` is an
            // execution verb that answers PLACEMENT, not conflict
            // (design.md:5907/:5920). This arm is reached ONLY for stmts that
            // clear `effects_mark_coroutine_boundary` — i.e. standalone
            // `sleep_ms` timer waits (`stmt_is_timer_suspend`), the one
            // `suspends` form proven independent (a bare timer park, no by-value
            // `Drop` params). Two of them overlap via the `emit_par_run`
            // thread-block path exactly like `blocks` (A2b, 2026-06-10).
            // Channel `recv` and network parks also carry `suspends` but never
            // reach here — the boundary gate excludes them upstream (a channel
            // recv has a happens-before with its producer; lifting it deadlocks,
            // and a network coroutine owns + drops by-value params; the network
            // fan-out is A2b-2).
            (Suspends, Suspends) => false,

            // User-defined verbs: conflict if same verb on same resource
            (UserDefined(va), UserDefined(vb)) => va == vb,

            // Cross-category: no conflict
            _ => false,
        }
    }

    /// Find groups of statements that can run in parallel.
    /// Uses a greedy approach: walk statements in order, grouping consecutive
    /// independent statements.
    /// Collect the names of locals bound to a HEAP-OWNING CONTAINER type
    /// (`Vec` / `String` / `Map` / `Set` / `SortedMap` / `SortedSet`) anywhere
    /// in `block`, recursively. Classification prefers the `let`'s explicit
    /// annotation; unannotated bindings use the typechecker's recorded pattern
    /// type (`pattern_binding_types`, keyed by the pattern span). Name-based
    /// on purpose: a same-named container binding in ANY scope conservatively
    /// marks the name (over-marking only de-parallelizes — it can never
    /// introduce a race). Feeds `ParallelGroup::captured_container_mutations`
    /// (B-2026-07-15-2).
    fn collect_container_locals(&self, block: &Block) -> HashSet<String> {
        fn type_name_is_container(name: &str) -> bool {
            let head = name.split(['[', ' ']).next().unwrap_or("");
            matches!(
                head,
                "Vec" | "String" | "Map" | "Set" | "SortedMap" | "SortedSet"
            )
        }
        fn type_expr_is_container(te: &TypeExpr) -> bool {
            match &te.kind {
                TypeKind::Path(p) => p.segments.last().is_some_and(|s| type_name_is_container(s)),
                _ => false,
            }
        }
        fn walk_block(this: &ConcurrencyChecker, block: &Block, out: &mut HashSet<String>) {
            for stmt in &block.stmts {
                walk_stmt(this, stmt, out);
            }
            if let Some(fe) = &block.final_expr {
                walk_expr(this, fe, out);
            }
        }
        fn classify_let(
            this: &ConcurrencyChecker,
            pattern: &Pattern,
            ty: &Option<TypeExpr>,
            out: &mut HashSet<String>,
        ) {
            let PatternKind::Binding(name) = &pattern.kind else {
                return;
            };
            let is_container = match ty {
                Some(te) => type_expr_is_container(te),
                None => this
                    .types
                    .and_then(|t| {
                        t.pattern_binding_types
                            .get(&SpanKey::from_span(&pattern.span))
                    })
                    .is_some_and(|n| type_name_is_container(n)),
            };
            if is_container {
                out.insert(name.clone());
            }
        }
        fn walk_stmt(this: &ConcurrencyChecker, stmt: &Stmt, out: &mut HashSet<String>) {
            match &stmt.kind {
                StmtKind::Let {
                    pattern, ty, value, ..
                } => {
                    classify_let(this, pattern, ty, out);
                    walk_expr(this, value, out);
                }
                StmtKind::LetUninit { name, ty, .. } => {
                    if type_expr_is_container(ty) {
                        out.insert(name.clone());
                    }
                }
                StmtKind::LetElse {
                    pattern,
                    ty,
                    value,
                    else_block,
                    ..
                } => {
                    classify_let(this, pattern, ty, out);
                    walk_expr(this, value, out);
                    walk_block(this, else_block, out);
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    walk_block(this, body, out);
                }
                StmtKind::Assign { target, value } => {
                    walk_expr(this, target, out);
                    walk_expr(this, value, out);
                }
                StmtKind::MultiAssign { targets, values } => {
                    for e in targets.iter().chain(values.iter()) {
                        walk_expr(this, e, out);
                    }
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    walk_expr(this, target, out);
                    walk_expr(this, value, out);
                }
                StmtKind::Expr(e) => walk_expr(this, e, out),
            }
        }
        fn walk_expr(this: &ConcurrencyChecker, e: &Expr, out: &mut HashSet<String>) {
            match &e.kind {
                ExprKind::Block(b)
                | ExprKind::Seq(b)
                | ExprKind::Par(b)
                | ExprKind::Unsafe(b)
                | ExprKind::Try(b)
                | ExprKind::Comptime(b) => walk_block(this, b, out),
                ExprKind::LabeledBlock { body, .. } => walk_block(this, body, out),
                ExprKind::If {
                    condition,
                    then_block,
                    else_branch,
                } => {
                    walk_expr(this, condition, out);
                    walk_block(this, then_block, out);
                    if let Some(eb) = else_branch {
                        walk_expr(this, eb, out);
                    }
                }
                ExprKind::IfLet {
                    value,
                    then_block,
                    else_branch,
                    ..
                } => {
                    walk_expr(this, value, out);
                    walk_block(this, then_block, out);
                    if let Some(eb) = else_branch {
                        walk_expr(this, eb, out);
                    }
                }
                ExprKind::Match { scrutinee, arms } => {
                    walk_expr(this, scrutinee, out);
                    for arm in arms {
                        walk_expr(this, &arm.body, out);
                    }
                }
                ExprKind::While { body, .. }
                | ExprKind::WhileLet { body, .. }
                | ExprKind::For { body, .. }
                | ExprKind::Loop { body, .. } => walk_block(this, body, out),
                ExprKind::Lock { body, .. } => walk_block(this, body, out),
                ExprKind::Providers { body, .. } => walk_block(this, body, out),
                _ => {}
            }
        }
        let mut out = HashSet::new();
        walk_block(self, block, &mut out);
        out
    }

    /// Locals whose type OWNS non-RC heap — a bare container
    /// (`Vec`/`String`/`Map`/`Set`/`SortedMap`/`SortedSet`) or an
    /// `Option[..]`/`Result[..]` whose payload carries one. These are the
    /// bindings for which a par-branch capture is a bit-copy of an OWNING
    /// header: a consuming use inside the branch (payload move-out, owned
    /// call arg) frees heap the parent's scope-exit cleanup still references
    /// (B-2026-07-16-19). `shared` payloads are excluded — RC capture
    /// bookkeeping keeps each side's counts balanced, so a consumed
    /// `Option[SharedNode]` capture is safe.
    ///
    /// Classification mirrors `collect_container_locals`: the declared
    /// annotation when present, else the typechecker's recorded binding type
    /// (string form). `HashMap` value is `true` when the type is an
    /// `Option`/`Result` wrapper (whose combinator METHODS consume `self`),
    /// `false` for a bare container (whose methods are ref-self dominated).
    /// Does the named user type (enum/struct) transitively OWN non-RC heap
    /// — a `String`/`Vec`/`Map`/`Set`/… payload or field, directly or
    /// through a nested non-`shared` aggregate? Drives the B-2026-07-22-9
    /// move-hazard classification: a bare owned-heap enum/struct binding
    /// produced in a par branch and then MOVED (`let c = a`, owned call
    /// arg) double-frees through the par-return writeback, exactly like the
    /// `Option`/`Result` payload case (B-2026-07-16-19). `shared`/`par`
    /// aggregates are RC-managed (balanced retain/release across the branch
    /// bit-copy) and excluded. `visited` guards recursive enums
    /// (`enum List { Nil, Cons(i64, List) }`).
    fn named_type_owns_heap(&self, name: &str, visited: &mut HashSet<String>) -> bool {
        if name.is_empty() || !visited.insert(name.to_string()) {
            return false;
        }
        for item in &self.program.items {
            match item {
                Item::EnumDef(e) if e.name == name => {
                    if e.is_shared || e.is_par {
                        return false;
                    }
                    return e.variants.iter().any(|v| match &v.kind {
                        VariantKind::Unit => false,
                        VariantKind::Tuple(tys) => {
                            tys.iter().any(|t| self.type_expr_owns_heap(t, visited))
                        }
                        VariantKind::Struct(fs) => {
                            fs.iter().any(|f| self.type_expr_owns_heap(&f.ty, visited))
                        }
                    });
                }
                Item::StructDef(s) if s.name == name => {
                    if s.is_shared || s.is_par {
                        return false;
                    }
                    return s
                        .fields
                        .iter()
                        .any(|f| self.type_expr_owns_heap(&f.ty, visited));
                }
                _ => {}
            }
        }
        false
    }

    /// Type-expr twin of [`Self::named_type_owns_heap`]: does this type own
    /// non-RC heap? A bare heap container, an `Option`/`Result` with a heap
    /// payload, or a user enum/struct that transitively owns heap.
    fn type_expr_owns_heap(&self, te: &TypeExpr, visited: &mut HashSet<String>) -> bool {
        match &te.kind {
            TypeKind::Path(p) => {
                let head = p.segments.last().map(String::as_str).unwrap_or("");
                if matches!(
                    head,
                    "String" | "Vec" | "Map" | "Set" | "SortedMap" | "SortedSet"
                ) {
                    return true;
                }
                if matches!(head, "Option" | "Result") {
                    return p.generic_args.iter().flatten().any(
                        |a| matches!(a, GenericArg::Type(t) if self.type_expr_owns_heap(t, visited)),
                    );
                }
                self.named_type_owns_heap(head, visited)
            }
            _ => false,
        }
    }

    fn collect_move_hazard_locals(&self, block: &Block) -> HashMap<String, bool> {
        fn head_is_container(head: &str) -> bool {
            matches!(
                head,
                "Vec" | "String" | "Map" | "Set" | "SortedMap" | "SortedSet"
            )
        }
        fn type_expr_hazard(this: &ConcurrencyChecker, te: &TypeExpr) -> Option<bool> {
            match &te.kind {
                TypeKind::Path(p) => {
                    let head = p.segments.last().map(String::as_str).unwrap_or("");
                    if head_is_container(head) {
                        return Some(false);
                    }
                    if matches!(head, "Option" | "Result") {
                        let payload_hazard = p.generic_args.iter().flatten().any(
                            |a| matches!(a, GenericArg::Type(t) if type_expr_hazard(this, t).is_some()),
                        );
                        if payload_hazard {
                            return Some(true);
                        }
                    }
                    // A user enum/struct that transitively owns heap is a
                    // move-hazard too (B-2026-07-22-9) — classified
                    // non-wrapper (`false`): its methods aren't `Option`/
                    // `Result` combinators, it just must not be
                    // par-produced-then-moved.
                    if this.named_type_owns_heap(head, &mut HashSet::new()) {
                        return Some(false);
                    }
                    None
                }
                _ => None,
            }
        }
        // Semantic-`Type` twin for the un-annotated case, resolved from the
        // typechecker's `expr_types` keyed by the LET RHS's span — the
        // `pattern_binding_types` string records only the head name
        // ("Option"), losing the payload that decides hazard-ness.
        fn semantic_type_hazard(
            this: &ConcurrencyChecker,
            t: &crate::typechecker::types::Type,
        ) -> Option<bool> {
            use crate::typechecker::types::Type as T;
            match t {
                T::Str => Some(false),
                T::Named { name, args } => {
                    if head_is_container(name) {
                        return Some(false);
                    }
                    if matches!(name.as_str(), "Option" | "Result")
                        && args.iter().any(|a| semantic_type_hazard(this, a).is_some())
                    {
                        return Some(true);
                    }
                    // User enum/struct transitively owning heap
                    // (B-2026-07-22-9) — the un-annotated `let a = mk_nums()`
                    // twin of the `type_expr_hazard` enum/struct arm.
                    if this.named_type_owns_heap(name, &mut HashSet::new()) {
                        return Some(false);
                    }
                    None
                }
                _ => None,
            }
        }
        fn classify_let(
            this: &ConcurrencyChecker,
            pattern: &Pattern,
            ty: &Option<TypeExpr>,
            value_span: Option<&crate::token::Span>,
            out: &mut HashMap<String, bool>,
        ) {
            let PatternKind::Binding(name) = &pattern.kind else {
                return;
            };
            let hazard = match ty {
                Some(te) => type_expr_hazard(this, te),
                None => value_span.and_then(|vs| {
                    this.types
                        .and_then(|t| t.expr_types.get(&SpanKey::from_span(vs)))
                        .and_then(|t| semantic_type_hazard(this, t))
                }),
            };
            if let Some(is_wrapper) = hazard {
                out.insert(name.clone(), is_wrapper);
            }
        }
        fn walk_block(this: &ConcurrencyChecker, block: &Block, out: &mut HashMap<String, bool>) {
            for stmt in &block.stmts {
                walk_stmt(this, stmt, out);
            }
            if let Some(fe) = &block.final_expr {
                walk_expr(this, fe, out);
            }
        }
        fn walk_stmt(this: &ConcurrencyChecker, stmt: &Stmt, out: &mut HashMap<String, bool>) {
            match &stmt.kind {
                StmtKind::Let {
                    pattern, ty, value, ..
                } => {
                    classify_let(this, pattern, ty, Some(&value.span), out);
                    walk_expr(this, value, out);
                }
                StmtKind::LetUninit { name, ty, .. } => {
                    if let Some(is_wrapper) = type_expr_hazard(this, ty) {
                        out.insert(name.clone(), is_wrapper);
                    }
                }
                StmtKind::LetElse {
                    pattern,
                    ty,
                    value,
                    else_block,
                    ..
                } => {
                    classify_let(this, pattern, ty, Some(&value.span), out);
                    walk_expr(this, value, out);
                    walk_block(this, else_block, out);
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    walk_block(this, body, out);
                }
                StmtKind::Assign { target, value } => {
                    walk_expr(this, target, out);
                    walk_expr(this, value, out);
                }
                StmtKind::MultiAssign { targets, values } => {
                    for e in targets.iter().chain(values.iter()) {
                        walk_expr(this, e, out);
                    }
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    walk_expr(this, target, out);
                    walk_expr(this, value, out);
                }
                StmtKind::Expr(e) => walk_expr(this, e, out),
            }
        }
        fn walk_expr(this: &ConcurrencyChecker, e: &Expr, out: &mut HashMap<String, bool>) {
            match &e.kind {
                ExprKind::Block(b)
                | ExprKind::Seq(b)
                | ExprKind::Par(b)
                | ExprKind::Unsafe(b)
                | ExprKind::Try(b)
                | ExprKind::Comptime(b) => walk_block(this, b, out),
                ExprKind::LabeledBlock { body, .. } => walk_block(this, body, out),
                ExprKind::If {
                    condition,
                    then_block,
                    else_branch,
                } => {
                    walk_expr(this, condition, out);
                    walk_block(this, then_block, out);
                    if let Some(eb) = else_branch {
                        walk_expr(this, eb, out);
                    }
                }
                ExprKind::IfLet {
                    value,
                    then_block,
                    else_branch,
                    ..
                } => {
                    walk_expr(this, value, out);
                    walk_block(this, then_block, out);
                    if let Some(eb) = else_branch {
                        walk_expr(this, eb, out);
                    }
                }
                ExprKind::Match { scrutinee, arms } => {
                    walk_expr(this, scrutinee, out);
                    for arm in arms {
                        walk_expr(this, &arm.body, out);
                    }
                }
                ExprKind::While { body, .. }
                | ExprKind::WhileLet { body, .. }
                | ExprKind::For { body, .. }
                | ExprKind::Loop { body, .. } => walk_block(this, body, out),
                ExprKind::Lock { body, .. } => walk_block(this, body, out),
                ExprKind::Providers { body, .. } => walk_block(this, body, out),
                _ => {}
            }
        }
        let mut out = HashMap::new();
        walk_block(self, block, &mut out);
        out
    }

    /// The set of move-hazard locals this statement CONSUMES — reads that
    /// transfer heap ownership out of the binding, so the stmt must not run
    /// in a par-branch worker while the parent still owns the original
    /// (B-2026-07-16-19). Consuming shapes recognized:
    ///
    ///   * `match X { .. }` / `if let P = X` on a bare hazard binding where
    ///     some arm pattern binds a payload out (the proven-broken repro:
    ///     the branch moves the payload into the arm binding and frees it,
    ///     the parent's scope-exit payload free fires again);
    ///   * a METHOD call on a bare `Option`/`Result` hazard receiver — the
    ///     combinator family (`unwrap*`/`map*`/`ok`/`take`/..) consumes
    ///     `self` (bare-container receivers stay eligible: their methods are
    ///     ref-self dominated, and gating them would de-parallelize the
    ///     bread-and-butter `v.iter().sum()` reader workers);
    ///   * a bare hazard binding as a call arg in an OWNED parameter
    ///     position — a user free fn whose param is neither `ref` /
    ///     `mut ref` / `mut Slice`, or a `Some`/`Ok`/`Err` constructor
    ///     (unresolvable callees — builtins like `println` — are treated as
    ///     borrowing);
    ///   * a bare hazard binding as ANY method-call argument (`v2.push(s)`
    ///     moves; read-only bare-container method args are rare enough that
    ///     the over-approximation costs little);
    ///   * a bare hazard binding as a `let`/`Assign` RHS (alias-move), a
    ///     struct-literal / array / tuple element (move into aggregate), or
    ///     a `for` iterable (owned iteration).
    ///
    /// Names introduced by the statement itself are the caller's job to
    /// subtract (see `analyze_function`).
    ///
    /// `count_wrapper_method_receiver` gates the `Option`/`Result` combinator-
    /// method-receiver case (`a.unwrap_or(..)` consumes `a`). The CONSUMER
    /// guard passes `true` (it must not enter a par group). The PRODUCER guard
    /// (B-2026-07-22-9) passes `false`: a published slot consumed by a wrapper
    /// combinator is made round-trip-safe by B-2026-07-17-4's branch-side
    /// publish suppression + parent re-registration, so its producer stays
    /// parallelizable — only genuine alias/owned MOVES of the published binding
    /// (`let c = a`, owned-arg, aggregate element, for-iterable) double-free
    /// with the writeback and must de-parallelize the producer.
    fn stmt_consuming_hazard_reads(
        &self,
        stmt: &Stmt,
        hazards: &HashMap<String, bool>,
        count_wrapper_method_receiver: bool,
    ) -> HashSet<String> {
        fn bare_name(e: &Expr) -> Option<&str> {
            match &e.kind {
                ExprKind::Identifier(n) => Some(n.as_str()),
                _ => None,
            }
        }
        struct W<'a> {
            this: &'a ConcurrencyChecker<'a>,
            hazards: &'a HashMap<String, bool>,
            count_wrapper_method_receiver: bool,
            out: HashSet<String>,
        }
        impl W<'_> {
            fn mark_if_hazard(&mut self, e: &Expr) {
                if let Some(n) = bare_name(e) {
                    if self.hazards.contains_key(n) {
                        self.out.insert(n.to_string());
                    }
                }
            }
            fn callee_param_owned(&self, callee: &str, idx: usize) -> bool {
                match self.this.function_bodies.get(callee) {
                    Some(f) => f.params.get(idx).is_none_or(|p| {
                        !matches!(
                            p.ty.kind,
                            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
                        )
                    }),
                    // Unresolvable callee: builtins (`println`, `assert`, ..)
                    // borrow their args — treat as non-consuming.
                    None => false,
                }
            }
            fn block(&mut self, b: &Block) {
                for s in &b.stmts {
                    self.stmt(s);
                }
                if let Some(fe) = &b.final_expr {
                    self.expr(fe);
                }
            }
            fn stmt(&mut self, s: &Stmt) {
                match &s.kind {
                    StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                        self.mark_if_hazard(value);
                        self.expr(value);
                        if let StmtKind::LetElse { else_block, .. } = &s.kind {
                            self.block(else_block);
                        }
                    }
                    StmtKind::LetUninit { .. } => {}
                    StmtKind::Assign { target, value } => {
                        self.mark_if_hazard(value);
                        self.expr(target);
                        self.expr(value);
                    }
                    StmtKind::MultiAssign { targets, values } => {
                        for v in values {
                            self.mark_if_hazard(v);
                        }
                        for e in targets.iter().chain(values.iter()) {
                            self.expr(e);
                        }
                    }
                    StmtKind::CompoundAssign { target, value, .. } => {
                        self.expr(target);
                        self.expr(value);
                    }
                    StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                        self.block(body);
                    }
                    StmtKind::Expr(e) => self.expr(e),
                }
            }
            fn expr(&mut self, e: &Expr) {
                match &e.kind {
                    ExprKind::Match { scrutinee, arms } => {
                        if let Some(n) = bare_name(scrutinee) {
                            if self.hazards.contains_key(n)
                                && arms.iter().any(|a| !a.pattern.binding_names().is_empty())
                            {
                                self.out.insert(n.to_string());
                            }
                        }
                        self.expr(scrutinee);
                        for arm in arms {
                            self.expr(&arm.body);
                        }
                    }
                    ExprKind::IfLet {
                        pattern,
                        value,
                        then_block,
                        else_branch,
                    } => {
                        if let Some(n) = bare_name(value) {
                            if self.hazards.contains_key(n) && !pattern.binding_names().is_empty() {
                                self.out.insert(n.to_string());
                            }
                        }
                        self.expr(value);
                        self.block(then_block);
                        if let Some(eb) = else_branch {
                            self.expr(eb);
                        }
                    }
                    ExprKind::MethodCall { object, args, .. } => {
                        if let Some(n) = bare_name(object) {
                            // Wrapper (`Option`/`Result`) receivers: the
                            // combinator family consumes self. Counted for the
                            // consumer guard; excluded for the producer guard
                            // (B-2026-07-17-4 makes the published-slot round-
                            // trip safe — see the fn doc comment).
                            if self.count_wrapper_method_receiver
                                && self.hazards.get(n).copied() == Some(true)
                            {
                                self.out.insert(n.to_string());
                            }
                        }
                        for a in args {
                            self.mark_if_hazard(&a.value);
                        }
                        self.expr(object);
                        for a in args {
                            self.expr(&a.value);
                        }
                    }
                    ExprKind::Call { callee, args } => {
                        if let Some(cn) = bare_name(callee) {
                            let is_ctor = matches!(cn, "Some" | "Ok" | "Err");
                            for (i, a) in args.iter().enumerate() {
                                if let Some(n) = bare_name(&a.value) {
                                    if self.hazards.contains_key(n)
                                        && (is_ctor || self.callee_param_owned(cn, i))
                                    {
                                        self.out.insert(n.to_string());
                                    }
                                }
                            }
                        }
                        self.expr(callee);
                        for a in args {
                            self.expr(&a.value);
                        }
                    }
                    ExprKind::For { iterable, body, .. } => {
                        self.mark_if_hazard(iterable);
                        self.expr(iterable);
                        self.block(body);
                    }
                    ExprKind::StructLiteral { fields, spread, .. } => {
                        for f in fields {
                            self.mark_if_hazard(&f.value);
                            self.expr(&f.value);
                        }
                        if let Some(sp) = spread {
                            self.expr(sp);
                        }
                    }
                    ExprKind::ArrayLiteral(elems) | ExprKind::Tuple(elems) => {
                        for el in elems {
                            self.mark_if_hazard(el);
                            self.expr(el);
                        }
                    }
                    ExprKind::Block(b)
                    | ExprKind::Seq(b)
                    | ExprKind::Par(b)
                    | ExprKind::Unsafe(b)
                    | ExprKind::Try(b)
                    | ExprKind::Comptime(b) => self.block(b),
                    ExprKind::LabeledBlock { body, .. } => self.block(body),
                    ExprKind::If {
                        condition,
                        then_block,
                        else_branch,
                    } => {
                        self.expr(condition);
                        self.block(then_block);
                        if let Some(eb) = else_branch {
                            self.expr(eb);
                        }
                    }
                    ExprKind::While {
                        condition, body, ..
                    } => {
                        self.expr(condition);
                        self.block(body);
                    }
                    ExprKind::WhileLet { value, body, .. } => {
                        self.expr(value);
                        self.block(body);
                    }
                    ExprKind::Loop { body, .. } => self.block(body),
                    ExprKind::Lock { body, .. } | ExprKind::Providers { body, .. } => {
                        self.block(body)
                    }
                    ExprKind::Binary { left, right, .. } => {
                        self.expr(left);
                        self.expr(right);
                    }
                    ExprKind::Unary { operand, .. } => self.expr(operand),
                    ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                        self.expr(object)
                    }
                    ExprKind::Index { object, index } => {
                        self.expr(object);
                        self.expr(index);
                    }
                    ExprKind::Range { start, end, .. } => {
                        if let Some(s) = start {
                            self.expr(s);
                        }
                        if let Some(en) = end {
                            self.expr(en);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut w = W {
            this: self,
            hazards,
            count_wrapper_method_receiver,
            out: HashSet::new(),
        };
        w.stmt(stmt);
        w.out
    }

    #[allow(clippy::too_many_arguments)] // B-2026-07-22-9 fix threads a second (producer-only) hazard set
    fn find_parallel_groups(
        &self,
        infos: &[StmtInfo],
        graph: &ConflictGraph,
        n: usize,
        container_locals: &HashSet<String>,
        consuming_hazard_reads: &[HashSet<String>],
        moving_hazard_reads: &[HashSet<String>],
        enclosing_fn: &str,
    ) -> Vec<ParallelGroup> {
        let mut groups: Vec<ParallelGroup> = Vec::new();
        let mut assigned = vec![false; n];

        // B-2026-07-22-9: a statement that PRODUCES a move-hazard binding
        // later consumed by a MOVE cannot be auto-parallelized either — the
        // dual of the consumer guard below (B-2026-07-16-19). When the
        // producer runs in a `__par_branch` worker, its owned heap value is
        // written back to the parent frame via the par-return slot (a
        // bit-copy of the {tag,ptr,len,cap} header); a later sequential
        // `let c = a` / owned-arg move of that binding then double-frees —
        // the move's source-null and the writeback copy don't compose, so
        // both the moved-into binding's scope-exit drop and the residual
        // writeback copy free the same buffer. De-parallelizing only the
        // CONSUMER (line ~3763) is insufficient: the consumer already runs
        // in the sequential tail there, yet the PRODUCER stays grouped and
        // the double-free still fires. The proven-broken repro is a
        // String-payload enum temp describe() sibling (which seeds the
        // group) beside a `let a = mk_nums()` Vec-payload producer whose `a`
        // is then `let c = a`-moved; either alone is clean (no group forms),
        // only their coexistence parallelizes the producer. Sequential is
        // always correct; auto-par is only an optimization.
        let mut hazard_producer = vec![false; n];
        for i in 0..n {
            let produces_consumed_hazard = infos[i].let_introduced.iter().any(|name| {
                // Consumed-by-MOVE by any LATER statement (the writeback +
                // move race is strictly forward: the producer's value is
                // handed to the parent, then a subsequent stmt moves it).
                // Uses `moving_hazard_reads`, not `consuming_hazard_reads`:
                // wrapper-combinator consumption of a published slot is safe
                // (B-2026-07-17-4) and must not de-parallelize the producer.
                ((i + 1)..n).any(|j| moving_hazard_reads[j].contains(name))
            });
            hazard_producer[i] = produces_consumed_hazard;
        }

        // For each unassigned statement, try to build a maximal parallel group
        for start in 0..n {
            if assigned[start] {
                continue;
            }

            // B-2026-07-22-9 seed guard: see `hazard_producer` above.
            if hazard_producer[start] {
                assigned[start] = true;
                continue;
            }

            // A statement that may exit the function early (contains `return`,
            // `break`, or `continue`) cannot share a par group with any
            // sibling — the par branch's `void` LLVM signature can't carry
            // the inner `ret X` and module verification fails.
            if infos[start].has_early_exit {
                assigned[start] = true;
                continue;
            }

            // A statement that calls a coroutine network-boundary fn — or any
            // `suspends` park that is not a standalone `sleep_ms` timer wait —
            // must NOT be auto-parallelized into a `__par_branch` worker: a
            // coroutine owns + drops its by-value params while auto-par captures
            // are shared-with-write-back (a `__par_branch`-lifted call would
            // double-drop an owned user-`Drop` arg), and a channel `recv` has a
            // happens-before with its producer that a fan-out would deadlock.
            // A direct `sleep_ms` timer park is the one independent `suspends`
            // form, so it is exempted and overlaps like `blocks` (A2b). See
            // `effects_mark_coroutine_boundary` / `stmt_is_timer_suspend`.
            if effects_mark_coroutine_boundary(&infos[start].effects)
                && !infos[start].is_timer_suspend
                && !infos[start].is_safe_network_fanout
                && infos[start].method_fanout_receiver_root.is_none()
            {
                assigned[start] = true;
                continue;
            }

            // A channel operation (`Channel.new` / `send` / `recv` /
            // `try_recv`) is never auto-parallelized — channels are explicit
            // communication primitives whose send-before-recv ordering a
            // par fan-out would break (and whose channel-end bindings would
            // be isolated into a branch's captured scope). Mirrors the
            // early-exit / coroutine-boundary seed guards. See
            // `stmt_has_channel_op`.
            if infos[start].has_channel_op {
                assigned[start] = true;
                continue;
            }

            // B-2026-07-16-19: a statement that CONSUMES a move-hazard local
            // captured from outside itself (moves heap ownership out of a
            // `match`/`if let` payload, an owned call arg, a bare-RHS alias)
            // must not run in a par-branch worker: the branch's move
            // machinery suppresses/frees only the branch's bit-copied env
            // alloca, while the parent's scope-exit cleanup still fires on
            // the original — a double-free the stmt-vs-stmt conflict graph
            // cannot model (the conflicting "read" is the parent's implicit
            // scope-exit drop, not a sibling statement). Sequential is
            // always correct; auto-par is only an optimization.
            if !consuming_hazard_reads[start].is_empty() {
                assigned[start] = true;
                continue;
            }

            // Console-output statements (`println` / `print` / `eprintln` / a
            // `Stdout`/`Stderr` write) are NOT suppressed here. They carry no
            // resource effect, so the conflict gate treats them as independent
            // and they fan out — but the runtime captures each branch's output
            // and replays it in branch (= source) order at the join
            // (`karac_par_run`'s ordered-output capture), so observable output
            // is byte-identical to sequential execution. This reverses
            // B-2026-06-13-18's blanket suppression, which traded away the
            // parallelism of logging-bearing independent work (the Parallax
            // demo's per-fetch trace) to avoid the race the buffering now
            // eliminates. See phase-6-runtime.md "Auto-par ordered output".

            let mut group_indices = vec![start];
            assigned[start] = true;

            // Try to add subsequent unassigned statements to this group.
            //
            // **Contiguous-only invariant.** A parallel group must be a
            // contiguous run of statements: code before the group runs
            // sequentially, the group fans out at one point through
            // `karac_par_run`, then code after the group runs
            // sequentially. Non-contiguous groups violate this — they
            // imply two interleaved fan-outs that the single-fan-out
            // runtime cannot express, and the codegen's
            // `i = max_idx + 1` step would skip past the second
            // group's stmts entirely. So when a candidate isn't
            // independent of the in-progress group, we **break**, not
            // continue — the group ends here and any later eligible
            // candidate becomes the seed of its own group.
            for candidate in (start + 1)..n {
                if assigned[candidate] {
                    break;
                }

                // Same rule applied to candidates: an early-exit stmt
                // ends the par group at its sibling boundary.
                if infos[candidate].has_early_exit {
                    break;
                }

                // A coroutine network-boundary statement (or a non-timer
                // `suspends` park) is never auto-parallelized — it must not join
                // a group seeded by a pure sibling either (see the seed-side
                // guard above). A direct `sleep_ms` timer wait is exempt and may
                // join. End the group at any other boundary.
                if effects_mark_coroutine_boundary(&infos[candidate].effects)
                    && !infos[candidate].is_timer_suspend
                    && !infos[candidate].is_safe_network_fanout
                    && infos[candidate].method_fanout_receiver_root.is_none()
                {
                    break;
                }

                // A channel-op statement ends the group at its sibling
                // boundary too (seed-side guard's candidate mirror).
                if infos[candidate].has_channel_op {
                    break;
                }

                // Consuming read of a move-hazard capture ends the group too
                // (seed-side guard's candidate mirror, B-2026-07-16-19).
                if !consuming_hazard_reads[candidate].is_empty() {
                    break;
                }

                // A move-hazard PRODUCER whose binding is consumed-by-move
                // later ends the group too (seed-side guard's candidate
                // mirror, B-2026-07-22-9).
                if hazard_producer[candidate] {
                    break;
                }

                // Console-output statements may JOIN a group (no candidate-side
                // break): the runtime's ordered-output capture preserves their
                // program-order observability across the fan-out. See the
                // seed-side note above and phase-6-runtime.md.

                // Check if candidate is independent of ALL statements already in the group
                let independent = group_indices
                    .iter()
                    .all(|&g| !graph.conflicts(candidate, g));

                if independent {
                    group_indices.push(candidate);
                    assigned[candidate] = true;
                } else {
                    break;
                }
            }

            // SELF-RECURSION gate (B-2026-07-15-4): a group whose statement
            // calls the enclosing function is a recursive divide-and-conquer
            // (`let left = build(..); let right = build(..)`). Auto-par
            // spawns per call with no sequential cutoff, so EVERY recursion
            // level re-dispatches (~70µs each, O(nodes) dispatches per
            // top-level call) — measured 175x wall-time regression on a
            // 15-node tree build at 20k reps, sys-time-dominated, identical
            // output. Until a work-stealing scheduler with a lazy sequential
            // cutoff exists, these groups run sequentially. Direct
            // self-calls only (bare fn name / method name); mutual recursion
            // through a helper is a documented residual.
            let is_self_recursive = group_indices
                .iter()
                .any(|&i| infos[i].called_fn_names.contains(enclosing_fn));

            // Only emit groups with more than 1 statement (parallelism requires >= 2)
            if group_indices.len() > 1 && !is_self_recursive {
                let reason = self.describe_group_reason(infos, &group_indices);
                // A group is trivial when running it in parallel can produce
                // no measurable speedup, so the `karac_par_run` spawn cost
                // (~70μs per dispatch on macOS) is pure overhead. Two cases:
                //
                // 1. All stmts are pure (no effects, no polymorphic calls) —
                //    the codegen could eliminate them, no point parallelizing.
                // 2. At most one stmt does meaningful work — the rest are
                //    constant-init lets/assigns that produce ~zero work for
                //    a par branch. The structural parallelism is zero (one
                //    branch holds all the work, the others idle through a
                //    join). Surfaced by the kata 6 zigzag bench 2026-05-17,
                //    where `convert_off` was forking three par groups per
                //    call (each shaped "one big loop + N let-binds"), adding
                //    2.2s of system-call time over 10K calls for no speedup.
                let all_pure = group_indices
                    .iter()
                    .all(|&i| infos[i].effects.is_empty() && !infos[i].calls_polymorphic);
                let non_constant_count = group_indices
                    .iter()
                    .filter(|&&i| !infos[i].is_constant_init)
                    .count();
                let is_trivial = all_pure || non_constant_count <= 1;
                // Union of (defines − let_introduced) across the group's
                // stmts. Names in this set name *captured* locals that
                // some branch will mutate without introducing them as a
                // fresh binding — the codegen needs this to bail when
                // those mutations would otherwise be lost across the
                // par-run join.
                let mut captured_mutations: HashSet<String> = HashSet::new();
                for &i in &group_indices {
                    for name in infos[i].defines.difference(&infos[i].let_introduced) {
                        captured_mutations.insert(name.clone());
                    }
                }
                let captured_container_mutations = captured_mutations
                    .intersection(container_locals)
                    .cloned()
                    .collect();
                groups.push(ParallelGroup {
                    statement_indices: group_indices,
                    reason,
                    is_trivial,
                    captured_mutations,
                    captured_container_mutations,
                });
            }
        }

        groups
    }

    /// Flag parallelism the *contiguous-only* grouper leaves on the table:
    /// pairs of mutually-independent statements that did not co-group only
    /// because they are non-adjacent in source order, where a legal reorder
    /// (one permitted by the data + effect dependency graph) would make them
    /// adjacent. This is the deterministic "a better order exists" advisory
    /// for the agent-driven reorder loop (phase-5-diagnostics.md option 1):
    /// instead of *guessing* that a reorder helps, the agent reads a sound
    /// dependency signal, applies it, and re-runs `check` / `query` to
    /// confirm. No transformation happens here.
    ///
    /// A pair `(i, j)`, `i < j`, is reported when:
    /// - they are independent (`!graph.conflicts(i, j)`) — they *could* run in
    ///   parallel;
    /// - they are non-adjacent (`j > i + 1`) — adjacency is what the grouper
    ///   already exploits, so only a gap represents missed parallelism;
    /// - at least one of them is currently **serial** (not in a multi-stmt
    ///   parallel group) — so acting on it adds parallelism rather than just
    ///   reshuffling two already-parallel statements;
    /// - both are parallel-eligible (the same seed guards `find_parallel_groups`
    ///   applies: not an early-exit / channel-op / non-timer coroutine boundary
    ///   / `seq` statement, and not a syntactic console write — see
    ///   [`reorder_eligible`]); and
    /// - a legal slide exists: either `j` moves left past every intervening
    ///   statement (each independent of `j`) or `i` moves right past them
    ///   (each independent of `i`). Each pairwise adjacent swap is between
    ///   independent statements, so the whole slide preserves data + effect
    ///   ordering.
    ///
    /// Soundness scope: the slide is proven safe against data + resource-effect
    /// dependencies (the conflict graph). Observable console-output ordering
    /// is resourceless and only filtered syntactically (`has_console_output`);
    /// output emitted transitively inside a callee is not modeled — the
    /// agent's verification loop is the backstop, as for any source reorder.
    fn find_reorder_opportunities(
        &self,
        infos: &[StmtInfo],
        graph: &ConflictGraph,
        n: usize,
        groups: &[ParallelGroup],
    ) -> Vec<ReorderOpportunity> {
        // A statement is "serial" unless it sits in an emitted (multi-stmt)
        // parallel group.
        let mut grouped = vec![false; n];
        for g in groups {
            for &idx in &g.statement_indices {
                grouped[idx] = true;
            }
        }

        let mut out = Vec::new();
        for i in 0..n {
            if !reorder_eligible(&infos[i]) {
                continue;
            }
            // `j > i + 1`: adjacent independents are already the grouper's job.
            for j in (i + 2)..n {
                if !reorder_eligible(&infos[j]) {
                    continue;
                }
                // Must be independent to ever parallelize.
                if graph.conflicts(i, j) {
                    continue;
                }
                // Both already parallel → reshuffling them adds nothing.
                if grouped[i] && grouped[j] {
                    continue;
                }
                // A legal slide makes them adjacent. `j` slides left past
                // (i, j) iff each intervening stmt is independent of `j`;
                // symmetrically for `i` sliding right.
                let between = (i + 1)..j;
                let j_slides_left = between.clone().all(|k| !graph.conflicts(j, k));
                let i_slides_right = between.clone().all(|k| !graph.conflicts(i, k));
                let movable = if j_slides_left {
                    j
                } else if i_slides_right {
                    i
                } else {
                    continue;
                };
                let stationary = if movable == j { i } else { j };
                let reason = format!(
                    "statements {i} and {j} are independent but separated by \
                     {} intervening statement{}; moving statement {movable} adjacent \
                     to statement {stationary} would let them parallelize",
                    j - i - 1,
                    if j - i - 1 == 1 { "" } else { "s" },
                );
                out.push(ReorderOpportunity {
                    statement_indices: vec![i, j],
                    movable_statement: movable,
                    reason,
                });
            }
        }
        out
    }

    /// Generate a human-readable reason for why a group of statements can be parallelized.
    fn describe_group_reason(&self, infos: &[StmtInfo], indices: &[usize]) -> String {
        let all_pure = indices.iter().all(|&i| infos[i].effects.is_empty());
        if all_pure {
            return "pure computations".to_string();
        }

        // Check if they all read different resources
        let mut all_resources: Vec<&str> = Vec::new();
        let mut has_reads_only = true;
        for &i in indices {
            for eff in &infos[i].effects {
                if !matches!(eff.verb, EffectVerbKind::Reads) {
                    has_reads_only = false;
                }
                all_resources.push(&eff.resource);
            }
        }

        if has_reads_only {
            // Check if same or different resources
            let unique: HashSet<&&str> = all_resources.iter().collect();
            if unique.len() > 1 {
                return "independent reads on different resources".to_string();
            }
            return "concurrent reads on same resource".to_string();
        }

        // Check if effects are on different resources
        let unique_resources: HashSet<&str> = all_resources.iter().copied().collect();
        if unique_resources.len() == all_resources.len() && unique_resources.len() > 1 {
            return "independent effects on different resources".to_string();
        }

        "no data or effect dependencies".to_string()
    }

    // ── Variable extraction helpers ────────────────────────────

    fn collect_pattern_bindings(&self, pattern: &Pattern, defines: &mut HashSet<String>) {
        match &pattern.kind {
            PatternKind::Binding(name) => {
                defines.insert(name.clone());
            }
            PatternKind::AtBinding { name, pattern, .. } => {
                defines.insert(name.clone());
                self.collect_pattern_bindings(pattern, defines);
            }
            PatternKind::Struct { fields, .. } => {
                for f in fields {
                    if let Some(ref p) = f.pattern {
                        self.collect_pattern_bindings(p, defines);
                    } else {
                        // Shorthand field: `Foo { x }` — the field name is the binding
                        defines.insert(f.name.clone());
                    }
                }
            }
            PatternKind::TupleVariant { patterns, .. } | PatternKind::Tuple(patterns) => {
                for p in patterns {
                    self.collect_pattern_bindings(p, defines);
                }
            }
            PatternKind::Or(patterns) => {
                for p in patterns {
                    self.collect_pattern_bindings(p, defines);
                }
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                for p in prefix.iter().chain(suffix.iter()) {
                    self.collect_pattern_bindings(p, defines);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    defines.insert(name.clone());
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
        }
    }

    fn collect_assign_target_defines(&self, expr: &Expr, defines: &mut HashSet<String>) {
        match &expr.kind {
            ExprKind::Identifier(name) => {
                defines.insert(name.clone());
            }
            // The receiver of a mutating `self.method()` call, and the root of a
            // `self.field = …` / `self.field[i] = …` write, is `self` — record it
            // under the canonical name "self" (matched by `collect_expr_reads`'s
            // SelfValue arm). Without this, a `mut ref self` method call recorded
            // no write and a `self.field` assignment defined nothing, so the
            // data-dependency check missed every self-mutation (self-hosting #8).
            ExprKind::SelfValue => {
                defines.insert("self".to_string());
            }
            ExprKind::FieldAccess { object, .. } => {
                // a.field = ... defines the root variable
                self.collect_assign_target_defines(object, defines);
            }
            ExprKind::Index { object, .. } => {
                self.collect_assign_target_defines(object, defines);
            }
            ExprKind::Unary {
                op: UnaryOp::Deref,
                operand,
            } => {
                // `*place = …` / `*place += …` writes THROUGH the deref, so the
                // mutated state is rooted at the operand's root. Critically,
                // `*m.entry(k).or_insert(d) += 1` writes the MAP `m`: without
                // recording it, the auto-par dependency check saw a `for`-loop
                // histogram body as not writing the map, then parallelized the
                // loop against a later `m.keys()` read — a read-after-write race
                // on the map (B-2026-06-20-16). A `*ref += …` on a mut-ref local
                // records the binding, which is conservative (it actually writes
                // the pointee) and so always sound for the parallel-safety gate.
                self.collect_assign_target_defines(operand, defines);
            }
            ExprKind::MethodCall { object, .. } => {
                // A method-chain PLACE target — `m.entry(k).or_insert(d)`,
                // `v.get_mut(i)` — is rooted at the receiver; record it so a
                // write through the returned slot serializes against sibling
                // reads of the same container.
                self.collect_assign_target_defines(object, defines);
            }
            _ => {}
        }
    }

    fn collect_assign_target_reads(&self, expr: &Expr, reads: &mut HashSet<String>) {
        match &expr.kind {
            ExprKind::FieldAccess { object, .. } => {
                self.collect_assign_target_reads(object, reads);
            }
            ExprKind::Index { object, index } => {
                self.collect_assign_target_reads(object, reads);
                self.collect_expr_reads(index, reads);
            }
            _ => {}
        }
    }

    // ── Expression read collection ─────────────────────────────

    fn collect_expr_reads(&self, expr: &Expr, reads: &mut HashSet<String>) {
        match &expr.kind {
            ExprKind::Identifier(name) => {
                reads.insert(name.clone());
            }
            // `self` (a `ref self`/`mut ref self` receiver) reads through the
            // canonical name "self" — the same name `collect_assign_target_defines`
            // records for a `self.field = …` write and a mutating `self.method()`
            // call. Without this arm a `self.field` read recorded nothing, so a
            // statement reading `self` after a `mut ref self` method mutated it
            // showed "no data dependency" and the auto-parallelizer raced the
            // two (self-hosting #8: the lexer's `skip_whitespace()` then
            // `self.start = self.pos`).
            ExprKind::SelfValue => {
                reads.insert("self".to_string());
            }
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
                self.collect_expr_reads(left, reads);
                self.collect_expr_reads(right, reads);
            }
            ExprKind::NilCoalesce { left, right } => {
                self.collect_expr_reads(left, reads);
                self.collect_expr_reads(right, reads);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.collect_expr_reads(operand, reads);
            }
            ExprKind::Call { callee, args } => {
                self.collect_expr_reads(callee, reads);
                for arg in args {
                    self.collect_expr_reads(&arg.value, reads);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.collect_expr_reads(object, reads);
                for arg in args {
                    self.collect_expr_reads(&arg.value, reads);
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.collect_expr_reads(object, reads);
            }
            ExprKind::Index { object, index } => {
                self.collect_expr_reads(object, reads);
                self.collect_expr_reads(index, reads);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.collect_expr_reads(object, reads);
                if let Some(args) = args {
                    for arg in args {
                        self.collect_expr_reads(&arg.value, reads);
                    }
                }
            }
            ExprKind::Block(block) | ExprKind::Comptime(block)
            | ExprKind::Unsafe(block)
            | ExprKind::Try(block)
            | ExprKind::Seq(block)
            | ExprKind::Par(block) => {
                self.collect_block_reads(block, reads);
            }
            ExprKind::Lock { body, .. } => {
                self.collect_block_reads(body, reads);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.collect_expr_reads(condition, reads);
                self.collect_block_reads(then_block, reads);
                if let Some(e) = else_branch {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.collect_expr_reads(value, reads);
                self.collect_block_reads(then_block, reads);
                if let Some(e) = else_branch {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.collect_expr_reads(scrutinee, reads);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.collect_expr_reads(guard, reads);
                    }
                    self.collect_expr_reads(&arm.body, reads);
                }
            }
            ExprKind::While {
                condition, body, ..
            }
            | ExprKind::For {
                iterable: condition,
                body,
                ..
            } => {
                self.collect_expr_reads(condition, reads);
                self.collect_block_reads(body, reads);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.collect_expr_reads(value, reads);
                self.collect_block_reads(body, reads);
            }
            ExprKind::Loop { body, .. } => {
                self.collect_block_reads(body, reads);
            }
            ExprKind::LabeledBlock { body, .. } => {
                self.collect_block_reads(body, reads);
            }
            ExprKind::Closure { body, .. } => {
                self.collect_expr_reads(body, reads);
            }
            ExprKind::Return(Some(inner)) => {
                self.collect_expr_reads(inner, reads);
            }
            ExprKind::Break {
                value: Some(inner), ..
            } => {
                self.collect_expr_reads(inner, reads);
            }
            ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
                for e in exprs {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.collect_expr_reads(value, reads);
                self.collect_expr_reads(count, reads);
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.collect_expr_reads(k, reads);
                    self.collect_expr_reads(v, reads);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.collect_expr_reads(&f.value, reads);
                }
                if let Some(s) = spread {
                    self.collect_expr_reads(s, reads);
                }
            }
            ExprKind::Cast { expr: inner, .. } => {
                self.collect_expr_reads(inner, reads);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.collect_expr_reads(s, reads);
                }
                if let Some(e) = end {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::Path { segments, .. } => {
                // A path like Mod::val — the first segment could be a variable
                if let Some(first) = segments.first() {
                    reads.insert(first.clone());
                }
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.collect_expr_reads(&b.value, reads);
                }
                self.collect_block_reads(body, reads);
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let ParsedInterpolationPart::Expr(inner, _) = part {
                        self.collect_expr_reads(inner, reads);
                    }
                }
            }
            // Leaf expressions that don't read variables
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            // NOTE: `ExprKind::SelfValue` is handled explicitly above (records
            // the read of "self") — it is intentionally NOT in this no-op leaf
            // group (self-hosting #8).
            | ExprKind::SelfType
            | ExprKind::Continue { .. }
            | ExprKind::Return(None)
            | ExprKind::Break { value: None, .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
        }
    }

    /// Walk an expression's nested blocks and record any outer-scope
    /// names written via `Assign` / `CompoundAssign` into `writes`.
    /// Critical for the auto-parallelizer's data-dependency reasoning:
    /// a `for v in coll { if v > m { m = v; } }` expression-statement
    /// must record `m` as a write so subsequent stmts that read `m`
    /// serialize against it. Local variables shadowed inside nested
    /// blocks (introduced by `let`) are intentionally still recorded
    /// here — the conflict check at the call site uses
    /// `Set::intersect` over a flat name set, so non-disjoint local
    /// shadowing of the same name produces an over-serialization that
    /// is correct (and conservative) rather than incorrect.
    fn collect_expr_inner_writes(&self, expr: &Expr, writes: &mut HashSet<String>) {
        match &expr.kind {
            ExprKind::Block(block) | ExprKind::Seq(block) => {
                self.collect_block_inner_writes(block, writes);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.collect_expr_inner_writes(condition, writes);
                self.collect_block_inner_writes(then_block, writes);
                if let Some(e) = else_branch {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.collect_expr_inner_writes(value, writes);
                self.collect_block_inner_writes(then_block, writes);
                if let Some(e) = else_branch {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                // B-2026-07-12-5: the SCRUTINEE (and arm guards) can mutate —
                // `match b.take() { .. }` where `take(mut ref self)` pops the
                // receiver. Without walking them the auto-par data-dependency
                // check missed the receiver write, so it raced the statements.
                self.collect_expr_inner_writes(scrutinee, writes);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.collect_expr_inner_writes(g, writes);
                    }
                    self.collect_expr_inner_writes(&arm.body, writes);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.collect_expr_inner_writes(condition, writes);
                self.collect_block_inner_writes(body, writes);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.collect_expr_inner_writes(value, writes);
                self.collect_block_inner_writes(body, writes);
            }
            ExprKind::Loop { body, .. } => self.collect_block_inner_writes(body, writes),
            ExprKind::For { body, .. } => self.collect_block_inner_writes(body, writes),
            ExprKind::Unsafe(block) | ExprKind::Par(block) => {
                self.collect_block_inner_writes(block, writes);
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                // A method whose declared/inferred effects include any
                // non-pure verb (`Writes`, `Allocates`, `Sends`, `Receives`,
                // `Panics`) is treated as mutating its receiver — record the
                // receiver's root identifier as a write so the
                // data-dependency check serializes it against sibling
                // reads of the same name. Without this, two `a.push(...)`
                // / `a.push(...)` calls are seen as read-only on `a` and
                // the auto-parallelizer races them on shared Vec state.
                //
                // A `mut ref self` method ALSO mutates its receiver — through
                // the borrow — even when it carries no resource-effect verb.
                // A parser cursor advance (`self.pos = self.pos + 1` inside
                // `self.parse_block()`) writes a plain scalar field, which is
                // ownership-level mutation with NO `writes(Resource)` effect,
                // so the effect heuristic above misses it. Without the
                // receiver-mode check, three sequential cursor-advancing calls
                // (`self.parse_expr_bp(0)`, `self.parse_block()`,
                // `self.parse_else()` in `parse_if`) recorded no write on
                // `self`, so the data-dependency check saw them as independent
                // and the auto-parallelizer raced them through `karac_par_run`
                // — corrupting the shared parser state (B-2026-07-09-12: the
                // self-hosted parser SEGV'd on every `if`/`loop`/`for`/`while`).
                if self.method_effects_imply_receiver_mutation(method)
                    || self.method_receiver_is_mut_ref(method)
                {
                    self.collect_assign_target_defines(object, writes);
                }
                self.collect_expr_inner_writes(object, writes);
                for arg in args {
                    self.collect_expr_inner_writes(&arg.value, writes);
                }
            }
            ExprKind::Call { callee, args } => {
                // A free-function call mutates caller-visible state through
                // `mut ref T` / `mut Slice[T]` parameters — record each
                // mutably-passed argument's root identifier as a write so
                // subsequent statements that read it serialize against the
                // call. Without this arm, `add_one(mut out); println(out.len())`
                // in `main` records no write on `out`, the dependency check
                // sees two reads, and the auto-parallelizer races the two
                // statements via `karac_par_run` — with `out` captured into
                // the par env BY VALUE, so the callee's header writeback
                // (len/cap/data after a push-grow) lands in the env copy and
                // the caller observes a stale empty Vec (kata 22, 2026-06-06).
                //
                // Two detection paths, OR'd:
                //   - the call-site `mut` marker (`f(mut x)` — required for
                //     fresh owned bindings per design.md Feature 4 Part 1½);
                //   - the callee's declared param mode when its body is in
                //     this program (`function_bodies`) — covers the unmarked
                //     mut-ref forwarding form (`x` already `mut ref` in
                //     scope) and any future marker-elision sites.
                let callee_params = self
                    .extract_callee_name(callee)
                    .and_then(|n| self.function_bodies.get(&n))
                    .map(|f| f.params.as_slice());
                for (i, arg) in args.iter().enumerate() {
                    let param_is_mut_ref =
                        callee_params.and_then(|ps| ps.get(i)).is_some_and(|p| {
                            matches!(p.ty.kind, TypeKind::MutRef(_) | TypeKind::MutSlice(_))
                        });
                    if arg.mut_marker || param_is_mut_ref {
                        self.collect_assign_target_defines(&arg.value, writes);
                    }
                    self.collect_expr_inner_writes(&arg.value, writes);
                }
            }
            // B-2026-07-12-5: a mutating method call (`b.push(x)`, `b.take()`)
            // can hide in ANY value-expression position, not just a bare
            // statement or a block. The auto-parallelizer's data-dependency
            // check reached the `MethodCall` / `Call` arms above only through
            // the block/branch arms, so a mutation nested in an f-string
            // interpolation, a `Some(..)` / tuple / index / binary / … missed
            // the receiver write and the statements were classed independent
            // and RACED — with the receiver captured BY VALUE into the par env,
            // so the mutation landed in a discarded copy (silent wrong answer:
            // `println(f"{b.push_len(x)}")` in a loop; `match b.take()`;
            // `Some(b.pop())`). Recurse into every sub-expression so those arms
            // see the write. Safe: a write is recorded ONLY for a genuinely
            // mutating call (the receiver-mode / mut-arg gates), so this only
            // ever ADDS serialization — it can never introduce a race.
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e, _) = part {
                        self.collect_expr_inner_writes(e, writes);
                    }
                }
            }
            ExprKind::Binary { left, right, .. }
            | ExprKind::NilCoalesce { left, right }
            | ExprKind::Pipe { left, right } => {
                self.collect_expr_inner_writes(left, writes);
                self.collect_expr_inner_writes(right, writes);
            }
            ExprKind::Unary { operand, .. } => self.collect_expr_inner_writes(operand, writes),
            ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => {
                self.collect_expr_inner_writes(inner, writes);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.collect_expr_inner_writes(object, writes);
            }
            ExprKind::Index { object, index } => {
                self.collect_expr_inner_writes(object, writes);
                self.collect_expr_inner_writes(index, writes);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.collect_expr_inner_writes(object, writes);
                if let Some(args) = args {
                    for a in args {
                        self.collect_expr_inner_writes(&a.value, writes);
                    }
                }
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.collect_expr_inner_writes(s, writes);
                }
                if let Some(e) = end {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::Tuple(items)
            | ExprKind::ArrayLiteral(items)
            | ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.collect_expr_inner_writes(value, writes);
                self.collect_expr_inner_writes(count, writes);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.collect_expr_inner_writes(k, writes);
                    self.collect_expr_inner_writes(v, writes);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.collect_expr_inner_writes(&f.value, writes);
                }
                if let Some(s) = spread {
                    self.collect_expr_inner_writes(s, writes);
                }
            }
            ExprKind::Return(Some(e)) => self.collect_expr_inner_writes(e, writes),
            ExprKind::Break { value: Some(e), .. } => self.collect_expr_inner_writes(e, writes),
            ExprKind::Try(block) | ExprKind::Comptime(block) => {
                self.collect_block_inner_writes(block, writes);
            }
            ExprKind::LabeledBlock { body, .. } => {
                self.collect_block_inner_writes(body, writes);
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.collect_expr_inner_writes(mutex, writes);
                self.collect_block_inner_writes(body, writes);
            }
            _ => {}
        }
    }

    /// Returns `true` if any callee key matching `<Type>.<method>` (or the
    /// bare `<method>`) carries an effect verb that implies mutation of
    /// the receiver state. Conservative: any non-pure verb counts, since
    /// the auto-parallelizer's job is to be sound, not maximally
    /// permissive. Lookup mirrors `collect_expr_effects`'s MethodCall arm.
    fn method_effects_imply_receiver_mutation(&self, method: &str) -> bool {
        let suffix = format!(".{}", method);
        for (key, set) in &self.effects.inferred_effects {
            if (key == method || key.ends_with(&suffix)) && effect_set_has_nonpure_verb(set) {
                return true;
            }
        }
        for (key, decl) in &self.effects.declared_effects {
            if key != method && !key.ends_with(&suffix) {
                continue;
            }
            match decl {
                DeclaredEffects::Explicit(set) | DeclaredEffects::PolymorphicWithFixed(set) => {
                    if effect_set_has_nonpure_verb(set) {
                        return true;
                    }
                }
                // Unknown effects → assume mutating.
                DeclaredEffects::Polymorphic => return true,
                DeclaredEffects::None => {}
            }
        }
        false
    }

    /// Returns `true` if any method named `method` (matched as `<Type>.<method>`)
    /// declares a `mut ref self` receiver. Such a method CAN mutate the receiver
    /// through the borrow independent of any resource-effect verb, so a call to
    /// it must be treated as writing its receiver for the auto-parallelizer's
    /// data-dependency gate. Conservative: matches on the method name across all
    /// types (like `method_effects_imply_receiver_mutation`), which can only
    /// over-serialize, never under-serialize — the sound direction for auto-par.
    /// This is the receiver-mode counterpart to the effect-verb heuristic and
    /// catches plain-field mutation (a parser cursor advance) that carries no
    /// `writes(Resource)` effect (B-2026-07-09-12).
    fn method_receiver_is_mut_ref(&self, method: &str) -> bool {
        let suffix = format!(".{}", method);
        self.method_bodies.iter().any(|(key, f)| {
            (key == method || key.ends_with(&suffix))
                && matches!(f.self_param, Some(SelfParam::MutRef))
        })
    }

    /// Walk a block's statements and record any outer-scope names
    /// written via `Assign` / `CompoundAssign` (plus inner writes of
    /// nested expressions). Companion to `collect_expr_inner_writes`.
    fn collect_block_inner_writes(&self, block: &Block, writes: &mut HashSet<String>) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
                    self.collect_assign_target_defines(target, writes);
                }
                StmtKind::Expr(e) => self.collect_expr_inner_writes(e, writes),
                StmtKind::Let { value, .. } => self.collect_expr_inner_writes(value, writes),
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    self.collect_expr_inner_writes(value, writes);
                    self.collect_block_inner_writes(else_block, writes);
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.collect_block_inner_writes(body, writes);
                }
                _ => {}
            }
        }
        if let Some(e) = &block.final_expr {
            self.collect_expr_inner_writes(e, writes);
        }
    }

    fn collect_block_reads(&self, block: &Block, reads: &mut HashSet<String>) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::MultiAssign { .. } => unreachable!(
                    "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
                ),
                StmtKind::Let { value, .. } => self.collect_expr_reads(value, reads),
                StmtKind::LetUninit { .. } => {}
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    self.collect_expr_reads(value, reads);
                    self.collect_block_reads(else_block, reads);
                }
                StmtKind::Assign { target, value } => {
                    self.collect_expr_reads(target, reads);
                    self.collect_expr_reads(value, reads);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.collect_expr_reads(target, reads);
                    self.collect_expr_reads(value, reads);
                }
                StmtKind::Expr(e) => self.collect_expr_reads(e, reads),
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.collect_block_reads(body, reads);
                }
            }
        }
        if let Some(e) = &block.final_expr {
            self.collect_expr_reads(e, reads);
        }
    }

    // ── Effect collection from expressions ─────────────────────

    fn collect_expr_effects(&self, expr: &Expr, info: &mut StmtInfo) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                // Look up callee effects
                if let Some(name) = self.extract_callee_name(callee) {
                    info.called_fn_names.insert(name.clone());
                    let from = info.effects.len();
                    self.add_function_effects(&name, info);
                    // Slice 3: substitute the callee's parameterized-resource
                    // keys (`writes(Db[id])`) with these arguments, for the
                    // effects just added.
                    self.apply_parameterized_keys(&name, args, from, info);
                }
                self.collect_expr_effects(callee, info);
                for arg in args {
                    self.collect_expr_effects(&arg.value, info);
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                info.called_fn_names.insert(method.clone());
                // Walk every effect key ending in `.<method>`. Builtin methods
                // (`Vec.push`, `Map.insert`, ...) live only in
                // `effects.inferred_effects`; user-defined impl methods live
                // in both `method_bodies` and `effects.inferred_effects`, so
                // iterating the latter covers both. Matches the renderer in
                // `concurrency_report::render_stmt_effects`.
                let from = info.effects.len();
                let suffix = format!(".{}", method);
                for key in self.effects.inferred_effects.keys() {
                    if key.ends_with(&suffix) {
                        self.add_function_effects(key, info);
                    }
                }
                for key in self.effects.declared_effects.keys() {
                    if key.ends_with(&suffix) {
                        self.add_function_effects(key, info);
                    }
                }
                // Also try bare method name (matches free-function shape).
                self.add_function_effects(method, info);
                // Slice 3: parameterized-resource keys for a method call. Resolve
                // the EXACT receiver-type method via `method_callee_types` (keyed
                // by the method-call span, which equals the receiver span) so the
                // callee's declared `Db[id]` param substitutes with THESE args
                // (method params exclude the receiver, so arg positions align).
                if let Some(types) = self.types {
                    if let Some(mkey) = types
                        .method_callee_types
                        .get(&SpanKey::from_span(&expr.span))
                    {
                        self.apply_parameterized_keys(mkey, args, from, info);
                    }
                }
                self.collect_expr_effects(object, info);
                for arg in args {
                    self.collect_expr_effects(&arg.value, info);
                }
            }
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
                self.collect_expr_effects(left, info);
                self.collect_expr_effects(right, info);
            }
            ExprKind::NilCoalesce { left, right } => {
                self.collect_expr_effects(left, info);
                self.collect_expr_effects(right, info);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.collect_expr_effects(operand, info);
            }
            ExprKind::Block(block)
            | ExprKind::Comptime(block)
            | ExprKind::Unsafe(block)
            | ExprKind::Try(block)
            | ExprKind::Seq(block)
            | ExprKind::Par(block) => {
                self.collect_block_effects(block, info);
            }
            ExprKind::Lock { body, .. } => {
                self.collect_block_effects(body, info);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.collect_expr_effects(condition, info);
                self.collect_block_effects(then_block, info);
                if let Some(e) = else_branch {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.collect_expr_effects(value, info);
                self.collect_block_effects(then_block, info);
                if let Some(e) = else_branch {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.collect_expr_effects(scrutinee, info);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.collect_expr_effects(guard, info);
                    }
                    self.collect_expr_effects(&arm.body, info);
                }
            }
            ExprKind::While {
                condition, body, ..
            }
            | ExprKind::For {
                iterable: condition,
                body,
                ..
            } => {
                self.collect_expr_effects(condition, info);
                self.collect_block_effects(body, info);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.collect_expr_effects(value, info);
                self.collect_block_effects(body, info);
            }
            ExprKind::Loop { body, .. } => {
                self.collect_block_effects(body, info);
            }
            ExprKind::LabeledBlock { body, .. } => {
                self.collect_block_effects(body, info);
            }
            ExprKind::Closure { body, .. } => {
                self.collect_expr_effects(body, info);
            }
            ExprKind::Return(Some(inner)) => {
                self.collect_expr_effects(inner, info);
            }
            ExprKind::Break {
                value: Some(inner), ..
            } => {
                self.collect_expr_effects(inner, info);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.collect_expr_effects(object, info);
            }
            ExprKind::Index { object, index } => {
                self.collect_expr_effects(object, info);
                self.collect_expr_effects(index, info);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.collect_expr_effects(object, info);
                if let Some(args) = args {
                    for arg in args {
                        self.collect_expr_effects(&arg.value, info);
                    }
                }
            }
            ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
                for e in exprs {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.collect_expr_effects(value, info);
                self.collect_expr_effects(count, info);
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.collect_expr_effects(k, info);
                    self.collect_expr_effects(v, info);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.collect_expr_effects(&f.value, info);
                }
                if let Some(s) = spread {
                    self.collect_expr_effects(s, info);
                }
            }
            ExprKind::Cast { expr: inner, .. } => {
                self.collect_expr_effects(inner, info);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.collect_expr_effects(s, info);
                }
                if let Some(e) = end {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.collect_expr_effects(&b.value, info);
                }
                self.collect_block_effects(body, info);
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let ParsedInterpolationPart::Expr(inner, _) = part {
                        self.collect_expr_effects(inner, info);
                    }
                }
            }
            // Leaf expressions — no effects
            ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Continue { .. }
            | ExprKind::Return(None)
            | ExprKind::Break { value: None, .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
        }
    }

    fn collect_block_effects(&self, block: &Block, info: &mut StmtInfo) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::MultiAssign { .. } => unreachable!(
                    "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
                ),
                StmtKind::Let { value, .. } => self.collect_expr_effects(value, info),
                StmtKind::LetUninit { .. } => {}
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    self.collect_expr_effects(value, info);
                    self.collect_block_effects(else_block, info);
                }
                StmtKind::Assign { target, value } => {
                    self.collect_expr_effects(target, info);
                    self.collect_expr_effects(value, info);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.collect_expr_effects(target, info);
                    self.collect_expr_effects(value, info);
                }
                StmtKind::Expr(e) => self.collect_expr_effects(e, info),
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.collect_block_effects(body, info);
                }
            }
        }
        if let Some(e) = &block.final_expr {
            self.collect_expr_effects(e, info);
        }
    }

    /// Look up a function's inferred effects and add them to the effect list.
    /// Also sets `info.calls_polymorphic` if the callee's declared effects
    /// include `with _` — in which case the inferred set alone doesn't describe
    /// what the callee may actually do at runtime.
    fn add_function_effects(&self, name: &str, info: &mut StmtInfo) {
        if let Some(effect_set) = self.effects.inferred_effects.get(name) {
            for te in &effect_set.effects {
                info.effects.push(StmtEffect {
                    verb: te.effect.verb.clone(),
                    resource: te.effect.resource.clone(),
                    source_callee: Some(name.to_string()),
                    key: None,
                });
            }
        }
        if matches!(
            self.effects.declared_effects.get(name),
            Some(DeclaredEffects::Polymorphic | DeclaredEffects::PolymorphicWithFixed(_))
        ) {
            info.calls_polymorphic = true;
        }
    }

    /// A2b-2 Phase 2 Slice 3: fill in `StmtEffect::key` for the effects a call
    /// contributed (the tail of `info.effects` starting at `from`), from the
    /// callee's DECLARED parameterized resources (`with writes(Db[id])`)
    /// substituted with the actual arguments. `callee` names the resolved
    /// function/method (`fn` name or `Type.method`); `args` are the call args.
    /// Only compile-time-literal partition keys are recorded (a variable arg
    /// stays `None` = unproven = conservatively conflicting). Additive: a
    /// callee with no `[param]` resource leaves every key `None`.
    fn apply_parameterized_keys(
        &self,
        callee: &str,
        args: &[CallArg],
        from: usize,
        info: &mut StmtInfo,
    ) {
        let Some(func) = self
            .function_bodies
            .get(callee)
            .or_else(|| self.method_bodies.get(callee))
        else {
            return;
        };
        let Some(list) = &func.effects else {
            return;
        };
        for item in &list.items {
            let EffectItem::Verb(ev) = item else {
                continue;
            };
            for res in &ev.resources {
                let Some(param) = &res.param else {
                    continue;
                };
                let Some(key) = resolve_param_key(param, &func.params, args) else {
                    continue;
                };
                let res_name = res.path.join(".");
                for e in info.effects[from..].iter_mut() {
                    if e.verb == ev.kind && e.resource == res_name {
                        e.key = Some(key.clone());
                    }
                }
            }
        }
    }

    /// Extract a callee name from a call expression.
    fn extract_callee_name(&self, callee: &Expr) -> Option<String> {
        match &callee.kind {
            ExprKind::Identifier(name) => Some(name.clone()),
            ExprKind::Path { segments, .. } => {
                if segments.len() == 2 {
                    Some(format!("{}.{}", segments[0], segments[1]))
                } else {
                    segments.last().cloned()
                }
            }
            _ => None,
        }
    }
}

// ── Reduction recognition helpers ──────────────────────────────

/// Pull the name out of a bare-identifier expression. Used by the
/// reduction recognizer to reject any assignment whose target is a
/// field access, index, or compound shape — those aren't a single
/// scalar accumulator and the fan-out / combine lowering doesn't cover
/// them at v1.
fn identifier_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        _ => None,
    }
}

/// True if `expr` is an integer literal — used to recognize the loop-
/// counter shape `i += 1` / `i = i + 1` and exclude it from the
/// reduction accumulator count. Floats are intentionally rejected here:
/// a float loop counter is unusual and the loop-counter excuse only
/// applies to integer steps anyway.
fn is_int_literal(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Integer(_, _))
}

/// True if `value` has shape `acc + int_literal` for the named `acc` —
/// the loop-counter step pattern in an explicit `while` loop. Folded
/// alongside reduction-shape writes so kata-7-style benches (`while k <
/// K { sum = sum + ...; k = k + 1; }`) classify cleanly without
/// forcing the loop counter through the reduction allow-list.
///
/// Accepts both the pre-lowered `Binary` shape and the lowered
/// `Call(Path([type, "add"]), [a, b])` shape (`src/lowering.rs`
/// rewrites every primitive binop into a method-call dispatch before
/// the CLI runs concurrencycheck — without the second arm, the
/// recognizer fires only for the test pipeline that skips lowering).
fn induction_step_via_assign(value: &Expr, acc_name: &str) -> bool {
    match &value.kind {
        ExprKind::Binary {
            op: BinOp::Add,
            left,
            right,
        } => is_acc_plus_int_literal(&left.kind, &right.kind, acc_name),
        ExprKind::Call { callee, args } => {
            match_lowered_op_call(callee, args, "add").is_some_and(|(left, right)| {
                is_acc_plus_int_literal(&left.kind, &right.kind, acc_name)
            })
        }
        _ => false,
    }
}

fn is_acc_plus_int_literal(left: &ExprKind, right: &ExprKind, acc_name: &str) -> bool {
    match (left, right) {
        (ExprKind::Identifier(n), ExprKind::Integer(_, _))
        | (ExprKind::Integer(_, _), ExprKind::Identifier(n)) => n == acc_name,
        _ => false,
    }
}

/// True if `value` has shape `acc <op> expr` or `expr <op> acc` for
/// the named `acc` and `op` in the reduction allow-list — the right-
/// hand side of `acc = acc <op> expr`. Returns the op kind on match.
/// Commutativity is exploited at recognition: an allow-list op `+/*/|/&/^`
/// is commutative, so the analyzer accepts `acc op expr` and `expr op
/// acc` symmetrically. The right-hand `expr` is unconstrained — any
/// shape that produces a value combinable with `acc` is fine; the
/// codegen slice will type-gate.
///
/// Like `induction_step_via_assign`, this checks both the pre-lowered
/// `Binary` and the lowered `Call(Path([type, op_method]), [a, b])`
/// shapes — see that function's doc comment for context.
fn reduction_binary_shape(value: &Expr, acc_name: &str) -> Option<ReductionOp> {
    match &value.kind {
        ExprKind::Binary { op, left, right } => {
            let red_op = ReductionOp::from_bin_op(op)?;
            // Direct shape: `acc <op> expr` or `expr <op> acc`.
            if acc_matches_either(&left.kind, &right.kind, acc_name) {
                return Some(red_op);
            }
            // Nested chain: `acc + a + b` parses left-associatively as
            // `Binary(+, Binary(+, acc, a), b)` — the direct match
            // above sees neither operand as the acc identifier. By
            // commutativity of the allow-list ops, any chain of the
            // same op containing the accumulator exactly once is a
            // valid reduction step: reorder to `acc + (others-combined)`
            // and the recognized reduction shape falls out. Count acc
            // occurrences across the same-op chain; recognize iff it
            // appears exactly once.
            if count_acc_in_chain(value, op, op_method_for_bin_op(op), acc_name) == 1 {
                return Some(red_op);
            }
            None
        }
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 || args.len() != 2 {
                return None;
            }
            let red_op = match segments[1].as_str() {
                "add" => ReductionOp::Add,
                "mul" => ReductionOp::Mul,
                "bitor" => ReductionOp::BitOr,
                "bitand" => ReductionOp::BitAnd,
                "bitxor" => ReductionOp::BitXor,
                "min" => ReductionOp::Min,
                "max" => ReductionOp::Max,
                _ => return None,
            };
            // Direct shape: `T.op(acc, expr)` / `T.op(expr, acc)`.
            if acc_matches_either(&args[0].value.kind, &args[1].value.kind, acc_name) {
                return Some(red_op);
            }
            // Post-lowering chain — mirror of the Binary branch above
            // but for the `Call(Path([T, op_method]), [a, b])` shape the
            // lowering pass emits. Use the bin-op corresponding to
            // segments[1] so the chain walker recognizes both pre- and
            // post-lowering nodes uniformly.
            let chain_op = bin_op_for_op_method(segments[1].as_str())?;
            if count_acc_in_chain(value, &chain_op, segments[1].as_str(), acc_name) == 1 {
                return Some(red_op);
            }
            None
        }
        _ => None,
    }
}

/// Count occurrences of `acc_name` (as a leaf `Identifier`) in a chain
/// of nested expressions where each level is either a `Binary(op, ...)`
/// matching `target_op` or a `Call(Path([_, target_method]), [...])`
/// matching `target_method`. Recursion stops at any expression that's
/// not a same-op chain node (those count as leaves and contribute 1
/// iff they're the acc identifier, else 0).
///
/// Used by `reduction_binary_shape` to recognize commutative-reduction
/// chains like `acc + a + b` (parses as `Binary(+, Binary(+, acc, a),
/// b)`) — any chain of the same allow-list op containing acc exactly
/// once is a valid reduction step under commutativity, since the chain
/// can be reordered to `acc + (others-combined)`.
fn count_acc_in_chain(
    expr: &Expr,
    target_op: &BinOp,
    target_method: &str,
    acc_name: &str,
) -> usize {
    match &expr.kind {
        ExprKind::Binary { op, left, right } if op == target_op => {
            count_acc_in_chain(left, target_op, target_method, acc_name)
                + count_acc_in_chain(right, target_op, target_method, acc_name)
        }
        ExprKind::Call { callee, args } if args.len() == 2 => {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                if segments.len() == 2 && segments[1] == target_method {
                    return count_acc_in_chain(&args[0].value, target_op, target_method, acc_name)
                        + count_acc_in_chain(&args[1].value, target_op, target_method, acc_name);
                }
            }
            0
        }
        ExprKind::Identifier(n) if n == acc_name => 1,
        _ => 0,
    }
}

/// Map a `BinOp` to its lowered op-method name (`Add` → `"add"`, etc.).
/// Mirror of `ReductionOp::from_bin_op`'s op-method conventions; used
/// by the chain walker so it can match both pre-lowering Binary and
/// post-lowering Call nodes uniformly under the same chain.
fn op_method_for_bin_op(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Mul => "mul",
        BinOp::BitOr => "bitor",
        BinOp::BitAnd => "bitand",
        BinOp::BitXor => "bitxor",
        // Min/Max have no BinOp glyph — never recognized as chain
        // members through the Binary path. Falls back to a name that
        // won't match any Call segment.
        _ => "",
    }
}

fn bin_op_for_op_method(method: &str) -> Option<BinOp> {
    match method {
        "add" => Some(BinOp::Add),
        "mul" => Some(BinOp::Mul),
        "bitor" => Some(BinOp::BitOr),
        "bitand" => Some(BinOp::BitAnd),
        "bitxor" => Some(BinOp::BitXor),
        // Min/Max are call-form only, no BinOp counterpart — the
        // chain walker still works through `target_method` matching
        // even though the BinOp side never fires.
        _ => None,
    }
}

fn acc_matches_either(left: &ExprKind, right: &ExprKind, acc_name: &str) -> bool {
    matches!(left, ExprKind::Identifier(n) if n == acc_name)
        || matches!(right, ExprKind::Identifier(n) if n == acc_name)
}

/// Recognize a conditional-assign Min/Max reduction:
/// `if x < acc { acc = x; }` → Min, `if x > acc { acc = x; }` → Max
/// (with symmetric `acc > x` → Min and `acc < x` → Max accepted too).
///
/// Returns `Some((acc_name, op))` when the if-stmt shapes a Min/Max
/// reduction step against a single accumulator. The recognizer is
/// conservative — extends to richer assignment-RHS expressions in a
/// follow-up if a workload surfaces the shape:
/// - else-less if only (no `else` / `else-if` arms),
/// - body is exactly one statement, an `Assign` to an identifier target,
/// - assignment value is a single identifier (matches the kata-153
///   `let x = ...; if x < m { m = x; }` desugar pattern; richer RHS
///   like `if a[i] < m { m = a[i]; }` is not supported at v1),
/// - condition is `Binary(Lt | Gt)` (or the lowered `Call(Path([T, "lt"|"gt"]), [a, b])`)
///   with both operands as identifiers, one matching the assignment
///   target and the other matching the assignment value.
fn conditional_minmax_shape(expr: &Expr) -> Option<(String, ReductionOp)> {
    let ExprKind::If {
        condition,
        then_block,
        else_branch,
    } = &expr.kind
    else {
        return None;
    };
    if else_branch.is_some() {
        return None;
    }
    if then_block.stmts.len() != 1 || then_block.final_expr.is_some() {
        return None;
    }
    let StmtKind::Assign { target, value } = &then_block.stmts[0].kind else {
        return None;
    };
    let acc_name = identifier_name(target)?;
    let ExprKind::Identifier(value_name) = &value.kind else {
        return None;
    };
    let (cmp_op, left, right) = match &condition.kind {
        ExprKind::Binary { op, left, right } => (op.clone(), left.as_ref(), right.as_ref()),
        ExprKind::Call { callee, args } if args.len() == 2 => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 {
                return None;
            }
            let op = match segments[1].as_str() {
                "lt" => BinOp::Lt,
                "gt" => BinOp::Gt,
                _ => return None,
            };
            (op, &args[0].value, &args[1].value)
        }
        _ => return None,
    };
    let ExprKind::Identifier(l_name) = &left.kind else {
        return None;
    };
    let ExprKind::Identifier(r_name) = &right.kind else {
        return None;
    };
    // `value < acc` → Min (new value is smaller, picked into acc).
    // `acc > value` → Min (commutative re-arrangement).
    // `value > acc` → Max, `acc < value` → Max (mirror).
    let red_op = match cmp_op {
        BinOp::Lt => {
            if l_name == value_name && r_name == &acc_name {
                ReductionOp::Min
            } else if l_name == &acc_name && r_name == value_name {
                ReductionOp::Max
            } else {
                return None;
            }
        }
        BinOp::Gt => {
            if l_name == value_name && r_name == &acc_name {
                ReductionOp::Max
            } else if l_name == &acc_name && r_name == value_name {
                ReductionOp::Min
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some((acc_name, red_op))
}

/// Match `Call(Path([type, method_name]), [a, b])` and return the two
/// arg expressions. Used by both `reduction_binary_shape` and
/// `induction_step_via_assign` to peek at the operand positions of a
/// post-lowering primitive op call.
fn match_lowered_op_call<'a>(
    callee: &Expr,
    args: &'a [crate::ast::CallArg],
    method_name: &str,
) -> Option<(&'a Expr, &'a Expr)> {
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return None;
    };
    if segments.len() != 2 || segments[1] != method_name || args.len() != 2 {
        return None;
    }
    Some((&args[0].value, &args[1].value))
}

/// Classify a single statement as a recognized accumulator update for a
/// reduction step: `acc = acc <op> EXPR` (Assign), `acc OP= EXPR`
/// (CompoundAssign), or chain shapes accepted by `reduction_binary_shape`.
/// Returns `Some((acc_name, op))` on a match; `None` otherwise.
///
/// Shared by both arms of `conditional_acc_update_shape` so the 2-arm
/// case can re-classify the else-arm with the same rules. The
/// unconditional `acc += const_lit` induction-step shape is
/// special-cased upstream in `classify_loop_body`'s CompoundAssign arm
/// (treated as the loop counter); under a conditional wrap the same
/// syntactic shape means "count of truthy iterations" and is a
/// legitimate reduction, so we do not bail here.
fn single_stmt_as_acc_update(stmt: &Stmt) -> Option<(String, ReductionOp)> {
    match &stmt.kind {
        StmtKind::Assign { target, value } => {
            let name = identifier_name(target)?;
            let op = reduction_binary_shape(value, &name)?;
            Some((name, op))
        }
        StmtKind::CompoundAssign { target, op, .. } => {
            let name = identifier_name(target)?;
            ReductionOp::from_compound_op(op).map(|red_op| (name, red_op))
        }
        _ => None,
    }
}

/// Same as [`single_stmt_as_acc_update`] but wrapping a `Block` that
/// must contain exactly one statement and no trailing expression.
fn single_stmt_block_as_acc_update(block: &Block) -> Option<(String, ReductionOp)> {
    if block.stmts.len() != 1 || block.final_expr.is_some() {
        return None;
    }
    single_stmt_as_acc_update(&block.stmts[0])
}

/// Recognize the collect-step shape: `acc.push(EXPR)` where `acc` is a
/// bare identifier (no field / index / chain receivers). Returns
/// `Some(acc_name)` on a match; `None` otherwise.
///
/// Generic-arg lists on `push` (`acc.push[T](x)`) are accepted only with
/// no args — the `push` method has no useful generic args today; the
/// matcher is shape-only and doesn't validate `acc`'s type. The codegen
/// layer (Phase 3) is responsible for confirming `acc: Vec[T]` /
/// `String` / similar; non-matching types fall through to sequential
/// code as a natural consequence of the codegen-side type check.
///
/// The single-arg requirement is the canonical `Vec::push(x)` shape; if
/// future workloads need `push_many(values)` or other multi-arg
/// collectors, the matcher can be extended.
pub(crate) fn collect_push_shape(expr: &Expr) -> Option<String> {
    let ExprKind::MethodCall {
        object,
        method,
        args,
        ..
    } = &expr.kind
    else {
        return None;
    };
    if method != "push" || args.len() != 1 {
        return None;
    }
    let ExprKind::Identifier(name) = &object.kind else {
        return None;
    };
    Some(name.clone())
}
