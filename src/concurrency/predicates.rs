//! Free-function AST predicates: constant-init, early-exit, user-defer,
//! channel-op, and console-output walks, plus `reorder_eligible`.
//!
//! Extracted verbatim from `concurrency.rs` (structural-debt extraction,
//! 2026-08-16); helpers are `pub(super)` — this is a private submodule of
//! the concurrency pass, not API.

use super::*;

/// True when an `EffectSet` contains any verb that implies side effects
/// beyond a pure read — used by `method_effects_imply_receiver_mutation`
/// to decide whether a method call should mark its receiver as written
/// for data-dependency reasoning.
pub(super) fn effect_set_has_nonpure_verb(set: &EffectSet) -> bool {
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
pub(super) fn stmt_is_constant_init(stmt: &Stmt) -> bool {
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
pub(super) fn expr_is_constant_init(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::ByteStringLit(_)
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

/// `true` iff this statement contains a jump that escapes a directly-nested
/// expression's control flow — i.e., that would, at codegen time, emit a
/// `ret X` (or branch to a loop's exit edge) bypassing the statement's
/// "fall through" exit. `loop_jumps` selects which jumps count; see
/// [`block_exits`].
///
/// This matters because a par branch is lowered to a standalone `void` LLVM
/// function, and an embedded `return X` from the original body would produce
/// `ret <T> X` inside the void branch and fail LLVM module verification.
fn stmt_exits(stmt: &Stmt, loop_jumps: bool) -> bool {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. }
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. }
        | StmtKind::Expr(value) => expr_exits(value, loop_jumps),
        StmtKind::LetElse {
            value, else_block, ..
        } => expr_exits(value, loop_jumps) || block_exits(else_block, loop_jumps),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => block_exits(body, loop_jumps),
        StmtKind::LetUninit { .. } => false,
    }
}

/// True when `block` can transfer control out of itself. `loop_jumps` selects
/// which jumps count: `true` includes `break` / `continue` (what
/// `find_parallel_groups` needs, since a par group is a block boundary),
/// `false` counts only jumps that leave the enclosing FUNCTION — `return` and
/// `?`. See [`stmt_has_early_exit`] and [`block_escapes_enclosing_fn`].
fn block_exits(block: &Block, loop_jumps: bool) -> bool {
    block.stmts.iter().any(|s| stmt_exits(s, loop_jumps))
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_exits(e, loop_jumps))
}

