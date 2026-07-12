//! Codegen lowering for module-level `let` / `let mut` bindings
//! (design.md §1278-1330).
//!
//! Slices 9 + 10 of the phase-8 module-let work — emits one LLVM
//! global per `Item::ModuleBinding`:
//!
//! - Immutable `let X: T = INIT` → `@X = internal constant T INIT`.
//! - Mutable `let mut X: T = INIT` → `@X = internal global T INIT`.
//! - `#[thread_local]` adds `thread_local` storage class
//!   (`GeneralDynamicTLSModel` — the platform-portable default).
//!
//! At use sites, references in function bodies lower to LLVM `load`
//! through `try_load_module_binding`; assignments through
//! `try_store_module_binding`. Both helpers return `Some(_)` when the
//! name resolves to a module binding so callers can short-circuit
//! their existing local-variable / const / fn-pointer dispatch.
//!
//! Initializer surface (slice 9 + 10):
//!
//! - Scalar literals (Integer / Float / Bool / Char / ByteLit) and
//!   `Unary { Neg, primitive }` over them (slice 9).
//! - `StringLit` lowered as the `(ptr, len, cap=0)` `StringSlice`
//!   shape — applies at the top-level binding type per the §1284
//!   carve-out and at nested positions inside composites (the
//!   typechecker accepts the literal in those positions; the
//!   3-word layout matches both `String` and `StringSlice`, and
//!   `cap=0` neutralises any scope-exit free) (slice 9 + 10).
//! - Composite literals — tuple, fixed-size array (`[a, b, c]` and
//!   `Array[a; n]` / bare `[v; n]` for a constant `n`), and struct
//!   constructors (`Foo { … }` whose field values are each
//!   themselves permitted forms) (slice 10).
//! - Enum unit-variants via `EnumName.Variant` paths — tag-only
//!   construction; payload variants are deferred to a follow-up
//!   because their word-stream encoding requires builder ops that
//!   const-init can't run (slice 10).
//! - `Vec.new()` / `VecDeque.new()` zero-arg calls — emitted as the
//!   canonical `{ptr=null, len=0, cap=0}` aggregate matching the
//!   runtime invariant from `assoc_call.rs`'s shared `Vec/VecDeque
//!   && method == "new"` arm. The empty-Vec representation is a
//!   true compile-time constant; no heap allocation is required
//!   until the first `push` at runtime.
//!
//! Deferred (slice 10 documents the position):
//!
//! - Compiler-recognised wrapper special forms (`LazyLock.new(…)`,
//!   `OnceLock.new()`, `OnceCell.new()`, `Atomic.new(LITERAL)`,
//!   `Mutex.new(LITERAL)`) — wait on the wrapper-type entries (each
//!   primitive's own codegen surface lands its const-init lowering).
//! - Identifier references to other module bindings — would need a
//!   forward-resolution pass that re-evaluates the referenced
//!   binding's init AST as a constant in-place; LLVM can't
//!   `load`-from-global as a constant initialiser.
//! - Constant folding of binary / nil-coalesce / non-Neg unary ops
//!   over permitted forms.
//! - Enum payload-variant construction (requires the existing
//!   builder-driven word-stream encoder from
//!   `try_compile_enum_variant`).

use inkwell::module::Linkage;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, GlobalValue};
use inkwell::AddressSpace;

use crate::ast::*;

/// Per-module-binding codegen state. Keyed by source-level binding
/// name in `Codegen::module_bindings`.
#[derive(Clone)]
pub(crate) struct ModuleBindingInfo<'ctx> {
    /// The LLVM global pointer. Use `as_pointer_value()` for
    /// `build_load` / `build_store` operands.
    pub(crate) global: GlobalValue<'ctx>,
    /// The LLVM type of the value stored at the global. Required as
    /// the explicit pointee for opaque-pointer `build_load` calls.
    pub(crate) llvm_ty: BasicTypeEnum<'ctx>,
    /// `true` for `let mut`; `false` for immutable `let`. The
    /// typechecker (slice 5) rejects assignments to immutable
    /// bindings before codegen runs, but the flag is preserved here
    /// so future callers (e.g. `karac explain`) can introspect.
    #[allow(dead_code)]
    pub(crate) is_mut: bool,
    /// The binding's declared `TypeExpr`, when present. Reseeded into
    /// `var_type_names` / `vec_elem_types` / etc. at the start of
    /// every function body (`compile_function` clears those side
    /// tables per function), so field / index / method dispatch sees
    /// the binding's type even though there's no local declaration
    /// inside the function body.
    pub(crate) declared_type: Option<TypeExpr>,
}

