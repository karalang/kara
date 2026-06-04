//! Effect-resource provider codegen.
//!
//! Houses the `with_provider[R]` lowering and `R.method(...)`
//! dispatch machinery: vtable emission, provider-data-ptr
//! materialization, the body-wrapping push/pop pair, dispatch
//! detection (`try_compile_provider_dispatch`), provider type
//! name inference, and the provider-method function type
//! constructor.

use std::collections::HashSet;

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::AddressSpace;

use super::helpers::{impl_target_name, match_with_provider_call};

impl<'ctx> super::Codegen<'ctx> {
    /// Emit a static vtable global per `impl T for U` where `T` was
    /// declared as a provider trait via some `effect resource R: T`.
    /// The vtable is an array of fn pointers in trait-method-declaration
    /// order; method dispatch at `R.method(...)` indexes into this array
    /// using the method's position in `provider_trait_methods[T]`.
    /// Symbol name: `@VT_<U>_<T>`. Stored in `provider_vtables` keyed by
    /// `(U, T)` for `with_provider[R]` lookup.
    pub(super) fn emit_provider_vtables(&mut self, program: &Program) {
        // Gather the set of provider trait names from the resource decls
        // walked earlier. Inherent impls (no trait) don't need vtables —
        // they're called directly by name.
        let provider_traits: HashSet<String> =
            self.provider_resource_traits.values().cloned().collect();
        if provider_traits.is_empty() {
            return;
        }

        let ptr_type = self.context.ptr_type(AddressSpace::default());
        for item in &program.items {
            let Item::ImplBlock(imp) = item else { continue };
            let Some(trait_path) = &imp.trait_name else {
                continue;
            };
            let Some(trait_name) = trait_path.segments.last().cloned() else {
                continue;
            };
            if !provider_traits.contains(&trait_name) {
                continue;
            }
            let Some(target_name) = impl_target_name(&imp.target_type) else {
                continue;
            };
            let Some(method_order) = self.provider_trait_methods.get(&trait_name).cloned() else {
                continue;
            };

            // Look up each method's compiled fn-ptr. Methods declared on
            // the impl but absent from the trait (extras) are ignored —
            // the vtable matches the trait's view. Trait methods missing
            // from the impl emit a null fn-ptr; calling such a vtable
            // slot would null-deref at runtime, but the typechecker
            // rejects partial impls so this case shouldn't reach codegen.
            let mut entries: Vec<inkwell::values::PointerValue<'ctx>> = Vec::new();
            for method_name in &method_order {
                let symbol = format!("{}.{}", target_name, method_name);
                let entry = match self.module.get_function(&symbol) {
                    Some(f) => f.as_global_value().as_pointer_value(),
                    None => ptr_type.const_null(),
                };
                entries.push(entry);
            }

            let vtable_array_ty = ptr_type.array_type(entries.len() as u32);
            let vtable_init = ptr_type.const_array(&entries);
            let vt_name = format!("VT_{}_{}", target_name, trait_name);
            let vt_global = self.module.add_global(vtable_array_ty, None, &vt_name);
            vt_global.set_initializer(&vtable_init);
            vt_global.set_linkage(Linkage::Internal);
            vt_global.set_constant(true);
            self.provider_vtables
                .insert((target_name, trait_name), vt_global);
        }
    }