fn expr_exits(expr: &Expr, loop_jumps: bool) -> bool {
    match &expr.kind {
        ExprKind::Return(_) => true,
        ExprKind::Break { .. } => loop_jumps,
        ExprKind::Continue { .. } => loop_jumps,
        // B-2026-08-26-27 — `?` IS AN EARLY EXIT, and omitting it here is why
        // two separate rows had to be filed for the same crash.
        //
        // `x?` returns from the ENCLOSING function on its failure arm. A
        // statement containing one therefore cannot be hoisted into a parallel
        // worker: a worker returns void, so the `?`'s early-return lowers to a
        // `ret {i64, i64}` inside a void function and the module verifier
        // rejects it — `karac build` refuses a program `--interp` runs
        // correctly.
        //
        // B-2026-08-26-26 treated the same crash as a MISSING EFFECT SEED and
        // fixed it by naming each fallible companion (`Vec.try_push`, …) so the
        // analyzer would read it as receiver-mutating. That worked for those
        // names and left the invariant unstated, so the next `?`-bearing call
        // to arrive — `Vec.try_from_iter`, a CONSTRUCTOR companion with no
        // receiver to mutate — crashed again in exactly the same way. This is
        // the property that actually holds: it is about `?`, not about which
        // function is being called, so it covers every future one for free.
        // No need to walk the operand: a `?` nested inside one (`f(g()?)?`) is
        // subsumed by this `true`.
        ExprKind::Question(_) => true,
        ExprKind::Block(b) => block_exits(b, loop_jumps),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            expr_exits(condition, loop_jumps)
                || block_exits(then_block, loop_jumps)
                || else_branch
                    .as_ref()
                    .is_some_and(|e| expr_exits(e, loop_jumps))
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            expr_exits(value, loop_jumps)
                || block_exits(then_block, loop_jumps)
                || else_branch
                    .as_ref()
                    .is_some_and(|e| expr_exits(e, loop_jumps))
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_exits(scrutinee, loop_jumps)
                || arms.iter().any(|a| expr_exits(&a.body, loop_jumps))
        }
        ExprKind::While {
            condition, body, ..
        } => expr_exits(condition, loop_jumps) || block_exits(body, loop_jumps),
        ExprKind::For { iterable, body, .. } => {
            expr_exits(iterable, loop_jumps) || block_exits(body, loop_jumps)
        }
        ExprKind::Loop { body, .. } => block_exits(body, loop_jumps),
        ExprKind::Binary { left, right, .. }
        | ExprKind::Pipe { left, right }
        | ExprKind::NilCoalesce { left, right } => {
            expr_exits(left, loop_jumps) || expr_exits(right, loop_jumps)
        }
        ExprKind::Unary { operand, .. } => expr_exits(operand, loop_jumps),
        ExprKind::Call { callee, args } => {
            expr_exits(callee, loop_jumps) || args.iter().any(|a| expr_exits(&a.value, loop_jumps))
        }
        ExprKind::MethodCall { object, args, .. } => {
            expr_exits(object, loop_jumps) || args.iter().any(|a| expr_exits(&a.value, loop_jumps))
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            expr_exits(object, loop_jumps)
        }
        ExprKind::Index { object, index } => {
            expr_exits(object, loop_jumps) || expr_exits(index, loop_jumps)
        }
        ExprKind::Tuple(elems) => elems.iter().any(|e| expr_exits(e, loop_jumps)),
        _ => false,
    }
}

/// `true` when `stmt` can transfer control out of its enclosing block —
/// `return`, `break`, `continue`, or a `?` whose failure arm returns.
pub(super) fn stmt_has_early_exit(stmt: &Stmt) -> bool {
    stmt_exits(stmt, /* loop_jumps = */ true)
}

/// `true` when `block` can transfer control out of the enclosing FUNCTION —
/// `return`, or a `?` whose failure arm returns. B-2026-08-26-27.
///
/// The distinction from [`stmt_has_early_exit`] is `break` / `continue`, and
/// it is the difference between a correct gate and one that de-parallelizes
/// working code. Those two jump to the nearest enclosing loop, so inside a
/// NESTED loop they never leave the body being considered for parallel
/// lowering — `test_ir_reduction_fires_with_break_in_nested_loop` exists
/// precisely to keep such a reduction firing, and gating reductions on the
/// broader predicate broke it.
///
/// `?` and `return` are different in kind: both leave the FUNCTION, so a
/// parallel worker (which returns void) cannot host either.
pub(super) fn block_escapes_enclosing_fn(block: &Block) -> bool {
    block_exits(block, /* loop_jumps = */ false)
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
pub(super) fn block_has_user_defer(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_user_defer)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_user_defer(e))
}

pub(super) fn stmt_has_user_defer(stmt: &Stmt) -> bool {
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

pub(super) fn expr_has_user_defer(expr: &Expr) -> bool {
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
pub(super) fn stmt_has_channel_op(stmt: &Stmt) -> bool {
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

pub(super) fn block_has_channel_op(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_channel_op)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_channel_op(e))
}

pub(super) fn expr_has_channel_op(expr: &Expr) -> bool {
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
pub(super) fn stmt_has_console_output(stmt: &Stmt) -> bool {
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

pub(super) fn block_has_console_output(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_console_output)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_console_output(e))
}

pub(super) fn expr_has_console_output(expr: &Expr) -> bool {
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
pub(super) fn reorder_eligible(info: &StmtInfo) -> bool {
    !info.has_early_exit
        && !info.has_channel_op
        && !info.has_console_output
        && !info.is_seq
        && (!effects_mark_coroutine_boundary(&info.effects)
            || info.is_timer_suspend
            || info.is_safe_network_fanout)
}