impl<'ctx> super::Codegen<'ctx> {
    /// Walk `program.items` and emit one LLVM global per
    /// `Item::ModuleBinding`. Populates `self.module_bindings` keyed
    /// by source name. Runs once at the start of `compile_program`,
    /// before function bodies are compiled, so forward references
    /// from any function body resolve.
    pub(crate) fn declare_module_bindings(&mut self, program: &Program) {
        for item in &program.items {
            let b = match item {
                Item::ModuleBinding(b) => b,
                _ => continue,
            };
            // `Map.new()` / `Set.new()` are NOT compile-time constants:
            // `karac_map_new` installs per-instance hash seeds + a vtable
            // before any op can run. Emit a placeholder `null` `ptr`
            // global (always non-constant — the prologue writes it once,
            // even for an immutable `let`) and defer the real
            // `karac_map_new(...)` to `__karac_static_init`, which runs
            // before `main`'s body. `#[thread_local]` is intentionally
            // not honored for these: the prologue initialises only the
            // main thread's instance (a thread-local Map handle would
            // need per-thread init — out of scope for the v1 floor).
            if let Some(is_set) = module_binding_is_map_set_new(b) {
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let global = self.module.add_global(ptr_ty, None, &b.name);
                global.set_initializer(&ptr_ty.const_null());
                global.set_constant(false);
                global.set_linkage(Linkage::Internal);
                self.module_bindings.insert(
                    b.name.clone(),
                    ModuleBindingInfo {
                        global,
                        llvm_ty: ptr_ty.into(),
                        is_mut: b.is_mut,
                        declared_type: b.ty.clone(),
                    },
                );
                self.map_set_module_inits.push((b.name.clone(), is_set));
                continue;
            }
            // `let CONFIG: OnceLock[T] = OnceLock.new()` — same placeholder-null-
            // ptr-global + static-init-prologue shape as Map/Set (the opaque
            // `*mut KaracOnce` handle is a runtime value, not a compile-time
            // constant). `karac_runtime_once_new` runs in `__karac_static_init`
            // before `main`. Never freed (module-lifetime). `#[thread_local]`
            // not honored (main-thread init only), matching Map/Set.
            if module_binding_is_once_new(b) {
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let global = self.module.add_global(ptr_ty, None, &b.name);
                global.set_initializer(&ptr_ty.const_null());
                global.set_constant(false);
                global.set_linkage(Linkage::Internal);
                self.module_bindings.insert(
                    b.name.clone(),
                    ModuleBindingInfo {
                        global,
                        llvm_ty: ptr_ty.into(),
                        is_mut: b.is_mut,
                        declared_type: b.ty.clone(),
                    },
                );
                self.once_module_inits.push(b.name.clone());
                continue;
            }
            let Some((llvm_ty, initializer)) = self.module_binding_init(b) else {
                // Not a foldable const shape. A COMPUTED / cross-referencing
                // initializer (`let DOUBLED: i64 = COUNT * 2;`) with a declared
                // type is deferred to `__karac_static_init`: declare a zero
                // placeholder global and record the initializer to `compile_expr`
                // there, before `main` (B-2026-07-11-16). Requires the declared
                // type to size the global; an un-annotated computed binding still
                // falls through to a skip (the read then errors — a loud
                // build-time signal, never a silent miscompile).
                // Size the placeholder global from the binding's declared type,
                // or — when un-annotated — the typechecker's inferred type for the
                // value expr (`module_binding_types`, never re-inferred in codegen).
                let ty_expr =
                    b.ty.clone()
                        .or_else(|| self.module_binding_types.get(&b.name).cloned());
                if let Some(ty_expr) = ty_expr {
                    let placeholder_ty = self.llvm_type_for_type_expr(&ty_expr);
                    if let Some(zero) = basic_zero_const(placeholder_ty) {
                        let global = self.module.add_global(placeholder_ty, None, &b.name);
                        global.set_initializer(&zero);
                        // Non-constant: `__karac_static_init` writes it once,
                        // even for an immutable `let` (mirrors the Map/Set path).
                        global.set_constant(false);
                        global.set_linkage(Linkage::Internal);
                        self.module_bindings.insert(
                            b.name.clone(),
                            ModuleBindingInfo {
                                global,
                                llvm_ty: placeholder_ty,
                                is_mut: b.is_mut,
                                declared_type: b.ty.clone(),
                            },
                        );
                        self.computed_module_inits
                            .push((b.name.clone(), b.value.clone()));
                    }
                }
                continue;
            };
            let global = self.module.add_global(llvm_ty, None, &b.name);
            global.set_initializer(&initializer);
            global.set_constant(!b.is_mut);
            global.set_linkage(Linkage::Internal);
            if b.attributes.iter().any(|a| a.is_bare("thread_local")) {
                global
                    .set_thread_local_mode(Some(inkwell::ThreadLocalMode::GeneralDynamicTLSModel));
            }
            self.module_bindings.insert(
                b.name.clone(),
                ModuleBindingInfo {
                    global,
                    llvm_ty,
                    is_mut: b.is_mut,
                    declared_type: b.ty.clone(),
                },
            );
        }

        // Declare (signature only) the static-init prologue if any
        // Map/Set module binding needs runtime initialisation. The body
        // is filled in at `finalize_module_binding_static_init` once all
        // type metadata is available; `main`'s entry emits a forward
        // `call` to it (a valid LLVM reference to an internal fn defined
        // later in the same module).
        if (!self.map_set_module_inits.is_empty()
            || !self.computed_module_inits.is_empty()
            || !self.once_module_inits.is_empty())
            && self.static_init_fn.is_none()
        {
            let void_ty = self.context.void_type();
            let fn_ty = void_ty.fn_type(&[], false);
            let f = self
                .module
                .add_function("__karac_static_init", fn_ty, Some(Linkage::Internal));
            self.static_init_fn = Some(f);
        }
    }

