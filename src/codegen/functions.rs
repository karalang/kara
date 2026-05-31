//! Function declaration + body compilation.
//!
//! Houses `apply_linker_attrs` (per-fn attribute lowering for
//! `#[link_name]` / `#[no_mangle]` / `#[used]`), `declare_function`
//! (LLVM `FunctionType` construction from a Kāra `Function` AST node),
//! and `compile_function` (the per-function-body compilation driver).

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::FunctionValue;
use inkwell::AddressSpace;

use super::state::VarSlot;

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn apply_linker_attrs(&mut self, fn_val: FunctionValue<'ctx>, attrs: &[Attribute]) {
        for attr in attrs {
            // Linker attributes are bare-name only; namespaced paths
            // (`#[diagnostic::*]`, tool namespaces) never reach codegen.
            if attr.path.len() != 1 {
                continue;
            }
            match attr.path[0].as_str() {
                "link_section" => {
                    // `#[link_section("name")]` — first positional arg or
                    // `string_value` carries the section literal. Skip
                    // silently when neither is present; the parser scaffolding
                    // accepts the attribute but does not yet enforce arg shape.
                    let section = attr.string_value.clone().or_else(|| {
                        attr.args.iter().find_map(|a| match a.value.as_ref() {
                            Some(Expr {
                                kind: ExprKind::StringLit(s),
                                ..
                            }) => Some(s.clone()),
                            _ => None,
                        })
                    });
                    if let Some(s) = section {
                        fn_val.as_global_value().set_section(Some(&s));
                    }
                }
                "no_mangle" => {
                    // No-op: codegen already emits the symbol under its
                    // source-level name. Tracked here so future mangling
                    // passes can opt out.
                }
                "used" if !self.used_symbols.contains(&fn_val) => {
                    self.used_symbols.push(fn_val);
                }
                _ => {}
            }
        }
    }

    pub(super) fn declare_function(
        &mut self,
        func: &Function,
    ) -> Result<FunctionValue<'ctx>, String> {
        if func.name == "main" {
            let main_type = self.context.i32_type().fn_type(&[], false);
            // Slice c-repl.B.4: under the REPL JIT path the entry
            // symbol is renamed per cell (`cell_main_<id>`) so
            // multiple cells' main fns can coexist in the same
            // JITDylib. The i32 return + special-case return-zero
            // arm still fires (the check at the body-emission site
            // pivots on `func.name`, which stays `"main"` in the
            // AST) — only the LLVM symbol changes. AOT builds and
            // one-shot JIT keep the literal "main".
            let symbol = self.main_symbol_override.as_deref().unwrap_or("main");
            return Ok(self.module.add_function(symbol, main_type, None));
        }

        let param_types: Vec<BasicMetadataTypeEnum<'ctx>> = func
            .params
            .iter()
            .map(|p| self.llvm_param_type(p))
            .collect();

        // A2 slice 2b.3: a coroutine-compiled network-boundary fn is a *ramp*.
        // It takes a hidden trailing `ptr` completion-slot param (the caller
        // `park_slot_new`s it and waits on it; the body signals it) and returns
        // `ptr` (the coro handle — UAF-safe to return from the single canonical
        // `coro.end`; the caller ignores it). The Kāra return value is plumbed
        // through the frame; a non-unit coroutine return is a follow-on slice.
        let fn_type = if self.is_coroutine_compiled(&func.name) {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let mut coro_params = param_types.clone();
            coro_params.push(ptr_ty.into());
            ptr_ty.fn_type(&coro_params, false)
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

        // Record which params are ref for call-site argument passing.
        let ref_flags: Vec<bool> = func
            .params
            .iter()
            .map(|p| matches!(&p.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
            .collect();
        self.fn_param_ref.insert(func.name.clone(), ref_flags);
        // Record slice-param element types for call-site coercion.
        let slice_elems: Vec<Option<BasicTypeEnum<'ctx>>> = func
            .params
            .iter()
            .map(|p| self.extract_slice_elem_type(&p.ty))
            .collect();
        self.fn_param_slice_elem
            .insert(func.name.clone(), slice_elems);

        // Record the return-type name (bare `Path` segment) so call-chain
        // field access on a call result can recover its static type — see
        // `compile_field_access` and bug #8 (shared-struct return field
        // access on an unbound call result).
        if let Some(ret_ty) = &func.return_type {
            if let TypeKind::Path(path) = &ret_ty.kind {
                if let Some(seg) = path.segments.first() {
                    self.fn_return_type_names
                        .insert(func.name.clone(), seg.clone());
                }
                // Record the inner shared name when the return type is
                // `Option[shared T]` — read by the let-stmt handler's
                // `RcDecOption` registration for untyped lets whose
                // RHS is a call to this function (`let out = call();`
                // shape; explicit `let out: Option[T] = ...` reads the
                // inner directly off the annotation).
                if let Some((inner_name, _)) = self.option_inner_shared_type_for_type_expr(ret_ty) {
                    self.fn_return_option_inner_shared
                        .insert(func.name.clone(), inner_name);
                }
            }
        }

        // Internal linkage for non-`pub`, non-FFI-marked functions lets LLVM's
        // inliner treat them as private to the translation unit — it can elide
        // the standalone symbol after inlining all callers, and the inliner's
        // cost model is more aggressive with internal callees. `pub` items keep
        // external linkage so future multi-crate compilation can resolve them,
        // and `#[no_mangle]` / `#[used]` keep external so the symbol survives
        // for FFI consumers / link-section anchors. `main` is handled above.
        //
        // Slice c-repl.B.4 follow-on: in REPL-cell mode (signaled by
        // `main_symbol_override.is_some()`), force External linkage on
        // every top-level user fn. Two correctness requirements:
        //
        //   (a) Body-emitting cells must export their fns so a later
        //       cell's declare-only reference resolves to them via
        //       the shared JITDylib's symbol table. Internal linkage
        //       hides the body from the JIT linker — cell N+1 sees an
        //       unresolved symbol and the call crashes the runner
        //       subprocess silently.
        //
        //   (b) Declare-only cells (`declare_only_fns` contains the
        //       name) must use External linkage because LLVM's
        //       verifier rejects Internal on body-less declarations
        //       (Internal implies "definition is local to this TU").
        //
        // Both arms collapse to the same rule: in REPL-cell mode,
        // every top-level fn is External. Non-REPL builds (AOT, one-
        // shot JIT, `karac test` synthesized harness) keep the
        // existing pub/FFI-vs-Internal split so the inliner can still
        // elide non-pub local fns.
        //
        // The latent bug surfaced in a 3-cell scenario (pure-items
        // cell defining the fn, then a stmt cell that JIT-installs
        // it, then a stmt cell that re-references it via declare-
        // only); B.4's existing 2-cell tests never exercised this
        // codepath because either the declare-only set was empty or
        // the cross-cell symbol resolution never fired.
        let linkage = if self.main_symbol_override.is_some()
            || func.is_pub
            || func
                .attributes
                .iter()
                .any(|a| a.is_bare("no_mangle") || a.is_bare("used"))
        {
            Some(Linkage::External)
        } else {
            Some(Linkage::Internal)
        };
        let fn_val = self.module.add_function(&func.name, fn_type, linkage);
        self.apply_linker_attrs(fn_val, &func.attributes);

        // Phase-7 line 5 sub-item 1 — hot-swap slot registration.
        // When `--enable-hot-swap` is active, every user-defined `pub fn`
        // (extern-public module symbol) gets a slot in the module's
        // indirection table; calls to it are lowered through that slot.
        // Private / default-visibility functions stay direct. Closure
        // bodies and synthesized clone/drop helpers do not flow through
        // this path — they're emitted via separate `add_function` calls
        // in `closures.rs` / `clone_drop.rs`.
        if self.hot_swap_enabled && func.is_pub {
            let slot = self.hot_swap_fns.len() as u32;
            self.hot_swap_slots.insert(func.name.clone(), slot);
            self.hot_swap_fns.push((slot, fn_val));
        }

        Ok(fn_val)
    }

    pub(super) fn compile_function(&mut self, func: &Function) -> Result<(), String> {
        // Slice c-repl.B.4: `func.name == "main"` may have been
        // registered under a different LLVM symbol via
        // `main_symbol_override` (e.g. `cell_main_<id>` for REPL
        // cells). Use the same override here so the body-emission
        // pass finds the LLVM function the declaration pass minted.
        // Every other fn name passes through unchanged.
        let llvm_name = if func.name == "main" {
            self.main_symbol_override.as_deref().unwrap_or("main")
        } else {
            func.name.as_str()
        };
        let fn_val = self
            .module
            .get_function(llvm_name)
            .ok_or_else(|| format!("Function '{}' not declared", llvm_name))?;

        self.current_fn = Some(fn_val);
        self.current_fn_name = func.name.clone();
        // A2 slice 2b.3: drain any prior function's coroutine context. A
        // coroutine fn's `emit_coro_ramp` sets it; `emit_coro_finish` clears it
        // — this reset is the belt-and-suspenders for an early-error exit.
        self.coro_ctx = None;
        self.coro_park_counter = 0;
        self.variables.clear();
        self.var_type_names.clear();
        self.var_option_shared_heap.clear();
        self.ref_params.clear();
        self.rc_fallback_heap_types.clear();
        // Per-function reset of the name-keyed local-variable type side-
        // tables. These mirror exactly what `register_var_from_type_expr`
        // (the reseed path below) repopulates; leaving them un-cleared
        // lets a binding in one function pollute a same-named binding in
        // the next, because every entry is keyed by bare variable name
        // with no scope/function qualifier. The corruption case: a
        // `fn f(s: ref String)` registers `vec_elem_types["s"]`, which
        // then persists into `fn g() { let mut s = 1i64; … }` — at g's
        // let site the stale "s is a Vec" entry queues a `FreeVecBuffer`
        // cleanup against g's i64 counter, so scope exit reads a bogus
        // `cap` past the 8-byte alloca and frees a garbage pointer
        // (SIGABRT at -O0, miscompiled infinite loop at -O3). `var_type_names`
        // was already cleared above for the same reason; the collection
        // side-tables were simply missing from the list.
        self.vec_elem_types.clear();
        self.var_elem_type_exprs.clear();
        self.string_vars.clear();
        self.slice_elem_types.clear();
        self.map_key_types.clear();
        self.map_val_types.clear();
        self.map_key_type_names.clear();
        self.map_key_type_exprs.clear();
        self.set_elem_types.clear();
        self.set_elem_type_names.clear();
        self.set_elem_type_exprs.clear();
        self.atomic_var_inner_is_bool.clear();
        self.scope_cleanup_actions.clear();
        self.scope_cleanup_actions.push(Vec::new());
        // Slice 10: reseed module-binding side-tables after the per-fn
        // clear. Module bindings live for the program's lifetime but
        // the clear above wipes their `var_type_names` / `vec_elem_types`
        // / etc. registrations — re-register from the persistent
        // `module_bindings` snapshot so field-access / method-dispatch
        // / index paths inside this function body see the binding's
        // declared type.
        self.reseed_module_binding_side_tables();
        // Clear cross-function staging slot. `last_fstr_acc` holds an
        // alloca-valued LLVM pointer scoped to a specific function body;
        // a stale value from a prior function's compilation must not
        // leak into the next. The intra-function take points (Let /
        // Assign / function-tail return for `InterpolatedStringLit`
        // shapes) usually clear it, but a function whose final f-string
        // sits behind a non-tail position (e.g. `let _ = f"…";`) can
        // leave the slot populated.
        self.last_fstr_acc = None;

        // Slice 4 follow-up (a) — wider-E payload reconstruction at the
        // `?` site (2026-05-26). Reset and re-populate the
        // current-function's Err-arm LLVM type from `func.return_type`
        // when the return type is syntactically `Result[T, E]`. Read by
        // `compile_question`'s `fail_bb` to reconstruct the source-typed
        // Err value from the result struct's payload words via
        // `rebuild_value_from_payload_words`. `None` (the default)
        // means the function doesn't return `Result[T, E]` or the
        // annotation isn't recognised — falls back to staging bare
        // `w0` as i64 in the `?` failure branch.
        self.current_fn_err_payload_ty = func.return_type.as_ref().and_then(|ret_ty| match &ret_ty
            .kind
        {
            TypeKind::Path(path) if path.segments.len() == 1 && path.segments[0] == "Result" => {
                path.generic_args
                    .as_ref()
                    .and_then(|args| match args.get(1) {
                        Some(GenericArg::Type(e_te)) => Some(self.llvm_type_for_type_expr(e_te)),
                        _ => None,
                    })
            }
            _ => None,
        });

        let entry = self.context.append_basic_block(fn_val, "entry");
        self.builder.position_at_end(entry);

        // A2 slice 2b.3: for a coroutine-compiled network-boundary fn, emit the
        // coro ramp prologue (coro.id/begin + completion slot + shared exit
        // blocks) at the top of entry, before param allocas — this sets
        // `self.coro_ctx`, so the leaf parks in the body lower to `coro.suspend`
        // and the body returns route to the completion block. `emit_coro_finish`
        // closes it out after the body.
        if self.is_coroutine_compiled(&func.name) {
            // The hidden completion-slot param is the trailing `ptr`, after the
            // Kāra params (declare_function appended it).
            let slot = fn_val
                .get_nth_param(func.params.len() as u32)
                .expect("coroutine completion-slot param")
                .into_pointer_value();
            self.emit_coro_ramp(fn_val, slot);
        }

        if func.name != "main" {
            for (i, param) in func.params.iter().enumerate() {
                let param_name = self.param_name(param);
                let param_val = fn_val.get_nth_param(i as u32).unwrap();
                let alloca = self.create_entry_alloca(fn_val, &param_name, param_val.get_type());
                self.builder.build_store(alloca, param_val).unwrap();
                // Track ref params: alloca holds a pointer-to-data.
                if let Some(inner_ty) = self.inner_type_of_ref(&param.ty) {
                    self.ref_params.insert(param_name.clone(), inner_ty);
                }
                // Register collection / String / struct side-tables for the
                // parameter. Mirrors the let-binding registration in
                // `compile_stmt(StmtKind::Let)` so every `ref T` /
                // `mut ref T` / owned-collection parameter participates in
                // the same method-dispatch surface as a let-bound local.
                //
                // For `ref T` / `mut ref T`, `register_var_from_type_expr`
                // is invoked with the inner type — `Vec`, `Map`, `Set`,
                // `String`, `Slice`, and bare user-type names all flow
                // through the same registrar. Without this, the
                // dispatcher in `compile_method_call` falls through to
                // the "no handler for method 'X' on variable 'v'" error
                // for any `mut ref Map[K,V]` / `mut ref Set[T]` /
                // `mut ref VecDeque[T]` receiver — the structural
                // symmetric of the for-loop binding gap fixed in commit
                // `394cd64` (struct fields in for-loop bodies) but for
                // the parameter-mode case. The fix also covers
                // `mut ref Vec[T]` / `mut ref String` uniformly,
                // collapsing the previous ad-hoc per-shape branches
                // into one call.
                //
                // Owned `Slice[T]` / `mut Slice[T]` params take the
                // type expression as-is (no inner unwrap) — both
                // `MutSlice(inner)` and `Path(Slice[...])` flow through
                // `register_var_from_type_expr`'s slice arm.
                let registration_te: Option<&TypeExpr> = match &param.ty.kind {
                    TypeKind::Ref(inner) | TypeKind::MutRef(inner) => Some(inner.as_ref()),
                    _ => Some(&param.ty),
                };
                if let Some(te) = registration_te {
                    self.register_var_from_type_expr(&param_name, te);
                }
                // Track the declared type name so field/variant lookups work on this param.
                // Both owned (`Type`) and ref-wrapped (`ref Type` / `mut ref Type`)
                // paths feed `var_type_names` with the inner struct/enum name —
                // `field_index_for` needs it to find the field index regardless of
                // whether the param is value-typed or pointer-typed.
                let path_for_type_name = match &param.ty.kind {
                    TypeKind::Path(p) => Some(p),
                    TypeKind::Ref(inner) | TypeKind::MutRef(inner) => match &inner.kind {
                        TypeKind::Path(p) => Some(p),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(path) = path_for_type_name {
                    if let Some(type_name) = path.segments.first() {
                        self.var_type_names
                            .insert(param_name.clone(), type_name.clone());
                        // rc_inc for shared-type parameters (caller keeps its
                        // reference). Only fires for owned Path params — a
                        // shared-typed `ref T` doesn't take ownership, so no
                        // refcount bump.
                        if matches!(&param.ty.kind, TypeKind::Path(_)) {
                            if let Some(info) = self.shared_types.get(type_name.as_str()).cloned() {
                                let ptr = param_val.into_pointer_value();
                                self.emit_refcount_inc(&param_name, info.heap_type, ptr);
                                self.track_rc_var(&param_name, ptr, info.heap_type);
                            }
                        }
                    }
                }
                // `Option[shared T]` parameter registration. The
                // param receives the caller's +1 ref by transfer:
                //   - Identifier-arg caller binding (`shadow(chain)`)
                //     has its RcDecOption cleanup defused at the
                //     call site by
                //     `suppress_source_option_shared_cleanup_for_arg`
                //     (in `call_dispatch.rs`); the chain's +1 moves
                //     into the callee's param slot.
                //   - Call-result direct arg (`shadow(make_chain(10))`)
                //     carries the callee's +1 in the return value's
                //     SSA — no caller-side binding exists, no
                //     suppression needed.
                // Either way, the callee owns one ref on entry; no
                // entry-side `emit_refcount_inc` is needed. The
                // `track_rc_option_var` call queues an `RcDecOption`
                // cleanup so the param's inner ref drops at function
                // exit, and populates `var_option_shared_heap` so the
                // Assign-arm in `compile_stmt` dispatches its dec/inc
                // dance for param-shadowing (`opt = Some(...)` /
                // `opt = other_opt`) — the leak shape the 79a7db8
                // follow-up notes called out. No-op for Option[T]
                // where T isn't a shared struct.
                if let Some((_, info)) = self.option_inner_shared_type_for_type_expr(&param.ty) {
                    let option_ty = self.enum_layouts["Option"].llvm_type;
                    self.track_rc_option_var(&param_name, alloca, option_ty, info.heap_type);
                }
                // RC-fallback boxing for non-shared, non-Vec parameters flagged by the
                // ownership checker. The param value is boxed in {i64 rc, T} on the heap
                // so multiple "consumers" each get a copy of T and the heap object is freed
                // at scope exit when the refcount reaches zero.
                let is_ref_param = self.ref_params.contains_key(&param_name);
                let is_vec_param = self.vec_elem_types.contains_key(&param_name);
                let is_shared_param = if let TypeKind::Path(path) = &param.ty.kind {
                    path.segments
                        .first()
                        .is_some_and(|n| self.shared_types.contains_key(n.as_str()))
                } else {
                    false
                };
                if !is_ref_param
                    && !is_vec_param
                    && !is_shared_param
                    && self.is_rc_fallback_binding(&param_name)
                {
                    let val_ty = param_val.get_type();
                    let heap_type = self
                        .context
                        .struct_type(&[self.context.i64_type().into(), val_ty], false);
                    let heap_ptr = self.emit_rc_alloc(heap_type);
                    let val_field = self
                        .builder
                        .build_struct_gep(heap_type, heap_ptr, 1, "rc_fb_param_val")
                        .unwrap();
                    self.builder.build_store(val_field, param_val).unwrap();
                    // Overwrite alloca to hold heap ptr instead of T.
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let ptr_alloca = self.create_entry_alloca(fn_val, &param_name, ptr_ty.into());
                    self.builder.build_store(ptr_alloca, heap_ptr).unwrap();
                    self.rc_fallback_heap_types
                        .insert(param_name.clone(), heap_type);
                    self.track_rc_var(&param_name, heap_ptr, heap_type);
                    self.variables.insert(
                        param_name,
                        VarSlot {
                            ptr: ptr_alloca,
                            ty: ptr_ty.into(),
                        },
                    );
                    continue;
                }
                self.variables.insert(
                    param_name,
                    VarSlot {
                        ptr: alloca,
                        ty: param_val.get_type(),
                    },
                );
            }
        }

        // Per-branch `Option[shared T]` tail-return compensation: arm the
        // flow-sensitive context so the body's final expression (and, through
        // `compile_block` / `compile_if_let` / `compile_match`, each branch's
        // final expression) compensates a bare-arg `Option[shared]` leaf in the
        // specific arm that returns it. Subsumes the old single merge-block
        // inc, which could not balance a function MIXING `Some(<alias>)` tails
        // with bare-arg returns (the recursive merge-two-sorted-lists shape).
        // Cleared right after the body so it never leaks into later state.
        self.tail_ret_inner = func
            .return_type
            .as_ref()
            .and_then(|te| self.option_inner_shared_type_for_type_expr(te))
            .map(|(_, info)| info.heap_type);

        // Contract emission setup (design.md § Contracts). Gated on
        // `!strip_contracts` so a release build (design: "stripped in
        // release") emits none of it — zero runtime cost, including the
        // `old(...)` pre-state clone. Suppressing the three setup statements
        // here is sufficient: `emit_ensures_checks` / `emit_invariant_checks`
        // both no-op on their now-empty state vectors at the return sites, no
        // `requires` assert is built, and `old(...)` (which lives only inside
        // `ensures` bodies) is never reached because those bodies aren't
        // compiled. The gate is a single decision point for the whole feature.
        if !self.strip_contracts {
            // `requires` preconditions: emit the entry-time predicate checks
            // now that parameters are bound and before the body runs. A false
            // predicate aborts with `contract violated`.
            self.emit_requires_checks(&func.requires)?;

            // `ensures` setup: capture `old(...)` pre-state now (entry
            // dominates every return point) and stash the clauses so
            // `emit_ensures_checks` can fire them inline before each `ret`
            // (the tail return below + every explicit `return`).
            self.capture_contract_old_snapshots(&func.ensures)?;
            self.current_contract_ensures = func.ensures.clone();

            // Struct/impl `invariant` setup (rule 3): resolve the receiver
            // type's invariants for this method and stash them so
            // `emit_invariant_checks` can fire them inline before each `ret`
            // (same exit points as `ensures`), with `self` bound. The synthetic
            // method function carries `Type.method` as its name and the
            // method's `is_pub` flag — both consumed by `method_invariants_for`.
            // Free functions and invariant-free structs yield an empty list.
            self.current_method_invariants = self.method_invariants_for(&func.name, func.is_pub);
        }

        // Slice 2 (auto-par codegen MVP): route the function body through
        // `compile_function_body`, which dispatches inferred parallel
        // groups to `karac_par_run` when a `ConcurrencyAnalysis` was
        // threaded into codegen. With no analysis, `compile_function_body`
        // falls through to `compile_block` and behavior is unchanged.
        let result = self.compile_function_body(&func.body)?;
        self.tail_ret_inner = None;

        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            // Contract `ensures` checks at the tail return (design.md
            // § Contracts), with `result` bound to the tail value — before
            // scope cleanup, so the postcondition sees live params / result.
            self.emit_ensures_checks(result)?;
            // Struct/impl `invariant` checks at the tail return (rule 3),
            // with `self` bound to the (possibly mutated) receiver — same
            // exit point as `ensures`, inert for non-method functions.
            self.emit_invariant_checks()?;

            // Move-aware scope-exit cleanup for tail-expression
            // returns. When the function's final expression is an
            // Identifier that names a tracked Vec / String binding,
            // the binding's data is being moved into the caller's
            // return value — but `track_vec_var` unconditionally
            // queued a `FreeVecBuffer` cleanup at the let-site, and
            // `emit_scope_cleanup` below would free the buffer the
            // caller now owns. Zero the source's `cap` field before
            // cleanup so `FreeVecBuffer`'s `cap > 0` check skips the
            // free; the returned struct (already loaded into
            // `result`) retains the original cap so the caller's
            // own scope cleanup runs against a valid buffer. Same
            // shape as `suppress_source_vec_cleanup_for_arg` used
            // when a tracked Vec is passed as a call argument.
            //
            // Early `return v` statements bypass `emit_scope_cleanup`
            // entirely (the terminator-already-set guard above), so
            // they don't need this — the move-aware suppression only
            // matters when scope cleanup is about to run.
            self.suppress_cleanup_for_tail_return(&func.body);
            // (Branch-buried `Option[shared]` tail returns are now compensated
            // per-branch during body compilation via `tail_ret_inner` →
            // `compile_tail_final_expr`; no merge-block inc here.)
            // Sibling to `suppress_cleanup_for_tail_return` for the
            // InterpolatedStringLit-tail case: when the function's final
            // expression is `f"…"`, the loaded {data, len, cap} is the
            // return value — but the f-string accumulator's queued
            // `FreeVecBuffer` would free `data` here, between the return-
            // value load and the `ret` instruction. The caller would
            // receive a struct with a dangling data pointer. Zero the
            // acc's `cap` so its cleanup no-ops; the caller's binding
            // becomes the unique owner (or, for a discarded call result,
            // the caller's expression-statement cleanup takes over).
            // Identifier-tail returns are handled by the existing
            // `suppress_cleanup_for_tail_return` above; the two paths
            // cover the two move-aware tail shapes that produce a String
            // value.
            if matches!(
                func.body.final_expr.as_deref().map(|e| &e.kind),
                Some(ExprKind::InterpolatedStringLit(_))
            ) {
                if let Some(acc) = self.last_fstr_acc.take() {
                    self.zero_vec_alloca_cap(acc);
                }
            }
            // Slice 2 (Phase 7 § *defer / errdefer codegen*): when the
            // function's tail expression is syntactically `Err(...)` or
            // `None`, route through the error-path cleanup so any
            // in-scope `errdefer { ... }` fires before the regular
            // drop+defer drain. Other tail shapes (`Ok(v)`, plain values,
            // void) stay on the normal-exit drain. Same syntactic
            // detector as the early-return arm in `compile_expr`.
            let tail_is_error_exit = func
                .body
                .final_expr
                .as_deref()
                .is_some_and(Self::is_error_exit_value);
            if tail_is_error_exit {
                // Slice 4 (Phase 7 § *defer / errdefer codegen*): stage
                // the tail-Err payload so an in-scope `errdefer(e) {
                // ... }` can bind `e`. The tail expr has already been
                // compiled into `result` by `compile_function_body`
                // above (which is the constructed Err struct
                // `{i64 tag, i64 w0, ...}`).
                //
                // Slice 4 follow-up (b) — double-eval fix (2026-05-26).
                // Same pure-vs-impure split as the early-return path in
                // `compile_expr`'s `ExprKind::Return` arm: pure
                // payload expressions (Identifier / Path / literals)
                // re-compile (preserves wider-E source-typed binding);
                // impure expressions extract the i64-coerced payload
                // word from `result`'s field 1 (single eval, accepts
                // i64-coerce trade for wider-E impure args). See
                // `Self::is_pure_recompilable` for the whitelist.
                let staged = func
                    .body
                    .final_expr
                    .as_deref()
                    .and_then(Self::err_payload_from_value)
                    .and_then(|payload_expr| {
                        if Self::is_pure_recompilable(payload_expr) {
                            self.compile_expr(payload_expr).ok()
                        } else {
                            let constructed = result?;
                            self.builder
                                .build_extract_value(
                                    constructed.into_struct_value(),
                                    1,
                                    "errdefer_tail_payload_w0",
                                )
                                .ok()
                        }
                    });
                self.pending_errdefer_payload = staged;
                self.emit_scope_cleanup_for_error_path();
                self.pending_errdefer_payload = None;
            } else {
                self.emit_scope_cleanup();
            }
            if let Some(ctx) = self.coro_ctx {
                // A2 slice 2b.3: a coroutine body's normal completion routes to
                // the signal + final-suspend block, not a `ret` (the ramp's
                // `ptr` return is emitted in the shared suspend-return block).
                // The Kāra tail value is discarded — unit-only for this slice.
                self.builder
                    .build_unconditional_branch(ctx.coro_return_bb)
                    .unwrap();
            } else if func.name == "main" {
                let zero = self.context.i32_type().const_int(0, false);
                self.builder.build_return(Some(&zero)).unwrap();
            } else if let Some(val) = result {
                // Void-return functions whose body's final expression
                // happens to produce an SSA value (e.g. `fn f() {
                // println(1) }` — `compile_print` returns i64-0 as a
                // unit placeholder, but the parser treats the no-`;`
                // call as the block's `final_expr`, so `compile_block`
                // hands it back as `Some(val)`). Emitting `ret i64 0`
                // against a `void` LLVM signature fails module
                // verification with "Found return instr that returns
                // non-void in Function of void return type". Detect the
                // mismatch here and discard the value — the function's
                // observable behavior is unchanged (it returns unit; the
                // i64-0 was a codegen-internal placeholder, never user-
                // visible). The mismatch shows up because several
                // codegen paths (`compile_print`, `compile_assert_eq`,
                // unknown-callee fallback) use the i64-0 placeholder
                // uniformly regardless of the callee's actual return
                // type; threading exact unit-vs-i64 distinction through
                // each emitter is bigger scope than this fix needs.
                let fn_returns_void = self
                    .current_fn
                    .and_then(|f| f.get_type().get_return_type())
                    .is_none();
                if fn_returns_void {
                    self.builder.build_return(None).unwrap();
                } else {
                    self.builder.build_return(Some(&val)).unwrap();
                }
            } else {
                self.builder.build_return(None).unwrap();
            }
        }

        // A2 slice 2b.3: close out the coroutine — fill the shared exit blocks
        // (coro_return = signal + final suspend; cleanup = destroy-edge free;
        // suspend_ret = end + ret slot) now that every park in the body has
        // wired its suspend switch to them. Copy the context out (it's `Copy`)
        // and drain it so it can't leak into the next function.
        if let Some(ctx) = self.coro_ctx {
            self.emit_coro_finish(&ctx);
            self.coro_ctx = None;
        }

        self.scope_cleanup_actions.clear();
        self.current_contract_ensures.clear();
        self.contract_old_snapshots.clear();
        self.current_method_invariants.clear();
        Ok(())
    }
}
