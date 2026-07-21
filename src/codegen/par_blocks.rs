//! `par {}` block lowering — branch-fn synthesis + runtime spawn.
//!
//! Houses `compile_par_block` (the entry point), `emit_par_run`
//! (which builds the `KaracBranch[]` array and emits the
//! `karac_par_run` call), `emit_par_branch_fn` (the per-branch
//! synthesized fn body), and `emit_branch_cancel_check` (the
//! cooperative-cancel atomic-load emitted before each call site
//! inside a par-branch body).

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::token::Span;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use super::state::{CleanupAction, ResultSlot, ReturnSlot, SlotOwnership, VarSlot};

/// Slice 1b/2 (Phase 7 — Par codegen: cancellation and error
/// propagation, 2026-05-20 / 2026-05-21) return type for
/// `emit_par_run`. First element is the return-slot map (slice A);
/// second is the parent-allocated Result-surface — Result-slot array
/// pointer + per-slot struct type + earliest-err-idx `i32` pointer
/// (sentinel `u32::MAX` = no err). `Some` only when at least one
/// branch is Result-typed.
type ParRunResult<'ctx> = (
    HashMap<String, BasicValueEnum<'ctx>>,
    Option<ParResultSurface<'ctx>>,
    // Per-slot ownership metadata for bindings whose branch-side
    // cleanup action was removed because the value moved to the
    // parent through the return slot — the rebinding sites
    // re-register cleanup against the parent's alloca from these
    // records (see `SlotOwnership`). Empty for the 0-/1-stmt
    // sequential fast paths (the binding compiles directly in the
    // parent frame there, so ownership never leaves it).
    HashMap<String, SlotOwnership<'ctx>>,
);

/// Parent-allocated state surfaced from `emit_par_run` to
/// `compile_par_block` when at least one branch in the par-block is
/// Result-typed. Slice 2 (2026-05-21) added `earliest_err_idx_ptr`:
/// branch fns do `atomicrmw umin` against this slot on Err detect,
/// and the parent loads it once after `karac_par_run` returns to pick
/// the source-order winner without a per-slot tag walk.
#[derive(Clone, Copy)]
pub(super) struct ParResultSurface<'ctx> {
    pub slots_ptr: PointerValue<'ctx>,
    pub slot_struct_ty: StructType<'ctx>,
    pub earliest_err_idx_ptr: PointerValue<'ctx>,
}

impl<'ctx> super::Codegen<'ctx> {
    /// Slice 1a (Phase 7 — Par codegen: cancellation and error
    /// propagation, 2026-05-18). Recognise a par-block branch whose
    /// statement is `let <name>: Result[T, E] = <expr>;` and return
    /// the binding name. Used by `compile_par_block` to build the
    /// `ResultSlot` list — each named binding's value is copied into
    /// a parent-allocated Result-slot before the branch returns, and
    /// the cancel flag is flipped on `Err` so siblings' cooperative
    /// cancel checks fire.
    ///
    /// Scope is annotation-only in slice 1a: only let-statements
    /// carrying an explicit `Result[...]` path annotation match.
    /// Inferred Result types (`let r = maybe_fail();`) fold in
    /// alongside slice 2's typechecker-side hooks. Returns `None`
    /// True for the INLINE-value concurrency primitives whose par captures
    /// must be shared by pointer rather than copied by value (B-2026-07-18-28):
    /// `Atomic[T]` (a bare `T` cell) and `Mutex[T]` (`{ i64 flag, T }`). Both
    /// live directly in the binding's stack slot, so a by-value env copy gives
    /// each branch a private cell whose RMW / lock is invisible to the parent
    /// and to sibling branches. `Arc`/`shared` capture is handled separately by
    /// `ParCaptureMode::SharedRc` (already a shared heap pointer), and plain
    /// `let mut` captures are rejected upstream by the concurrency checker
    /// (B-2026-07-18-27) — so this set is exactly the two by-pointer cells.
    /// The classification is by codegen's `var_type_names` surface tag, the
    /// same tag `resolve_atomic_storage` / the `lock` lowering dispatch on.
    fn is_par_shared_cell_type(type_name: Option<&str>) -> bool {
        matches!(type_name, Some("Atomic") | Some("Mutex"))
    }

    /// for non-let statements and for lets without a Result-shaped
    /// annotation.
    fn branch_result_binding_name(stmt: &Stmt) -> Option<String> {
        let (pattern, ty_opt) = match &stmt.kind {
            StmtKind::Let { pattern, ty, .. } | StmtKind::LetElse { pattern, ty, .. } => {
                (pattern, ty.as_ref())
            }
            _ => return None,
        };
        let PatternKind::Binding(name) = &pattern.kind else {
            return None;
        };
        let ty = ty_opt?;
        if let TypeKind::Path(path) = &ty.kind {
            if path.segments.len() == 1 && path.segments[0] == "Result" {
                return Some(name.clone());
            }
        }
        None
    }

