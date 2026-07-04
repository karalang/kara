//! Auto-par reduction codegen — fan-out + serial-combine lowering for
//! loops the slice-1 analyzer recognized as reductions.
//!
//! Hooked from `stmts.rs::compile_function_body`: when a top-level loop
//! statement carries a `LoopReduction` tag and matches the v1 supported
//! shape, this module synthesizes per-(op, type) `init_slot` /
//! `worker_fn` / `combine_fn` LLVM functions, builds a stack-allocated
//! `KaracReduceDescriptor`, and emits a call into the slice-2
//! `karac_par_reduce` runtime entry. After the call returns the parent-
//! allocated `out_slot` is loaded back into the source-level accumulator's
//! alloca, so subsequent reads (`println(acc)`, etc.) see the reduced
//! value.
//!
//! ## v1 supported shape
//!
//! - Source loop: `for k in lo..hi { ... }` for any `lo` expression of
//!   the accumulator type (slice 3b + 3b.3), and `while k < hi { ...;
//!   k = k + 1; }` with `let mut k: T = 0` (slice 3b.4 — while-shape
//!   still requires zero init).
//! - Op: all five recognized reduction ops — `+`, `*`, `|`, `&`, `^`
//!   (slice 3b.1).
//! - Accumulator type: any integer width — i8/i16/i32/i64 (and the
//!   matching unsigned widths, which LLVM doesn't distinguish from
//!   signed at the IR layer) (slice 3b.2). The (op, type) pair
//!   determines the identity element and combine instruction; helpers
//!   are cached per pair via the LLVM symbol table.
//! - Body: anything `compile_block` already lowers, with the source-
//!   level accumulator and loop-variable rebound to fresh per-worker
//!   allocas. Captures of outer-scope variables are passed through an
//!   env-struct (same shape as `par_blocks`'s capture machinery).
//! - Early exits (`return` / `break` / `continue`) in the body reject
//!   the lowering — they'd cross the worker-fn boundary and produce
//!   invalid IR.
//!
//! Shapes outside this set return `None` from
//! `try_emit_reduction_lowering` and the caller falls back to the
//! existing sequential codegen path; the analyzer tag is preserved,
//! ready for broader lowering when those follow-ups land.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    BinOp, Block, CompoundOp, Expr, ExprKind, Function, Item, PatternKind, Program, Stmt, StmtKind,
};
use crate::concurrency::{LoopReduction, ReductionOp};
use crate::token::IntSuffix;

use inkwell::intrinsics::Intrinsic;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, IntType, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::state::{AssertedIndexBound, VarSlot};

/// `(runtime_captures, const_int_captures)` returned by
/// `partition_const_int_captures`. The `const_int_captures` tuple
/// carries `(binding_name, literal_value, integer_suffix)` so the
/// caller can materialize each entry as a typed LLVM constant.
type ConstIntCapturePartition = (Vec<String>, Vec<(String, i64, Option<IntSuffix>)>);

impl<'ctx> super::Codegen<'ctx> {
    /// Try to lower the top-level statement at `stmt_index` (inside
    /// `parent_body`) as a recognized reduction. Returns `Ok(Some(()))`
    /// if the statement was lowered (the caller skips the normal
    /// stmt-compile path); `Ok(None)` if the shape is outside the v1
    /// supported set and the caller should fall back to sequential
    /// codegen. `Err(_)` propagates a codegen error from inside the
    /// worker-fn synthesis.
    ///
    /// `parent_body` is needed by the `while`-shape path (slice 3b.4)
    /// to peek `parent_body.stmts[stmt_index - 1]` for the loop
    /// variable's `let mut k: T = 0;` init.
    #[allow(clippy::result_large_err)]
    pub(super) fn try_emit_reduction_lowering(
        &mut self,
        parent_body: &Block,
        stmt_index: usize,
    ) -> Result<Option<()>, String> {
        let stmt = &parent_body.stmts[stmt_index];

        let reduction = self.loop_reduction_for_stmt(stmt_index).cloned();
        let Some(reduction) = reduction else {
            return Ok(None);
        };

        // Collect-style reductions take a separate code path — accumulator
        // is a `Vec[T]` (24-byte `{ptr, len, cap}`), not an integer; per-worker
        // partials live in 24-byte slots; init writes an empty Vec; combine
        // extends src into dst (heap concat + buffer-takeover). The scalar
        // helpers below assume an integer accumulator with a single-instr
        // combine, so Collect dispatches before those gates run.
        if reduction.op == ReductionOp::Collect {
            return self.try_emit_collect_reduction_lowering(parent_body, stmt_index, &reduction);
        }

        // Unpack the loop expression. Two shapes supported in v1:
        //   - `for k in 0..hi { ... }` (slice 3b)
        //   - `while k < hi { ...; k = k + 1; }` (slice 3b.4)
        // Other loop expressions fall through.
        let StmtKind::Expr(expr) = &stmt.kind else {
            return Ok(None);
        };
        let Some(shape) = self.extract_loop_shape(parent_body, stmt_index, expr) else {
            return Ok(None);
        };

        // Verify the accumulator's lowered type is one of the supported
        // integer widths (i8 / i16 / i32 / i64; unsigned widths share the
        // same LLVM int type). The (op, type) pair drives the identity
        // element and combine instruction, both threaded through to the
        // helper synthesis below. Non-int (struct / float / pointer) and
        // non-power-of-two widths fall through to sequential codegen —
        // float reductions specifically need an `#[fp_reassoc]` opt-in
        // (see `ReductionOp` doc comment) and aren't in v1.
        let Some(acc_slot) = self.variables.get(&reduction.accumulator).copied() else {
            return Ok(None);
        };
        let BasicTypeEnum::IntType(acc_int_ty) = acc_slot.ty else {
            return Ok(None);
        };
        if !matches!(acc_int_ty.get_bit_width(), 8 | 16 | 32 | 64) {
            return Ok(None);
        }

        // Early exits in the (post-stripped) body would cross the worker-fn
        // boundary and generate `ret <T>` inside a void worker fn → invalid
        // IR. Mirrors the analyzer's existing `stmt_has_early_exit` rule
        // applied to par-group siblings.
        if block_has_early_exit(&shape.body) {
            return Ok(None);
        }

        // Memory-bound gate (slice: memory-bound rejection, 2026-05-20).
        // Surfaced by the Min/Max slice's kata-153 measurement: the
        // existing cost gates (3b.5 compile-time + 3b.8 runtime-time)
        // are compute-units-aware but not memory-bandwidth-aware. For
        // a body that's mostly memory-streaming (`let x = nums[i]; if
        // x < m { m = x; }`), the compute-unit estimate looks
        // parallelizable (10M units >> 180k threshold for N=2M) but the
        // wall-clock is bottlenecked on memory bandwidth — splitting the
        // work across workers doesn't reduce wall, but does pay the
        // dispatch overhead + extra User-CPU (kata-153 saw 3.5ms → 11.8ms
        // User-CPU with no wall improvement, plus a +262 KiB binary
        // from linking par_reduce). Heuristic: skip the lowering when
        // the body has at least one Index/FieldAccess (memory access)
        // AND no substantial function/method call (a substantial call
        // suggests compute work beyond the memory access). Trivial
        // accessor MethodCalls — `len`, `is_empty`, `as_slice`,
        // `as_str` — don't count as substantial; they're just shape
        // queries on the collection. The gate fires *before* the
        // cost-model gates so the per-iter estimate isn't wasted on a
        // loop we'll reject anyway.
        if body_is_memory_bound(&shape.body) {
            return Ok(None);
        }

        // Estimate per-iter body cost once — used for both the codegen-
        // time gate (literal-K loops) below and the runtime-time gate
        // (slice 3b.8) via the descriptor's `per_iter_cost_units` field.
        // The body walker bottoms at 1, never 0, so a sentinel-0 in the
        // emitted descriptor only happens if codegen-side estimation is
        // intentionally skipped (it isn't here). Uses `program_snapshot`
        // to thread a free-fn body lookup into the estimator so calls
        // into known callees fold the callee's body cost into the per-
        // iter total instead of counting them as the opaque CALL_COST_UNITS
        // constant (slice: cost-gate fn-call body cost, 2026-05-20).
        let per_iter_cost = match &self.program_snapshot {
            Some(prog) => CostEstimator::new(prog).estimate_body(&shape.body),
            None => estimate_body_cost_units(&shape.body),
        };

        // Cost-model gate (slice 3b.5, 2026-05-20). When the iteration
        // count is statically known and the per-iter cost estimate puts
        // total work below `REDUCE_DISPATCH_THRESHOLD_UNITS`, the
        // par_reduce dispatch overhead (Box alloc + queue push + Condvar
        // wake/wait + N-way combine) would dominate the actual loop
        // work — sequential codegen wins by ~µs to ~ms. Variable-K
        // loops (including variable-lo loops) bypass this compile-time
        // gate (in practice they're typically large, like the kata-7
        // bench's `k_iters = 50_000_000`); the runtime-side gate
        // (slice 3b.8) catches the rare small variable-K case at run
        // time using the same `per_iter_cost` threaded into the
        // descriptor below.
        if let Some(k) = const_eval_iter_count(&shape.end_expr, shape.lo_expr.as_ref()) {
            let total = k.saturating_mul(per_iter_cost);
            if total < REDUCE_DISPATCH_THRESHOLD_UNITS {
                return Ok(None);
            }
        }

        // Compile the end bound (and `lo`, if present) in the parent
        // context. `iter_total = end - lo` is what the runtime sees;
        // it's widened to i64 below for the descriptor's `iter_total`
        // field. `lo` itself is threaded into the worker through env-
        // struct field 0 (slice 3b.3) so the worker can shift its
        // chunk-local index back to the source-level `k`.
        let end_val = self.compile_expr(&shape.end_expr)?.into_int_value();

        // The source-level loop variable's type is unified with the
        // range elem type, which equals end_val's type. The body's
        // `acc <op> k` requires acc and k to have the same int type
        // (no implicit numeric conversion in kara), so a mismatch
        // between end_val's type and the accumulator's type means the
        // source wouldn't have type-checked in the first place — but
        // we belt-and-suspenders gate it explicitly here so the worker
        // fn synthesis can rely on `loop_var_ty == acc_int_ty` and emit
        // one consistent type throughout. The dead `end_val` instructions
        // when this gate fires are removed by LLVM's DCE pass.
        if end_val.get_type() != acc_int_ty {
            return Ok(None);
        }

        // Compile `lo` once in the parent (if present) and compute
        // `iter_total = end - lo`. Both operands are `acc_int_ty`; the
        // type check above guarantees `end_val`'s type, and the source
        // typechecker's range-unification rule guarantees `lo`'s type
        // matches `end`'s (same belt-and-suspenders gate fires if the
        // typed AST somehow violates it).
        let (iter_total_val, lo_val) = match &shape.lo_expr {
            None => (end_val, None),
            Some(lo_expr) => {
                let lo_val = self.compile_expr(lo_expr)?.into_int_value();
                if lo_val.get_type() != acc_int_ty {
                    return Ok(None);
                }
                let iter_total = self
                    .builder
                    .build_int_sub(end_val, lo_val, "iter.total")
                    .unwrap();
                (iter_total, Some(lo_val))
            }
        };

        // Synthesize the per-(op, type) helper functions.
        let init_fn = self.emit_reduce_init_fn(reduction.op, acc_int_ty);
        let combine_fn = self.emit_reduce_combine_fn(reduction.op, acc_int_ty);

        // Capture set for the worker fn: variables the body reads that
        // aren't the accumulator, aren't the loop variable, and aren't
        // introduced inside the body itself. Filtered to live entries in
        // `self.variables` so module-level functions, struct names, etc.
        // (which `refs_in_block` doesn't distinguish) drop out cleanly.
        //
        // Partitioned into runtime captures (flow through the env-struct
        // load in the worker) and const-int captures (materialized as
        // LLVM constants directly in the worker body, so downstream uses
        // like `k % const_pow2` fold to an `and`-mask). The const-prop
        // path covers the common bench-shape `let n: i64 = 8i64;` →
        // `idx = k % n` pattern; without it, LLVM can't see across the
        // par-reduce runtime call boundary into the descriptor's ctx
        // field and is forced to emit a runtime sdiv/msub per iter.
        let captures =
            self.collect_reduction_captures(&shape.body, &reduction.accumulator, &shape.loop_var);
        let (runtime_captures, const_int_captures) =
            self.partition_const_int_captures(&captures, parent_body, stmt_index);

        let worker_fn = self.emit_reduce_worker_fn(
            &reduction,
            acc_int_ty,
            &shape.loop_var,
            &shape.body,
            &runtime_captures,
            &const_int_captures,
            lo_val.is_some(),
        )?;

        self.emit_reduce_call(
            init_fn,
            worker_fn,
            combine_fn,
            iter_total_val,
            acc_slot,
            acc_int_ty,
            &reduction,
            &runtime_captures,
            lo_val,
            per_iter_cost,
        )?;

        Ok(Some(()))
    }

