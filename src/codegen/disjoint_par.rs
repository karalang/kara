//! Auto-par **indexed-write** fan-out codegen — the third compute-fan-out
//! shape, alongside parallel `let`-groups and associative reductions
//! (`design.md § 8876`).
//!
//! Hooked from `stmts.rs` right after the reduction attempt: when a loop
//! carries a `DisjointWriteLoop` tag whose per-iteration footprint proof
//! discharged (`src/index_disjoint.rs`, sub-slice 2), this module splits the
//! loop's iteration space across the worker pool. Each worker runs the *source
//! body verbatim* over a contiguous chunk of the outer loop variable; the
//! proof is what says two chunks can never write the same slot.
//!
//! ## Why this reuses `karac_par_reduce` rather than adding a runtime entry
//!
//! The reduce substrate already does exactly the required job: split
//! `[0, iter_total)` into contiguous per-worker chunks, invoke a codegen-emitted
//! `worker_fn(slot, start, end, ctx, cancel)` per chunk, join. The only piece
//! this shape does not need is the accumulator — so the descriptor gets a
//! degenerate 8-byte slot with an init that zeroes it and a **no-op combine**,
//! and the worker never touches it. Adding a `karac_par_for` symbol would have
//! bought nothing but a new ABI surface and an archive-rebuild flag day.
//!
//! ## How the writes land
//!
//! Captures travel through the same env-struct channel the reduction workers
//! use: by value for scalars and aggregates, by pointer for fixed-size arrays.
//! For a `Vec` / `Slice` write target that is exactly right — the by-value copy
//! duplicates the `{ptr, len, cap}` *header*, and the `ptr` field still points
//! at the one shared heap buffer, so `out[i] = v` in a worker stores into the
//! caller's memory. That works precisely because an index-assign never
//! reallocates; anything that could (`push`, a `mut ref` handoff to a callee)
//! is rejected upstream by the proof and its caller-applied gates, so no worker
//! can ever grow a buffer out from under its siblings.
//!
//! The representation is re-checked here rather than trusted from the analysis
//! (`disjoint_target_shares_storage`): a hash container's `m[k] = v` is spelled
//! like an element store, and a by-value capture of a `Map` control block would
//! not share storage the way a Vec header does. The analysis declines those too
//! — this is the backstop that keeps the *binary* right regardless.
//!
//! ## What is deliberately NOT enabled yet
//!
//! `KARAC_PAR_ORDER_FREE_FLAG` fits this shape by construction — slots stay at
//! identity and every iteration writes a footprint keyed by its own index, so
//! the runtime could legally chop the range into more chunks than workers and
//! let workers pull dynamically (the heterogeneity win on P/E-core hosts). It
//! is left off for this landing: it is a pure scheduling knob with no
//! correctness stake here, and turning it on belongs with the re-bench in the
//! Prism/Veil conversion sub-slice, where the effect is measurable rather than
//! asserted.
//!
//! `KARAC_AUTO_PAR=0` disables this path like every other auto-par lowering —
//! that is the A/B lever the differential harness sub-slice drives.

use crate::ast::{Block, StmtKind};
use crate::par_cost::{extract_loop_shape, fanout_verdict_with_cost};

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, IntType, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::reduce::block_has_early_exit;
use super::state::VarSlot;

