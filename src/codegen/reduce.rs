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
//! ## v1 narrow shape (slice 3b)
//!
//! - Source loop: `for k in 0..hi { ... }` only (no `while`, no
//!   non-zero `lo` — those land in 3b.1).
//! - Op: `+` only. The other allow-listed ops (`*`, `|`, `&`, `^`) need
//!   one new (init, combine) specialization each — additive follow-up.
//! - Accumulator type: i64 only.  Same extension story as the op set.
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

use std::collections::HashSet;

use crate::ast::{Block, Expr, ExprKind, PatternKind, Stmt, StmtKind};
use crate::concurrency::{LoopReduction, ReductionOp};

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, StructType};
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::state::VarSlot;

impl<'ctx> super::Codegen<'ctx> {
    /// Try to lower the top-level statement at `stmt_index` as a recognized
    /// reduction. Returns `Ok(Some(()))` if the statement was lowered (the
    /// caller skips the normal stmt-compile path); `Ok(None)` if the shape
    /// is outside the v1 supported set and the caller should fall back to
    /// sequential codegen. `Err(_)` propagates a codegen error from inside
    /// the worker-fn synthesis.
    #[allow(clippy::result_large_err)]
    pub(super) fn try_emit_reduction_lowering(
        &mut self,
        stmt_index: usize,
        stmt: &Stmt,
    ) -> Result<Option<()>, String> {
        let reduction = self.loop_reduction_for_stmt(stmt_index).cloned();
        let Some(reduction) = reduction else {
            return Ok(None);
        };

        // v1 only supports `+`. Other allow-list ops fall through — the
        // analyzer's tag is preserved for the follow-up specialization.
        if reduction.op != ReductionOp::Add {
            return Ok(None);
        }

        // Unpack the loop expression. v1 only handles `for k in 0..hi`.
        let StmtKind::Expr(expr) = &stmt.kind else {
            return Ok(None);
        };
        let ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } = &expr.kind
        else {
            return Ok(None);
        };
        let PatternKind::Binding(loop_var_name) = &pattern.kind else {
            return Ok(None);
        };
        let ExprKind::Range {
            start,
            end,
            inclusive: false,
        } = &iterable.kind
        else {
            return Ok(None);
        };
        let Some(end_expr) = end else {
            return Ok(None);
        };
        // v1 requires `lo == 0` so the worker's chunk-local index doubles
        // as the source-level `k`. Non-zero starts land in 3b.1 by
        // threading the offset through ctx and adding it at use sites.
        let lo_is_zero = match start {
            None => true,
            Some(s) => matches!(s.kind, ExprKind::Integer(0, _)),
        };
        if !lo_is_zero {
            return Ok(None);
        }

        // Verify the accumulator's lowered type is i64. The reduction's
        // identity (0 for `+`) and combine op are type-specialized; other
        // int widths add one (init, combine) pair each.
        let Some(acc_slot) = self.variables.get(&reduction.accumulator).copied() else {
            return Ok(None);
        };
        let i64_t: BasicTypeEnum<'ctx> = self.context.i64_type().into();
        if acc_slot.ty != i64_t {
            return Ok(None);
        }

        // Early exits in the body would cross the worker-fn boundary and
        // generate a `ret <T>` inside a void worker fn → invalid IR.
        // Mirrors the analyzer's existing `stmt_has_early_exit` rule
        // applied to par-group siblings.
        if block_has_early_exit(body) {
            return Ok(None);
        }

        // Compile the end bound in the parent context. This is `iter_total`
        // for the runtime (since lo == 0). Must be i64 — the descriptor
        // and worker fn both treat the iteration index space as i64.
        let end_val = self.compile_expr(end_expr)?.into_int_value();

        // Synthesize the per-(op, type) helper functions.
        let init_fn = self.emit_reduce_init_fn_add_i64();
        let combine_fn = self.emit_reduce_combine_fn_add_i64();

        // Capture set for the worker fn: variables the body reads that
        // aren't the accumulator, aren't the loop variable, and aren't
        // introduced inside the body itself. Filtered to live entries in
        // `self.variables` so module-level functions, struct names, etc.
        // (which `refs_in_block` doesn't distinguish) drop out cleanly.
        let captures = self.collect_reduction_captures(body, &reduction.accumulator, loop_var_name);