    /// Extract the canonical shape of a recognized reduction loop. Returns
    /// `Some(LoopShape)` when the loop matches one of v1's supported shapes
    /// (for-range with `lo == 0`, or while with an explicit `k = k + 1`
    /// induction step preceded by `let mut k: T = 0;`), `None` otherwise.
    /// Decouples the shape-parsing complexity from the lowering caller so
    /// future shapes (non-zero `lo`, larger step constants, while_let,
    /// loop with break, etc.) extend by adding match arms here without
    /// changing the lowering body.
    fn extract_loop_shape(
        &self,
        parent_body: &Block,
        stmt_index: usize,
        expr: &Expr,
    ) -> Option<LoopShape> {
        match &expr.kind {
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                // `for k in ..` binds `k`; `for _ in ..` discards it. The
                // wildcard case is just as parallelizable — the body never
                // reads the loop variable, so the reduction is independent
                // of iteration order. Synthesize a sentinel name (with
                // chars illegal in a source identifier, so it can never
                // collide with a captured outer variable) for the unused
                // per-worker loop-index alloca. Other pattern kinds (tuple,
                // struct destructure, etc.) aren't loop-counter shapes and
                // fall through to sequential codegen.
                let loop_var = match &pattern.kind {
                    PatternKind::Binding(name) => name.clone(),
                    PatternKind::Wildcard => "<reduce-wildcard-idx>".to_string(),
                    _ => return None,
                };
                let ExprKind::Range {
                    start,
                    end,
                    inclusive: false,
                } = &iterable.kind
                else {
                    return None;
                };
                let end_expr = end.as_ref()?;
                // Slice 3b.3: any `lo` expression of the accumulator
                // type is supported by adding it to the worker's chunk-
                // local index. `None` / `Integer(0)` normalize to
                // `lo_expr = None` (no shift math — the worker's local
                // index already matches the source-level k).
                let lo_expr = match start.as_deref() {
                    None => None,
                    Some(s) if matches!(s.kind, ExprKind::Integer(0, _)) => None,
                    Some(s) => Some(s.clone()),
                };
                Some(LoopShape {
                    loop_var,
                    end_expr: (**end_expr).clone(),
                    body: body.clone(),
                    lo_expr,
                })
            }
            ExprKind::While {
                condition, body, ..
            } => {
                // Pull `loop_var` and `end_expr` out of the condition.
                // Accepts both `Binary { Lt, Ident(k), end }` (pre-
                // lowering) and `Call(Path([T, "lt"]), [Ident(k), end])`
                // (post-lowering). The body must contain exactly one step-
                // 1 increment of the loop var as its terminal stmt; the
                // recognizer (slice 1) already accepted the loop as an
                // induction-step + reduction pair, so we can be opinionated
                // about the shape here.
                let (loop_var, end_expr) = parse_lt_condition(condition)?;

                // The body's last stmt must be `loop_var = loop_var + 1`
                // (or `loop_var += 1`, either pre- or post-lowered). Strip
                // it so the worker's loop scaffolding handles the
                // increment via the back-edge — same shape as the for-loop
                // path, no need to re-think the worker fn synth.
                let stripped_body = strip_terminal_step_one_increment(body, &loop_var)?;

                // The immediately preceding stmt must be `let mut k: T =
                // <anything>;`. Slices 3b.9 + 3b.10 normalize the init:
                //   - `Integer(0)`: `lo_expr = None` (no shift math).
                //   - Non-zero int literal: `lo_expr = Some(literal)` —
                //     re-compile the literal in the par_reduce setup;
                //     it's a constant, no side effects, free.
                //   - Anything else: `lo_expr = Some(Identifier(k))` —
                //     load from the parent's k alloca (the let-stmt
                //     already evaluated the init expression and stored
                //     the result; reading it back guarantees single
                //     evaluation regardless of side effects in the init
                //     expression).
                // Adjacent let + while (no intervening stmts) means
                // nothing modifies k between the init and the dispatch.
                if stmt_index == 0 {
                    return None;
                }
                let init_expr = preceding_stmt_init(parent_body, stmt_index, &loop_var)?;
                let lo_expr = match &init_expr.kind {
                    ExprKind::Integer(0, _) => None,
                    ExprKind::Integer(_, _) => Some(init_expr),
                    _ => Some(Expr {
                        kind: ExprKind::Identifier(loop_var.clone()),
                        span: init_expr.span,
                    }),
                };

                Some(LoopShape {
                    loop_var,
                    end_expr,
                    body: stripped_body,
                    lo_expr,
                })
            }
            _ => None,
        }
    }

    /// The set of outer-scope variables the body reads, minus the
    /// accumulator, the loop variable, and any body-local let-bindings.
    /// Sorted so the env-struct field order is deterministic across runs.
    fn collect_reduction_captures(
        &self,
        body: &Block,
        acc_name: &str,
        loop_var_name: &str,
    ) -> Vec<String> {
        let mut refs: HashSet<String> = HashSet::new();
        let mut defs: HashSet<String> = HashSet::new();
        self.refs_in_block(body, &mut refs, &mut defs);
        let mut out: Vec<String> = refs
            .into_iter()
            .filter(|n| n != acc_name)
            .filter(|n| n != loop_var_name)
            .filter(|n| !defs.contains(n))
            .filter(|n| self.variables.contains_key(n.as_str()))
            .collect();
        out.sort();
        out
    }

    /// Partition the capture set into (runtime, const_int). A capture is
    /// "const_int" when its defining `let` in `parent_body` is non-mut,
    /// initializes from an integer literal, and isn't subsequently
    /// reassigned before `stmt_index`. Const-int captures get materialized
    /// directly into the worker fn as LLVM constants (so LLVM can fold
    /// downstream `k % CONST_POW2` into an `and`-mask, etc.) instead of
    /// flowing through the par-reduce env-struct load.
    ///
    /// Only handles top-level `let` statements in `parent_body` for v1.
    /// Captures defined in nested blocks above the loop, or via
    /// `let mut` plus a later assignment, stay on the runtime path.
    /// This is the common case for bench-shape constants like
    /// `let n: i64 = 8i64;`.
    fn partition_const_int_captures(
        &self,
        captures: &[String],
        parent_body: &Block,
        stmt_index: usize,
    ) -> ConstIntCapturePartition {
        let mut runtime = Vec::with_capacity(captures.len());
        let mut consts = Vec::new();
        for name in captures {
            if let Some((value, sfx)) = find_top_level_const_int_init(parent_body, name, stmt_index)
            {
                consts.push((name.clone(), value, sfx));
            } else {
                runtime.push(name.clone());
            }
        }
        (runtime, consts)
    }

    /// Walk the worker body looking for indexing sites whose bounds can
    /// be hoisted out of the per-iter check via a one-time preflight at
    /// fn entry. v1 recognizes the pattern:
    ///
    /// ```text
    /// let idx = <loop_var> % <positive_int_literal>;  // top-level in body
    /// ...
    /// <captured_vec>[idx]  // anywhere in body, possibly nested in calls
    /// ```
    ///
    /// Given:
    /// - the par-reduce loop var is known >= 0 (see the assume in
    ///   `emit_reduce_worker_fn` — the runtime always passes start as a
    ///   usize and the back-edge only adds 1, so SCEV proves the chain),
    /// - the modulo divisor is a positive literal,
    ///
    /// `idx` lives in `[0, divisor)`. If we additionally prove
    /// `captured_vec.len() >= divisor` once at fn entry (the preflight
    /// check), every per-iter `captured_vec[idx]` bounds check becomes
    /// redundant.
    ///
    /// Conservative rules for v1 — both keep the analysis local and
    /// avoid soundness traps without much loss of coverage on the kata
    /// surface:
    ///
    /// - The let-binding for `idx` must be at the body's top level
    ///   (not nested inside if/match/for/while/etc.). Bench-shape
    ///   pattern; nested would need scope-aware tracking to be sound.
    /// - `idx` must be non-mut. A `let mut idx = ...; idx = ...;`
    ///   could change the bound between iterations.
    /// - The captured vec must not be mutated anywhere in the body.
    ///   Conservative: any method call on the vec or any
    ///   `vec[...] = ...` write disqualifies — `len`/`is_empty` would
    ///   be sound to allow but the bench doesn't use them on the
    ///   captured vec, so the conservative rule costs nothing.
    /// - The vec must be a runtime capture in `self.variables` (so its
    ///   identity is stable and len is loop-invariant). Const-int
    ///   captures are scalars, not Vecs, so the filter naturally
    ///   excludes them.
    fn find_modulo_hoistable_bounds(
        &self,
        body: &Block,
        loop_var: &str,
        runtime_captures: &[String],
        const_int_captures: &[(String, i64, Option<IntSuffix>)],
    ) -> Vec<HoistableModuloBound> {
        // Const-int captures: `n` in `let n: i64 = 8i64;` gets materialized
        // as an LLVM constant inside the worker (see partition_const_int_
        // captures), but the AST still sees `k % n` as a Binary{Mod, k,
        // Identifier("n")}. Look up `n` here to recover the literal value
        // so the BCE recognizer doesn't miss the const-propped shape.
        let const_int_lookup: HashMap<&str, i64> = const_int_captures
            .iter()
            .map(|(name, value, _sfx)| (name.as_str(), *value))
            .collect();

        // Top-level let-bindings of the form `let idx = loop_var % LIT`
        // (or `loop_var % const_int_capture` resolved via the lookup).
        let mut idx_to_upper: HashMap<String, i64> = HashMap::new();
        for stmt in &body.stmts {
            let StmtKind::Let {
                is_mut: false,
                pattern,
                value,
                ..
            } = &stmt.kind
            else {
                continue;
            };
            let PatternKind::Binding(idx_name) = &pattern.kind else {
                continue;
            };
            if let Some(upper) = modulo_upper_for_loop_var(value, loop_var, &const_int_lookup) {
                if upper > 0 {
                    // First binding wins — a re-let with the same name
                    // shadows but the first one is what the indexing in
                    // between observes. For v1 just disable BCE in that
                    // case rather than reasoning about shadow lifetimes.
                    idx_to_upper.entry(idx_name.clone()).or_insert(upper);
                }
            }
        }
        if idx_to_upper.is_empty() {
            return Vec::new();
        }

        // Collect names of vecs that are mutated anywhere in the body —
        // these can't safely hoist their bounds check.
        let mut mutated: HashSet<String> = HashSet::new();
        for stmt in &body.stmts {
            collect_mutated_vec_names_in_stmt(stmt, &mut mutated);
        }
        if let Some(e) = &body.final_expr {
            collect_mutated_vec_names_in_expr(e, &mut mutated);
        }

        let captured: HashSet<&str> = runtime_captures.iter().map(String::as_str).collect();

        // Walk for `<captured_vec>[<idx_var>]` sites and record one
        // HoistableModuloBound per unique (vec, idx) pair. Same vec/idx
        // pair indexed in multiple places only needs one preflight.
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut out: Vec<HoistableModuloBound> = Vec::new();
        for stmt in &body.stmts {
            collect_modulo_index_sites_in_stmt(
                stmt,
                &captured,
                &idx_to_upper,
                &mutated,
                &mut seen,
                &mut out,
            );
        }
        if let Some(e) = &body.final_expr {
            collect_modulo_index_sites_in_expr(
                e,
                &captured,
                &idx_to_upper,
                &mutated,
                &mut seen,
                &mut out,
            );
        }

        // Drop any bound whose captured base is a by-pointer fixed-size
        // `[N x T]` array (B-2026-06-15-3): unlike a Vec, an array capture has
        // NO `{ptr,len,cap}` header — its VarSlot points straight at the inline
        // elements, so `emit_modulo_bce_preflight`'s `build_struct_gep(_, _, 1)`
        // would read element[1] as the "length" and panic on that garbage
        // (the auto-par `atab[k % m]` miscompile found by kata 60: for
        // `btab = [1,2,3,4]`, element[1] = 2 < upper 4 → false preflight trap).
        // An array's length is the compile-time constant N, so its per-iter
        // `data[idx]` bounds check against static N is already emitted and
        // correct — just don't hoist. Only Vec captures (a genuine runtime
        // length in field 1) are eligible for the preflight + BCE elision.
        out.retain(|b| {
            !matches!(
                self.variables.get(&b.vec_var).map(|s| s.ty),
                Some(BasicTypeEnum::ArrayType(_))
            )
        });

        // Deterministic order — env-struct etc. already use sorted keys
        // for IR stability; bounds order doesn't affect correctness but
        // a stable order keeps IR-text-pinned tests reproducible.
        out.sort_by(|a, b| {
            (&a.vec_var, &a.idx_var, a.upper_lit).cmp(&(&b.vec_var, &b.idx_var, b.upper_lit))
        });
        out
    }

    /// Emit a one-time `if vec.len() < UPPER_LIT panic` check for the
    /// captured Vec named in `bound`, at the current builder position.
    /// On entry the builder is in some block B; on return the builder
    /// is positioned in the post-check "ok" block. The fail-path block
    /// is terminated with `unreachable` after the panic call.
    fn emit_modulo_bce_preflight(
        &mut self,
        bound: &HoistableModuloBound,
        worker_fn: FunctionValue<'ctx>,
    ) {
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        // The captured Vec was unpacked into a worker-local alloca; the
        // alloca is the struct pointer (owned, not ref).
        let vec_ptr = self
            .variables
            .get(&bound.vec_var)
            .expect("hoistable BCE referenced a missing capture")
            .ptr;
        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 1, "bce.len.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "bce.len")
            .unwrap()
            .into_int_value();
        let lit = i64_t.const_int(bound.upper_lit as u64, true);
        let fail_bb = self
            .context
            .append_basic_block(worker_fn, "bce.preflight.fail");
        let ok_bb = self
            .context
            .append_basic_block(worker_fn, "bce.preflight.ok");
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::ULT, len, lit, "bce.preflight.cmp")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp, fail_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(fail_bb);
        self.emit_panic("vec index out of bounds");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
    }

    /// Synthesize `void init_slot(*mut u8 slot) { *(IntT*)slot = identity; }`
    /// for the given `(op, int_ty)` pair. Helpers are cached per pair via
    /// the LLVM symbol table (re-adding the same name returns the existing
    /// function), so multiple reduction sites in the same module that share
    /// an (op, type) share one definition.
    ///
    /// Identity per op:
    /// - `Add`, `BitOr`, `BitXor` → 0
    /// - `Mul`                    → 1
    /// - `BitAnd`                 → all-ones (-1 / `TYPE_MAX` unsigned —
    ///   same bit pattern under two's-complement, which LLVM uses uniformly)
    fn emit_reduce_init_fn(
        &mut self,
        op: ReductionOp,
        int_ty: IntType<'ctx>,
    ) -> FunctionValue<'ctx> {
        let name = reduce_helper_name("init", op, int_ty);
        if let Some(existing) = self.module.get_function(&name) {
            return existing;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_ty = self
            .context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::from(ptr_ty)], false);
        let f = self.module.add_function(&name, fn_ty, None);

        let saved_bb = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let slot_ptr = f.get_nth_param(0).unwrap().into_pointer_value();
        self.builder
            .build_store(slot_ptr, reduce_identity(op, int_ty))
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Synthesize `void combine(*mut u8 dst, *const u8 src)
    /// { *(IntT*)dst = *(IntT*)dst <op> *(IntT*)src; }` for the given
    /// `(op, int_ty)` pair. Same caching pattern as `emit_reduce_init_fn`.
    /// Op → LLVM instruction:
    /// - `Add`    → `add`
    /// - `Mul`    → `mul`
    /// - `BitOr`  → `or`
    /// - `BitAnd` → `and`
    /// - `BitXor` → `xor`
    fn emit_reduce_combine_fn(
        &mut self,
        op: ReductionOp,
        int_ty: IntType<'ctx>,
    ) -> FunctionValue<'ctx> {
        let name = reduce_helper_name("combine", op, int_ty);
        if let Some(existing) = self.module.get_function(&name) {
            return existing;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_ty = self.context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_ty),
                BasicMetadataTypeEnum::from(ptr_ty),
            ],
            false,
        );
        let f = self.module.add_function(&name, fn_ty, None);

        let saved_bb = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let dst_ptr = f.get_nth_param(0).unwrap().into_pointer_value();
        let src_ptr = f.get_nth_param(1).unwrap().into_pointer_value();
        let d = self
            .builder
            .build_load(int_ty, dst_ptr, "d")
            .unwrap()
            .into_int_value();
        let s = self
            .builder
            .build_load(int_ty, src_ptr, "s")
            .unwrap()
            .into_int_value();
        let folded = self.emit_reduce_combine_inst(op, d, s);
        self.builder.build_store(dst_ptr, folded).unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Emit the combine instruction for two `IntValue`s under `op`. Shared
    /// between the combine fn body (per-pair fold) and `emit_reduce_call`'s
    /// post-call fold that folds the parent's pre-existing accumulator
    /// value with the par_reduce result. Keeping the per-op selection in
    /// one helper means a future op addition only updates one match.
    ///
    /// For Min/Max, emits `icmp slt`/`icmp sgt` + `select` — `-O2`'s
    /// InstCombine lifts the idiom to `llvm.smin.iN` / `llvm.smax.iN`
    /// intrinsics at the backend.
    fn emit_reduce_combine_inst(
        &self,
        op: ReductionOp,
        d: IntValue<'ctx>,
        s: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        match op {
            ReductionOp::Add => self.builder.build_int_add(d, s, "sum").unwrap(),
            ReductionOp::Mul => self.builder.build_int_mul(d, s, "prod").unwrap(),
            ReductionOp::BitOr => self.builder.build_or(d, s, "or").unwrap(),
            ReductionOp::BitAnd => self.builder.build_and(d, s, "and").unwrap(),
            ReductionOp::BitXor => self.builder.build_xor(d, s, "xor").unwrap(),
            ReductionOp::Min => {
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, d, s, "min.cmp")
                    .unwrap();
                self.builder
                    .build_select(cmp, d, s, "min")
                    .unwrap()
                    .into_int_value()
            }
            ReductionOp::Max => {
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGT, d, s, "max.cmp")
                    .unwrap();
                self.builder
                    .build_select(cmp, d, s, "max")
                    .unwrap()
                    .into_int_value()
            }
            // Collect's combine is heap-Vec extend, not a single LLVM
            // instruction. Guarded out by `try_emit_reduction_lowering`'s
            // early-return; unreachable in Phase 2.
            ReductionOp::Collect => unreachable!("Collect bypasses int combine"),
        }
    }

    /// Synthesize the per-call worker fn. Each call emits a fresh function
    /// named `__karac_reduce_worker_<N>` (`N` monotonically allocated from
    /// `par_counter` so collisions can't happen across multiple reduction
    /// sites in the same module). Body shape:
    ///
    /// ```text
    /// void worker(ptr slot, i64 start, i64 end, ptr ctx, ptr cancel) {
    ///   // Unpack captures from ctx into local allocas.
    ///   let cap0 = ((env*)ctx)->field_0;
    ///   ...
    ///   // Local accumulator + loop variable.
    ///   let mut <acc> = identity;
    ///   let mut <k> = start;
    ///   while (k < end) {
    ///     // The source-level body, lowered against the local <acc>,
    ///     // <k>, and capture allocas.
    ///     <body>
    ///     k = k + 1;
    ///   }
    ///   // Publish the partial back to the caller's slot.
    ///   *(i64*)slot = <acc>;
    /// }
    /// ```
    ///
    /// State save/restore mirrors `emit_par_branch_fn` so compiling the
    /// body recursively doesn't leak loop frames, variable bindings, or
    /// cleanup actions back into the parent function.
    #[allow(clippy::result_large_err)]
    #[allow(clippy::too_many_arguments)]
    fn emit_reduce_worker_fn(
        &mut self,
        reduction: &LoopReduction,
        acc_int_ty: IntType<'ctx>,
        loop_var_name: &str,
        body: &Block,
        captures: &[String],
        const_int_captures: &[(String, i64, Option<IntSuffix>)],
        has_lo: bool,
    ) -> Result<FunctionValue<'ctx>, String> {
        let worker_id = self.par_counter;
        self.par_counter += 1;
        let name = format!("__karac_reduce_worker_{worker_id}");

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let fn_ty = self.context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_ty), // slot
                BasicMetadataTypeEnum::from(i64_t),  // start
                BasicMetadataTypeEnum::from(i64_t),  // end
                BasicMetadataTypeEnum::from(ptr_ty), // ctx
                BasicMetadataTypeEnum::from(ptr_ty), // cancel
            ],
            false,
        );
        let worker_fn = self.module.add_function(&name, fn_ty, None);

        // Save outer codegen state — about to compile body in a fresh
        // function context. Mirror `emit_par_branch_fn`'s save/restore.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_cleanup = std::mem::take(&mut self.scope_cleanup_actions);
        let saved_cancel_ptr = self.branch_cancel_ptr.take();
        self.scope_cleanup_actions.push(Vec::new());

        self.current_fn = Some(worker_fn);
        let entry = self.context.append_basic_block(worker_fn, "entry");
        self.builder.position_at_end(entry);

        // Build the env-struct type. Layout (slice 3b.3):
        //   - If `has_lo`: field 0 is `lo: acc_int_ty`, then captures.
        //   - Otherwise: just captures (current shape from 3b/3b.1/3b.2).
        // env-struct is present (env_ctx_ptr != null) iff `has_lo` or
        // there's at least one capture — both conditions need the same
        // unpack channel.
        let env_struct_ty: Option<StructType<'ctx>> = if !has_lo && captures.is_empty() {
            None
        } else {
            let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(captures.len() + 1);
            if has_lo {
                field_tys.push(acc_int_ty.into());
            }
            let env_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            for n in captures {
                let ty = saved_vars[n].ty;
                // B-2026-06-15-3: a fixed-size `[N x T]` array capture is passed
                // BY POINTER, not inline — see the extract loop below.
                field_tys.push(if matches!(ty, BasicTypeEnum::ArrayType(_)) {
                    env_ptr_ty.into()
                } else {
                    ty
                });
            }
            Some(self.context.struct_type(&field_tys, false))
        };

        // `lo_in_worker` holds the worker-local copy of the source-level
        // start bound — added to raw_start/raw_end below to recover the
        // source-level k. `None` when `has_lo` is false (no shift math).
        let mut lo_in_worker: Option<IntValue<'ctx>> = None;

        if let Some(env_ty) = env_struct_ty {
            let ctx_ptr = worker_fn.get_nth_param(3).unwrap().into_pointer_value();
            let env_val = self
                .builder
                .build_load::<BasicTypeEnum<'ctx>>(env_ty.into(), ctx_ptr, "__reduce_env_load")
                .unwrap()
                .into_struct_value();
            // Field 0 holds `lo` when present. Extract as a plain
            // IntValue — no alloca needed; it's only read twice (in the
            // start/end shift below) and never written.
            let capture_field_base = if has_lo {
                let lo_field = self
                    .builder
                    .build_extract_value(env_val, 0, "__reduce_lo")
                    .unwrap()
                    .into_int_value();
                lo_in_worker = Some(lo_field);
                1
            } else {
                0
            };
            for (i, var_name) in captures.iter().enumerate() {
                let cap_ty = saved_vars[var_name].ty;
                let field_idx = (capture_field_base + i) as u32;
                let field_val = self
                    .builder
                    .build_extract_value(env_val, field_idx, var_name)
                    .unwrap();
                if matches!(cap_ty, BasicTypeEnum::ArrayType(_)) {
                    // B-2026-06-15-3: by-pointer array capture — the env field
                    // IS the pointer to the parent's array (a read-only
                    // reduction input; the parent frame outlives the join).
                    // Register it directly as the var's slot (ty stays the
                    // array type so `data[idx]` GEPs correctly) — NO alloca +
                    // by-value copy. Passing the `[N x T]` inline made LLVM O2
                    // scalarize the 40 KB env load/store into N element ops,
                    // blowing up DAGCombiner store-merging (auto-par compile of
                    // brute_force 0.07 -> 2.15 s; #3629 60 s).
                    self.variables.insert(
                        var_name.clone(),
                        VarSlot {
                            ptr: field_val.into_pointer_value(),
                            ty: cap_ty,
                        },
                    );
                } else {
                    let alloca = self.create_entry_alloca(worker_fn, var_name, cap_ty);
                    self.builder.build_store(alloca, field_val).unwrap();
                    self.variables.insert(
                        var_name.clone(),
                        VarSlot {
                            ptr: alloca,
                            ty: cap_ty,
                        },
                    );
                }
                if let Some(type_name) = saved_var_types.get(var_name) {
                    self.var_type_names
                        .insert(var_name.clone(), type_name.clone());
                }
            }
        }

        // Materialize const-int captures as LLVM constants stored into
        // worker-local allocas. The store-of-const pattern (rather than
        // a pure SSA constant) keeps the body's read path uniform with
        // runtime captures — the body emits an ordinary `load` against
        // `self.variables[name].ptr` either way, and LLVM's mem2reg +
        // InstCombine collapse the alloca/store/load chain into the bare
        // constant downstream. Const captures *do not* appear in the env
        // struct (see emit_reduce_call's matching capture loop), so they
        // cost zero in descriptor bandwidth.
        for (var_name, value, sfx) in const_int_captures {
            let cap_ty = saved_vars[var_name].ty;
            let const_val = self.const_int_for_suffix(*value, *sfx);
            let alloca = self.create_entry_alloca(worker_fn, var_name, cap_ty);
            self.builder.build_store(alloca, const_val).unwrap();
            self.variables.insert(
                var_name.clone(),
                VarSlot {
                    ptr: alloca,
                    ty: cap_ty,
                },
            );
            if let Some(type_name) = saved_var_types.get(var_name) {
                self.var_type_names
                    .insert(var_name.clone(), type_name.clone());
            }
        }

        // Allocate the worker-local accumulator at the (op, type) identity
        // (see `reduce_identity`): 0 for `+` / `|` / `^`, 1 for `*`,
        // all-ones for `&`. The combine fn folds these per-worker partials
        // into the final result.
        let acc_alloca =
            self.create_entry_alloca(worker_fn, &reduction.accumulator, acc_int_ty.into());
        self.builder
            .build_store(acc_alloca, reduce_identity(reduction.op, acc_int_ty))
            .unwrap();
        self.variables.insert(
            reduction.accumulator.clone(),
            VarSlot {
                ptr: acc_alloca,
                ty: acc_int_ty.into(),
            },
        );

        // Allocate the loop variable, init to `start`. The body sees
        // `<loop_var>` as a plain mutable alloca of `acc_int_ty`; the
        // increment runs in the bottom of `loop.body` (between body
        // emission and the back-edge), so a body-internal read of
        // `<loop_var>` observes the current iteration's value. The
        // runtime calls workers with i64 start/end (descriptor-driven);
        // for narrower loop var types we truncate here. The gate in
        // `try_emit_reduction_lowering` ensured the source end value fits
        // in `acc_int_ty`, so the truncation is value-preserving.
        let raw_start = worker_fn.get_nth_param(1).unwrap().into_int_value();
        let raw_end = worker_fn.get_nth_param(2).unwrap().into_int_value();
        let (start_val, end_val) = if acc_int_ty.get_bit_width() < 64 {
            let s = self
                .builder
                .build_int_truncate(raw_start, acc_int_ty, "start.trunc")
                .unwrap();
            let e = self
                .builder
                .build_int_truncate(raw_end, acc_int_ty, "end.trunc")
                .unwrap();
            (s, e)
        } else {
            (raw_start, raw_end)
        };
        // Slice 3b.3: shift the chunk-local indices by the source-level
        // start bound so the body's `k` reads observe the right values.
        // For `for k in 5..15`: iter_total = 10, worker sees raw 0..10,
        // shifted by lo=5 → 5..15. For `lo == 0` (the common case), no
        // shift math at all — `lo_in_worker` is None.
        let (start_val, end_val) = match lo_in_worker {
            Some(lo) => {
                let s = self
                    .builder
                    .build_int_add(start_val, lo, "start.shift")
                    .unwrap();
                let e = self
                    .builder
                    .build_int_add(end_val, lo, "end.shift")
                    .unwrap();
                (s, e)
            }
            None => (start_val, end_val),
        };
        // Tell LLVM the loop variable stays non-negative for the whole
        // worker. The runtime passes start/end as non-negative usize-
        // sized values; when `lo_in_worker` is None, the worker-local
        // start_val == raw chunk start >= 0, and the back-edge only ever
        // adds 1, so SCEV can induct `k >= 0` across the loop. With that
        // fact in hand, InstCombine folds signed-mod / signed-div by
        // positive power-of-two literals (`srem k, 8` → `urem k, 8` →
        // `and k, 7`) instead of emitting the four-instruction signed-
        // mod sequence (`negs/and/and/csneg` on ARM64). Surfaced on the
        // kata-8 atoi bench whose inner `idx = k % n` with `n=8` was
        // hitting the signed sequence.
        //
        // Restricted to `lo_in_worker.is_none()` — for non-zero lo we
        // don't have a compile-time guarantee that `lo + raw_start` is
        // still non-negative (the kata's existing slice 3b.3 supports
        // any lo, including negative). Generalizing is a follow-up:
        // accept the assume when `lo_expr` proves >= 0 at codegen time.
        if lo_in_worker.is_none() {
            let assume_intrinsic = Intrinsic::find("llvm.assume").expect("llvm.assume must exist");
            // Not overloaded, so empty param-types is correct.
            let assume_fn = assume_intrinsic
                .get_declaration(&self.module, &[])
                .expect("llvm.assume declaration");
            let nonneg = self
                .builder
                .build_int_compare(
                    IntPredicate::SGE,
                    start_val,
                    acc_int_ty.const_zero(),
                    "k.start.nonneg",
                )
                .unwrap();
            self.builder
                .build_call(assume_fn, &[nonneg.into()], "")
                .unwrap();
        }
        let k_alloca = self.create_entry_alloca(worker_fn, loop_var_name, acc_int_ty.into());
        self.builder.build_store(k_alloca, start_val).unwrap();
        self.variables.insert(
            loop_var_name.to_string(),
            VarSlot {
                ptr: k_alloca,
                ty: acc_int_ty.into(),
            },
        );

        // Hoisted bounds-check elision (slice: par-reduce modulo BCE).
        // For each top-level `let idx = loop_var % POSITIVE_LIT` in the
        // body, every `<captured_vec>[idx]` use can skip its per-iter
        // bounds check if we prove ONCE here that `captured_vec.len() >=
        // POSITIVE_LIT`. Without this, the kata-8 atoi inner loop spent
        // 2 ARM instructions per iter on `cmp x4, x12 / b.hs panic` even
        // though `idx ∈ [0, 8)` and `inputs.len() == 8` are both
        // statically determinable. Emitting the preflight + pushing
        // `LowerBound { idx } + UpperBound { idx, vec_var }` facts into
        // `asserted_index_bounds` lets `emit_split_bounds_check` drop the
        // per-iter check entirely.
        //
        // Restricted to `lo_in_worker.is_none()` (mirror of the assume
        // gate above): the loop-var-non-negative assumption that makes
        // `idx ∈ [0, LIT)` sound only holds when we don't apply a
        // non-zero lo shift. Generalizing along the same surface as the
        // assume.
        let pushed_bce_facts = if lo_in_worker.is_none() {
            let hoistable = self.find_modulo_hoistable_bounds(
                body,
                loop_var_name,
                captures,
                const_int_captures,
            );
            let mut facts = Vec::with_capacity(hoistable.len() * 2);
            for h in &hoistable {
                self.emit_modulo_bce_preflight(h, worker_fn);
                facts.push(AssertedIndexBound::LowerBound {
                    idx_var: h.idx_var.clone(),
                });
                facts.push(AssertedIndexBound::UpperBound {
                    idx_var: h.idx_var.clone(),
                    vec_var: h.vec_var.clone(),
                });
            }
            let n_pushed = facts.len();
            self.asserted_index_bounds.extend(facts);
            n_pushed
        } else {
            0
        };

        // Loop scaffolding: cond → body → incr → cond → ... → exit
        let cond_bb = self.context.append_basic_block(worker_fn, "loop.cond");
        let body_bb = self.context.append_basic_block(worker_fn, "loop.body");
        let exit_bb = self.context.append_basic_block(worker_fn, "loop.exit");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let k_now = self
            .builder
            .build_load(acc_int_ty, k_alloca, "k")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, k_now, end_val, "loop.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        // Per-iteration scope frame so body-local lets (e.g. `let result =
        // convert_off(...)` returning a `Vec[char]`) drop at end of each
        // iteration. Without this, every `let` inside the loop body
        // registers cleanup against the worker's top frame (pushed at the
        // start of this fn, drained once at `exit_bb`) — every iteration's
        // heap allocations pile up and only the last iteration's value
        // gets dropped. Surfaced on the kata 6 (zigzag conversion) bench
        // whose `convert_off` returns a `Vec[char]` each of 10K iterations:
        // peak RSS climbed to 498 MiB vs 1.5 MiB on the seq lane. Mirrors
        // the per-iteration frame discipline in `compile_while` /
        // `compile_loop` / `compile_for_range`.
        self.scope_cleanup_actions.push(Vec::new());
        // Compile the body in the worker fn's scope. `self.variables` now
        // binds the accumulator + loop var + captures to the worker's
        // local allocas, so the body's compile output reads/writes them
        // correctly.
        let body_result = self.compile_block(body);
        // Pop the BCE facts we pushed before the loop — they're scoped to
        // this worker's body only. Done before propagating any body error
        // so the bounds stack is balanced even on the error path.
        for _ in 0..pushed_bce_facts {
            self.asserted_index_bounds.pop();
        }
        body_result?;

        // Increment + back-edge. The body's emit may have left the
        // builder positioned in a different basic block (nested control
        // flow). If the current block already has a terminator (e.g. a
        // body-internal `break` or `return` — both rejected upstream),
        // skip the back-edge. Otherwise drain the per-iteration cleanup
        // frame before emitting `k = k + 1; br cond`.
        let current_bb = self.builder.get_insert_block().unwrap();
        if current_bb.get_terminator().is_none() {
            self.drain_top_frame_with_emit();
            let k_cur = self
                .builder
                .build_load(acc_int_ty, k_alloca, "k.cur")
                .unwrap()
                .into_int_value();
            let k_next = self
                .builder
                .build_int_add(k_cur, acc_int_ty.const_int(1, false), "k.next")
                .unwrap();
            self.builder.build_store(k_alloca, k_next).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();
        } else {
            // Body-terminator path (rejected upstream today; defensive in
            // case future shapes admit it). The terminator path already
            // walked its own cleanup via emit_scope_cleanup, so just pop
            // the per-iteration frame to balance the stack.
            self.scope_cleanup_actions.pop();
        }

        self.builder.position_at_end(exit_bb);
        // Publish the worker's partial to the caller's slot. The slot's
        // memory width matches `acc_int_ty` — set up in `emit_reduce_call`
        // via the descriptor's `slot_size` / `slot_align` fields, which the
        // runtime uses to allocate one slot per worker.
        let final_acc = self
            .builder
            .build_load(acc_int_ty, acc_alloca, "acc.final")
            .unwrap();
        let slot_ptr = worker_fn.get_nth_param(0).unwrap().into_pointer_value();
        self.builder.build_store(slot_ptr, final_acc).unwrap();
        // Drain any cleanup actions the body queued (Vec/String drops on
        // body-local lets, etc.) before returning. Mirrors emit_par_branch_fn.
        self.emit_scope_cleanup();
        self.builder.build_return(None).unwrap();

        // Restore outer state.
        self.branch_cancel_ptr = saved_cancel_ptr;
        self.scope_cleanup_actions = saved_cleanup;
        self.loop_stack = saved_loop_stack;
        self.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        Ok(worker_fn)
    }

    /// Build the env-struct + descriptor + out_slot allocas in the parent
    /// frame, populate them, and emit the call to `karac_par_reduce`.
    /// After the call, load `out_slot` and store into the source-level
    /// accumulator's alloca so subsequent reads see the reduced value.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::result_large_err)]
    fn emit_reduce_call(
        &mut self,
        init_fn: FunctionValue<'ctx>,
        worker_fn: FunctionValue<'ctx>,
        combine_fn: FunctionValue<'ctx>,
        iter_total: IntValue<'ctx>,
        acc_slot: VarSlot<'ctx>,
        acc_int_ty: IntType<'ctx>,
        reduction: &LoopReduction,
        captures: &[String],
        lo_val: Option<IntValue<'ctx>>,
        per_iter_cost_units: u64,
    ) -> Result<(), String> {
        let parent_fn = self
            .current_fn
            .expect("emit_reduce_call must run inside a function");
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // Build the env-struct in the parent frame, populate it. Layout
        // mirrors the worker fn's unpack order in `emit_reduce_worker_fn`:
        //   - If `lo_val.is_some()`: field 0 is `lo: acc_int_ty`, then
        //     captures.
        //   - Otherwise: just captures.
        // Null ctx is only safe when both lo is absent AND captures is
        // empty — the runtime passes ctx through to worker_fn unchanged.
        let env_ctx_ptr: PointerValue<'ctx> = if lo_val.is_none() && captures.is_empty() {
            ptr_ty.const_null()
        } else {
            let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(captures.len() + 1);
            if lo_val.is_some() {
                field_tys.push(acc_int_ty.into());
            }
            for n in captures {
                let ty = self.variables[n].ty;
                // B-2026-06-15-3: `[N x T]` array captures travel by pointer
                // (matches the worker's env layout).
                field_tys.push(if matches!(ty, BasicTypeEnum::ArrayType(_)) {
                    ptr_ty.into()
                } else {
                    ty
                });
            }
            let env_ty = self.context.struct_type(&field_tys, false);
            let env_alloca = self.create_entry_alloca(parent_fn, "__reduce_env", env_ty.into());
            let mut env_agg = env_ty.get_undef();
            let capture_base = if let Some(lo) = lo_val {
                env_agg = self
                    .builder
                    .build_insert_value(env_agg, lo, 0, "__reduce_env_lo")
                    .unwrap()
                    .into_struct_value();
                1
            } else {
                0
            };
            for (i, name) in captures.iter().enumerate() {
                let slot = self.variables[name];
                // B-2026-06-15-3: by-pointer for array captures (pass the
                // parent array's address); by-value otherwise.
                let val: BasicValueEnum<'ctx> = if matches!(slot.ty, BasicTypeEnum::ArrayType(_)) {
                    slot.ptr.into()
                } else {
                    self.builder.build_load(slot.ty, slot.ptr, name).unwrap()
                };
                env_agg = self
                    .builder
                    .build_insert_value(
                        env_agg,
                        val,
                        (capture_base + i) as u32,
                        "__reduce_env_field",
                    )
                    .unwrap()
                    .into_struct_value();
            }
            self.builder.build_store(env_alloca, env_agg).unwrap();
            env_alloca
        };

        // Build the descriptor struct.  Layout matches `runtime/src/lib.rs`'s
        // `#[repr(C)] KaracReduceDescriptor`: i64 iter_total + i64 slot_size +
        // i64 slot_align + ptr init + ptr worker + ptr combine + ptr ctx +
        // i64 per_iter_cost_units (slice 3b.8).
        let desc_ty = self.context.struct_type(
            &[
                i64_t.into(),  // iter_total
                i64_t.into(),  // slot_size
                i64_t.into(),  // slot_align
                ptr_ty.into(), // init_slot
                ptr_ty.into(), // worker_fn
                ptr_ty.into(), // combine_fn
                ptr_ty.into(), // ctx
                i64_t.into(),  // per_iter_cost_units
            ],
            false,
        );
        let desc_alloca = self.create_entry_alloca(parent_fn, "__reduce_desc", desc_ty.into());

        // Slot size / align track the accumulator width. Power-of-two
        // widths (i8/i16/i32/i64) have align == size on every target
        // karac compiles for; the gate in `try_emit_reduction_lowering`
        // rejects any other width before we reach here.
        let slot_byte_width: u64 = (acc_int_ty.get_bit_width() / 8) as u64;
        let slot_size = i64_t.const_int(slot_byte_width, false);
        let slot_align = i64_t.const_int(slot_byte_width, false);

        // Widen iter_total to i64 if the source's `end` evaluated to a
        // narrower int — the descriptor field is u64 (wasm32-clean; see
        // the runtime's `KaracReduceDescriptor::iter_total` field note).
        // zext (not sext): iter_total represents a non-negative count, so
        // zero-extension is correct for both signed source types (whose
        // positive values fit unchanged) and unsigned source types (whose
        // high-bit-set values would sext to a wrong negative i64).
        let iter_total_widened = if iter_total.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_z_extend(iter_total, i64_t, "iter.widen")
                .unwrap()
        } else {
            iter_total
        };

        // Populate via insertvalue + a single store. Order matches the
        // Rust struct layout; clippy would complain about a fluent
        // insert_value chain so we bind step-by-step.
        let mut desc_agg = desc_ty.get_undef();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, iter_total_widened, 0, "d.iter_total")
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, slot_size, 1, "d.slot_size")
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, slot_align, 2, "d.slot_align")
            .unwrap()
            .into_struct_value();
        let init_ptr = init_fn.as_global_value().as_pointer_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, init_ptr, 3, "d.init_slot")
            .unwrap()
            .into_struct_value();
        let worker_ptr = worker_fn.as_global_value().as_pointer_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, worker_ptr, 4, "d.worker_fn")
            .unwrap()
            .into_struct_value();
        let combine_ptr = combine_fn.as_global_value().as_pointer_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, combine_ptr, 5, "d.combine_fn")
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, env_ctx_ptr, 6, "d.ctx")
            .unwrap()
            .into_struct_value();
        // Slice 3b.8: per-iter cost estimate, in "1 unit ≈ 1 ns" — the
        // runtime uses iter_total × per_iter_cost to decide whether to
        // dispatch to the pool or fall back to single-worker on the
        // caller's thread. `0` is the sentinel "no estimate, always
        // dispatch"; codegen always emits a real estimate (the body-cost
        // walker bottoms at 1).
        let per_iter_const = i64_t.const_int(per_iter_cost_units, false);
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, per_iter_const, 7, "d.per_iter_cost")
            .unwrap()
            .into_struct_value();
        self.builder.build_store(desc_alloca, desc_agg).unwrap();

        // Allocate the out_slot in the parent frame. The runtime writes
        // the reduced value here before returning; the parent then loads
        // it back into the source-level accumulator's alloca. The slot
        // width matches `acc_int_ty` so the load below picks up the full
        // reduced value with no widening.
        let out_slot = self.create_entry_alloca(parent_fn, "__reduce_out", acc_int_ty.into());

        // Spawn site id — slice 3b reuses par_counter (the same monotonic
        // counter par-blocks use). The runtime currently ignores this
        // arg for reductions (no frame-tracking surface in the reduce
        // path yet), but the FFI takes it so we feed a unique value.
        let spawn_site_id = self
            .context
            .i32_type()
            .const_int(self.par_counter as u64, false);
        self.par_counter += 1;

        self.builder
            .build_call(
                self.karac_par_reduce_fn,
                &[desc_alloca.into(), out_slot.into(), spawn_site_id.into()],
                "",
            )
            .unwrap();

        // Load the reduced value, fold it with the parent's pre-existing
        // accumulator value via the op's combine, then store back. The
        // fold is the load-bearing step for Min/Max correctness: kata-153
        // shapes the loop as `let mut m = nums[0]; for i in 1..n {
        // if nums[i] < m { m = nums[i]; }}` — m starts at the first
        // element, not at i64::MAX, so without folding the parent's
        // initial value the parallel version would drop nums[0] from
        // consideration. The fold also generalizes Add correctly when
        // the user writes `let mut sum = 100; for k... sum += k`
        // (initial value != identity) — without the fold, the 100 was
        // silently dropped in the prior codegen.
        let reduced = self
            .builder
            .build_load(acc_int_ty, out_slot, "reduced")
            .unwrap()
            .into_int_value();
        let parent_initial = self
            .builder
            .build_load(acc_int_ty, acc_slot.ptr, "acc.initial")
            .unwrap()
            .into_int_value();
        let final_value = self.emit_reduce_combine_inst(reduction.op, parent_initial, reduced);
        self.builder.build_store(acc_slot.ptr, final_value).unwrap();

        Ok(())
    }

    // ── Collect-style reduction lowering (Phase 3, 2026-05-21) ─────────
    //
    // `#[par_unordered] for k in 0..K { ... acc.push(EXPR); ... }` —
    // accumulator is a `Vec[T]`, slot is the 24-byte `{ptr, len, cap}`
    // struct, init writes an empty Vec, combine extends src into dst
    // (heap concat + src-buffer takeover). The recognizer at
    // `src/concurrency.rs::collect_push_shape` gates this on the explicit
    // `#[par_unordered]` attribute since the worker-combine order is not
    // input-order-preserving; the attribute is the user's "I tolerate
    // any ordering" opt-in. v1 supports int element types (`Vec[i8]` …
    // `Vec[i64]`); String / nested-Vec / struct element types fall back
    // to sequential codegen until a workload surfaces them.

    /// Lower a Collect-tagged loop. Mirrors `try_emit_reduction_lowering`'s
    /// shape-extract path but uses the Vec-struct (24-byte slot) ABI and
    /// dispatches to the Collect-specific worker / init / combine helpers.
    /// Returns `Ok(None)` for any shape outside the Phase-3 supported set
    /// (non-int element types, non-Vec accumulator type, early exits in
    /// the body); falls back to sequential codegen for those.
    #[allow(clippy::result_large_err)]
    fn try_emit_collect_reduction_lowering(
        &mut self,
        parent_body: &Block,
        stmt_index: usize,
        reduction: &LoopReduction,
    ) -> Result<Option<()>, String> {
        let stmt = &parent_body.stmts[stmt_index];
        let StmtKind::Expr(expr) = &stmt.kind else {
            return Ok(None);
        };
        let Some(shape) = self.extract_loop_shape(parent_body, stmt_index, expr) else {
            return Ok(None);
        };

        // Accumulator must be a Vec[T] — the `{ptr, len, cap}` struct
        // shape — with an integer element type for Phase 3. The
        // recognizer (`collect_push_shape`) checks the source-level shape
        // `acc.push(EXPR)`; here we verify the LLVM-side type matches.
        let Some(acc_slot) = self.variables.get(&reduction.accumulator).copied() else {
            return Ok(None);
        };
        if !self.llvm_ty_is_vec_struct(acc_slot.ty) {
            return Ok(None);
        }
        let elem_ty = self.vec_elem_type_for_var(&reduction.accumulator);
        let BasicTypeEnum::IntType(elem_int_ty) = elem_ty else {
            return Ok(None);
        };
        if !matches!(elem_int_ty.get_bit_width(), 8 | 16 | 32 | 64) {
            return Ok(None);
        }

        // Early exits would cross the worker-fn boundary the same way
        // they do for scalar reductions. Same rejection.
        if block_has_early_exit(&shape.body) {
            return Ok(None);
        }

        // Cost gates: `#[par_unordered]` is an explicit opt-in, so we
        // skip the memory-bound gate and emit a zero per-iter cost
        // sentinel (runtime treats 0 as "always dispatch"; see
        // `runtime/src/lib.rs` DISPATCH_OVERHEAD_PER_CALL_UNITS_RT).
        // The user accepted parallel dispatch when they wrote the
        // attribute; second-guessing here would surprise them.
        let per_iter_cost: u64 = 0;

        // Compile `end` and `lo` once. The loop variable's int type is
        // taken from `end_val`; the recognizer's range-unification has
        // already ensured the loop body's `k` reads see this type.
        let end_val = self.compile_expr(&shape.end_expr)?.into_int_value();
        let loop_var_int_ty = end_val.get_type();
        let (iter_total_val, lo_val) = match &shape.lo_expr {
            None => (end_val, None),
            Some(lo_expr) => {
                let lo_val = self.compile_expr(lo_expr)?.into_int_value();
                if lo_val.get_type() != loop_var_int_ty {
                    return Ok(None);
                }
                let iter_total = self
                    .builder
                    .build_int_sub(end_val, lo_val, "iter.total")
                    .unwrap();
                (iter_total, Some(lo_val))
            }
        };

        // Synthesize the per-elem-type helpers. Cached by element-type
        // bit width so multiple reduction sites in the same module share
        // one definition.
        let init_fn = self.emit_reduce_collect_init_fn(elem_int_ty);
        let combine_fn = self.emit_reduce_collect_combine_fn(elem_int_ty);

        // Capture set + partition (same machinery as scalar — Collect's
        // body can read any outer-scope binding the body refs).
        let captures =
            self.collect_reduction_captures(&shape.body, &reduction.accumulator, &shape.loop_var);
        let (runtime_captures, const_int_captures) =
            self.partition_const_int_captures(&captures, parent_body, stmt_index);

        let worker_fn = self.emit_reduce_collect_worker_fn(
            reduction,
            loop_var_int_ty,
            elem_int_ty,
            &shape.loop_var,
            &shape.body,
            &runtime_captures,
            &const_int_captures,
            lo_val.is_some(),
        )?;

        self.emit_reduce_collect_call(
            init_fn,
            worker_fn,
            combine_fn,
            iter_total_val,
            acc_slot,
            loop_var_int_ty,
            reduction,
            &runtime_captures,
            lo_val,
            per_iter_cost,
        )?;

        Ok(Some(()))
    }

    /// Synthesize `void init_slot_collect_i<N>(*mut u8 slot)` — writes the
    /// 24-byte `{null, 0, 0}` empty-Vec literal into the slot. Mirrors
    /// `emit_reduce_init_fn`'s caching pattern but per-element-type.
    fn emit_reduce_collect_init_fn(&mut self, elem_int_ty: IntType<'ctx>) -> FunctionValue<'ctx> {
        let name = format!(
            "__karac_reduce_init_collect_i{}",
            elem_int_ty.get_bit_width()
        );
        if let Some(existing) = self.module.get_function(&name) {
            return existing;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_ty = self
            .context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::from(ptr_ty)], false);
        let f = self.module.add_function(&name, fn_ty, None);

        let saved_bb = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let slot_ptr = f.get_nth_param(0).unwrap().into_pointer_value();
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let null_ptr = ptr_ty.const_null();
        let zero = i64_t.const_zero();
        let mut empty = vec_ty.get_undef();
        empty = self
            .builder
            .build_insert_value(empty, null_ptr, 0, "v.data")
            .unwrap()
            .into_struct_value();
        empty = self
            .builder
            .build_insert_value(empty, zero, 1, "v.len")
            .unwrap()
            .into_struct_value();
        empty = self
            .builder
            .build_insert_value(empty, zero, 2, "v.cap")
            .unwrap()
            .into_struct_value();
        self.builder.build_store(slot_ptr, empty).unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Synthesize `void combine_collect_i<N>(*mut u8 dst, *const u8 src)`
    /// — extends `src` into `dst`, transferring src's elements to dst's
    /// final buffer and zeroing src so its slot's subsequent cleanup is a
    /// no-op. Four-path strategy (Phase 3.1, 2026-05-21): the runtime calls
    /// combine_fn N times (once per worker) into the same dst, so the
    /// naive "fresh malloc + 2× memcpy per call" shape would do O(N²)
    /// memcpy traffic across the chain. This implementation reuses dst's
    /// existing buffer when it fits, grows with amortized doubling when
    /// it doesn't, and adopts src's buffer wholesale on the first combine
    /// (when dst is still empty) — total memcpy traffic across the N
    /// combines drops to O(total_elements), and the first combine pays
    /// zero memcpy cost at all.
    ///
    /// 1. `new_len = dst.len + src.len`. If 0 → early return (both empty).
    /// 2. `dst.cap == 0` (first combine into an empty dst) → adopt src's
    ///    `{data, cap}` wholesale; no alloc, no memcpy. dst now owns
    ///    src's buffer. src zeroed.
    /// 3. `dst.cap >= new_len` (dst already has room) → memcpy src into
    ///    dst's tail, free src.data, update dst.len. dst.data and dst.cap
    ///    untouched.
    /// 4. Otherwise (need to grow) → new_cap = max(new_len, dst.cap × 2)
    ///    (amortized-doubling growth, like Vec.push's growth strategy);
    ///    malloc one new buffer, memcpy both sides into it, free old
    ///    dst.data and src.data.
    fn emit_reduce_collect_combine_fn(
        &mut self,
        elem_int_ty: IntType<'ctx>,
    ) -> FunctionValue<'ctx> {
        let name = format!(
            "__karac_reduce_combine_collect_i{}",
            elem_int_ty.get_bit_width()
        );
        if let Some(existing) = self.module.get_function(&name) {
            return existing;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_ty),
                BasicMetadataTypeEnum::from(ptr_ty),
            ],
            false,
        );
        let f = self.module.add_function(&name, fn_ty, None);

        let saved_bb = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(f, "entry");
        let check_cap_bb = self.context.append_basic_block(f, "check.dst.cap");
        let adopt_bb = self.context.append_basic_block(f, "adopt.src");
        let check_room_bb = self.context.append_basic_block(f, "check.has.room");
        let append_bb = self.context.append_basic_block(f, "append.in.place");
        let grow_bb = self.context.append_basic_block(f, "grow.and.copy");
        let exit_bb = self.context.append_basic_block(f, "exit");
        self.builder.position_at_end(entry);

        let vec_ty = self.vec_struct_type();
        let dst_ptr = f.get_nth_param(0).unwrap().into_pointer_value();
        let src_ptr = f.get_nth_param(1).unwrap().into_pointer_value();
        let elem_size = elem_int_ty.size_of();

        // Load all six fields up front. LLVM's mem2reg + DSE collapse any
        // load that ends up unused on a given path.
        let dst_data_p = self
            .builder
            .build_struct_gep(vec_ty, dst_ptr, 0, "dst.data.ptr")
            .unwrap();
        let dst_len_p = self
            .builder
            .build_struct_gep(vec_ty, dst_ptr, 1, "dst.len.ptr")
            .unwrap();
        let dst_cap_p = self
            .builder
            .build_struct_gep(vec_ty, dst_ptr, 2, "dst.cap.ptr")
            .unwrap();
        let src_data_p = self
            .builder
            .build_struct_gep(vec_ty, src_ptr, 0, "src.data.ptr")
            .unwrap();
        let src_len_p = self
            .builder
            .build_struct_gep(vec_ty, src_ptr, 1, "src.len.ptr")
            .unwrap();
        let src_cap_p = self
            .builder
            .build_struct_gep(vec_ty, src_ptr, 2, "src.cap.ptr")
            .unwrap();

        let dst_data = self
            .builder
            .build_load(ptr_ty, dst_data_p, "dst.data")
            .unwrap()
            .into_pointer_value();
        let dst_len = self
            .builder
            .build_load(i64_t, dst_len_p, "dst.len")
            .unwrap()
            .into_int_value();
        let dst_cap = self
            .builder
            .build_load(i64_t, dst_cap_p, "dst.cap")
            .unwrap()
            .into_int_value();
        let src_data = self
            .builder
            .build_load(ptr_ty, src_data_p, "src.data")
            .unwrap()
            .into_pointer_value();
        let src_len = self
            .builder
            .build_load(i64_t, src_len_p, "src.len")
            .unwrap()
            .into_int_value();
        let src_cap = self
            .builder
            .build_load(i64_t, src_cap_p, "src.cap")
            .unwrap()
            .into_int_value();

        let zero = i64_t.const_zero();
        let null_ptr = ptr_ty.const_null();
        let new_len = self
            .builder
            .build_int_add(dst_len, src_len, "new.len")
            .unwrap();
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, new_len, zero, "new_len.zero")
            .unwrap();
        self.builder
            .build_conditional_branch(is_zero, exit_bb, check_cap_bb)
            .unwrap();

        // ── check_cap_bb: is dst still empty? ──
        // First-combine fast path: when dst.cap == 0 the init_slot left a
        // `{null, 0, 0}` placeholder there. Skip the alloc + memcpy and
        // just adopt src's `{data, cap}` wholesale.
        self.builder.position_at_end(check_cap_bb);
        let dst_empty = self
            .builder
            .build_int_compare(IntPredicate::EQ, dst_cap, zero, "dst.cap.zero")
            .unwrap();
        self.builder
            .build_conditional_branch(dst_empty, adopt_bb, check_room_bb)
            .unwrap();

        // ── adopt_bb: dst takes ownership of src's buffer. ──
        self.builder.position_at_end(adopt_bb);
        self.builder.build_store(dst_data_p, src_data).unwrap();
        self.builder.build_store(dst_len_p, src_len).unwrap();
        self.builder.build_store(dst_cap_p, src_cap).unwrap();
        self.builder.build_store(src_data_p, null_ptr).unwrap();
        self.builder.build_store(src_len_p, zero).unwrap();
        self.builder.build_store(src_cap_p, zero).unwrap();
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        // ── check_room_bb: does dst already have room for src? ──
        self.builder.position_at_end(check_room_bb);
        let has_room = self
            .builder
            .build_int_compare(IntPredicate::UGE, dst_cap, new_len, "dst.has.room")
            .unwrap();
        self.builder
            .build_conditional_branch(has_room, append_bb, grow_bb)
            .unwrap();

        // ── append_bb: memcpy src into dst's tail. ──
        self.builder.position_at_end(append_bb);
        let dst_tail = unsafe {
            self.builder
                .build_gep(elem_int_ty, dst_data, &[dst_len], "dst.tail")
                .unwrap()
        };
        let src_bytes_append = self
            .builder
            .build_int_mul(src_len, elem_size, "src.bytes.append")
            .unwrap();
        self.builder
            .build_memcpy(dst_tail, 8, src_data, 8, src_bytes_append)
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[src_data.into()], "")
            .unwrap();
        self.builder.build_store(dst_len_p, new_len).unwrap();
        self.builder.build_store(src_data_p, null_ptr).unwrap();
        self.builder.build_store(src_len_p, zero).unwrap();
        self.builder.build_store(src_cap_p, zero).unwrap();
        // dst.data and dst.cap are unchanged on this path; no stores.
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        // ── grow_bb: allocate a fresh buffer at max(new_len, 2*dst.cap), ──
        // copy dst's then src's elements, free old buffers. The doubling
        // gives amortized O(1) growth — across N combine calls, total
        // memcpy traffic stays O(total_elements) instead of O(N×total).
        self.builder.position_at_end(grow_bb);
        let double_cap = self
            .builder
            .build_int_mul(dst_cap, i64_t.const_int(2, false), "double.cap")
            .unwrap();
        let use_new_len = self
            .builder
            .build_int_compare(IntPredicate::UGT, new_len, double_cap, "use.new_len")
            .unwrap();
        let new_cap = self
            .builder
            .build_select(use_new_len, new_len, double_cap, "new.cap")
            .unwrap()
            .into_int_value();
        let new_bytes = self
            .builder
            .build_int_mul(new_cap, elem_size, "new.bytes")
            .unwrap();
        let new_data = self
            .builder
            .build_call(self.malloc_fn, &[new_bytes.into()], "new.data")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let dst_bytes = self
            .builder
            .build_int_mul(dst_len, elem_size, "dst.bytes")
            .unwrap();
        self.builder
            .build_memcpy(new_data, 8, dst_data, 8, dst_bytes)
            .unwrap();
        let new_tail = unsafe {
            self.builder
                .build_gep(elem_int_ty, new_data, &[dst_len], "new.tail")
                .unwrap()
        };
        let src_bytes_grow = self
            .builder
            .build_int_mul(src_len, elem_size, "src.bytes.grow")
            .unwrap();
        self.builder
            .build_memcpy(new_tail, 8, src_data, 8, src_bytes_grow)
            .unwrap();
        // free(null) is a no-op per C spec — dst.data is null here only
        // when dst.cap > 0 was impossible (i.e. unreachable for this
        // path, since adopt_bb caught dst.cap == 0).
        self.builder
            .build_call(self.free_fn, &[dst_data.into()], "")
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[src_data.into()], "")
            .unwrap();
        self.builder.build_store(dst_data_p, new_data).unwrap();
        self.builder.build_store(dst_len_p, new_len).unwrap();
        self.builder.build_store(dst_cap_p, new_cap).unwrap();
        self.builder.build_store(src_data_p, null_ptr).unwrap();
        self.builder.build_store(src_len_p, zero).unwrap();
        self.builder.build_store(src_cap_p, zero).unwrap();
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Synthesize the per-call Collect worker fn. Mirrors
    /// `emit_reduce_worker_fn`'s scaffolding but the accumulator is a
    /// local `Vec[T]` alloca (initialized to `{null, 0, 0}`), and the
    /// final publish copies the Vec struct into the slot rather than
    /// storing an integer. The worker does NOT register the local Vec
    /// for cleanup — its buffer ownership transfers to the slot at
    /// function exit; the next combine_fn call takes responsibility for
    /// freeing it. Body-local lets register their own cleanup as usual.
    #[allow(clippy::result_large_err)]
    #[allow(clippy::too_many_arguments)]
    fn emit_reduce_collect_worker_fn(
        &mut self,
        reduction: &LoopReduction,
        loop_var_int_ty: IntType<'ctx>,
        elem_int_ty: IntType<'ctx>,
        loop_var_name: &str,
        body: &Block,
        captures: &[String],
        const_int_captures: &[(String, i64, Option<IntSuffix>)],
        has_lo: bool,
    ) -> Result<FunctionValue<'ctx>, String> {
        let worker_id = self.par_counter;
        self.par_counter += 1;
        let name = format!("__karac_reduce_worker_collect_{worker_id}");

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let fn_ty = self.context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_ty), // slot
                BasicMetadataTypeEnum::from(i64_t),  // start
                BasicMetadataTypeEnum::from(i64_t),  // end
                BasicMetadataTypeEnum::from(ptr_ty), // ctx
                BasicMetadataTypeEnum::from(ptr_ty), // cancel
            ],
            false,
        );
        let worker_fn = self.module.add_function(&name, fn_ty, None);

        // Save outer state. Vec captures need vec_elem_types preserved
        // for body-side `cap.len()` / `cap[idx]` etc. to dispatch through
        // `compile_vec_method`; same for `var_type_names` for type-aware
        // lookups. We re-insert the relevant outer entries after taking.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_cleanup = std::mem::take(&mut self.scope_cleanup_actions);
        let saved_cancel_ptr = self.branch_cancel_ptr.take();
        let saved_elem_types = std::mem::take(&mut self.vec_elem_types);
        self.scope_cleanup_actions.push(Vec::new());

        self.current_fn = Some(worker_fn);
        let entry = self.context.append_basic_block(worker_fn, "entry");
        self.builder.position_at_end(entry);

        // Env-struct unpack (mirror of scalar): lo + captures by value.
        let env_struct_ty: Option<StructType<'ctx>> = if !has_lo && captures.is_empty() {
            None
        } else {
            let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(captures.len() + 1);
            if has_lo {
                field_tys.push(loop_var_int_ty.into());
            }
            let env_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            for n in captures {
                let ty = saved_vars[n].ty;
                // B-2026-06-15-3: array captures by pointer (see extract loop).
                field_tys.push(if matches!(ty, BasicTypeEnum::ArrayType(_)) {
                    env_ptr_ty.into()
                } else {
                    ty
                });
            }
            Some(self.context.struct_type(&field_tys, false))
        };
        let mut lo_in_worker: Option<IntValue<'ctx>> = None;
        if let Some(env_ty) = env_struct_ty {
            let ctx_ptr = worker_fn.get_nth_param(3).unwrap().into_pointer_value();
            let env_val = self
                .builder
                .build_load::<BasicTypeEnum<'ctx>>(env_ty.into(), ctx_ptr, "__reduce_env_load")
                .unwrap()
                .into_struct_value();
            let capture_field_base = if has_lo {
                let lo_field = self
                    .builder
                    .build_extract_value(env_val, 0, "__reduce_lo")
                    .unwrap()
                    .into_int_value();
                lo_in_worker = Some(lo_field);
                1
            } else {
                0
            };
            for (i, var_name) in captures.iter().enumerate() {
                let cap_ty = saved_vars[var_name].ty;
                let field_idx = (capture_field_base + i) as u32;
                let field_val = self
                    .builder
                    .build_extract_value(env_val, field_idx, var_name)
                    .unwrap();
                if matches!(cap_ty, BasicTypeEnum::ArrayType(_)) {
                    // B-2026-06-15-3: by-pointer array capture — env field IS
                    // the parent array's pointer; register it directly (no
                    // by-value copy that LLVM O2 scalarizes into N stores).
                    self.variables.insert(
                        var_name.clone(),
                        VarSlot {
                            ptr: field_val.into_pointer_value(),
                            ty: cap_ty,
                        },
                    );
                } else {
                    let alloca = self.create_entry_alloca(worker_fn, var_name, cap_ty);
                    self.builder.build_store(alloca, field_val).unwrap();
                    self.variables.insert(
                        var_name.clone(),
                        VarSlot {
                            ptr: alloca,
                            ty: cap_ty,
                        },
                    );
                }
                if let Some(type_name) = saved_var_types.get(var_name) {
                    self.var_type_names
                        .insert(var_name.clone(), type_name.clone());
                }
                if let Some(et) = saved_elem_types.get(var_name) {
                    self.vec_elem_types.insert(var_name.clone(), *et);
                }
            }
        }

        // Const-int captures.
        for (var_name, value, sfx) in const_int_captures {
            let cap_ty = saved_vars[var_name].ty;
            let const_val = self.const_int_for_suffix(*value, *sfx);
            let alloca = self.create_entry_alloca(worker_fn, var_name, cap_ty);
            self.builder.build_store(alloca, const_val).unwrap();
            self.variables.insert(
                var_name.clone(),
                VarSlot {
                    ptr: alloca,
                    ty: cap_ty,
                },
            );
            if let Some(type_name) = saved_var_types.get(var_name) {
                self.var_type_names
                    .insert(var_name.clone(), type_name.clone());
            }
        }

        // Allocate the local Vec accumulator, init to `{null, 0, 0}`,
        // register under the source-level acc name so the body's
        // `acc.push(x)` dispatches into `compile_vec_method` against
        // this alloca. NOT registered for cleanup — the slot inherits
        // ownership at function exit.
        let vec_ty = self.vec_struct_type();
        let acc_alloca = self.create_entry_alloca(worker_fn, &reduction.accumulator, vec_ty.into());
        let null_ptr = ptr_ty.const_null();
        let zero = i64_t.const_zero();
        let mut empty = vec_ty.get_undef();
        empty = self
            .builder
            .build_insert_value(empty, null_ptr, 0, "acc.data")
            .unwrap()
            .into_struct_value();
        empty = self
            .builder
            .build_insert_value(empty, zero, 1, "acc.len")
            .unwrap()
            .into_struct_value();
        empty = self
            .builder
            .build_insert_value(empty, zero, 2, "acc.cap")
            .unwrap()
            .into_struct_value();
        self.builder.build_store(acc_alloca, empty).unwrap();
        self.variables.insert(
            reduction.accumulator.clone(),
            VarSlot {
                ptr: acc_alloca,
                ty: vec_ty.into(),
            },
        );
        self.vec_elem_types
            .insert(reduction.accumulator.clone(), elem_int_ty.into());

        // Loop var (mirror of scalar). Truncate runtime's i64 start/end
        // to the loop-var int width when narrower; shift by lo if set.
        let raw_start = worker_fn.get_nth_param(1).unwrap().into_int_value();
        let raw_end = worker_fn.get_nth_param(2).unwrap().into_int_value();
        let (start_val, end_val) = if loop_var_int_ty.get_bit_width() < 64 {
            let s = self
                .builder
                .build_int_truncate(raw_start, loop_var_int_ty, "start.trunc")
                .unwrap();
            let e = self
                .builder
                .build_int_truncate(raw_end, loop_var_int_ty, "end.trunc")
                .unwrap();
            (s, e)
        } else {
            (raw_start, raw_end)
        };
        let (start_val, end_val) = match lo_in_worker {
            Some(lo) => {
                let s = self
                    .builder
                    .build_int_add(start_val, lo, "start.shift")
                    .unwrap();
                let e = self
                    .builder
                    .build_int_add(end_val, lo, "end.shift")
                    .unwrap();
                (s, e)
            }
            None => (start_val, end_val),
        };
        let k_alloca = self.create_entry_alloca(worker_fn, loop_var_name, loop_var_int_ty.into());
        self.builder.build_store(k_alloca, start_val).unwrap();
        self.variables.insert(
            loop_var_name.to_string(),
            VarSlot {
                ptr: k_alloca,
                ty: loop_var_int_ty.into(),
            },
        );

        // Loop scaffolding (no BCE preflight for Collect — body is
        // push-heavy, not index-heavy).
        let cond_bb = self.context.append_basic_block(worker_fn, "loop.cond");
        let body_bb = self.context.append_basic_block(worker_fn, "loop.body");
        let exit_bb = self.context.append_basic_block(worker_fn, "loop.exit");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let k_now = self
            .builder
            .build_load(loop_var_int_ty, k_alloca, "k")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, k_now, end_val, "loop.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let body_result = self.compile_block(body);
        body_result?;

        let current_bb = self.builder.get_insert_block().unwrap();
        if current_bb.get_terminator().is_none() {
            let k_cur = self
                .builder
                .build_load(loop_var_int_ty, k_alloca, "k.cur")
                .unwrap()
                .into_int_value();
            let k_next = self
                .builder
                .build_int_add(k_cur, loop_var_int_ty.const_int(1, false), "k.next")
                .unwrap();
            self.builder.build_store(k_alloca, k_next).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();
        }

        self.builder.position_at_end(exit_bb);
        // Publish: load the local Vec struct and store into the slot.
        // The slot now owns the heap buffer. Body-local lets get their
        // cleanup via emit_scope_cleanup (acc was never registered).
        let final_vec = self
            .builder
            .build_load(vec_ty, acc_alloca, "acc.final")
            .unwrap();
        let slot_ptr = worker_fn.get_nth_param(0).unwrap().into_pointer_value();
        self.builder.build_store(slot_ptr, final_vec).unwrap();
        self.emit_scope_cleanup();
        self.builder.build_return(None).unwrap();

        // Restore outer state.
        self.vec_elem_types = saved_elem_types;
        self.branch_cancel_ptr = saved_cancel_ptr;
        self.scope_cleanup_actions = saved_cleanup;
        self.loop_stack = saved_loop_stack;
        self.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        Ok(worker_fn)
    }

    /// Build the descriptor + out_slot in the parent frame, emit the
    /// `karac_par_reduce` call, then fold the runtime-folded `out_slot`
    /// Vec into the parent's existing accumulator via `combine_fn`.
    /// Mirrors `emit_reduce_call` but with `slot_size = 24` (the Vec
    /// struct width), `slot_align = 8`, and the Vec-aware post-call
    /// fold.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::result_large_err)]
    fn emit_reduce_collect_call(
        &mut self,
        init_fn: FunctionValue<'ctx>,
        worker_fn: FunctionValue<'ctx>,
        combine_fn: FunctionValue<'ctx>,
        iter_total: IntValue<'ctx>,
        acc_slot: VarSlot<'ctx>,
        loop_var_int_ty: IntType<'ctx>,
        _reduction: &LoopReduction,
        captures: &[String],
        lo_val: Option<IntValue<'ctx>>,
        per_iter_cost_units: u64,
    ) -> Result<(), String> {
        let parent_fn = self
            .current_fn
            .expect("emit_reduce_collect_call must run inside a function");
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // Env struct: lo (if present) + captures by value.
        let env_ctx_ptr: PointerValue<'ctx> = if lo_val.is_none() && captures.is_empty() {
            ptr_ty.const_null()
        } else {
            let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(captures.len() + 1);
            if lo_val.is_some() {
                field_tys.push(loop_var_int_ty.into());
            }
            for n in captures {
                let ty = self.variables[n].ty;
                // B-2026-06-15-3: array captures by pointer (matches worker).
                field_tys.push(if matches!(ty, BasicTypeEnum::ArrayType(_)) {
                    ptr_ty.into()
                } else {
                    ty
                });
            }
            let env_ty = self.context.struct_type(&field_tys, false);
            let env_alloca = self.create_entry_alloca(parent_fn, "__reduce_env", env_ty.into());
            let mut env_agg = env_ty.get_undef();
            let capture_base = if let Some(lo) = lo_val {
                env_agg = self
                    .builder
                    .build_insert_value(env_agg, lo, 0, "__reduce_env_lo")
                    .unwrap()
                    .into_struct_value();
                1
            } else {
                0
            };
            for (i, name) in captures.iter().enumerate() {
                let slot = self.variables[name];
                // B-2026-06-15-3: by-pointer for array captures (pass the
                // parent array's address); by-value otherwise.
                let val: BasicValueEnum<'ctx> = if matches!(slot.ty, BasicTypeEnum::ArrayType(_)) {
                    slot.ptr.into()
                } else {
                    self.builder.build_load(slot.ty, slot.ptr, name).unwrap()
                };
                env_agg = self
                    .builder
                    .build_insert_value(
                        env_agg,
                        val,
                        (capture_base + i) as u32,
                        "__reduce_env_field",
                    )
                    .unwrap()
                    .into_struct_value();
            }
            self.builder.build_store(env_alloca, env_agg).unwrap();
            env_alloca
        };

        // Descriptor — same shape as scalar but slot_size = 24, slot_align = 8.
        let desc_ty = self.context.struct_type(
            &[
                i64_t.into(),  // iter_total
                i64_t.into(),  // slot_size
                i64_t.into(),  // slot_align
                ptr_ty.into(), // init_slot
                ptr_ty.into(), // worker_fn
                ptr_ty.into(), // combine_fn
                ptr_ty.into(), // ctx
                i64_t.into(),  // per_iter_cost_units
            ],
            false,
        );
        let desc_alloca = self.create_entry_alloca(parent_fn, "__reduce_desc", desc_ty.into());

        let slot_size = i64_t.const_int(24, false);
        let slot_align = i64_t.const_int(8, false);

        let iter_total_widened = if iter_total.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_z_extend(iter_total, i64_t, "iter.widen")
                .unwrap()
        } else {
            iter_total
        };

        let mut desc_agg = desc_ty.get_undef();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, iter_total_widened, 0, "d.iter_total")
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, slot_size, 1, "d.slot_size")
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, slot_align, 2, "d.slot_align")
            .unwrap()
            .into_struct_value();
        let init_ptr = init_fn.as_global_value().as_pointer_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, init_ptr, 3, "d.init_slot")
            .unwrap()
            .into_struct_value();
        let worker_ptr = worker_fn.as_global_value().as_pointer_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, worker_ptr, 4, "d.worker_fn")
            .unwrap()
            .into_struct_value();
        let combine_ptr = combine_fn.as_global_value().as_pointer_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, combine_ptr, 5, "d.combine_fn")
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, env_ctx_ptr, 6, "d.ctx")
            .unwrap()
            .into_struct_value();
        let per_iter_const = i64_t.const_int(per_iter_cost_units, false);
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, per_iter_const, 7, "d.per_iter_cost")
            .unwrap()
            .into_struct_value();
        self.builder.build_store(desc_alloca, desc_agg).unwrap();

        // out_slot: a 24-byte Vec struct alloca in the parent frame.
        let vec_ty = self.vec_struct_type();
        let out_slot = self.create_entry_alloca(parent_fn, "__reduce_out", vec_ty.into());

        let spawn_site_id = self
            .context
            .i32_type()
            .const_int(self.par_counter as u64, false);
        self.par_counter += 1;

        self.builder
            .build_call(
                self.karac_par_reduce_fn,
                &[desc_alloca.into(), out_slot.into(), spawn_site_id.into()],
                "",
            )
            .unwrap();

        // Post-call fold: extend out_slot's Vec into the parent's
        // existing accumulator. `combine_fn` takes (dst, src) and
        // transfers src's elements into dst, freeing both old buffers
        // and zeroing src. The parent's pre-existing items (e.g. a
        // `let mut results = Vec.new(); results.push(-1);` before the
        // loop) appear first in the final dst; runtime-folded
        // contributions follow.
        self.builder
            .build_call(combine_fn, &[acc_slot.ptr.into(), out_slot.into()], "")
            .unwrap();

        Ok(())
    }
}