    /// Theme 6 sub-step 3: lower `with_provider[R](provider, ||body)`.
    ///
    /// Generates:
    /// ```text
    ///   %frame = alloca ProviderFrame
    ///   %data = <pointer to provider value>
    ///   call void @karac_provider_push(%frame, <resource_id>, %data, @VT_<U>_<T>)
    ///   <body>                                    ; inlined closure body
    ///   call void @karac_provider_pop()
    ///   ; result = body's value
    /// ```
    ///
    /// The `ProviderFrame` is alloca'd on the entry block so each
    /// `with_provider` call site has its own per-invocation slot — the
    /// runtime only mutates head pointers, the storage is caller-owned.
    /// Restrictions for v1: the closure argument must be an inline
    /// `||body` literal (the canonical Parallax-lite shape); a named
    /// closure-binding form would require routing through the indirect
    /// closure-call path. The provider's impl-target type is inferred
    /// from a small set of receiver-shape patterns (identifier whose
    /// `var_type_names` is set, struct literal, shared-struct value);
    /// other shapes return a codegen error.
    pub(super) fn compile_with_provider(
        &mut self,
        resource: &str,
        provider_expr: &Expr,
        closure_expr: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // 0. Trait-less resources have no `effect resource R: T` declaration,
        //    so no provider trait and no trait-keyed vtable. They override via
        //    the ambient runtime-stack path, which pushes the override onto
        //    the SAME `karac_provider_*` stack as trait-ful resources — using
        //    a vtable synthesized from the resource's method order — so the
        //    override is visible across function-call boundaries, matching the
        //    interpreter. Two flavours, both routed here:
        //      - **Prelude ambient** (`Clock`, `Env`, …): method order from
        //        `prelude::AMBIENT_RESOURCE_METHODS`; an unoverridden call
        //        falls back to the builtin FFI default.
        //      - **User trait-less** (`effect resource R;` with no `: T`):
        //        method order from the override type's inherent impl, recorded
        //        in `user_ambient_resource_methods` by the eager pre-pass;
        //        there is no FFI default, so dispatch is always through the
        //        active override (the effect checker guarantees R is in scope).
        //
        //    The discriminator is trait-*absence*, NOT ID-absence: codegen
        //    mints a stable resource ID for every ambient resource (see
        //    `compile_program`), and a few prelude resources (`Network`,
        //    `ProcessTable`) carry an `Item::EffectResource` declaration so
        //    they land in `provider_resource_ids` anyway — but none has a
        //    `: T` provider trait, so keying on `provider_resource_traits`
        //    routes all trait-less resources to the ambient path while leaving
        //    user `effect resource R: T` on the trait-vtable path below.
        if !self.provider_resource_traits.contains_key(resource) {
            return self.compile_with_provider_ambient(resource, provider_expr, closure_expr);
        }

        // 1. Resolve the resource ID and provider trait. Both must have
        //    been populated by the early walk over `Item::EffectResource`
        //    in `compile_program`; absence here means the resource
        //    name is bogus or the resource has no provider trait
        //    (`effect resource R;` without `: T`), which the typechecker
        //    should already reject before codegen runs.
        let resource_id = self
            .provider_resource_ids
            .get(resource)
            .copied()
            .ok_or_else(|| {
                format!(
                    "with_provider: unknown effect resource '{}' (no resource ID assigned)",
                    resource
                )
            })?;
        let trait_name = self
            .provider_resource_traits
            .get(resource)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "with_provider: resource '{}' has no provider trait — `with_provider` \
                     requires `effect resource {}: T` for some trait T",
                    resource, resource
                )
            })?;

        // 2. Infer the provider's impl-target type and look up its vtable.
        let provider_type_name = self
            .infer_provider_type_name(provider_expr)
            .ok_or_else(|| {
                format!(
                    "with_provider[{}]: cannot infer concrete provider type at codegen — \
                 supported shapes are an identifier with a known struct type or a \
                 struct literal",
                    resource
                )
            })?;
        let vt_global = self
            .provider_vtables
            .get(&(provider_type_name.clone(), trait_name.clone()))
            .copied()
            .ok_or_else(|| {
                format!(
                    "with_provider[{}]: no vtable found for `impl {} for {}` — check that \
                     the impl exists and `effect resource {}: {}` is declared at the top level",
                    resource, trait_name, provider_type_name, resource, trait_name
                )
            })?;

        // 3. Materialize a pointer to the provider's data. For shared
        //    structs, the loaded variable value IS the heap pointer
        //    (`{refcount, fields...}`); for value-type structs, take the
        //    storage alloca, or alloca-and-store a fresh value when the
        //    provider expression isn't a known identifier.
        let data_ptr = self.compile_provider_data_ptr(provider_expr, &provider_type_name)?;

        // 4. Alloca a `ProviderFrame` on the function entry block so the
        //    storage outlives the push/pop pair without re-alloca'ing
        //    on each loop iteration if a `with_provider` is in a loop.
        let fn_val = self.current_fn.ok_or_else(|| {
            "with_provider: no current function (called from top-level?)".to_string()
        })?;
        let frame_ptr = self.create_entry_alloca(fn_val, "wp.frame", self.provider_frame_ty.into());

        // 5. Push: karac_provider_push(frame, resource_id, data, vtable_ptr).
        let i32_t = self.context.i32_type();
        let id_v = i32_t.const_int(resource_id as u64, false);
        let vtable_ptr = vt_global.as_pointer_value();
        self.builder
            .build_call(
                self.karac_provider_push_fn,
                &[
                    frame_ptr.into(),
                    id_v.into(),
                    data_ptr.into(),
                    vtable_ptr.into(),
                ],
                "",
            )
            .unwrap();

        // 6. Inline the closure body. Only inline `||body` is supported in
        //    v1 — the body's free variables resolve against the outer
        //    scope, exactly as the interpreter handles a `with_provider`
        //    closure (see `Interpreter::eval_with_provider`).
        let body_result = self.compile_with_provider_body(closure_expr, resource)?;

        // 7. Pop: karac_provider_pop(). Matches the push; the runtime
        //    asserts head==frame and walks back to `frame.prev`.
        self.builder
            .build_call(self.karac_provider_pop_fn, &[], "")
            .unwrap();

        Ok(body_result)
    }

    /// Phase-8 line 153: lower `with_span(span, ||body)`.
    ///
    /// Generates:
    /// ```text
    ///   %prev = call i64 @karac_tracing_get_active_span()
    ///   %sid  = <span_expr>.span_id                  ; i64 field read
    ///   call void @karac_tracing_set_active_span(%sid)
    ///   <body>                                        ; inlined closure body
    ///   call void @karac_tracing_set_active_span(%prev)
    ///   ; result = body's value
    /// ```
    ///
    /// Mirrors `compile_with_provider`'s push/inline-body/pop shape, but
    /// over the per-thread active-span register instead of the provider
    /// stack: snapshot the prior active span, install `span.span_id` for
    /// the body, restore the snapshot after. As with `with_provider`, the
    /// body must be an inline `||body` literal (its free variables resolve
    /// against the outer scope) and the restore-after-body sequencing
    /// matches `with_provider`'s — an early `return` *inside* the body
    /// returns from the enclosing function and bypasses the inline
    /// restore, exactly as `with_provider`'s pop does; the common
    /// fall-through and closure-local `return` cases restore correctly.
    pub(super) fn compile_with_span(
        &mut self,
        span_expr: &Expr,
        closure_expr: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // 1. Snapshot the prior active span id.
        let prev = self
            .builder
            .build_call(self.karac_tracing_get_active_span_fn, &[], "ws.prev")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();

        // 2. Read `span.span_id` (i64, field index 1 of `Span`) by
        //    compiling a field access on the span expression — handles
        //    identifier / literal / builder-chain receivers uniformly.
        let span_id = self.compile_field_access(span_expr, "span_id")?;

        // 3. Install the body's active span.
        self.builder
            .build_call(self.karac_tracing_set_active_span_fn, &[span_id.into()], "")
            .unwrap();

        // 4. Inline the closure body (only `||body` literals; mirrors
        //    `compile_with_provider_body`).
        let ExprKind::Closure { params, body, .. } = &closure_expr.kind else {
            return Err(
                "with_span: second argument must be an inline `||body` literal".to_string(),
            );
        };
        if !params.is_empty() {
            return Err(format!(
                "with_span: closure must take zero arguments, got {}",
                params.len()
            ));
        }
        let body_result = self.compile_expr(body)?;

        // 5. Restore the prior active span.
        self.builder
            .build_call(self.karac_tracing_set_active_span_fn, &[prev.into()], "")
            .unwrap();

        Ok(body_result)
    }

    /// `with_provider[R]` lowering for a *trait-less* resource overridden by
    /// a statically-typed provider — either a prelude ambient resource
    /// (`Clock`, `Env`, …) or a user `effect resource R;` (no `: T`).
    /// Runtime-stack path (unified with the trait-ful dispatch): push the
    /// override onto the same `karac_provider_*` stack, keyed by the
    /// resource's minted ID, carrying a vtable synthesized from the
    /// resource's method order (prelude: `AMBIENT_RESOURCE_METHODS`; user:
    /// the override type's inherent impl, recorded in
    /// `user_ambient_resource_methods`). The override is therefore visible to
    /// method calls *across function-call boundaries* (the call sites consult
    /// the runtime stack — see `try_compile_provider_dispatch` /
    /// `compile_ambient_resource_method`), matching the interpreter and the
    /// trait-ful path. This is what makes `karac test` provider fixtures
    /// work: `test_main_synth` wraps a *call* to the test fn, so the body's
    /// `Clock.now()` / `AuditLog.count()` is cross-boundary.
    ///
    /// The provider type must be statically inferable at this site
    /// (struct literal or typed identifier — the same shapes
    /// `infer_provider_type_name` accepts for user resources). A
    /// runtime-typed provider (e.g. a fn-return value) yields a precise
    /// error — a deliberate v1 non-goal that matches the user-resource
    /// path's same restriction.
    fn compile_with_provider_ambient(
        &mut self,
        resource: &str,
        provider_expr: &Expr,
        closure_expr: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let provider_type_name = self
            .infer_provider_type_name(provider_expr)
            .ok_or_else(|| {
                format!(
                    "with_provider[{}]: ambient-resource override requires a statically-typed \
                 provider (struct literal or typed binding); runtime-typed providers \
                 (e.g. a function return) are not supported on the codegen path in v1",
                    resource
                )
            })?;
        let resource_id = *self.provider_resource_ids.get(resource).ok_or_else(|| {
            format!(
                "with_provider[{}]: ambient resource has no minted resource ID — add it to \
                 `prelude::AMBIENT_RESOURCE_METHODS` (codegen bug)",
                resource
            )
        })?;
        let vtable_ptr = self.emit_ambient_vtable(&provider_type_name, resource)?;
        let data_ptr = self.compile_provider_data_ptr(provider_expr, &provider_type_name)?;

        // Alloca the ProviderFrame on the entry block (one slot reused
        // across loop iterations), then push / body / pop — identical to
        // the user-resource path.
        let fn_val = self.current_fn.ok_or_else(|| {
            "with_provider: no current function (called from top-level?)".to_string()
        })?;
        let frame_ptr =
            self.create_entry_alloca(fn_val, "wp.amb.frame", self.provider_frame_ty.into());
        let id_v = self.context.i32_type().const_int(resource_id as u64, false);
        self.builder
            .build_call(
                self.karac_provider_push_fn,
                &[
                    frame_ptr.into(),
                    id_v.into(),
                    data_ptr.into(),
                    vtable_ptr.into(),
                ],
                "",
            )
            .unwrap();
        let body_result = self.compile_with_provider_body(closure_expr, resource)?;
        self.builder
            .build_call(self.karac_provider_pop_fn, &[], "")
            .unwrap();
        Ok(body_result)
    }

    /// Lazily emit (or fetch) the override vtable for an ambient resource:
    /// a `[N x ptr]` global of the override type `U`'s `@U.<method>`
    /// fn-pointers in the resource's canonical method order
    /// (`AMBIENT_RESOURCE_METHODS`). Methods `U` doesn't implement get a
    /// null slot — the call site null-checks the loaded fn-ptr and falls
    /// to the ambient-default FFI for those. Keyed `(U, resource)` in the
    /// shared `provider_vtables` map (the resource name plays the "trait"
    /// role; no collision since ambient resource names aren't trait
    /// names). Emitted at the `with_provider` site (after all impl methods
    /// are declared, so `get_function` resolves), once per `(U, resource)`.
    fn emit_ambient_vtable(
        &mut self,
        override_type: &str,
        resource: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let key = (override_type.to_string(), resource.to_string());
        if let Some(g) = self.provider_vtables.get(&key) {
            return Ok(g.as_pointer_value());
        }
        // Method order: prelude ambient resources have a hardcoded canonical
        // order; trait-less *user* resources derive theirs from the override
        // type's inherent impl, recorded in `user_ambient_resource_methods`
        // by the eager pre-pass (`emit_ambient_provider_vtables`).
        let methods: Vec<String> = if let Some(m) = crate::prelude::AMBIENT_RESOURCE_METHODS
            .iter()
            .find(|(r, _)| *r == resource)
            .map(|(_, m)| *m)
        {
            m.iter().map(|s| s.to_string()).collect()
        } else if let Some(m) = self.user_ambient_resource_methods.get(resource) {
            m.clone()
        } else {
            return Err(format!(
                "with_provider[{}]: no method order for resource — prelude resources need an \
                 entry in `prelude::AMBIENT_RESOURCE_METHODS`; a trait-less user resource needs \
                 its override type's inherent-impl methods recorded by the eager vtable pre-pass",
                resource
            ));
        };
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let mut entries: Vec<inkwell::values::PointerValue<'ctx>> = Vec::new();
        for method_name in methods {
            let symbol = format!("{}.{}", override_type, method_name);
            let entry = match self.module.get_function(&symbol) {
                Some(f) => f.as_global_value().as_pointer_value(),
                None => ptr_type.const_null(),
            };
            entries.push(entry);
        }
        let vtable_array_ty = ptr_type.array_type(entries.len() as u32);
        let vtable_init = ptr_type.const_array(&entries);
        let vt_name = format!("VT_AMBIENT_{}_{}", override_type, resource);
        let vt_global = self.module.add_global(vtable_array_ty, None, &vt_name);
        vt_global.set_initializer(&vtable_init);
        vt_global.set_linkage(Linkage::Internal);
        vt_global.set_constant(true);
        self.provider_vtables.insert(key, vt_global);
        Ok(vt_global.as_pointer_value())
    }

    /// Eagerly emit ambient override vtables for every `with_provider[R]`
    /// (ambient `R`) site in the program, BEFORE any function body is
    /// compiled. This is the ambient analog of `emit_provider_vtables` and
    /// is required for correctness: the call-site dispatch
    /// (`ambient_override_fn_type` / `compile_ambient_dispatch_branch`)
    /// decides whether to emit the runtime branch by checking whether a
    /// `(U, R)` vtable exists — and the test fn (which contains the ambient
    /// call) is compiled BEFORE the synthesized `main` (which holds the
    /// `with_provider` site), so a lazily-emitted vtable would not yet
    /// exist when the call site needs it. Walks function and impl-method
    /// bodies for `with_provider[R]` calls, resolving the override type
    /// from a struct literal or a `let`-bound struct literal (the shapes
    /// `infer_provider_type_name` and `test_main_synth` produce).
    pub(super) fn emit_ambient_provider_vtables(&mut self, program: &Program) {
        // A trait-less *user* resource has no trait to pin its method order,
        // so derive it from the override type's inherent impl. Collect every
        // type's inherent-impl method order once up front; the scan records
        // `user_ambient_resource_methods[R]` from this when it meets a
        // `with_provider[R]` whose override type is statically known.
        let inherent_methods = collect_inherent_methods(program);
        // Map of constructor-call key → concrete return-type name, so a
        // provider bound to a *call* (`let p = makeFoo();` or an inline
        // `with_provider[R](makeFoo(), ...)`) resolves to its struct type
        // here exactly as the real `with_provider` lowering does via
        // `var_type_names`. Without this the pre-pass only recognized
        // `StructLiteral` providers, so a trait-less user resource whose
        // ctor is a function/assoc-fn call errored "no method order".
        let ctor_returns = collect_ctor_return_types(program);
        let empty = std::collections::HashMap::new();
        for item in &program.items {
            match item {
                Item::Function(f) => self.scan_block_for_ambient_overrides(
                    &f.body,
                    &inherent_methods,
                    &ctor_returns,
                    &empty,
                ),
                Item::ImplBlock(imp) => {
                    for ii in &imp.items {
                        if let ImplItem::Method(m) = ii {
                            self.scan_block_for_ambient_overrides(
                                &m.body,
                                &inherent_methods,
                                &ctor_returns,
                                &empty,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn scan_block_for_ambient_overrides(
        &mut self,
        block: &Block,
        inherent_methods: &std::collections::HashMap<String, Vec<String>>,
        ctor_returns: &std::collections::HashMap<String, String>,
        inherited: &std::collections::HashMap<String, String>,
    ) {
        // `let p = <provider>` bindings let a provider passed by identifier
        // resolve to its struct type (test_main_synth binds the fixture ctor
        // to a `let` before the `with_provider`). The RHS may be a struct
        // literal OR a constructor call (`makeFoo()` / `Type.new()`) — both
        // resolve through `ambient_provider_type`. `inherited` seeds the map
        // with provider bindings from enclosing scopes so a nested
        // `with_provider` inside a block-bodied closure
        // (`with_provider[A](pa, || { with_provider[B](pb, ...) })`, where
        // `pb` is bound in the outer block) still resolves `pb` — without it
        // the inner block started empty and dropped the override.
        let mut bindings: std::collections::HashMap<String, String> = inherited.clone();
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, value, .. } => {
                    if let PatternKind::Binding(name) = &pattern.kind {
                        if let Some(ty) = ambient_provider_type(value, &bindings, ctor_returns) {
                            bindings.insert(name.clone(), ty);
                        }
                    }
                    self.scan_expr_for_ambient_overrides(
                        value,
                        &bindings,
                        inherent_methods,
                        ctor_returns,
                    );
                }
                StmtKind::Expr(e) => self.scan_expr_for_ambient_overrides(
                    e,
                    &bindings,
                    inherent_methods,
                    ctor_returns,
                ),
                _ => {}
            }
        }
        if let Some(tail) = &block.final_expr {
            self.scan_expr_for_ambient_overrides(tail, &bindings, inherent_methods, ctor_returns);
        }
    }

    fn scan_expr_for_ambient_overrides(
        &mut self,
        expr: &Expr,
        bindings: &std::collections::HashMap<String, String>,
        inherent_methods: &std::collections::HashMap<String, Vec<String>>,
        ctor_returns: &std::collections::HashMap<String, String>,
    ) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some((resource, provider_expr, closure_expr)) =
                    match_with_provider_call(callee, args)
                {
                    // Trait-less resources (prelude ambient OR user
                    // `effect resource R;`) route through the runtime-stack
                    // ambient path and need a `(U, R)` override vtable. Keyed
                    // on `provider_resource_ids` (every valid resource has an
                    // ID by now) to skip bogus names, and trait-absence to
                    // leave trait-ful resources on `emit_provider_vtables`.
                    if self.provider_resource_ids.contains_key(&resource)
                        && !self.provider_resource_traits.contains_key(&resource)
                    {
                        if let Some(ty) =
                            ambient_provider_type(provider_expr, bindings, ctor_returns)
                        {
                            // A trait-less *user* resource (not a prelude
                            // ambient one) has no canonical method order, so
                            // record it from the override type's inherent impl
                            // — vtable emission and call-site dispatch then
                            // index consistently. Prelude resources keep their
                            // hardcoded `AMBIENT_RESOURCE_METHODS` order and an
                            // FFI default, so they are NOT recorded here (that
                            // membership is what `try_compile_provider_dispatch`
                            // uses to route user resources through the
                            // always-override, no-default path).
                            if !crate::prelude::PRELUDE_EFFECT_RESOURCES
                                .contains(&resource.as_str())
                            {
                                if let Some(methods) = inherent_methods.get(&ty) {
                                    self.user_ambient_resource_methods
                                        .entry(resource.clone())
                                        .or_insert_with(|| methods.clone());
                                }
                            }
                            // Idempotent: `emit_ambient_vtable` no-ops if the
                            // `(U, R)` vtable already exists.
                            let _ = self.emit_ambient_vtable(&ty, &resource);
                        }
                    }
                    if let ExprKind::Closure { body, .. } = &closure_expr.kind {
                        self.scan_expr_for_ambient_overrides(
                            body,
                            bindings,
                            inherent_methods,
                            ctor_returns,
                        );
                    }
                    return;
                }
                for a in args {
                    self.scan_expr_for_ambient_overrides(
                        &a.value,
                        bindings,
                        inherent_methods,
                        ctor_returns,
                    );
                }
            }
            ExprKind::Block(b) => {
                self.scan_block_for_ambient_overrides(b, inherent_methods, ctor_returns, bindings)
            }
            ExprKind::Closure { body, .. } => {
                self.scan_expr_for_ambient_overrides(body, bindings, inherent_methods, ctor_returns)
            }
            _ => {}
        }
    }

    /// The LLVM `FunctionType` to use for an indirect call through an
    /// ambient override vtable slot, recovered from any override type `U`
    /// whose `(U, resource)` vtable was emitted in this module (all impls
    /// of a given ambient method share the same lowered signature). Returns
    /// `None` when no override vtable for `resource` exists — i.e. no
    /// `with_provider[resource]` appears in the module, so no override can
    /// be active and the call site skips the runtime branch entirely.
    pub(super) fn ambient_override_fn_type(
        &self,
        resource: &str,
        method: &str,
    ) -> Option<inkwell::types::FunctionType<'ctx>> {
        for (target, r) in self.provider_vtables.keys() {
            if r == resource {
                if let Some(f) = self.module.get_function(&format!("{}.{}", target, method)) {
                    return Some(f.get_type());
                }
            }
        }
        None
    }

    /// Determine the concrete impl-target type name of a provider
    /// expression at codegen, used to look up the right `@VT_<U>_<T>`
    /// vtable. Supports:
    ///   - `ExprKind::Identifier(n)` whose `var_type_names[n]` is set
    ///     (covers `let p = MyProvider { ... }; with_provider[R](p, ...)`);
    ///   - `ExprKind::StructLit { name, ... }` for inline construction.
    ///
    /// Other shapes (function-return values, field projections, etc.)
    /// fall through and the caller emits a codegen error.
    pub(super) fn infer_provider_type_name(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Identifier(n) => self.var_type_names.get(n.as_str()).cloned(),
            ExprKind::StructLiteral { path, .. } => path.last().cloned(),
            _ => None,
        }
    }

    /// Materialize a pointer to the provider value's data, suitable for
    /// passing to `karac_provider_push` and reading back as `*const Self`
    /// inside vtable methods.
    ///
    /// - **Shared struct provider:** the loaded value IS the heap pointer
    ///   (`{refcount, fields...}`). Vtable methods for shared structs
    ///   already know how to skip the refcount slot, so we pass the heap
    ///   pointer directly.
    /// - **Value-type struct provider, identifier receiver:** use the
    ///   variable's alloca pointer via `get_data_ptr`. This is in-place
    ///   (no copy), so mutations through `mut ref self` persist back to
    ///   the binding — same semantics as a direct method call.
    /// - **Value-type struct provider, struct-literal receiver (or
    ///   anything else):** alloca a fresh slot, store the compiled value,
    ///   and pass that. The lifetime of the alloca is the enclosing
    ///   function frame, so the runtime stack can hold the pointer for
    ///   the entire `with_provider` body without aliasing concerns.
    pub(super) fn compile_provider_data_ptr(
        &mut self,
        expr: &Expr,
        type_name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        if self.shared_types.contains_key(type_name) {
            let v = self.compile_expr(expr)?;
            let pv = v.into_pointer_value();
            return Ok(pv);
        }
        if let ExprKind::Identifier(name) = &expr.kind {
            if let Some(ptr) = self.get_data_ptr(name) {
                return Ok(ptr);
            }
        }
        let fn_val = self
            .current_fn
            .ok_or_else(|| "with_provider: no current function for provider alloca".to_string())?;
        let v = self.compile_expr(expr)?;
        let alloca = self.create_entry_alloca(fn_val, "wp.data", v.get_type());
        self.builder.build_store(alloca, v).unwrap();
        Ok(alloca)
    }

    /// Inline-compile the `with_provider` body closure. Only the
    /// `||body` literal form is supported — non-zero-arg closures would
    /// indicate a typechecker bug (the with_provider signature requires
    /// `() -> R`), and named closure values would need the indirect
    /// fat-pointer call path which v1 does not wire up here.
    pub(super) fn compile_with_provider_body(
        &mut self,
        closure_expr: &Expr,
        resource: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ExprKind::Closure { params, body, .. } = &closure_expr.kind else {
            return Err(format!(
                "with_provider[{}]: closure argument must be an inline `||body` \
                 literal (named closure bindings unsupported in v1)",
                resource
            ));
        };
        if !params.is_empty() {
            return Err(format!(
                "with_provider[{}]: closure must take zero arguments, got {}",
                resource,
                params.len()
            ));
        }
        self.compile_expr(body)
    }

    /// Theme 6 sub-step 4: lower `R.method(args)` dispatch when `R` is an
    /// `effect resource R: T`. Returns `Some(value)` when dispatch fires;
    /// `None` when `name` isn't a known provider resource, in which case
    /// the caller falls through to `compile_assoc_call` (so non-resource
    /// `Type.method(...)` shapes — `Vec::new`, primitive ops, user
    /// `Type.method` — keep working unchanged).
    ///
    /// IR shape:
    /// ```text
    ///   %res = call %ProviderLookupResult @karac_provider_lookup(<id>)
    ///   %data = extractvalue %ProviderLookupResult %res, 0
    ///   %vt = extractvalue %ProviderLookupResult %res, 1
    ///   %fn_slot = getelementptr [N x ptr], ptr %vt, i64 0, i64 <method_idx>
    ///   %fn = load ptr, ptr %fn_slot
    ///   <ret> = call <FnTy> %fn(%data, <user_args>...)
    /// ```
    ///
    /// Method index comes from the trait's source-declaration order.
    /// The indirect-call FunctionType is borrowed from any concrete
    /// `<U>.<method>` symbol we already declared during impl-method
    /// declaration: every provider impl of the same trait method shares
    /// the same lowered LLVM signature (`*const Self` first arg lowers
    /// to `ptr`, primitives lower the same way regardless of `U`), so
    /// any one will do.
    ///
    /// v1 restriction: no scope-empty / null-vtable runtime check —
    /// the typechecker's effect-checker enforces `R` is in scope at
    /// every call site. A bug there or a programmatic misuse would
    /// crash via null-deref of the vtable load below; tightening this
    /// to a structured panic is a sub-step 6+ task.
    pub(super) fn try_compile_provider_dispatch(
        &mut self,
        name: &str,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let Some(&resource_id) = self.provider_resource_ids.get(name) else {
            return Ok(None);
        };

        // Determine the method order + indirect-call FunctionType. Two kinds
        // of overridable resource reach here:
        //   - **Trait-ful** (`effect resource R: T`): index into the trait's
        //     declaration-order methods; the fn-type comes from any impl of
        //     the trait method.
        //   - **Trait-less *user*** (`effect resource R;`): no trait, so index
        //     into the override type's inherent-impl order recorded in
        //     `user_ambient_resource_methods` (eager vtable pre-pass), with the
        //     resource name `R` itself playing the trait-key role in
        //     `provider_vtables` (`(U, R)`). Both dispatch unconditionally
        //     through the active override (no FFI default) — the effect
        //     checker guarantees `R` is in scope at the call site.
        // A prelude ambient resource (`Clock`, …) is trait-less too but is
        // NOT recorded in `user_ambient_resource_methods`: it falls through
        // to the ambient FFI-default path (`compile_ambient_resource_method`)
        // via `Ok(None)` so an unoverridden call gets the builtin behaviour.
        let (method_order, fn_type) =
            if let Some(trait_name) = self.provider_resource_traits.get(name).cloned() {
                let order = self
                    .provider_trait_methods
                    .get(&trait_name)
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "R.method dispatch: provider trait '{}' has no recorded method order \
                         (vtable emission and dispatch out of sync — codegen bug)",
                            trait_name
                        )
                    })?;
                // Borrow the FunctionType from any impl of this trait method.
                // All impls of the same trait share the same lowered signature.
                let ft = self
                    .provider_method_fn_type(&trait_name, method)
                    .ok_or_else(|| {
                        format!(
                            "R.method dispatch: no impl found for `{}::{}` — at least one \
                     `impl {} for U` must exist to populate the vtable",
                            trait_name, method, trait_name
                        )
                    })?;
                (order, ft)
            } else if let Some(order) = self.user_ambient_resource_methods.get(name).cloned() {
                // Trait-less user resource. The `(U, R)` vtable uses the resource
                // name as its "trait" key, so `provider_method_fn_type(R, method)`
                // recovers the lowered signature from `@U.method`.
                let ft = self.provider_method_fn_type(name, method).ok_or_else(|| {
                    format!(
                        "R.method dispatch: no override impl found for trait-less resource '{}' \
                     method '{}' — a `with_provider[{}]` with a struct-typed provider must \
                     supply an `impl U {{ fn {}(...) }}`",
                        name, method, name, method
                    )
                })?;
                (order, ft)
            } else {
                // `effect resource R;` with no recorded override in this module
                // (never reached a scannable `with_provider[R]` site). Fall through
                // to the regular assoc-call path so an upstream typechecker error
                // or a future R-as-ID use stays observable.
                return Ok(None);
            };
        let method_idx = method_order
            .iter()
            .position(|m| m == method)
            .ok_or_else(|| {
                format!(
                    "R.method dispatch: '{}' is not a method recorded for resource '{}' \
                 (method order: {:?})",
                    method, name, method_order
                )
            })?;

        let i32_t = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let id_v = i32_t.const_int(resource_id as u64, false);

        // 1. karac_provider_lookup(resource_id) → { data, vtable }.
        let lookup_call = self
            .builder
            .build_call(self.karac_provider_lookup_fn, &[id_v.into()], "wp.lookup")
            .unwrap();
        let lookup_sv = lookup_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(lookup_sv, 0, "wp.lookup.data")
            .unwrap()
            .into_pointer_value();
        let vtable_ptr = self
            .builder
            .build_extract_value(lookup_sv, 1, "wp.lookup.vt")
            .unwrap()
            .into_pointer_value();

        // 2. GEP into the vtable for method_idx, load the fn pointer.
        //    Vtable layout is `[N x ptr]` per `emit_provider_vtables`,
        //    so the slot offset is just `method_idx` in pointer units.
        //    Use a flat offset GEP to avoid recomputing the array size.
        let idx_v = i32_t.const_int(method_idx as u64, false);
        let fn_slot = unsafe {
            self.builder
                .build_gep(ptr_ty, vtable_ptr, &[idx_v], "wp.fn.slot")
                .unwrap()
        };
        let fn_ptr = self
            .builder
            .build_load(ptr_ty, fn_slot, "wp.fn")
            .unwrap()
            .into_pointer_value();

        // 3. Build call args: self first (data_ptr OR loaded struct
        //    value, see below), then user args.
        //
        //    The lowered impl method's `self` lowering depends on the
        //    source mode: `ref self` / `mut ref self` lower to a `ptr`
        //    param (the provider's storage address), so we pass
        //    `data_ptr` directly. Owned `self` lowers to a *by-value*
        //    struct param — the runtime stack only holds the storage
        //    address, so we must load the struct value from `data_ptr`
        //    before the call. Without the load the indirect call's
        //    arg type (`ptr`) mismatches the signature's first param
        //    (`{ struct fields... }`) and LLVM's module verifier
        //    rejects the IR with `Call parameter type does not match
        //    function signature!`. The load is safe — provider data
        //    outlives `with_provider`'s body by construction (the
        //    caller's alloca, kept alive by `karac_provider_push`'s
        //    stack frame and popped only at the matching
        //    `karac_provider_pop`). Shared-struct providers are
        //    already handled upstream — `compile_provider_data_ptr`
        //    materializes the RC heap pointer; the trait method's
        //    `self` is `ref Self` in that case, so the load branch is
        //    not taken.
        let self_param_ty = fn_type
            .get_param_types()
            .into_iter()
            .next()
            .ok_or_else(|| {
                format!(
                    "R.method dispatch: provider method `{}.{}` has no self parameter \
                     in its lowered signature — codegen bug",
                    name, method
                )
            })?;
        let self_arg: BasicMetadataValueEnum<'ctx> = match self_param_ty {
            inkwell::types::BasicMetadataTypeEnum::PointerType(_) => {
                BasicMetadataValueEnum::from(data_ptr)
            }
            inkwell::types::BasicMetadataTypeEnum::StructType(st) => {
                let loaded = self
                    .builder
                    .build_load(st, data_ptr, "wp.self.owned")
                    .unwrap();
                BasicMetadataValueEnum::from(loaded)
            }
            other => {
                return Err(format!(
                    "R.method dispatch: unexpected self-param lowering `{:?}` for `{}.{}` — \
                     expected ptr (ref self / mut ref self / shared) or struct (owned self)",
                    other, name, method
                ));
            }
        };
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![self_arg];
        for a in args {
            let v = self.compile_expr(&a.value)?;
            call_args.push(BasicMetadataValueEnum::from(v));
        }

        // 4. Indirect call through the loaded fn pointer.
        let call = self
            .builder
            .build_indirect_call(fn_type, fn_ptr, &call_args, "wp.call")
            .unwrap();
        let basic = call.try_as_basic_value();
        if basic.is_instruction() {
            // void-returning method — fill the expression slot with
            // const-0 i64, mirroring how the user-impl-method dispatch
            // path handles unit-returning method calls.
            Ok(Some(self.context.i64_type().const_int(0, false).into()))
        } else {
            Ok(Some(basic.unwrap_basic()))
        }
    }

    /// Find the LLVM `FunctionType` for a provider trait method by
    /// looking up any concrete `<U>.<method>` symbol whose `(U, T)` pair
    /// is registered in `provider_vtables`. Returns `None` when no impl
    /// has been declared yet (which would mean the vtable couldn't have
    /// been emitted either — handled as a dispatch error by the caller).
    pub(super) fn provider_method_fn_type(
        &self,
        trait_name: &str,
        method: &str,
    ) -> Option<inkwell::types::FunctionType<'ctx>> {
        for (target, t) in self.provider_vtables.keys() {
            if t == trait_name {
                let qualified = format!("{}.{}", target, method);
                if let Some(f) = self.module.get_function(&qualified) {
                    return Some(f.get_type());
                }
            }
        }
        None
    }
}