    /// Fill in the `__karac_static_init` body declared by
    /// `declare_module_bindings`: for each `Map.new()` / `Set.new()`
    /// module binding, build a fresh runtime handle (`karac_map_new`)
    /// and store it into the binding's global. Runs after all function
    /// bodies are compiled — so struct/enum type metadata is fully
    /// populated — and before module verification. The handle is never
    /// freed: a module binding lives for the whole process. No-op when
    /// no Map/Set module binding exists.
    ///
    /// `main`'s entry emits a forward `call void @__karac_static_init()`
    /// (see `compile_function`), so this prologue runs before any user
    /// statement observes the global.
    pub(crate) fn finalize_module_binding_static_init(&mut self) {
        let Some(init_fn) = self.static_init_fn else {
            return;
        };
        let inits = self.map_set_module_inits.clone();

        let prev_block = self.builder.get_insert_block();
        let prev_fn = self.current_fn;
        let entry = self.context.append_basic_block(init_fn, "entry");
        self.builder.position_at_end(entry);
        self.current_fn = Some(init_fn);

        for (name, is_set) in inits {
            // Reseed the per-binding key/val type side tables from the
            // declared annotation so `build_map_new_handle` computes the
            // right element sizes + hash/eq fns. `compile_function`
            // clears these per function and finalize runs after the last
            // body, so re-register explicitly here.
            let declared = self
                .module_bindings
                .get(&name)
                .and_then(|i| i.declared_type.clone());
            if let Some(te) = declared {
                self.register_var_from_type_expr(&name, &te);
            }
            let global_ptr = self
                .module_bindings
                .get(&name)
                .map(|i| i.global.as_pointer_value());
            let handle = self.build_map_new_handle(&name, is_set);
            if let Some(gp) = global_ptr {
                self.builder.build_store(gp, handle).unwrap();
            }
        }

        // `OnceLock[T]` module bindings: build a fresh runtime cell handle
        // (`karac_runtime_once_new`) and store it into the binding's global.
        // No side-table reseed is needed to *construct* the cell (the handle is
        // type-agnostic; `T` only matters at the `set`/`get` call sites, where
        // `reseed_module_binding_side_tables` repopulates `once_var_types` per
        // function). Never freed — module-lifetime.
        let once_inits = self.once_module_inits.clone();
        for name in once_inits {
            let global_ptr = self
                .module_bindings
                .get(&name)
                .map(|i| i.global.as_pointer_value());
            let new_fn = self
                .module
                .get_function("karac_runtime_once_new")
                .expect("karac_runtime_once_new declared in Codegen::new");
            let handle = self
                .builder
                .build_call(new_fn, &[], "modonce.new")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            if let Some(gp) = global_ptr {
                self.builder.build_store(gp, handle).unwrap();
            }
        }

        // Computed / cross-referencing initializers (B-2026-07-11-16): compile
        // each stored initializer expr and store it into the binding's global.
        // `compile_expr` handles `Identifier`→load the referenced global (a prior
        // binding's value is already in place — declaration order is preserved,
        // and each store here is visible to a later load) and `Binary`→arithmetic.
        // Reseed the module-binding side tables once so a referenced binding's
        // user-type / collection metadata is visible (mirrors the per-binding
        // reseed the Map/Set loop does).
        let computed = self.computed_module_inits.clone();
        if !computed.is_empty() {
            self.reseed_module_binding_side_tables();
        }
        for (name, init) in computed {
            let global_ptr = self
                .module_bindings
                .get(&name)
                .map(|i| i.global.as_pointer_value());
            let Some(gp) = global_ptr else { continue };
            if let Ok(val) = self.compile_expr(&init) {
                self.builder.build_store(gp, val).unwrap();
            }
        }
        self.builder.build_return(None).unwrap();

        self.current_fn = prev_fn;
        if let Some(bb) = prev_block {
            self.builder.position_at_end(bb);
        }
    }