// ── (op, type) helper naming + identities ─────────────────────────────
//
// The init/combine fn pair for a given reduction is uniquely determined
// by `(op, int_ty)`. Helper names follow `__karac_reduce_<role>_<op>_<ty>`
// so multiple reduction sites that share an (op, type) share one
// definition (cached via the LLVM module's symbol table) and the IR is
// readable for the test suite (which greps for these names).

/// Short-name slug for an op, used in helper fn names. Mirrors the
/// op-method suffix used in `concurrency.rs::reduction_binary_shape`
/// (`add` / `mul` / `bitor` / `bitand` / `bitxor`) so the IR symbol
/// matches the analyzer's vocabulary.
fn reduce_op_short_name(op: ReductionOp) -> &'static str {
    match op {
        ReductionOp::Add => "add",
        ReductionOp::Mul => "mul",
        ReductionOp::BitOr => "bitor",
        ReductionOp::BitAnd => "bitand",
        ReductionOp::BitXor => "bitxor",
        ReductionOp::Min => "min",
        ReductionOp::Max => "max",
        // Collect is short-circuited in `try_emit_reduction_lowering`
        // before reaching the helper-naming path; this arm is here for
        // exhaustiveness so an accidental future caller fails-loud.
        ReductionOp::Collect => "collect",
    }
}