    /// Compile a `par {}` block by spawning each stmt as a per-branch
    /// fn, building a `KaracBranch[]` array, and handing it to
    /// `karac_par_run`. Each branch fn is given a fresh stack ctx that
    /// captures any outer bindings it reads (and writes them back through
    /// caller-allocated return slots when applicable).
    ///
    /// **Block-result semantics (design.md § Explicit Concurrency).**
    /// `par {}` is a block expression: its value is the value of the
    /// last expression, exactly like any other block. The canonical
    /// shape from `docs/syntax.md § 5.9 Parallel Blocks` and
    /// `docs/design.md § Explicit Concurrency` is:
    ///
    /// ```kara
    /// let (a, b) = par {
    ///     let p = fetch_profile(uid)
    ///     let o = fetch_orders(uid)
    ///     (p, o)
    /// }
    /// ```
    ///
    /// Each top-level statement is a concurrent branch; the final
    /// expression `(p, o)` is the join expression that combines the
    /// per-branch results. For the join expression to see `p` / `o`
    /// the branches' let-introduced bindings must escape to the
    /// surrounding scope across the join barrier.
    ///
    /// **Bug #6 fix (2026-05-16).** Pre-fix this passed an empty slot
    /// list, so let-bindings inside the par-block branches stayed
    /// branch-local and the final expression's read of them errored
    /// with "Undefined variable 'p'" at codegen. Fix: walk
    /// `block.final_expr` for references to let-introduced names in
    /// the branches, materialize a `ReturnSlot` per matching binding
    /// (sibling to the auto-par dispatch site's
    /// `compute_return_slots_checked`), thread them through
    /// `emit_par_run`, and bind each returned slot value as a fresh
    /// parent-scope local before compiling the join expression.
    ///
    /// Bindings whose let-RHS has an un-inferrable LLVM type
    /// (closures, monomorphized bodies not yet declared) are dropped
    /// from the slot list — the branch still runs concurrently but
    /// the binding stays branch-local, and any join-expression read
    /// of that name surfaces an "Undefined variable" diagnostic.
    /// Matches the auto-par site's conservative fallback.
    ///
    /// When `block.final_expr` is `None` or references no
    /// branch-defined names, the slot list is empty and the IR is
    /// byte-identical to slice 2's pre-fix shape — only the
    /// final-expression compile-and-return is added.
    #[allow(clippy::result_large_err)]
    pub(super) fn compile_par_block(
        &mut self,
        block: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Step 1 — Identify branch-local let-introduced bindings and the
        // branch index that defines each. Branches are top-level
        // statements in source order; statement index = branch index for
        // the explicit-par path (no sorting like the auto-par dispatch
        // site does — the branches ARE the statements in order).
        let mut defined: HashMap<String, (usize, &Stmt)> = HashMap::new();
        for (branch_idx, stmt) in block.stmts.iter().enumerate() {
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    if let PatternKind::Binding(name) = &pattern.kind {
                        defined.insert(name.clone(), (branch_idx, stmt));
                    }
                }
                _ => {}
            }
        }

        // Step 2 — The join barrier hoists EVERY top-level branch `let`
        // binding into the surrounding scope (B-2026-07-11-3), so each one
        // becomes a return slot — not just names read by an optional tail
        // expression. This is the shape `par { let a = f(); let b = g(); }
        // (a, b)` needs: the consuming code sits AFTER the block, invisible
        // to `block.final_expr`, so a final-expr-only read set missed it and
        // the binding never got a slot. A binding that turns out unused
        // after the block is simply an unused local (its value still
        // transfers out and drops at the enclosing scope's end, like any
        // other `let`). Slots are sorted by (branch index, name) for a
        // deterministic layout, matching the auto-par dispatch's ordering.
        let mut names_with_branch: Vec<(usize, String, &Stmt)> = defined
            .into_iter()
            .map(|(name, (branch_idx, stmt))| (branch_idx, name, stmt))
            .collect();
        names_with_branch.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let mut return_slots: Vec<super::state::ReturnSlot<'ctx>> = Vec::new();
        for (branch_idx, name, stmt) in names_with_branch {
            if let Some(llvm_ty) = self.infer_let_binding_llvm_type(stmt) {
                return_slots.push(super::state::ReturnSlot {
                    binding_name: name,
                    branch_index: branch_idx,
                    llvm_ty,
                    var_type_name: Self::let_binding_annotation_type_name(stmt),
                });
            }
            // Un-inferrable RHS: conservatively drop. Sibling to the
            // auto-par site's behavior. Reads in the join expression
            // will fall through to the standard "Undefined variable"
            // diagnostic.
        }

        // Step 4 (slice 1a — Par codegen: cancellation and error
        // propagation, 2026-05-18) — Build the `ResultSlot` list for
        // branches whose let-statement carries an explicit
        // `Result[T, E]` type annotation. Each such branch will write
        // its terminal Result value into a parent-allocated slot before
        // returning, and on `Err` (tag == 0) also store `true` into the
        // per-call cancel flag so sibling branches' cooperative cancel
        // checks fire. Detection scope for slice 1a is annotation-only
        // — broader inference folds in alongside slice 2's typechecker
        // hooks. `array_index` is the slot's position in the
        // parent-allocated dense `[N_results x Result_t_e]` array
        // (non-result branches don't consume an index).
        let mut result_slots: Vec<ResultSlot> = Vec::new();
        for (branch_idx, stmt) in block.stmts.iter().enumerate() {
            if let Some(name) = Self::branch_result_binding_name(stmt) {
                let array_index = result_slots.len();
                result_slots.push(ResultSlot {
                    binding_name: name,
                    branch_index: branch_idx,
                    array_index,
                });
            }
        }

        // Step 5 — Run the branches. `emit_par_run` allocates a
        // parent-side return struct (one field per slot), passes its
        // pointer through the per-branch env struct, and each branch
        // fn writes its slot's value before returning. The barrier
        // inside `karac_par_run` guarantees all writes are visible by
        // the time the runtime call returns. Slice 1b/2: emit_par_run
        // additionally returns the parent-allocated `ParResultSurface`
        // when any branch is Result-typed — slot array pointer + slot
        // struct type + the `i32` "earliest err idx" cell that branch
        // fns CAS-min'd into. Step 7 uses this to surface the
        // source-order first Err without walking every slot tag.
        let (slot_values, result_surface, slot_ownership) =
            self.emit_par_run(&block.stmts, &block.span, &return_slots, &result_slots)?;

        // Step 6 — Bind each loaded slot value as a fresh local in the
        // surrounding scope. Mirrors the auto-par dispatch site's
        // bind-back step (`compile_function_body` ~line 189) so the
        // join expression's identifier reads resolve through
        // `self.variables` just like any other in-scope local.
        if let Some(parent_fn) = self.current_fn {
            let vec_st: BasicTypeEnum<'ctx> = self.vec_struct_type().into();
            for slot in &return_slots {
                if let Some(loaded) = slot_values.get(&slot.binding_name) {
                    let alloca =
                        self.create_entry_alloca(parent_fn, &slot.binding_name, slot.llvm_ty);
                    self.builder.build_store(alloca, *loaded).unwrap();
                    self.variables.insert(
                        slot.binding_name.clone(),
                        super::state::VarSlot {
                            ptr: alloca,
                            ty: slot.llvm_ty,
                        },
                    );
                    // Preserve narrow-unsigned signedness across the join —
                    // `i8` erases `u8` (B-2026-07-03-21); mirrors the auto-par
                    // dispatch site.
                    if let Some(tn) = &slot.var_type_name {
                        self.record_var_type_name(slot.binding_name.clone(), tn.clone());
                    }
                    // Vec/String slot: register a placeholder element
                    // type so subsequent `.len()` / `.is_empty()` etc.
                    // dispatch through `compile_vec_method`. The
                    // `or_insert` guard preserves any pre-existing
                    // annotation registered before the par-block fired
                    // (mirrors the auto-par dispatch path).
                    if slot.llvm_ty == vec_st {
                        self.vec_elem_types
                            .entry(slot.binding_name.clone())
                            .or_insert_with(|| self.context.i64_type().into());
                        // B-2026-07-02-4 (explicit-par sibling of the
                        // auto-par dispatch fix in stmts.rs): a Vec/String
                        // slot's heap crossed the par boundary with NO
                        // parent-side cleanup at all — both slots' entire
                        // contents and buffers leaked per invocation.
                        // Register the same rich element dispatch the
                        // LET site uses (agg/map/tensor element drops,
                        // one-level fast path otherwise).
                        let elem_ty = self.vec_elem_types.get(slot.binding_name.as_str()).copied();
                        let elem_te = self
                            .var_elem_type_exprs
                            .get(slot.binding_name.as_str())
                            .cloned();
                        let is_tensor_elem = elem_te
                            .as_ref()
                            .map(|te| self.tensor_var_info_from_type_expr(te).is_some())
                            .unwrap_or(false);
                        let map_elem_drop = elem_te
                            .as_ref()
                            .and_then(|te| self.vec_elem_map_drop_for_type_expr(te));
                        let agg_elem_drop = elem_te
                            .as_ref()
                            .and_then(|te| self.vec_elem_agg_drop_for_type_expr(te));
                        if is_tensor_elem {
                            self.track_vec_of_tensors_var(alloca);
                        } else if let Some(map_drop) = map_elem_drop {
                            self.track_vec_of_maps_var(alloca, map_drop);
                        } else if let (Some(agg_drop), Some(et)) = (agg_elem_drop, elem_ty) {
                            self.track_vec_of_aggs_var(alloca, et, agg_drop);
                        } else {
                            self.track_vec_var(alloca, elem_ty);
                        }
                    }
                    // Moved-in ownership (Map / File / enum / struct /
                    // user-Drop / SoA slots): the branch removed its
                    // cleanup action when it published the value — the
                    // parent is now the unique owner, so re-register
                    // the equivalent action against the parent alloca
                    // (mirrors the auto-par dispatch site).
                    self.register_slot_ownership(&slot.binding_name, alloca, &slot_ownership);
                }
            }
        }

        // Step 7 — Compile the join expression. The block-result
        // semantics dictate the par-block's value is the join's value.
        // When there is no final expression, the par-block evaluates
        // to unit (i64 0) — preserves the pre-fix behavior for
        // statement-form par-blocks (`par { side_effect_a();
        // side_effect_b(); }`).
        //
        // Slice 1b (2026-05-20) introduced a per-slot tag-walk: chain
        // of N `__par_err_check_<i>` blocks each loading the tag of
        // slot `i` and branching on Err. Slice 2 (Phase 7 — Par
        // codegen: cancellation and error propagation, 2026-05-21)
        // replaces the walk with a single load of the parent-
        // allocated `__par_earliest_err_idx` cell that branch fns
        // CAS-min'd into on Err detect. Source-order semantics fall
        // out for free: each branch's `array_index` matches its
        // source-order branch index (slots are assigned in source
        // order in step 4), and `atomicrmw umin` keeps the smallest.
        // The cell's sentinel value `u32::MAX` means "no branch
        // erred"; any value strictly less is a valid index into the
        // `[N x Result_struct_ty]` slot array.
        //
        // IR shape: one `__par_err_check` BB (load idx, compare with
        // sentinel, br to `__par_err_found` or `__par_compile_join`),
        // one `__par_err_found` BB (GEP slot at loaded idx, load the
        // Result, br to exit), `__par_compile_join` (evaluate user
        // join), `__par_block_exit` (2-incoming phi). The phi type
        // matches the shared Result struct (`{ i64, i64 }`); the join
        // expression is assumed to produce a value of the same LLVM
        // type — a typechecker hook to enforce this at the source
        // level remains a follow-up.
        //
        // Skip the err-walk machinery when there are no Result slots
        // OR no join expression — for either, slice 1a's behavior is
        // already correct (slot-write + Err-triggers-cancel cascades
        // to siblings, no phi value to surface).
        let has_result_slots = result_surface.is_some() && block.final_expr.is_some();
        if !has_result_slots {
            return if let Some(expr) = &block.final_expr {
                self.compile_expr(expr)
            } else {
                Ok(self.context.i64_type().const_int(0, false).into())
            };
        }

        let surface = result_surface.expect("has_result_slots gates on Some(_)");
        let slots_ptr = surface.slots_ptr;
        let result_st = surface.slot_struct_ty;
        let earliest_err_idx_ptr = surface.earliest_err_idx_ptr;
        let join_expr = block
            .final_expr
            .as_ref()
            .expect("has_result_slots gates on Some(_)");
        let parent_fn = self.current_fn.expect("par-block must be inside a fn");
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let zero_i64 = i64_t.const_zero();
        let sentinel = i32_t.const_int(u32::MAX as u64, false);

        // BB skeleton: one check, one err_found, the compile_join,
        // and the exit. Allocate up-front so forward branches resolve.
        let check_bb = self
            .context
            .append_basic_block(parent_fn, "__par_err_check");
        let err_found_bb = self
            .context
            .append_basic_block(parent_fn, "__par_err_found");
        let join_bb = self
            .context
            .append_basic_block(parent_fn, "__par_compile_join");
        let exit_bb = self
            .context
            .append_basic_block(parent_fn, "__par_block_exit");

        // Jump from the current BB (where step 6's let-bindings just
        // landed) into the check.
        self.builder.build_unconditional_branch(check_bb).unwrap();

        // check BB: load the earliest-err-idx cell once, compare with
        // the sentinel, branch. The load is unordered/monotonic
        // because the `karac_par_run` join barrier already provides a
        // happens-before edge from every branch's atomicrmw to here.
        self.builder.position_at_end(check_bb);
        let earliest_err_idx = self
            .builder
            .build_load(i32_t, earliest_err_idx_ptr, "__par_earliest_err_idx")
            .unwrap()
            .into_int_value();
        let any_err = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                earliest_err_idx,
                sentinel,
                "__par_any_err",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(any_err, err_found_bb, join_bb)
            .unwrap();

        // err_found BB: GEP into slots_ptr[earliest_err_idx] and load
        // the full Result. Slot indices stored in the cell are u32;
        // zext to i64 for the GEP index operand.
        self.builder.position_at_end(err_found_bb);
        let arr_ty = result_st.array_type(0);
        let idx_i64 = self
            .builder
            .build_int_z_extend(earliest_err_idx, i64_t, "__par_err_idx_i64")
            .unwrap();
        let slot_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(
                    arr_ty,
                    slots_ptr,
                    &[zero_i64, idx_i64],
                    "__par_err_slot_ptr",
                )
                .unwrap()
        };
        let err_val = self
            .builder
            .build_load(result_st, slot_ptr, "__par_err_slot_val")
            .unwrap();
        let err_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        // compile_join BB: evaluate the user's join expression. The
        // builder's insertion block may advance through nested control
        // flow during `compile_expr`; capture the *final* block and
        // its produced value, then branch to exit.
        self.builder.position_at_end(join_bb);
        let join_val = self.compile_expr(join_expr)?;
        let join_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        // Exit BB: 2-incoming phi (err-found, join). The phi's type
        // matches the shared Result struct layout (`{ i64, i64 }`).
        self.builder.position_at_end(exit_bb);
        let phi = self
            .builder
            .build_phi(result_st, "__par_block_value")
            .unwrap();
        phi.add_incoming(&[(&err_val, err_end_bb), (&join_val, join_end_bb)]);
        Ok(phi.as_basic_value())
    }

    /// Re-register a slot binding's transferred cleanup against the
    /// PARENT's fresh alloca. Counterpart of the branch-side action
    /// removal in `emit_par_branch_fn` — together they implement
    /// move-only slot semantics for ownership-bearing slot types
    /// (Map / File / value-enum / value-struct / user-Drop / SoA):
    /// the branch publishes and forgets; the parent owns and frees
    /// exactly once at its scope exit. Shared by both rebinding
    /// sites (`compile_par_block` Step 6 and the auto-par dispatch
    /// in `compile_function_body`). No-op for bindings without a
    /// transfer record (i64 / f64 / Vec / RC slots — the latter two
    /// have their own suppression/track flows).
    pub(super) fn register_slot_ownership(
        &mut self,
        binding_name: &str,
        parent_alloca: PointerValue<'ctx>,
        slot_ownership: &HashMap<String, SlotOwnership<'ctx>>,
    ) {
        let Some(transfer) = slot_ownership.get(binding_name) else {
            return;
        };
        let action = match *transfer {
            SlotOwnership::Map {
                key_is_vec,
                val_is_vec,
                val_shared_heap_type,
                key_shared_heap_type,
                val_drop_fn,
            } => CleanupAction::FreeMapHandle {
                map_alloca: parent_alloca,
                key_is_vec,
                val_is_vec,
                val_shared_heap_type,
                key_shared_heap_type,
                val_drop_fn,
            },
            SlotOwnership::File => CleanupAction::FreeFileHandle {
                file_alloca: parent_alloca,
            },
            SlotOwnership::Enum { drop_fn } => CleanupAction::EnumDrop {
                enum_alloca: parent_alloca,
                drop_fn,
            },
            SlotOwnership::Struct { drop_fn } => CleanupAction::StructDrop {
                struct_alloca: parent_alloca,
                drop_fn,
            },
            SlotOwnership::User { drop_fn } => CleanupAction::UserDrop {
                binding_name: binding_name.to_string(),
                binding_ptr: parent_alloca,
                drop_fn,
                // Par-branch write-back registration: no Kāra type name in
                // scope here. The empty name keeps this entry out of the NLL
                // early-fire gate (`fire_due_user_drops` requires a known
                // non-shared struct name) — it drains at scope exit as before.
                type_name: String::new(),
            },
            SlotOwnership::Soa {
                soa_struct_ty,
                num_hot_groups,
                has_cold,
                soa_drop_fn,
            } => CleanupAction::FreeSoaGroups {
                soa_alloca: parent_alloca,
                soa_struct_ty,
                num_hot_groups,
                has_cold,
                soa_drop_fn,
            },
            SlotOwnership::Column { string_elem } => CleanupAction::FreeColumn {
                column_alloca: parent_alloca,
                string_elem,
            },
            SlotOwnership::DataFrame => CleanupAction::FreeDataFrame {
                df_alloca: parent_alloca,
            },
            SlotOwnership::Tensor => CleanupAction::FreeTensor {
                tensor_alloca: parent_alloca,
            },
        };
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(action);
        }
    }

    /// Lower a list of statements to a `karac_par_run` runtime dispatch.
    ///
    /// Shared between the explicit-`par`-block lowering (`compile_par_block`)
    /// and slice 2's auto-par lowering on inferred parallel groups
    /// (`compile_function_body`). Both call sites pass a slice of stmts that
    /// should run concurrently and a span used for capture-set scoping —
    /// for the explicit path the span is the par-block's own span; for the
    /// inferred path it is best-effort the function-body span (per-stmt
    /// span resolution is slice 3's concern). Trivial fan-outs (zero or
    /// one statement) compile sequentially without invoking the runtime.
    ///
    /// **Slice A (Phase-7 — Par codegen: return values, 2026-05-09):**
    /// `return_slots` carries the per-group set of let-bindings whose
    /// values must flow out of the parallel group to subsequent stmts in
    /// the surrounding function body. For each non-empty slot list, this
    /// function: (1) synthesizes a parent-allocated return struct
    /// `__karac_ParGroup_<spawn_site_id>_Returns` with one field per
    /// slot in slot-order; (2) passes its pointer through the env-struct
    /// as a trailing field so each branch can write to it; (3) the
    /// branch fn writes its produced value(s) into the assigned
    /// field(s) right after the let-binding's local alloca is filled,
    /// before the branch returns; (4) after `karac_par_run` joins, the
    /// parent loads each slot back into a `HashMap<String,
    /// BasicValueEnum>` keyed by binding-name. The caller (the auto-par
    /// dispatch site in `compile_function_body`) consumes the map to
    /// bind each loaded value as a fresh local in the function-body
    /// scope. Empty `return_slots` reduces to slice 2's behavior:
    /// no return-struct alloca, no slot field on the env-struct, no
    /// loads after the runtime call.
    ///
    /// **Slice 1b (Phase 7 — Par codegen: cancellation and error
    /// propagation, 2026-05-20).** The second tuple element returned is
    /// the `(slots_array_ptr, Result_struct_ty)` pair for the parent-
    /// allocated `__par_result_slots` array — `Some` when `result_slots`
    /// is non-empty, `None` otherwise. `compile_par_block` uses it to
    /// walk the slots in branch-index order after `karac_par_run`
    /// returns and phi-merge the first Err it finds against the join
    /// expression's value. The auto-par dispatch site in
    /// `compile_function_body` always passes an empty `result_slots`
    /// list and discards this tuple element.
    #[allow(clippy::result_large_err)]
    pub(super) fn emit_par_run(
        &mut self,
        stmts: &[Stmt],
        span: &Span,
        return_slots: &[ReturnSlot<'ctx>],
        result_slots: &[ResultSlot],
    ) -> Result<ParRunResult<'ctx>, String> {
        // Zero statements: nothing to do. Single statement: no parallelism
        // needed — compile in place to avoid the runtime call overhead.
        // The slot map is populated by reading each slot's binding from
        // `self.variables` after `compile_stmt` runs, so the caller's
        // outside-of-group reads still resolve.
        if stmts.is_empty() {
            return Ok((HashMap::new(), None, HashMap::new()));
        }
        if stmts.len() == 1 {
            self.compile_stmt(&stmts[0])?;
            let mut map: HashMap<String, BasicValueEnum<'ctx>> = HashMap::new();
            for slot in return_slots {
                if let Some(local) = self.variables.get(&slot.binding_name).copied() {
                    let v = self
                        .builder
                        .build_load(local.ty, local.ptr, &slot.binding_name)
                        .unwrap();
                    map.insert(slot.binding_name.clone(), v);
                }
            }
            return Ok((map, None, HashMap::new()));
        }

        // 1. Collect the union of captured variables across all branch statements.
        //    Intersection with self.variables filters out non-locals (top-level
        //    functions, struct names, etc.) that refs_in_block doesn't distinguish.
        let mut refs: HashSet<String> = HashSet::new();
        let mut inner_defs: HashSet<String> = HashSet::new();
        for stmt in stmts {
            let mini = Block {
                stmts: vec![stmt.clone()],
                final_expr: None,
                span: span.clone(),
            };
            self.refs_in_block(&mini, &mut refs, &mut inner_defs);
        }
        let mut captures: Vec<String> = refs
            .into_iter()
            .filter(|n| !inner_defs.contains(n))
            .filter(|n| self.variables.contains_key(n.as_str()))
            .collect();
        captures.sort(); // deterministic order

        // 2. Build the shared env struct. Captured user locals fill the
        //    leading slots; the next slot (added in slice 4) is the
        //    `*const ProviderFrame` snapshot of the calling thread's
        //    stack head (Theme 6 sub-step 5 — provider inheritance).
        //    The next slot (added in slice A) is a `*mut
        //    ParGroupReturns` pointing at the parent-allocated return
        //    struct — branches dereference and write through it.
        //    The final slot (slice 1a of the Phase-7 cancellation /
        //    error-propagation tranche, 2026-05-18) is a `*mut
        //    [Result_t_e; N_results]` — the per-branch Result slot
        //    array. Each Result-tracking branch writes its terminal
        //    Result value to its assigned slot before `ret void`. The
        //    env-struct grows by one pointer field whether the slot
        //    list is empty or not (ABI uniformity — keeps the env-
        //    struct shape predictable per spawn-site for downstream
        //    debugger introspection).
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Concurrency primitives stored as INLINE value cells — `Atomic[T]`
        // (a bare `T` slot) and `Mutex[T]` (`{ i64 flag, T }`) — must be
        // captured BY POINTER so every branch's RMW / lock hits the SAME
        // parent cell. Capturing by value gives each branch a private copy
        // whose mutations are silently dropped, and dropped mutations is the
        // whole of B-2026-07-18-28 (the design-recommended `Atomic`/`Mutex`
        // escape hatch producing a wrong answer with no diagnostic). The
        // parent frame outlives every branch (`karac_par_run` is a barrier),
        // so threading its alloca address through the env struct is sound — and
        // it is the only way real cross-thread atomicity/mutual-exclusion holds.
        let par_shared_cell: Vec<bool> = captures
            .iter()
            .map(|n| Self::is_par_shared_cell_type(self.var_type_names.get(n).map(String::as_str)))
            .collect();
        let mut env_field_types: Vec<BasicTypeEnum<'ctx>> = captures
            .iter()
            .zip(&par_shared_cell)
            .map(|(n, &by_ptr)| {
                if by_ptr {
                    ptr_ty.into()
                } else {
                    self.variables[n].ty
                }
            })
            .collect();
        let provider_head_idx = env_field_types.len();
        env_field_types.push(ptr_ty.into());
        // phase-8 line 153: i64 snapshot of the parent thread's active
        // span id, so each spawned worker inherits it (mirrors the
        // provider-head snapshot above). Placed immediately after the
        // provider head — the worker reads it at `captures.len() + 1`, the
        // same hardcoded-offset convention the provider head uses
        // (`captures.len()`); the trailing pointer slots below shift by one
        // automatically because their indices are recomputed here and
        // passed to the branch-fn emitter.
        let active_span_idx = env_field_types.len();
        env_field_types.push(self.context.i64_type().into());
        let par_returns_idx = env_field_types.len();
        env_field_types.push(ptr_ty.into());
        let par_result_slots_idx = env_field_types.len();
        env_field_types.push(ptr_ty.into());
        // Slice 2 (2026-05-21): parent-allocated `i32` cell — branch
        // fns CAS-min their `array_index` here on Err detect so the
        // parent can pick the source-order winner without walking
        // every slot's tag. Cell pointer is null when no branch is
        // Result-typed (same ABI uniformity as `__par_result_slots`).
        let par_earliest_err_idx = env_field_types.len();
        env_field_types.push(ptr_ty.into());
        let env_struct_ty = self.context.struct_type(&env_field_types, false);

        // 3. Allocate and populate the env struct in the outer function.
        //    Captures are copied by value (sufficient for ints, floats,
        //    pointers — the types the rest of codegen already supports).
        //    The provider-head field is filled by calling
        //    `karac_provider_get_stack_head()`; that read is cheap (one
        //    TLS get) and runs once per par-block, not per branch.
        let outer_fn = self.current_fn.unwrap();
        let env_alloca = self.create_entry_alloca(outer_fn, "__par_env", env_struct_ty.into());
        let mut env_agg = env_struct_ty.get_undef();
        for (i, name) in captures.iter().enumerate() {
            let slot = self.variables[name];
            // Shared-cell capture (Atomic/Mutex): thread the parent alloca
            // ADDRESS through the env struct instead of the loaded value, so
            // the branch binds to the one shared cell (B-2026-07-18-28).
            let val: BasicValueEnum<'ctx> = if par_shared_cell[i] {
                slot.ptr.into()
            } else {
                self.builder.build_load(slot.ty, slot.ptr, name).unwrap()
            };
            env_agg = self
                .builder
                .build_insert_value(env_agg, val, i as u32, "__par_env_field")
                .unwrap()
                .into_struct_value();
        }
        let head_val = self
            .builder
            .build_call(
                self.karac_provider_get_stack_head_fn,
                &[],
                "__par_env_head_snap",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        env_agg = self
            .builder
            .build_insert_value(
                env_agg,
                head_val,
                provider_head_idx as u32,
                "__par_env_head",
            )
            .unwrap()
            .into_struct_value();
        // phase-8 line 153: snapshot the active span id alongside the
        // provider head, so workers inherit the parent's active span.
        let active_span_snap = self
            .builder
            .build_call(
                self.karac_tracing_get_active_span_fn,
                &[],
                "__par_env_active_span_snap",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        env_agg = self
            .builder
            .build_insert_value(
                env_agg,
                active_span_snap,
                active_span_idx as u32,
                "__par_env_active_span",
            )
            .unwrap()
            .into_struct_value();

        // Slice A: mint the per-group return-struct type and alloca it
        // in the parent frame. We use the spawn-site ID (recorded just
        // below by `record_spawn_site`) as the type-name disambiguator.
        // To know the ID before recording, we mint it here and pass it
        // through. The struct lives module-scope as a named LLVM struct
        // so re-emission collisions are caught by inkwell. Empty slot
        // list → no struct, no alloca, the env-struct's
        // `__par_returns` field is a null `ptr` (never dereferenced
        // because the branch's slot-write path is dead code without
        // slots).
        let par_id = self.record_spawn_site(span, Some(stmts.len() as u32));
        let return_struct_ty: Option<StructType<'ctx>> = if return_slots.is_empty() {
            None
        } else {
            let name = format!("__karac_ParGroup_{par_id}_Returns");
            let st = self.context.opaque_struct_type(&name);
            let field_tys: Vec<BasicTypeEnum<'ctx>> =
                return_slots.iter().map(|s| s.llvm_ty).collect();
            st.set_body(&field_tys, false);
            Some(st)
        };
        let return_struct_alloca: PointerValue<'ctx> = if let Some(st) = return_struct_ty {
            self.create_entry_alloca(outer_fn, "__par_returns", st.into())
        } else {
            ptr_ty.const_null()
        };
        env_agg = self
            .builder
            .build_insert_value(
                env_agg,
                return_struct_alloca,
                par_returns_idx as u32,
                "__par_env_returns",
            )
            .unwrap()
            .into_struct_value();

        // Slice 1a (Phase 7 — Par codegen: cancellation and error
        // propagation, 2026-05-18). Allocate the parent-side Result-
        // slot array — a dense `[N_results x Result_t_e]` where
        // `N_results = result_slots.len()` (non-Result branches don't
        // consume a slot). The branch fn locates its slot by the
        // `array_index` recorded in its `ResultSlot`. Empty list → no
        // array, the env-struct's `__par_result_slots` field is a null
        // `ptr` (never dereferenced because no branch's slot-write
        // path is reachable). Each slot is `{ i64 tag, i64 w0 }` per
        // the Result lowering convention in
        // `seed_builtin_enum_layouts` (Err = tag 0, Ok = tag 1).
        let result_slot_struct_ty: Option<StructType<'ctx>> = if result_slots.is_empty() {
            None
        } else {
            self.enum_layouts.get("Result").map(|l| l.llvm_type)
        };
        let result_slots_alloca: PointerValue<'ctx> = if let Some(rty) = result_slot_struct_ty {
            let arr_ty = rty.array_type(result_slots.len() as u32);
            self.create_entry_alloca(outer_fn, "__par_result_slots", arr_ty.into())
        } else {
            ptr_ty.const_null()
        };
        env_agg = self
            .builder
            .build_insert_value(
                env_agg,
                result_slots_alloca,
                par_result_slots_idx as u32,
                "__par_env_result_slots",
            )
            .unwrap()
            .into_struct_value();

        // Slice 2 (2026-05-21): allocate the parent-side earliest-err
        // index cell — an `i32` initialised to `u32::MAX` (sentinel
        // for "no branch erred"). Branch fns do `atomicrmw umin` on
        // it with their `array_index` when their let-bound Result is
        // Err; the parent reads the cell once after `karac_par_run`
        // returns to pick the source-order winner. Skipped when no
        // branch is Result-typed (same null-ptr convention as
        // `__par_result_slots`).
        let i32_t = self.context.i32_type();
        let sentinel_max_u32 = i32_t.const_int(u32::MAX as u64, false);
        let earliest_err_idx_alloca: PointerValue<'ctx> = if result_slot_struct_ty.is_some() {
            let a = self.create_entry_alloca(outer_fn, "__par_earliest_err_idx", i32_t.into());
            self.builder.build_store(a, sentinel_max_u32).unwrap();
            a
        } else {
            ptr_ty.const_null()
        };
        env_agg = self
            .builder
            .build_insert_value(
                env_agg,
                earliest_err_idx_alloca,
                par_earliest_err_idx as u32,
                "__par_env_earliest_err_idx",
            )
            .unwrap()
            .into_struct_value();
        self.builder.build_store(env_alloca, env_agg).unwrap();

        // 4. Generate one branch function per statement.
        //    The SpawnSiteId minted above is reused as the branch fn
        //    name disambiguator and as the `karac_par_run` argument
        //    (Debugger Contract slice 4: the runtime uses it to
        //    populate `KaracFrame::spawn_site_id` for slice 5's
        //    enumeration surface).
        //
        // L227: resolve per-capture modes for this par block from the
        // ownership-pass classification. `None` (ownership pass not
        // run, or this par block not classified) → every capture
        // defaults to `Copy` semantics (today's behavior). Cloned
        // into a local Vec so the borrow doesn't conflict with
        // mutable codegen state during the branch-fn loop.
        let par_modes: Option<Vec<(String, crate::ownership::ParCaptureMode)>> = self
            .par_capture_modes
            .get(&crate::resolver::SpanKey::from_span(span))
            .cloned();
        let mut branch_fn_ptrs: Vec<PointerValue<'ctx>> = Vec::with_capacity(stmts.len());
        // Ownership metadata for slot bindings whose branch-side
        // cleanup was removed at branch end — each branch fn drains
        // its own slots' actions into this map; the parent rebinding
        // sites consume it (returned as ParRunResult's third element).
        let mut slot_ownership: HashMap<String, SlotOwnership<'ctx>> = HashMap::new();
        for (i, stmt) in stmts.iter().enumerate() {
            // Per-branch slot list: only the slots whose `branch_index`
            // matches this branch flow into `emit_par_branch_fn` for
            // slot-write emission. Branches with no slots emit unchanged.
            let branch_slots: Vec<ReturnSlot<'ctx>> = return_slots
                .iter()
                .filter(|s| s.branch_index == i)
                .cloned()
                .collect();
            // Slice 1a — lookup this branch's `ResultSlot` (at most
            // one per branch in slice 1a, since each branch is a
            // single statement and we only detect let-statements).
            let branch_result_slot: Option<ResultSlot> =
                result_slots.iter().find(|s| s.branch_index == i).cloned();
            let fn_ptr = self.emit_par_branch_fn(
                par_id,
                i,
                stmt,
                &captures,
                &env_field_types,
                env_struct_ty,
                par_returns_idx,
                return_struct_ty,
                &branch_slots,
                return_slots,
                par_result_slots_idx,
                result_slot_struct_ty,
                branch_result_slot,
                par_earliest_err_idx,
                par_modes.as_deref(),
                &mut slot_ownership,
            )?;
            branch_fn_ptrs.push(fn_ptr);
        }

        // 5. Build the KaracBranch array on the stack, one entry per branch.
        let i64_type = self.context.i64_type();
        let branches_ty = self.karac_branch_ty.array_type(stmts.len() as u32);
        let branches_alloca =
            self.create_entry_alloca(outer_fn, "__par_branches", branches_ty.into());
        for (i, fn_ptr) in branch_fn_ptrs.iter().enumerate() {
            let mut entry = self.karac_branch_ty.get_undef();
            entry = self
                .builder
                .build_insert_value(entry, *fn_ptr, 0, "__par_branch_fn")
                .unwrap()
                .into_struct_value();
            entry = self
                .builder
                .build_insert_value(entry, env_alloca, 1, "__par_branch_ctx")
                .unwrap()
                .into_struct_value();
            let idx = [
                i64_type.const_int(0, false),
                i64_type.const_int(i as u64, false),
            ];
            let elem_ptr = unsafe {
                self.builder
                    .build_in_bounds_gep(branches_ty, branches_alloca, &idx, "__par_branch_slot")
                    .unwrap()
            };
            self.builder.build_store(elem_ptr, entry).unwrap();
        }

        // 6. Call karac_par_run(branches, count, par_id, parent_cancel).
        //    `par_id` (Debugger Contract slice 4) was minted via
        //    `record_spawn_site` above; the runtime uses it to populate
        //    `KaracFrame::spawn_site_id` for slice 5's enumeration surface.
        //    `parent_cancel` (phase-6 line 475) is the *enclosing* branch's
        //    cancel flag when this `par` is nested inside another parallel
        //    region — `self.branch_cancel_ptr` is still the enclosing
        //    branch's pointer here (the inner branch fns saved/restored it
        //    around their own bodies in `emit_par_branch_fn` above). The
        //    runtime's join loop polls it so an outer cancel cascades into
        //    this region. Null at the top level (not inside any branch).
        let count = i64_type.const_int(stmts.len() as u64, false);
        let par_id_val = self.context.i32_type().const_int(par_id as u64, false);
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let parent_cancel = match self.branch_cancel_ptr {
            Some(p) => p,
            None => ptr_type.const_null(),
        };
        self.builder
            .build_call(
                self.karac_par_run_fn,
                &[
                    branches_alloca.into(),
                    count.into(),
                    par_id_val.into(),
                    parent_cancel.into(),
                ],
                "__par_run",
            )
            .unwrap();

        // 7. Slice A: load each return slot back from the parent-allocated
        //    return struct. The runtime barrier inside `karac_par_run`
        //    guarantees all branch fns completed before this point, so
        //    every slot the analyzer assigned is initialized (decision
        //    iii — move-only slot semantics with no destructor; the
        //    barrier replaces the destructor that would otherwise
        //    enforce ordering).
        let mut slot_values: HashMap<String, BasicValueEnum<'ctx>> = HashMap::new();
        if let Some(st) = return_struct_ty {
            for (field_idx, slot) in return_slots.iter().enumerate() {
                let field_ptr = self
                    .builder
                    .build_struct_gep(
                        st,
                        return_struct_alloca,
                        field_idx as u32,
                        &format!("__par_slot_{}_ptr", slot.binding_name),
                    )
                    .unwrap();
                let val = self
                    .builder
                    .build_load(slot.llvm_ty, field_ptr, &slot.binding_name)
                    .unwrap();
                slot_values.insert(slot.binding_name.clone(), val);
            }
        }
        // Slice 1b/2: surface the parent-side Result state to
        // `compile_par_block` — slot array pointer + slot struct type
        // (1b) + earliest-err-idx pointer (2). When no branch is
        // Result-typed neither the array nor the cell was allocated,
        // so the surface is `None` and the parent skips the err-pick
        // machinery entirely.
        let result_surface = match (result_slot_struct_ty, result_slots.is_empty()) {
            (Some(rty), false) => Some(ParResultSurface {
                slots_ptr: result_slots_alloca,
                slot_struct_ty: rty,
                earliest_err_idx_ptr: earliest_err_idx_alloca,
            }),
            _ => None,
        };
        Ok((slot_values, result_surface, slot_ownership))
    }

    /// Lower `collect_all_vec(fs)` (phase-6 slice 1b) — the homogeneous
    /// gather-all-errors parallel primitive. `fs : Vec[Fn() -> Result[T,
    /// E]]`; runs every closure to completion and returns
    /// `Vec[Result[T, E]]` with `output[i]` == outcome of `fs[i]`.
    ///
    /// Lowering (dynamic-N, reuses `karac_par_run` unchanged — gather-mode
    /// is simply "never flip the cancel flag on `Err`", which the
    /// trampoline below honours by never touching its cancel arg):
    ///   1. read the input Vec's data pointer + length N (a runtime value);
    ///   2. `malloc` three N-sized arrays — N `Result` slots (kept: becomes
    ///      the output Vec's buffer), N branch ctx structs + N `KaracBranch`
    ///      (freed after the join);
    ///   3. a runtime counted loop fills, per `i`: ctx[i] = {closure[i].fn,
    ///      closure[i].env, &slots[i]} and branches[i] = {trampoline, &ctx[i]};
    ///   4. `karac_par_run(branches, N, par_id, parent_cancel)` runs them all
    ///      and joins (the runtime barrier orders every slot write before it
    ///      returns);
    ///   5. assemble the output Vec `{slots, N, N}` and free the temp arrays.
    ///
    /// The `Result` LLVM layout is type-erased (`enum_layouts["Result"]`,
    /// uniform across `T`/`E`), so a single shared trampoline + ctx layout
    /// serve every `collect_all_vec` call site regardless of element type.
    pub(super) fn compile_collect_all_vec(
        &mut self,
        fs_expr: &Expr,
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let outer_fn = self
            .current_fn
            .ok_or_else(|| "collect_all_vec outside a function".to_string())?;

        // The type-erased `Result[T, E]` LLVM struct (same layout the
        // closures return by value, per `compile_closure`'s struct-return
        // arm). Reused for the slots, the output Vec elements, and the
        // trampoline's indirect-call return type.
        let result_ty = self
            .enum_layouts
            .get("Result")
            .ok_or_else(|| "collect_all_vec: Result enum layout missing".to_string())?
            .llvm_type;
        // Closure fat-pointer `{ fn_ptr, env_ptr }` — the Vec's element type.
        let closure_ty = self.closure_value_type();
        // Per-branch ctx `{ ptr fn_ptr, ptr env_ptr, ptr slot_ptr }`.
        let ctx_ty = self
            .context
            .struct_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);

        // Shared trampoline (emitted once per module).
        let trampoline = self.emit_collect_all_vec_trampoline(result_ty, ctx_ty)?;

        // 1. Evaluate the input Vec; extract data pointer + length N.
        let fs_val = self.compile_expr(fs_expr)?.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(fs_val, 0, "cav.fs.data")
            .unwrap()
            .into_pointer_value();
        let n = self
            .builder
            .build_extract_value(fs_val, 1, "cav.fs.len")
            .unwrap()
            .into_int_value();

        // 2. malloc the three N-sized arrays.
        let result_size = result_ty.size_of().unwrap();
        let slots_bytes = self
            .builder
            .build_int_mul(n, result_size, "cav.slots.bytes")
            .unwrap();
        let slots_ptr = self
            .builder
            .build_call(self.malloc_fn, &[slots_bytes.into()], "cav.slots")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let ctx_size = ctx_ty.size_of().unwrap();
        let ctxs_bytes = self
            .builder
            .build_int_mul(n, ctx_size, "cav.ctxs.bytes")
            .unwrap();
        let ctxs_ptr = self
            .builder
            .build_call(self.malloc_fn, &[ctxs_bytes.into()], "cav.ctxs")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let branch_size = self.karac_branch_ty.size_of().unwrap();
        let branches_bytes = self
            .builder
            .build_int_mul(n, branch_size, "cav.branches.bytes")
            .unwrap();
        let branches_ptr = self
            .builder
            .build_call(self.malloc_fn, &[branches_bytes.into()], "cav.branches")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // 3. Fill loop: for i in 0..N.
        let cond_bb = self.context.append_basic_block(outer_fn, "cav.fill.cond");
        let body_bb = self.context.append_basic_block(outer_fn, "cav.fill.body");
        let exit_bb = self.context.append_basic_block(outer_fn, "cav.fill.exit");
        let i_alloca = self.create_entry_alloca(outer_fn, "cav.i", i64_t.into());
        self.builder
            .build_store(i_alloca, i64_t.const_zero())
            .unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let i_cur = self
            .builder
            .build_load(i64_t, i_alloca, "cav.i.cur")
            .unwrap()
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, i_cur, n, "cav.lt")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        // closure_i = fs.data[i]; split into fn_ptr / env_ptr.
        let closure_ep = unsafe {
            self.builder
                .build_gep(closure_ty, data_ptr, &[i_cur], "cav.closure.ep")
                .unwrap()
        };
        let closure_val = self
            .builder
            .build_load(closure_ty, closure_ep, "cav.closure")
            .unwrap()
            .into_struct_value();
        let fn_ptr = self
            .builder
            .build_extract_value(closure_val, 0, "cav.fn")
            .unwrap();
        let env_ptr = self
            .builder
            .build_extract_value(closure_val, 1, "cav.env")
            .unwrap();
        // slot_i = &slots[i].
        let slot_ep = unsafe {
            self.builder
                .build_gep(result_ty, slots_ptr, &[i_cur], "cav.slot.ep")
                .unwrap()
        };
        // ctx[i] = { fn_ptr, env_ptr, slot_ep }.
        let ctx_ep = unsafe {
            self.builder
                .build_gep(ctx_ty, ctxs_ptr, &[i_cur], "cav.ctx.ep")
                .unwrap()
        };
        let cf0 = self
            .builder
            .build_struct_gep(ctx_ty, ctx_ep, 0, "cav.ctx.fn")
            .unwrap();
        self.builder.build_store(cf0, fn_ptr).unwrap();
        let cf1 = self
            .builder
            .build_struct_gep(ctx_ty, ctx_ep, 1, "cav.ctx.env")
            .unwrap();
        self.builder.build_store(cf1, env_ptr).unwrap();
        let cf2 = self
            .builder
            .build_struct_gep(ctx_ty, ctx_ep, 2, "cav.ctx.slot")
            .unwrap();
        self.builder.build_store(cf2, slot_ep).unwrap();
        // branches[i] = { trampoline, &ctx[i] }.
        let branch_ep = unsafe {
            self.builder
                .build_gep(
                    self.karac_branch_ty,
                    branches_ptr,
                    &[i_cur],
                    "cav.branch.ep",
                )
                .unwrap()
        };
        let bf0 = self
            .builder
            .build_struct_gep(self.karac_branch_ty, branch_ep, 0, "cav.branch.fn")
            .unwrap();
        self.builder
            .build_store(bf0, trampoline.as_global_value().as_pointer_value())
            .unwrap();
        let bf1 = self
            .builder
            .build_struct_gep(self.karac_branch_ty, branch_ep, 1, "cav.branch.ctx")
            .unwrap();
        self.builder.build_store(bf1, ctx_ep).unwrap();
        let i_next = self
            .builder
            .build_int_add(i_cur, i64_t.const_int(1, false), "cav.i.next")
            .unwrap();
        self.builder.build_store(i_alloca, i_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(exit_bb);

        // 4. karac_par_run(branches, N, par_id, parent_cancel). `parent_cancel`
        //    cascades an enclosing region's cancel inward (null at top level);
        //    this region never originates a cancel (gather-mode), but a panic
        //    in any branch still aborts under v1 panic=abort.
        let par_id = self.record_spawn_site(call_span, None);
        let par_id_val = self.context.i32_type().const_int(par_id as u64, false);
        let parent_cancel = match self.branch_cancel_ptr {
            Some(p) => p,
            None => ptr_ty.const_null(),
        };
        self.builder
            .build_call(
                self.karac_par_run_fn,
                &[
                    branches_ptr.into(),
                    n.into(),
                    par_id_val.into(),
                    parent_cancel.into(),
                ],
                "cav.par_run",
            )
            .unwrap();

        // 5. Free the temp arrays (the slots buffer is kept — it becomes the
        //    output Vec's storage).
        self.builder
            .build_call(self.free_fn, &[branches_ptr.into()], "")
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[ctxs_ptr.into()], "")
            .unwrap();

        // 5b. Free the input Vec's buffer. `fs` is a moved owned param (the
        //     ownership pass rejects use-after-move), so the caller's
        //     scope-exit drop is suppressed and `collect_all_vec` owns its
        //     disposal. ONLY the heap `{ptr,len,cap}` buffer is freed — the
        //     closures' envs are stack allocas in the constructing frame
        //     (`compile_closure` uses `create_entry_alloca`), valid across
        //     the synchronous `karac_par_run` join and reclaimed at frame
        //     exit; freeing them here would corrupt the stack. cap-guarded:
        //     an empty Vec (cap 0) has no allocation.
        let fs_cap = self
            .builder
            .build_extract_value(fs_val, 2, "cav.fs.cap")
            .unwrap()
            .into_int_value();
        // `Vec[Fn]` outer buffer — closure element size not bound here; 1.
        self.emit_free_if_cap_positive(data_ptr, fs_cap, 1);

        // 6. Output Vec[Result[T, E]] = { slots, N, N }.
        Ok(self.build_vec_value(slots_ptr, n, n))
    }

    /// Lower `collect_all(|| a, || b, …)` (phase-6) — the heterogeneous
    /// fixed-arity (2..=8) gather. Each `arg` is an inline closure
    /// `Fn() -> Result[Ai, Ei]`; runs them all in parallel and returns the
    /// tuple `(Result[A1,E1], …, Result[An,En])`, position-bound.
    ///
    /// Static-N sibling of `compile_collect_all_vec`: it reuses the very
    /// same `__collect_all_vec_branch` trampoline + `karac_par_run`
    /// gather (the `Result` LLVM layout is type-erased, so a tuple of
    /// heterogeneous `Result`s is just a struct of uniform `Result`
    /// structs), but the N branches are known at compile time, so the
    /// slot / ctx / branch arrays are stack allocas (no malloc/free, no
    /// input Vec to free), and the result is assembled into a tuple
    /// aggregate rather than a Vec. The closures' env allocas live in this
    /// frame and stay valid across the synchronous `karac_par_run` join.
    pub(super) fn compile_collect_all(
        &mut self,
        args: &[CallArg],
        call_span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let outer_fn = self
            .current_fn
            .ok_or_else(|| "collect_all outside a function".to_string())?;
        let n = args.len();

        let result_ty = self
            .enum_layouts
            .get("Result")
            .ok_or_else(|| "collect_all: Result enum layout missing".to_string())?
            .llvm_type;
        let ctx_ty = self
            .context
            .struct_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let trampoline = self.emit_collect_all_vec_trampoline(result_ty, ctx_ty)?;

        // Fixed-size stack arrays (N known at compile time).
        let slots_arr_ty = result_ty.array_type(n as u32);
        let ctxs_arr_ty = ctx_ty.array_type(n as u32);
        let branches_arr_ty = self.karac_branch_ty.array_type(n as u32);
        let slots = self.create_entry_alloca(outer_fn, "ca.slots", slots_arr_ty.into());
        let ctxs = self.create_entry_alloca(outer_fn, "ca.ctxs", ctxs_arr_ty.into());
        let branches = self.create_entry_alloca(outer_fn, "ca.branches", branches_arr_ty.into());

        // Per branch: compile the inline closure to a `{fn_ptr, env_ptr}`
        // fat-pointer, then fill ctx[i] = {fn_ptr, env_ptr, &slots[i]} and
        // branches[i] = {trampoline, &ctx[i]}.
        let tramp_ptr = trampoline.as_global_value().as_pointer_value();
        for (i, arg) in args.iter().enumerate() {
            let closure_val = self.compile_expr(&arg.value)?.into_struct_value();
            let fn_ptr = self
                .builder
                .build_extract_value(closure_val, 0, "ca.fn")
                .unwrap();
            let env_ptr = self
                .builder
                .build_extract_value(closure_val, 1, "ca.env")
                .unwrap();
            let idx = [i64_t.const_zero(), i64_t.const_int(i as u64, false)];
            let slot_ep = unsafe {
                self.builder
                    .build_in_bounds_gep(slots_arr_ty, slots, &idx, "ca.slot.ep")
                    .unwrap()
            };
            let ctx_ep = unsafe {
                self.builder
                    .build_in_bounds_gep(ctxs_arr_ty, ctxs, &idx, "ca.ctx.ep")
                    .unwrap()
            };
            let cf0 = self
                .builder
                .build_struct_gep(ctx_ty, ctx_ep, 0, "ca.ctx.fn")
                .unwrap();
            self.builder.build_store(cf0, fn_ptr).unwrap();
            let cf1 = self
                .builder
                .build_struct_gep(ctx_ty, ctx_ep, 1, "ca.ctx.env")
                .unwrap();
            self.builder.build_store(cf1, env_ptr).unwrap();
            let cf2 = self
                .builder
                .build_struct_gep(ctx_ty, ctx_ep, 2, "ca.ctx.slot")
                .unwrap();
            self.builder.build_store(cf2, slot_ep).unwrap();
            let branch_ep = unsafe {
                self.builder
                    .build_in_bounds_gep(branches_arr_ty, branches, &idx, "ca.branch.ep")
                    .unwrap()
            };
            let bf0 = self
                .builder
                .build_struct_gep(self.karac_branch_ty, branch_ep, 0, "ca.branch.fn")
                .unwrap();
            self.builder.build_store(bf0, tramp_ptr).unwrap();
            let bf1 = self
                .builder
                .build_struct_gep(self.karac_branch_ty, branch_ep, 1, "ca.branch.ctx")
                .unwrap();
            self.builder.build_store(bf1, ctx_ep).unwrap();
        }

        // karac_par_run(&branches[0], N, par_id, parent_cancel).
        let branches_base = unsafe {
            self.builder
                .build_in_bounds_gep(
                    branches_arr_ty,
                    branches,
                    &[i64_t.const_zero(), i64_t.const_zero()],
                    "ca.branches.base",
                )
                .unwrap()
        };
        let par_id = self.record_spawn_site(call_span, Some(n as u32));
        let par_id_val = self.context.i32_type().const_int(par_id as u64, false);
        let parent_cancel = match self.branch_cancel_ptr {
            Some(p) => p,
            None => ptr_ty.const_null(),
        };
        self.builder
            .build_call(
                self.karac_par_run_fn,
                &[
                    branches_base.into(),
                    i64_t.const_int(n as u64, false).into(),
                    par_id_val.into(),
                    parent_cancel.into(),
                ],
                "ca.par_run",
            )
            .unwrap();

        // Assemble the tuple `(Result, …, Result)` from the slots (each
        // slot is a fully-written `Result` by the time the join returns).
        let tuple_ty = self.context.struct_type(&vec![result_ty.into(); n], false);
        let mut agg = tuple_ty.get_undef();
        for i in 0..n {
            let idx = [i64_t.const_zero(), i64_t.const_int(i as u64, false)];
            let slot_ep = unsafe {
                self.builder
                    .build_in_bounds_gep(slots_arr_ty, slots, &idx, "ca.read.ep")
                    .unwrap()
            };
            let slot_val = self
                .builder
                .build_load(result_ty, slot_ep, "ca.read")
                .unwrap();
            agg = self
                .builder
                .build_insert_value(agg, slot_val, i as u32, "ca.elem")
                .unwrap()
                .into_struct_value();
        }
        Ok(agg.into())
    }

    /// Emit the shared `collect_all_vec` branch trampoline (once per
    /// module): `void __collect_all_vec_branch(ptr ctx, ptr cancel)`.
    /// `ctx` is `{ fn_ptr, env_ptr, slot_ptr }`; it invokes the closure
    /// (`fn_ptr(env_ptr) -> Result`, the by-value struct-return closure
    /// ABI) and stores the `Result` into the slot. It NEVER reads `cancel`
    /// — that is precisely what makes this gather-mode rather than fail-fast.
    fn emit_collect_all_vec_trampoline(
        &mut self,
        result_ty: StructType<'ctx>,
        ctx_ty: StructType<'ctx>,
    ) -> Result<FunctionValue<'ctx>, String> {
        let name = "__collect_all_vec_branch";
        if let Some(f) = self.module.get_function(name) {
            return Ok(f);
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_ty = self.context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_ty),
                BasicMetadataTypeEnum::from(ptr_ty),
            ],
            false,
        );
        let tramp = self.module.add_function(name, fn_ty, None);

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(tramp);
        let entry = self.context.append_basic_block(tramp, "entry");
        self.builder.position_at_end(entry);

        let ctx_arg = tramp.get_nth_param(0).unwrap().into_pointer_value();
        let f0 = self
            .builder
            .build_struct_gep(ctx_ty, ctx_arg, 0, "cav.t.fn.ptr")
            .unwrap();
        let fn_ptr = self
            .builder
            .build_load(ptr_ty, f0, "cav.t.fn")
            .unwrap()
            .into_pointer_value();
        let f1 = self
            .builder
            .build_struct_gep(ctx_ty, ctx_arg, 1, "cav.t.env.ptr")
            .unwrap();
        let env_ptr = self
            .builder
            .build_load(ptr_ty, f1, "cav.t.env")
            .unwrap()
            .into_pointer_value();
        let f2 = self
            .builder
            .build_struct_gep(ctx_ty, ctx_arg, 2, "cav.t.slot.ptr")
            .unwrap();
        let slot_ptr = self
            .builder
            .build_load(ptr_ty, f2, "cav.t.slot")
            .unwrap()
            .into_pointer_value();

        // result = fn_ptr(env_ptr) — closure ABI is `Result(ptr env)`.
        let closure_fn_ty = result_ty.fn_type(&[BasicMetadataTypeEnum::from(ptr_ty)], false);
        let result_val = self
            .builder
            .build_indirect_call(closure_fn_ty, fn_ptr, &[env_ptr.into()], "cav.t.invoke")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_store(slot_ptr, result_val).unwrap();
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Ok(tramp)
    }

    /// Generate the branch function for a single par-block statement.
    /// Signature: `void __par_branch_<par_id>_<i>(ptr ctx, ptr cancel_flag)`.
    ///
    /// The function unpacks captured locals from the shared env struct,
    /// compiles the statement, and returns. Captures are loaded as fresh
    /// allocas so the statement body sees them as ordinary locals.
    ///
    /// **Slice A (Phase-7 — Par codegen: return values):** when
    /// `branch_slots` is non-empty, after the statement body's
    /// `compile_stmt` succeeds, this function emits a load+store
    /// sequence for each assigned slot — loading the just-bound
    /// variable's value out of its branch-local alloca and storing it
    /// into the matching field of the parent-allocated return struct
    /// (reached via the `__par_returns` field of the env struct). The
    /// store happens *before* the branch fn's `ret void`, so by the
    /// time `karac_par_run`'s join barrier returns to the parent every
    /// slot the analyzer assigned is initialized. Move-only semantics
    /// (decision iii): the branch's `scope_cleanup_actions` are
    /// discarded on `emit_par_branch_fn` exit (the existing
    /// `mem::take`/restore dance), so destructor-bearing slot values
    /// move into the slot rather than being dropped at branch end —
    /// the parent's load + subsequent `track_*` is the unique cleanup
    /// owner.
    #[allow(clippy::result_large_err)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_par_branch_fn(
        &mut self,
        par_id: u32,
        index: usize,
        stmt: &Stmt,
        captures: &[String],
        env_field_types: &[BasicTypeEnum<'ctx>],
        env_struct_ty: StructType<'ctx>,
        par_returns_idx: usize,
        return_struct_ty: Option<StructType<'ctx>>,
        branch_slots: &[ReturnSlot<'ctx>],
        all_slots: &[ReturnSlot<'ctx>],
        par_result_slots_idx: usize,
        result_slot_struct_ty: Option<StructType<'ctx>>,
        branch_result_slot: Option<ResultSlot>,
        par_earliest_err_idx: usize,
        par_capture_modes: Option<&[(String, crate::ownership::ParCaptureMode)]>,
        slot_ownership: &mut HashMap<String, SlotOwnership<'ctx>>,
    ) -> Result<PointerValue<'ctx>, String> {
        let fn_name = format!("__par_branch_{}_{}", par_id, index);
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Branch function signature: void fn(ptr ctx, ptr cancel_flag)
        let fn_type = self.context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_ty),
                BasicMetadataTypeEnum::from(ptr_ty),
            ],
            false,
        );
        let branch_fn = self.module.add_function(&fn_name, fn_type, None);

        // Save outer codegen state — we're about to compile a fresh function.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_cleanup = std::mem::take(&mut self.scope_cleanup_actions);
        // Branch body needs its own root cleanup frame so the
        // `track_vec_var` / `track_map_var` / `track_rc_var` calls
        // emitted while compiling the branch's stmts have a frame to
        // push into. `track_*` is a no-op when `scope_cleanup_actions`
        // is empty (its body is `if let Some(frame) =
        // self.scope_cleanup_actions.last_mut()`); without the push
        // here, every branch-local Vec / String / Map / RC binding
        // silently fails to queue its cleanup action and leaks at
        // branch exit. Mirrors `compile_function`'s entry-time push
        // at the start of every user function. The `cancel-path`
        // pre-fix wasn't affected because `emit_branch_cancel_check`
        // ran while the branch's body was already mid-emission with
        // (sometimes) other frames pushed by nested control flow;
        // the normal-completion path runs at branch root.
        self.scope_cleanup_actions.push(Vec::new());
        let saved_cancel_ptr = self.branch_cancel_ptr.take();

        self.current_fn = Some(branch_fn);
        let entry = self.context.append_basic_block(branch_fn, "entry");
        self.builder.position_at_end(entry);

        // Cancel check at branch start: if *cancel_flag != 0, return immediately.
        let cancel_ptr = branch_fn.get_nth_param(1).unwrap().into_pointer_value();
        // Stash the cancel pointer so subsequent `compile_call` invocations
        // can emit mid-branch cooperative cancel checks before each callee.
        self.branch_cancel_ptr = Some(cancel_ptr);
        let i8_ty = self.context.i8_type();
        let cancel_val = self
            .builder
            .build_load(i8_ty, cancel_ptr, "cancel")
            .unwrap()
            .into_int_value();
        let is_cancelled = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                cancel_val,
                i8_ty.const_int(0, false),
                "is_cancelled",
            )
            .unwrap();
        let body_bb = self.context.append_basic_block(branch_fn, "body");
        let cancel_bb = self.context.append_basic_block(branch_fn, "cancelled");
        self.builder
            .build_conditional_branch(is_cancelled, cancel_bb, body_bb)
            .unwrap();
        self.builder.position_at_end(cancel_bb);
        self.builder.build_return(None).unwrap();
        self.builder.position_at_end(body_bb);

        // Theme 6 sub-step 5: seed this worker thread's provider stack
        // from the env-struct snapshot taken at par-block entry. Always
        // emitted because every par-block env-struct now carries the
        // head-pointer slot in its trailing field (the captures vec may
        // be empty but the env still has at least the one ptr field).
        // Run before unpacking captures so any with_provider bindings
        // are visible inside their initialization (defensive — none of
        // the existing capture-init paths invoke R.method, but this
        // ordering is the cheap, future-proof choice).
        let env_ptr = branch_fn.get_nth_param(0).unwrap().into_pointer_value();
        let env_val_for_head = self
            .builder
            .build_load::<BasicTypeEnum<'ctx>>(env_struct_ty.into(), env_ptr, "__env_head_load")
            .unwrap();
        let head_val = self
            .builder
            .build_extract_value(
                env_val_for_head.into_struct_value(),
                captures.len() as u32,
                "__par_branch_head",
            )
            .unwrap();
        self.builder
            .build_call(
                self.karac_provider_set_stack_head_fn,
                &[head_val.into()],
                "",
            )
            .unwrap();

        // phase-8 line 153: inherit the parent's active span. The i64
        // snapshot sits immediately after the provider head, so it's at
        // `captures.len() + 1` (same hardcoded-offset convention as the
        // provider head's `captures.len()` above).
        let active_span_val = self
            .builder
            .build_extract_value(
                env_val_for_head.into_struct_value(),
                captures.len() as u32 + 1,
                "__par_branch_active_span",
            )
            .unwrap();
        self.builder
            .build_call(
                self.karac_tracing_set_active_span_fn,
                &[active_span_val.into()],
                "",
            )
            .unwrap();

        // Unpack captures from the env struct into fresh allocas.
        //
        // L227 (non-trivial captures): after the alloca/store, for any
        // capture whose ownership-pass mode is `SharedRc`, emit an
        // atomic rc_inc on the heap pointer and register the binding
        // with `track_rc_var` so the branch-exit scope cleanup
        // balances it with an atomic rc_dec. Without the inc, a
        // single-branch capture that flows into a function consuming
        // the reference (the spec's "sole-ownership move" case) would
        // race the parent's owning reference: the consume's
        // scope-exit dec would drop refcount to zero mid-par, and
        // the parent's subsequent dec would touch freed memory. The
        // inc + track pair is atomic on both sides so sibling
        // branches and the parent's stash dec are race-free; the
        // ownership pass also promotes the parent's binding into
        // `arc_values`, which routes the parent's scope-exit dec
        // through `emit_arc_dec` for symmetry. Captures not in the
        // modes list (or the list itself absent) fall through to
        // today's by-value-through-env behavior — Copy semantics.
        if !captures.is_empty() {
            let env_val = self
                .builder
                .build_load::<BasicTypeEnum<'ctx>>(env_struct_ty.into(), env_ptr, "__env")
                .unwrap();
            for (i, var_name) in captures.iter().enumerate() {
                let cap_ty = env_field_types[i];
                let field_val = self
                    .builder
                    .build_extract_value(env_val.into_struct_value(), i as u32, var_name)
                    .unwrap();
                // Shared-cell capture (Atomic/Mutex, B-2026-07-18-28): the env
                // field is the PARENT alloca address, not a value copy. Bind the
                // branch-local directly to that shared address (no private
                // alloca, no store) so `.fetch_add` / `.store` / `lock {}` RMW
                // the one parent cell and the mutation is visible after the
                // barrier. `saved_var_types` is the parent's `var_type_names`
                // (mem::take'd above), so this classification matches the
                // by-pointer decision `emit_par_run` made when it built the env
                // struct; `saved_vars` still holds the parent slot's value type
                // (`i64` for Atomic, `{i64,T}` for Mutex) that the atomic/lock
                // codegen reads back through `resolve_atomic_storage`.
                if Self::is_par_shared_cell_type(saved_var_types.get(var_name).map(String::as_str))
                {
                    let shared_ptr = field_val.into_pointer_value();
                    let value_ty = saved_vars.get(var_name).map(|s| s.ty).unwrap_or(cap_ty);
                    self.variables.insert(
                        var_name.clone(),
                        VarSlot {
                            ptr: shared_ptr,
                            ty: value_ty,
                        },
                    );
                    if let Some(type_name) = saved_var_types.get(var_name) {
                        self.var_type_names
                            .insert(var_name.clone(), type_name.clone());
                    }
                    continue;
                }
                let alloca = self.create_entry_alloca(branch_fn, var_name, cap_ty);
                self.builder.build_store(alloca, field_val).unwrap();
                self.variables.insert(
                    var_name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: cap_ty,
                    },
                );
                // Propagate the outer scope's struct/enum type binding so
                // method dispatch can route `var.method()` through the
                // user impl-block path inside the par branch.
                if let Some(type_name) = saved_var_types.get(var_name) {
                    self.var_type_names
                        .insert(var_name.clone(), type_name.clone());
                }
                // L227 SharedRc path: atomic rc_inc + cleanup registration.
                let is_shared_rc = par_capture_modes.is_some_and(|modes| {
                    modes.iter().any(|(n, m)| {
                        n == var_name && matches!(m, crate::ownership::ParCaptureMode::SharedRc)
                    })
                });
                if is_shared_rc {
                    if let Some(type_name) = saved_var_types.get(var_name) {
                        if let Some(heap_type) =
                            self.shared_types.get(type_name).map(|i| i.heap_type)
                        {
                            self.emit_arc_inc(heap_type, field_val.into_pointer_value());
                            self.track_rc_var(var_name, alloca, heap_type);
                        }
                    }
                }
            }
        }

        // Compile the statement body. Any errors surface to the outer context.
        let stmt_result = self.compile_stmt(stmt);

        // Slice A: emit slot writes for class-(ii) bindings produced by
        // this branch. Walk `branch_slots` (the slots whose
        // `branch_index == index`), find the matching variable in
        // `self.variables` (just bound by the let inside `compile_stmt`
        // above), load it, then store into the parent-allocated return
        // struct's field at the slot's position in `all_slots`. Done
        // before the branch fn's `ret` so the runtime barrier inside
        // `karac_par_run` correctly orders the writes against the
        // parent's subsequent load.
        let stmt_ok = stmt_result.is_ok()
            && self
                .builder
                .get_insert_block()
                .unwrap()
                .get_terminator()
                .is_none();
        if stmt_ok && !branch_slots.is_empty() {
            if let Some(rt_struct) = return_struct_ty {
                // Reload the env-struct here to extract the
                // `__par_returns` pointer. We can't keep a stale value
                // from prologue because `compile_stmt` may have emitted
                // arbitrary basic blocks between then and now; safer to
                // re-load.
                let env_val = self
                    .builder
                    .build_load::<BasicTypeEnum<'ctx>>(
                        env_struct_ty.into(),
                        env_ptr,
                        "__env_for_returns",
                    )
                    .unwrap();
                let returns_ptr_v = self
                    .builder
                    .build_extract_value(
                        env_val.into_struct_value(),
                        par_returns_idx as u32,
                        "__par_returns_ptr",
                    )
                    .unwrap();
                let returns_ptr = returns_ptr_v.into_pointer_value();
                for slot in branch_slots {
                    // Find this slot's index in the all-slots list (i.e.
                    // its field position in the return struct). Linear
                    // search — slot lists are tiny (≤ branch count).
                    let Some(field_idx) = all_slots
                        .iter()
                        .position(|s| s.binding_name == slot.binding_name)
                    else {
                        continue;
                    };
                    let Some(local) = self.variables.get(&slot.binding_name).copied() else {
                        // Variable wasn't bound (compile_stmt error path,
                        // class-(ii) binding shape mismatch, etc.) — skip
                        // the slot write defensively.
                        continue;
                    };
                    let val = self
                        .builder
                        .build_load(
                            local.ty,
                            local.ptr,
                            &format!("__par_slot_{}_load", slot.binding_name),
                        )
                        .unwrap();
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            rt_struct,
                            returns_ptr,
                            field_idx as u32,
                            &format!("__par_slot_{}_dst", slot.binding_name),
                        )
                        .unwrap();
                    self.builder.build_store(field_ptr, val).unwrap();
                }
            }
        }

        // Slice 1a (Phase 7 — Par codegen: cancellation and error
        // propagation, 2026-05-18): emit the Result-slot write for
        // branches whose let-statement is `Result[T, E]`-typed, and
        // conditionally store `true` into the per-call cancel flag
        // when the stored tag is `Err` (== 0). Done BEFORE the
        // scope-cleanup + `ret void` below so the runtime barrier
        // inside `karac_par_run` orders the slot-write and the
        // cancel-flag store against the parent's subsequent reads.
        //
        // ABI: result slots live in a parent-allocated dense array of
        // `Result_t_e` cells, pointer-passed through the env-struct's
        // `__par_result_slots` field. This branch's array index is the
        // `array_index` recorded in its `ResultSlot` (assigned by
        // `compile_par_block`'s detection pass). Cancel-flag pointer is
        // the branch fn's second parameter, captured into
        // `self.branch_cancel_ptr` during the entry-time setup above.
        if stmt_ok {
            if let (Some(slot), Some(rty)) = (branch_result_slot.as_ref(), result_slot_struct_ty) {
                if let Some(local) = self.variables.get(&slot.binding_name).copied() {
                    let env_val = self
                        .builder
                        .build_load::<BasicTypeEnum<'ctx>>(
                            env_struct_ty.into(),
                            env_ptr,
                            "__env_for_result_slots",
                        )
                        .unwrap();
                    let slots_ptr_v = self
                        .builder
                        .build_extract_value(
                            env_val.into_struct_value(),
                            par_result_slots_idx as u32,
                            "__par_result_slots_ptr",
                        )
                        .unwrap();
                    let slots_ptr = slots_ptr_v.into_pointer_value();

                    // GEP into `slots_ptr[array_index]`. The slot
                    // array is allocated as `[N x Result_t_e]`; index
                    // through with a constant array-index pair.
                    let i64_t = self.context.i64_type();
                    let zero = i64_t.const_zero();
                    let arr_idx = i64_t.const_int(slot.array_index as u64, false);
                    let arr_ty = rty.array_type(0); // length doesn't matter for GEP element-typing
                    let slot_ptr = unsafe {
                        self.builder
                            .build_in_bounds_gep(
                                arr_ty,
                                slots_ptr,
                                &[zero, arr_idx],
                                &format!("__par_result_slot_{}_ptr", slot.binding_name),
                            )
                            .unwrap()
                    };

                    // Load the just-bound Result value out of the
                    // branch's local alloca and store it into the slot.
                    let val = self
                        .builder
                        .build_load(
                            local.ty,
                            local.ptr,
                            &format!("__par_result_slot_{}_load", slot.binding_name),
                        )
                        .unwrap();
                    self.builder.build_store(slot_ptr, val).unwrap();

                    // Cancel-flag store on Err: extract tag (field 0
                    // of the Result struct), compare against 0 (Err
                    // tag per `seed_builtin_enum_layouts`), and
                    // store `1u8` into the cancel flag pointer when
                    // equal. The store is unconditional within the
                    // is-err arm; sibling branches' next cooperative
                    // cancel check observes the flip.
                    if let (BasicValueEnum::StructValue(sv), Some(cancel_ptr)) =
                        (val, self.branch_cancel_ptr)
                    {
                        let tag = self
                            .builder
                            .build_extract_value(
                                sv,
                                0,
                                &format!("__par_result_slot_{}_tag", slot.binding_name),
                            )
                            .unwrap()
                            .into_int_value();
                        let zero_i64 = i64_t.const_zero();
                        let is_err = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::EQ,
                                tag,
                                zero_i64,
                                &format!("__par_result_slot_{}_is_err", slot.binding_name),
                            )
                            .unwrap();
                        let set_bb = self.context.append_basic_block(
                            branch_fn,
                            &format!("__par_result_{}_set_cancel", slot.binding_name),
                        );
                        let cont_bb = self.context.append_basic_block(
                            branch_fn,
                            &format!("__par_result_{}_after_cancel", slot.binding_name),
                        );
                        self.builder
                            .build_conditional_branch(is_err, set_bb, cont_bb)
                            .unwrap();
                        self.builder.position_at_end(set_bb);
                        let i8_t = self.context.i8_type();
                        let one_i8 = i8_t.const_int(1, false);
                        self.builder.build_store(cancel_ptr, one_i8).unwrap();

                        // Slice 2 (2026-05-21): publish this branch's
                        // `array_index` into the parent-side earliest-
                        // err-idx cell. `atomicrmw umin` keeps the
                        // smallest seen index, which by construction
                        // matches source order (slots are assigned in
                        // ascending branch-index order in
                        // `compile_par_block` step 4). The sentinel
                        // `u32::MAX` set at parent-alloca time is
                        // strictly greater than every valid index, so
                        // the first Err always wins the umin even when
                        // multiple branches err concurrently. Ordering
                        // is Monotonic — the `karac_par_run` join
                        // barrier supplies the happens-before edge the
                        // parent needs before its plain load in
                        // `compile_par_block` step 7.
                        let env_val_for_idx = self
                            .builder
                            .build_load::<BasicTypeEnum<'ctx>>(
                                env_struct_ty.into(),
                                env_ptr,
                                "__env_for_earliest_err_idx",
                            )
                            .unwrap();
                        let idx_cell_ptr_v = self
                            .builder
                            .build_extract_value(
                                env_val_for_idx.into_struct_value(),
                                par_earliest_err_idx as u32,
                                "__par_earliest_err_idx_ptr",
                            )
                            .unwrap();
                        let idx_cell_ptr = idx_cell_ptr_v.into_pointer_value();
                        let i32_t = self.context.i32_type();
                        let my_idx = i32_t.const_int(slot.array_index as u64, false);
                        self.builder
                            .build_atomicrmw(
                                inkwell::AtomicRMWBinOp::UMin,
                                idx_cell_ptr,
                                my_idx,
                                inkwell::AtomicOrdering::Monotonic,
                            )
                            .unwrap();

                        self.builder.build_unconditional_branch(cont_bb).unwrap();
                        self.builder.position_at_end(cont_bb);
                    }
                }
            }
        }

        // Terminate the branch function. The par-block API discards branch
        // return values in this first cut.
        //
        // Before the `ret void`, fire every cleanup the branch body
        // accumulated (Vec/String buffer frees, Map handle frees, RC
        // decs). The branch body started with an empty cleanup frame
        // (`std::mem::take(&mut self.scope_cleanup_actions)` above), so
        // the queue holds only allocations made INSIDE the branch — none
        // of the parent's pre-branch allocations are at risk of getting
        // double-freed here. Pre-fix, the queue was just discarded by
        // the `self.scope_cleanup_actions = saved_cleanup` restore below,
        // leaking every branch-local allocation; the kata-6 bench at
        // K = 10_000 measured ~474 MiB peak RSS from this leak alone.
        // The cancel-path branch above (`emit_branch_cancel_check`)
        // already fires `emit_scope_cleanup` before its `ret void` —
        // this is the symmetric fix for the normal-completion path.
        //
        // Slot-source suppression: when the branch produced a class-(ii)
        // binding consumed in the parent scope, the slot-write loop
        // above structurally-copied the local's `{ptr, len, cap}` into
        // the parent's return struct, so the local and the parent's
        // slot field now alias the same heap buffer. Zero the local's
        // `cap` so the queued `FreeVecBuffer`'s `cap > 0` guard skips
        // the free — leaving the parent's slot value as the unique
        // (non-tracked) owner. Mirrors `suppress_cleanup_for_tail_return`
        // for function-tail Identifier returns; same shape works here.
        // The slot value's eventual cleanup is a separate question —
        // see the parent-side comment at compile_function_body's
        // slot-binding site for the design intent.
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            for slot in branch_slots {
                if let Some(local) = self.variables.get(&slot.binding_name).copied() {
                    let vec_st: BasicTypeEnum<'ctx> = self.vec_struct_type().into();
                    if local.ty == vec_st {
                        self.zero_vec_alloca_cap(local.ptr);
                        continue;
                    }
                }
                // RC-bearing slot sources: a `let binding = <expr>` whose
                // result type involves a `shared struct` / `shared enum`
                // had `track_rc_var` / `track_rc_option_var` register a
                // queued `RcDec` / `RcDecOption` cleanup against the
                // binding's local alloca during the body's `compile_stmt`.
                // Run on branch exit, that dec would drop the same heap
                // object the parent's slot field still references —
                // observed as `Option[ListNode]` payload reading freed
                // memory after `karac_par_run` join (kata 2 add-two-numbers
                // bench corruption, 2026-05-17).
                //
                // Suppress by structurally mutating the local's tracked
                // state to a value the cleanup's runtime guard treats as
                // "nothing to drop":
                //   - `RcDec` reloads `variables[name].ptr` and skips when
                //     null → store a null pointer into the local alloca.
                //   - `RcDecOption` reloads the option tag and skips when
                //     tag != Some → store a zero tag into field 0 of the
                //     option slot.
                // The slot-write loop above already copied the live value
                // into the return struct, so the parent receives an
                // intact pointer. Mirrors the Vec `cap=0` suppression
                // pattern, generalised over the RC cleanup shapes.
                let frame_idx = self.scope_cleanup_actions.len().saturating_sub(1);
                let published_ptr = self.variables.get(&slot.binding_name).map(|v| v.ptr);
                let mut nullify_local: Option<PointerValue<'ctx>> = None;
                let mut zero_opt_tag: Option<(PointerValue<'ctx>, StructType<'ctx>)> = None;
                // Inline `Option`/`Result` payload slots (B-2026-07-16-19):
                // the branch's `let e = first_word("")` registered a tag-
                // guarded `FreeInlineOptionPayload` (or the Result / Option-
                // Map sibling) against the branch-local alloca. The slot-
                // write above published the WHOLE tagged value — payload
                // pointer included — to the parent, so the branch's drain
                // must not fire (pre-fix it freed the payload the parent
                // was about to consume: `a.unwrap_or(..)` after the join
                // read freed memory, then freed it again). Suppress by
                // storing a tag the action's runtime guard matches with NO
                // live variant — chosen at compile time to differ from
                // `some_tag` / `ok_tag` / `err_tag` — mirroring the
                // `RcDecOption` zero-tag suppression above. The parent
                // rebind site re-registers the equivalent cleanup against
                // its fresh alloca (stmts.rs slot-rebind loop), making the
                // parent the unique owner.
                let mut sentinel_tag_stores: Vec<(PointerValue<'ctx>, StructType<'ctx>, u64)> =
                    Vec::new();
                fn tag_not_in(live: &[u64]) -> u64 {
                    (0..=live.len() as u64 + 1)
                        .find(|c| !live.contains(c))
                        .unwrap_or(u64::MAX)
                }
                if let Some(frame) = self.scope_cleanup_actions.get(frame_idx) {
                    for action in frame {
                        match action {
                            CleanupAction::RcDec { name, .. } if *name == slot.binding_name => {
                                if let Some(local) = self.variables.get(&slot.binding_name).copied()
                                {
                                    nullify_local = Some(local.ptr);
                                }
                            }
                            CleanupAction::RcDecOption {
                                name,
                                option_slot,
                                option_ty,
                                ..
                            } if *name == slot.binding_name => {
                                zero_opt_tag = Some((*option_slot, *option_ty));
                            }
                            CleanupAction::FreeInlineOptionPayload {
                                option_slot,
                                option_ty,
                                some_tag,
                                ..
                            } if Some(*option_slot) == published_ptr => {
                                sentinel_tag_stores.push((
                                    *option_slot,
                                    *option_ty,
                                    tag_not_in(&[*some_tag]),
                                ));
                            }
                            CleanupAction::FreeInlineOptionMapPayload {
                                option_slot,
                                option_ty,
                                some_tag,
                                ..
                            } if Some(*option_slot) == published_ptr => {
                                sentinel_tag_stores.push((
                                    *option_slot,
                                    *option_ty,
                                    tag_not_in(&[*some_tag]),
                                ));
                            }
                            CleanupAction::FreeInlineResultPayload {
                                result_slot,
                                result_ty,
                                ok_tag,
                                err_tag,
                                ..
                            } if Some(*result_slot) == published_ptr => {
                                sentinel_tag_stores.push((
                                    *result_slot,
                                    *result_ty,
                                    tag_not_in(&[*ok_tag, *err_tag]),
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                for (tagged_slot, tagged_ty, sentinel) in sentinel_tag_stores {
                    let tag_ptr = self
                        .builder
                        .build_struct_gep(
                            tagged_ty,
                            tagged_slot,
                            0,
                            &format!("{}_par_suppress_payload_tag", slot.binding_name),
                        )
                        .unwrap();
                    let sentinel_c = self.context.i64_type().const_int(sentinel, false);
                    let _ = self.builder.build_store(tag_ptr, sentinel_c);
                }
                if let Some(ptr) = nullify_local {
                    let null = self.context.ptr_type(AddressSpace::default()).const_null();
                    let _ = self.builder.build_store(ptr, null);
                }
                if let Some((opt_slot, opt_ty)) = zero_opt_tag {
                    let tag_ptr = self
                        .builder
                        .build_struct_gep(
                            opt_ty,
                            opt_slot,
                            0,
                            &format!("{}_par_suppress_tag", slot.binding_name),
                        )
                        .unwrap();
                    let zero = self.context.i64_type().const_int(0, false);
                    let _ = self.builder.build_store(tag_ptr, zero);
                }
                // Ownership-bearing handle / payload cleanups (Map /
                // File / value-enum / value-struct / user-Drop / SoA):
                // the slot-write above published this binding's value
                // to the parent, so the branch's queued action must NOT
                // run — pre-fix it did, and the parent's first use of
                // the slot value was a use-after-free (segfault on the
                // `let name = "ka" + "ra"; let mut m = Map.new();
                // m.insert(..)` auto-par shape: the branch freed the
                // map handle it had just written into the return
                // struct). Unlike the Vec / RC suppressions above,
                // these shapes have no "nothing to drop" sentinel state
                // a store could install (a UserDrop body is arbitrary
                // user code), so the action is REMOVED from the frame
                // and its metadata surfaced through `slot_ownership`;
                // the parent rebinding sites (auto-par dispatch in
                // `stmts.rs`, `compile_par_block` Step 6) re-register
                // the equivalent cleanup against the parent's fresh
                // alloca — the parent becomes the unique owner, exactly
                // like the Vec `track_vec_var` re-track.
                let local_ptr = self.variables.get(&slot.binding_name).map(|v| v.ptr);
                if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                    frame.retain(|action| {
                        let transfer = match action {
                            CleanupAction::FreeMapHandle {
                                map_alloca,
                                key_is_vec,
                                val_is_vec,
                                val_shared_heap_type,
                                key_shared_heap_type,
                                val_drop_fn,
                            } if Some(*map_alloca) == local_ptr => Some(SlotOwnership::Map {
                                key_is_vec: *key_is_vec,
                                val_is_vec: *val_is_vec,
                                val_shared_heap_type: *val_shared_heap_type,
                                key_shared_heap_type: *key_shared_heap_type,
                                val_drop_fn: *val_drop_fn,
                            }),
                            CleanupAction::FreeFileHandle { file_alloca }
                                if Some(*file_alloca) == local_ptr =>
                            {
                                Some(SlotOwnership::File)
                            }
                            CleanupAction::EnumDrop {
                                enum_alloca,
                                drop_fn,
                            } if Some(*enum_alloca) == local_ptr => {
                                Some(SlotOwnership::Enum { drop_fn: *drop_fn })
                            }
                            CleanupAction::StructDrop {
                                struct_alloca,
                                drop_fn,
                            } if Some(*struct_alloca) == local_ptr => {
                                Some(SlotOwnership::Struct { drop_fn: *drop_fn })
                            }
                            CleanupAction::UserDrop {
                                binding_name,
                                drop_fn,
                                ..
                            } if *binding_name == slot.binding_name => {
                                Some(SlotOwnership::User { drop_fn: *drop_fn })
                            }
                            CleanupAction::FreeSoaGroups {
                                soa_alloca,
                                soa_struct_ty,
                                num_hot_groups,
                                has_cold,
                                soa_drop_fn,
                            } if Some(*soa_alloca) == local_ptr => Some(SlotOwnership::Soa {
                                soa_struct_ty: *soa_struct_ty,
                                num_hot_groups: *num_hot_groups,
                                has_cold: *has_cold,
                                soa_drop_fn: *soa_drop_fn,
                            }),
                            // Owned Column / DataFrame / Tensor handles: the
                            // slot-write above published the control-block
                            // pointer to the parent's return slot, so the
                            // branch's own free (three-buffer FreeColumn /
                            // entries-loop FreeDataFrame / null-guarded
                            // FreeTensor) must NOT run — pre-fix it did, and
                            // the parent's first use of the slot value read a
                            // dangling control block (B-2026-07-03-32).
                            CleanupAction::FreeColumn {
                                column_alloca,
                                string_elem,
                            } if Some(*column_alloca) == local_ptr => Some(SlotOwnership::Column {
                                string_elem: *string_elem,
                            }),
                            CleanupAction::FreeDataFrame { df_alloca }
                                if Some(*df_alloca) == local_ptr =>
                            {
                                Some(SlotOwnership::DataFrame)
                            }
                            CleanupAction::FreeTensor { tensor_alloca }
                                if Some(*tensor_alloca) == local_ptr =>
                            {
                                Some(SlotOwnership::Tensor)
                            }
                            _ => None,
                        };
                        match transfer {
                            Some(t) => {
                                slot_ownership.insert(slot.binding_name.clone(), t);
                                false
                            }
                            None => true,
                        }
                    });
                }
            }
            // Recursion suppression (par-slice 4 — same shape as the
            // cancel-bb's cleanup site in `emit_branch_cancel_check`).
            // A user defer body containing a call would route the call
            // through `compile_call` → `emit_branch_cancel_check`,
            // which would walk `scope_cleanup_actions` and re-encounter
            // the SAME UserDefer (still in the frame; only removed
            // when the frame pops). At compile time that's infinite
            // recursion. Branch end is the terminal cleanup site
            // before `ret void` — there's no further cancellation
            // semantics to enforce. Save + null + restore the cancel
            // pointer to suppress nested cancel-checks during the
            // drain.
            let inner_saved_cancel_ptr = self.branch_cancel_ptr.take();
            self.emit_scope_cleanup();
            self.branch_cancel_ptr = inner_saved_cancel_ptr;
            self.builder.build_return(None).unwrap();
        }

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

        stmt_result?;
        Ok(branch_fn.as_global_value().as_pointer_value())
    }

    /// If we are currently compiling a par-branch function body, emit a
    /// cooperative cancel check at the current insertion point: load the
    /// runtime's `AtomicBool` cancel flag, branch to a fresh "cancelled"
    /// block when set, otherwise fall through to a "continue" block. The
    /// cancelled block drains scope cleanup actions and `return`s void
    /// from the branch function, mirroring the entry-time check shape.
    /// No-op outside par branches.
    ///
    /// `callee` is the canonical name of the call about to be emitted (free
    /// fn `name` or `Type.method`). When `Some(name)` and
    /// `callee_effectful[name] == false`, the check is skipped — the
    /// callee carries no `reads`/`writes`/`sends`/`receives`, so a mid-branch
    /// cancellation cannot observe a partial side effect via this call.
    /// `None` (or an unknown name) preserves the conservative MVP behavior.
    pub(super) fn emit_branch_cancel_check(&mut self, label: &str, callee: Option<&str>) {
        let Some(cancel_ptr) = self.branch_cancel_ptr else {
            return;
        };
        if let Some(name) = callee {
            if let Some(false) = self.callee_effectful.get(name) {
                return;
            }
        }
        let i8_ty = self.context.i8_type();
        let cancel_val = self
            .builder
            .build_load(i8_ty, cancel_ptr, &format!("{label}.cancel.flag"))
            .unwrap()
            .into_int_value();
        let is_cancelled = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                cancel_val,
                i8_ty.const_int(0, false),
                &format!("{label}.cancelled"),
            )
            .unwrap();
        let fn_val = self.current_fn.unwrap();
        let cancel_bb = self
            .context
            .append_basic_block(fn_val, &format!("{label}.cancel.bb"));
        let cont_bb = self
            .context
            .append_basic_block(fn_val, &format!("{label}.cont.bb"));
        self.builder
            .build_conditional_branch(is_cancelled, cancel_bb, cont_bb)
            .unwrap();
        self.builder.position_at_end(cancel_bb);
        // Par-cancellation slice 4 (Phase 7 § *Par codegen: cancellation
        // and error propagation*): route through the error-path drain
        // so `errdefer { ... }` blocks registered in the branch body
        // fire on cooperative cancellation (design.md: "observing
        // cancellation — `errdefer(e)` sees `e = Cancelled`"). User
        // `defer { ... }` blocks already drained via `emit_scope_cleanup`,
        // but its UserErrDefer skip (added in defer-codegen slice 2)
        // meant errdefers were silently swallowed at this exit point.
        // The error-path drain runs errdefers in phase 1 (LIFO across
        // frames) then drops + defers in phase 2 — matching the
        // interpreter's per-scope `run_cleanup` shape when
        // `ExitPath::Cancelled` fires (`src/interpreter/eval_stmt.rs`).
        //
        // Recursion suppression: temporarily clear `branch_cancel_ptr`
        // while emitting the cleanup drain. A user defer/errdefer body
        // can contain calls (e.g. `defer { println("x"); }`) which
        // route through `compile_call`, which calls back into
        // `emit_branch_cancel_check` — if the cancel ptr is still
        // set, that re-entry walks `scope_cleanup_actions` again and
        // re-encounters the SAME `UserDefer` / `UserErrDefer` action
        // still living in the outer frame (it's only removed when its
        // containing frame pops). At compile time this is an infinite
        // recursion. Conceptually the cleanup IS the cancel-exit
        // path's terminal work — there's nothing meaningful to
        // re-cancel inside cleanup bodies. Save + null + restore the
        // cancel pointer to suppress nested cancel-checks during this
        // drain.
        let saved_cancel_ptr = self.branch_cancel_ptr.take();
        self.emit_scope_cleanup_for_error_path();
        self.branch_cancel_ptr = saved_cancel_ptr;
        self.builder.build_return(None).unwrap();
        self.builder.position_at_end(cont_bb);
    }
}