impl<'ctx> super::Codegen<'ctx> {
    /// Try to lower the loop statement at `stmt_index` (inside `parent_body`)
    /// as a disjoint-indexed-write fan-out.
    ///
    /// `Ok(Some(()))` means lowered (the caller skips the sequential path);
    /// `Ok(None)` means "not this shape" and the caller falls through. Every
    /// `None` here is a *decline to parallelize*, never a miscompile — the
    /// sequential lowering is always correct.
    #[allow(clippy::result_large_err)]
    pub(super) fn try_emit_disjoint_write_lowering(
        &mut self,
        parent_body: &Block,
        stmt_index: usize,
    ) -> Result<Option<()>, String> {
        // `KARAC_AUTO_PAR=0` must disable every auto-par lowering, including
        // the ones reached from `compile_block` rather than the parallel-group
        // dispatch. This is the differential harness's A/B lever, so it has to
        // be honoured before anything else runs.
        if self.auto_par_disabled {
            return Ok(None);
        }

        let stmt = &parent_body.stmts[stmt_index];
        let StmtKind::Expr(stmt_expr) = &stmt.kind else {
            return Ok(None);
        };
        let tag = self
            .disjoint_write_loop_for_stmt(stmt_index, &stmt_expr.span)
            .cloned();
        let Some(tag) = tag else {
            return Ok(None);
        };
        // A declined proof is not a candidate. The reason is already on the
        // query surface; codegen just runs the loop sequentially.
        if !tag.proven() {
            return Ok(None);
        }

        let Some(shape) = extract_loop_shape(parent_body, stmt_index, stmt_expr) else {
            return Ok(None);
        };

        // Early exits would emit `ret <T>` inside a void worker fn — invalid
        // IR. The proof already declines them; this is the belt to that
        // suspenders, and it keeps the invariant local to the code that
        // depends on it.
        if block_has_early_exit(&shape.body) {
            return Ok(None);
        }

        // Same cost model the query reports, via the same entry point
        // (B-2026-07-29-33's whole point: one definition, two callers). A
        // second copy here would drift and make `karac query concurrency`
        // confidently wrong about its own binary.
        let bound_refs_param = self.expr_references_current_param(&shape.end_expr)
            || shape
                .lo_expr
                .as_ref()
                .is_some_and(|e| self.expr_references_current_param(e));
        // `per_iter_cost` also stamps the descriptor's runtime-gate field
        // below; taking it from the verdict call keeps the body from being
        // walked by the estimator twice.
        let (verdict, per_iter_cost) = fanout_verdict_with_cost(
            &shape.body,
            &shape.end_expr,
            shape.lo_expr.as_ref(),
            self.program_snapshot.as_deref(),
            bound_refs_param,
            // Indexed-write loops have no scalar accumulator; the type gate
            // does not apply (B-2026-07-31-14).
            true,
            Some(&shape.loop_var),
        );
        if !verdict.is_fanout() {
            return Ok(None);
        }

        // ── Every remaining gate that can decline runs BEFORE any IR is
        // emitted. ──
        //
        // Bailing after `compile_expr(&shape.end_expr)` would leave the bound's
        // evaluation in the parent's instruction stream and then hand the loop
        // to the sequential path, which compiles the bound again — evaluating a
        // side-effecting `for y in 0..next_row()` twice. Every check below is a
        // pure analysis over the AST and codegen's side tables, so the whole
        // decision is made before the first `build_*` call.
        let captures = self.collect_disjoint_captures(&shape.body, &shape.loop_var);
        let (runtime_captures, const_int_captures): super::reduce::ConstIntCapturePartition =
            self.partition_const_int_captures(&captures, parent_body, stmt_index);

        for t in &tag.targets {
            // The frame must have a slot for the target, and that slot's
            // representation must SHARE storage when captured — a `Vec`/`String`
            // header (`{ptr,len,cap}`), a slice (`{ptr,len}`), or a fixed-size
            // array (which travels by pointer). Anything else would give each
            // worker a private copy whose writes evaporate at join.
            let Some(slot) = self.variables.get(t.target.as_str()).copied() else {
                return Ok(None);
            };
            if !self.disjoint_target_shares_storage(t.target.as_str(), slot.ty) {
                return Ok(None);
            }
            // A target must also ride the RUNTIME capture channel: the const-int
            // partition materializes a capture as an LLVM constant inside the
            // worker, which for a buffer means writing into a private copy.
            // Targets are collections, never int literals, so this cannot fire
            // today — checked rather than assumed.
            if const_int_captures.iter().any(|(n, _, _)| n == &t.target)
                || !runtime_captures.contains(&t.target)
            {
                return Ok(None);
            }
        }

        // ── Past this point the lowering is committed and emits IR. ──
        let end_val = self.compile_expr(&shape.end_expr)?.into_int_value();
        let loop_var_int_ty = end_val.get_type();
        // Clamped to `max(_, 0)`: the descriptor's `iter_total` is a `u64`, and
        // an inverted range (`for y in 5..3`) would otherwise stamp a negative
        // count as ~2^64 and fan out over iterations the sequential loop never
        // runs. See `clamp_iter_total_nonneg`.
        let (iter_total_val, lo_val) = match &shape.lo_expr {
            None => (self.clamp_iter_total_nonneg(end_val), None),
            Some(lo_expr) => {
                let lo = self.compile_expr(lo_expr)?.into_int_value();
                if lo.get_type() != loop_var_int_ty {
                    // Unreachable given the typechecker's range unification —
                    // the same belt-and-suspenders gate `reduce.rs` keeps. It is
                    // the one bail left that has already emitted IR; if range
                    // unification ever stops guaranteeing this, the bound would
                    // be evaluated twice and this needs a pre-emission answer.
                    return Ok(None);
                }
                let total = self
                    .builder
                    .build_int_sub(end_val, lo, "disjoint.iter.total")
                    .unwrap();
                (self.clamp_iter_total_nonneg(total), Some(lo))
            }
        };

        let init_fn = self.emit_disjoint_init_fn();
        let combine_fn = self.emit_disjoint_combine_fn();
        let worker_fn = self.emit_disjoint_worker_fn(
            loop_var_int_ty,
            &shape.loop_var,
            &shape.body,
            &runtime_captures,
            &const_int_captures,
            lo_val.is_some(),
        )?;
        self.emit_disjoint_call(
            init_fn,
            worker_fn,
            combine_fn,
            iter_total_val,
            loop_var_int_ty,
            &runtime_captures,
            lo_val,
            per_iter_cost,
        )?;
        // Sibling of `KARAC_REDUCE_DEBUG`: one line per emitted fan-out, so a
        // build can be checked for "did this loop actually get a worker"
        // without reading the symbol table or the IR.
        if std::env::var("KARAC_DISJOINT_DEBUG").as_deref() == Ok("1") {
            eprintln!(
                "karac-disjoint-debug: fn={} stmt_index={} line={} loop_var={} targets=[{}] per_iter_cost={}",
                self.current_fn_name,
                stmt_index,
                stmt_expr.span.line,
                tag.loop_var,
                tag.targets
                    .iter()
                    .map(|t| format!("{}:{}", t.target, t.stride))
                    .collect::<Vec<_>>()
                    .join(","),
                per_iter_cost,
            );
        }
        Ok(Some(()))
    }