/// Build the helper-fn name for a `(role, op, int_ty)` triple. `role`
/// is `"init"` or `"combine"`. Types render as `i<bit_width>` —
/// LLVM doesn't distinguish signed from unsigned at the IR layer, so
/// `i32` covers both `i32` and `u32` source types.
fn reduce_helper_name(role: &str, op: ReductionOp, int_ty: IntType<'_>) -> String {
    format!(
        "__karac_reduce_{role}_{}_i{}",
        reduce_op_short_name(op),
        int_ty.get_bit_width()
    )
}

/// Identity element for `op` on `int_ty`. The per-worker accumulator is
/// initialized to this value; the slot's init fn writes the same value.
/// LLVM uses two's-complement for all int types, so `const_all_ones` for
/// `BitAnd` is correct for both signed (-1) and unsigned (`TYPE_MAX`)
/// source-level types.
///
/// Min / Max identities are signed-T::MAX and signed-T::MIN respectively
/// — the analyzer's call-form and conditional-assign recognition (slice:
/// Min/Max combined, 2026-05-20) fires only against signed source types
/// today, so the identity values match the source-level convention. An
/// unsigned variant requires threading a signedness bit through
/// `ReductionOp` and is deferred until a workload surfaces it.
fn reduce_identity<'ctx>(op: ReductionOp, int_ty: IntType<'ctx>) -> IntValue<'ctx> {
    match op {
        ReductionOp::Add | ReductionOp::BitOr | ReductionOp::BitXor => int_ty.const_zero(),
        ReductionOp::Mul => int_ty.const_int(1, false),
        ReductionOp::BitAnd => int_ty.const_all_ones(),
        ReductionOp::Min => signed_int_max(int_ty),
        ReductionOp::Max => signed_int_min(int_ty),
        // Collect's identity is an empty container (`Vec.new()`), not an
        // integer — Collect reductions take the heap-allocated path
        // shipped in Phase 3 and never reach the integer-identity helper.
        // `try_emit_reduction_lowering`'s early-return for Collect is
        // what guards this; this arm is unreachable in Phase 2 and exists
        // only for match exhaustiveness.
        ReductionOp::Collect => unreachable!("Collect bypasses int identity"),
    }
}