    /// Reseed `var_type_names` / `vec_elem_types` / etc. for every
    /// module binding that carries a declared type. `compile_function`
    /// clears those side tables on function entry; without this
    /// helper the binding's user-type / collection metadata is
    /// invisible inside function bodies (the field-access /
    /// method-dispatch / index paths consult those tables and fall
    /// through to a silent `i64 0` placeholder when the lookup
    /// misses). Called from `compile_function` after the clear and
    /// before the parameter-registration loop.
    pub(crate) fn reseed_module_binding_side_tables(&mut self) {
        let pairs: Vec<(String, TypeExpr)> = self
            .module_bindings
            .iter()
            .filter_map(|(n, info)| info.declared_type.as_ref().map(|t| (n.clone(), t.clone())))
            .collect();
        for (name, te) in pairs {
            self.register_var_from_type_expr(&name, &te);
            // `Fn(...)`-typed bindings live in `closure_fn_types`, which
            // `register_var_from_type_expr` doesn't cover (fn prologues
            // register params into it directly). Monos swap that table
            // out wholesale (`take_var_side_tables`), so reseed it here.
            if let crate::ast::TypeKind::FnType {
                params,
                return_type,
                ..
            } = &te.kind
            {
                let fn_type = self.closure_abi_fn_type(params, return_type.as_deref());
                self.closure_fn_types.insert(name.clone(), fn_type);
            }
        }
    }

    /// Compute the LLVM type + constant initializer for a module
    /// binding. Returns `None` when the binding's initializer shape
    /// is outside the slice-9-supported surface (defers cleanly to
    /// slice 10 or a future widening).
    fn module_binding_init(
        &self,
        b: &ModuleBinding,
    ) -> Option<(BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>)> {
        // Two paths: declared type + bare string literal short-circuits
        // to the §1284 `StringSlice` carve-out (the literal can't infer
        // to StringSlice on its own), and everything else goes through
        // the value-driven path.
        if let Some(ref ty_expr) = b.ty {
            if let Some(pair) = self.modbind_init_from_declared(ty_expr, &b.value) {
                return Some(pair);
            }
        }
        self.modbind_init_from_value(&b.value)
    }

    /// Initializer path when the binding has an explicit type
    /// annotation. Handles the StringSlice + StringLit pairing per
    /// §1284 and falls back to the value-driven path when the
    /// annotation doesn't change the lowering.
    fn modbind_init_from_declared(
        &self,
        ty_expr: &TypeExpr,
        value: &Expr,
    ) -> Option<(BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>)> {
        if is_string_slice_type(ty_expr) {
            if let ExprKind::StringLit(s) = &value.kind {
                return Some(self.modbind_string_slice_const(s));
            }
        }
        None
    }