    /// Does a by-value capture of `ty` (or, for arrays, a by-pointer capture)
    /// still address the SAME storage the parent writes?
    ///
    /// This is the load-bearing question for the whole lowering. `Vec` /
    /// `String` (`{ptr,len,cap}`) and `Slice` (`{ptr,len}`) headers copy
    /// cheaply while their `ptr` field keeps pointing at one shared buffer, so
    /// a worker's `out[i] = v` reaches the caller's memory. A fixed-size array
    /// travels by pointer (B-2026-06-15-3) and is shared outright. Any other
    /// representation — a plain scalar, a by-value user struct — would give
    /// each worker a private copy whose writes evaporate at join, so it
    /// declines.
    fn disjoint_target_shares_storage(&self, name: &str, ty: BasicTypeEnum<'ctx>) -> bool {
        if matches!(ty, BasicTypeEnum::ArrayType(_)) {
            return true;
        }
        if self.llvm_ty_is_vec_struct(ty) {
            return true;
        }
        // Slice params/locals register their element type; their LLVM shape is
        // the 2-field `{ptr, i64}` struct.
        self.slice_elem_types.contains_key(name) && matches!(ty, BasicTypeEnum::StructType(_))
    }

    /// Capture set for a disjoint-write worker: every outer binding the body
    /// references, minus the loop variable.
    ///
    /// Unlike the reduction path there is no accumulator to exclude — the
    /// write targets ARE captures, and that is the point. `refs_in_block`
    /// records assignment-target roots as refs, so `out` in `out[i] = v` is
    /// collected without special casing.
    fn collect_disjoint_captures(&self, body: &Block, loop_var_name: &str) -> Vec<String> {
        // The empty accumulator name excludes nothing (no source binding is
        // named ""), which is exactly the difference from the reduction path.
        self.collect_reduction_captures(body, "", loop_var_name)
    }