/// Signed `T::MAX` constant for `int_ty` — `(1 << (bit_width - 1)) - 1`.
/// 64-bit special-case avoids the shift overflow that `1u64 << 64` would
/// trip on platforms where the shift amount is undefined for the full
/// width.
fn signed_int_max<'ctx>(int_ty: IntType<'ctx>) -> IntValue<'ctx> {
    let bit_width = int_ty.get_bit_width();
    let value = if bit_width >= 64 {
        i64::MAX as u64
    } else {
        (1u64 << (bit_width - 1)) - 1
    };
    int_ty.const_int(value, true)
}

/// Signed `T::MIN` constant for `int_ty` — `1 << (bit_width - 1)` (the
/// sign-bit-only two's-complement encoding). `const_int` takes a `u64`
/// payload and reinterprets the low `bit_width` bits according to the
/// `sign_extend` flag — passing the bit pattern with `true` produces
/// the correct negative value at every supported width.
fn signed_int_min<'ctx>(int_ty: IntType<'ctx>) -> IntValue<'ctx> {
    let bit_width = int_ty.get_bit_width();
    let value = if bit_width >= 64 {
        1u64 << 63
    } else {
        1u64 << (bit_width - 1)
    };
    int_ty.const_int(value, true)
}

// ── Cost-model gate (slice 3b.5, 2026-05-20) ──────────────────────────
//
// Compile-time gate that decides whether to lower a recognized reduction
// to `karac_par_reduce` or fall back to sequential codegen. Goal: keep
// the dispatch overhead (~tens of µs per call — Box alloc + queue push
// + Condvar wake/wait + N-way combine) from eating the work it parallelizes
// when the loop is small or the body is trivial.
//
// **Units convention.** Costs are expressed in "1 unit ≈ 1 ns" — same
// as how `DISPATCH_OVERHEAD_PER_CALL_UNITS` was calibrated. Per-iter
// body cost is estimated by walking the AST; the estimate is rough but
// monotone (more ops → higher estimate). For variable-K loops where K
// isn't a literal at compile time, the gate is bypassed (the runtime
// can't see through to the source expression cheaply at codegen time;
// most variable-K loops in practice are large like kata-7's 50M).