    /// Initializer path driven entirely by the value expression's
    /// shape. Covers the literal surface — Integer / Float / Bool /
    /// Char / Byte / StringLit (lowered as StringSlice when no
    /// annotation directs otherwise) — plus `Unary { Neg, primitive }`
    /// and the slice-10 composite shapes (Tuple / ArrayLiteral /
    /// RepeatLiteral / StructLiteral / enum unit-variant paths).
    fn modbind_init_from_value(
        &self,
        value: &Expr,
    ) -> Option<(BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>)> {
        match &value.kind {
            ExprKind::Integer(n, sfx) => {
                let v = self.const_int_for_suffix(*n, *sfx);
                Some((v.get_type().into(), v.into()))
            }
            ExprKind::Float(f, sfx) => {
                let v = self.const_float_for_suffix(*f, *sfx);
                Some((v.get_type().into(), v.into()))
            }
            ExprKind::Bool(b) => {
                let bool_ty = self.context.bool_type();
                Some((
                    bool_ty.into(),
                    bool_ty.const_int(u64::from(*b), false).into(),
                ))
            }
            ExprKind::CharLit(c) => {
                let i32_ty = self.context.i32_type();
                Some((i32_ty.into(), i32_ty.const_int(u64::from(*c), false).into()))
            }
            ExprKind::ByteLit(b) => {
                let i8_ty = self.context.i8_type();
                Some((i8_ty.into(), i8_ty.const_int(u64::from(*b), false).into()))
            }
            ExprKind::StringLit(s) => Some(self.modbind_string_slice_const(s)),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => match &operand.kind {
                ExprKind::Integer(n, sfx) => {
                    let v = self.const_int_for_suffix(-*n, *sfx);
                    Some((v.get_type().into(), v.into()))
                }
                ExprKind::Float(f, sfx) => {
                    let v = self.const_float_for_suffix(-*f, *sfx);
                    Some((v.get_type().into(), v.into()))
                }
                _ => None,
            },
            // ── Composite shapes (slice 10) ──────────────────────────
            ExprKind::Tuple(items) => self.modbind_tuple_const(items),
            ExprKind::ArrayLiteral(items) => self.modbind_array_const(items),
            ExprKind::RepeatLiteral {
                type_name,
                value: elem,
                count,
            } => self.modbind_repeat_const(type_name.as_deref(), elem, count),
            ExprKind::StructLiteral { path, fields, .. } => {
                let name = path.last().map(|s| s.as_str()).unwrap_or("");
                self.modbind_struct_const(name, fields)
            }
            ExprKind::Path { segments, .. } => self.modbind_enum_unit_variant_const(segments),
            // `Vec.new()` / `VecDeque.new()` — runtime invariant is the
            // `{ptr=null, len=0, cap=0}` aggregate (see `assoc_call.rs`'s
            // shared `Vec/VecDeque && method == "new"` arm); emit the
            // matching LLVM const struct as the global's initializer. No
            // heap allocation is needed and the value is a true
            // compile-time constant. The typechecker accepts this shape
            // via the same-named arm in
            // `module_binding_call_is_special_form`.
            ExprKind::Call { callee, args } if args.is_empty() => {
                self.modbind_empty_vec_const(callee)
            }
            _ => None,
        }
    }

    /// Recognise `Vec.new()` / `VecDeque.new()` in the call-callee
    /// position and emit the canonical empty-Vec `{null, 0, 0}` const
    /// struct. Returns `None` for any other callee shape so unrelated
    /// zero-arg calls fall through to the outer `_ => None` rejection.
    fn modbind_empty_vec_const(
        &self,
        callee: &Expr,
    ) -> Option<(BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>)> {
        let ExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        if segments.len() != 2 || segments[1] != "new" {
            return None;
        }
        if segments[0] != "Vec" && segments[0] != "VecDeque" {
            return None;
        }
        let vec_ty = self.vec_struct_type();
        let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
        let zero = self.context.i64_type().const_int(0, false);
        let agg = self
            .context
            .const_struct(&[null_ptr.into(), zero.into(), zero.into()], false);
        Some((vec_ty.into(), agg.into()))
    }

    /// Tuple → anonymous LLVM struct constant. Each element is
    /// lowered as its own constant; the outer struct's field types
    /// follow the inferred per-element types (so a heterogeneous
    /// tuple like `(0_i32, true)` keeps the `(i32, i1)` shape rather
    /// than collapsing to `(i64, i64)`). Returns `None` if any
    /// element falls outside the composite surface.
    fn modbind_tuple_const(
        &self,
        items: &[Expr],
    ) -> Option<(BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>)> {
        let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(items.len());
        let mut field_vals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(items.len());
        for it in items {
            let (ty, val) = self.modbind_init_from_value(it)?;
            field_tys.push(ty);
            field_vals.push(val);
        }
        let struct_ty = self.context.struct_type(&field_tys, false);
        let agg = self.context.const_struct(&field_vals, false);
        Some((struct_ty.into(), agg.into()))
    }

