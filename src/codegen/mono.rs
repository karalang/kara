//! Generic-call monomorphization + per-K/V Map specialization.
//!
//! Houses the generic-function compilation pipeline (
//! `compile_generic_call`, `declare_mono_function`, `compile_mono_function`,
//! `infer_type_args`, `unify_type_expr`, `is_known_concrete_type`,
//! `mangle_mono_name`, `verify_bounds_at_codegen`,
//! `llvm_type_satisfies_trait`, `llvm_type_to_mangle_str`)
//! and the per-(K, V) `Map[K, V]` method monomorphization that
//! emits inlined hash / probe / load functions to short-circuit
//! the erased `karac_map_*` runtime path (`mono_map_cache_key`,
//! `should_use_mono_map_for`, `get_or_emit_map_mono_methods`,
//! `emit_mono_map_insert_old_body`, `emit_mono_map_get_body`).

use crate::ast::*;
use std::collections::HashMap;

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::helpers::{const_value_from_literal_expr, const_value_to_mangle_str};
use super::state::{LayoutId, MapMonoMethods, VarSlot};

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn compile_generic_call(
        &mut self,
        name: &str,
        args: &[CallArg],
        explicit_generic_args: Option<&[GenericArg]>,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let generic_fn = self.generic_fns[name].clone();

        // Compile argument values so we can infer concrete types.
        let arg_vals: Vec<BasicValueEnum<'ctx>> = args
            .iter()
            .map(|a| self.compile_expr(&a.value))
            .collect::<Result<_, _>>()?;

        // Infer type arguments from the argument value types.
        let mut subst = self.infer_type_args(&generic_fn, &arg_vals);

        // Const generics slice 1b: process explicit generic args. For
        // each formal param the user supplied an explicit arg for,
        // override the inferred type subst (for type params) or
        // populate a parallel const_subst (for const params). The
        // const_subst flows to `mangle_mono_name` so each distinct
        // const-arg tuple produces a distinct mono symbol. Slice 4
        // will collapse this into a single `SubstValue<'ctx>` shape
        // (fork F2) once codegen body lowering needs const-param
        // identifier resolution.
        let mut const_subst: HashMap<String, crate::prelude::ConstValue> = HashMap::new();
        if let (Some(explicit), Some(gp)) = (explicit_generic_args, &generic_fn.generic_params) {
            for (param, arg) in gp.params.iter().zip(explicit.iter()) {
                match arg {
                    GenericArg::Type(t) => {
                        let llvm_ty = self.llvm_type_for_type_expr(t);
                        subst.insert(param.name.clone(), llvm_ty);
                    }
                    GenericArg::Const(e) => {
                        if let Some(cv) = const_value_from_literal_expr(e) {
                            const_subst.insert(param.name.clone(), cv);
                        }
                    }
                    // Shape args never reach mono — the typechecker's
                    // v1 stub rejects shape-kinded generics before
                    // codegen runs. Benign skip rather than unreachable!
                    // so a bypassed-typecheck path cannot panic here.
                    GenericArg::Shape(_) => {}
                }
            }
        }

        // Slice 0.a sub-step 2 — codegen monomorphization-request bound
        // enforcement (defense-in-depth). The typechecker discharges
        // bounds at every call site (`discharge_type_bounds` /
        // `normalize_bounds_into_where_clause`); this hook fires only
        // for paths that reach codegen with a still-unsatisfied bound
        // (a future cross-module path, or a typechecker-internal call
        // that bypassed the discharge). Covers built-in trait names
        // against primitive LLVM types only — user-trait-on-user-type
        // requires an impl-table threading slice that isn't built yet.
        self.verify_bounds_at_codegen(&generic_fn, &subst)?;

        // Cross-argument `?`-dim equality asserts at the call boundary
        // (design.md § Runtime equality check). For a callee that shares a
        // named `Dim` parameter across two `Tensor` params (the `K` in
        // `matmul(a: [M, K], b: [K, N])`), insert a runtime check that the
        // bound argument dims agree — the type system can't prove two `?`
        // dims equal statically. Emitted here, before the specialization is
        // generated and called, so the trap fires ahead of the operation
        // (and ahead of any tensor read the callee would do out of bounds).
        // The `arg_vals` were just compiled above; a tensor value is a
        // single pointer, so this consults no variable slots.
        self.emit_tensor_crossarg_dim_asserts(&generic_fn, args, &arg_vals)?;

        // Per-layout-monomorphization axis — forward layout-flow inference
        // (`docs/spikes/per-layout-monomorphization.md`). The layout half of
        // the monomorph key: each layout-carrying param's active `LayoutId`,
        // keyed by param name. Slice 1 resolves every entry to `Aos`, so the
        // mangled name below is unchanged and the monomorph is byte-identical
        // to the name-keyed model.
        let layout_subst = self.compute_call_layout_subst(&generic_fn, args);

        // Mangle a unique name for this specialization (e.g. `max$i64`).
        // A generic call carries no backward (return) layout inference yet —
        // that path is the non-generic `ensure_layout_mono_generated` entry —
        // so the return axis is `Aos` here.
        let mangled = self.mangle_mono_name(
            name,
            &generic_fn,
            &subst,
            &const_subst,
            &layout_subst,
            &LayoutId::Aos,
        );

        // Slice 8y: per-call-site decision on whether the caller
        // takes the state-machine intercept path or falls through to
        // a direct call. `true` (state-machine) is the conservative
        // default — it kicks in when the callee has static
        // network-yield effects, when the callee is non-pure-polymorphic,
        // or when no `call_effect_subs` resolution is available. The
        // optimization fires only for callees declared with a
        // purely-polymorphic effect surface (`with E` or `with _`,
        // no fixed portion) whose per-call `E` bindings resolve to
        // an effect set free of `sends(Network)` / `receives(Network)`.
        //
        // Per-mono state-machine helpers stay emitted unconditionally
        // (the four helpers are idempotent across call sites and a
        // future call site of the same mono whose `E` resolves to
        // network-yield will need them). Only the intercept site
        // below consults this flag — direct call when `false`,
        // state-machine invocation when `true`.
        let use_state_machine = self.call_uses_state_machine(call_span, name);

        // Generate the specialization if we haven't done so yet.
        if !self.generated_monos.contains(&mangled) {
            // Mark as in-progress before recursing to avoid infinite loops.
            self.generated_monos.insert(mangled.clone());

            // Save all per-function codegen state — we're about to compile a
            // different function inline.
            let saved_bb = self.builder.get_insert_block();
            let saved_fn = self.current_fn;
            let saved_vars = std::mem::take(&mut self.variables);
            let saved_var_types = std::mem::take(&mut self.var_type_names);
            // The mono body is compiled INLINE, mid-caller — so its tensor
            // param registrations (added by `compile_mono_function`) must not
            // leak into the caller's `tensor_var_infos`, which is keyed by
            // bare var name and would otherwise have a caller-side `a` / `b`
            // overwritten by the callee's same-named tensor param. Swap to a
            // clean slate for the body (module-level tensor bindings are
            // re-seeded inside `compile_mono_function`) and restore below —
            // parallel to `variables` / `var_type_names`.
            let saved_tensor_infos = std::mem::take(&mut self.tensor_var_infos);
            // The mono body manages its OWN scope-cleanup frame stack
            // (pushed/drained in `compile_mono_function`, mirroring
            // `compile_function`). Because the body compiles inline,
            // mid-caller, its frames must not be appended to — or drained
            // out of — the caller's live stack: a callee `let out` cleanup
            // landing on the caller's frame would be emitted in the caller's
            // scope where the callee's alloca doesn't dominate ("Instruction
            // does not dominate all uses"). Swap to an empty stack for the
            // body and restore the caller's below — parallel to `variables`.
            let saved_cleanup = std::mem::take(&mut self.scope_cleanup_actions);
            // A mono body is a top-level function, not a par branch — it must
            // compile with `branch_cancel_ptr = None` so `compile_call`'s
            // cooperative cancel check stays a no-op (the ptr names a par
            // branch fn's cancel param, valid only inside that branch). The
            // body compiles INLINE, so without this an auto-par branch
            // emitted while lowering an EARLIER mono (whose loops
            // parallelized) leaves `branch_cancel_ptr` set, and the NEXT
            // mono's first call emits a cancel check against that stale ptr
            // → "Referring to an argument in another function" + a `ret void`
            // in a value-returning fn. Reset for the body, restore the
            // caller's value below (re-entrant, like `variables`).
            let saved_cancel_ptr = self.branch_cancel_ptr.take();
            let saved_loop_stack = std::mem::take(&mut self.loop_stack);
            let saved_subst = std::mem::replace(&mut self.type_subst, subst.clone());
            // Const generics slice 4: thread the const-arg substitution
            // into the body-lowering pass so `compile_expr Identifier`
            // can resolve const-param refs against it. Parallel to
            // `type_subst`'s save/restore.
            let saved_const_subst = std::mem::replace(&mut self.const_subst, const_subst.clone());
            // Per-layout-monomorphization axis: thread the per-call layout
            // substitution into the body-lowering pass. Parallel to
            // `type_subst` / `const_subst`. Slice 1 always carries `Aos`
            // entries, so body lowering (which doesn't yet consult this map)
            // is unchanged; slice 2 reads it to select the SoA access paths.
            let saved_layout_subst =
                std::mem::replace(&mut self.layout_subst, layout_subst.clone());
            // Slice 4: `compile_mono_function`'s prologue may register SoA
            // borrow params in `ref_params` (a generic fn with a `ref Vec[E]`
            // param whose binding-site layout is SoA). Swap it out for the mono
            // body and restore below, like `variables` — see the matching note
            // in `ensure_layout_mono_generated`.
            let saved_ref_params = std::mem::take(&mut self.ref_params);
            // Slice 5: per-binding layout carrier — the mono body seeds its own
            // locals at their `let` sites; swap out the caller's map and restore
            // below, parallel to `variables` / `ref_params`.
            let saved_binding_layouts = std::mem::take(&mut self.binding_layouts);

            // Declare then compile the specialization.
            self.declare_mono_function(&generic_fn, &mangled)?;
            self.compile_mono_function(&generic_fn, &mangled)?;

            // Slice 8v Phase 2: when the polymorphic source is a
            // network-yielding fn (entry in `program.state_struct_layouts`
            // under its base name), emit per-mono state-machine helpers
            // (state-struct LLVM type + poll-fn + constructor +
            // destructor) under the mangled key. `type_subst` is STILL
            // ACTIVE here — the restore steps run after this — so
            // `llvm_type_for_name("T")` inside the helpers resolves
            // correctly to the per-mono concrete LLVM type. The
            // orchestrator no-ops when the base key isn't in
            // `state_struct_layouts` (non-yielding generic fn — the
            // common case), so the cost for the common path is one
            // HashMap lookup per generic-call mono.
            self.emit_state_machine_helpers_for_mono(name, &mangled);

            // Restore state.
            self.binding_layouts = saved_binding_layouts;
            self.ref_params = saved_ref_params;
            self.layout_subst = saved_layout_subst;
            self.const_subst = saved_const_subst;
            self.type_subst = saved_subst;
            self.loop_stack = saved_loop_stack;
            self.branch_cancel_ptr = saved_cancel_ptr;
            self.scope_cleanup_actions = saved_cleanup;
            self.tensor_var_infos = saved_tensor_infos;
            self.var_type_names = saved_var_types;
            self.variables = saved_vars;
            self.current_fn = saved_fn;
            if let Some(bb) = saved_bb {
                self.builder.position_at_end(bb);
            }
        }

        // Slice 8v Phase 2: per-mono caller-side intercept. When
        // the polymorphic source is a network-yielding fn, the
        // per-mono state-machine helpers were emitted at the mangled
        // key by `emit_state_machine_helpers_for_mono` above. Replace
        // the direct `call @<mangled>(args)` with the state-machine
        // invocation shape — mirrors slice 8d's caller-side intercept
        // (in `src/codegen/call_dispatch.rs`) keyed on the mangled
        // name instead of the source-level callee name:
        //
        //   %state  = call ptr @__kara_state_new_<mangled>()
        //   store args into state struct captured-local fields
        //   br label %kara.poll_loop
        // kara.poll_loop:
        //   %result = call i8 @__kara_poll_<mangled>(ptr %state, ptr null)
        //   %pending = icmp eq i8 %result, 0
        //   br i1 %pending, label %kara.poll_yield, label %kara.poll_done
        // kara.poll_yield:
        //   call i32 @sched_yield()
        //   br label %kara.poll_loop
        // kara.poll_done:
        //   load terminal return value (if non-unit)
        //   call void @free(ptr %state)
        //
        // Slice 8d's incomplete state-struct destructor invocation
        // (the slice ships the destructor but doesn't yet call it
        // from any use site) carries over here — destructor wiring
        // for both the slice 8d and this per-mono intercept is a
        // separate follow-on slice. Cooperative yield (`sched_yield`)
        // matches the slice 8e shape so the parent task doesn't
        // busy-spin between poll-fn invocations.
        //
        // Slice 8y: gate the intercept on the per-call
        // `use_state_machine` decision. When `false`, take the
        // direct-call path even if the per-mono state-machine helpers
        // were emitted earlier (by this or an earlier call site of
        // the same mono).
        let ctor_fn_opt = if use_state_machine {
            self.state_machine_state_constructors.get(&mangled).copied()
        } else {
            None
        };
        if let Some(ctor_fn) = ctor_fn_opt {
            let poll_fn = self
                .state_machine_poll_fns
                .get(&mangled)
                .copied()
                .expect("poll-fn co-emitted with state-machine constructor");
            let state_struct = self
                .state_struct_types
                .get(&mangled)
                .copied()
                .expect("state struct type co-emitted with constructor");
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i8_ty = self.context.i8_type();
            let cur_fn = self
                .builder
                .get_insert_block()
                .and_then(|bb| bb.get_parent())
                .expect("compile_generic_call inside a function context");

            // Allocate the state struct via the constructor helper.
            let state_call = self
                .builder
                .build_call(ctor_fn, &[], "kara.state")
                .expect("call per-mono state-struct constructor");
            let state_ptr = state_call
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();

            // Thread arg values into the state struct's captured-local
            // slots — mirrors slice 8f's discipline. State-struct
            // layout positions parameters first (1..=K after the tag
            // at 0), so arg `i` goes into field `i + 1`. Per-mono
            // emission used the active `type_subst` so the field
            // types match `arg_vals[i].get_type()` for owned-value
            // params.
            //
            // Slice 8z: extend the store discipline to `ref T` /
            // `mut ref T` / `mut Slice[T]` param shapes — without
            // this, the intercept stored a loaded value (Vec struct,
            // i64, etc.) into a ptr- or Slice-struct-shaped state-
            // struct field and produced ill-typed IR that the LLVM
            // verifier rejects. Mirrors slice 8d's non-generic
            // intercept: ref param → `get_data_ptr(var_name)` for
            // Identifier args; ref param → materialize into stack
            // temp for rvalue args (`val` from `arg_vals[i]` is the
            // already-compiled value, alloca + store + optional
            // `track_vec_var` for Vec-struct-shaped rvalues so the
            // heap buffer's scope-exit cleanup queues correctly);
            // `mut Slice[T]` param → `coerce_to_slice(arg, elem_ty)`
            // synthesizes the `{ptr, i64}` slice header at the call
            // site. The tables `fn_param_ref` and
            // `fn_param_slice_elem` are populated by
            // `declare_mono_function` against the mangled key (slice
            // 8z extension) so the lookups resolve to per-mono
            // results that honor the active `type_subst`.
            let ref_flags = self.fn_param_ref.get(&mangled).cloned().unwrap_or_default();
            let slice_elems = self
                .fn_param_slice_elem
                .get(&mangled)
                .cloned()
                .unwrap_or_default();
            for (i, val) in arg_vals.iter().enumerate() {
                let field_idx = (i + 1) as u32;
                let field_ptr = self
                    .builder
                    .build_struct_gep(
                        state_struct,
                        state_ptr,
                        field_idx,
                        &format!("kara.arg{i}.field_ptr"),
                    )
                    .expect("GEP per-mono state struct field for arg");

                let is_ref = ref_flags.get(i).copied().unwrap_or(false);
                let slice_elem = slice_elems.get(i).copied().flatten();

                let to_store: BasicValueEnum<'ctx> = if is_ref {
                    // Ref param: pass a pointer to the caller-side
                    // data, not the loaded value. Identifier args
                    // resolve through `get_data_ptr`; rvalue args
                    // (literals, function returns, arithmetic) get
                    // materialized into an entry-block alloca whose
                    // pointer is stored into the field.
                    if let ExprKind::Identifier(var_name) = &args[i].value.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            self.materialize_rvalue_for_ref_arg(*val, i)
                        }
                    } else if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&args[i].value)? {
                        // `vec[idx]` borrow — element pointer in place
                        // (no shallow-copy + drop double-free). The
                        // pre-compiled `*val` load is left dead (DCE'd).
                        elem_ptr.into()
                    } else {
                        self.materialize_rvalue_for_ref_arg(*val, i)
                    }
                } else if let Some(elem_ty) = slice_elem {
                    // `mut Slice[T]` param: synthesize the slice
                    // header (`{ptr, i64}`) from the arg. Falls
                    // through to the loaded value for shapes the
                    // coercion doesn't recognize (matches the
                    // non-generic intercept's discipline).
                    match self.coerce_to_slice(&args[i].value, elem_ty)? {
                        Some(slice_val) => slice_val,
                        None => *val,
                    }
                } else {
                    *val
                };

                self.builder
                    .build_store(field_ptr, to_store)
                    .expect("store arg into per-mono state struct field");
            }

            let loop_bb = self.context.append_basic_block(cur_fn, "kara.poll_loop");
            let yield_bb = self.context.append_basic_block(cur_fn, "kara.poll_yield");
            let done_bb = self.context.append_basic_block(cur_fn, "kara.poll_done");
            self.builder
                .build_unconditional_branch(loop_bb)
                .expect("br to per-mono poll loop");
            self.builder.position_at_end(loop_bb);
            let null_cancel = ptr_ty.const_null();
            let poll_call = self
                .builder
                .build_call(
                    poll_fn,
                    &[state_ptr.into(), null_cancel.into()],
                    "kara.poll_result",
                )
                .expect("call per-mono poll-fn");
            let poll_result = poll_call
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let is_pending = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    poll_result,
                    i8_ty.const_int(0, false),
                    "kara.is_pending",
                )
                .expect("icmp eq i8 result, 0 for per-mono");
            self.builder
                .build_conditional_branch(is_pending, yield_bb, done_bb)
                .expect("br on per-mono poll discriminant");

            self.builder.position_at_end(yield_bb);
            self.builder
                .build_call(self.sched_yield_fn, &[], "kara.yield_result")
                .expect("call sched_yield for per-mono cooperative yield");
            self.builder
                .build_unconditional_branch(loop_bb)
                .expect("br back to per-mono poll loop after yield");

            self.builder.position_at_end(done_bb);
            // Slice 8i shape: when the mono's return type is non-unit
            // (recorded under the mangled key by
            // `emit_state_struct_type_for_key` when the polymorphic
            // source had a non-unit return type and active `type_subst`
            // resolved to a `state_machine_return_types`-eligible
            // type), load the terminal field BEFORE freeing.
            let call_result =
                if let Some(ret_ty) = self.state_machine_return_types.get(&mangled).copied() {
                    let n_fields = state_struct.count_fields();
                    let terminal_idx = n_fields - 1;
                    let terminal_ptr = self
                        .builder
                        .build_struct_gep(
                            state_struct,
                            state_ptr,
                            terminal_idx,
                            "kara.return.field_ptr",
                        )
                        .expect("GEP per-mono terminal return-value field on caller side");
                    self.builder
                        .build_load(ret_ty, terminal_ptr, "kara.return.value")
                        .expect("load per-mono callee return value from terminal field")
                } else {
                    self.context.i64_type().const_int(0, false).into()
                };
            self.builder
                .build_call(self.free_fn, &[state_ptr.into()], "")
                .expect("call free on per-mono state struct");
            return Ok(call_result);
        }

        // Non-yielding generic call: emit the direct call to the
        // mono'd specialization. This is the common case for
        // generic functions — most user generics aren't network-
        // yielding (only those reachable to `sends(Network)` /
        // `receives(Network)` end up in `state_struct_layouts`).
        let func = match self.module.get_function(&mangled) {
            Some(f) => f,
            None => return Ok(self.context.i64_type().const_int(0, false).into()),
        };

        let compiled_args: Vec<BasicMetadataValueEnum<'ctx>> = arg_vals
            .iter()
            .map(|v| BasicMetadataValueEnum::from(*v))
            .collect();

        let call = self
            .builder
            .build_call(func, &compiled_args, "call")
            .unwrap();

        let basic_val = call.try_as_basic_value();
        if basic_val.is_instruction() {
            Ok(self.context.i64_type().const_int(0, false).into())
        } else {
            Ok(basic_val.unwrap_basic())
        }
    }

    /// Phase 6 line 26 slice 8y: decide whether a generic call site
    /// should take the per-mono state-machine intercept path or fall
    /// through to a direct call.
    ///
    /// Returns `true` (state-machine intercept) when EITHER:
    ///   - the callee is NOT in `state_struct_layouts` — but the
    ///     intercept gate below additionally requires the per-mono
    ///     helpers to exist, so this branch is moot for callees that
    ///     wouldn't take the intercept anyway. We return `false`
    ///     in this case so the predicate stays parsimonious.
    ///   - the callee IS in `state_struct_layouts` AND is NOT in
    ///     `callee_purely_polymorphic_effects` — callees with static
    ///     fixed effects (`Explicit` or `PolymorphicWithFixed`) may
    ///     carry `sends(Network)` / `receives(Network)` in the static
    ///     portion regardless of any `with E` resolution, so the
    ///     intercept must fire to drive their internal yields.
    ///   - the callee IS purely polymorphic AND `call_effect_subs[span]`
    ///     records at least one effect-variable binding to a
    ///     network-yield verb (`sends(Network)` / `receives(Network)`):
    ///     state-machine path needed.
    ///
    /// Returns `false` (direct call) when the callee is purely
    /// polymorphic AND all of its `call_effect_subs[span]` bindings
    /// resolve to a non-network effect set, or when no entry is
    /// present at all (the callee has no effect-variable parameters
    /// at all, which today indicates a `with _` anonymous polymorphic
    /// surface — conservative `true` keeps the intercept in that
    /// case).
    ///
    /// **Soundness caveat:** for a private fn whose body contains
    /// static yield points (e.g. `fn op[T, with E](cb: Fn() with E)
    /// with E { fetch(); cb(); }`), the callee's body parks at
    /// `fetch()` regardless of `E`. The current architecture's
    /// `state_struct_layouts` population coupling — only populated
    /// when the body contains static yield points — means the
    /// optimization's only reachable scenario co-occurs with body
    /// yields, and the skip is technically unsound in production
    /// (the direct-call path would block at the body's internal
    /// fetch). The v1 test-harness `fetch` stubs are empty-bodied
    /// so the skip is harmless in tests; production correctness
    /// awaits a follow-on slice that decouples `state_struct_layouts`
    /// population from the body-yield-points requirement (broadens
    /// the candidate pool to purely-polymorphic-no-body-yield
    /// callees, after which the slice 8y gate fires soundly).
    pub(super) fn call_uses_state_machine(
        &self,
        call_span: &crate::token::Span,
        base_key: &str,
    ) -> bool {
        let snap = match self.program_snapshot.as_ref() {
            Some(s) => s,
            None => return false,
        };
        if !snap.state_struct_layouts.contains_key(base_key) {
            return false;
        }
        if !snap.callee_purely_polymorphic_effects.contains(base_key) {
            return true;
        }
        let key = (call_span.offset, call_span.length);
        let bindings = match snap.call_effect_subs.get(&key) {
            Some(b) => b,
            None => return true,
        };
        bindings.values().any(|effects| {
            effects
                .iter()
                .any(|e| (e.verb == "sends" || e.verb == "receives") && e.resource == "Network")
        })
    }

    /// Declare the LLVM function for a monomorphized specialization.
    /// `type_subst` must already be populated before calling this.
    /// Slice 8z: materialize a non-place rvalue arg into an entry-block
    /// alloca so the `ref T` per-mono caller-side intercept can store
    /// the resulting `ptr` into the state struct's field. Mirrors
    /// slice 8d's identical mechanic in `compile_call` — a literal /
    /// arithmetic / function-return arg bound to a `ref T` param has
    /// no addressable storage, so codegen mints one. Vec-struct-shaped
    /// values (Vec / VecDeque / String) get queued for scope-exit
    /// `FreeVecBuffer` via `track_vec_var` so the heap buffer's
    /// cleanup runs at the caller's scope boundary; primitives and
    /// pointer-shaped temporaries (string literals, etc.) need no
    /// such tracking. Slice 8ad widened visibility to `pub(super)` so
    /// the non-generic state-machine intercept in `call_dispatch.rs`
    /// can call this same helper for its `ref T` rvalue path.
    pub(super) fn materialize_rvalue_for_ref_arg(
        &mut self,
        val: BasicValueEnum<'ctx>,
        arg_idx: usize,
    ) -> BasicValueEnum<'ctx> {
        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .expect("compile_generic_call or compile_call inside a function context");
        let temp = self.create_entry_alloca(
            cur_fn,
            &format!("kara.arg{arg_idx}.ref_rvalue"),
            val.get_type(),
        );
        self.builder
            .build_store(temp, val)
            .expect("store rvalue value into ref-arg materialization slot");
        if self.llvm_ty_is_vec_struct(val.get_type()) {
            self.track_vec_var(temp, None);
        }
        temp.into()
    }

    /// Generate (declare + compile) a per-layout monomorph of a *non-generic*
    /// function under `mangled`, with `layout_subst` active so its `Vec[E]`
    /// params lower SoA against the caller's argument layout (slice 2) and
    /// `return_layout` active so a non-`Aos` return lowers the LLVM return type
    /// to the SoA struct and the returned local(s) build SoA (slice 3). The
    /// non-specialized (all-`Aos`) body was already compiled in the normal
    /// module pass; this adds the SoA variant as a distinct symbol. Idempotent
    /// via `generated_monos`. Mirrors `compile_generic_call`'s mono-entry
    /// save/restore, with empty type/const substs (a non-generic callee has no
    /// type/const params) — and restores even on error so a failed body can't
    /// leave a half-swapped builder/var state behind.
    pub(super) fn ensure_layout_mono_generated(
        &mut self,
        func: &Function,
        mangled: &str,
        layout_subst: HashMap<String, LayoutId>,
        return_layout: LayoutId,
    ) -> Result<(), String> {
        if self.generated_monos.contains(mangled) {
            return Ok(());
        }
        self.generated_monos.insert(mangled.to_string());

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_tensor_infos = std::mem::take(&mut self.tensor_var_infos);
        let saved_cleanup = std::mem::take(&mut self.scope_cleanup_actions);
        let saved_cancel_ptr = self.branch_cancel_ptr.take();
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_subst = std::mem::take(&mut self.type_subst);
        let saved_const_subst = std::mem::take(&mut self.const_subst);
        let saved_layout_subst = std::mem::replace(&mut self.layout_subst, layout_subst);
        let saved_return_layout = std::mem::replace(&mut self.return_layout, return_layout);
        // Slice 4: the mono prologue now registers SoA `ref`/`mut ref Vec[E]`
        // params in `ref_params` (so the access paths deref the slot once).
        // `ref_params` is per-function state the caller doesn't otherwise swap
        // out, so take it for the mono body (empty → the prologue rebuilds it
        // for this mono's own params) and restore the caller's map after —
        // mirroring the `variables` save/restore above. Without this a mono's
        // ref param would mark a same-named caller binding as a borrow.
        let saved_ref_params = std::mem::take(&mut self.ref_params);
        // Slice 5: the mono body seeds its own locals' layouts in
        // `binding_layouts` at their `let` sites. Take the caller's carrier for
        // the duration (the body starts empty, like `variables`) and restore it
        // after, so a mono's local can't leak its SoA-ness back to a same-named
        // caller binding.
        let saved_binding_layouts = std::mem::take(&mut self.binding_layouts);

        let result = self
            .declare_mono_function(func, mangled)
            .and_then(|_| self.compile_mono_function(func, mangled));

        self.binding_layouts = saved_binding_layouts;
        self.ref_params = saved_ref_params;
        self.return_layout = saved_return_layout;
        self.layout_subst = saved_layout_subst;
        self.const_subst = saved_const_subst;
        self.type_subst = saved_subst;
        self.loop_stack = saved_loop_stack;
        self.branch_cancel_ptr = saved_cancel_ptr;
        self.scope_cleanup_actions = saved_cleanup;
        self.tensor_var_infos = saved_tensor_infos;
        self.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        result
    }

    pub(super) fn declare_mono_function(
        &mut self,
        func: &Function,
        mangled: &str,
    ) -> Result<FunctionValue<'ctx>, String> {
        let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = func
            .params
            .iter()
            .map(|p| self.llvm_param_type(p))
            .collect();
        // Per-layout-monomorphization (slice 2): a `Vec[E]` param whose active
        // `LayoutId` (in the current monomorph's `layout_subst`) is `Soa` is
        // passed as the 4-field SoA struct, not the AoS `{ptr,len,cap}` Vec —
        // the caller holds that SoA struct for the argument binding. Mirrors
        // the name-keyed by-value signature patch (functions.rs); keyed on the
        // layout subst, not the param name, so it crosses call boundaries
        // regardless of binding name. No-op outside a layout-monomorph.
        for (i, p) in func.params.iter().enumerate() {
            if let Some(soa) = self.active_param_soa_layout(p) {
                let soa_ty = self.soa_vec_type(soa.num_groups, soa.cold_group.is_some());
                param_types[i] = soa_ty.into();
            }
        }

        // Per-layout-monomorphization backward axis (slice 3): a non-`Aos`
        // return layout lowers the LLVM return type to the 4-field SoA struct
        // (`soa_vec_type`), not the AoS `{ptr,len,cap}` the declared `Vec[E]`
        // would give. The caller binds the result into its SoA slot; the body
        // builds + returns the SoA struct. No-op outside a return-SoA mono.
        let soa_return = match &self.return_layout {
            LayoutId::Soa(block) => self.soa_layouts.get(block).cloned(),
            LayoutId::Aos => None,
        };
        let fn_type = if let Some(soa) = soa_return {
            let soa_ty = self.soa_vec_type(soa.num_groups, soa.cold_group.is_some());
            soa_ty.fn_type(&param_types, false)
        } else {
            match self.llvm_return_type(&func.return_type) {
                Some(BasicTypeEnum::IntType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::FloatType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::PointerType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::StructType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::ArrayType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::VectorType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::ScalableVectorType(_)) | None => {
                    self.context.void_type().fn_type(&param_types, false)
                }
            }
        };

        // Slice 8z: mirror the non-generic `declare_one_function` ref /
        // slice-elem table population for the mangled per-mono key.
        // Without this, slice 8d's caller-side arg-passing rules (ref →
        // pass pointer, mut Slice → coerce to slice header) are
        // unreachable from `compile_generic_call`'s per-mono state-
        // machine intercept — the intercept's arg-store loop falls
        // through to "store loaded value" for ref / slice params and
        // mints stores of the wrong LLVM type into the ptr / Slice-
        // struct-shaped state-struct field. Type-parameter-typed ref
        // (`ref T`) keeps `ref_flag: true` regardless of T's
        // resolution; `mut Slice[T]`'s element type resolves through
        // `extract_slice_elem_type` → `llvm_type_for_type_expr`, which
        // honors the active `type_subst`.
        let ref_flags: Vec<bool> = func
            .params
            .iter()
            .map(|p| matches!(&p.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
            .collect();
        self.fn_param_ref.insert(mangled.to_string(), ref_flags);
        let slice_elems: Vec<Option<BasicTypeEnum<'ctx>>> = func
            .params
            .iter()
            .map(|p| self.extract_slice_elem_type(&p.ty))
            .collect();
        self.fn_param_slice_elem
            .insert(mangled.to_string(), slice_elems);

        Ok(self.module.add_function(mangled, fn_type, None))
    }

    /// Compile the body of a monomorphized specialization.
    /// `type_subst` must already be populated and per-function state must be fresh.
    pub(super) fn compile_mono_function(
        &mut self,
        func: &Function,
        mangled: &str,
    ) -> Result<(), String> {
        let fn_val = self
            .module
            .get_function(mangled)
            .ok_or_else(|| format!("Mono '{}' not declared", mangled))?;

        self.current_fn = Some(fn_val);
        self.variables.clear();
        self.var_type_names.clear();
        // Per-binding layout carrier (slice 5): the caller's map was swapped out
        // (`mem::take`) at the mono entry point, so this fresh body starts empty
        // and seeds its own locals; `let`-site registrations land here.
        self.binding_layouts.clear();
        self.inline_option_payload_vars.clear();
        self.inline_result_payload_vars.clear();
        self.inline_option_map_payload_vars.clear();
        // Function-level scope-cleanup frame for owned locals (`Tensor` /
        // `Vec` / `String` / `Map` lets needing drop), mirroring
        // `compile_function`. The caller's frame stack was swapped out in
        // `compile_generic_call`, so this is the body's sole, fresh stack;
        // let-site registrations land here and drain at the tail return
        // below. Without it, a mono body's `let out = Tensor.zeros(…)`
        // FreeTensor cleanup leaked into the caller's frame and was emitted
        // where the callee's alloca didn't dominate ("Instruction does not
        // dominate all uses").
        self.scope_cleanup_actions.clear();
        self.scope_cleanup_actions.push(Vec::new());
        // Slice 10: reseed module-binding side-tables in monomorphised
        // bodies too (same reason as the `compile_function` path —
        // `var_type_names` is cleared per function).
        self.reseed_module_binding_side_tables();

        let entry = self.context.append_basic_block(fn_val, "entry");
        self.builder.position_at_end(entry);

        for (i, param) in func.params.iter().enumerate() {
            let param_name = self.param_name(param);
            let param_val = fn_val.get_nth_param(i as u32).unwrap();
            // Per-layout-monomorphization (slice 2): a `Vec[E]` param whose
            // active `LayoutId` is `Soa` arrives as the 4-field SoA struct
            // (the signature was patched in `declare_mono_function`). Spill it
            // to a slot typed as the SoA struct and register the binding so the
            // body's access paths (`active_soa_layout`) lower SoA against it.
            // Ownership is CALLER-RETAINS, mirroring the name-keyed by-value
            // path (functions.rs): the callee borrows the moved-in 4-field
            // header sharing the caller's group buffers, so NO `FreeSoaGroups`
            // cleanup here — the caller's binding frees them exactly once.
            if let Some(soa) = self.active_param_soa_layout(param) {
                let soa_ty = self.soa_vec_type(soa.num_groups, soa.cold_group.is_some());
                let alloca = self.create_entry_alloca(fn_val, &param_name, soa_ty.into());
                self.builder.build_store(alloca, param_val).unwrap();
                self.variables.insert(
                    param_name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: soa_ty.into(),
                    },
                );
                continue;
            }
            let alloca = self.create_entry_alloca(fn_val, &param_name, param_val.get_type());
            self.builder.build_store(alloca, param_val).unwrap();
            // Per-layout-monomorphization (slice 4): a SoA-carrying `ref`/
            // `mut ref Vec[E]` param arrives as a POINTER to the caller's SoA
            // struct (the signature is the pointer ABI — `active_param_soa_layout`
            // returned `None` for the borrow form, so the by-value SoA branch
            // above was skipped). Register it in `ref_params` so the SoA access
            // paths (`compile_soa_index_read` / `compile_soa_method`) deref the
            // slot once before GEPing groups/len — exactly the by-ref-reads
            // discipline `compile_function` applies, which the mono prologue
            // otherwise omits. Guarded on `active_soa_layout` (true iff
            // `layout_subst[param]` is `Soa`) so only SoA borrow params get the
            // entry; an `Aos` ref Vec param is unaffected. Without this the
            // access path reads the pointer bytes as the SoA struct → garbage
            // len → SIGTRAP, the same silent miscompile the same-name by-ref
            // path fixed for `compile_function`.
            if self.active_soa_layout(&param_name).is_some() {
                if let Some(inner_ty) = self.inner_type_of_ref(&param.ty) {
                    self.ref_params.insert(param_name.clone(), inner_ty);
                }
            }
            // Track declared type name for struct/enum field resolution.
            if let TypeKind::Path(path) = &param.ty.kind {
                if let Some(type_name) = path.segments.first() {
                    self.var_type_names
                        .insert(param_name.clone(), type_name.clone());
                }
            }
            // Register `Tensor` params in `tensor_var_infos` so the body's
            // multi-dim index (`a[i, j]`), `shape()`/`rank()`, and the
            // shape-transform family recognize them. Without this a
            // shape-generic body — `fn f[M, K, N](a: Tensor[T, [M, K]], …)`
            // indexing / `shape()`-ing its tensor params — failed codegen
            // with "Index operator applied to non-array type" (the params
            // were bound as opaque pointers only). The shape literal's named
            // `Dim` params (`M`, `K`) carry no static value, so they lower to
            // runtime `?` dims read from the header; the element type
            // resolves through the active `type_subst` (set by
            // `compile_generic_call` around this call). Mirrors
            // `compile_function`'s `register_var_from_type_expr` for the
            // tensor case — the other collection side-tables stay on the
            // minimal mono binding (full registration would change cleanup
            // behavior for existing generic Vec/Map/String fns). A
            // `ref Tensor` param registers off its inner type (the by-value
            // ref-tensor ABI hands back the same block pointer).
            let tensor_te = match &param.ty.kind {
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                _ => &param.ty,
            };
            if let Some(info) = self.tensor_var_info_from_type_expr(tensor_te) {
                self.tensor_var_infos.insert(param_name.clone(), info);
            }
            self.variables.insert(
                param_name,
                VarSlot {
                    ptr: alloca,
                    ty: param_val.get_type(),
                },
            );
        }

        // Per-layout-monomorphization backward axis (slice 3): in a return-SoA
        // mono, seed the local(s) that flow to the return value with the
        // receiving binding's layout, so the body's construction
        // (`let out = Vec.new()`), mutation (`out.push(…)`), and tail
        // (`out`) all lower SoA via `active_soa_layout` — and the returned
        // value is the 4-field SoA struct the patched signature
        // (`declare_mono_function`) returns. Seeding happens AFTER the param
        // prologue so a returned local never shadows a SoA param's slot.
        // No-op outside a return-SoA mono.
        let ret_block = match &self.return_layout {
            LayoutId::Soa(block) => Some(block.clone()),
            LayoutId::Aos => None,
        };
        if let Some(block) = ret_block {
            for name in self.soa_return_local_names(&func.body) {
                self.layout_subst.insert(name, LayoutId::Soa(block.clone()));
            }
        }

        let result = self.compile_block(&func.body)?;

        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            // Drain the function-level cleanup frame at the tail return,
            // mirroring `compile_function`. Move-aware suppression first:
            // when the body's tail is a bare Identifier naming an owned
            // local that is moved out as the return value (`matmul`'s
            // `out`), null its slot / flip its sentinel so the
            // `FreeTensor` / `FreeVecBuffer` walk skips it — the caller now
            // owns the value. (Early `return` statements drain via their
            // own path in `compile_expr`; that path is reached only when
            // the block left a terminator, so it's excluded here.)
            self.suppress_cleanup_for_tail_return(&func.body);
            self.emit_scope_cleanup();
            if let Some(val) = result {
                self.builder.build_return(Some(&val)).unwrap();
            } else {
                self.builder.build_return(None).unwrap();
            }
        }
        // Leave the frame stack as the caller swapped it in
        // (`compile_generic_call` restores its own); clearing keeps the
        // post-body state tidy and matches `compile_function`'s exit.
        self.scope_cleanup_actions.clear();

        Ok(())
    }

    /// The local binding name(s) that flow to this function's return value as
    /// a bare `Vec[E]` identifier — used by the return-SoA monomorph path
    /// (slice 3) to seed them with the receiving binding's layout so the body
    /// builds + returns the SoA struct. Detection MIRRORS
    /// `suppress_cleanup_for_tail_return`'s tail analysis (the block's
    /// `final_expr`, or the last statement's `return <expr>;` value): seeding
    /// and the matching move-out suppression must agree on the same name, or a
    /// returned local would build SoA without its `FreeSoaGroups` suppressed
    /// (leak) or be suppressed without building SoA (type mismatch). Only a
    /// bare identifier qualifies; branch-leaf / multi-`return` returns are a
    /// follow-on slice (spike §8) — they degrade to AoS, never miscompile.
    fn soa_return_local_names(&self, body: &Block) -> Vec<String> {
        let mut names = Vec::new();
        let from_final = body.final_expr.as_deref();
        let from_last_stmt = body.stmts.last().and_then(|s| match &s.kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Return(Some(boxed)) => Some(boxed.as_ref()),
                _ => Some(e),
            },
            _ => None,
        });
        if let Some(expr) = from_final.or(from_last_stmt) {
            if let ExprKind::Identifier(name) = &expr.kind {
                names.push(name.clone());
            }
        }
        names
    }

    /// Infer the type-parameter substitution for a generic function call by
    /// matching each parameter's declared type against the concrete argument type.
    pub(super) fn infer_type_args(
        &self,
        func: &Function,
        arg_vals: &[BasicValueEnum<'ctx>],
    ) -> HashMap<String, BasicTypeEnum<'ctx>> {
        let mut subst = HashMap::new();
        for (param, val) in func.params.iter().zip(arg_vals.iter()) {
            self.unify_type_expr(&param.ty, val.get_type(), &mut subst);
        }
        subst
    }

    /// Recursively match a declared type expression against a concrete LLVM type,
    /// recording bindings for any unbound type parameters found.
    pub(super) fn unify_type_expr(
        &self,
        ty: &TypeExpr,
        concrete: BasicTypeEnum<'ctx>,
        subst: &mut HashMap<String, BasicTypeEnum<'ctx>>,
    ) {
        if let TypeKind::Path(path) = &ty.kind {
            if path.segments.len() == 1 && path.generic_args.is_none() {
                let name = &path.segments[0];
                // Treat as a type parameter if it's not a known concrete type.
                if !self.is_known_concrete_type(name) {
                    subst.entry(name.clone()).or_insert(concrete);
                }
            }
            // TODO: unify generic args (e.g. `Vec[T]`) when container types are codegen'd.
        }
    }

    /// Returns true if `name` is a built-in concrete type or a declared struct/enum.
    pub(super) fn is_known_concrete_type(&self, name: &str) -> bool {
        matches!(
            name,
            "i8" | "i16"
                | "i32"
                | "i64"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "isize"
                | "usize"
                | "f32"
                | "f64"
                | "bool"
                | "str"
                | "String"
                | "char"
        ) || self.struct_types.contains_key(name)
            || self.enum_layouts.contains_key(name)
    }

    /// Build a mangled name for a specialization, e.g. `max$i64` or `zip$i64$f64`.
    ///
    /// `layout_subst` adds the per-layout-monomorphization axis: a layout
    /// suffix (`$soa_<name>`) for any layout-carrying value param whose active
    /// `LayoutId` is non-`Aos`, so each layout variant is a distinct LLVM
    /// symbol (`docs/spikes/per-layout-monomorphization.md` §4.3). `Aos`
    /// contributes no suffix, so an all-`Aos` call keeps the existing symbol.
    /// `return_layout` adds the backward-inference axis (slice 3): a non-`Aos`
    /// *return* layout appends a `$ret_soa_<name>` suffix, so a helper called
    /// to return one layout vs. another (or vs. plain AoS) is a distinct symbol.
    pub(super) fn mangle_mono_name(
        &self,
        base: &str,
        func: &Function,
        subst: &HashMap<String, BasicTypeEnum<'ctx>>,
        const_subst: &HashMap<String, crate::prelude::ConstValue>,
        layout_subst: &HashMap<String, LayoutId>,
        return_layout: &LayoutId,
    ) -> String {
        let mut mangled = base.to_string();
        // Type / const generic axes (only for a generic function — a
        // non-generic layout-monomorph has no `generic_params`).
        if let Some(gp) = &func.generic_params {
            for param in &gp.params {
                // Const generics slice 1b: const params take priority over
                // type subst when both maps are populated (the const_subst
                // is keyed by formal name, the type subst doesn't carry
                // const params).
                if param.is_const {
                    if let Some(cv) = const_subst.get(&param.name) {
                        mangled.push('$');
                        mangled.push_str(&const_value_to_mangle_str(cv));
                    }
                } else if let Some(ty) = subst.get(&param.name) {
                    mangled.push('$');
                    mangled.push_str(&self.llvm_type_to_mangle_str(*ty));
                }
            }
        }
        // Per-layout-monomorphization axis: append a per-param layout suffix
        // for any value param carrying a non-`Aos` layout. Applies to generic
        // and non-generic functions alike (slice 2 monomorphizes plain `Vec[E]`
        // helpers per the caller's arg layout). The param NAME is part of the
        // suffix (`$<param>_soa_<layout>`) so that two different layout
        // assignments over the same params can't collide — e.g. `f(grid,plain)`
        // (`$a_soa_grid`) vs `f(plain,grid)` (`$b_soa_grid`) are distinct
        // monomorphs. An all-`Aos` call adds no suffix, so the symbol is
        // unchanged for non-SoA code.
        for param in &func.params {
            if let Some(name) = param.name() {
                if let Some(suffix) = layout_subst.get(name).and_then(LayoutId::mangle_suffix) {
                    mangled.push('$');
                    mangled.push_str(name);
                    mangled.push('_');
                    mangled.push_str(&suffix);
                }
            }
        }
        // Per-layout-monomorphization backward axis (slice 3): a non-`Aos`
        // return layout appends `$ret_soa_<name>`. Disjoint from the per-param
        // `$<param>_soa_<name>` suffixes (the `ret` keyword can't be a param
        // name), so a fn that both takes and returns SoA gets both.
        if let Some(suffix) = return_layout.mangle_suffix() {
            mangled.push_str("$ret_");
            mangled.push_str(&suffix);
        }
        mangled
    }

    /// Forward layout-flow inference for a call
    /// (`docs/spikes/per-layout-monomorphization.md` §4.2): the `LayoutId` of
    /// each layout-carrying (`Vec[E]`) value param, keyed by param name. This
    /// is the layout half of the monomorph key fed to `mangle_mono_name` and
    /// (slice 2) to body lowering via `self.layout_subst`.
    ///
    /// **Forward (arguments):** a param's `LayoutId` is the binding-site layout
    /// of the matching argument's *root* — but only when the argument is a bare
    /// binding (a whole `Vec[E]`). A projection (`grid[i]`, `g.field`) yields a
    /// materialized AoS element/field, so it is `Aos`; nested layout-through-
    /// aggregate flow is deferred (spike §8). When the matching argument's root
    /// is a `layout`-declared / SoA-forwarded binding, the param is `Soa(name)`,
    /// monomorphizing the callee against the caller's physical layout.
    pub(super) fn compute_call_layout_subst(
        &self,
        func: &Function,
        args: &[CallArg],
    ) -> HashMap<String, LayoutId> {
        let mut layout_subst = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if !Self::param_is_layout_carrying(param) {
                continue;
            }
            let Some(name) = param.name() else { continue };
            let layout = args
                .get(i)
                .map(|a| self.arg_root_layout_id(&a.value))
                .unwrap_or(LayoutId::Aos);
            layout_subst.insert(name.to_string(), layout);
        }
        layout_subst
    }

    /// The `LayoutId` an argument expression contributes to forward inference.
    /// Only a bare binding (whole `Vec[E]`) carries its binding-site layout; any
    /// other shape (projection, call result, literal) is `Aos` for the first
    /// slices (top-level `Vec[E]` only — spike §8).
    fn arg_root_layout_id(&self, expr: &Expr) -> LayoutId {
        match &expr.kind {
            ExprKind::Identifier(name) => self.active_layout_id(name),
            _ => LayoutId::Aos,
        }
    }

    /// The active physical layout of a binding at a *use site* in the current
    /// codegen context, read purely from the value carriers (slice 5 — no
    /// name-keyed `soa_layouts` lookup): the per-call layout subst (a
    /// SoA-forwarded param/return in the active monomorph) takes precedence,
    /// then the per-binding `binding_layouts` carrier (an in-function local
    /// seeded at its binding site by `seed_binding_site_layout`), else `Aos`.
    /// This is design.md Feature 1's "the value carrier is a `LayoutId`
    /// attached to bindings, not the binding name": a binding reads as SoA iff
    /// it was *made* SoA — by the call dispatch (`layout_subst`) or at its `let`
    /// (`binding_layouts`) — so a base-symbol param that merely shares a name
    /// with a `layout` block no longer lowers SoA by coincidence.
    pub(super) fn active_layout_id(&self, binding_name: &str) -> LayoutId {
        if let Some(layout) = self.layout_subst.get(binding_name) {
            return layout.clone();
        }
        if let Some(layout) = self.binding_layouts.get(binding_name) {
            return layout.clone();
        }
        LayoutId::Aos
    }

    /// Resolve a `let` binding's layout from its binding *site* and, if SoA,
    /// seed the per-binding `binding_layouts` carrier so every downstream use
    /// reads it via `active_layout_id` (no further name-keyed lookups). This is
    /// the **one sanctioned origin name-match** (design.md Feature 1: "layout
    /// binds to the binding site"): the binding's layout is the active
    /// `layout_subst` entry if present — a returned local seeded by a return-SoA
    /// mono (slice 3), or a name the dispatch already laid out — otherwise the
    /// `layout <name>` origin map keyed by the binding's own name. Returns the
    /// resolved `SoaLayout` (and records the carrier) for a SoA binding, or
    /// `None` for an `Aos` one. Called only from the `let` arm; use sites read
    /// `active_soa_layout`, which never touches the origin map.
    pub(super) fn seed_binding_site_layout(
        &mut self,
        binding_name: &str,
    ) -> Option<super::state::SoaLayout> {
        let layout = if let Some(layout) = self.layout_subst.get(binding_name) {
            layout.clone()
        } else if self.soa_layouts.contains_key(binding_name) {
            LayoutId::Soa(binding_name.to_string())
        } else {
            LayoutId::Aos
        };
        match layout {
            LayoutId::Soa(block) => {
                self.binding_layouts
                    .insert(binding_name.to_string(), LayoutId::Soa(block.clone()));
                self.soa_layouts.get(&block).cloned()
            }
            LayoutId::Aos => None,
        }
    }

    /// The `SoaLayout` metadata for a binding whose active layout is `Soa`, or
    /// `None` when it is `Aos`. Resolves the `Soa(<block-name>)` id through the
    /// `soa_layouts` origin map. The single body-lowering trigger that replaces
    /// the raw `soa_layouts.get(name)` / `.contains_key(name)` access checks, so
    /// a mono SoA param (not itself a `layout`-block name) lowers SoA.
    pub(super) fn active_soa_layout(&self, binding_name: &str) -> Option<super::state::SoaLayout> {
        match self.active_layout_id(binding_name) {
            LayoutId::Soa(block) => self.soa_layouts.get(&block).cloned(),
            LayoutId::Aos => None,
        }
    }

    /// The `SoaLayout` for a value param whose active `LayoutId` (in the current
    /// monomorph's `layout_subst`) is `Soa` — drives the SoA param signature
    /// and prologue in the mono path. Returns `None` outside a layout-monomorph
    /// (empty `layout_subst`), so the normal `compile_function` pass is
    /// unaffected and the name-keyed declaring-fn path still applies.
    pub(super) fn active_param_soa_layout(&self, param: &Param) -> Option<super::state::SoaLayout> {
        // By-value only (slice 4): a `ref`/`mut ref Vec[E]` SoA param keeps its
        // pointer ABI — the caller passes `&struct` and the mono body derefs
        // once through `ref_params` — so its *signature* is NOT patched to the
        // SoA struct by value. Only an owned by-value `Vec[E]` param's
        // signature becomes the 4-field SoA struct. (The param still carries a
        // `Soa` entry in `layout_subst`, which drives the body's access paths
        // via `active_soa_layout`; this guard only suppresses the signature
        // rewrite for the borrow forms.)
        if matches!(&param.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
            return None;
        }
        let name = param.name()?;
        match self.layout_subst.get(name) {
            Some(LayoutId::Soa(block)) => self.soa_layouts.get(block).cloned(),
            _ => None,
        }
    }

    /// Whether a value-or-borrow param's declared type is a layout-carrying
    /// collection — a `Vec[E]` (owned `Vec[E]`, `ref Vec[E]`, or
    /// `mut ref Vec[E]`) whose physical layout the per-layout-monomorphization
    /// axis can vary (`Aos` vs an SoA grouping). Peels one `ref`/`mut ref` so
    /// borrow forms also gate the dispatch + populate `layout_subst` (slice 4:
    /// a SoA buffer through a shared by-ref helper monomorphizes per the
    /// caller's buffer layout, regardless of the param name). The *signature*
    /// difference between owned and borrow forms is handled downstream by
    /// `active_param_soa_layout` (by-value gets the SoA struct; borrow keeps
    /// the pointer ABI and derefs in the body).
    pub(super) fn param_is_layout_carrying(param: &Param) -> bool {
        let underlying = match &param.ty.kind {
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) => &inner.kind,
            other => other,
        };
        matches!(
            underlying,
            TypeKind::Path(path) if path.segments.last().map(String::as_str) == Some("Vec")
        )
    }

    /// Whether a function's declared return type is a layout-carrying `Vec[E]`
    /// — the backward-inference (slice 3) analog of `param_is_layout_carrying`.
    /// Gates the return-SoA monomorph: only a function that returns a whole
    /// `Vec[E]` can be specialized to return an SoA struct.
    pub(super) fn return_is_layout_carrying(func: &Function) -> bool {
        matches!(
            func.return_type.as_ref().map(|t| &t.kind),
            Some(TypeKind::Path(path)) if path.segments.last().map(String::as_str) == Some("Vec")
        )
    }

    /// Whether a `let`-binding RHS is a direct call to a known user function
    /// whose return type is a layout-carrying `Vec[E]` — the gate for the
    /// backward-inference SoA-let path (slice 3). Matches `compile_call`'s
    /// callee-name extraction (bare identifier / single-segment path), so the
    /// callee resolved here is exactly the one the dispatch monomorphizes.
    /// Excludes `Vec.new()` (a 2-segment `Vec::new` path handled by
    /// `compile_soa_new`) and any non-`fn_asts` callee (intrinsics, generics),
    /// keeping the SoA-let path in lockstep with the backward dispatch — so the
    /// bound call result is always the SoA struct the slot expects.
    pub(super) fn let_rhs_calls_layout_returning_fn(&self, value: &Expr) -> bool {
        let ExprKind::Call { callee, .. } = &value.kind else {
            return false;
        };
        let name = match &callee.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::Path {
                segments,
                generic_args: None,
            } if segments.len() == 1 => segments[0].as_str(),
            _ => return false,
        };
        self.fn_asts
            .get(name)
            .is_some_and(Self::return_is_layout_carrying)
    }

    /// Slice 0.a sub-step 2 — codegen monomorphization-request bound
    /// enforcement.
    ///
    /// Walks both inline-form (`fn f[T: Bound]`) and where-clause
    /// (`fn f[T] where T: Bound`) bounds against the concrete LLVM
    /// substitution. Returns `Err` when a primitive LLVM type
    /// demonstrably fails to satisfy a built-in trait bound (e.g.
    /// `f64` for `Hash` / `Eq` / `Ord`), matching the typechecker's
    /// `type_supports_*` shape on primitives.
    ///
    /// **Scope is intentionally narrow.** The typechecker discharges
    /// bound violations at every call site (`discharge_type_bounds`),
    /// so this hook is purely defense-in-depth for paths that reach
    /// codegen without a typechecker pass (no such path exists in the
    /// single-CU compiler today, but cross-module compilation would
    /// open one). Coverage:
    /// - Built-in traits (`Hash` / `Eq` / `PartialEq` / `Ord` /
    ///   `PartialOrd` / `Display` / `Clone` / `Copy`) checked against
    ///   primitive LLVM types via `llvm_type_satisfies_trait`.
    /// - Non-primitive LLVM types (pointers, structs) and unknown
    ///   trait names fall through permissively — verifying those
    ///   requires plumbing the typechecker's impl table into codegen
    ///   (deferred; tracked as a hard-stop trigger in
    ///   `phase-7-codegen.md § Trait-bounds-at-codegen enforcement`).
    pub(super) fn verify_bounds_at_codegen(
        &self,
        generic_fn: &Function,
        subst: &HashMap<String, BasicTypeEnum<'ctx>>,
    ) -> Result<(), String> {
        if let Some(gp) = &generic_fn.generic_params {
            for param in &gp.params {
                if param.bounds.is_empty() {
                    continue;
                }
                let Some(concrete) = subst.get(&param.name) else {
                    continue;
                };
                for bound in &param.bounds {
                    let Some(trait_name) = bound.path.last() else {
                        continue;
                    };
                    if !self.llvm_type_satisfies_trait(*concrete, trait_name) {
                        return Err(format!(
                            "trait bound `{}: {}` is not satisfied at monomorphization site for `{}` \
                             (concrete type `{}` does not implement `{}`)",
                            param.name,
                            trait_name,
                            generic_fn.name,
                            self.llvm_type_to_mangle_str(*concrete),
                            trait_name,
                        ));
                    }
                }
            }
        }

        if let Some(wc) = &generic_fn.where_clause {
            for constraint in &wc.constraints {
                let WhereConstraint::TypeBound {
                    type_name, bounds, ..
                } = constraint
                else {
                    continue;
                };
                let Some(concrete) = subst.get(type_name) else {
                    continue;
                };
                for bound in bounds {
                    let Some(trait_name) = bound.path.last() else {
                        continue;
                    };
                    if !self.llvm_type_satisfies_trait(*concrete, trait_name) {
                        return Err(format!(
                            "trait bound `{}: {}` is not satisfied at monomorphization site for `{}` \
                             (concrete type `{}` does not implement `{}`)",
                            type_name,
                            trait_name,
                            generic_fn.name,
                            self.llvm_type_to_mangle_str(*concrete),
                            trait_name,
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    /// Conservative LLVM-type-vs-built-in-trait predicate used by
    /// `verify_bounds_at_codegen`. Mirrors the typechecker's
    /// `type_supports_*` helpers but operates on `BasicTypeEnum`
    /// instead of `Type`. Permissive on non-primitive shapes
    /// (`PointerType`, `StructType`) and unknown trait names — those
    /// cases are the typechecker's responsibility today; the codegen
    /// hook only catches the unambiguous primitive violations
    /// (f32/f64 failing `Hash` / `Eq` / `Ord`).
    pub(super) fn llvm_type_satisfies_trait(
        &self,
        ty: BasicTypeEnum<'ctx>,
        trait_name: &str,
    ) -> bool {
        match trait_name {
            "Hash" | "Eq" | "Ord" => !matches!(ty, BasicTypeEnum::FloatType(_)),
            "PartialEq" | "PartialOrd" | "Display" | "Clone" | "Copy" => true,
            _ => true,
        }
    }

    /// Produce a stable string token for an LLVM type suitable for name mangling.
    pub(super) fn llvm_type_to_mangle_str(&self, ty: BasicTypeEnum<'ctx>) -> String {
        match ty {
            BasicTypeEnum::IntType(t) => match t.get_bit_width() {
                1 => "bool".to_string(),
                8 => "i8".to_string(),
                16 => "i16".to_string(),
                32 => "i32".to_string(),
                64 => "i64".to_string(),
                w => format!("i{}", w),
            },
            BasicTypeEnum::FloatType(t) => {
                // Distinguish f32 from f64 by comparing with context-canonical types.
                if t == self.context.f32_type() {
                    "f32".to_string()
                } else {
                    "f64".to_string()
                }
            }
            BasicTypeEnum::PointerType(_) => "ptr".to_string(),
            BasicTypeEnum::StructType(_) => "struct".to_string(),
            _ => "opaque".to_string(),
        }
    }

    // ── Monomorphized Map[K, V] symbol emission (Slice 1) ───────

    /// Byte offsets into the runtime's `#[repr(C)]` `KaracMap`
    /// layout (`runtime/src/map.rs`). Codegen-emitted monomorphized
    /// `Map[K, V]` method symbols load these fields by direct GEP +
    /// load against a `*mut KaracMap` opaque pointer rather than
    /// calling through the type-erased `karac_map_*` runtime
    /// functions. Pinned by the runtime-side unit test
    /// `karac_map_field_offsets_match_codegen` — any drift trips
    /// the runtime test before the binary can diverge.
    const KARAC_MAP_STATUS_OFFSET: u64 = 0;
    const KARAC_MAP_KV_OFFSET: u64 = 8;
    const KARAC_MAP_CAPACITY_OFFSET: u64 = 16;
    const KARAC_MAP_LEN_OFFSET: u64 = 24;
    const KARAC_MAP_TOMBSTONES_OFFSET: u64 = 32;
    /// Bucket status-byte sentinels for the monomorphized probe
    /// loop. Must match the runtime's `BUCKET_EMPTY` /
    /// `BUCKET_OCCUPIED` / `BUCKET_TOMBSTONE` constants in
    /// `runtime/src/map.rs`.
    const BUCKET_EMPTY: u64 = 0;
    const BUCKET_OCCUPIED: u64 = 1;
    const BUCKET_TOMBSTONE: u64 = 2;

    /// Cache key for the monomorphized Map[K, V] symbol family —
    /// `"{key_mangle}_{val_mangle}"` (e.g. `"i64_i64"`). Mirrors the
    /// content-addressed scheme used by `mangle_mono_name` for user
    /// generic fns, expressed in terms of `llvm_type_to_mangle_str`'s
    /// stable token set so distinct K/V tuples never collide.
    pub(super) fn mono_map_cache_key(
        &self,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) -> String {
        format!(
            "{}_{}",
            self.llvm_type_to_mangle_str(key_ty),
            self.llvm_type_to_mangle_str(val_ty),
        )
    }

    /// Gate predicate: does this K/V tuple route through the
    /// monomorphized Map path? Every tuple that returns `false`
    /// falls through to the erased `karac_map_*` runtime per § 3.6
    /// coexist-during-migration. Slice 5 deletes the erased
    /// fallback entirely.
    ///
    /// Slice 1 shipped `Map[i64, i64]`. Slice 2 adds the `i32`
    /// key family — that covers `Map[char, i64]` (the LeetCode #3
    /// kata's K/V tuple, since `char` lowers to LLVM `i32` per
    /// Slice 2.0) and `Map[i32, i64]` if anyone instantiates it.
    /// Both mangle identically (`i32_i64`) and share a single
    /// mono symbol — the K/V slot layout and FNV-1a-over-4-bytes
    /// hash are byte-identical regardless of which surface name
    /// the user wrote.
    pub(super) fn should_use_mono_map_for(
        &self,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) -> bool {
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let key_ok = matches!(key_ty, BasicTypeEnum::IntType(t) if t == i32_t || t == i64_t);
        let val_ok = matches!(val_ty, BasicTypeEnum::IntType(t) if t == i64_t);
        key_ok && val_ok
    }

    /// Lazily emit the monomorphized `Map[K, V]` method-symbol family
    /// for a given K/V tuple and return the cached handles. Each
    /// per-method `FunctionValue` is emitted with `LinkOnceODR`
    /// linkage so cross-crate / cross-TU duplicates collapse at link
    /// time (locked design § 3.2).
    ///
    /// Slice 1a ships **wrapper bodies only**: each mono method
    /// forwards to the corresponding erased `karac_map_*` runtime
    /// function 1:1. The wrapper exists at this slice to validate
    /// emission, mangling, dispatch wiring, and `linkonce_odr`
    /// linkage — `nm | grep karac_map_i64_i64_len | wc -l == 1`
    /// after the slice lands. Slice 1b replaces hot-path bodies
    /// (`insert_old`, `get`) with fully-inlined LLVM (direct i64
    /// hash + icmp eq), unlocking the bench gain.
    pub(super) fn get_or_emit_map_mono_methods(
        &mut self,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) -> MapMonoMethods<'ctx> {
        let cache_key = self.mono_map_cache_key(key_ty, val_ty);
        if let Some(entry) = self.map_mono_methods.get(&cache_key) {
            return *entry;
        }

        let saved_bb = self.builder.get_insert_block();

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // len: direct GEP + load against the runtime's `#[repr(C)]`
        // `KaracMap.len` field. Drops the function-pointer indirection
        // and the extern call overhead the erased fallback's
        // `karac_map_len` carried. Offset pinned by the runtime-side
        // `karac_map_field_offsets_match_codegen` unit test.
        let len_name = format!("karac_map_{cache_key}_len");
        let len_fn = match self.module.get_function(&len_name) {
            Some(f) => f,
            None => {
                let len_ty = i64_t.fn_type(&[ptr_ty.into()], false);
                let f = self
                    .module
                    .add_function(&len_name, len_ty, Some(Linkage::LinkOnceODR));
                let entry = self.context.append_basic_block(f, "entry");
                self.builder.position_at_end(entry);
                let map_arg = f.get_nth_param(0).unwrap().into_pointer_value();
                let i8_t = self.context.i8_type();
                let offset = i64_t.const_int(Self::KARAC_MAP_LEN_OFFSET, false);
                let len_field_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(i8_t, map_arg, &[offset], "mono.len.field.ptr")
                        .unwrap()
                };
                let len = self
                    .builder
                    .build_load(i64_t, len_field_ptr, "mono.len")
                    .unwrap();
                self.builder.build_return(Some(&len)).unwrap();
                f
            }
        };

        // insert_old: fast path inlines load-factor check, FNV-1a
        // hash (via direct call to the existing `karac_hash_<K>`
        // helper — same hash as the erased fallback so cross-path
        // consistency holds while coexist is in effect), linear
        // probe with empty / tombstone / occupied switch, and
        // inline K-typed icmp eq. Slow path (resize-needed branch
        // and safety fallback for the impossible exhausted-probe
        // case) forwards to `karac_map_insert_old` extern.
        let insert_name = format!("karac_map_{cache_key}_insert_old");
        let insert_old_fn = match self.module.get_function(&insert_name) {
            Some(f) => f,
            None => {
                let bool_t = self.context.bool_type();
                let insert_ty = bool_t.fn_type(
                    &[ptr_ty.into(), key_ty.into(), val_ty.into(), ptr_ty.into()],
                    false,
                );
                let f =
                    self.module
                        .add_function(&insert_name, insert_ty, Some(Linkage::LinkOnceODR));
                self.emit_mono_map_insert_old_body(f, key_ty, val_ty);
                f
            }
        };

        // get: same shape as insert_old's fast path but read-only.
        // No load-factor branch (get never resizes), no tombstone
        // tracking, no fresh-slot writes. Probe loop terminates on
        // EMPTY (return false) or OCCUPIED-with-matching-key (load
        // val, store to out_val, return true). On exhausted probe
        // (would be unreachable under valid resize policy, but
        // guarded for safety) returns false.
        let get_name = format!("karac_map_{cache_key}_get");
        let get_fn = match self.module.get_function(&get_name) {
            Some(f) => f,
            None => {
                let bool_t = self.context.bool_type();
                let get_ty = bool_t.fn_type(&[ptr_ty.into(), key_ty.into(), ptr_ty.into()], false);
                let f = self
                    .module
                    .add_function(&get_name, get_ty, Some(Linkage::LinkOnceODR));
                self.emit_mono_map_get_body(f, key_ty, val_ty);
                f
            }
        };

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        let methods = MapMonoMethods {
            len_fn,
            insert_old_fn,
            get_fn,
        };
        self.map_mono_methods.insert(cache_key, methods);
        methods
    }

    /// Emit the fast-path-inlined body of the monomorphized
    /// `karac_map_<K>_<V>_insert_old` function. The shape mirrors
    /// the runtime's `KaracMap::insert` algorithm
    /// (`runtime/src/map.rs:166`) — load-factor branch first,
    /// then linear probe — but inlines the hash (via direct call
    /// to `karac_hash_<K>`, the same FNV-1a helper the erased
    /// fallback's function-pointer hash dispatches to) and the eq
    /// (direct icmp on the K LLVM type), dropping the function-
    /// pointer indirection that defines the erasure tax.
    ///
    /// Slice 1b emitted this for (i64, i64) only; Slice 2 generalizes
    /// to any (i32 / i64 key) × (i64 val) pair so `Map[char, i64]`
    /// can share the shape — char lowers to LLVM i32 (Slice 2.0).
    ///
    /// On entry the function has signature `i1 (ptr map, K key,
    /// V val, ptr out_old_val)`. On exit, every path terminates
    /// with `ret i1` (the existed bit).
    pub(super) fn emit_mono_map_insert_old_body(
        &mut self,
        f: FunctionValue<'ctx>,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let key_int_ty = key_ty.into_int_type();
        let val_int_ty = val_ty.into_int_type();
        let key_size = (key_int_ty.get_bit_width() as u64).div_ceil(8);
        let val_size = (val_int_ty.get_bit_width() as u64).div_ceil(8);
        let kv_size_bytes = key_size + val_size;

        let map_arg = f.get_nth_param(0).unwrap().into_pointer_value();
        let key_arg = f.get_nth_param(1).unwrap().into_int_value();
        let val_arg = f.get_nth_param(2).unwrap().into_int_value();
        let out_old_arg = f.get_nth_param(3).unwrap().into_pointer_value();

        // Match the mangle-token used by `mono_map_cache_key` so the
        // helper name aligns with the symbol family. Both `char` (4-
        // byte) and `i32` keys hash via `karac_hash_i32` here even
        // though the erased fallback's stored function-pointer might
        // be `karac_hash_char` — both are FNV-1a over 4 bytes and
        // produce identical output for identical input, so cross-
        // path consistency holds.
        let hash_name = self.llvm_type_to_mangle_str(key_ty);
        let hash_fn = self.emit_hash_fn_for_type(&hash_name, key_ty);

        let entry_bb = self.context.append_basic_block(f, "entry");
        let slow_bb = self.context.append_basic_block(f, "slow_path");
        let fast_bb = self.context.append_basic_block(f, "fast_path");
        let probe_cond_bb = self.context.append_basic_block(f, "probe.cond");
        let probe_body_bb = self.context.append_basic_block(f, "probe.body");
        let case_empty_bb = self.context.append_basic_block(f, "case.empty");
        let case_tomb_check_bb = self.context.append_basic_block(f, "case.check_tomb");
        let case_tomb_bb = self.context.append_basic_block(f, "case.tomb");
        let case_occupied_bb = self.context.append_basic_block(f, "case.occupied");
        let match_found_bb = self.context.append_basic_block(f, "match.found");
        let exhausted_bb = self.context.append_basic_block(f, "exhausted");

        // ── entry: field loads + load-factor check ────────────────
        self.builder.position_at_end(entry_bb);
        let len_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_LEN_OFFSET, false)],
                    "len.p",
                )
                .unwrap()
        };
        let len = self
            .builder
            .build_load(i64_t, len_p, "len")
            .unwrap()
            .into_int_value();
        let tomb_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_TOMBSTONES_OFFSET, false)],
                    "tomb.p",
                )
                .unwrap()
        };
        let tombs = self
            .builder
            .build_load(i64_t, tomb_p, "tombs")
            .unwrap()
            .into_int_value();
        let cap_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_CAPACITY_OFFSET, false)],
                    "cap.p",
                )
                .unwrap()
        };
        let cap = self
            .builder
            .build_load(i64_t, cap_p, "cap")
            .unwrap()
            .into_int_value();

        // Load factor: (len + tombs + 1) * 4 > cap * 3 → resize
        let sum = self.builder.build_int_add(len, tombs, "len+tombs").unwrap();
        let sum1 = self
            .builder
            .build_int_add(sum, i64_t.const_int(1, false), "lt+1")
            .unwrap();
        let lhs = self
            .builder
            .build_int_mul(sum1, i64_t.const_int(4, false), "lhs")
            .unwrap();
        let rhs = self
            .builder
            .build_int_mul(cap, i64_t.const_int(3, false), "rhs")
            .unwrap();
        let need_resize = self
            .builder
            .build_int_compare(IntPredicate::UGT, lhs, rhs, "need_resize")
            .unwrap();
        self.builder
            .build_conditional_branch(need_resize, slow_bb, fast_bb)
            .unwrap();

        // ── slow_path: forward to erased karac_map_insert_old ─────
        self.builder.position_at_end(slow_bb);
        let slow_key_slot = self.builder.build_alloca(key_ty, "slow.key.slot").unwrap();
        let slow_val_slot = self.builder.build_alloca(val_ty, "slow.val.slot").unwrap();
        self.builder.build_store(slow_key_slot, key_arg).unwrap();
        self.builder.build_store(slow_val_slot, val_arg).unwrap();
        let slow_existed = self
            .builder
            .build_call(
                self.karac_map_insert_old_fn,
                &[
                    map_arg.into(),
                    slow_key_slot.into(),
                    slow_val_slot.into(),
                    out_old_arg.into(),
                ],
                "slow.existed",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&slow_existed)).unwrap();

        // ── fast_path: load status/kv ptrs, inline hash ───────────
        self.builder.position_at_end(fast_bb);
        let status_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_STATUS_OFFSET, false)],
                    "status.pp",
                )
                .unwrap()
        };
        let status_ptr = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                status_pp,
                "status",
            )
            .unwrap()
            .into_pointer_value();
        let kv_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_KV_OFFSET, false)],
                    "kv.pp",
                )
                .unwrap()
        };
        let kv_ptr = self
            .builder
            .build_load(self.context.ptr_type(AddressSpace::default()), kv_pp, "kv")
            .unwrap()
            .into_pointer_value();

        // Compute hash via direct call to karac_hash_<K>. Stack-
        // alloca + store + call matches the existing erased path's
        // hash exactly (same FNV-1a basis + prime, same byte order).
        let hash_key_slot = self.builder.build_alloca(key_ty, "hash.key.slot").unwrap();
        self.builder.build_store(hash_key_slot, key_arg).unwrap();
        let hash = self
            .builder
            .build_call(hash_fn, &[hash_key_slot.into()], "hash")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let mask = self
            .builder
            .build_int_sub(cap, i64_t.const_int(1, false), "mask")
            .unwrap();
        let start = self.builder.build_and(hash, mask, "start").unwrap();
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── probe.cond: 3-PHI'd state, bound check on i ───────────
        self.builder.position_at_end(probe_cond_bb);
        let i_phi = self.builder.build_phi(i64_t, "i").unwrap();
        let ft_phi = self.builder.build_phi(i64_t, "ft").unwrap();
        let ft_set_phi = self.builder.build_phi(bool_t, "ft_set").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), fast_bb)]);
        ft_phi.add_incoming(&[(&i64_t.const_zero(), fast_bb)]);
        ft_set_phi.add_incoming(&[(&bool_t.const_zero(), fast_bb)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let ft_val = ft_phi.as_basic_value().into_int_value();
        let ft_set_val = ft_set_phi.as_basic_value().into_int_value();
        let bound_done = self
            .builder
            .build_int_compare(IntPredicate::UGE, i_val, cap, "bound.done")
            .unwrap();
        self.builder
            .build_conditional_branch(bound_done, exhausted_bb, probe_body_bb)
            .unwrap();

        // ── probe.body: compute slot, load status, switch ─────────
        self.builder.position_at_end(probe_body_bb);
        let sum_si = self.builder.build_int_add(start, i_val, "sum.si").unwrap();
        let slot = self.builder.build_and(sum_si, mask, "slot").unwrap();
        let status_slot_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, status_ptr, &[slot], "status.slot.p")
                .unwrap()
        };
        let status_byte = self
            .builder
            .build_load(i8_t, status_slot_p, "status.byte")
            .unwrap()
            .into_int_value();
        let is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(Self::BUCKET_EMPTY, false),
                "is.empty",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_empty, case_empty_bb, case_tomb_check_bb)
            .unwrap();

        // ── case.check_tomb: branch tomb vs occupied ──────────────
        self.builder.position_at_end(case_tomb_check_bb);
        let is_tomb = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(Self::BUCKET_TOMBSTONE, false),
                "is.tomb",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_tomb, case_tomb_bb, case_occupied_bb)
            .unwrap();

        // ── case.empty: write fresh entry, possibly at earlier tomb
        self.builder.position_at_end(case_empty_bb);
        let target_slot = self
            .builder
            .build_select(ft_set_val, ft_val, slot, "target.slot")
            .unwrap()
            .into_int_value();
        let kv_size = i64_t.const_int(kv_size_bytes, false);
        let target_off = self
            .builder
            .build_int_mul(target_slot, kv_size, "target.off")
            .unwrap();
        let target_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[target_off], "target.kv.p")
                .unwrap()
        };
        self.builder.build_store(target_kv_p, key_arg).unwrap();
        let target_val_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    target_kv_p,
                    &[i64_t.const_int(key_size, false)],
                    "target.val.p",
                )
                .unwrap()
        };
        self.builder.build_store(target_val_p, val_arg).unwrap();
        let target_status_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, status_ptr, &[target_slot], "target.status.p")
                .unwrap()
        };
        self.builder
            .build_store(
                target_status_p,
                i8_t.const_int(Self::BUCKET_OCCUPIED, false),
            )
            .unwrap();
        // len += 1
        let new_len = self
            .builder
            .build_int_add(len, i64_t.const_int(1, false), "len.new")
            .unwrap();
        self.builder.build_store(len_p, new_len).unwrap();
        // if ft_set, tombs -= 1
        let tombs_dec = self
            .builder
            .build_int_sub(tombs, i64_t.const_int(1, false), "tombs.dec")
            .unwrap();
        let new_tombs = self
            .builder
            .build_select(ft_set_val, tombs_dec, tombs, "tombs.new")
            .unwrap()
            .into_int_value();
        self.builder.build_store(tomb_p, new_tombs).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_zero()))
            .unwrap();

        // ── case.tomb: remember first tomb, continue probing ─────
        self.builder.position_at_end(case_tomb_bb);
        let new_ft = self
            .builder
            .build_select(ft_set_val, ft_val, slot, "ft.new")
            .unwrap()
            .into_int_value();
        let tomb_i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next.tomb")
            .unwrap();
        i_phi.add_incoming(&[(&tomb_i_next, case_tomb_bb)]);
        ft_phi.add_incoming(&[(&new_ft, case_tomb_bb)]);
        ft_set_phi.add_incoming(&[(&bool_t.const_int(1, false), case_tomb_bb)]);
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── case.occupied: eq-check, found vs continue ───────────
        self.builder.position_at_end(case_occupied_bb);
        let slot_off = self
            .builder
            .build_int_mul(slot, kv_size, "slot.off")
            .unwrap();
        let slot_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[slot_off], "slot.kv.p")
                .unwrap()
        };
        let slot_key = self
            .builder
            .build_load(key_int_ty, slot_kv_p, "slot.key")
            .unwrap()
            .into_int_value();
        let key_match = self
            .builder
            .build_int_compare(IntPredicate::EQ, slot_key, key_arg, "key.match")
            .unwrap();
        let occ_i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next.occ")
            .unwrap();
        // Pre-build the no-match phi inputs.
        i_phi.add_incoming(&[(&occ_i_next, case_occupied_bb)]);
        ft_phi.add_incoming(&[(&ft_val, case_occupied_bb)]);
        ft_set_phi.add_incoming(&[(&ft_set_val, case_occupied_bb)]);
        self.builder
            .build_conditional_branch(key_match, match_found_bb, probe_cond_bb)
            .unwrap();

        // ── match.found: copy old val out, write new val ─────────
        self.builder.position_at_end(match_found_bb);
        let slot_val_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    slot_kv_p,
                    &[i64_t.const_int(key_size, false)],
                    "slot.val.p",
                )
                .unwrap()
        };
        let old_val = self
            .builder
            .build_load(val_int_ty, slot_val_p, "old.val")
            .unwrap()
            .into_int_value();
        self.builder.build_store(out_old_arg, old_val).unwrap();
        self.builder.build_store(slot_val_p, val_arg).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        // ── exhausted: unreachable under correct resize policy,
        //               fall back to erased extern for safety ──────
        self.builder.position_at_end(exhausted_bb);
        let safe_key_slot = self.builder.build_alloca(key_ty, "safe.key.slot").unwrap();
        let safe_val_slot = self.builder.build_alloca(val_ty, "safe.val.slot").unwrap();
        self.builder.build_store(safe_key_slot, key_arg).unwrap();
        self.builder.build_store(safe_val_slot, val_arg).unwrap();
        let safe_existed = self
            .builder
            .build_call(
                self.karac_map_insert_old_fn,
                &[
                    map_arg.into(),
                    safe_key_slot.into(),
                    safe_val_slot.into(),
                    out_old_arg.into(),
                ],
                "safe.existed",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&safe_existed)).unwrap();
    }

    /// Emit the fast-path-inlined body of the monomorphized
    /// `karac_map_<K>_<V>_get` function. Mirrors `KaracMap::lookup` and
    /// `KaracMap::get` from `runtime/src/map.rs:120` — but inlines hash,
    /// probe, K-typed eq, and the val load on match. No load-factor /
    /// resize branch (get never resizes); no tombstone-tracking PHI
    /// (get doesn't write).
    ///
    /// Slice 1b emitted this for (i64, i64) only; Slice 2 generalizes
    /// to any (i32 / i64 key) × (i64 val) pair so `Map[char, i64]`
    /// shares the shape.
    ///
    /// On entry the function has signature `i1 (ptr map, K key,
    /// ptr out_val)`. Returns true and writes the value through
    /// `out_val` on match; returns false otherwise, leaving
    /// `out_val` untouched.
    pub(super) fn emit_mono_map_get_body(
        &mut self,
        f: FunctionValue<'ctx>,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let key_int_ty = key_ty.into_int_type();
        let val_int_ty = val_ty.into_int_type();
        let key_size = (key_int_ty.get_bit_width() as u64).div_ceil(8);
        let val_size = (val_int_ty.get_bit_width() as u64).div_ceil(8);
        let kv_size_bytes = key_size + val_size;

        let map_arg = f.get_nth_param(0).unwrap().into_pointer_value();
        let key_arg = f.get_nth_param(1).unwrap().into_int_value();
        let out_val_arg = f.get_nth_param(2).unwrap().into_pointer_value();

        let hash_name = self.llvm_type_to_mangle_str(key_ty);
        let hash_fn = self.emit_hash_fn_for_type(&hash_name, key_ty);

        let entry_bb = self.context.append_basic_block(f, "entry");
        let probe_cond_bb = self.context.append_basic_block(f, "probe.cond");
        let probe_body_bb = self.context.append_basic_block(f, "probe.body");
        let check_occupied_bb = self.context.append_basic_block(f, "check.occupied");
        let eq_check_bb = self.context.append_basic_block(f, "eq.check");
        let match_found_bb = self.context.append_basic_block(f, "match.found");
        let not_found_bb = self.context.append_basic_block(f, "not.found");

        // ── entry: load cap / status / kv, compute hash and start ─
        self.builder.position_at_end(entry_bb);
        let cap_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_CAPACITY_OFFSET, false)],
                    "cap.p",
                )
                .unwrap()
        };
        let cap = self
            .builder
            .build_load(i64_t, cap_p, "cap")
            .unwrap()
            .into_int_value();
        let status_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_STATUS_OFFSET, false)],
                    "status.pp",
                )
                .unwrap()
        };
        let status_ptr = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                status_pp,
                "status",
            )
            .unwrap()
            .into_pointer_value();
        let kv_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_KV_OFFSET, false)],
                    "kv.pp",
                )
                .unwrap()
        };
        let kv_ptr = self
            .builder
            .build_load(self.context.ptr_type(AddressSpace::default()), kv_pp, "kv")
            .unwrap()
            .into_pointer_value();
        let hash_key_slot = self.builder.build_alloca(key_ty, "hash.key.slot").unwrap();
        self.builder.build_store(hash_key_slot, key_arg).unwrap();
        let hash = self
            .builder
            .build_call(hash_fn, &[hash_key_slot.into()], "hash")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let mask = self
            .builder
            .build_int_sub(cap, i64_t.const_int(1, false), "mask")
            .unwrap();
        let start = self.builder.build_and(hash, mask, "start").unwrap();
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── probe.cond: PHI for i; bound-check vs cap ─────────────
        self.builder.position_at_end(probe_cond_bb);
        let i_phi = self.builder.build_phi(i64_t, "i").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), entry_bb)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let bound_done = self
            .builder
            .build_int_compare(IntPredicate::UGE, i_val, cap, "bound.done")
            .unwrap();
        self.builder
            .build_conditional_branch(bound_done, not_found_bb, probe_body_bb)
            .unwrap();

        // ── probe.body: load status, branch on empty ──────────────
        self.builder.position_at_end(probe_body_bb);
        let sum_si = self.builder.build_int_add(start, i_val, "sum.si").unwrap();
        let slot = self.builder.build_and(sum_si, mask, "slot").unwrap();
        let status_slot_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, status_ptr, &[slot], "status.slot.p")
                .unwrap()
        };
        let status_byte = self
            .builder
            .build_load(i8_t, status_slot_p, "status.byte")
            .unwrap()
            .into_int_value();
        let is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(Self::BUCKET_EMPTY, false),
                "is.empty",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_empty, not_found_bb, check_occupied_bb)
            .unwrap();

        // ── check.occupied: tombstone → continue, occupied → eq ──
        self.builder.position_at_end(check_occupied_bb);
        let is_occupied = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(Self::BUCKET_OCCUPIED, false),
                "is.occupied",
            )
            .unwrap();
        let tomb_i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next.tomb")
            .unwrap();
        // Tombstone path: advance i, branch to probe.cond.
        i_phi.add_incoming(&[(&tomb_i_next, check_occupied_bb)]);
        self.builder
            .build_conditional_branch(is_occupied, eq_check_bb, probe_cond_bb)
            .unwrap();

        // ── eq.check: inline icmp eq on K key ────────────────────
        self.builder.position_at_end(eq_check_bb);
        let kv_size = i64_t.const_int(kv_size_bytes, false);
        let slot_off = self
            .builder
            .build_int_mul(slot, kv_size, "slot.off")
            .unwrap();
        let slot_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[slot_off], "slot.kv.p")
                .unwrap()
        };
        let slot_key = self
            .builder
            .build_load(key_int_ty, slot_kv_p, "slot.key")
            .unwrap()
            .into_int_value();
        let key_match = self
            .builder
            .build_int_compare(IntPredicate::EQ, slot_key, key_arg, "key.match")
            .unwrap();
        let nomatch_i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next.nomatch")
            .unwrap();
        i_phi.add_incoming(&[(&nomatch_i_next, eq_check_bb)]);
        self.builder
            .build_conditional_branch(key_match, match_found_bb, probe_cond_bb)
            .unwrap();

        // ── match.found: load val, write out, return true ────────
        self.builder.position_at_end(match_found_bb);
        let slot_val_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    slot_kv_p,
                    &[i64_t.const_int(key_size, false)],
                    "slot.val.p",
                )
                .unwrap()
        };
        let val = self
            .builder
            .build_load(val_int_ty, slot_val_p, "val")
            .unwrap()
            .into_int_value();
        self.builder.build_store(out_val_arg, val).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        // ── not.found: return false, out_val untouched ───────────
        self.builder.position_at_end(not_found_bb);
        self.builder
            .build_return(Some(&bool_t.const_zero()))
            .unwrap();
    }
}