/// Per-call overhead of dispatching to `karac_par_reduce`, in
/// "1 unit ≈ 1 ns." Calibrated against the kata-7 bench: the pool-share
/// refactor (slice 3b.7) measured dispatch latency at ~10µs per call
/// for N=18 workers including Box alloc + queue push + N Condvar wakes
/// + the final N-way combine. Round-up to 10,000 units (10µs).
const DISPATCH_OVERHEAD_PER_CALL_UNITS: u64 = 10_000;

/// Worker count we assume at compile time for the threshold math. Real
/// runtime worker count is `available_parallelism()` (typically 4–18 on
/// developer machines), but we don't have that at codegen time — and
/// even if we did, baking it into the binary would defeat the
/// portability of the artifact. Median modern CPU is 8 cores; use that
/// as the assumed N. Slight under-estimate on big.LITTLE machines
/// (M5 Pro has 18 cores) lowers the threshold a bit, which is the safer
/// direction (more loops cross the gate at small K).
const ASSUMED_WORKER_COUNT: u64 = 8;

/// Threshold for the cost-model gate. Total work (K × per-iter cost) must
/// exceed this for the par_reduce dispatch to win. With the calibration
/// above, this is 80,000 unit-iterations ≈ 80µs of estimated work — at
/// that scale, the ~10µs dispatch overhead amortizes to roughly 12% of
/// runtime, leaving most of the work for parallel speedup.
const REDUCE_DISPATCH_THRESHOLD_UNITS: u64 =
    DISPATCH_OVERHEAD_PER_CALL_UNITS * ASSUMED_WORKER_COUNT;

/// Threshold for the `karac_par_run` (parallel-group dispatch) gate.
/// Sum of per-branch costs must exceed this for dispatch to win;
/// otherwise the group falls back to sequential statement codegen.
///
/// **Calibration is separate from the reduce gate.** `REDUCE_DISPATCH_
/// THRESHOLD_UNITS` (= 80K units) is calibrated for `iter_total ×
/// per_iter_cost` where `iter_total` is in the millions (kata-7's
/// 50M iters validates that scale). For par_run, the comparable
/// metric is `sum_branch_costs` where N branches is small (2-4
/// typically) and each branch represents a single statement evaluation
/// — orders of magnitude smaller numbers. Reusing 80K here gated out
/// every realistic par_run shape (including the existing
/// `test_auto_par_*_emits_par_run` IR pins, whose fixture fns have
/// trivial bodies like `{ 1 }`). The right calibration is closer to
/// `DISPATCH_OVERHEAD_PER_CALL_UNITS / 20`, which puts the per-branch
/// threshold (sum / N) at ~5x dispatch overhead for typical N=2-4 —
/// enough to amortize while still catching kata-2's wasteful prologue
/// group (cost ≈ 411 estimator units, well below 500).
///
/// Surfaced 2026-05-23 by the kata-2 (add-two-numbers) bench: the
/// 2-stmt prologue group `let b = make_nines(n); let l1 =
/// from_array(a.as_slice());` produced wasteful dispatch (+263 KiB
/// binary, 0 wall benefit) because no codegen-time cost gate existed
/// for the par_run path, only the analyzer-level `is_trivial` check
/// which was correctly false (both stmts are effectful, both write to
/// distinct allocator resources). See
/// `docs/implementation_checklist/phase-7-codegen.md` § "Auto-par
/// `karac_par_run` (find_parallel_groups): per-stmt cost gate" for
/// the worked example.
pub(super) const PAR_RUN_DISPATCH_THRESHOLD_UNITS: u64 = 500;

/// Minimum per-branch cost for the par_run gate to fire. Below this,
/// the estimator hasn't seen enough structure in the branch's
/// resolved body to be confident the work is genuinely small — most
/// commonly because the branch's top-level callee is a thin wrapper
/// whose body is just a method call (e.g. parallax's
/// `fn fetch_profile(uid) -> Profile with reads(UserDB) {
/// UserDB.fetch_profile(uid) }`, body cost ≈ 10). For those shapes
/// the gate stays *off* — the actual work lives inside the impl
/// method body which the estimator can't see (`CostEstimator` only
/// traces into free-fn callees by name, not into trait method impls;
/// extending it needs typechecker-resolved receiver-type info, a
/// separate slice). Kata-2's branches go through an inline-resolved
/// free fn with a visible loop (cost ≈ 211 each); they sit safely
/// above this threshold, so the gate can fire.
pub(super) const PAR_RUN_VISIBILITY_THRESHOLD_UNITS: u64 = 50;

/// Compute (total, min_per_branch) cost for a parallel group, used by
/// the codegen-time par_run gate. The gate fires only when *both*
/// thresholds clear: total below dispatch threshold AND every branch
/// above the visibility threshold (i.e. the estimator saw real
/// structure in every branch, not just a thin wrapper shell). Returns
/// `(0, 0)` when no `Program` snapshot is available — the caller
/// treats `(0, _)` as "no estimate" and skips the gate.
///
/// Mirrors `CostEstimator::estimate_body`'s per-stmt walk: each
/// branch's cost is `estimate_stmt(stmt)` (which folds Let/Assign/
/// CompoundAssign/Expr/LetUninit/Defer arms). The estimator inlines
/// free-function callee bodies up to `INLINE_DEPTH_CAP` levels deep —
/// same shape as the par_reduce gate's per-iter cost.
pub(super) fn estimate_par_run_group_cost_units(
    program: Option<&Program>,
    group_stmts: &[&Stmt],
) -> (u64, u64) {
    let Some(program) = program else {
        // No snapshot → can't estimate. `(0, 0)` is the sentinel; the
        // caller treats it as "no estimate" and lets the par_run
        // dispatch proceed (the analyzer-level `is_trivial` gate is
        // still in force, plus the slice-2 group_defines_binding_used_
        // outside drop).
        return (0, 0);
    };
    let mut estimator = CostEstimator::new(program);
    let mut total: u64 = 0;
    let mut min_per_branch: u64 = u64::MAX;
    for stmt in group_stmts {
        let c = estimator.estimate_stmt(stmt);
        total = total.saturating_add(c);
        if c < min_per_branch {
            min_per_branch = c;
        }
    }
    if group_stmts.is_empty() {
        return (0, 0);
    }
    (total, min_per_branch)
}

/// Try to const-evaluate the loop's iteration count = `end - lo` to a
/// literal. Returns `None` for any non-literal shape on either bound
/// (Identifier, expression involving captures, etc.) so the cost-model
/// gate conservatively assumes "large enough to parallelize." Pre- and
/// post-lowering both leave integer literals untouched, so this is
/// shape-agnostic across the pipeline. `lo_expr = None` means "no lo
/// in the source" (treated as 0 — the slice 3b / 3b.4 shape).
fn const_eval_iter_count(end_expr: &Expr, lo_expr: Option<&Expr>) -> Option<u64> {
    let end_lit = const_eval_int_literal(end_expr)?;
    let lo_lit = match lo_expr {
        Some(e) => const_eval_int_literal(e)?,
        None => 0,
    };
    let count = end_lit.checked_sub(lo_lit)?;
    if count >= 0 {
        Some(count as u64)
    } else {
        None
    }
}

/// Pull a signed-int literal out of an Expr. Returns `None` for any non-
/// literal shape — including negative literals that the parser already
/// represents as a Unary{Neg, Integer(n)} rather than Integer(-n); v1's
/// reduction range bounds rarely use negatives so the literal arm is
/// sufficient. Pre- and post-lowering both leave Integer(n) untouched.
fn const_eval_int_literal(expr: &Expr) -> Option<i64> {
    if let ExprKind::Integer(n, _) = expr.kind {
        Some(n)
    } else {
        None
    }
}

/// Codegen-time per-iter body-cost estimator. Walks the AST with weights
/// chosen to bias toward the actual code shape: arithmetic / comparison
/// / cast each cost a small constant; function and method calls fall
/// back to `CALL_COST_UNITS` for opaque callees but recursively estimate
/// the callee's body when it's a known free function in this program
/// (up to `INLINE_DEPTH_CAP` levels deep). Control-flow takes the
/// max-arm path (conservative for cost, so the gate over-counts and
/// thus over-parallelizes — acceptable bias for v1). A nested loop with a
/// compile-time-evaluable range (`for i in 0..16`) uses its exact trip
/// count; a runtime-bounded loop (`while`, `for x in v.iter()`, runtime
/// range, `loop`) uses `RUNTIME_NESTED_LOOP_MULTIPLIER` since the trip
/// count is unknown at codegen time.
///
/// The inlining-aware path (slice: cost-gate fn-call body cost,
/// 2026-05-20) addresses the constant-10 underestimate surfaced by the
/// post-3b.10 re-bench sweep: `for _ in 0..K { sum += f(big_input); }`
/// shapes scored as `K * 10` cost units regardless of what `f` did,
/// so K=10 outer reductions over heavy callees (kata-121's max_profit,
/// kata-153's find_min) failed the cost gate and ran sequentially. By
/// recursing into resolvable callees the gate now reflects the callee's
/// structural cost (number of stmts, branches, inner loops) rather than
/// a constant.
struct CostEstimator<'a> {
    /// Free-function bodies keyed by source name. Built once from
    /// `Program.items` at construction; method bodies are not included
    /// at v1 (`MethodCall` and 2+-segment `Path` calls keep the
    /// `CALL_COST_UNITS` fallback — adding receiver-type-resolved
    /// method lookup needs typechecker info threaded in, deferred).
    fn_bodies: HashMap<String, &'a Function>,
    /// Current inlining recursion depth. Bounded by `INLINE_DEPTH_CAP`
    /// to prevent unbounded recursion on indirect-recursive call graphs
    /// (`A → B → A`) without needing a visited-set: the depth alone is
    /// a safe upper bound because each recursive call increments it.
    depth: u32,
}

impl<'a> CostEstimator<'a> {
    /// Recursion cap for body inlining. Three levels = the caller, one
    /// callee, one grand-callee — enough to estimate a `sum += f(...)`
    /// shape that hides a real-work-doing loop inside `f`, without
    /// blowing up on deep call chains. Past the cap, calls fall back
    /// to `CALL_COST_UNITS` so the estimator always terminates.
    const INLINE_DEPTH_CAP: u32 = 3;

    fn new(program: &'a Program) -> Self {
        let mut fn_bodies = HashMap::new();
        for item in &program.items {
            if let Item::Function(f) = item {
                fn_bodies.insert(f.name.clone(), f);
            }
        }
        Self {
            fn_bodies,
            depth: 0,
        }
    }

    /// Body-cost entry point. Per-iter cost in "1 unit ≈ 1 ns" —
    /// matches the calibration unit of `DISPATCH_OVERHEAD_PER_CALL_UNITS`
    /// so threshold math stays apples-to-apples.
    fn estimate_body(&mut self, body: &Block) -> u64 {
        let mut total: u64 = 0;
        for stmt in &body.stmts {
            total = total.saturating_add(self.estimate_stmt(stmt));
        }
        if let Some(e) = &body.final_expr {
            total = total.saturating_add(self.estimate_expr(e));
        }
        // Bound at 1 so a trivially-empty body (no stmts, no final expr
        // — analyzer rejects this earlier but the helper stays safe)
        // doesn't gate out every loop at K * 0 = 0 < threshold.
        total.max(1)
    }

