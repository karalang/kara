//! Codegen lowering for module-level `let` / `let mut` bindings
//! (design.md §1278-1330).
//!
//! Slice 9 of the phase-8 module-let work — emits one LLVM global
//! per `Item::ModuleBinding`:
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
//! Initializer surface (v1): the four primitive literal shapes
//! (Integer / Float / Bool / Char / ByteLit), `StringLit` paired with
//! a `StringSlice` declared type (or no annotation — defaults to
//! StringSlice at module scope per §1284), and `Unary { Neg }`
//! wrapping a primitive literal. Composite initializers
//! (struct/enum/tuple literals, compiler-recognised special forms
//! `LazyLock.new(...)` / `OnceLock.new()` / `Atomic.new(...)`, etc.)
//! are deferred to slice 10's cross-coordination with the wrapper
//! types. The typechecker (slice 4) already rejects any composite
//! shape outside the permitted surface, so the codegen surface only
//! needs to handle the shapes that reach it.

use inkwell::module::Linkage;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, GlobalValue};
use inkwell::AddressSpace;

use crate::ast::*;

/// Per-module-binding codegen state. Keyed by source-level binding
/// name in `Codegen::module_bindings`.
#[derive(Clone, Copy)]
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
            let Some((llvm_ty, initializer)) = self.module_binding_init(b) else {
                // Initializer shape not supported by slice-9 — skip
                // emission. The typechecker rejects any shape outside
                // the permitted surface earlier (§1280-1297); this
                // arm only fires for forms slice 9 explicitly defers
                // (compound literals, special-form constructors).
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
                },
            );
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
    /// annotation directs otherwise) — plus `Unary { Neg, primitive }`.
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
            _ => None,
        }
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