        let worker_fn = self.emit_reduce_worker_fn(&reduction, loop_var_name, body, &captures)?;

        self.emit_reduce_call(
            init_fn, worker_fn, combine_fn, end_val, acc_slot, &reduction, &captures,
        )?;

        Ok(Some(()))
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

    /// `void init_slot(*mut u8 slot) { *(i64*)slot = 0; }` — Add identity
    /// for i64. Cached per module via the LLVM symbol table (re-adding
    /// the same name returns the existing function), so multiple
    /// reduction sites share one definition.
    fn emit_reduce_init_fn_add_i64(&mut self) -> FunctionValue<'ctx> {
        let name = "__karac_reduce_init_add_i64";
        if let Some(existing) = self.module.get_function(name) {
            return existing;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_ty = self
            .context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::from(ptr_ty)], false);
        let f = self.module.add_function(name, fn_ty, None);

        let saved_bb = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let slot_ptr = f.get_nth_param(0).unwrap().into_pointer_value();
        self.builder
            .build_store(slot_ptr, self.context.i64_type().const_zero())
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// `void combine(*mut u8 dst, *const u8 src) { *(i64*)dst += *(i64*)src; }`
    /// — Add fold for i64. Same caching pattern as `emit_reduce_init_fn_add_i64`.
    fn emit_reduce_combine_fn_add_i64(&mut self) -> FunctionValue<'ctx> {
        let name = "__karac_reduce_combine_add_i64";
        if let Some(existing) = self.module.get_function(name) {
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
        let f = self.module.add_function(name, fn_ty, None);

        let saved_bb = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let i64_t = self.context.i64_type();
        let dst_ptr = f.get_nth_param(0).unwrap().into_pointer_value();
        let src_ptr = f.get_nth_param(1).unwrap().into_pointer_value();
        let d = self
            .builder
            .build_load(i64_t, dst_ptr, "d")
            .unwrap()
            .into_int_value();
        let s = self
            .builder
            .build_load(i64_t, src_ptr, "s")
            .unwrap()
            .into_int_value();
        let sum = self.builder.build_int_add(d, s, "sum").unwrap();
        self.builder.build_store(dst_ptr, sum).unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
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
        loop_var_name: &str,
        body: &Block,
        captures: &[String],
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

        // Unpack captures from the env-struct pointed to by ctx into
        // fresh local allocas, rebinding `self.variables` so the body
        // resolves capture reads to the worker's local copy.
        let env_struct_ty: Option<StructType<'ctx>> = if captures.is_empty() {
            None
        } else {
            let field_tys: Vec<BasicTypeEnum<'ctx>> =
                captures.iter().map(|n| saved_vars[n].ty).collect();
            Some(self.context.struct_type(&field_tys, false))
        };

        if let Some(env_ty) = env_struct_ty {
            let ctx_ptr = worker_fn.get_nth_param(3).unwrap().into_pointer_value();
            let env_val = self
                .builder
                .build_load::<BasicTypeEnum<'ctx>>(env_ty.into(), ctx_ptr, "__reduce_env_load")
                .unwrap()
                .into_struct_value();
            for (i, var_name) in captures.iter().enumerate() {
                let cap_ty = saved_vars[var_name].ty;
                let field_val = self
                    .builder
                    .build_extract_value(env_val, i as u32, var_name)
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

        // Allocate the worker-local accumulator at identity. Add identity
        // for i64 is 0 — extending to other ops/types means picking the
        // right constant per (op, type) pair here.
        let acc_alloca = self.create_entry_alloca(worker_fn, &reduction.accumulator, i64_t.into());
        self.builder
            .build_store(acc_alloca, i64_t.const_zero())
            .unwrap();
        self.variables.insert(
            reduction.accumulator.clone(),
            VarSlot {
                ptr: acc_alloca,
                ty: i64_t.into(),
            },
        );

        // Allocate the loop variable, init to `start`. The body sees
        // `<loop_var>` as a plain mutable i64 alloca; the increment runs
        // in the bottom of `loop.body` (between body emission and the
        // back-edge), so a body-internal read of `<loop_var>` observes
        // the current iteration's value.
        let start_val = worker_fn.get_nth_param(1).unwrap().into_int_value();
        let end_val = worker_fn.get_nth_param(2).unwrap().into_int_value();
        let k_alloca = self.create_entry_alloca(worker_fn, loop_var_name, i64_t.into());
        self.builder.build_store(k_alloca, start_val).unwrap();
        self.variables.insert(
            loop_var_name.to_string(),
            VarSlot {
                ptr: k_alloca,
                ty: i64_t.into(),
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
            .build_load(i64_t, k_alloca, "k")
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
                .build_load(i64_t, k_alloca, "k.cur")
                .unwrap()
                .into_int_value();
            let k_next = self
                .builder
                .build_int_add(k_cur, i64_t.const_int(1, false), "k.next")
                .unwrap();
            self.builder.build_store(k_alloca, k_next).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();
        }

        self.builder.position_at_end(exit_bb);
        // Publish the worker's partial to the caller's slot.
        let final_acc = self
            .builder
            .build_load(i64_t, acc_alloca, "acc.final")
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
        _reduction: &LoopReduction,
        captures: &[String],
    ) -> Result<(), String> {
        let parent_fn = self
            .current_fn
            .expect("emit_reduce_call must run inside a function");
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // Build the env-struct in the parent frame and copy captures.
        let env_ctx_ptr: PointerValue<'ctx> = if captures.is_empty() {
            // Null ctx — the runtime passes it through to worker_fn
            // unchanged, and the worker's capture-unpack code path is
            // skipped because env_struct_ty was None at synthesis.
            ptr_ty.const_null()
        } else {
            let field_tys: Vec<BasicTypeEnum<'ctx>> =
                captures.iter().map(|n| self.variables[n].ty).collect();
            let env_ty = self.context.struct_type(&field_tys, false);
            let env_alloca = self.create_entry_alloca(parent_fn, "__reduce_env", env_ty.into());
            let mut env_agg = env_ty.get_undef();
            for (i, name) in captures.iter().enumerate() {
                let slot = self.variables[name];
                let val = self.builder.build_load(slot.ty, slot.ptr, name).unwrap();
                env_agg = self
                    .builder
                    .build_insert_value(env_agg, val, i as u32, "__reduce_env_field")
                    .unwrap()
                    .into_struct_value();
            }
            self.builder.build_store(env_alloca, env_agg).unwrap();
            env_alloca
        };

        // Build the descriptor struct.  Layout matches `runtime/src/lib.rs`'s
        // `#[repr(C)] KaracReduceDescriptor`: i64 iter_total + i64 slot_size +
        // i64 slot_align + ptr init + ptr worker + ptr combine + ptr ctx.
        let desc_ty = self.context.struct_type(
            &[
                i64_t.into(),  // iter_total
                i64_t.into(),  // slot_size
                i64_t.into(),  // slot_align
                ptr_ty.into(), // init_slot
                ptr_ty.into(), // worker_fn
                ptr_ty.into(), // combine_fn
                ptr_ty.into(), // ctx
            ],
            false,
        );
        let desc_alloca = self.create_entry_alloca(parent_fn, "__reduce_desc", desc_ty.into());

        let slot_size = i64_t.const_int(8, false); // i64
        let slot_align = i64_t.const_int(8, false);

        // Widen iter_total to i64 if the source's `end` evaluated to a
        // narrower int — the descriptor field is usize (i64 on 64-bit).
        let iter_total_widened = if iter_total.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(iter_total, i64_t, "iter.widen")
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
        self.builder.build_store(desc_alloca, desc_agg).unwrap();

        // Allocate the out_slot in the parent frame. The runtime writes
        // the reduced value here before returning; the parent then loads
        // it back into the source-level accumulator's alloca.
        let out_slot = self.create_entry_alloca(parent_fn, "__reduce_out", i64_t.into());

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

        // Load the reduced value and store into the source-level
        // accumulator's alloca. Subsequent reads of the source-level
        // accumulator (`println(sum)`) see the reduced value.
        let reduced = self.builder.build_load(i64_t, out_slot, "reduced").unwrap();
        self.builder.build_store(acc_slot.ptr, reduced).unwrap();

        Ok(())
    }
}

/// Mirror of `crate::concurrency::block_has_early_exit` reachable from
/// codegen. Cheap structural walk — returns true iff the block contains
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