/// Resolve the concrete override type name of a `with_provider` provider
/// argument during the eager ambient-vtable pre-pass: a struct literal
/// gives its type directly; an identifier resolves through the in-scope
/// `let p = StructLit` bindings. Other shapes (function returns, etc.) are
/// unsupported on the codegen path and return `None` (the `with_provider`
/// lowering then errors at the site with a precise message).
fn ambient_provider_type(
    expr: &Expr,
    bindings: &std::collections::HashMap<String, String>,
    ctor_returns: &std::collections::HashMap<String, String>,
) -> Option<String> {
    match &expr.kind {
        ExprKind::StructLiteral { path, .. } => path.last().cloned(),
        ExprKind::Identifier(n) => bindings.get(n).cloned(),
        // Constructor calls: `makeFoo()` (free fn) or `Type.new()` (assoc
        // fn) — resolve to the callee's declared return type. Mirrors what
        // the real `with_provider` lowering recovers from `var_type_names`
        // for a `let p = makeFoo(); with_provider[R](p, ...)` binding.
        ExprKind::Call { callee, .. } => {
            ctor_call_key(callee).and_then(|k| ctor_returns.get(&k).cloned())
        }
        _ => None,
    }
}

/// Callee → constructor key for [`ambient_provider_type`]'s call arm:
/// a bare `Identifier(name)` keys as `"name"` (free fn); a two-segment
/// `Path([Type, method])` keys as `"Type.method"` (associated fn /
/// `Type.new()`). Other shapes have no constructor key.
fn ctor_call_key(callee: &Expr) -> Option<String> {
    match &callee.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        ExprKind::Path { segments, .. } if segments.len() == 2 => {
            Some(format!("{}.{}", segments[0], segments[1]))
        }
        _ => None,
    }
}