    /// Fixed-size array literal `[a, b, c]` → LLVM array constant.
    /// All elements must lower to the same LLVM type (enforced by
    /// the typechecker before codegen runs). Empty literals are
    /// rejected here because the element type is unknown without an
    /// annotation — slice 9's declared-type path could be extended
    /// later to handle empty `Array[T, 0]` via the annotation.
    fn modbind_array_const(
        &self,
        items: &[Expr],
    ) -> Option<(BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>)> {
        let first = items.first()?;
        let (elem_ty, first_val) = self.modbind_init_from_value(first)?;
        let mut vals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(items.len());
        vals.push(first_val);
        for it in &items[1..] {
            let (ty, val) = self.modbind_init_from_value(it)?;
            if ty != elem_ty {
                return None;
            }
            vals.push(val);
        }
        let arr_ty = array_type_of(elem_ty, items.len() as u32);
        let arr = const_array_of(elem_ty, &vals)?;
        Some((arr_ty, arr))
    }

    /// Repeat literal `[v; n]` (bare or `Array[v; n]`) → LLVM array
    /// constant of length `n`, each element a copy of `v`'s constant.
    /// `Vec[v; n]` is rejected by the typechecker before codegen
    /// because it's heap-allocated.
    fn modbind_repeat_const(
        &self,
        type_name: Option<&str>,
        elem: &Expr,
        count: &Expr,
    ) -> Option<(BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>)> {
        match type_name {
            None | Some("Array") => {}
            _ => return None,
        }
        let n = match &count.kind {
            ExprKind::Integer(n, _) if *n >= 0 => *n as u32,
            _ => return None,
        };
        let (elem_ty, elem_val) = self.modbind_init_from_value(elem)?;
        let arr_ty = array_type_of(elem_ty, n);
        let vals: Vec<BasicValueEnum<'ctx>> = (0..n).map(|_| elem_val).collect();
        let arr = const_array_of(elem_ty, &vals)?;
        Some((arr_ty, arr))
    }

    /// Struct literal `Foo { a: v_a, b: v_b }` → LLVM named-struct
    /// constant. Looks up `Foo` in `struct_types` for the LLVM type
    /// and in `struct_field_names` for the declaration-order layout
    /// so source-order field listing maps into LLVM-order slots.
    /// `..spread` is not supported in const-init position (the
    /// typechecker accepts it structurally but expanding the spread
    /// at codegen would require resolving the spread expression to
    /// a known struct constant; deferred).
    fn modbind_struct_const(
        &self,
        name: &str,
        fields: &[FieldInit],
    ) -> Option<(BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>)> {
        let struct_ty = *self.struct_types.get(name)?;
        let field_order = self.struct_field_names.get(name)?;
        let mut values: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(field_order.len());
        for fname in field_order {
            let field = fields.iter().find(|f| &f.name == fname)?;
            let (_ty, val) = self.modbind_init_from_value(&field.value)?;
            values.push(val);
        }
        if values.len() != field_order.len() {
            return None;
        }
        let agg = struct_ty.const_named_struct(&values);
        Some((struct_ty.into(), agg.into()))
    }

    /// Enum unit-variant via `EnumName.Variant` path → enum layout's
    /// LLVM struct constant with the tag at field 0 and the payload
    /// area zero-initialised (unit variants have zero source-level
    /// payload). Payload-bearing variants `EnumName.Variant(args…)`
    /// arrive as `ExprKind::Call` and aren't handled here — see the
    /// deferred-list in the module header.
    fn modbind_enum_unit_variant_const(
        &self,
        segments: &[String],
    ) -> Option<(BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>)> {
        if segments.len() != 2 {
            return None;
        }
        let enum_name = &segments[0];
        let variant_name = &segments[1];
        let layout = self.enum_layouts.get(enum_name)?;
        let tag = *layout.tags.get(variant_name)?;
        let field_count = layout.field_counts.get(variant_name).copied().unwrap_or(0);
        if field_count != 0 {
            return None;
        }
        let i64_ty = self.context.i64_type();
        let struct_ty = layout.llvm_type;
        let field_tys = struct_ty.get_field_types();
        let mut field_vals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(field_tys.len());
        field_vals.push(i64_ty.const_int(tag, false).into());
        for ft in field_tys.iter().skip(1) {
            field_vals.push(basic_zero_const(*ft)?);
        }
        let agg = struct_ty.const_named_struct(&field_vals);
        Some((struct_ty.into(), agg.into()))
    }

