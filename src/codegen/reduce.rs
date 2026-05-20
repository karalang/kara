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

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, IntType, StructType};
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::state::VarSlot;

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
        let captures =
            self.collect_reduction_captures(&shape.body, &reduction.accumulator, &shape.loop_var);

        let worker_fn = self.emit_reduce_worker_fn(
            &reduction,
            acc_int_ty,
            &shape.loop_var,
            &shape.body,
            &captures,
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
            &captures,
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
                let PatternKind::Binding(loop_var) = &pattern.kind else {
                    return None;
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
                    loop_var: loop_var.clone(),
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
    fn emit_reduce_worker_fn(
        &mut self,
        reduction: &LoopReduction,
        acc_int_ty: IntType<'ctx>,
        loop_var_name: &str,
        body: &Block,
        captures: &[String],
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
            for n in captures {
                field_tys.push(saved_vars[n].ty);
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
                let alloca = self.create_entry_alloca(worker_fn, var_name, cap_ty);
                self.builder.build_store(alloca, field_val).unwrap();
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
        let k_alloca = self.create_entry_alloca(worker_fn, loop_var_name, acc_int_ty.into());
        self.builder.build_store(k_alloca, start_val).unwrap();
        self.variables.insert(
            loop_var_name.to_string(),
            VarSlot {
                ptr: k_alloca,
                ty: acc_int_ty.into(),
            },
        );

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
        // Compile the body in the worker fn's scope. `self.variables` now
        // binds the accumulator + loop var + captures to the worker's
        // local allocas, so the body's compile output reads/writes them
        // correctly.
        let body_result = self.compile_block(body);
        body_result?;

        // Increment + back-edge. The body's emit may have left the
        // builder positioned in a different basic block (nested control
        // flow). If the current block already has a terminator (e.g. a
        // body-internal `break` or `return` — both rejected upstream),
        // skip the back-edge. Otherwise emit `k = k + 1; br cond`.
        let current_bb = self.builder.get_insert_block().unwrap();
        if current_bb.get_terminator().is_none() {
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
                field_tys.push(self.variables[n].ty);
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
                let val = self.builder.build_load(slot.ty, slot.ptr, name).unwrap();
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
        // narrower int — the descriptor field is usize (i64 on 64-bit).
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
/// thus over-parallelizes — acceptable bias for v1). Nested loops use a
/// fixed multiplier (`NESTED_LOOP_MULTIPLIER`) since the inner-trip
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

            // Inner loops: multiply by a fixed assumed trip-count.
            ExprKind::While {
                condition, body, ..
            } => {
                let c = self.estimate_expr(condition);
                let b = self.estimate_body(body);
                NESTED_LOOP_MULTIPLIER.saturating_mul(c.saturating_add(b))
            }
            ExprKind::WhileLet { value, body, .. } => {
                let v = self.estimate_expr(value);
                let b = self.estimate_body(body);
                NESTED_LOOP_MULTIPLIER.saturating_mul(v.saturating_add(b))
            }
            ExprKind::For { iterable, body, .. } => {
                let it = self.estimate_expr(iterable);
                let b = self.estimate_body(body);
                NESTED_LOOP_MULTIPLIER.saturating_mul(it.saturating_add(b))
            }
            ExprKind::Loop { body, .. } => {
                NESTED_LOOP_MULTIPLIER.saturating_mul(self.estimate_body(body))
            }

            // Blocks and other shape-passthrough nodes: cost of the contained block.
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) => {
                self.estimate_body(b)
            }
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

/// Nested-loop multiplier — `for i in body { for j in inner_body { ... } }`
/// inflates inner_body's cost by this factor under the assumption that
/// the inner loop iterates a non-trivial number of times. Real inner-
/// trip counts vary wildly; 16 picks a middle-ground that doesn't tank
/// the gate on small nested loops while still flagging genuinely
/// expensive bodies. Could be tuned per-bench later.
const NESTED_LOOP_MULTIPLIER: u64 = 16;

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
