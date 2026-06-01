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
        // 0. Ambient prelude resources (`Clock`, `Env`, …) have no
        //    `effect resource R: T` declaration, so no provider trait and
        //    no trait-keyed vtable. They override via the ambient path,
        //    which (as of the runtime-dispatch slice) pushes the override
        //    onto the SAME runtime provider stack as user resources —
        //    using a synthesized vtable built from the ambient resource's
        //    canonical method order — so the override is visible across
        //    function-call boundaries, matching the interpreter.
        //
        //    The discriminator is trait-*absence*, NOT ID-absence: codegen
        //    now mints a stable resource ID for every ambient resource
        //    (see `compile_program`), and a few prelude resources
        //    (`Network`, `ProcessTable`) carry an `Item::EffectResource`
        //    declaration so they land in `provider_resource_ids` anyway —
        //    but none has a `: T` provider trait, so keying on
        //    `provider_resource_traits` routes all of them to the ambient
        //    path while leaving user `effect resource R: T` on the vtable
        //    path. (User resources are never in `PRELUDE_EFFECT_RESOURCES`.)
        if crate::prelude::PRELUDE_EFFECT_RESOURCES.contains(&resource)
            && !self.provider_resource_traits.contains_key(resource)
        {
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

    /// `with_provider[R]` lowering for an ambient prelude resource
    /// (`Clock`, `Env`, …) overridden by a statically-typed provider.
    /// Runtime-stack path (unified with the user-resource dispatch): push
    /// the override onto the same `karac_provider_*` stack, keyed by the
    /// ambient resource's minted ID, carrying a vtable synthesized from
    /// the resource's canonical method order. The override is therefore
    /// visible to ambient method calls *across function-call boundaries*
    /// (the call sites consult the runtime stack — see
    /// `try_compile_ambient_dispatch`), matching the interpreter and the
    /// user-resource path. This is what makes `karac test` provider
    /// fixtures work: `test_main_synth` wraps a *call* to the test fn, so
    /// the body's `Clock.now()` is cross-boundary.
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
        let methods = crate::prelude::AMBIENT_RESOURCE_METHODS
            .iter()
            .find(|(r, _)| *r == resource)
            .map(|(_, m)| *m)
            .ok_or_else(|| {
                format!(
                    "with_provider[{}]: no canonical method order for ambient resource — add it \
                     to `prelude::AMBIENT_RESOURCE_METHODS`",
                    resource
                )
            })?;
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
        for item in &program.items {
            match item {
                Item::Function(f) => self.scan_block_for_ambient_overrides(&f.body),
                Item::ImplBlock(imp) => {
                    for ii in &imp.items {
                        if let ImplItem::Method(m) = ii {
                            self.scan_block_for_ambient_overrides(&m.body);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn scan_block_for_ambient_overrides(&mut self, block: &Block) {
        // `let p = StructLit` bindings let a provider passed by identifier
        // resolve to its struct type (test_main_synth binds the fixture
        // ctor to a `let` before the `with_provider`).
        let mut bindings: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, value, .. } => {
                    if let (PatternKind::Binding(name), ExprKind::StructLiteral { path, .. }) =
                        (&pattern.kind, &value.kind)
                    {
                        if let Some(ty) = path.last() {
                            bindings.insert(name.clone(), ty.clone());
                        }
                    }
                    self.scan_expr_for_ambient_overrides(value, &bindings);
                }
                StmtKind::Expr(e) => self.scan_expr_for_ambient_overrides(e, &bindings),
                _ => {}
            }
        }
        if let Some(tail) = &block.final_expr {
            self.scan_expr_for_ambient_overrides(tail, &bindings);
        }
    }

    fn scan_expr_for_ambient_overrides(
        &mut self,
        expr: &Expr,
        bindings: &std::collections::HashMap<String, String>,
    ) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some((resource, provider_expr, closure_expr)) =
                    match_with_provider_call(callee, args)
                {
                    if crate::prelude::PRELUDE_EFFECT_RESOURCES.contains(&resource.as_str())
                        && !self.provider_resource_traits.contains_key(&resource)
                    {
                        if let Some(ty) = ambient_provider_type(provider_expr, bindings) {
                            // Idempotent: `emit_ambient_vtable` no-ops if the
                            // `(U, R)` vtable already exists.
                            let _ = self.emit_ambient_vtable(&ty, &resource);
                        }
                    }
                    if let ExprKind::Closure { body, .. } = &closure_expr.kind {
                        self.scan_expr_for_ambient_overrides(body, bindings);
                    }
                    return;
                }
                for a in args {
                    self.scan_expr_for_ambient_overrides(&a.value, bindings);
                }
            }
            ExprKind::Block(b) => self.scan_block_for_ambient_overrides(b),
            ExprKind::Closure { body, .. } => self.scan_expr_for_ambient_overrides(body, bindings),
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
        let Some(trait_name) = self.provider_resource_traits.get(name).cloned() else {
            // `effect resource R;` (no `: T`) — no dispatch possible.
            // Fall through to the regular assoc-call path so an
            // upstream typechecker error or a future R-as-ID use stays
            // observable.
            return Ok(None);
        };

        let method_order = self
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
        let method_idx = method_order
            .iter()
            .position(|m| m == method)
            .ok_or_else(|| {
                format!(
                "R.method dispatch: '{}' is not a method of provider trait '{}' for resource '{}'",
                method, trait_name, name
            )
            })?;

        // Borrow the FunctionType from any impl of this trait method.
        // All impls of the same trait share the same lowered signature.
        let fn_type = self
            .provider_method_fn_type(&trait_name, method)
            .ok_or_else(|| {
                format!(
                    "R.method dispatch: no impl found for `{}::{}` — at least one \
                     `impl {} for U` must exist to populate the vtable",
                    trait_name, method, trait_name
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
                    "R.method dispatch: provider trait method `{}::{}` has no self parameter \
                     in its lowered signature — codegen bug",
                    trait_name, method
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
                    "R.method dispatch: unexpected self-param lowering `{:?}` for `{}::{}` — \
                     expected ptr (ref self / mut ref self / shared) or struct (owned self)",
                    other, trait_name, method
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
) -> Option<String> {
    match &expr.kind {
        ExprKind::StructLiteral { path, .. } => path.last().cloned(),
        ExprKind::Identifier(n) => bindings.get(n).cloned(),
        _ => None,
    }
}