    /// Materialise a constant `StringSlice` (the codegen-layer
    /// `{ptr, len, cap=0}` shape that `Vec` / `String` literals
    /// share). `cap=0` marks the buffer as static so scope-exit
    /// cleanup never tries to `free(ptr)`. The byte payload is
    /// emitted as its own internal-constant global so multiple
    /// bindings that share the same string don't duplicate the data.
    fn modbind_string_slice_const(&self, s: &str) -> (BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>) {
        let bytes = s.as_bytes();
        let i8_ty = self.context.i8_type();
        let arr_ty = i8_ty.array_type(bytes.len() as u32 + 1); // +1 for terminator
        let i64_ty = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let data = self.context.const_string(bytes, true); // null-terminated
        let data_global = self.module.add_global(
            arr_ty,
            None,
            &format!("modbind.str.{}", self.module_bindings.len()),
        );
        data_global.set_initializer(&data);
        data_global.set_constant(true);
        data_global.set_linkage(Linkage::Internal);

        let struct_ty = self.vec_struct_type();
        let len_const = i64_ty.const_int(bytes.len() as u64, false);
        let cap_const = i64_ty.const_int(0, false);
        let data_ptr = data_global.as_pointer_value();
        // Cast the [N x i8]* to i8* for the struct's ptr field.
        let data_ptr_cast = data_ptr.const_cast(ptr_ty);
        let agg = struct_ty.const_named_struct(&[
            data_ptr_cast.as_basic_value_enum(),
            len_const.into(),
            cap_const.into(),
        ]);
        (struct_ty.into(), agg.into())
    }

    /// Load the value at a module binding's global, if `name`
    /// resolves to one. Returns the loaded `BasicValueEnum`. Callers
    /// invoke this from the `compile_expr` Identifier arm before
    /// falling back to `consts` / local-variable lookups.
    pub(crate) fn try_load_module_binding(&self, name: &str) -> Option<BasicValueEnum<'ctx>> {
        let info = self.module_bindings.get(name)?;
        let loaded = self
            .builder
            .build_load(info.llvm_ty, info.global.as_pointer_value(), "modbind.load")
            .ok()?;
        Some(loaded)
    }

    /// Store `val` at a module binding's global, if `name` resolves
    /// to one. Returns `true` when the store fires (caller skips its
    /// local-variable store path). The typechecker rejects writes to
    /// immutable bindings before codegen, so a write reaching this
    /// path on an `is_mut = false` global is impossible under correct
    /// upstream behaviour — LLVM enforces it independently via the
    /// `constant` global flag, which would surface as a verifier
    /// error rather than a silent corruption.
    pub(crate) fn try_store_module_binding(&self, name: &str, val: BasicValueEnum<'ctx>) -> bool {
        let Some(info) = self.module_bindings.get(name) else {
            return false;
        };
        self.builder
            .build_store(info.global.as_pointer_value(), val)
            .ok();
        true
    }
}

/// `Some(is_set)` when the binding's initializer is a zero-arg
/// `Map.new()` (`Some(false)`) or `Set.new()` (`Some(true)`) call.
/// These need a runtime initialiser — `karac_map_new` installs
/// per-instance hash seeds + a vtable — so they take the
/// placeholder-global + static-init-prologue path rather than the
/// const-init lowering. Mirrors the typechecker's
/// `module_binding_call_is_special_form` Map/Set arm.
fn module_binding_is_map_set_new(b: &ModuleBinding) -> Option<bool> {
    let ExprKind::Call { callee, args } = &b.value.kind else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return None;
    };
    if segments.len() != 2 || segments[1] != "new" {
        return None;
    }
    match segments[0].as_str() {
        "Map" => Some(false),
        "Set" => Some(true),
        _ => None,
    }
}

/// `true` when the binding's initializer is a zero-arg `OnceLock.new()` call.
/// Like Map/Set this needs a runtime handle (`karac_runtime_once_new`), so it
/// takes the placeholder-global + static-init path rather than const-init.
/// `OnceCell.new()` is intentionally excluded — `OnceCell` is rejected at
/// module scope by the typechecker (`E_ONCE_CELL_AT_MODULE_SCOPE`), so a
/// module binding is always `OnceLock`.
fn module_binding_is_once_new(b: &ModuleBinding) -> bool {
    let ExprKind::Call { callee, args } = &b.value.kind else {
        return false;
    };
    if !args.is_empty() {
        return false;
    }
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return false;
    };
    segments.len() == 2 && segments[0] == "OnceLock" && segments[1] == "new"
}