/// Map every free function and inherent (non-trait) impl method to the
/// concrete name of its return type, when that return type is a plain
/// named type. Keyed `"name"` for free fns and `"Type.method"` for impl
/// methods — the same keyspace [`ctor_call_key`] produces — so a provider
/// bound to a constructor call resolves to its struct type in the eager
/// ambient-vtable pre-pass (matching the real lowering's `var_type_names`
/// path). Trait-method impls are skipped: a trait-less resource's override
/// is constructed via an inherent ctor or struct literal, not a trait fn.
fn collect_ctor_return_types(program: &Program) -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                if let Some(ret) = f.return_type.as_ref().and_then(impl_target_name) {
                    out.insert(f.name.clone(), ret);
                }
            }
            Item::ImplBlock(imp) if imp.trait_name.is_none() => {
                let Some(target) = impl_target_name(&imp.target_type) else {
                    continue;
                };
                for ii in &imp.items {
                    if let ImplItem::Method(m) = ii {
                        if let Some(ret) = m.return_type.as_ref().and_then(impl_target_name) {
                            out.insert(format!("{target}.{}", m.name), ret);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Collect each type's inherent-impl (`impl T { ... }`, no trait) method
/// names in source order. Used to derive the canonical method order for a
/// trait-less *user* effect resource (`effect resource R;`) from its
/// override type `U`, since `R` has no trait to pin the vtable layout.
/// A type with multiple inherent impl blocks contributes its methods in
/// block-then-declaration order; trait impls are excluded (a trait-less
/// resource's override dispatches through the type's own inherent methods).
fn collect_inherent_methods(program: &Program) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for item in &program.items {
        if let Item::ImplBlock(imp) = item {
            if imp.trait_name.is_some() {
                continue;
            }
            let Some(target) = impl_target_name(&imp.target_type) else {
                continue;
            };
            let entry = out.entry(target).or_default();
            for ii in &imp.items {
                if let ImplItem::Method(m) = ii {
                    entry.push(m.name.clone());
                }
            }
        }
    }
    out
}
