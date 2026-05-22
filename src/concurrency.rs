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
    matches!(
        value.kind,
        ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Identifier(_)
    )
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
    /// Top-level loops in the function body whose only loop-carried write
    /// is a reduction over an outer-scope accumulator with an op in the
    /// associative + commutative allow-list. Codegen consumes this list
    /// to lower the loop as a fan-out + reduce: each worker processes a
    /// contiguous slice of the iteration space into a per-thread partial,
    /// then a final serial pass combines the partials with the same op.
    /// See `docs/implementation_checklist/phase-7-codegen.md` — "Auto-par
    /// reduction recognition" — for the policy and slicing plan.
    pub loop_reductions: Vec<LoopReduction>,
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
#[derive(Debug, Clone)]
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

/// An effect associated with a statement.
#[derive(Debug, Clone)]
struct StmtEffect {
    verb: EffectVerbKind,
    resource: String,
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
                loop_reductions: Vec::new(),
            };
        }

        // Step 1: Extract metadata for each statement
        let stmt_infos: Vec<StmtInfo> = stmts.iter().map(|s| self.analyze_stmt(s, false)).collect();

        // Step 2: Build dependency graph (adjacency list of conflicts)
        // dep_edges[i] contains all j where i depends on j (or they must serialize)
        let mut has_edge = vec![vec![false; total_statements]; total_statements];

        for i in 0..total_statements {
            for j in 0..i {
                if self.statements_conflict(&stmt_infos[j], &stmt_infos[i]) {
                    has_edge[i][j] = true;
                    has_edge[j][i] = true;
                }
            }
        }

        // Step 3: Find maximal independent sets (greedy graph coloring approach)
        // We group statements that have no edges between them.
        let parallel_groups = self.find_parallel_groups(&stmt_infos, &has_edge, total_statements);

        // Step 4: Recognize reductions in top-level loops. Independent of
        // the parallel-group / dependency machinery — a reduction loop
        // has a loop-carried dependency that the parallel-group analysis
        // correctly serializes, but the loop's iteration space can still
        // be split across workers when the op is associative + commutative.
        let loop_reductions = self.recognize_reductions(func);

        FunctionConcurrency {
            parallel_groups,
            total_statements,
            loop_reductions,
        }
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
            is_constant_init: stmt_is_constant_init(stmt),
        };

        match &stmt.kind {
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
    ///   - blocks + blocks = CONFLICT
    ///   - suspends + suspends = CONFLICT
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
        // Group 3: allocates — self-conflict
        // Group 4: panics — self-conflict
        // Group 5: blocks — self-conflict
        // Group 6: suspends — self-conflict
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

            // Self-conflicts for singleton verbs
            (Allocates, Allocates) => true,
            (Panics, Panics) => true,
            (Blocks, Blocks) => true,
            (Suspends, Suspends) => true,

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
        has_edge: &[Vec<bool>],
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

                // Check if candidate is independent of ALL statements already in the group
                let independent = group_indices.iter().all(|&g| !has_edge[candidate][g]);

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
            PatternKind::AtBinding { name, pattern } => {
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
            ExprKind::FieldAccess { object, .. } => {
                // a.field = ... defines the root variable
                self.collect_assign_target_defines(object, defines);
            }
            ExprKind::Index { object, .. } => {
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
            ExprKind::Block(block)
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
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::SelfValue
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
