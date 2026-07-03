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
        && (!effects_mark_coroutine_boundary(&info.effects) || info.is_timer_suspend)
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
}

impl<'a> ConcurrencyChecker<'a> {
    pub fn new(program: &'a Program, effects: &'a EffectCheckResult) -> Self {
        let mut checker = ConcurrencyChecker {
            program,
            effects,
            function_bodies: HashMap::new(),
            method_bodies: HashMap::new(),
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

        if total_statements == 0 {
            return FunctionConcurrency {
                parallel_groups: Vec::new(),
                total_statements: 0,
                statement_spans: Vec::new(),
                loop_reductions: Vec::new(),
                serialization_points: Vec::new(),
                reorder_opportunities: Vec::new(),
            };
        }

        // Step 1: Extract metadata for each statement
        let stmt_infos: Vec<StmtInfo> = stmts.iter().map(|s| self.analyze_stmt(s, false)).collect();

        // Step 2: Build the conflict graph + the serialization-point list
        // (the inverse of the parallel groups: for every conflicting pair,
        // *why* they can't parallelize + which callee's effect is to blame).
        // Uses a sparse inverted index rather than a dense O(n²) matrix — see
        // `build_conflict_graph` / [`ConflictGraph`].
        let (graph, serialization_points) = self.build_conflict_graph(&stmt_infos);

        // Step 3: Find maximal independent sets (greedy graph coloring approach)
        // We group statements that have no edges between them.
        let parallel_groups = self.find_parallel_groups(&stmt_infos, &graph, total_statements);

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
        let mut resource = String::new();
        let mut verbs: Option<(EffectVerbKind, EffectVerbKind)> = None;
        let mut callees: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for ae in &a.effects {
            for be in &b.effects {
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
        for (idx, stmt) in func.body.stmts.iter().enumerate() {
            let StmtKind::Expr(expr) = &stmt.kind else {
                continue;
            };
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
                _ => continue,
            };
            if let Some((accumulator, op)) = self.classify_loop_body(body, attributes) {
                // Decline a reduction whose per-iteration work recurses into
                // the enclosing function (e.g. a backtracking counter
                // `if legal { total = total + count(...deeper...) }`). The
                // reduction itself is arithmetically valid, but parallelizing
                // it opens a fresh nested parallel region at every recursion
                // level; the fan-out compounds and exhausts the stack (a
                // SIGBUS at depth — correct output only survives for tiny
                // inputs). The sequential lowering is correct and safe, so
                // fall back to it. Direct self-recursion is the demonstrated
                // and common case (B-2026-07-03-13); transitive/mutual
                // recursion through a helper is a known residual gap.
                if crate::call_graph::block_calls_function(body, &func.name) {
                    continue;
                }
                out.push(LoopReduction {
                    accumulator,
                    op,
                    stmt_index: idx,
                    loop_line: expr.span.line,
                });
            }
        }
        out
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
                    } else if let Some(op) = reduction_binary_shape(value, &name) {
                        match reduction {
                            None => reduction = Some((name, op)),
                            Some((ref existing_name, existing_op)) => {
                                if existing_name != &name || existing_op != op {
                                    return None;
                                }
                            }
                        }
                    } else {
                        return None;
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
    fn analyze_stmt(&self, stmt: &Stmt, is_seq: bool) -> StmtInfo {
        let mut info = StmtInfo {
            defines: HashSet::new(),
            let_introduced: HashSet::new(),
            reads: HashSet::new(),
            effects: Vec::new(),
            calls_polymorphic: false,
            is_seq,
            has_early_exit: stmt_has_early_exit(stmt),
            has_channel_op: stmt_has_channel_op(stmt),
            has_console_output: stmt_has_console_output(stmt),
            is_timer_suspend: stmt_is_timer_suspend(stmt),
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

        info
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

        // Effect conflict
        self.effects_conflict(&a.effects, &b.effects)
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

        // Same resource: check verb categories
        use EffectVerbKind::*;

        // Group 1: reads/writes — same category
        // Group 2: sends/receives — same category
        // Group 3: allocates — informational, NOT a conflict (A3a; design.md)
        // Group 4: panics — informational, NOT a conflict (A3b; design.md)
        // Group 5: blocks — execution verb, NOT a conflict (A1; design.md:5907)
        // Group 6: suspends — self-conflict (pending A2)
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
    fn find_parallel_groups(
        &self,
        infos: &[StmtInfo],
        graph: &ConflictGraph,
        n: usize,
    ) -> Vec<ParallelGroup> {
        let mut groups: Vec<ParallelGroup> = Vec::new();
        let mut assigned = vec![false; n];

        // For each unassigned statement, try to build a maximal parallel group
        for start in 0..n {
            if assigned[start] {
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
                {
                    break;
                }

                // A channel-op statement ends the group at its sibling
                // boundary too (seed-side guard's candidate mirror).
                if infos[candidate].has_channel_op {
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

            // Only emit groups with more than 1 statement (parallelism requires >= 2)
            if group_indices.len() > 1 {
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
                groups.push(ParallelGroup {
                    statement_indices: group_indices,
                    reason,
                    is_trivial,
                    captured_mutations,
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
                    if let ParsedInterpolationPart::Expr(inner) = part {
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
                then_block,
                else_branch,
                ..
            } => {
                self.collect_block_inner_writes(then_block, writes);
                if let Some(e) = else_branch {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                self.collect_block_inner_writes(then_block, writes);
                if let Some(e) = else_branch {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.collect_expr_inner_writes(&arm.body, writes);
                }
            }
            ExprKind::While { body, .. } => self.collect_block_inner_writes(body, writes),
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
                if self.method_effects_imply_receiver_mutation(method) {
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
                    self.add_function_effects(&name, info);
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
                // Walk every effect key ending in `.<method>`. Builtin methods
                // (`Vec.push`, `Map.insert`, ...) live only in
                // `effects.inferred_effects`; user-defined impl methods live
                // in both `method_bodies` and `effects.inferred_effects`, so
                // iterating the latter covers both. Matches the renderer in
                // `concurrency_report::render_stmt_effects`.
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
                    if let ParsedInterpolationPart::Expr(inner) = part {
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
fn collect_push_shape(expr: &Expr) -> Option<String> {
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