/// `true` when `ty` names `StringSlice` (single-segment path). The
/// §1284 carve-out routes a bare string literal through the
/// `(ptr, len)` lowering even though the literal would otherwise
/// infer to `String`. See `check_module_binding_init` for the
/// typechecker mirror.
fn is_string_slice_type(ty: &TypeExpr) -> bool {
    match &ty.kind {
        TypeKind::Path(path) => path
            .segments
            .last()
            .map(|s| s == "StringSlice")
            .unwrap_or(false),
        _ => false,
    }
}

/// Dispatch `BasicType::array_type(n)` over the `BasicTypeEnum`
/// variants — inkwell exposes `array_type` per concrete type with
/// no trait-level entry, so we match the variant manually.
fn array_type_of(elem_ty: BasicTypeEnum<'_>, n: u32) -> BasicTypeEnum<'_> {
    match elem_ty {
        BasicTypeEnum::IntType(t) => t.array_type(n).into(),
        BasicTypeEnum::FloatType(t) => t.array_type(n).into(),
        BasicTypeEnum::PointerType(t) => t.array_type(n).into(),
        BasicTypeEnum::StructType(t) => t.array_type(n).into(),
        BasicTypeEnum::ArrayType(t) => t.array_type(n).into(),
        BasicTypeEnum::VectorType(t) => t.array_type(n).into(),
        BasicTypeEnum::ScalableVectorType(t) => t.array_type(n).into(),
    }
}

/// Build a constant array from per-element constants of a known
/// element type. Each element's enum variant must match `elem_ty`'s
/// variant — the typechecker enforces a homogeneous element type
/// upstream, but we re-check at the value level so a type-mismatch
/// surfaces as a clean `None` rather than an LLVM panic.
fn const_array_of<'ctx>(
    elem_ty: BasicTypeEnum<'ctx>,
    vals: &[BasicValueEnum<'ctx>],
) -> Option<BasicValueEnum<'ctx>> {
    match elem_ty {
        BasicTypeEnum::IntType(t) => {
            let v: Vec<_> = vals
                .iter()
                .map(|v| match v {
                    BasicValueEnum::IntValue(iv) => Some(*iv),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(t.const_array(&v).into())
        }
        BasicTypeEnum::FloatType(t) => {
            let v: Vec<_> = vals
                .iter()
                .map(|v| match v {
                    BasicValueEnum::FloatValue(fv) => Some(*fv),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(t.const_array(&v).into())
        }
        BasicTypeEnum::StructType(t) => {
            let v: Vec<_> = vals
                .iter()
                .map(|v| match v {
                    BasicValueEnum::StructValue(sv) => Some(*sv),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(t.const_array(&v).into())
        }
        BasicTypeEnum::ArrayType(t) => {
            let v: Vec<_> = vals
                .iter()
                .map(|v| match v {
                    BasicValueEnum::ArrayValue(av) => Some(*av),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(t.const_array(&v).into())
        }
        BasicTypeEnum::PointerType(t) => {
            let v: Vec<_> = vals
                .iter()
                .map(|v| match v {
                    BasicValueEnum::PointerValue(pv) => Some(*pv),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(t.const_array(&v).into())
        }
        BasicTypeEnum::VectorType(t) => {
            let v: Vec<_> = vals
                .iter()
                .map(|v| match v {
                    BasicValueEnum::VectorValue(vv) => Some(*vv),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(t.const_array(&v).into())
        }
        BasicTypeEnum::ScalableVectorType(_) => None,
    }
}

/// Zero constant for the given basic type. Used by the enum
/// unit-variant builder to fill the post-tag payload words.
/// Scalable vectors aren't part of the supported surface.
fn basic_zero_const(ty: BasicTypeEnum<'_>) -> Option<BasicValueEnum<'_>> {
    Some(match ty {
        BasicTypeEnum::IntType(t) => t.const_zero().into(),
        BasicTypeEnum::FloatType(t) => t.const_zero().into(),
        BasicTypeEnum::PointerType(t) => t.const_zero().into(),
        BasicTypeEnum::StructType(t) => t.const_zero().into(),
        BasicTypeEnum::ArrayType(t) => t.const_zero().into(),
        BasicTypeEnum::VectorType(t) => t.const_zero().into(),
        BasicTypeEnum::ScalableVectorType(_) => return None,
    })
}