    fn estimate_stmt(&mut self, stmt: &Stmt) -> u64 {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                1u64.saturating_add(self.estimate_expr(value))
            }
            StmtKind::Assign { target, value } => 1u64
                .saturating_add(self.estimate_expr(target))
                .saturating_add(self.estimate_expr(value)),
            StmtKind::CompoundAssign { target, value, .. } => 2u64
                .saturating_add(self.estimate_expr(target))
                .saturating_add(self.estimate_expr(value)),
            StmtKind::Expr(e) => self.estimate_expr(e),
            StmtKind::LetUninit { .. } => 1,
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                // Defer bodies run at scope exit, not per-iter — but in
                // the worker-fn the worker scope IS the iter scope (one
                // alloca frame), so count once. Conservative; the
                // slice-3b worker-fn synth pushes one cleanup frame per
                // call anyway.
                self.estimate_body(body)
            }
        }
    }

    /// Resolve a Call's callee identifier to a free-fn body cost when
    /// possible. Returns `CALL_COST_UNITS` when the callee shape isn't
    /// a known free-fn name, or when the recursion depth cap is hit.
    /// Caller is responsible for adding arg costs separately — this
    /// returns the body-walk cost only (mirrors the prior CALL_COST_UNITS
    /// semantics, which represented the callee body opaquely).
    fn call_body_cost(&mut self, callee: &Expr) -> u64 {
        if self.depth >= Self::INLINE_DEPTH_CAP {
            return CALL_COST_UNITS;
        }
        let name = match &callee.kind {
            ExprKind::Identifier(n) => Some(n.clone()),
            ExprKind::Path { segments, .. } if segments.len() == 1 => Some(segments[0].clone()),
            _ => None,
        };
        let Some(name) = name else {
            return CALL_COST_UNITS;
        };
        let Some(f) = self.fn_bodies.get(&name).copied() else {
            return CALL_COST_UNITS;
        };
        self.depth += 1;
        let cost = self.estimate_body(&f.body);
        self.depth -= 1;
        cost
    }

    fn estimate_expr(&mut self, expr: &Expr) -> u64 {
        match &expr.kind {
            // Free: leaf literals + identifier loads. SSA-promoted alloca
            // reads compile to a single load that the LLVM backend almost
            // always folds into the consuming instruction.
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::Bool(_)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType => 0,

            // Arithmetic / bitwise / comparison: 1 unit each plus operand costs.
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => 1u64
                .saturating_add(self.estimate_expr(left))
                .saturating_add(self.estimate_expr(right)),
            ExprKind::NilCoalesce { left, right } => 1u64
                .saturating_add(self.estimate_expr(left))
                .saturating_add(self.estimate_expr(right)),
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                1u64.saturating_add(self.estimate_expr(operand))
            }
            ExprKind::Cast { expr: inner, .. } => 1u64.saturating_add(self.estimate_expr(inner)),

            // Indexing: 2 units (GEP + load + bounds check) plus operand costs.
            ExprKind::Index { object, index } => 2u64
                .saturating_add(self.estimate_expr(object))
                .saturating_add(self.estimate_expr(index)),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                1u64.saturating_add(self.estimate_expr(object))
            }

            // Calls: try to inline the callee's body cost when the callee
            // is a known free fn within the recursion depth cap; else
            // fall back to `CALL_COST_UNITS`. Args + callee-expr eval
            // costs added separately.
            ExprKind::Call { callee, args } => {
                let mut c: u64 = self.call_body_cost(callee);
                c = c.saturating_add(self.estimate_expr(callee));
                for arg in args {
                    c = c.saturating_add(self.estimate_expr(&arg.value));
                }
                c
            }
            ExprKind::MethodCall { object, args, .. } => {
                // Method receiver type resolution isn't threaded into
                // the estimator at v1 — keep the opaque CALL_COST_UNITS
                // fallback. Adding receiver-type-aware method lookup
                // requires the typechecker's method_callee_types table.
                let mut c: u64 = CALL_COST_UNITS;
                c = c.saturating_add(self.estimate_expr(object));
                for arg in args {
                    c = c.saturating_add(self.estimate_expr(&arg.value));
                }
                c
            }
            ExprKind::OptionalChain { object, args, .. } => {
                let mut c: u64 = CALL_COST_UNITS;
                c = c.saturating_add(self.estimate_expr(object));
                if let Some(args) = args {
                    for arg in args {
                        c = c.saturating_add(self.estimate_expr(&arg.value));
                    }
                }
                c
            }

            // Control-flow: walk arms, take the max (conservative cost).
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                let cond = self.estimate_expr(condition);
                let then_cost = self.estimate_body(then_block);
                let else_cost = else_branch
                    .as_ref()
                    .map(|e| self.estimate_expr(e))
                    .unwrap_or(0);
                cond.saturating_add(then_cost.max(else_cost))
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                let v = self.estimate_expr(value);
                let then_cost = self.estimate_body(then_block);
                let else_cost = else_branch
                    .as_ref()
                    .map(|e| self.estimate_expr(e))
                    .unwrap_or(0);
                v.saturating_add(then_cost.max(else_cost))
            }
            ExprKind::Match { scrutinee, arms } => {
                let s = self.estimate_expr(scrutinee);
                let arm_max = arms
                    .iter()
                    .map(|a| self.estimate_expr(&a.body))
                    .max()
                    .unwrap_or(0);
                s.saturating_add(arm_max)
            }

            // Inner loops: trip count drives the cost. A compile-time-
            // evaluable `for i in lo..hi` uses its EXACT count (no over- or
            // under-estimate); every runtime-bounded loop (while, while-let,
            // `for x in v.iter()`, runtime/step_by ranges, bare loop) uses
            // `RUNTIME_NESTED_LOOP_MULTIPLIER` — the flat-16 it replaces was
            // orders of magnitude low for real scans (see the const's doc).
            ExprKind::While {
                condition, body, ..
            } => {
                let c = self.estimate_expr(condition);
                let b = self.estimate_body(body);
                RUNTIME_NESTED_LOOP_MULTIPLIER.saturating_mul(c.saturating_add(b))
            }
            ExprKind::WhileLet { value, body, .. } => {
                let v = self.estimate_expr(value);
                let b = self.estimate_body(body);
                RUNTIME_NESTED_LOOP_MULTIPLIER.saturating_mul(v.saturating_add(b))
            }
            ExprKind::For { iterable, body, .. } => {
                let it = self.estimate_expr(iterable);
                let b = self.estimate_body(body);
                // `for i in lo..hi` with literal bounds → exact trip count.
                // (Half-open only; an inclusive `..=` const range is rare in
                // a hot inner loop and falls through to the runtime path.)
                if let ExprKind::Range {
                    start,
                    end: Some(end),
                    inclusive: false,
                } = &iterable.kind
                {
                    if let Some(count) = const_eval_iter_count(end, start.as_deref()) {
                        return count.saturating_mul(b.max(1)).saturating_add(it);
                    }
                }
                RUNTIME_NESTED_LOOP_MULTIPLIER.saturating_mul(it.saturating_add(b))
            }
            ExprKind::Loop { body, .. } => {
                RUNTIME_NESTED_LOOP_MULTIPLIER.saturating_mul(self.estimate_body(body))
            }

            // Blocks and other shape-passthrough nodes: cost of the contained block.
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b) => self.estimate_body(b),
            ExprKind::Par(b) => self.estimate_body(b),
            ExprKind::Lock { body, .. } => self.estimate_body(body),
            ExprKind::LabeledBlock { body, .. } => self.estimate_body(body),

            // Composite literals — cost is sum of element costs.
            ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
                let mut c: u64 = 0;
                for e in elems {
                    c = c.saturating_add(self.estimate_expr(e));
                }
                c
            }
            ExprKind::RepeatLiteral { value, count, .. } => 1u64
                .saturating_add(self.estimate_expr(value))
                .saturating_add(self.estimate_expr(count)),
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                let mut c: u64 = 1;
                for e in items {
                    c = c.saturating_add(self.estimate_expr(e));
                }
                c
            }
            ExprKind::MapLiteral(entries) => {
                let mut c: u64 = 1;
                for (k, v) in entries {
                    c = c.saturating_add(self.estimate_expr(k));
                    c = c.saturating_add(self.estimate_expr(v));
                }
                c
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                let mut c: u64 = 1;
                for f in fields {
                    c = c.saturating_add(self.estimate_expr(&f.value));
                }
                if let Some(s) = spread {
                    c = c.saturating_add(self.estimate_expr(s));
                }
                c
            }
            ExprKind::Range { start, end, .. } => {
                let mut c: u64 = 0;
                if let Some(s) = start {
                    c = c.saturating_add(self.estimate_expr(s));
                }
                if let Some(e) = end {
                    c = c.saturating_add(self.estimate_expr(e));
                }
                c
            }
            ExprKind::Closure { body, .. } => self.estimate_expr(body),
            ExprKind::Providers { bindings, body } => {
                let mut c: u64 = 0;
                for b in bindings {
                    c = c.saturating_add(self.estimate_expr(&b.value));
                }
                c.saturating_add(self.estimate_body(body))
            }
            ExprKind::Return(Some(inner)) => self.estimate_expr(inner),
            ExprKind::Break { value: Some(v), .. } => self.estimate_expr(v),
            ExprKind::InterpolatedStringLit(parts) => {
                let mut c: u64 = 1;
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(inner) = part {
                        c = c.saturating_add(self.estimate_expr(inner));
                    }
                }
                c
            }

            // Pure control-edge shapes.
            ExprKind::Continue { .. }
            | ExprKind::Return(None)
            | ExprKind::Break { value: None, .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => 0,
        }
    }
}

/// Free-fn wrapper kept for backward compatibility with internal call
/// sites that don't need the inlining-aware path. Internally builds an
/// estimator with an empty `fn_bodies` map — semantically equivalent
/// to "every call is opaque, return CALL_COST_UNITS" — so it matches
/// the pre-slice behavior on its own.
fn estimate_body_cost_units(body: &Block) -> u64 {
    let mut est = CostEstimator {
        fn_bodies: HashMap::new(),
        depth: 0,
    };
    est.estimate_body(body)
}

/// Function-call cost — function-call ABI alone is on the order of 5–20
/// ns (PLT + arg marshalling + branch); add ~10 units for the callee
/// body when the callee is opaque (Method call, multi-segment Path,
/// past the recursion-depth cap). When the callee is a resolvable free
/// fn within the cap, the body's actual structural cost replaces this
/// constant — see `CostEstimator::call_body_cost`.
const CALL_COST_UNITS: u64 = 10;

/// Trip-count multiplier for a loop whose bound is *runtime* (not a
/// compile-time-evaluable range): `while i < s.len()`, `for x in v.iter()`,
/// `for j in (a..=b).step_by(k)`, `loop { ... }`. The flat
/// `NESTED_LOOP_MULTIPLIER = 16` underestimated these by orders of
/// magnitude — a `while i < hn` over a 2M-element slice runs millions of
/// times, not 16 — so a doubly-nested runtime scan (`str_str`'s
/// `while i { while j { s[i+j] == n[j] } }`, kata-28) scored ≈30k cost
/// units (`16² × body × K=10`) and fell under the 80k dispatch threshold,
/// declining a real ~11× parallel win to a serial run. 64 is calibrated
/// so a doubly-nested runtime loop crosses the gate (`64² × body × K`)
/// while a *single* runtime loop at small K stays conservatively serial
/// (kata-1 hash_map's lone `for i in 0..n` at K=10 ≈ 64 × body × 10 stays
/// well under threshold) — over-firing genuinely light bodies is the cost
/// we keep bounded, since the existing gate philosophy already biases
/// toward over-counting (control-flow takes the max arm). Compile-time-
/// evaluable ranges (`for i in 0..16`) bypass this entirely and use their
/// exact count (see the `For`/`While`/`Loop` arms in `estimate_expr`).
/// Surfaced + calibrated by the 2026-06-13 `for _` auto-par re-bench sweep
/// (phase-7-codegen.md); the calibration follow-up the closed
/// "function-call body-cost estimation" slice deferred "when needed".
const RUNTIME_NESTED_LOOP_MULTIPLIER: u64 = 64;

/// Canonical shape of a recognized reduction loop. Built by
/// `extract_loop_shape` from either the `for k in lo..hi` shape
/// (slices 3b + 3b.3) or the `while k < hi { ...; k = k + 1; }` shape
/// (slice 3b.4) and consumed by the lowering path. `body` is the source
/// body with the while-shape's terminal increment already stripped — so
/// the worker fn synth treats both shapes identically and always emits
/// its own back-edge `k += 1`. `lo_expr` is `None` when the source's
/// start bound is absent or `Integer(0)` (the common case — no shift
/// math at all in the worker); `Some(expr)` otherwise (slice 3b.3 — the
/// expr is compiled in the parent, passed through env-struct field 0,
/// and added to the worker's chunk-local start/end). The while-shape
/// always sets `lo_expr = None` since its loop-var init is gated to
/// literal 0 by `preceding_stmt_inits_to_zero`.
struct LoopShape {
    loop_var: String,
    end_expr: Expr,
    body: Block,
    lo_expr: Option<Expr>,
}

/// Match a less-than condition into `(loop_var_name, end_expr)`.
/// Accepts both pre-lowering `Binary { Lt, Ident(k), end }` and post-
/// lowering `Call(Path([type, "lt"]), [Ident(k), end])` — the codegen
/// pipeline runs `src/lowering.rs` before reaching us, so the post-
/// lowering shape is the common case, but `compile_to_ir` tests that
/// skip lowering need the pre-lowering arm too.
fn parse_lt_condition(condition: &Expr) -> Option<(String, Expr)> {
    match &condition.kind {
        ExprKind::Binary {
            op: BinOp::Lt,
            left,
            right,
        } => {
            let ExprKind::Identifier(name) = &left.kind else {
                return None;
            };
            Some((name.clone(), (**right).clone()))
        }
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 || segments[1] != "lt" || args.len() != 2 {
                return None;
            }
            let ExprKind::Identifier(name) = &args[0].value.kind else {
                return None;
            };
            Some((name.clone(), args[1].value.clone()))
        }
        _ => None,
    }
}

/// If the last stmt of `body` is `loop_var = loop_var + 1` or
/// `loop_var += 1` (in either pre- or post-lowered form), return a
/// fresh `Block` with that stmt removed. Returns `None` if the terminal
/// shape doesn't match — the recognizer (slice 1) only emits a
/// `LoopReduction` when the body has at most one induction step, so a
/// loop tagged as a reduction whose body's terminal stmt isn't the
/// step must have a non-canonical layout we don't handle in v1.
///
/// Also returns `None` when the loop variable is written anywhere else
/// in the body (defense-in-depth — the analyzer already rejects that
/// shape, but the codegen check costs nothing and pins the invariant).
fn strip_terminal_step_one_increment(body: &Block, loop_var: &str) -> Option<Block> {
    let last = body.stmts.last()?;
    if !is_step_one_increment_stmt(last, loop_var) {
        return None;
    }
    // Verify no other stmt in the body writes the loop variable. A
    // body-internal `k = <expr>` in the middle would shift the worker
    // fn out of the simple chunk-local-counter model.
    for (idx, s) in body.stmts.iter().enumerate() {
        if idx + 1 == body.stmts.len() {
            break;
        }
        if stmt_writes_loop_var(s, loop_var) {
            return None;
        }
    }
    let mut stripped = body.clone();
    stripped.stmts.pop();
    Some(stripped)
}

/// True iff `stmt` is `loop_var = loop_var + 1` or `loop_var += 1`,
/// in either pre-lowering or post-lowering form. The constant `1` is
/// matched by value (any int suffix accepted; the recognizer already
/// gates on int suffix at the analyzer level).
fn is_step_one_increment_stmt(stmt: &Stmt, loop_var: &str) -> bool {
    match &stmt.kind {
        StmtKind::Assign { target, value } => {
            if !is_named_identifier(target, loop_var) {
                return false;
            }
            // Pre-lowering: Binary { Add, Ident(loop_var), Int(1) }.
            // Lowered: Call(Path([T, "add"]), [Ident(loop_var), Int(1)]).
            match &value.kind {
                ExprKind::Binary {
                    op: BinOp::Add,
                    left,
                    right,
                } => is_loop_var_plus_one(left, right, loop_var),
                ExprKind::Call { callee, args } => {
                    let ExprKind::Path { segments, .. } = &callee.kind else {
                        return false;
                    };
                    if segments.len() != 2 || segments[1] != "add" || args.len() != 2 {
                        return false;
                    }
                    is_loop_var_plus_one(&args[0].value, &args[1].value, loop_var)
                }
                _ => false,
            }
        }
        StmtKind::CompoundAssign {
            target,
            op: CompoundOp::Add,
            value,
        } => is_named_identifier(target, loop_var) && is_int_literal_one(value),
        _ => false,
    }
}

fn is_loop_var_plus_one(left: &Expr, right: &Expr, loop_var: &str) -> bool {
    let left_is_var = matches!(&left.kind, ExprKind::Identifier(n) if n == loop_var);
    let right_is_var = matches!(&right.kind, ExprKind::Identifier(n) if n == loop_var);
    let left_is_one = is_int_literal_one(left);
    let right_is_one = is_int_literal_one(right);
    (left_is_var && right_is_one) || (right_is_var && left_is_one)
}

fn is_int_literal_one(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Integer(1, _))
}

fn is_named_identifier(expr: &Expr, name: &str) -> bool {
    matches!(&expr.kind, ExprKind::Identifier(n) if n == name)
}

/// Whether a stmt writes (Assign / CompoundAssign target = identifier)
/// the named loop variable. Used to defense-in-depth the
/// `strip_terminal_step_one_increment` body scan.
fn stmt_writes_loop_var(stmt: &Stmt, loop_var: &str) -> bool {
    match &stmt.kind {
        StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
            is_named_identifier(target, loop_var)
        }
        _ => false,
    }
}

/// If `parent_body.stmts[stmt_index - 1]` is `let mut loop_var: T =
/// <anything>;`, return the init expression. Caller decides how to
/// translate the init into the worker's chunk-local shift:
///   - `Integer(0)` → `lo_expr = None` (no shift math, current path).
///   - Non-zero int literal → `lo_expr = Some(literal)` (slice 3b.9 —
///     re-compile literal in the parent's par_reduce setup, free).
///   - Anything else → `lo_expr = Some(Identifier(loop_var))` (slice
///     3b.10 — load from the parent's already-initialized k alloca
///     instead of re-evaluating the init expression, which would
///     double-evaluate side effects).
///
/// Returns `None` if the preceding stmt isn't a let-mut binding of the
/// loop var. Caller guarantees `stmt_index > 0`.
fn preceding_stmt_init(parent_body: &Block, stmt_index: usize, loop_var: &str) -> Option<Expr> {
    let prev = &parent_body.stmts[stmt_index - 1];
    let StmtKind::Let {
        pattern,
        value,
        is_mut: true,
        ..
    } = &prev.kind
    else {
        return None;
    };
    let PatternKind::Binding(name) = &pattern.kind else {
        return None;
    };
    if name != loop_var {
        return None;
    }
    Some(value.clone())
}

/// Const-prop helper for `partition_const_int_captures`: if the parent
/// body has a top-level `let <name>: <T> = <int-literal>;` (non-mut)
/// stmt before `stmt_index`, and no later top-level stmt reassigns
/// `<name>` before `stmt_index`, return the literal's value + suffix.
///
/// Conservative on purpose:
/// - Top-level stmts only (skip lets nested inside if/for/while/match).
///   Reductions land at top level, so the captured constant is almost
///   always a top-level let.
/// - Non-mut only. A `let mut n = 8; ...; n = 9;` would be unsound to
///   const-prop. Easier than scanning for later writes.
/// - Integer literal only. Bool/Float would extend cleanly but no kata
///   exercises them through par-reduce captures yet.
/// - Single-name binding patterns only — destructuring lets don't fit
///   the "captured name" shape collect_reduction_captures returns.
fn find_top_level_const_int_init(
    parent_body: &Block,
    name: &str,
    stmt_index: usize,
) -> Option<(i64, Option<IntSuffix>)> {
    let mut found: Option<(i64, Option<IntSuffix>)> = None;
    for (i, stmt) in parent_body.stmts.iter().enumerate() {
        if i >= stmt_index {
            break;
        }
        match &stmt.kind {
            StmtKind::Let {
                is_mut: false,
                pattern,
                value,
                ..
            } => {
                let PatternKind::Binding(let_name) = &pattern.kind else {
                    continue;
                };
                if let_name != name {
                    continue;
                }
                let ExprKind::Integer(n, sfx) = &value.kind else {
                    return None;
                };
                found = Some((*n, *sfx));
            }
            StmtKind::Let {
                is_mut: true,
                pattern,
                ..
            }
            | StmtKind::LetElse { pattern, .. } => {
                if let PatternKind::Binding(let_name) = &pattern.kind {
                    if let_name == name {
                        return None;
                    }
                }
            }
            StmtKind::LetUninit { name: let_name, .. } if let_name == name => return None,
            StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. }
                if is_named_identifier(target, name) =>
            {
                return None
            }
            _ => {}
        }
    }
    found
}

/// A worker-body indexing site whose bounds check can be hoisted out
/// of the per-iter path via a single preflight at fn entry. See the
/// `find_modulo_hoistable_bounds` doc-comment for the recognition
/// rules and soundness arguments.
#[derive(Debug, Clone)]
struct HoistableModuloBound {
    /// Captured Vec name being indexed (`inputs` in the kata-8 bench).
    vec_var: String,
    /// Local let-bound index name (`idx` in the bench).
    idx_var: String,
    /// Exclusive upper bound proved for `idx` — the modulo divisor.
    /// Preflight emits `if vec.len() < upper_lit panic`.
    upper_lit: i64,
}

