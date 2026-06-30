//! AST-to-LLVM type lowering for the Codegen pass.
//!
//! Houses `llvm_type_for_type_expr` (the central `TypeExpr` → LLVM
//! type recursive walker) along with its supporting methods:
//! per-shape LLVM struct constructors (`vec_struct_type`,
//! `slice_struct_type`), primitive / float / int suffix-to-type
//! converters (`llvm_int_type_for_suffix`, `llvm_float_type_for_suffix`,
//! `const_int_for_suffix`, `const_float_for_suffix`,
//! `compile_primitive_const`), per-collection element-type extractors
//! (`extract_slice_elem_type`, `extract_vec_elem_type`,
//! `extract_map_key_name`, `extract_map_kv_types`,
//! `extract_set_elem_type`, `extract_set_elem_name`),
//! type-mangling (`mangled_type_name`), per-var registration
//! (`register_var_from_type_expr`, `register_for_loop_bindings`),
//! a handful of expression-kind recognisers used for early dispatch
//! (`is_map_new_call`, `is_set_new_call`, `is_vec_new_call`,
//! `is_string_new_call`, `is_string_binary_op`,
//! `is_runtime_introspection_call`, `first_operand_is_string`),
//! and the shared-type accessors (`is_shared_type`, `shared_heap_type`,
//! `shared_type_for_expr`).

use std::collections::HashMap;

use crate::ast::*;
use crate::token::{FloatSuffix, IntSuffix};

use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, StructType};
use inkwell::values::BasicValueEnum;
use inkwell::AddressSpace;

use super::helpers::{
    const_value_as_u32, map_kv_type_exprs, set_inner_type_expr, slice_inner_type_expr,
    vec_inner_type_expr,
};
use super::state::SharedTypeInfo;

impl<'ctx> super::Codegen<'ctx> {
    // ── Type resolution ───────────────────────────────────────────