    /// `void __karac_disjoint_init(*mut u8 slot)` — zero the 8-byte slot.
    ///
    /// The slot is vestigial: this shape has no accumulator, and the worker
    /// never writes it. It exists because `karac_par_reduce`'s ABI allocates
    /// one slot per worker and calls `init_slot` on each. Zeroing (rather than
    /// leaving it undef) keeps the memory defined for sanitizers.
    fn emit_disjoint_init_fn(&mut self) -> FunctionValue<'ctx> {
        let name = "__karac_disjoint_init";
        if let Some(existing) = self.module.get_function(name) {
            return existing;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_ty = self
            .context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::from(ptr_ty)], false);
        let f = self.module.add_function(name, fn_ty, None);
        let saved_bb = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let slot = f.get_nth_param(0).unwrap().into_pointer_value();
        self.builder.build_store(slot, i64_t.const_zero()).unwrap();
        self.builder.build_return(None).unwrap();
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// `void __karac_disjoint_combine(*mut u8 dst, *const u8 src)` — no-op.
    ///
    /// There are no partials to fold: each worker wrote its results straight
    /// into the shared buffer. The runtime still walks the combine chain, so
    /// the function must exist and must do nothing.
    fn emit_disjoint_combine_fn(&mut self) -> FunctionValue<'ctx> {
        let name = "__karac_disjoint_combine";
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
        self.builder.build_return(None).unwrap();
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Synthesize the per-chunk worker: run the source loop body for the outer
    /// loop variable over `[start, end)`.
    ///
    /// Structurally this is `emit_reduce_worker_fn` minus the accumulator (no
    /// identity-seeded alloca, no publish-to-slot at exit). It is written out
    /// rather than sharing that function because the two halves that must agree
    /// are *this* function and `emit_disjoint_call` — they emit and consume one
    /// env-struct layout — and coupling to the reduction path would make a
    /// change there able to silently corrupt this one. The env layout is
    /// deliberately identical (`[lo] + captures`) so the two are easy to read
    /// side by side.
    #[allow(clippy::result_large_err)]
    fn emit_disjoint_worker_fn(
        &mut self,
        loop_var_int_ty: IntType<'ctx>,
        loop_var_name: &str,
        body: &Block,
        captures: &[String],
        const_int_captures: &[(String, i64, Option<crate::token::IntSuffix>)],
        has_lo: bool,
    ) -> Result<FunctionValue<'ctx>, String> {
        let worker_id = self.par_counter;
        self.par_counter += 1;
        let name = format!("__karac_disjoint_worker_{worker_id}");

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_ty), // slot (unused)
                BasicMetadataTypeEnum::from(i64_t),  // start
                BasicMetadataTypeEnum::from(i64_t),  // end
                BasicMetadataTypeEnum::from(ptr_ty), // ctx
                BasicMetadataTypeEnum::from(ptr_ty), // cancel
            ],
            false,
        );
        let worker_fn = self.module.add_function(&name, fn_ty, None);

        // Save outer codegen state — the body compiles in a fresh function
        // context. Mirrors `emit_reduce_worker_fn`'s save/restore set exactly;
        // maps it does NOT take (`ref_params`, `slice_elem_types`,
        // `vec_elem_types`) stay visible so a captured collection's element
        // metadata still drives indexing, which is the same arrangement
        // reduction bodies already rely on for their reads.
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

        // Env-struct layout: `[lo] + captures`. Must match
        // `emit_disjoint_call`'s build order field for field.
        let env_struct_ty: Option<StructType<'ctx>> = if !has_lo && captures.is_empty() {
            None
        } else {
            let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(captures.len() + 1);
            if has_lo {
                field_tys.push(loop_var_int_ty.into());
            }
            for n in captures {
                let ty = saved_vars[n].ty;
                field_tys.push(if matches!(ty, BasicTypeEnum::ArrayType(_)) {
                    ptr_ty.into()
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
                .build_load::<BasicTypeEnum<'ctx>>(env_ty.into(), ctx_ptr, "__disjoint_env_load")
                .unwrap()
                .into_struct_value();
            let capture_field_base = if has_lo {
                let lo_field = self
                    .builder
                    .build_extract_value(env_val, 0, "__disjoint_lo")
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
                    // By-pointer array capture: the env field IS the parent's
                    // array address, so element stores land in shared storage —
                    // which is what makes an array a legal write target.
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

        // Loop variable: chunk-local `[start, end)` from the runtime, widened
        // or truncated to the source's int width, then shifted by `lo`.
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
        // Per-iteration cleanup frame, so a body-local `let` that owns heap
        // (an intermediate `Vec`, a `String`) drops each iteration instead of
        // piling up for the whole chunk. Same discipline as the reduction
        // worker and as `compile_for_range`.
        self.scope_cleanup_actions.push(Vec::new());
        self.compile_block(body)?;

        let current_bb = self.builder.get_insert_block().unwrap();
        if current_bb.get_terminator().is_none() {
            self.drain_top_frame_with_emit();
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
        } else {
            // Defensive: early exits are rejected upstream, so this is
            // unreachable today. Pop the per-iteration frame to keep the stack
            // balanced if a future shape admits it.
            self.scope_cleanup_actions.pop();
        }

        self.builder.position_at_end(exit_bb);
        // No slot publish — this shape has no accumulator. The worker's whole
        // output is the stores its body already made into the shared buffers.
        self.emit_scope_cleanup();
        self.builder.build_return(None).unwrap();

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

    /// Build the env struct + `KaracReduceDescriptor` in the parent frame and
    /// call `karac_par_reduce`. Nothing is read back afterwards: the workers'
    /// stores already landed in the caller's buffers.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::result_large_err)]
    fn emit_disjoint_call(
        &mut self,
        init_fn: FunctionValue<'ctx>,
        worker_fn: FunctionValue<'ctx>,
        combine_fn: FunctionValue<'ctx>,
        iter_total: IntValue<'ctx>,
        loop_var_int_ty: IntType<'ctx>,
        captures: &[String],
        lo_val: Option<IntValue<'ctx>>,
        per_iter_cost_units: u64,
    ) -> Result<(), String> {
        let parent_fn = self
            .current_fn
            .expect("emit_disjoint_call must run inside a function");
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // Env struct — layout mirrors `emit_disjoint_worker_fn`'s unpack.
        let env_ctx_ptr: PointerValue<'ctx> = if lo_val.is_none() && captures.is_empty() {
            ptr_ty.const_null()
        } else {
            let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(captures.len() + 1);
            if lo_val.is_some() {
                field_tys.push(loop_var_int_ty.into());
            }
            for n in captures {
                let ty = self.variables[n].ty;
                field_tys.push(if matches!(ty, BasicTypeEnum::ArrayType(_)) {
                    ptr_ty.into()
                } else {
                    ty
                });
            }
            let env_ty = self.context.struct_type(&field_tys, false);
            let env_alloca = self.create_entry_alloca(parent_fn, "__disjoint_env", env_ty.into());
            let mut env_agg = env_ty.get_undef();
            let capture_base = if let Some(lo) = lo_val {
                env_agg = self
                    .builder
                    .build_insert_value(env_agg, lo, 0, "__disjoint_env_lo")
                    .unwrap()
                    .into_struct_value();
                1
            } else {
                0
            };
            for (i, name) in captures.iter().enumerate() {
                let slot = self.variables[name];
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
                        "__disjoint_env_field",
                    )
                    .unwrap()
                    .into_struct_value();
            }
            self.builder.build_store(env_alloca, env_agg).unwrap();
            env_alloca
        };

        // Descriptor — same `#[repr(C)] KaracReduceDescriptor` layout the
        // reduction path stamps.
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
        let desc_alloca = self.create_entry_alloca(parent_fn, "__disjoint_desc", desc_ty.into());

        // Degenerate 8-byte slot: the runtime allocates one per worker and
        // runs init + combine over them. Nothing reads the result.
        let slot_size = i64_t.const_int(8, false);
        let slot_align = i64_t.const_int(8, false);

        // zext, not sext: `iter_total` is a non-negative count, and the
        // descriptor field is u64 (see the runtime's field-width note).
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
        desc_agg = self
            .builder
            .build_insert_value(
                desc_agg,
                init_fn.as_global_value().as_pointer_value(),
                3,
                "d.init_slot",
            )
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(
                desc_agg,
                worker_fn.as_global_value().as_pointer_value(),
                4,
                "d.worker_fn",
            )
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(
                desc_agg,
                combine_fn.as_global_value().as_pointer_value(),
                5,
                "d.combine_fn",
            )
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(desc_agg, env_ctx_ptr, 6, "d.ctx")
            .unwrap()
            .into_struct_value();
        desc_agg = self
            .builder
            .build_insert_value(
                desc_agg,
                i64_t.const_int(per_iter_cost_units, false),
                7,
                "d.per_iter_cost",
            )
            .unwrap()
            .into_struct_value();
        self.builder.build_store(desc_alloca, desc_agg).unwrap();

        let out_slot = self.create_entry_alloca(parent_fn, "__disjoint_out", i64_t.into());
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
        Ok(())
    }
}