/// If `value` is `<loop_var> % <positive_int>`, return the integer.
/// Mirrors the operator surface the typechecker lowers `%` into: pre-
/// lowering it's `BinOp::Mod`, post-lowering it becomes
/// `Call(Path([T, "rem"]), [lhs, rhs])`. We accept both.
///
/// The divisor is either an integer literal *or* an identifier that
/// names a known const-int capture (the let-init-const-prop fix in
/// `partition_const_int_captures` swaps the value in but leaves the
/// AST `Identifier(name)` — the lookup recovers the literal value).
fn modulo_upper_for_loop_var(
    value: &Expr,
    loop_var: &str,
    const_int_lookup: &HashMap<&str, i64>,
) -> Option<i64> {
    match &value.kind {
        ExprKind::Binary {
            op: BinOp::Mod,
            left,
            right,
        } => modulo_arms_match(left, right, loop_var, const_int_lookup),
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 || segments[1] != "rem" || args.len() != 2 {
                return None;
            }
            modulo_arms_match(&args[0].value, &args[1].value, loop_var, const_int_lookup)
        }
        _ => None,
    }
}

fn modulo_arms_match(
    left: &Expr,
    right: &Expr,
    loop_var: &str,
    const_int_lookup: &HashMap<&str, i64>,
) -> Option<i64> {
    let ExprKind::Identifier(name) = &left.kind else {
        return None;
    };
    if name != loop_var {
        return None;
    }
    let divisor = match &right.kind {
        ExprKind::Integer(n, _) => *n,
        ExprKind::Identifier(name) => *const_int_lookup.get(name.as_str())?,
        _ => return None,
    };
    if divisor > 0 {
        Some(divisor)
    } else {
        None
    }
}

/// Mark any vec-named identifier that's potentially mutated in `stmt`.
/// Conservative: any `vec.method()` call counts (we'd need a per-method
/// read-only allowlist to be precise, but the par-reduce body doesn't
/// typically call methods on captured vecs anyway, so the cost of the
/// conservative call is near zero on the kata surface).
fn collect_mutated_vec_names_in_stmt(stmt: &Stmt, out: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } | StmtKind::Expr(value) => {
            collect_mutated_vec_names_in_expr(value, out);
        }
        StmtKind::Assign { target, value } => {
            mark_assign_target(target, out);
            collect_mutated_vec_names_in_expr(target, out);
            collect_mutated_vec_names_in_expr(value, out);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            mark_assign_target(target, out);
            collect_mutated_vec_names_in_expr(target, out);
            collect_mutated_vec_names_in_expr(value, out);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                collect_mutated_vec_names_in_stmt(s, out);
            }
            if let Some(e) = &body.final_expr {
                collect_mutated_vec_names_in_expr(e, out);
            }
        }
        StmtKind::LetUninit { .. } => {}
    }
}

/// Mark the root of an assignment target as mutated:
/// - `x = ...` → mark x
/// - `x[i] = ...` → mark x (the index-assign mutates x's contents)
/// - `x.f = ...` → mark x (field assign — for tracking we treat field
///   writes as full-vec mutation; would underrate the precision of a
///   `vec.field` write but vecs don't have user fields)
fn mark_assign_target(target: &Expr, out: &mut HashSet<String>) {
    let mut cur = target;
    loop {
        match &cur.kind {
            ExprKind::Identifier(name) => {
                out.insert(name.clone());
                return;
            }
            ExprKind::Index { object, .. }
            | ExprKind::FieldAccess { object, .. }
            | ExprKind::TupleIndex { object, .. } => {
                cur = object;
            }
            _ => return,
        }
    }
}

fn collect_mutated_vec_names_in_expr(expr: &Expr, out: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::MethodCall { object, args, .. } => {
            if let ExprKind::Identifier(name) = &object.kind {
                out.insert(name.clone());
            }
            collect_mutated_vec_names_in_expr(object, out);
            for a in args {
                collect_mutated_vec_names_in_expr(&a.value, out);
            }
        }
        ExprKind::Call { callee, args } => {
            collect_mutated_vec_names_in_expr(callee, out);
            for a in args {
                collect_mutated_vec_names_in_expr(&a.value, out);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            collect_mutated_vec_names_in_expr(left, out);
            collect_mutated_vec_names_in_expr(right, out);
        }
        ExprKind::Unary { operand, .. } => collect_mutated_vec_names_in_expr(operand, out),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_mutated_vec_names_in_expr(condition, out);
            for s in &then_block.stmts {
                collect_mutated_vec_names_in_stmt(s, out);
            }
            if let Some(e) = &then_block.final_expr {
                collect_mutated_vec_names_in_expr(e, out);
            }
            if let Some(e) = else_branch {
                collect_mutated_vec_names_in_expr(e, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_mutated_vec_names_in_expr(condition, out);
            for s in &body.stmts {
                collect_mutated_vec_names_in_stmt(s, out);
            }
            if let Some(e) = &body.final_expr {
                collect_mutated_vec_names_in_expr(e, out);
            }
        }
        ExprKind::For { iterable, body, .. } => {
            collect_mutated_vec_names_in_expr(iterable, out);
            for s in &body.stmts {
                collect_mutated_vec_names_in_stmt(s, out);
            }
            if let Some(e) = &body.final_expr {
                collect_mutated_vec_names_in_expr(e, out);
            }
        }
        ExprKind::Loop { body, .. } => {
            for s in &body.stmts {
                collect_mutated_vec_names_in_stmt(s, out);
            }
            if let Some(e) = &body.final_expr {
                collect_mutated_vec_names_in_expr(e, out);
            }
        }
        ExprKind::Block(b) | ExprKind::Seq(b) => {
            for s in &b.stmts {
                collect_mutated_vec_names_in_stmt(s, out);
            }
            if let Some(e) = &b.final_expr {
                collect_mutated_vec_names_in_expr(e, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_mutated_vec_names_in_expr(scrutinee, out);
            for arm in arms {
                collect_mutated_vec_names_in_expr(&arm.body, out);
            }
        }
        ExprKind::Cast { expr: inner, .. } => collect_mutated_vec_names_in_expr(inner, out),
        ExprKind::Index { object, index } => {
            collect_mutated_vec_names_in_expr(object, out);
            collect_mutated_vec_names_in_expr(index, out);
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            collect_mutated_vec_names_in_expr(object, out);
        }
        ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
            for e in elems {
                collect_mutated_vec_names_in_expr(e, out);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_mutated_vec_names_in_expr(s, out);
            }
            if let Some(e) = end {
                collect_mutated_vec_names_in_expr(e, out);
            }
        }
        ExprKind::Return(Some(e)) | ExprKind::Break { value: Some(e), .. } => {
            collect_mutated_vec_names_in_expr(e, out);
        }
        _ => {}
    }
}

fn collect_modulo_index_sites_in_stmt(
    stmt: &Stmt,
    captured: &HashSet<&str>,
    idx_to_upper: &HashMap<String, i64>,
    mutated: &HashSet<String>,
    seen: &mut HashSet<(String, String)>,
    out: &mut Vec<HoistableModuloBound>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. }
        | StmtKind::LetElse { value, .. }
        | StmtKind::Expr(value)
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. } => {
            collect_modulo_index_sites_in_expr(value, captured, idx_to_upper, mutated, seen, out);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                collect_modulo_index_sites_in_stmt(s, captured, idx_to_upper, mutated, seen, out);
            }
            if let Some(e) = &body.final_expr {
                collect_modulo_index_sites_in_expr(e, captured, idx_to_upper, mutated, seen, out);
            }
        }
        StmtKind::LetUninit { .. } => {}
    }
}

fn collect_modulo_index_sites_in_expr(
    expr: &Expr,
    captured: &HashSet<&str>,
    idx_to_upper: &HashMap<String, i64>,
    mutated: &HashSet<String>,
    seen: &mut HashSet<(String, String)>,
    out: &mut Vec<HoistableModuloBound>,
) {
    if let ExprKind::Index { object, index } = &expr.kind {
        if let (ExprKind::Identifier(vec_name), ExprKind::Identifier(idx_name)) =
            (&object.kind, &index.kind)
        {
            if captured.contains(vec_name.as_str()) && !mutated.contains(vec_name) {
                if let Some(upper) = idx_to_upper.get(idx_name) {
                    let key = (vec_name.clone(), idx_name.clone());
                    if !seen.contains(&key) {
                        seen.insert(key);
                        out.push(HoistableModuloBound {
                            vec_var: vec_name.clone(),
                            idx_var: idx_name.clone(),
                            upper_lit: *upper,
                        });
                    }
                }
            }
        }
        collect_modulo_index_sites_in_expr(object, captured, idx_to_upper, mutated, seen, out);
        collect_modulo_index_sites_in_expr(index, captured, idx_to_upper, mutated, seen, out);
        return;
    }
    match &expr.kind {
        ExprKind::Binary { left, right, .. } => {
            collect_modulo_index_sites_in_expr(left, captured, idx_to_upper, mutated, seen, out);
            collect_modulo_index_sites_in_expr(right, captured, idx_to_upper, mutated, seen, out);
        }
        ExprKind::Unary { operand, .. } => {
            collect_modulo_index_sites_in_expr(operand, captured, idx_to_upper, mutated, seen, out);
        }
        ExprKind::Call { callee, args } => {
            collect_modulo_index_sites_in_expr(callee, captured, idx_to_upper, mutated, seen, out);
            for a in args {
                collect_modulo_index_sites_in_expr(
                    &a.value,
                    captured,
                    idx_to_upper,
                    mutated,
                    seen,
                    out,
                );
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_modulo_index_sites_in_expr(object, captured, idx_to_upper, mutated, seen, out);
            for a in args {
                collect_modulo_index_sites_in_expr(
                    &a.value,
                    captured,
                    idx_to_upper,
                    mutated,
                    seen,
                    out,
                );
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_modulo_index_sites_in_expr(
                condition,
                captured,
                idx_to_upper,
                mutated,
                seen,
                out,
            );
            for s in &then_block.stmts {
                collect_modulo_index_sites_in_stmt(s, captured, idx_to_upper, mutated, seen, out);
            }
            if let Some(e) = &then_block.final_expr {
                collect_modulo_index_sites_in_expr(e, captured, idx_to_upper, mutated, seen, out);
            }
            if let Some(e) = else_branch {
                collect_modulo_index_sites_in_expr(e, captured, idx_to_upper, mutated, seen, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_modulo_index_sites_in_expr(
                condition,
                captured,
                idx_to_upper,
                mutated,
                seen,
                out,
            );
            for s in &body.stmts {
                collect_modulo_index_sites_in_stmt(s, captured, idx_to_upper, mutated, seen, out);
            }
            if let Some(e) = &body.final_expr {
                collect_modulo_index_sites_in_expr(e, captured, idx_to_upper, mutated, seen, out);
            }
        }
        ExprKind::For { iterable, body, .. } => {
            collect_modulo_index_sites_in_expr(
                iterable,
                captured,
                idx_to_upper,
                mutated,
                seen,
                out,
            );
            for s in &body.stmts {
                collect_modulo_index_sites_in_stmt(s, captured, idx_to_upper, mutated, seen, out);
            }
            if let Some(e) = &body.final_expr {
                collect_modulo_index_sites_in_expr(e, captured, idx_to_upper, mutated, seen, out);
            }
        }
        ExprKind::Loop { body, .. } | ExprKind::Block(body) | ExprKind::Seq(body) => {
            for s in &body.stmts {
                collect_modulo_index_sites_in_stmt(s, captured, idx_to_upper, mutated, seen, out);
            }
            if let Some(e) = &body.final_expr {
                collect_modulo_index_sites_in_expr(e, captured, idx_to_upper, mutated, seen, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_modulo_index_sites_in_expr(
                scrutinee,
                captured,
                idx_to_upper,
                mutated,
                seen,
                out,
            );
            for arm in arms {
                collect_modulo_index_sites_in_expr(
                    &arm.body,
                    captured,
                    idx_to_upper,
                    mutated,
                    seen,
                    out,
                );
            }
        }
        ExprKind::Cast { expr: inner, .. } => {
            collect_modulo_index_sites_in_expr(inner, captured, idx_to_upper, mutated, seen, out);
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            collect_modulo_index_sites_in_expr(object, captured, idx_to_upper, mutated, seen, out);
        }
        ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
            for e in elems {
                collect_modulo_index_sites_in_expr(e, captured, idx_to_upper, mutated, seen, out);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_modulo_index_sites_in_expr(s, captured, idx_to_upper, mutated, seen, out);
            }
            if let Some(e) = end {
                collect_modulo_index_sites_in_expr(e, captured, idx_to_upper, mutated, seen, out);
            }
        }
        ExprKind::Return(Some(e)) | ExprKind::Break { value: Some(e), .. } => {
            collect_modulo_index_sites_in_expr(e, captured, idx_to_upper, mutated, seen, out);
        }
        _ => {}
    }
}

/// a `return` / `break` / `continue` reachable from any statement or
/// nested expression. Reductions whose body has an early exit are
/// rejected at the lowering check, falling back to sequential codegen
/// rather than emitting a `ret <T>` inside the void worker fn.
fn block_has_early_exit(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_early_exit)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_early_exit(e))
}

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

fn expr_has_early_exit(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Return(_) | ExprKind::Break { .. } | ExprKind::Continue { .. } => true,
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
        ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
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

// ── Memory-bound rejection (2026-05-20) ──────────────────────────────
//
// The cost-model gates count compute units (arithmetic, branches,
// estimated callee bodies), but treat each memory access (`Index`,
// `FieldAccess` on a collection) at the same low weight as a single
// compute op. For a body that's dominated by memory reads with little
// compute beside it (kata-153's `find_min` inner `let x = nums[i];
// if x < m { m = x; }`), the cost-units estimate looks parallelizable
// (~10M units total for N=2M) but the wall-clock is bandwidth-bound:
// every worker fights for the same memory channel, splitting the
// scan across cores doesn't reduce wall-clock, and the par_reduce
// dispatch adds both User-CPU cost (workers spinning + dispatching
// across cores) and binary size (+262 KiB to link the runtime).
//
// Heuristic: skip the lowering when the body's per-iter shape is
// `read + minimal compute` (at least one Index/FieldAccess, no
// substantial function call). A "substantial" call is any free-fn
// Call or any MethodCall whose method isn't a known trivial accessor
// (`len`, `is_empty`, `as_slice`, `as_str`) — these accessors just
// shape-query the collection and don't add real per-iter compute.
// Bodies with a substantial call (e.g. `sum + reverse(inputs[k])`
// in kata-7's outer loop) bypass this gate because the call usually
// contributes enough compute to amortize the dispatch overhead
// regardless of the indexed read alongside it.
//
// False-positive risk: pure compute-bound loops with no memory access
// pass through (no Index → memory_count == 0 → gate doesn't fire),
// which is correct. False-negative risk: a body with a heavy Call
// + heavy Index (e.g., `f(big_index_chain)`) gets parallelized even
// though it's probably memory-bound — the call carries the gate over.
// Accepting this false-negative direction is the safer bias: missing
// a parallelism win on a hybrid workload is recoverable (we can land
// a smarter detector later), but over-parallelizing memory-bound work
// pays cost every run.

fn body_is_memory_bound(body: &Block) -> bool {
    let mut detector = MemoryBoundDetector {
        memory_count: 0,
        substantial_call: false,
    };
    detector.visit_body(body);
    detector.memory_count > 0 && !detector.substantial_call
}

struct MemoryBoundDetector {
    memory_count: u32,
    substantial_call: bool,
}

impl MemoryBoundDetector {
    fn visit_body(&mut self, body: &Block) {
        for stmt in &body.stmts {
            self.visit_stmt(stmt);
        }
        if let Some(e) = &body.final_expr {
            self.visit_expr(e);
        }
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => self.visit_expr(value),
            StmtKind::Assign { target, value } => {
                self.visit_expr(target);
                self.visit_expr(value);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.visit_expr(target);
                self.visit_expr(value);
            }
            StmtKind::Expr(e) => self.visit_expr(e),
            StmtKind::LetUninit { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.visit_body(body),
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Index { object, index } => {
                self.memory_count = self.memory_count.saturating_add(1);
                self.visit_expr(object);
                self.visit_expr(index);
            }
            ExprKind::FieldAccess { object, .. } => {
                self.memory_count = self.memory_count.saturating_add(1);
                self.visit_expr(object);
            }
            ExprKind::Call { callee, args } => {
                // The lowering pass rewrites every primitive binop /
                // comparison into a `Call(Path([type, op_method]), [a, b])`
                // shape (e.g. `x < m` → `Call(Path(["i64", "lt"]), [x, m])`).
                // These are intrinsic operator dispatches, not real
                // function calls — counting them as `substantial_call`
                // would defeat the memory-bound gate for every body that
                // has any arithmetic or comparison post-lowering (which
                // is every kata's body). Filter those out before tagging
                // the call as substantial.
                if !is_lowered_primitive_op_call(callee) {
                    self.substantial_call = true;
                }
                self.visit_expr(callee);
                for arg in args {
                    self.visit_expr(&arg.value);
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                if !is_trivial_accessor_method(method) {
                    self.substantial_call = true;
                }
                self.visit_expr(object);
                for arg in args {
                    self.visit_expr(&arg.value);
                }
            }
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
                self.visit_expr(left);
                self.visit_expr(right);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.visit_expr(operand);
            }
            ExprKind::Cast { expr: inner, .. } => self.visit_expr(inner),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.visit_expr(condition);
                self.visit_body(then_block);
                if let Some(e) = else_branch {
                    self.visit_expr(e);
                }
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.visit_body(b),
            // Other shapes (literals, identifiers, paths, etc.) contribute
            // no memory access or call signal.
            _ => {}
        }
    }
}

fn is_trivial_accessor_method(method: &str) -> bool {
    matches!(
        method,
        "len" | "is_empty" | "as_slice" | "as_str" | "as_bytes"
    )
}

/// Recognize the lowering-pass-emitted shape for a primitive operator
/// dispatch — `Call(Path([type, op_method]), [a, b])` where `op_method`
/// is one of the standard arithmetic / comparison / bitwise / shift
/// methods. These are intrinsic op calls and should not count as
/// "substantial" callees for the memory-bound gate.
fn is_lowered_primitive_op_call(callee: &Expr) -> bool {
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return false;
    };
    if segments.len() != 2 {
        return false;
    }
    matches!(
        segments[1].as_str(),
        // Arithmetic
        "add" | "sub" | "mul" | "div" | "rem" | "neg"
        // Comparison
        | "eq" | "ne" | "lt" | "le" | "gt" | "ge"
        // Bitwise
        | "bitor" | "bitand" | "bitxor" | "bitnot"
        // Shifts
        | "shl" | "shr"
        // Min/Max — added by the combined Min/Max slice (2026-05-20)
        | "min" | "max"
    )
}