    pub(super) fn llvm_type_for_type_expr(&self, ty: &TypeExpr) -> BasicTypeEnum<'ctx> {
        match &ty.kind {
            TypeKind::Path(path) => {
                let name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
                // Refinement alias (`type Email = String where …`): lower to
                // the base's layout (phase-9 step 4). A refinement is
                // layout-identical to its base — without this it would hit
                // the `i64` fall-through in `llvm_type_for_name` and
                // mis-size a non-`i64`-based refinement.
                if let Some(base) = self.refinement_bases.get(name) {
                    return self.llvm_type_for_type_expr(&base.clone());
                }
                // Distinct type (`distinct type UserId = i64`): layout-
                // identical to its base, so lower to the base's layout (the
                // distinct name itself has no struct/enum def and would hit
                // the `i64` fall-through in `llvm_type_for_name`).
                if let Some(base) = self.distinct_bases.get(name) {
                    return self.llvm_type_for_type_expr(&base.clone());
                }
                if name == "Array" {
                    if let Some(arr_ty) = self.llvm_array_type(&path.generic_args) {
                        return arr_ty;
                    }
                }
                if name == "Vector" {
                    if let Some(vec_ty) = self.llvm_vector_type(&path.generic_args) {
                        return vec_ty;
                    }
                }
                if name == "Vec" || name == "VecDeque" {
                    // `VecDeque[T]` shares `Vec[T]`'s `{ptr, len, cap}`
                    // codegen layout — see `extract_vec_elem_type` and
                    // the `Vec.new` / `VecDeque.new` arm in
                    // `compile_assoc_call`. Without this branch, the
                    // baked `struct VecDeque[T] { }` shape (an empty
                    // struct) isn't in `struct_types` either (codegen
                    // doesn't import baked stdlib items), so the
                    // fall-through reaches `llvm_type_for_name`'s
                    // unknown-name arm and returns `i64`. That would
                    // size every escaped-binding parent slot at 8
                    // bytes — but the par-branch then stores a real
                    // 24-byte vec aggregate at that slot, overflowing
                    // 16 bytes into the adjacent alloca and corrupting
                    // any neighbouring local (the Map+VecDeque
                    // co-existence repro).
                    return self.vec_struct_type().into();
                }
                if name == "Slice" {
                    return self.slice_struct_type().into();
                }
                if name == "Tensor" {
                    // `Tensor[T, Shape]` is a single pointer to one
                    // malloc'd `[rank][dims][data]` block — see
                    // `src/codegen/tensor.rs`. Without this branch the
                    // baked `struct Tensor[T, ...S] { handle_id: i64 }`
                    // shape would lower as a 1-field struct and every
                    // tensor-typed slot would mis-size.
                    return self.context.ptr_type(AddressSpace::default()).into();
                }
                if name == "Column" {
                    // `Column[T]` is a single pointer to one malloc'd
                    // control block `{ ptr data, ptr null_bitmap, i64
                    // len, i64 capacity }` — see `src/codegen/column.rs`.
                    // Like Tensor, the baked `struct Column[=T] {
                    // handle_id: i64 }` shape would otherwise mis-size.
                    return self.context.ptr_type(AddressSpace::default()).into();
                }
                if name == "DataFrame" {
                    // `DataFrame` is a single pointer to one malloc'd
                    // control block `{ ptr entries, i64 len, i64 capacity }`
                    // — see `src/codegen/dataframe.rs`. The baked
                    // `struct DataFrame { handle_id: i64 }` shape would
                    // otherwise mis-size.
                    return self.context.ptr_type(AddressSpace::default()).into();
                }
                if name == "Atomic" {
                    // `Atomic[T]` is a transparent wrapper over `T` —
                    // baked as `struct Atomic[T] { }` in
                    // `runtime/stdlib/atomic.kara`, but at the LLVM
                    // level the storage IS the inner primitive (no
                    // header). `load atomic` / `store atomic`
                    // instructions operate directly on integer
                    // pointers; consumers of `Atomic[i64]` see plain
                    // `i64` slot storage so subsequent `.store(v, ord)`
                    // / `.load(ord)` dispatch (in `compile_method_call`)
                    // targets the same alloca with atomic memory ops.
                    // **`Atomic[bool]` is widened to i8** — LLVM rejects
                    // `load atomic i1` / `store atomic i1` directly, so
                    // the slot is i8 with zext on store and trunc on
                    // load (the wrapping happens in
                    // `compile_atomic_method`, gated on the per-receiver
                    // inner-is-bool side-tables).
                    if let Some(args) = &path.generic_args {
                        if let Some(GenericArg::Type(inner)) = args.first() {
                            if is_bool_type_expr(inner) {
                                return self.context.i8_type().into();
                            }
                            return self.llvm_type_for_type_expr(inner);
                        }
                    }
                }
                // `Mutex[T]` — a spinlock-guarded cell laid out as
                // `{ i64 lockflag, T value }`. Unlike `Atomic[T]` (transparent),
                // `Mutex` carries an explicit lock word: `lock m { ... }`
                // acquires by TAS-spinning on field 0 (`atomicrmw xchg`),
                // exposes field 1 as a `mut ref T` alias for the body, then
                // releases by storing 0. `Mutex.new(v)` builds `{ 0, v }`.
                // (Slice 1: spinlock — a blocking/futex mutex is a perf
                // follow-on.)
                if name == "Mutex" {
                    if let Some(args) = &path.generic_args {
                        if let Some(GenericArg::Type(inner)) = args.first() {
                            let inner_ty = self.llvm_type_for_type_expr(inner);
                            return self
                                .context
                                .struct_type(&[self.context.i64_type().into(), inner_ty], false)
                                .into();
                        }
                    }
                }
                // Map[K,V] and Set[T] are opaque heap pointers managed by the
                // karac_map_* runtime functions.
                if name == "Map" || name == "Set" || name == "SortedSet" || name == "SortedMap" {
                    return self.context.ptr_type(AddressSpace::default()).into();
                }
                // HTTP handler ABI trampoline (2026-05-09, F2): `Request` is
                // an opaque heap pointer wrapping the runtime's
                // `*const KaracHttpRequest`. The shim emitted at the
                // `Server.serve(handler)` dispatch site packs the FFI request
                // pointer into this slot before invoking the user handler;
                // `Request.path()` / `.method()` extract the pointer and
                // round-trip through the runtime externs.
                if name == "Request" {
                    return self.context.ptr_type(AddressSpace::default()).into();
                }
                // Phase 8 `File` handle slice F3: opaque heap pointer
                // wrapping the runtime's `*mut KaracFile`. Constructed by
                // `karac_runtime_file_open` / `_create` / `_append`; freed
                // at scope exit via `karac_runtime_file_close` through the
                // `FreeFileHandle` cleanup action (F4).
                if name == "File" {
                    return self.context.ptr_type(AddressSpace::default()).into();
                }
                // Channel ends (`Sender[T]` / `Receiver[T]`) are opaque heap
                // pointers to the runtime's refcounted `KaracChannel`
                // (`runtime/src/channel.rs`). Both ends carry the same
                // pointer; the element type erases into the queue's byte
                // blobs, so the LLVM type is just `ptr` regardless of `T`.
                // `Channel` itself is only ever used as `Channel.new()` (an
                // associated call), never as a value type — but lower it the
                // same way for uniformity. Drop at scope exit via
                // `CleanupAction::DropChannelEnd` (refcount decrement).
                if name == "Sender" || name == "Receiver" || name == "Channel" {
                    return self.context.ptr_type(AddressSpace::default()).into();
                }
                self.llvm_type_for_name(name)
            }
            TypeKind::Tuple(elems) if elems.is_empty() => {
                // unit type → i64 zero
                self.context.i64_type().into()
            }
            TypeKind::Tuple(elems) => {
                let fields: Vec<BasicTypeEnum<'ctx>> = elems
                    .iter()
                    .map(|e| self.llvm_type_for_type_expr(e))
                    .collect();
                self.context.struct_type(&fields, false).into()
            }
            TypeKind::Ref(_) | TypeKind::MutRef(_) => {
                self.context.ptr_type(AddressSpace::default()).into()
            }
            // Raw pointers (`*const T` / `*mut T`) are genuine LLVM `ptr`
            // values. Until `CStr.as_ptr` landed (the first safe
            // pointer-producer — design.md § C-String Literals), no Kāra
            // expression ever *produced* a pointer value, so the `_ => i64`
            // fall-through below silently mis-declared every pointer-typed
            // extern/host-fn parameter as `i64` — unobservable while
            // pointer params were declare-only, but a module-verifier
            // error ("Call parameter type does not match function
            // signature") the moment a real `ptr` argument reaches the
            // call. `wasm_glue.rs`'s `JsScalar::Number` mapping for
            // pointer params already assumed the real `ptr` lowering
            // (wasm32 pointers are i32-width scalars, not i64).
            TypeKind::Pointer { .. } => self.context.ptr_type(AddressSpace::default()).into(),
            TypeKind::MutSlice(_) => self.slice_struct_type().into(),
            // A first-class `Fn(...)` / `OnceFn(...)` value is represented by
            // the same `{fn_ptr, env_ptr}` closure fat-pointer struct as a
            // closure literal (`closure_value_type`). Without this arm the
            // type fell through to the `i64` default, so a `Fn`-typed
            // parameter / field / local slot was mis-sized at 8 bytes while a
            // closure value (or a reified bare fn name) is a 16-byte fat
            // pointer — a higher-order call (`apply(doubler, x)` against
            // `fn apply(f: Fn(i64)->i64, …)`) then failed LLVM module
            // verification: "Call parameter type does not match function
            // signature" (B-2026-06-20-1).
            TypeKind::FnType { .. } => self.closure_value_type().into(),
            _ => self.context.i64_type().into(),
        }
    }

    /// Extract the inner type from a ref/mut ref type expression.
    pub(super) fn inner_type_of_ref(&self, ty: &TypeExpr) -> Option<BasicTypeEnum<'ctx>> {
        match &ty.kind {
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) => {
                Some(self.llvm_type_for_type_expr(inner))
            }
            _ => None,
        }
    }

    /// Lower `Array[T, N]` generic args to an LLVM `[N x T]` type.
    /// Mirrors typechecker::lower_array_type — accepts only positive integer-literal size.
    pub(super) fn llvm_array_type(
        &self,
        generic_args: &Option<Vec<GenericArg>>,
    ) -> Option<BasicTypeEnum<'ctx>> {
        let args = generic_args.as_ref()?;
        if args.len() != 2 {
            return None;
        }
        let elem_ty_expr = match &args[0] {
            GenericArg::Type(t) => t,
            GenericArg::Const(_) => return None,
            GenericArg::Shape(_) => return None,
        };
        let size = match &args[1] {
            GenericArg::Const(expr) => match &expr.kind {
                ExprKind::Integer(n, _) if *n >= 0 => *n as u32,
                // Const generics slice 4: const-param identifier ref
                // (`Array[T, N]` where `N` is a const-generic param
                // bound at the active monomorphization). Look up in
                // `const_subst` and recover the integer width.
                ExprKind::Identifier(name) => {
                    let cv = self.const_subst.get(name)?;
                    const_value_as_u32(cv)?
                }
                _ => return None,
            },
            // Slice 1c parser disambiguation: the generic-args parser
            // can't distinguish a type-param ref from a const-param
            // ref at parse time (no scope info), so `Array[T, N]`
            // routes N as `GenericArg::Type(Path(N))`. Recover the
            // const-param at codegen extraction.
            GenericArg::Type(te) => {
                if let TypeKind::Path(p) = &te.kind {
                    if p.segments.len() == 1 && p.generic_args.is_none() {
                        let name = &p.segments[0];
                        if let Some(cv) = self.const_subst.get(name) {
                            const_value_as_u32(cv)?
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            GenericArg::Shape(_) => return None,
        };
        let elem_ty = self.llvm_type_for_type_expr(elem_ty_expr);
        Some(elem_ty.array_type(size).into())
    }

    /// Lower `Vector[T, N]` generic args to an LLVM `<N x T>` SIMD vector type
    /// (design.md § Portable SIMD — `repr(simd)` layout). Distinct from
    /// [`llvm_array_type`]'s `[N x T]` aggregate: arithmetic on `<N x T>` is
    /// element-wise and LLVM's instruction selector emits native SIMD where the
    /// target supports it (tier 1) and scalarizes otherwise (tier 3) — so the
    /// auto-fallback rule is the backend's job for basic ops. The lane count /
    /// element extraction mirrors `llvm_array_type`; only the final
    /// `vec_type` vs `array_type` differs. Element is always int or float (the
    /// typechecker rejects non-numeric `T` at `lower_vector_type`).
    pub(super) fn llvm_vector_type(
        &self,
        generic_args: &Option<Vec<GenericArg>>,
    ) -> Option<BasicTypeEnum<'ctx>> {
        let args = generic_args.as_ref()?;
        if args.len() != 2 {
            return None;
        }
        let elem_ty_expr = match &args[0] {
            GenericArg::Type(t) => t,
            GenericArg::Const(_) => return None,
            GenericArg::Shape(_) => return None,
        };
        let size = match &args[1] {
            GenericArg::Const(expr) => match &expr.kind {
                ExprKind::Integer(n, _) if *n > 0 => *n as u32,
                ExprKind::Identifier(name) => {
                    let cv = self.const_subst.get(name)?;
                    const_value_as_u32(cv)?
                }
                _ => return None,
            },
            GenericArg::Type(te) => {
                if let TypeKind::Path(p) = &te.kind {
                    if p.segments.len() == 1 && p.generic_args.is_none() {
                        let name = &p.segments[0];
                        if let Some(cv) = self.const_subst.get(name) {
                            const_value_as_u32(cv)?
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            GenericArg::Shape(_) => return None,
        };
        let elem_ty = self.llvm_type_for_type_expr(elem_ty_expr);
        let vec_ty = match elem_ty {
            BasicTypeEnum::IntType(it) => it.vec_type(size),
            BasicTypeEnum::FloatType(ft) => ft.vec_type(size),
            _ => return None,
        };
        Some(vec_ty.into())
    }

    /// Vec[T] runtime layout: `{ ptr data, i64 len, i64 capacity }`.
    pub(super) fn vec_struct_type(&self) -> StructType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default()).into();
        let i64_ty = self.context.i64_type().into();
        self.context.struct_type(&[ptr_ty, i64_ty, i64_ty], false)
    }

    /// True when `ty` is the runtime `{ptr, i64, i64}` shape used for
    /// `Vec[T]` and `String`. Used by the recursive-drop cleanup path to
    /// decide whether each element of an outer `Vec[Vec[…]]` needs its
    /// own data buffer freed before the outer buffer is released.
    /// Comparison is by LLVM type identity — every codegen call to
    /// `vec_struct_type()` returns the same context-uniqued struct, so
    /// pointer equality on the structs is sound.
    pub(super) fn llvm_ty_is_vec_struct(&self, ty: BasicTypeEnum<'ctx>) -> bool {
        match ty {
            BasicTypeEnum::StructType(st) => st == self.vec_struct_type(),
            _ => false,
        }
    }

    /// When `ty` is a by-value aggregate (tuple / user struct) stored as a
    /// `Vec` element and at least one of its fields is an owned Vec / String
    /// (`vec_struct_type`-shaped `{ptr, len, cap}`), return those field
    /// indices; `None` otherwise (including when `ty` IS the Vec/String
    /// struct — that case is the `llvm_ty_is_vec_struct` fast path).
    ///
    /// Drives the `FreeVecBuffer` drain's recursion into a heap-bearing
    /// tuple element (`Vec[(i64, String)]`, B-2026-06-10-5): the vec-struct
    /// fast path only frees an element that is ITSELF a Vec/String, so a
    /// String nested in a tuple element leaks. A Vec element is always owned
    /// (borrows are never stored owned in a Vec), so every vec-struct-shaped
    /// field is an owned buffer to free. One level into the element — a heap
    /// field that is itself a tuple / Map / nested collection is not reached
    /// (same deeper-nesting limitation as the one-level Vec recursion).
    pub(super) fn struct_owned_vec_field_indices(
        &self,
        ty: BasicTypeEnum<'ctx>,
    ) -> Option<Vec<u32>> {
        let st = match ty {
            BasicTypeEnum::StructType(st) => st,
            _ => return None,
        };
        if st == self.vec_struct_type() {
            return None;
        }
        let vec_ty: BasicTypeEnum<'ctx> = self.vec_struct_type().into();
        let idxs: Vec<u32> = (0..st.count_fields())
            .filter(|&i| st.get_field_type_at_index(i) == Some(vec_ty))
            .collect();
        if idxs.is_empty() {
            None
        } else {
            Some(idxs)
        }
    }

    /// Slice[T] and `mut Slice[T]` runtime layout: `{ ptr data, i64 len }`.
    /// Mutability is a type-system concept — the physical layout is identical
    /// for read-only and mutable slices. 16 bytes on 64-bit platforms.
    pub(super) fn slice_struct_type(&self) -> StructType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default()).into();
        let i64_ty = self.context.i64_type().into();
        self.context.struct_type(&[ptr_ty, i64_ty], false)
    }

    /// Phase 8 `File` handle slice F3/F4: ABI struct the
    /// `karac_runtime_file_*` externs write into via out-param.
    /// Layout `{ i64 value, i32 error_kind, i32 _pad, ptr
    /// error_msg_ptr, i64 error_msg_len }` — 32 bytes, alignment 8,
    /// pinned by the runtime crate's `test_io_result_layout_pinned`.
    /// F4 method codegen allocas a slot of this type, passes its
    /// pointer to the runtime call, then GEPs + loads the field
    /// values to construct the surface `Result[T, IoError]`.
    pub(super) fn kara_io_result_type(&self) -> StructType<'ctx> {
        let i64_ty = self.context.i64_type().into();
        let i32_ty = self.context.i32_type().into();
        let ptr_ty = self.context.ptr_type(AddressSpace::default()).into();
        self.context
            .struct_type(&[i64_ty, i32_ty, i32_ty, ptr_ty, i64_ty], false)
    }

    /// Produce an LLVM integer type matching the source-level suffix.
    /// `None` defaults to `i64`. `I128`/`U128` map to LLVM `i128_type`
    /// (added 2026-05-11 alongside the `IntSize::I128`/`UIntSize::U128`
    /// typechecker extension that unblocks const generics slice 2b).
    pub(super) fn llvm_int_type_for_suffix(
        &self,
        sfx: Option<IntSuffix>,
    ) -> inkwell::types::IntType<'ctx> {
        match sfx {
            None => self.context.i64_type(),
            Some(IntSuffix::I8) | Some(IntSuffix::U8) => self.context.i8_type(),
            Some(IntSuffix::I16) | Some(IntSuffix::U16) => self.context.i16_type(),
            Some(IntSuffix::I32) | Some(IntSuffix::U32) => self.context.i32_type(),
            Some(IntSuffix::I64) | Some(IntSuffix::U64) => self.context.i64_type(),
            Some(IntSuffix::I128) | Some(IntSuffix::U128) => self.context.i128_type(),
        }
    }

    pub(super) fn llvm_float_type_for_suffix(
        &self,
        sfx: Option<FloatSuffix>,
    ) -> inkwell::types::FloatType<'ctx> {
        match sfx {
            None | Some(FloatSuffix::F64) => self.context.f64_type(),
            Some(FloatSuffix::F32) => self.context.f32_type(),
        }
    }

    pub(super) fn const_int_for_suffix(
        &self,
        n: i64,
        sfx: Option<IntSuffix>,
    ) -> inkwell::values::IntValue<'ctx> {
        let is_signed = matches!(
            sfx,
            None | Some(IntSuffix::I8)
                | Some(IntSuffix::I16)
                | Some(IntSuffix::I32)
                | Some(IntSuffix::I64)
                | Some(IntSuffix::I128)
        );
        self.llvm_int_type_for_suffix(sfx)
            .const_int(n as u64, is_signed)
    }

    pub(super) fn const_float_for_suffix(
        &self,
        f: f64,
        sfx: Option<FloatSuffix>,
    ) -> inkwell::values::FloatValue<'ctx> {
        self.llvm_float_type_for_suffix(sfx).const_float(f)
    }

    /// Lower a primitive-type associated constant (`i64.MAX`,
    /// `f64.INFINITY`, `usize.MAX`, etc.) to an LLVM constant of the
    /// matching width. The interpreter consumes the same `ConstValue`
    /// table but coerces signed / unsigned to a single `Value::Int(i64)`;
    /// codegen keeps the precise width so downstream arithmetic
    /// type-checks correctly at the LLVM level.
    pub(super) fn compile_primitive_const(
        &self,
        cv: &crate::prelude::ConstValue,
    ) -> BasicValueEnum<'ctx> {
        use crate::prelude::ConstValue::*;
        match cv {
            I8(v) => self.context.i8_type().const_int(*v as u64, true).into(),
            I16(v) => self.context.i16_type().const_int(*v as u64, true).into(),
            I32(v) => self.context.i32_type().const_int(*v as u64, true).into(),
            I64(v) => self.context.i64_type().const_int(*v as u64, true).into(),
            // const generics slice 2b — i128 / u128 use LLVM's native
            // i128 type. The const_int builder takes a u64; for 128-bit
            // values we lose the high bits — acceptable for v1's source
            // surface (parser literals are bounded to i64 / u64), and
            // round-trips clean for values that fit. A future widening
            // of `ExprKind::Integer` to carry i128 bits replaces this
            // truncation with `const_int_arbitrary_precision`.
            I128(v) => self.context.i128_type().const_int(*v as u64, true).into(),
            U8(v) => self.context.i8_type().const_int(*v as u64, false).into(),
            U16(v) => self.context.i16_type().const_int(*v as u64, false).into(),
            U32(v) => self.context.i32_type().const_int(*v as u64, false).into(),
            U64(v) => self.context.i64_type().const_int(*v, false).into(),
            U128(v) => self.context.i128_type().const_int(*v as u64, false).into(),
            // v1 is 64-bit only — usize is u64.
            Usize(v) => self.context.i64_type().const_int(*v, false).into(),
            // Float widths. `const_float` accepts an f64 input and
            // narrows for f32; for INFINITY / NEG_INFINITY / NAN this
            // round-trip is exact (the bit patterns survive the
            // f64→f32 conversion).
            F32(v) => self.context.f32_type().const_float(*v as f64).into(),
            F64(v) => self.context.f64_type().const_float(*v).into(),
            // Const generics slice 2 — Bool / Char / EnumVariant flow
            // through here only when a primitive-table constant uses
            // one of these variants (none do today). Slice 4 wires
            // const-param identifier lowering to call `compile_primitive_const`
            // for const-args at use sites; this arm prepares the surface.
            Bool(b) => self.context.bool_type().const_int(*b as u64, false).into(),
            Char(c) => self.context.i32_type().const_int(*c as u64, false).into(),
            // Fieldless-enum variant: tag-only payload as `i64` (matches
            // the existing enum-variant tag width).
            EnumVariant { discriminant, .. } => self
                .context
                .i64_type()
                .const_int(*discriminant as u64, true)
                .into(),
        }
    }

    /// Infer the slice element type from a let-binding RHS that produces
    /// a slice value. Recognizes `.as_slice()` / `.as_slice_mut()` on a
    /// known sequence variable, range-indexing `x[a..b]` on the same,
    /// and `s.bytes()` on a `String` (always `u8`, since `bytes()` is
    /// String-only at the typechecker layer).
    /// Returns `None` when the RHS is not a slice-producing shape.
    pub(super) fn infer_slice_elem_from_rhs(&self, expr: &Expr) -> Option<BasicTypeEnum<'ctx>> {
        match &expr.kind {
            ExprKind::MethodCall { object, method, .. }
                if method == "as_slice" || method == "as_slice_mut" =>
            {
                self.infer_elem_from_source(object)
            }
            ExprKind::MethodCall { method, .. } if method == "bytes" => {
                // `String.bytes() -> Slice[u8]`. Element type is fixed
                // — typechecker has gated the receiver to String.
                Some(self.context.i8_type().into())
            }
            ExprKind::MethodCall { method, .. } if method == "as_bytes" => {
                // `CStr.as_bytes() -> Slice[u8]` (design.md § C-String
                // Literals). Same fixed-u8 story as `bytes` above —
                // `as_bytes` is CStr-only at the typechecker layer, and
                // the value is already the `{ptr, i64}` slice header.
                Some(self.context.i8_type().into())
            }
            ExprKind::Index { object, index } => {
                if matches!(&index.kind, ExprKind::Range { .. }) {
                    self.infer_elem_from_source(object)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Look up the element LLVM type of a known sequence variable (Array,
    /// Vec, or Slice).
    pub(super) fn infer_elem_from_source(&self, object: &Expr) -> Option<BasicTypeEnum<'ctx>> {
        let name = if let ExprKind::Identifier(n) = &object.kind {
            n
        } else {
            return None;
        };
        if let Some(slot) = self.variables.get(name.as_str()) {
            if let BasicTypeEnum::ArrayType(at) = slot.ty {
                return Some(at.get_element_type());
            }
        }
        if let Some(&elem) = self.slice_elem_types.get(name.as_str()) {
            return Some(elem);
        }
        if let Some(&elem) = self.vec_elem_types.get(name.as_str()) {
            return Some(elem);
        }
        if let Some(&BasicTypeEnum::ArrayType(at)) = self.ref_params.get(name.as_str()) {
            return Some(at.get_element_type());
        }
        None
    }

    /// Extract the element LLVM type from a `Slice[T]` or `mut Slice[T]`
    /// type expression.
    pub(super) fn extract_slice_elem_type(&self, te: &TypeExpr) -> Option<BasicTypeEnum<'ctx>> {
        match &te.kind {
            TypeKind::Path(path) => {
                let name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
                if name != "Slice" {
                    return None;
                }
                let args = path.generic_args.as_ref()?;
                if args.len() != 1 {
                    return None;
                }
                match &args[0] {
                    GenericArg::Type(t) => Some(self.llvm_type_for_type_expr(t)),
                    GenericArg::Const(_) => None,
                    GenericArg::Shape(_) => None,
                }
            }
            TypeKind::MutSlice(element) => Some(self.llvm_type_for_type_expr(element)),
            // `ref Slice[T]` / `mut ref Slice[T]` — a reference to an
            // already-borrowed slice handle (design.md § Slices uses
            // `ref Slice[T]`). Peel the redundant `ref` and report the inner
            // slice's element type, so the call site synthesizes a proper
            // `{ptr,len}` header for it instead of classifying it as a bare ref
            // param. Recursing is safe — a non-slice inner (`ref Vec[T]`,
            // `ref i64`) still returns `None`. Without this, `ref Slice` was a
            // bare ref param and an `Array` argument was passed as its raw
            // element storage — a bogus slice header → segfault
            // (B-2026-06-19-1).
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) => self.extract_slice_elem_type(inner),
            _ => None,
        }
    }

    /// Extract the element LLVM type from a `Vec[T]` type expression.
    pub(super) fn extract_vec_elem_type(&self, te: &TypeExpr) -> Option<BasicTypeEnum<'ctx>> {
        if let TypeKind::Path(path) = &te.kind {
            let name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
            // `VecDeque[T]` shares `Vec[T]`'s `{ptr, len, cap}` codegen
            // layout — front-end ops (`push_front` / `pop_front`)
            // translate to memmove-shifted insert/remove at index 0
            // inside `compile_vec_method`. The mirror means a
            // `let q: VecDeque[i64] = ...` binding registers under
            // `vec_elem_types` and dispatches through the existing
            // Vec method-call surface unchanged.
            if name == "Vec" || name == "VecDeque" {
                if let Some(args) = &path.generic_args {
                    if let Some(GenericArg::Type(elem_te)) = args.first() {
                        return Some(self.llvm_type_for_type_expr(elem_te));
                    }
                }
            }
        }
        None
    }

    /// For an `Option[T]` type expr, return the `FreeVecBuffer`-style
    /// element type used to free the inline `Some` payload's nested heap,
    /// when `T` is a heap-owning `{ptr,len,cap}` value (`String` / `Vec[U]`
    /// / `VecDeque[U]`). `Some(i8)` for `String` (free the char buffer, no
    /// recursion); `Some(llvm(U))` for `Vec[U]` (a Vec-struct `U` triggers
    /// the per-element inner free in the emitter). `None` for any other `T`
    /// — scalar (`Option[i64]`), shared/RC, `Map`/`Set`, struct/tuple, or
    /// oversized-boxed payloads (handled by other cleanup paths or out of
    /// this slice) — and for any non-`Option` type. Drives
    /// `CleanupAction::FreeInlineOptionPayload` registration; the erased
    /// `Option` layout can't carry this per-instantiation choice itself.
    /// See B-2026-06-10-6.
    pub(super) fn option_inline_payload_elem(&self, te: &TypeExpr) -> Option<BasicTypeEnum<'ctx>> {
        let TypeKind::Path(p) = &te.kind else {
            return None;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return None;
        }
        let arg0 = match p.generic_args.as_ref()?.first()? {
            GenericArg::Type(t) => t,
            _ => return None,
        };
        self.inline_heap_payload_elem(arg0)
    }

    /// For an `Option[Map[K,V]]` / `Option[Set[T]]` type expr, return the
    /// per-half map drop classification for the inline handle payload (the
    /// same `MapElemDrop` a standalone Map binding / `Vec[Map]` element uses).
    /// `None` for any non-`Option`, or an `Option` whose arg isn't `Map`/`Set`
    /// (those go through `option_inline_payload_elem` or leak nothing). The
    /// erased `Option` layout can't carry this; drives
    /// `CleanupAction::FreeInlineOptionMapPayload`. B-2026-06-10-6 follow-on.
    pub(super) fn option_inline_map_payload(
        &self,
        te: &TypeExpr,
    ) -> Option<crate::codegen::state::MapElemDrop<'ctx>> {
        let TypeKind::Path(p) = &te.kind else {
            return None;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return None;
        }
        let arg0 = match p.generic_args.as_ref()?.first()? {
            GenericArg::Type(t) => t,
            _ => return None,
        };
        self.vec_elem_map_drop_for_type_expr(arg0)
    }

    /// The `FreeVecBuffer`-style element type for a single enum-payload type
    /// argument `arg`, when `arg` is an inline heap `{ptr,len,cap}` value
    /// (`String` / `Vec[U]` / `VecDeque[U]`). `Some(i8)` for `String`;
    /// `Some(llvm(U))` for `Vec[U]` (a Vec-struct `U` triggers the per-
    /// element inner free in the emitter). `None` for scalar / shared-RC /
    /// Map/Set / struct/tuple / boxed payloads. Shared by the `Option` and
    /// `Result` inline-payload detectors (B-2026-06-10-6).
    pub(super) fn inline_heap_payload_elem(&self, arg: &TypeExpr) -> Option<BasicTypeEnum<'ctx>> {
        if self.is_string_type_expr(arg) {
            return Some(self.context.i8_type().into());
        }
        if let TypeKind::Path(ip) = &arg.kind {
            if matches!(
                ip.segments.last().map(|s| s.as_str()),
                Some("Vec") | Some("VecDeque")
            ) {
                // The Vec payload is itself `{ptr,len,cap}`; its element
                // type drives the recursive inner free (a Vec-struct elem
                // → per-element buffer free, a scalar → outer buffer only).
                return self
                    .extract_vec_elem_type(arg)
                    .or_else(|| Some(self.context.i8_type().into()));
            }
        }
        None
    }

    /// For a `Result[T, E]` type expr, return `(ok_elem, err_elem)` payload
    /// element types for the `Ok`/`Err` inline-heap overlays — each `Some`
    /// only when that half is an inline heap `{ptr,len,cap}` value. Returns
    /// the outer `Some((..,..))` only when AT LEAST ONE half is heap (so a
    /// fully-scalar `Result[i64, i32]` registers no cleanup). Drives
    /// `CleanupAction::FreeInlineResultPayload`; the erased `Result` layout
    /// can't carry these per-instantiation choices. B-2026-06-10-6 follow-on.
    #[allow(clippy::type_complexity)]
    pub(super) fn result_inline_payload_elems(
        &self,
        te: &TypeExpr,
    ) -> Option<(Option<BasicTypeEnum<'ctx>>, Option<BasicTypeEnum<'ctx>>)> {
        let TypeKind::Path(p) = &te.kind else {
            return None;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Result") {
            return None;
        }
        let args = p.generic_args.as_ref()?;
        let ok_arg = match args.first()? {
            GenericArg::Type(t) => t,
            _ => return None,
        };
        let err_arg = match args.get(1)? {
            GenericArg::Type(t) => t,
            _ => return None,
        };
        let ok_elem = self.inline_heap_payload_elem(ok_arg);
        let err_elem = self.inline_heap_payload_elem(err_arg);
        if ok_elem.is_none() && err_elem.is_none() {
            return None;
        }
        Some((ok_elem, err_elem))
    }

    /// `StringSlice` borrowed-view type — a `Path` whose head segment is
    /// `StringSlice`. Kept separate from [`is_string_type_expr`] so the
    /// owned-String drop/copy/move consumers of that predicate don't treat a
    /// borrow as owned (design.md § StringSlice).
    pub(super) fn is_string_slice_type_expr(te: &TypeExpr) -> bool {
        matches!(
            &te.kind,
            TypeKind::Path(p) if p.segments.first().map(|s| s.as_str()) == Some("StringSlice")
        )
    }

    pub(super) fn is_string_type_expr(&self, te: &TypeExpr) -> bool {
        if let TypeKind::Path(path) = &te.kind {
            // "str" is the typechecker-internal spelling (`Type::Str` →
            // `type_to_type_expr` → `path("str")`, e.g. in
            // `pattern_binding_inner_types` for an untyped
            // `let combos = make();` where make returns Vec[String]) —
            // every other String/str consumer in codegen already treats
            // the two as synonyms (`synth.rs`, `declarations.rs`,
            // `types_lowering::llvm_type_for_type_expr`, …). Without the
            // "str" arm here, the indexed-receiver synth registration
            // (`combos[j].len()`) missed the String side-tables and
            // method dispatch fell through (kata-22 bench, 2026-06-06).
            matches!(
                path.segments.first().map(|s| s.as_str()),
                Some("String") | Some("str")
            )
        } else {
            false
        }
    }

    /// True for `CStr` and `ref CStr` type expressions. The `ref` form is
    /// the surface type of a `c"..."` literal (design.md § C-String
    /// Literals); the bare form is accepted because the fat `{ptr, i64}`
    /// value IS the reference — there's no distinct owned layout at v1.
    pub(super) fn is_cstr_type_expr(te: &TypeExpr) -> bool {
        match &te.kind {
            TypeKind::Path(path) => path.segments.last().map(|s| s.as_str()) == Some("CStr"),
            TypeKind::Ref(inner) => Self::is_cstr_type_expr(inner),
            _ => false,
        }
    }

    /// Extract the key type name string from a `Map[K, V]` type expression.
    /// Returns a canonical mangled name suitable for `karac_hash_<name>` —
    /// path segment for named types, `tuple_T1_T2_…_Tn` for tuples (recursive).
    pub(super) fn extract_map_key_name(te: &TypeExpr) -> Option<String> {
        if let TypeKind::Path(path) = &te.kind {
            if path.segments.first().map(|s| s.as_str()) != Some("Map") {
                return None;
            }
            let args = path.generic_args.as_ref()?;
            if let Some(GenericArg::Type(k_te)) = args.first() {
                return Some(Self::mangled_type_name(k_te));
            }
        }
        None
    }

    /// Produce a canonical, deterministic name for a `TypeExpr` suitable for
    /// use as a per-type function suffix (`karac_hash_<name>`, `karac_eq_<name>`).
    /// Path types collapse to their head segment; tuples mangle recursively as
    /// `tuple_T1_T2_…_Tn`. Unsupported shapes fall back to "unknown" — the
    /// typechecker's `K: Hash + Eq` enforcement prevents codegen from ever
    /// reaching such a key type.
    pub(super) fn mangled_type_name(te: &TypeExpr) -> String {
        match &te.kind {
            TypeKind::Path(p) => p
                .segments
                .first()
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            TypeKind::Tuple(elems) if elems.is_empty() => "unit".to_string(),
            TypeKind::Tuple(elems) => {
                let parts: Vec<String> = elems.iter().map(Self::mangled_type_name).collect();
                format!("tuple_{}", parts.join("_"))
            }
            _ => "unknown".to_string(),
        }
    }

    /// If `name` is a refinement type (`type Email = String where …`),
    /// resolve it to the *name* of its base type, peeling nested
    /// refinements; otherwise return `name` unchanged. Codegen records a
    /// binding's type name in `var_type_names` for value-level dispatch
    /// (method calls, `println`, arg coercion); a refinement carries no
    /// runtime identity, so those sites must see the base name to dispatch
    /// correctly (phase-9 step 5a). A no-op for every non-refinement name.
    pub(super) fn refinement_base_name(&self, name: &str) -> String {
        let mut cur = name.to_string();
        while let Some(base) = self.refinement_bases.get(&cur) {
            let next = Self::mangled_type_name(base);
            if next == cur {
                break;
            }
            cur = next;
        }
        cur
    }

    /// Record a binding's type name in `var_type_names`, normalizing a
    /// refinement to its base name first (see `refinement_base_name`). All
    /// `var_type_names` writes route through here so a refinement-typed
    /// binding dispatches as its base everywhere downstream.
    pub(super) fn record_var_type_name(&mut self, var: String, ty_name: String) {
        let normalized = self.refinement_base_name(&ty_name);
        // DataFrame is non-generic, so its bindings don't flow through a
        // typed-exprs side-table the way Column/Tensor do — record
        // membership here (every path that names a binding goes through
        // this fn) so method dispatch + the `FreeDataFrame` tracker
        // recognise it (`src/codegen/dataframe.rs`).
        if normalized == "DataFrame" {
            self.dataframe_var_infos.insert(var.clone());
        }
        self.var_type_names.insert(var, normalized);
    }

    /// Extract (K, V) LLVM types from a `Map[K, V]` type expression.
    pub(super) fn extract_map_kv_types(
        &self,
        te: &TypeExpr,
    ) -> Option<(BasicTypeEnum<'ctx>, BasicTypeEnum<'ctx>)> {
        if let TypeKind::Path(path) = &te.kind {
            if path.segments.first().map(|s| s.as_str()) != Some("Map") {
                return None;
            }
            let args = path.generic_args.as_ref()?;
            if args.len() != 2 {
                return None;
            }
            let k = match &args[0] {
                GenericArg::Type(t) => self.llvm_type_for_type_expr(t),
                _ => return None,
            };
            let v = match &args[1] {
                GenericArg::Type(t) => self.llvm_type_for_type_expr(t),
                _ => return None,
            };
            Some((k, v))
        } else {
            None
        }
    }

    /// Extract the element LLVM type from a `Set[T]` type expression.
    /// Mirror of `extract_map_kv_types` for the single-type-parameter Set.
    pub(super) fn extract_set_elem_type(&self, te: &TypeExpr) -> Option<BasicTypeEnum<'ctx>> {
        if let TypeKind::Path(path) = &te.kind {
            if path.segments.first().map(|s| s.as_str()) != Some("Set") {
                return None;
            }
            let args = path.generic_args.as_ref()?;
            if args.is_empty() {
                return None;
            }
            if let GenericArg::Type(t) = &args[0] {
                return Some(self.llvm_type_for_type_expr(t));
            }
        }
        None
    }

    /// Extract the element shallow type-name (e.g. `"i64"`, `"String"`) from
    /// a `Set[T]` type expression. Used to drive hash/eq fn selection.
    /// Mirror of `extract_map_key_name`.
    pub(super) fn extract_set_elem_name(te: &TypeExpr) -> Option<String> {
        if let TypeKind::Path(path) = &te.kind {
            if path.segments.first().map(|s| s.as_str()) != Some("Set") {
                return None;
            }
            let args = path.generic_args.as_ref()?;
            if let Some(GenericArg::Type(elem_te)) = args.first() {
                return Some(Self::mangled_type_name(elem_te));
            }
        }
        None
    }

    /// Register a variable's collection-type metadata in the side-tables
    /// driven off a Kāra `TypeExpr`. Mirrors the let-statement site at
    /// `compile_stmt(StmtKind::Let)` so for-loop bindings can inherit the
    /// same registrations from the source's stored element `TypeExpr`.
    ///
    /// Populates whichever subset of `vec_elem_types` / `slice_elem_types` /
    /// `map_key_types` / `map_val_types` / `map_key_type_names` /
    /// `var_elem_type_exprs` / `map_key_type_exprs` / `set_elem_types` /
    /// `set_elem_type_names` / `set_elem_type_exprs` matches the `TypeExpr`
    /// shape; primitives (and other shapes we don't track) are no-ops.
    /// If `te` names a refinement alias (`type NonEmpty[T] = Vec[T] where …`),
    /// resolve it to its *instantiated* base `TypeExpr` — substituting the
    /// alias's generic params with the use-site's generic args
    /// (`NonEmpty[EnrichedRow]` → `Vec[EnrichedRow]`). A refinement carries no
    /// runtime identity, so a binding must register against its base for
    /// collection method dispatch (`.len()`, `for x in`, indexing) and field
    /// access to see the real `Vec`/`String`/`Map`/struct. Returns `None` for
    /// every non-refinement type; callers recurse to peel nested aliases.
    pub(super) fn resolve_refinement_alias_te(&self, te: &TypeExpr) -> Option<TypeExpr> {
        let TypeKind::Path(path) = &te.kind else {
            return None;
        };
        let name = path.segments.first()?;
        let base = self.refinement_bases.get(name)?.clone();
        // Build the param→arg substitution from the alias's declared param
        // names (parallel `refinement_generic_params`) zipped against the
        // use-site type args. Non-`Type` args (const/shape) and arity
        // mismatches simply don't contribute — the base is returned with as
        // much substituted as is well-formed.
        let mut subst = std::collections::HashMap::new();
        if let (Some(params), Some(args)) = (
            self.refinement_generic_params.get(name),
            path.generic_args.as_ref(),
        ) {
            for (pname, arg) in params.iter().zip(args.iter()) {
                if let GenericArg::Type(t) = arg {
                    subst.insert(pname.clone(), t.clone());
                }
            }
        }
        Some(Self::subst_type_params(&base, &subst))
    }

    pub(super) fn register_var_from_type_expr(&mut self, var_name: &str, te: &TypeExpr) {
        // Refinement alias: register against the instantiated base type so the
        // binding dispatches as its real `Vec`/`String`/struct everywhere. The
        // recursion peels nested aliases (`type A = B`, `type B = Vec[T]`).
        if let Some(base) = self.resolve_refinement_alias_te(te) {
            self.register_var_from_type_expr(var_name, &base);
            return;
        }
        // Atomic[T] — transparent wrapper type registered with
        // `var_type_names = "Atomic"` so downstream `.load(ord)` /
        // `.store(v, ord)` method-call dispatch (the Atomic arm in
        // `compile_method_call`) recognises the receiver. Critical for
        // the FieldAccess path: `try_compile_field_receiver_method`
        // synthesises a binding for `c.atomic_field` and routes it
        // back through `register_var_from_type_expr` with the field's
        // TypeExpr (`Atomic[T]`); without this arm the synth binding
        // would miss `var_type_names` and fall through to the user
        // impl-block lookup, which errors. Baked stdlib's empty
        // `struct Atomic[T] { }` shape isn't in `struct_types`, so the
        // user-type fallback at the bottom of this fn doesn't catch
        // it either — Atomic needs an explicit arm. Returns early.
        if let TypeKind::Path(path) = &te.kind {
            if path.segments.last().map(|s| s.as_str()) == Some("Atomic") {
                self.var_type_names
                    .insert(var_name.to_string(), "Atomic".to_string());
                // Also track inner-is-bool for the synth-binding case —
                // `try_compile_field_receiver_method` mints a synth
                // `__field_elem_N` from `c.atomic_bool_field.method()`
                // and routes it back through this fn with the field's
                // `Atomic[bool]` TypeExpr. Without this, `compile_atomic_method`'s
                // Identifier-path bool detection would miss the synth
                // and emit the wrong-width store/load.
                if is_atomic_bool_type_expr(te) {
                    self.atomic_var_inner_is_bool.insert(var_name.to_string());
                }
                return;
            }
        }
        // Tensor[T, Shape] — register the element type + static dims so
        // indexing / method dispatch and the cleanup tracker recognise
        // the binding (`src/codegen/tensor.rs`). Splice-bearing shapes
        // return None from the extractor and deliberately skip
        // registration: rank unknown, and the only ops the typechecker
        // admits on them (shape()/rank()) dispatch via the side-table /
        // method path without needing a per-var registration.
        if let Some(info) = self.tensor_var_info_from_type_expr(te) {
            self.tensor_var_infos.insert(var_name.to_string(), info);
            return;
        }
        // Column[T] — register the element LLVM type so indexing
        // (`c[i] -> Option[T]`), method dispatch, and the cleanup tracker
        // recognise the binding (`src/codegen/column.rs`).
        if let Some(info) = self.column_var_info_from_type_expr(te) {
            self.column_var_infos.insert(var_name.to_string(), info);
            return;
        }
        // DataFrame — non-generic, so just record membership so method
        // dispatch + the `FreeDataFrame` cleanup tracker recognise the
        // binding (`src/codegen/dataframe.rs`).
        if let crate::ast::TypeKind::Path(p) = &te.kind {
            if p.segments.last().map(|s| s.as_str()) == Some("DataFrame") {
                self.dataframe_var_infos.insert(var_name.to_string());
                return;
            }
        }
        if let Some(elem_ty) = self.extract_vec_elem_type(te) {
            self.vec_elem_types.insert(var_name.to_string(), elem_ty);
            if let Some(inner) = vec_inner_type_expr(te) {
                self.var_elem_type_exprs.insert(var_name.to_string(), inner);
            }
            return;
        }
        if self.is_string_type_expr(te) {
            self.vec_elem_types
                .insert(var_name.to_string(), self.context.i8_type().into());
            self.string_vars.insert(var_name.to_string());
            return;
        }
        // `StringSlice` — a borrowed view sharing String's `{ptr,len,cap}`
        // layout with `cap == 0`. Register it as a string-like var so its
        // read-methods (`len`/`to_string`/`slice`/`find`/…) route through
        // `compile_vec_method`, but deliberately NOT via `is_string_type_expr`
        // (whose other consumers drive owned-String drop / defensive-copy /
        // move-suppression — a borrow needs none of that). The `cap == 0`
        // borrow means any scope-exit free that fires is `cap > 0`-guarded to
        // a no-op, so no buffer is freed for the view (design.md § StringSlice).
        if Self::is_string_slice_type_expr(te) {
            self.vec_elem_types
                .insert(var_name.to_string(), self.context.i8_type().into());
            self.string_vars.insert(var_name.to_string());
            return;
        }
        if let Some(elem_ty) = self.extract_slice_elem_type(te) {
            self.slice_elem_types.insert(var_name.to_string(), elem_ty);
            if let Some(inner) = slice_inner_type_expr(te) {
                self.var_elem_type_exprs.insert(var_name.to_string(), inner);
            }
            return;
        }
        if let Some((k_ty, v_ty)) = self.extract_map_kv_types(te) {
            self.map_key_types.insert(var_name.to_string(), k_ty);
            self.map_val_types.insert(var_name.to_string(), v_ty);
            if let Some(k_name) = Self::extract_map_key_name(te) {
                self.map_key_type_names.insert(var_name.to_string(), k_name);
            }
            if let Some((k_te, v_te)) = map_kv_type_exprs(te) {
                self.map_key_type_exprs.insert(var_name.to_string(), k_te);
                self.var_elem_type_exprs.insert(var_name.to_string(), v_te);
            }
            return;
        }
        if let Some(elem_ty) = self.extract_set_elem_type(te) {
            self.set_elem_types.insert(var_name.to_string(), elem_ty);
            if let Some(elem_name) = Self::extract_set_elem_name(te) {
                self.set_elem_type_names
                    .insert(var_name.to_string(), elem_name);
            }
            if let Some(elem_te) = set_inner_type_expr(te) {
                self.set_elem_type_exprs
                    .insert(var_name.to_string(), elem_te);
            }
            return;
        }
        // Bare user-type names (struct / shared struct / enum) — register
        // `var_type_names` so `field_index_for` / `shared_type_for_expr`
        // can resolve `.field` accesses on the binding. Without this, a
        // for-loop iterating `Vec[Node] / Vec[N]` binds `x` into the
        // value/pointer slot but the type is invisible to downstream
        // field-access codegen, which then silently returns `i64 0` for
        // every `x.field` read (cond_simple bug, 2026-05-16). Mirrors
        // `compile_stmt(StmtKind::Let)`'s `type_hint` registration path
        // (stmts.rs ~line 730) so for-loop bindings see the same
        // type-name visibility as a `let x: Foo = ...` binding.
        if let TypeKind::Path(path) = &te.kind {
            if let Some(seg) = path.segments.last().cloned() {
                // Scalar integer primitives are recorded too — without this, a
                // `for b in vec_u8` loop var (or a destructured `u8`/`u16`/…
                // element) has no recorded type name, so `expr_is_unsigned_int`
                // can't pick unsigned formatting and `to_string`/print/coercion
                // sign-extends (255u8 → -1). The `let b: u8 = …` path already
                // records this (stmts.rs); this brings for-loop / destructured
                // element bindings to parity. Surfaced by the self-hosted lexer's
                // c-string byte render of multi-byte `\u{…}` escapes (2026-06-12):
                // `for b in cs.bytes { …b.to_string() }` rendered 195 as -61.
                let is_int_prim = matches!(
                    seg.as_str(),
                    "u8" | "u16"
                        | "u32"
                        | "u64"
                        | "u128"
                        | "usize"
                        | "i8"
                        | "i16"
                        | "i32"
                        | "i64"
                        | "i128"
                        | "isize"
                );
                if self.struct_types.contains_key(seg.as_str())
                    || self.shared_types.contains_key(seg.as_str())
                    || self.enum_layouts.contains_key(seg.as_str())
                    || is_int_prim
                {
                    self.record_var_type_name(var_name.to_string(), seg);
                }
            }
        }
    }

    /// Register collection side-tables for the bindings produced by a
    /// for-loop's destructuring pattern, using the source variable's
    /// stored element `TypeExpr`. Without this, `for s in vec_of_strings`
    /// binds `s` only in `self.variables` — method dispatch in
    /// `compile_expr_method_call` then misses the Vec/Slice/Map side-table
    /// check and falls through to the silent-`0` default.
    pub(super) fn register_for_loop_bindings(&mut self, pattern: &Pattern, source_var: &str) {
        match &pattern.kind {
            PatternKind::Binding(name) => {
                if let Some(elem_te) = self.var_elem_type_exprs.get(source_var).cloned() {
                    self.register_var_from_type_expr(name, &elem_te);
                    self.mark_for_loop_borrow_if_heap(name);
                }
            }
            // `for (k, v) in m` — only legal tuple iteration shape today
            // (Map). `for (a, b) in vec_of_tuples` would fall through; the
            // tuple-element-classification follow-up would extend this arm.
            PatternKind::Tuple(pats) if pats.len() == 2 => {
                if let PatternKind::Binding(k_name) = &pats[0].kind {
                    if let Some(k_te) = self.map_key_type_exprs.get(source_var).cloned() {
                        self.register_var_from_type_expr(k_name, &k_te);
                        self.mark_for_loop_borrow_if_heap(k_name);
                    }
                }
                if let PatternKind::Binding(v_name) = &pats[1].kind {
                    if let Some(v_te) = self.var_elem_type_exprs.get(source_var).cloned() {
                        self.register_var_from_type_expr(v_name, &v_te);
                        self.mark_for_loop_borrow_if_heap(v_name);
                    }
                }
            }
            _ => {}
        }
    }

    /// Mark a `for`-loop element binding as a heap borrow needing a defensive
    /// copy at retaining-consume sites (see `for_loop_borrow_vars`). Only
    /// String / Vec (`{ptr,len,cap}`) elements qualify — scalars carry no
    /// buffer to alias, so consuming them is a plain bit-copy.
    pub(super) fn mark_for_loop_borrow_if_heap(&mut self, name: &str) {
        if self.vec_elem_types.contains_key(name) {
            self.for_loop_borrow_vars.insert(name.to_string());
        }
    }

    pub(super) fn is_map_new_call(&self, expr: &Expr) -> bool {
        if let ExprKind::Call { callee, .. } = &expr.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                return segments.len() == 2 && segments[0] == "Map" && segments[1] == "new";
            }
        }
        false
    }

    pub(super) fn is_set_new_call(&self, expr: &Expr) -> bool {
        if let ExprKind::Call { callee, .. } = &expr.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                return segments.len() == 2 && segments[0] == "Set" && segments[1] == "new";
            }
        }
        false
    }

    pub(super) fn is_vec_new_call(&self, expr: &Expr) -> bool {
        if let ExprKind::Call { callee, .. } = &expr.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                return segments.len() == 2 && segments[0] == "Vec" && segments[1] == "new";
            }
        }
        false
    }

    /// Recognise `Vec.with_capacity(n)` — the capacity-presized form of
    /// `Vec.new()`. The `presize` lowering pass rewrites `let mut v = Vec.new()`
    /// into this when `v` is filled by a simple counted loop (a single
    /// unconditional `push` whose trip count is known), so a SoA-laid-out buffer
    /// built that way (`init_grid`/`fan_collide`/`fan_stream`'s
    /// `while … { v.push(..) }`) reaches the let arm as `with_capacity`, not
    /// `new`. The SoA let path treats the two identically (the capacity is a
    /// hint the lazily-grown SoA groups ignore); without recognising it here, a
    /// SoA binding initialised in a counted loop fell through to the AoS
    /// `{ptr,len,cap}` path while its declared/inferred layout was the 4-field
    /// SoA struct — an LLVM return-type / use-site mismatch.
    pub(super) fn is_vec_with_capacity_call(&self, expr: &Expr) -> bool {
        if let ExprKind::Call { callee, .. } = &expr.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                return segments.len() == 2
                    && segments[0] == "Vec"
                    && segments[1] == "with_capacity";
            }
        }
        false
    }

    /// Recognise `Atomic.new(v)` — the constructor for the transparent
    /// `Atomic[T]` wrapper. Used by the let-binding registration in
    /// `compile_stmt(Let)` to set `var_type_names[a] = "Atomic"` so
    /// subsequent `a.load(ord)` / `a.store(v, ord)` dispatches route
    /// through the Atomic arm in `compile_method_call` rather than the
    /// user impl-block lookup (which would fail — `Atomic.load` /
    /// `.store` aren't user-defined methods).
    pub(super) fn is_atomic_new_call(&self, expr: &Expr) -> bool {
        if let ExprKind::Call { callee, .. } = &expr.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                return segments.len() == 2 && segments[0] == "Atomic" && segments[1] == "new";
            }
        }
        false
    }

    /// Debugger Contract slice 5: `Runtime.list_par_blocks()` /
    /// `Runtime.list_tasks()` return `Vec[ParBlockInfo]` /
    /// `Vec[TaskInfo]`. Used by the let-binding registration to set up
    /// `vec_elem_types` so subsequent `.len()` / `.is_empty()` dispatches
    /// through `compile_vec_method`.
    pub(super) fn is_runtime_introspection_call(&self, expr: &Expr) -> bool {
        if let ExprKind::Call { callee, .. } = &expr.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                return segments.len() == 2
                    && segments[0] == "Runtime"
                    && (segments[1] == "list_par_blocks" || segments[1] == "list_tasks");
            }
        }
        false
    }

    pub(super) fn is_string_binary_op(&self, expr: &Expr) -> bool {
        // Source-form `a + b` (pre-lowering).
        if let ExprKind::Binary {
            op: BinOp::Add,
            left,
            ..
        } = &expr.kind
        {
            return self.first_operand_is_string(left);
        }
        // Lowered form `Call(Path(["String", "add"]), [a, b])` — produced by
        // the operator lowering pass. Also recognize String + String here.
        if let ExprKind::Call { callee, args } = &expr.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                if segments.len() == 2 && segments[0] == "String" && segments[1] == "add" {
                    if let Some(first) = args.first() {
                        return self.first_operand_is_string(&first.value);
                    }
                }
            }
        }
        false
    }

    /// Helper: is this expression a string literal or a known string variable?
    pub(super) fn first_operand_is_string(&self, expr: &Expr) -> bool {
        if matches!(&expr.kind, ExprKind::StringLit(_)) {
            return true;
        }
        if let ExprKind::Identifier(name) = &expr.kind {
            return self
                .vec_elem_types
                .get(name.as_str())
                .map(|t| t.is_int_type() && t.into_int_type().get_bit_width() == 8)
                .unwrap_or(false);
        }
        false
    }

    pub(super) fn is_string_new_call(&self, expr: &Expr) -> bool {
        if let ExprKind::Call { callee, .. } = &expr.kind {
            if let ExprKind::Path { segments, .. } = &callee.kind {
                return segments.len() == 2 && segments[0] == "String" && segments[1] == "new";
            }
        }
        false
    }

    /// Look up the Vec element type for a variable, defaulting to i64.
    pub(super) fn vec_elem_type_for_var(&self, name: &str) -> BasicTypeEnum<'ctx> {
        self.vec_elem_types
            .get(name)
            .copied()
            .unwrap_or_else(|| self.context.i64_type().into())
    }

    pub(super) fn llvm_type_for_name(&self, name: &str) -> BasicTypeEnum<'ctx> {
        // Active monomorphization substitution takes priority.
        if let Some(&ty) = self.type_subst.get(name) {
            return ty;
        }
        // Refinement alias → base layout (phase-9 step 4). See the parallel
        // arm in `llvm_type_for_type_expr`; this name-level path is hit when
        // a refinement is referenced by bare name (e.g. a recorded
        // `var_type_names` entry or a struct-field type name).
        if let Some(base) = self.refinement_bases.get(name) {
            return self.llvm_type_for_type_expr(&base.clone());
        }
        // Distinct type → base layout. The name-level path is hit when a
        // distinct type is referenced by bare name (a recorded
        // `var_type_names` entry or a struct-field type name).
        if let Some(base) = self.distinct_bases.get(name) {
            return self.llvm_type_for_type_expr(&base.clone());
        }
        match name {
            "i8" | "u8" => self.context.i8_type().into(),
            "i16" | "u16" => self.context.i16_type().into(),
            "i32" | "u32" => self.context.i32_type().into(),
            "i64" | "u64" | "isize" | "usize" => self.context.i64_type().into(),
            "f32" => self.context.f32_type().into(),
            "f64" => self.context.f64_type().into(),
            "bool" => self.context.bool_type().into(),
            // `char` is a Unicode scalar value — 32 bits, same LLVM
            // type as `i32` / `u32`. Without this arm, `char` falls
            // through to the i64 default below, which caused
            // `Map[char, V].new()` to allocate `key_size = 8` byte
            // slots for 4-byte char keys; the runtime would then
            // copy 4 bytes of stack-neighbor garbage alongside
            // every char into the kv table. Hash/eq only read the
            // first 4 bytes so it worked accidentally as long as
            // the garbage was consistent between insert and get
            // sites — Slice 1b.3 sidestepped that fragility with a
            // forced alloca-order in the get arm; this fix
            // addresses the root cause. Surfaced by the
            // monomorphized-collections Slice 2 investigation.
            "char" => self.context.i32_type().into(),
            // `VecDeque` shares `Vec[T]`'s `{ptr, len, cap}` layout (see
            // the parallel arm in `llvm_type_for_type_expr`). The baked
            // `struct VecDeque[T] { }` isn't in `struct_types` from
            // codegen's perspective (baked stdlib items aren't fed in),
            // so calls that reach `llvm_type_for_name` directly without
            // generic args would otherwise fall to the i64 default.
            // `StringSlice` is a borrowed view over a `String`'s UTF-8 bytes
            // and shares its `{ptr, len, cap}` layout — represented with
            // `cap == 0` (a non-owning borrow, so scope-exit drop's `cap > 0`
            // guard no-ops). design.md § StringSlice. Reusing the String shape
            // lets every String read-method lower unchanged on a StringSlice.
            "String" | "str" | "StringSlice" | "VecDeque" | "Vec" => self.vec_struct_type().into(),
            // Slice 8z: `pattern_binding_types` records the canonical
            // `"Slice"` surface name for `Type::Slice` parameters /
            // bindings (typechecker `record_pattern_inner_type` arm). The
            // matching name-level lookup must return the slice struct
            // shape (`{ptr, i64}`) so state-struct layout emission sizes
            // the field correctly; without this arm, the field landed at
            // the i64 default (8 bytes) while the actual store wrote a
            // 16-byte slice header, producing a size mismatch the LLVM
            // verifier accepted under opaque pointers but that semantics-
            // wise overflowed the field. Mirrors `llvm_type_for_type_expr`
            // line 67-69's identical `TypeKind::Path("Slice")` arm.
            "Slice" => self.slice_struct_type().into(),
            // Phase 6 line 17 — baked-stdlib single-i64-field network
            // structs. All three (`TcpListener`, `TcpStream`, `WebSocket`)
            // share the same `{ fd: i64 }` layout per their declarations
            // in `runtime/stdlib/tcp.kara` + `ws.kara` (i64 since the
            // Windows-IOCP-prep fd ABI widening — a Windows `SOCKET` is
            // pointer-sized; Unix `RawFd` is i32 in the low half). Codegen does not
            // load baked stdlib struct definitions into `self.struct_types`
            // (the value-site lowerings hand-roll the struct via
            // `context.struct_type(&[i32_type], false)` — see
            // `lower_tcp_listener_bind` / `lower_websocket_accept` /
            // etc.). Without these name arms, a user fn taking one of
            // these types by value (`fn handle(ws: WebSocket)`) would
            // get an `i64` LLVM parameter signature from the fall-through
            // default below, while the call site loads `{ i32 }` from
            // the arg's slot — the LLVM verifier rejects with `Call
            // parameter type does not match function signature`. Surfaced
            // by Demo 1 (line 170) slice 1's accept-loop
            // `tg.spawn(|| handle(ws))` pattern; same root cause for any
            // direct by-value pass. Mirrors the `String` / `Vec` /
            // `Slice` baked-stdlib arms above.
            "TcpListener" | "TcpStream" | "WebSocket" => self
                .context
                .struct_type(&[self.context.i64_type().into()], false)
                .into(),
            // Phase 6 line 236 slice 2 — TLS baked-stdlib structs.
            // `TlsListener` carries `{ fd: i64, config: *mut TlsConfig }`
            // (the slice-1 FFI's `karac_runtime_tls_config_new` returns
            // an opaque pointer that the listener struct keeps for
            // forwarding to each `karac_runtime_tls_accept` call).
            // `TlsStream` is `{ fd: i64 }` — identical to `TcpStream`,
            // since the TLS session state lives in the runtime-side
            // `SESSIONS` registry keyed by fd. Same rationale as the
            // TCP arm above: by-value param sites would otherwise hit
            // the i64 fall-through default.
            "TlsListener" => self
                .context
                .struct_type(
                    &[
                        self.context.i64_type().into(),
                        self.context.ptr_type(AddressSpace::default()).into(),
                    ],
                    false,
                )
                .into(),
            "TlsStream" => self
                .context
                .struct_type(&[self.context.i64_type().into()], false)
                .into(),
            // Phase 6 line 218 — baked-stdlib concurrency handles.
            // `TaskGroup { id: i64 }` (`runtime/stdlib/task_group.kara`)
            // and `TaskHandle[T] { task_id: i64 }` both lower to the
            // hand-rolled `{ i64 }` value shape at their construction
            // sites (`TaskGroup.new()` in `assoc_call.rs`, the spawn
            // wrap in `task_group.rs`). Like the TCP/TLS structs above,
            // codegen never loads these baked stdlib defs into
            // `struct_types`, so a type-annotation-driven type lookup
            // (`let g: TaskGroup = ...`) would otherwise hit the `i64`
            // fall-through default below. That mis-sizes any slot built
            // from the annotation rather than the value — e.g.
            // auto-parallelization's return-slot inference
            // (`infer_let_binding_llvm_type`) sizes the escaped `g`
            // binding at `i64`, then `tg.spawn(...)`'s receiver load
            // reads a bare `i64` where the dispatcher expects the
            // `{ i64 }` struct and panics (`into_struct_value` on an
            // `IntValue`). `TaskHandle`'s `T` only governs the `.join()`
            // return type (recovered separately); the handle value is
            // always `{ i64 }` regardless. Same family as B-2026-06-07-2
            // (struct-returned-by-value ABI fall-through).
            "TaskGroup" | "TaskHandle" => self
                .context
                .struct_type(&[self.context.i64_type().into()], false)
                .into(),
            name => {
                // Shared types are heap-allocated pointers.
                if self.shared_types.contains_key(name) {
                    return self.context.ptr_type(AddressSpace::default()).into();
                }
                if let Some(st) = self.struct_types.get(name) {
                    (*st).into()
                } else if let Some(ut) = self.union_types.get(name) {
                    // FFI unions are encoded as a storage struct whose
                    // ABI size / alignment match the union's max-field
                    // shape (phase 5 line 569 slice 4). Returning the
                    // storage type here is what makes `size_of[Foo]` /
                    // `align_of[Foo]` report the correct numbers for
                    // free, and lets bindings declared as `let u: Foo`
                    // alloca the right number of bytes.
                    (*ut).into()
                } else if let Some(layout) = self.enum_layouts.get(name) {
                    // Enum types are represented as tagged-union structs.
                    layout.llvm_type.into()
                } else {
                    self.context.i64_type().into()
                }
            }
        }
    }

    pub(super) fn llvm_return_type(&self, ty: &Option<TypeExpr>) -> Option<BasicTypeEnum<'ctx>> {
        let te = ty.as_ref()?;
        match &te.kind {
            TypeKind::Path(path) => {
                let name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
                if name.is_empty() {
                    return None;
                }
                // Delegate to llvm_type_for_type_expr so generic types
                // (Array[T, N], Vec[T], Slice[T], Map[K,V], …) are honored
                // — bare llvm_type_for_name drops generic args.
                Some(self.llvm_type_for_type_expr(te))
            }
            TypeKind::Tuple(elems) if elems.is_empty() => None,
            // A `-> ref BorrowedStruct` return (a struct with `ref` fields,
            // design.md Feature 4 Part 3) is returned BY VALUE — the `ref`
            // marks the struct's borrow scope, not pointer indirection. The
            // struct itself is a value whose `ref` fields are embedded
            // pointers; returning it copies that value (the source still owns
            // the borrowed storage). This differs from `-> ref String`, where
            // `ref` means "return a pointer". Lower to the inner struct type
            // so the body's normal value-return path matches the signature.
            // `current_fn_returns_ref` / `fn_ref_return_inner` are likewise
            // suppressed for these (see `declare_one_function` /
            // `compile_function`), keeping the pointer-borrow ABI out of the
            // borrowed-struct path entirely (B-2026-06-07-5).
            TypeKind::Ref(inner) | TypeKind::MutRef(inner)
                if self.ref_return_is_value_abi(inner) =>
            {
                Some(self.llvm_type_for_type_expr(inner))
            }
            _ => Some(self.llvm_type_for_type_expr(te)),
        }
    }

    /// Does a `-> ref T` / `-> mut ref T` return use the BY-VALUE ABI
    /// (return the value itself, no pointer indirection / caller-side
    /// load) rather than the pointer-borrow ABI? True for two value types
    /// whose representation already carries the borrow:
    /// - a **borrowed struct** (a struct value with embedded `ref` fields);
    /// - a **tensor** (`Tensor[T, Shape]`), whose value is a single heap
    ///   `ptr` to the `[rank][dims][data]` block — `ref Tensor` borrows the
    ///   same block with no extra indirection, so it is returned by value
    ///   (the pointer) and the caller uses it directly. Shape-independent:
    ///   concrete / `?` / splice shapes all qualify.
    ///
    /// Suppressing the pointer-borrow ABI here keeps `fn_ref_return_inner`
    /// unset and `current_fn_returns_ref` false for these returns (see
    /// `declare_one_function` / `compile_function`), so the body's normal
    /// value-return path matches the signature and the caller does no
    /// extra load (which for a tensor would dereference the rank word as a
    /// pointer — the phase-11 `ref Tensor` AOT trap).
    pub(super) fn ref_return_is_value_abi(&self, inner: &TypeExpr) -> bool {
        if self.type_expr_is_borrowed_struct(inner) {
            return true;
        }
        matches!(
            &inner.kind,
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Tensor")
        )
    }

    /// True when a function's declared return type is `ref T` / `mut ref T`
    /// whose inner `T` uses the by-value ref-return ABI (`ref_return_is_value_abi`).
    pub(super) fn return_type_ref_is_value_abi(&self, ret: &Option<TypeExpr>) -> bool {
        matches!(
            ret.as_ref().map(|t| &t.kind),
            Some(TypeKind::Ref(inner) | TypeKind::MutRef(inner))
                if self.ref_return_is_value_abi(inner)
        )
    }

    /// Does this type expression name a *borrowed struct* — a user struct
    /// with at least one `ref` / `mut ref` field (design.md Feature 4 Part
    /// 3)? Such a struct is a value type whose lifetime is bounded by its
    /// borrowed fields; `-> ref BorrowedStruct` returns it by value rather
    /// than through the pointer-borrow ABI. Reads the per-struct field
    /// TypeExprs `declare_structs` populated, so it is only accurate after
    /// that pass (which runs before any function declaration).
    pub(super) fn type_expr_is_borrowed_struct(&self, te: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &te.kind else {
            return false;
        };
        let Some(name) = p.segments.first() else {
            return false;
        };
        self.struct_field_type_exprs
            .get(name)
            .is_some_and(|fields| {
                fields
                    .iter()
                    .any(|f| matches!(f.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
            })
    }

    pub(super) fn llvm_param_type(&self, param: &Param) -> BasicMetadataTypeEnum<'ctx> {
        BasicMetadataTypeEnum::from(self.llvm_type_for_type_expr(&param.ty))
    }

    // ── Shared type helpers ─────────────────────────────────────────

    /// Check if a type name refers to a shared (RC) type.
    #[allow(dead_code)]
    pub(super) fn is_shared_type(&self, name: &str) -> bool {
        self.shared_types.contains_key(name)
    }

    /// Get the heap struct type for a shared type, if it exists.
    #[allow(dead_code)]
    pub(super) fn shared_heap_type(&self, name: &str) -> Option<StructType<'ctx>> {
        self.shared_types.get(name).map(|info| info.heap_type)
    }

    /// If the expression refers to a variable of shared type, return the type name and info.
    pub(super) fn shared_type_for_expr(
        &self,
        expr: &Expr,
    ) -> Option<(String, SharedTypeInfo<'ctx>)> {
        // The receiver name. `self` parses as `ExprKind::SelfValue`, not
        // `Identifier("self")`, so it's resolved as the bound local `"self"` —
        // exactly like a plain identifier receiver. Both a `ref self` shared
        // method and the constructor invariant binding store the heap pointer in
        // the `self` alloca, and the shared field-access path loads it with a
        // single `build_load` (the read path mirrors `compile_field_store`'s
        // shared branch), so no constructor-vs-method gate is needed here.
        let var_name: Option<&str> = match &expr.kind {
            ExprKind::Identifier(name) => Some(name.as_str()),
            ExprKind::SelfValue => Some("self"),
            _ => None,
        };
        if let Some(var_name) = var_name {
            if let Some(type_name) = self.var_type_names.get(var_name) {
                if let Some(info) = self.shared_types.get(type_name.as_str()) {
                    return Some((type_name.clone(), info.clone()));
                }
            }
        }
        // A chained field access whose *intermediate* field is shared:
        // `h.a.v` where `Holder { a: Leaf }` and `Leaf` is `shared`. The
        // inline `a` field lowers to the 8-byte RC pointer, so reading `.v`
        // through it must load that pointer and GEP into the heap payload —
        // exactly the shared GEP-deref the identifier/`self` arms enable.
        // Without this, the chained read fell to `compile_field_access`'s
        // generic struct-value path: `compile_expr(h.a)` yields a
        // `PointerValue` (the extracted inline RC pointer), the
        // `StructValue` guard misses, and the access returned the const-0
        // placeholder (silent wrong value). `compile_expr(object)` already
        // produces that inline RC pointer, which is precisely the `ptr`
        // the shared branch loads. Recover the field's surface type name
        // via `type_name_of_expr` (which resolves a `FieldAccess` chain
        // through `struct_field_type_names`) and look it up in
        // `shared_types`.
        if let ExprKind::FieldAccess { .. } = &expr.kind {
            if let Some(type_name) = self.type_name_of_expr(expr) {
                if let Some(info) = self.shared_types.get(type_name.as_str()) {
                    return Some((type_name, info.clone()));
                }
            }
        }
        None
    }

    /// If the expression is a call-shaped node (`Call`, `MethodCall`,
    /// or 2-segment `Path` assoc-call) whose static return type names a
    /// known shared struct / enum, return that type name and info.
    /// Companion to `shared_type_for_expr` which only handles variable
    /// references. Used by `compile_field_access` for the call-chain
    /// shape `helper().field` — see bug #8 (call-chain field access on
    /// shared-struct return).
    pub(super) fn shared_type_for_call_like(
        &self,
        expr: &Expr,
    ) -> Option<(String, SharedTypeInfo<'ctx>)> {
        // Fast path for the `m.get(k).unwrap()` / `.expect()` chain
        // shape: the outer call is `unwrap` / `expect` on an
        // `Option`/`Result` whose receiver is itself a MethodCall
        // (Map.get, Vec.first, VecDeque.pop_front, etc.). The
        // standard `Type.method` → `fn_return_type_names` lookup
        // doesn't apply because the inner call's return type
        // depends on the receiver's V parameter, not on a fixed
        // function name. But the typechecker already recorded the
        // resolved inner T in `method_unwrap_inner_types[span]` —
        // use that to recover the shared struct info when T is
        // shared.
        //
        // Without this path, FieldAccess like `m.get(k).unwrap().val`
        // falls through to the generic non-shared field access,
        // which compiles `.val` as a literal i64 zero instead of
        // GEPing the val field on the heap struct. Surfaced while
        // writing the kata 133 regression test
        // (`asan_map_get_shared_value_in_loop_no_alias_collapse`):
        // the let-binding form `let n = m.get(k).unwrap();
        // println(n.val)` worked because `n` is an Identifier and
        // hits `shared_type_for_expr`'s arm, but the inline chain
        // `println(m.get(k).unwrap().val)` produced zeros.
        if let ExprKind::MethodCall { method, .. } = &expr.kind {
            if method == "unwrap" || method == "expect" {
                let key = (expr.span.offset, expr.span.length);
                if let Some(te) = self.method_unwrap_inner_types.get(&key) {
                    if let TypeKind::Path(p) = &te.kind {
                        if let Some(seg) = p.segments.last() {
                            if let Some(info) = self.shared_types.get(seg.as_str()).cloned() {
                                return Some((seg.clone(), info));
                            }
                        }
                    }
                }
            }
        }
        let fn_name: String = match &expr.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Identifier(n) => n.clone(),
                ExprKind::Path { segments, .. } if segments.len() == 2 => {
                    format!("{}.{}", segments[0], segments[1])
                }
                ExprKind::Path { segments, .. } if segments.len() == 1 => segments[0].clone(),
                _ => return None,
            },
            // MethodCall receivers: synthesize the canonical `Type.method`
            // key from the receiver's static type. `declare_function`
            // already registers impl methods in `fn_return_type_names`
            // under their qualified name (`Holder.make`), so the same
            // lookup as the free-fn / 2-segment Path paths works once
            // the key is built. Identifier receivers cover the
            // `holder.make().val` minimal repro from bug #8 (item 5).
            // Method-chain receivers (`foo().make()`) and field-access
            // receivers (`obj.holder.make()`) are out of scope for this
            // slice — they need recursive receiver-type recovery; the
            // free-fn / 2-segment Path / Identifier-receiver shapes
            // cover the common case.
            ExprKind::MethodCall { object, method, .. } => match &object.kind {
                ExprKind::Identifier(name) => {
                    let recv_type = self.var_type_names.get(name)?;
                    format!("{recv_type}.{method}")
                }
                _ => return None,
            },
            _ => return None,
        };
        let type_name = self.fn_return_type_names.get(&fn_name)?.clone();
        let info = self.shared_types.get(&type_name)?.clone();
        Some((type_name, info))
    }

    /// Resolve the heap layout for a Map variable's *value* when V is a
    /// shared struct / shared enum. Returns `None` when V is anything
    /// else (primitive, Vec, String, owned struct, …). Used by
    /// `track_map_var` / cleanup-action wiring to decide whether the
    /// per-bucket rc_dec walk in `emit_map_shared_half_rc_dec_walk` needs
    /// to fire at scope exit. Source of truth is the `TypeExpr` stored
    /// in `var_elem_type_exprs[var_name]` (the value-side TypeExpr
    /// recorded at let-binding registration) — its head segment is the
    /// shared type's surface name, looked up in `shared_types`.
    pub(super) fn map_val_shared_heap_type_for(&self, var_name: &str) -> Option<StructType<'ctx>> {
        let v_te = self.var_elem_type_exprs.get(var_name)?;
        let head = match &v_te.kind {
            TypeKind::Path(p) => p.segments.first()?.as_str(),
            _ => return None,
        };
        let info = self.shared_types.get(head)?;
        Some(info.heap_type)
    }

    /// Resolve the heap layout for a Map / Set variable's *key* when
    /// K is a shared struct / shared enum. Mirrors
    /// `map_val_shared_heap_type_for` on the K side. For `Set[shared
    /// T]`, the element T occupies the key half of the underlying
    /// `Map[T, ()]` bucket — `set_elem_type_exprs` provides the
    /// TypeExpr, parallel to `map_key_type_exprs` for Maps. Returns
    /// `None` when K is anything else (primitive, Vec, String, owned
    /// struct, …); the `FreeMapHandle` cleanup then skips the
    /// key-side rc_dec walk.
    pub(super) fn map_key_shared_heap_type_for(&self, var_name: &str) -> Option<StructType<'ctx>> {
        let k_te = self
            .map_key_type_exprs
            .get(var_name)
            .or_else(|| self.set_elem_type_exprs.get(var_name))?;
        Self::shared_heap_type_for_type_expr_with(&self.shared_types, k_te)
    }

    /// Resolve the heap layout for an arbitrary `TypeExpr` whose head
    /// is a shared struct / shared enum. Returns `None` when the head
    /// isn't a `Path` (tuple, ref, fn type, …) or when the head name
    /// isn't a known shared type. Used by struct-field drop synthesis
    /// to inspect a `Map[K, sharedV]` / `Set[sharedT]` field's K / V
    /// halves without needing a per-binding side-table — the field
    /// type carries the K/V `TypeExpr`s directly.
    pub(super) fn shared_heap_type_for_type_expr(&self, te: &TypeExpr) -> Option<StructType<'ctx>> {
        Self::shared_heap_type_for_type_expr_with(&self.shared_types, te)
    }

    fn shared_heap_type_for_type_expr_with(
        shared_types: &HashMap<String, SharedTypeInfo<'ctx>>,
        te: &TypeExpr,
    ) -> Option<StructType<'ctx>> {
        let head = match &te.kind {
            TypeKind::Path(p) => p.segments.first()?.as_str(),
            _ => return None,
        };
        let info = shared_types.get(head)?;
        Some(info.heap_type)
    }

    /// If `te` is `Option[T]` and `T` is a known `shared` struct / enum,
    /// return the inner shared type's surface name plus its
    /// `SharedTypeInfo`. Returns `None` for plain shared `T` (use
    /// `shared_heap_type_for_type_expr` for that), for non-shared
    /// `Option[T]` (`Option[i64]`, `Option[String]`, …), or for any
    /// non-Path TypeExpr. Used by the let-stmt handler to recognize
    /// `let x: Option[ShareT] = …;` and queue an `RcDecOption` cleanup
    /// so the inner pointer's refcount drops on scope exit. The Option
    /// outer layout is the seeded `{tag, w0, w1, w2}` shape; the inner
    /// pointer occupies w0 (per `coerce_to_payload_words(ptr, 3)`'s
    /// primitive fast path, which `ptr_to_int`s the pointer into w0
    /// and zero-fills w1/w2).
    /// Niche-opt lookup: for a `shared struct` field, return the inner
    /// shared struct's `heap_type` iff the field is stored as a niche-
    /// optimized pointer (null = None, non-null = Some) rather than the
    /// conventional 4-i64 Option enum. Returns `None` for conventional
    /// fields. Decoupled from `option_inner_shared_type_for_type_expr`:
    /// niche eligibility is decided at `declare_structs` time against a
    /// pre-collected shared-struct name set; this getter looks up the
    /// already-stamped per-field decision plus the inner type's current
    /// `heap_type` (resolved via `shared_types`, so self-referential
    /// shapes resolve symmetrically once both names are registered).
    pub(crate) fn niche_field_inner_heap_type(
        &self,
        struct_name: &str,
        field_idx: usize,
    ) -> Option<StructType<'ctx>> {
        let outer = self.shared_types.get(struct_name)?;
        let inner_name = outer.niche_option_fields.get(field_idx)?.as_ref()?;
        Some(self.shared_types.get(inner_name)?.heap_type)
    }

    /// Niche-opt field load: given the heap pointer to a niche-optimized
    /// `Option[shared T]` slot, materialize a full 4-i64 Option struct
    /// SSA value so downstream code (pattern match, RcDecOption cleanup,
    /// chain-inc balancing) sees the same shape it sees for conventional
    /// fields. Tag is derived from null-ness of the loaded pointer; w0
    /// carries the pointer-as-i64; w1/w2 are zero.
    pub(crate) fn niche_load_option_field(
        &self,
        field_ptr: inkwell::values::PointerValue<'ctx>,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let loaded_ptr = self
            .builder
            .build_load(ptr_ty, field_ptr, &format!("{name}.niche.ptr"))
            .unwrap()
            .into_pointer_value();
        self.niche_ptr_to_option_value(loaded_ptr, name)
    }

    /// Niche-ABI unpack: materialize a full 4-i64 Option struct SSA value
    /// from a bare nullable pointer (null = `None`, non-null = `Some`).
    /// The SSA core of `niche_load_option_field` (which loads from a heap
    /// slot first); also used directly by `compile_function`'s entry
    /// unpack for niche-ABI params and `compile_call`'s result unpack for
    /// niche-ABI returns, where the pointer arrives as an SSA value
    /// rather than behind a slot.
    pub(crate) fn niche_ptr_to_option_value(
        &self,
        loaded_ptr: inkwell::values::PointerValue<'ctx>,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let option_ty = self.enum_layouts["Option"].llvm_type;
        let is_null = self
            .builder
            .build_is_null(loaded_ptr, &format!("{name}.niche.is_null"))
            .unwrap();
        let is_some = self
            .builder
            .build_not(is_null, &format!("{name}.niche.is_some"))
            .unwrap();
        let tag = self
            .builder
            .build_int_z_extend(is_some, i64_t, &format!("{name}.niche.tag"))
            .unwrap();
        let w0 = self
            .builder
            .build_ptr_to_int(loaded_ptr, i64_t, &format!("{name}.niche.w0"))
            .unwrap();
        // Start from `const_zero` so w1/w2 are already 0; only fill tag and w0.
        let mut val: inkwell::values::StructValue<'ctx> = option_ty.const_zero();
        val = self
            .builder
            .build_insert_value(val, tag, 0, &format!("{name}.niche.opt.tag"))
            .unwrap()
            .into_struct_value();
        val = self
            .builder
            .build_insert_value(val, w0, 1, &format!("{name}.niche.opt.w0"))
            .unwrap()
            .into_struct_value();
        val.into()
    }

    /// Niche-opt field store: convert a 4-i64 Option struct SSA value
    /// into the single pointer the niche slot expects. Used by callers
    /// that have already handled refcount bookkeeping (old-side dec,
    /// new-side inc) — this helper does the byte-level write only.
    ///
    /// Tag-aware: when `tag == None`, stores `null` regardless of `w0`.
    /// `None` values are built as `get_undef()` + tag store (see
    /// `try_compile_enum_variant`'s non-shared branch) so `w0`/`w1`/`w2`
    /// are LLVM `undef` for None. The conventional path doesn't care
    /// (consumers gate on tag), but a niche store that copies undef as
    /// a ptr lets it materialize as any value, including a non-null
    /// one that breaks downstream null-as-None reads.
    pub(crate) fn niche_store_option_field(
        &self,
        field_ptr: inkwell::values::PointerValue<'ctx>,
        new_val: BasicValueEnum<'ctx>,
    ) {
        let new_ptr = self.option_value_to_niche_ptr(new_val);
        self.builder.build_store(field_ptr, new_ptr).unwrap();
    }

    /// Niche-ABI pack: convert a 4-i64 Option struct SSA value into the
    /// single nullable pointer the niche representation expects. The SSA
    /// core of `niche_store_option_field` (which follows with a heap-slot
    /// store); also used directly by the function-return sites and
    /// `compile_call`'s arg loop for niche-ABI signatures, where the
    /// pointer is passed by value rather than stored.
    ///
    /// Tag-aware (same rationale as the store path): a `None` value is
    /// built as `get_undef()` + tag store, so `w0` must not be trusted
    /// when the tag says None — select null instead. A value that is
    /// already a pointer passes through unchanged (defensive; today every
    /// caller hands the conventional struct shape).
    pub(crate) fn option_value_to_niche_ptr(
        &self,
        new_val: BasicValueEnum<'ctx>,
    ) -> inkwell::values::PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        if let BasicValueEnum::PointerValue(p) = new_val {
            return p;
        }
        let sv = new_val.into_struct_value();
        let tag = self
            .builder
            .build_extract_value(sv, 0, "niche.store.tag")
            .unwrap()
            .into_int_value();
        let w0 = self
            .builder
            .build_extract_value(sv, 1, "niche.store.w0")
            .unwrap()
            .into_int_value();
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);
        let is_some = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                tag,
                i64_t.const_int(some_tag, false),
                "niche.store.is_some",
            )
            .unwrap();
        let ptr_from_w0 = self
            .builder
            .build_int_to_ptr(w0, ptr_ty, "niche.store.ptr_some")
            .unwrap();
        self.builder
            .build_select(is_some, ptr_from_w0, ptr_ty.const_null(), "niche.store.ptr")
            .unwrap()
            .into_pointer_value()
    }

    /// True when the function currently being compiled returns
    /// `Option[shared T]` under the niche call ABI (single nullable
    /// `ptr`), i.e. every `ret` must pack the conventional 4-i64 Option
    /// value through `option_value_to_niche_ptr` first. Keyed off
    /// `current_fn`'s LLVM symbol name so nested-fn compiles (closures,
    /// par branch fns, reduce workers, generic monos — all of which swap
    /// `current_fn` and none of which mint `fn_niche_abi` entries)
    /// resolve to `false` without any flag threading.
    pub(crate) fn current_fn_ret_is_niche(&self) -> bool {
        let Some(cf) = self.current_fn else {
            return false;
        };
        cf.get_name()
            .to_str()
            .ok()
            .and_then(|name| self.fn_niche_abi.get(name))
            .is_some_and(|abi| abi.ret)
    }

    /// Inner shared heap type when the function currently being compiled
    /// returns `Option[shared T]` — the same record `compile_function`
    /// seeds `tail_ret_inner` from, but derivable at any point in the
    /// body (the flow-sensitive `tail_ret_inner` is deliberately cleared
    /// during statement compilation, so an explicit `return expr;` can't
    /// read it). Keyed off `current_fn`'s LLVM symbol name via
    /// `fn_return_option_inner_shared`, so nested-fn compiles (closures,
    /// par branch fns, monos) resolve to `None` without flag threading —
    /// same discipline as `current_fn_ret_is_niche`.
    pub(crate) fn current_fn_ret_option_inner_heap(&self) -> Option<StructType<'ctx>> {
        let cf = self.current_fn?;
        let name = cf.get_name().to_str().ok()?;
        let inner = self.fn_return_option_inner_shared.get(name)?;
        Some(self.shared_types.get(inner.as_str())?.heap_type)
    }

    pub(super) fn option_inner_shared_type_for_type_expr(
        &self,
        te: &TypeExpr,
    ) -> Option<(String, SharedTypeInfo<'ctx>)> {
        let p = match &te.kind {
            TypeKind::Path(p) => p,
            _ => return None,
        };
        // Accept both bare `Option[T]` and `path::to::Option[T]` (the
        // latter is rare today but mirrors how `shared_heap_type_for_type_expr`
        // doesn't require single-segment paths — defensive against
        // future path-qualified Option references).
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return None;
        }
        let args = p.generic_args.as_ref()?;
        let inner_te = args.iter().find_map(|a| match a {
            GenericArg::Type(t) => Some(t),
            _ => None,
        })?;
        let inner_path = match &inner_te.kind {
            TypeKind::Path(p) => p,
            _ => return None,
        };
        let inner_name = inner_path.segments.last()?;
        let info = self.shared_types.get(inner_name.as_str())?.clone();
        Some((inner_name.clone(), info))
    }

    /// For a let-binding's declared `Option[T]` / `Result[T, E]` type,
    /// return each *boxed* payload variant as
    /// `(enum_name, payload_variant, inner_struct_name)`. A variant's
    /// payload is boxed when its LLVM word count exceeds the enum's fixed
    /// area (Option = 3, Result = 5) — the same predicate
    /// `coerce_to_payload_words` packs with. Shared payloads lower to a
    /// 1-word RC pointer and fitting payloads stay ≤ area, so both yield
    /// nothing. `inner_struct_name` names a user struct payload (so its
    /// `__karac_drop_struct_<T>` field cleanup runs before the box is
    /// freed) or `None` for tuples / scalars. Drives `track_boxed_enum_var`
    /// at the let-site. See docs/spikes/oversized-enum-payload.md.
    pub(super) fn boxed_enum_payload_variants(
        &self,
        te: &TypeExpr,
    ) -> Vec<(&'static str, &'static str, Option<String>)> {
        let TypeKind::Path(p) = &te.kind else {
            return vec![];
        };
        let enum_name = p.segments.last().map(|s| s.as_str());
        let (enum_lit, area, variants): (&'static str, usize, &[&'static str]) = match enum_name {
            Some("Option") => ("Option", 3, &["Some"]),
            Some("Result") => ("Result", 5, &["Ok", "Err"]),
            _ => return vec![],
        };
        let args: Vec<&TypeExpr> = match &p.generic_args {
            Some(a) => a
                .iter()
                .filter_map(|g| match g {
                    GenericArg::Type(t) => Some(t),
                    _ => None,
                })
                .collect(),
            None => return vec![],
        };
        let mut out = Vec::new();
        for (i, variant) in variants.iter().enumerate() {
            let Some(arg) = args.get(i) else {
                continue;
            };
            let ll = self.llvm_type_for_type_expr(arg);
            if Self::llvm_type_word_count(ll) > area {
                let inner_struct = match &arg.kind {
                    TypeKind::Path(ip) => ip
                        .segments
                        .last()
                        .map(|s| s.as_str())
                        .filter(|n| self.struct_types.contains_key(*n))
                        .map(|n| n.to_string()),
                    _ => None,
                };
                out.push((enum_lit, *variant, inner_struct));
            }
        }
        out
    }

    /// Recover the source `TypeExpr` of an *untyped* `let`'s RHS when it is a
    /// direct call to a known free function, so the oversized-payload box drop
    /// (`boxed_enum_payload_variants` + `track_boxed_enum_var`) can run without
    /// an annotation (`let o = make_opt()` where `make_opt -> Option[Wide]`).
    /// Returns the callee's recorded return `TypeExpr` (`fn_return_type_exprs`).
    /// Method-call RHS (`let o = v.pop()`) is a deferred narrow case — the box
    /// leaks but never double-frees; see docs/spikes/oversized-enum-payload.md
    /// §3. `None` for any other RHS shape (the caller keeps the annotation).
    pub(super) fn untyped_let_boxed_enum_te(&self, value: &Expr) -> Option<TypeExpr> {
        let ExprKind::Call { callee, .. } = &value.kind else {
            return None;
        };
        let ExprKind::Identifier(name) = &callee.kind else {
            return None;
        };
        self.fn_return_type_exprs.get(name).cloned()
    }
}

/// Is `te` a path whose last segment is `bool`? Used by the Atomic arm
/// of `llvm_type_for_type_expr` to widen `Atomic[bool]`'s slot to i8
/// (LLVM rejects `load atomic i1` / `store atomic i1` directly), and
/// by the let-stmt + struct-field codegen paths to record per-receiver
/// "this Atomic slot's inner type is bool" so `.load` truncs and
/// `.store` zexts at the call site.
pub(super) fn is_bool_type_expr(te: &TypeExpr) -> bool {
    if let TypeKind::Path(p) = &te.kind {
        return p.segments.last().map(|s| s.as_str()) == Some("bool");
    }
    false
}

/// If `te` matches `Atomic[T]` and `T` is `bool`, returns true.
/// Threads the `Atomic[bool]` annotation through to the codegen sites
/// that need to know about the i8/i1 mismatch (let-stmt registration,
/// struct-field receiver dispatch).
pub(super) fn is_atomic_bool_type_expr(te: &TypeExpr) -> bool {
    let path = match &te.kind {
        TypeKind::Path(p) => p,
        _ => return false,
    };
    if path.segments.last().map(|s| s.as_str()) != Some("Atomic") {
        return false;
    }
    let args = match &path.generic_args {
        Some(a) => a,
        None => return false,
    };
    let inner = args.iter().find_map(|a| match a {
        GenericArg::Type(t) => Some(t),
        _ => None,
    });
    matches!(inner, Some(t) if is_bool_type_expr(t))
}
