//! Function declaration + body compilation.
//!
//! Houses `apply_linker_attrs` (per-fn attribute lowering for
//! `#[link_name]` / `#[no_mangle]` / `#[used]`), `declare_function`
//! (LLVM `FunctionType` construction from a Kāra `Function` AST node),
//! and `compile_function` (the per-function-body compilation driver).

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, FunctionValue};
use inkwell::AddressSpace;

use super::state::VarSlot;

/// True when `return_type` denotes `Self` or the named type `type_name` — the
/// constructor return shape (a `-> Self` parses as `TypeKind::Path(["Self"])`;
/// an explicit `-> Type` is `Path([…, "Type"])`). Distinguishes a constructor
/// (whose return value carries the type's invariants) from a static associated
/// function returning some unrelated type. Mirrors the interpreter's helper of
/// the same name.
fn returns_self_or_type(return_type: Option<&TypeExpr>, type_name: &str) -> bool {
    match return_type.map(|t| &t.kind) {
        Some(TypeKind::Path(p)) => {
            matches!(p.segments.last().map(String::as_str), Some(seg) if seg == "Self" || seg == type_name)
        }
        _ => false,
    }
}

/// The type name of a **by-value** struct param — a single-segment
/// `TypeKind::Path` (`s: Stats`). `None` for a `ref`/`mut ref`/pointer param
/// (`ref Stats`, `*const Stats`) or any non-path type, so only genuine
/// by-value struct params reach the AArch64 ABI coercion.
fn struct_by_value_param_name(ty: &TypeExpr) -> Option<String> {
    match &ty.kind {
        TypeKind::Path(p) if p.segments.len() == 1 => Some(p.segments[0].clone()),
        _ => None,
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// The per-target classification for `func`'s `#[repr(C)]` struct-by-value
    /// return: `Direct` when it doesn't apply (not an export, not a by-value
    /// struct return, or a target/shape that returns raw — e.g. an AArch64 HFA
    /// or any x86-64 struct ≤ 16 B), `Coerced` for an AArch64 ≤ 16 B register
    /// return, or `Sret` for a larger-than-16 B return on either target
    /// (B-2026-07-09-2 Slices 2 + 3b/3c).
    fn repr_c_struct_return_class_for(
        &mut self,
        func: &Function,
    ) -> Result<super::types_lowering::Arm64ReturnClass<'ctx>, String> {
        use super::types_lowering::Arm64ReturnClass;
        if !crate::cheader::is_exported(func) {
            return Ok(Arm64ReturnClass::Direct);
        }
        let Some(rt) = func.return_type.as_ref() else {
            return Ok(Arm64ReturnClass::Direct);
        };
        let Some(name) = struct_by_value_param_name(rt) else {
            return Ok(Arm64ReturnClass::Direct);
        };
        if !self.struct_types.contains_key(name.as_str()) {
            return Ok(Arm64ReturnClass::Direct);
        }
        if self.target_is_aarch64 {
            self.arm64_repr_c_struct_return_coercion(&name)
        } else if self.target_is_x86_64 {
            self.x86_64_repr_c_struct_return_class(&name)
        } else {
            Ok(Arm64ReturnClass::Direct)
        }
    }

    /// The `#[link_name("symbol")]` value carried by `attrs`, if any —
    /// the C/foreign symbol an `unsafe extern` import binds to, distinct
    /// from the Kāra identifier. Reads `string_value` first (the parser's
    /// canonical slot) then falls back to scanning positional args for a
    /// string literal, exactly as the `link_section` handler in
    /// [`apply_linker_attrs`] does. Bare/non-string `#[link_name]` yields
    /// `None` (the caller then keeps the Kāra name). This is what lets a
    /// snake_case Kāra fn bind a PascalCase C symbol — the LLVM-C API
    /// (`LLVMContextCreate`, …) is the motivating consumer
    /// (`docs/spikes/self-hosting-llvm-c-ffi.md` § Linking).
    pub(super) fn link_name_attr(attrs: &[Attribute]) -> Option<String> {
        attrs.iter().find_map(|attr| {
            if attr.path.len() != 1 || attr.path[0] != "link_name" {
                return None;
            }
            attr.string_value.clone().or_else(|| {
                attr.args.iter().find_map(|a| match a.value.as_ref() {
                    Some(Expr {
                        kind: ExprKind::StringLit(s),
                        ..
                    }) => Some(s.clone()),
                    _ => None,
                })
            })
        })
    }

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
        // FFI export Case 2 (design.md § Panic Semantics at the FFI
        // Boundary): an `extern "C-unwind" fn` export must let a body
        // panic propagate across the boundary as a C++-shaped unwind.
        // That requires the panic-unwind substrate (LLVM invoke /
        // landingpad / personality + `panic = "unwind"`), which this
        // backend does not yet have — panics currently lower to a print
        // + `exit(1)` (abort-style). Rather than silently miscompile a
        // C-unwind export into an abort (which would defeat the ABI's
        // whole purpose), reject it with a pointer to the working
        // alternative. `extern "C"` (Case 1) needs no substrate: a body
        // panic already aborts the process, which IS the case-1
        // defined-abort contract.
        if func.abi.as_deref() == Some("C-unwind") {
            return Err(format!(
                "exported `extern \"C-unwind\" fn '{}'` cannot be compiled: propagating an \
                 unwinding panic across the FFI boundary requires the panic-unwind substrate \
                 (LLVM invoke/landingpad + `panic = \"unwind\"`), which is not implemented in \
                 this backend (panics currently lower to abort). Use `extern \"C\"` instead — a \
                 body panic auto-aborts at the boundary (design.md § Panic Semantics at the FFI \
                 Boundary, case 1) — or wrap the body in `catch_panic` to return a C-shaped \
                 error code. Tracked at docs/implementation_checklist/phase-6-runtime.md \
                 § \"Panic semantics at the FFI boundary\".",
                func.name
            ));
        }

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

        // AArch64 `#[repr(C)]` struct-by-value ABI (B-2026-07-09-2): an
        // exported fn's by-value struct param is passed per AAPCS (coerced to a
        // register type), not as a raw LLVM struct (which only matches the C
        // ABI on x86-64). Coercion is export-only + arm64-only, so x86-64 and
        // every non-export signature are byte-identical to before. The
        // reconstruction from the coerced value happens in the body prologue.
        let is_export = crate::cheader::is_exported(func);
        let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> =
            Vec::with_capacity(func.params.len());
        let mut coerced_struct_params: Vec<(usize, String)> = Vec::new();
        let mut indirect_struct_params: Vec<(usize, String)> = Vec::new();
        for (i, p) in func.params.iter().enumerate() {
            if is_export {
                if let Some(name) = struct_by_value_param_name(&p.ty) {
                    if self.struct_types.contains_key(name.as_str()) {
                        // Per-target `#[repr(C)]` struct-by-value param ABI. On
                        // AArch64 the full AAPCS classifier applies (HFA / ≤ 16 B
                        // register coercion / larger-than-16 B indirect). On
                        // x86-64 only the larger-than-16 B MEMORY case needs
                        // handling (indirect `byval`); ≤ 16 B stays raw.
                        let class = if self.target_is_aarch64 {
                            self.arm64_repr_c_struct_coercion(&name)?
                        } else if self.target_is_x86_64 {
                            self.x86_64_repr_c_struct_param_class(&name)?
                        } else {
                            super::types_lowering::Arm64ParamClass::Direct
                        };
                        match class {
                            super::types_lowering::Arm64ParamClass::Coerced(coerced) => {
                                param_types.push(coerced.into());
                                coerced_struct_params.push((i, name));
                                continue;
                            }
                            super::types_lowering::Arm64ParamClass::Indirect => {
                                // Larger than 16 B: passed by pointer — a plain
                                // `ptr` to a caller copy (AArch64) or a `ptr
                                // byval(%Struct)` (x86-64, attribute added after
                                // `add_function`). The prologue loads the struct
                                // value back through it either way.
                                param_types
                                    .push(self.context.ptr_type(AddressSpace::default()).into());
                                indirect_struct_params.push((i, name));
                                continue;
                            }
                            super::types_lowering::Arm64ParamClass::Direct => {}
                        }
                    }
                }
            }
            param_types.push(self.llvm_param_type(p));
        }
        if !coerced_struct_params.is_empty() {
            self.arm64_coerced_struct_params
                .insert(func.name.clone(), coerced_struct_params);
            self.abi_adapted_export_names.insert(func.name.clone());
        }
        if !indirect_struct_params.is_empty() {
            self.indirect_struct_params
                .insert(func.name.clone(), indirect_struct_params);
            self.abi_adapted_export_names.insert(func.name.clone());
        }

        // Niche call ABI for `Option[shared T]` signature positions
        // (wip-shared-struct-codegen-followups Slice 1 + method
        // extension): pass/return a single nullable `ptr` (null = None)
        // instead of the 4-i64 Option enum struct, mirroring the
        // field-niche layout so the type is pointer-shaped on both sides
        // of the call boundary. Applies to free user fns AND impl
        // methods (dotted names) — every method call surface packs/
        // unpacks via the shared helpers (`pack_niche_abi_args` /
        // `unpack_niche_abi_ret`): `usercall` (assoc_call.rs),
        // `usermethod` (method_call.rs; the calls.rs receiver variants
        // route through it), and the provider-vtable dispatch
        // (provider.rs — its indirect-call FunctionType already comes
        // from a declared impl fn via `provider_method_fn_type`, so a
        // niche-shaped impl flows through the vtable type-consistently;
        // ambient builtin resources have fixed scalar signatures that
        // never qualify). Still excluded:
        //   - coroutine ramps keep their own `ptr`-handle convention.
        //   - generic fns never reach this path (monomorphized
        //     signatures are declared in `declare_mono_function`).
        //   - extern decls are declared elsewhere (FFI shape is
        //     contract-fixed).
        // Eligibility per position reuses the field-niche predicate
        // (`option_inner_shared_type_for_type_expr`) so the two niche
        // surfaces stay in sync. The record lands in `fn_niche_abi`;
        // every pack/unpack site keys off that map.
        let niche_eligible = !self.is_coroutine_compiled(&func.name);
        let niche_params: Vec<bool> = func
            .params
            .iter()
            .map(|p| niche_eligible && self.option_inner_shared_type_for_type_expr(&p.ty).is_some())
            .collect();
        let niche_ret = niche_eligible
            && func
                .return_type
                .as_ref()
                .is_some_and(|te| self.option_inner_shared_type_for_type_expr(te).is_some());
        if niche_params.iter().any(|&p| p) || niche_ret {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            for (i, &is_niche) in niche_params.iter().enumerate() {
                if is_niche {
                    param_types[i] = ptr_ty.into();
                }
            }
            self.fn_niche_abi.insert(
                func.name.clone(),
                super::state::NicheAbi {
                    ret: niche_ret,
                    params: niche_params,
                },
            );
        }

        // By-value SoA param ABI: the BASE symbol always lowers a `Vec[E]` param
        // as the AoS `{ptr,len,cap}` struct (slice 5). A SoA-laid-out argument
        // never reaches the base symbol — the call dispatch
        // (`compute_call_layout_subst` + `ensure_layout_mono_generated`) routes
        // it to a per-layout monomorph whose signature is patched to the 4-field
        // SoA struct in `declare_mono_function`. The old name-keyed
        // `soa_value_param_layout` patch here (slice 1) lowered the base param
        // SoA whenever its NAME coincided with a `layout` block — a footgun:
        // calling the same base symbol with a *non*-SoA `Vec[E]` (any AoS arg)
        // then marshalled a 3-field AoS Vec into a 4-field SoA slot. Retired in
        // slice 5; the mono path (regardless of param name) is the sole SoA
        // by-value carrier.

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
        } else if niche_ret {
            // Niche return: single nullable ptr instead of the 4-i64
            // Option struct (and instead of the sret round-trip the
            // struct shape costs on aarch64). The body's return sites
            // pack via `option_value_to_niche_ptr`.
            self.context
                .ptr_type(AddressSpace::default())
                .fn_type(&param_types, false)
        } else if crate::cheader::boxed_return_of(func).is_some()
            || self.boxed_enum_export_names.contains(&func.name)
        {
            // C-ABI auto-boxed aggregate return (additive-interop Slice 4
            // Path B): the export returns an opaque pointer to a heap box
            // instead of the `{data,len,cap}` value in registers (which
            // wouldn't match the SysV struct-return ABI). The return sites
            // box via `box_return_value`. A Slice-2a tagged-union `#[repr(C)]`
            // enum return (`boxed_enum_export_names`) is boxed identically —
            // its `{ tag, w0 }` value likewise doesn't match the by-value ABI.
            self.context
                .ptr_type(AddressSpace::default())
                .fn_type(&param_types, false)
        } else if let super::types_lowering::Arm64ReturnClass::Coerced(coerced_ret) =
            self.repr_c_struct_return_class_for(func)?
        {
            // AArch64 `#[repr(C)]` struct-by-value return (B-2026-07-09-2
            // Slice 2): return the AAPCS-coerced register type (`i64` /
            // `[2 x i64]`) instead of the raw struct. Each return site
            // reinterprets its struct value into it. Recorded so the body
            // prologue picks up the coercion, and the fn is marked a coerced
            // export (internal Kāra calls rejected — the caller expects a
            // struct value, not the register form). x86-64 never yields
            // `Coerced` (≤ 16 B stays raw there).
            self.arm64_coerced_struct_returns
                .insert(func.name.clone(), coerced_ret);
            self.abi_adapted_export_names.insert(func.name.clone());
            match coerced_ret {
                BasicTypeEnum::IntType(t) => t.fn_type(&param_types, false),
                BasicTypeEnum::ArrayType(t) => t.fn_type(&param_types, false),
                _ => unreachable!("arm64 struct-return coercion is i64 or [2 x i64]"),
            }
        } else if let super::types_lowering::Arm64ReturnClass::Sret(struct_ty) =
            self.repr_c_struct_return_class_for(func)?
        {
            // `#[repr(C)]` larger-than-16 B struct return (B-2026-07-09-2 Slice
            // 3b/3c): returned via `sret` on both AArch64 (x8) and x86-64 SysV
            // (rdi). The function returns `void` and gains a leading `ptr
            // sret(%Struct)` result param; each return site stores the struct
            // value through it. Recorded so the body sets up the sret param +
            // shifts Kāra param indices; the `sret` attribute (ABI-load-bearing)
            // is added after `add_function`. Internal Kāra calls are rejected.
            self.sret_struct_returns
                .insert(func.name.clone(), struct_ty);
            self.abi_adapted_export_names.insert(func.name.clone());
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let mut sret_params: Vec<BasicMetadataTypeEnum<'ctx>> =
                Vec::with_capacity(param_types.len() + 1);
            sret_params.push(ptr_ty.into());
            sret_params.extend_from_slice(&param_types);
            self.context.void_type().fn_type(&sret_params, false)
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
            // Full return TypeExpr — the untyped-let oversized-enum boxing
            // path needs the generic arg of `Option[T]` / `Result[T, E]`,
            // which the bare-segment `fn_return_type_names` below drops.
            self.fn_return_type_exprs
                .insert(func.name.clone(), ret_ty.clone());
            // Borrow return (`-> ref T` / `-> mut ref T`): record the
            // inner `T` so the caller binds the call result as a ref-local
            // (deref on use) instead of as a raw value — caller half of
            // B-2026-06-07-5. A `ref BorrowedStruct` return is EXCLUDED: it
            // is returned by value (see `llvm_return_type`), so the caller
            // binds an ordinary struct value, not a ref-local.
            if let TypeKind::Ref(inner) | TypeKind::MutRef(inner) = &ret_ty.kind {
                if !self.ref_return_is_value_abi(inner) {
                    self.fn_ref_return_inner
                        .insert(func.name.clone(), (**inner).clone());
                }
            }
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
            || self.force_external_linkage
            || func.is_pub
            || func.abi.is_some()
            || func
                .attributes
                .iter()
                .any(|a| a.is_bare("no_mangle") || a.is_bare("used"))
        {
            // `func.abi.is_some()` — an FFI export (`extern "C" fn`). The
            // symbol must have External linkage so a C caller can resolve
            // it; Kāra fn names are already un-mangled (`add_function`
            // uses the bare name), so no name change is needed. This is
            // the codegen half of FFI export Case 1 (the auto-abort half
            // is free: a body panic already aborts the process). A
            // non-`pub` `extern "C" fn` is still exported — C-callability
            // is the point of the marker, independent of Kāra visibility.
            Some(Linkage::External)
        } else {
            Some(Linkage::Internal)
        };
        let fn_val = self.module.add_function(&func.name, fn_type, linkage);
        self.apply_linker_attrs(fn_val, &func.attributes);
        self.emit_param_alias_attrs(fn_val, func);
        self.emit_codegen_hint_attrs(fn_val, func);

        // `sret` return (B-2026-07-09-2 Slice 3b/3c): attach the `sret(%Struct)`
        // type attribute to the leading result pointer. This is ABI-load-bearing,
        // not cosmetic — the backend uses it to route the pointer through the
        // dedicated result register (x8 on AArch64, rdi on x86-64 SysV); a plain
        // ptr param would take an ordinary arg register and break the C contract.
        // Matches `clang`'s `ptr sret(%struct.P) %0` on both targets.
        if let Some(&struct_ty) = self.sret_struct_returns.get(&func.name) {
            use inkwell::types::AnyType;
            let kind_id = inkwell::attributes::Attribute::get_named_enum_kind_id("sret");
            let attr = self
                .context
                .create_type_attribute(kind_id, struct_ty.as_any_type_enum());
            fn_val.add_attribute(inkwell::attributes::AttributeLoc::Param(0), attr);
        }

        // x86-64 SysV indirect struct param (B-2026-07-09-2 Slice 3c): a
        // larger-than-16 B `#[repr(C)]` struct is MEMORY class, passed as a `ptr
        // byval(%Struct)`. The `byval` attribute is ABI-load-bearing on x86-64 —
        // it tells the backend the caller has placed a copy on the stack and the
        // callee reads through the pointer (matches `clang`'s `ptr byval(...)`).
        // AArch64 does NOT use `byval` (its indirect params are plain pointers),
        // so this is x86-64-only. The LLVM param index is the Kāra index shifted
        // by +1 when the fn also returns via `sret` (the leading result pointer).
        if self.target_is_x86_64 {
            if let Some(indirect) = self.indirect_struct_params.get(&func.name).cloned() {
                use inkwell::types::AnyType;
                let sret_base = u32::from(self.sret_struct_returns.contains_key(&func.name));
                let kind_id = inkwell::attributes::Attribute::get_named_enum_kind_id("byval");
                for (idx, struct_name) in indirect {
                    if let Some(&struct_ty) = self.struct_types.get(struct_name.as_str()) {
                        let attr = self
                            .context
                            .create_type_attribute(kind_id, struct_ty.as_any_type_enum());
                        fn_val.add_attribute(
                            inkwell::attributes::AttributeLoc::Param(idx as u32 + sret_base),
                            attr,
                        );
                    }
                }
            }
        }

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

    /// Emit ownership-derived LLVM pointer-aliasing attributes on parameters
    /// (noalias-ref-params slice 1). Today this emits `noalias` on every
    /// `mut ref T` parameter, lowering the language's exclusive-borrow
    /// guarantee into the attribute LLVM's alias analysis and loop vectorizer
    /// want but could never prove on their own.
    ///
    /// **Why `mut ref T` → `noalias` is sound.** A `mut ref T` is an
    /// *exclusive* borrow: while it is live, no other reference — shared or
    /// mutable — to the same object may be used. That is precisely LLVM's
    /// `noalias` contract (memory reached through this pointer is reached
    /// through no pointer not derived from it). The guarantee is pinned by
    /// the type system (design.md § Variance — "`mut ref T` is invariant in
    /// `T` — load-bearing soundness pin") and enforced by the borrow checker
    /// and RC-fallback pass (design.md § Part 4 — "RC and mutation are mutually
    /// exclusive": a value with two un-ordered live uses is demoted to RC and
    /// can then never be mutably borrowed, so an RC-aliased value never
    /// reaches a `mut ref` position). Exclusivity *subsumes* interior
    /// mutability, so — unlike the `ref T` read-borrow case — no "Freeze"
    /// predicate is needed: even if `T` carries `Atomic[_]` / `Mutex[_]`
    /// fields, a `mut ref` to it is still the unique live path.
    ///
    /// **The one carve-out: shared types.** A `shared struct` / `shared enum`
    /// uses RC reference semantics with *per-field runtime borrow flags*
    /// (design.md § Part 5), so its exclusivity is a dynamic check, not a
    /// static one — exactly the assumption `noalias` must not rest on. The
    /// type system already forbids `mut ref self` on a shared type (a `mut
    /// ref` needs exclusive ownership, impossible under multiple RC holders),
    /// so in principle none reach here; we nonetheless skip any `mut ref`
    /// whose referent is a known shared type, closing the danger zone even if
    /// a checker hole ever lets one through.
    ///
    /// **What is NOT a target.** `ref T` (shared read borrow) is deferred: it
    /// needs `readonly` gated on a transitive Freeze predicate because
    /// `Atomic[_]`/`Mutex[_]` fields mutate through a shared `ref self`
    /// (design.md § Part 5 — Kāra's `UnsafeCell` analogue). `mut Slice[T]`
    /// (`TypeKind::MutSlice`) is a by-value `{ptr,len}` fat struct — its
    /// pointer is a field, not the parameter — so slice-kernel disjointness
    /// needs `!alias.scope`/`!noalias` metadata on the loads/stores
    /// (design.md:§ proven disjointness lowering), a separate slice.
    /// Monomorphized generics are declared in `declare_mono_function`, not
    /// here, so they are a follow-on for the same treatment.
    ///
    /// **Index correspondence.** Kāra param `i` is LLVM param `i`: the niche
    /// ABI rewrites param *types* but never reorders, and the coroutine ramp
    /// appends its hidden completion slot *after* all Kāra params, so
    /// `AttributeLoc::Param(i)` is correct on both paths. (`mut ref self`
    /// receivers are desugared into `params[0]` upstream by
    /// `make_impl_method_function`, so they are covered.)
    fn emit_param_alias_attrs(&self, fn_val: FunctionValue<'ctx>, func: &Function) {
        let noalias_kind = inkwell::attributes::Attribute::get_named_enum_kind_id("noalias");
        debug_assert!(noalias_kind != 0, "noalias attribute kind-id must resolve");
        for (i, param) in func.params.iter().enumerate() {
            let TypeKind::MutRef(inner) = &param.ty.kind else {
                continue;
            };
            // Skip the shared-type carve-out (see doc comment): a `mut ref`
            // to a `shared struct`/`shared enum` rests on runtime borrow
            // flags, not static exclusivity.
            if let TypeKind::Path(p) = &inner.kind {
                if let Some(name) = p.segments.last() {
                    if self.shared_types.contains_key(name.as_str()) {
                        continue;
                    }
                }
            }
            fn_val.add_attribute(
                inkwell::attributes::AttributeLoc::Param(i as u32),
                self.context.create_enum_attribute(noalias_kind, 0),
            );
        }
    }

    /// Lower the codegen-hint attributes (`#[inline]`,
    /// `#[inline(always)]`, `#[inline(never)]`, `#[cold]`) to their LLVM
    /// function-attribute equivalents. The inline axis maps
    /// `Default → inlinehint`, `Always → alwaysinline`,
    /// `Never → noinline`; `#[cold]` maps to `cold`. The two axes are
    /// independent and both may be present (`#[cold] #[inline(never)]`
    /// is the canonical "definitely cold, definitely out of line" pair).
    /// These are advisory at the LLVM level — reported behavior, not
    /// guaranteed semantics (design.md § Codegen Hint Attributes).
    fn emit_codegen_hint_attrs(&self, fn_val: FunctionValue<'ctx>, func: &Function) {
        let mut attrs: Vec<&'static str> = Vec::new();
        match func.inline_hint {
            Some(InlineHint::Default) => attrs.push("inlinehint"),
            Some(InlineHint::Always) => attrs.push("alwaysinline"),
            Some(InlineHint::Never) => attrs.push("noinline"),
            None => {}
        }
        if func.is_cold {
            attrs.push("cold");
        }
        for name in attrs {
            let kind = inkwell::attributes::Attribute::get_named_enum_kind_id(name);
            debug_assert!(kind != 0, "{name} attribute kind-id must resolve");
            fn_val.add_attribute(
                inkwell::attributes::AttributeLoc::Function,
                self.context.create_enum_attribute(kind, 0),
            );
        }
    }

    pub(super) fn compile_function(&mut self, func: &Function) -> Result<(), String> {
        // Heap-closure-env epic Slice 0 (B-2026-06-22-2): refuse to emit a
        // function that RETURNS a closure capturing one of its locals — the
        // captured env is a stack alloca that dangles once the frame exits, a
        // silent miscompile. Honest compile error until heap envs land. Pure
        // pre-check; no IR emitted yet.
        self.reject_escaping_capturing_closure(func)?;
        // Slice 1: a capturing-closure literal that is this function's direct
        // tail escapes via the return → it gets a reference-counted HEAP env
        // (so its captures outlive the frame). Record its span for
        // `compile_closure`, and reset the per-function heap-env-binding set.
        self.current_fn_heap_closure_spans.clear();
        if let Some(span) = self.func_tail_heap_closure_span(func) {
            self.current_fn_heap_closure_spans.insert(span);
        }
        self.heap_env_closure_vars.clear();
        self.heap_env_owner_fields.clear();
        self.heap_env_tuple_owners.clear();
        self.heap_env_array_owners.clear();
        self.heap_env_vec_owners.clear();
        // Slice 1 misuse guard (B-2026-06-22-2): a heap-env closure binding may
        // only be CALLED in its owning function. Reject returning / copying /
        // storing / passing it, or an unbound `make(..)`, with an honest error
        // — otherwise the RC env would be double-freed, leaked, or used after
        // free. Runs after `fns_returning_heap_env` is populated (in `compile`).
        self.reject_heap_env_misuse(func)?;
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
        // Per-binding layout carrier (slice 5): function-scoped like
        // `variables`, so a `layout`-named local in one function can't bleed
        // its SoA-ness into a same-named binding in the next.
        self.binding_layouts.clear();
        // The base symbol returns AoS (the declared `Vec[E]` lowers to
        // `{ptr,len,cap}`); record its tail-returned local(s) so
        // `seed_binding_site_layout` does NOT name-match them SoA — a returned
        // local stays AoS here, matching the AoS return type. (A SoA-returning
        // specialization is the `return_layout` mono, not this base symbol.)
        self.soa_return_locals = self
            .soa_return_local_names(&func.body)
            .into_iter()
            .collect();
        self.inline_option_payload_vars.clear();
        self.boxed_enum_payload_vars.clear();
        self.inline_result_payload_vars.clear();
        self.inline_option_map_payload_vars.clear();
        self.inline_option_agg_payload_vars.clear();
        self.var_option_shared_heap.clear();
        self.ref_params.clear();
        self.entry_slot_ref_vars.clear();
        self.owned_vecstr_params.clear();
        self.for_loop_borrow_vars.clear();
        self.for_loop_owned_agg_vars.clear();
        self.owned_struct_params.clear();
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
        // Name-keyed instantiated-generic-enum types (`Option[String]`, …) for
        // heap-payload `==`. Same per-function-reset rationale as the other
        // name-keyed tables above: a stale entry from one function's `a:
        // Option[String]` must not resolve a next function's same-named
        // `a: Option[i64]` (which would mis-route a scalar `==` to the heap
        // String comparator). Repopulated below from params and at let sites.
        self.enum_inst_var_types.clear();
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
        // The handle-backed builtins were missing from this list (same
        // per-function-reset rationale — every entry is keyed by bare var
        // name): a `c: Column[i64]` in one function must not make the
        // Column intercept fire on a same-named non-column binding in the
        // next. Found alongside the mono-side leak (S6a / `SavedVarSideTables`).
        self.column_var_infos.clear();
        self.tensor_var_infos.clear();
        self.dataframe_var_infos.clear();
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

        // B-2026-06-12-9: `main() -> Result[(), E]` adaptation. `main` lowers
        // to the C entry `i32 main()`, so a Result-returning body cannot `ret`
        // its `{tag, …}` aggregate (verify failure). Capture E's `TypeExpr`
        // here; the tail / explicit-`return` / `?`-error sites consult it to
        // emit the design.md § Entry Point exit-code adaptation (`Ok(())` → 0,
        // `Err(e)` → `Error: {e}` to stderr + 1) instead of an aggregate `ret`.
        self.main_result_err_te = if func.name == "main" {
            func.return_type
                .as_ref()
                .and_then(|ret_ty| match &ret_ty.kind {
                    TypeKind::Path(path)
                        if path.segments.len() == 1 && path.segments[0] == "Result" =>
                    {
                        path.generic_args
                            .as_ref()
                            .and_then(|args| match args.get(1) {
                                Some(GenericArg::Type(e_te)) => Some(e_te.clone()),
                                _ => None,
                            })
                    }
                    _ => None,
                })
        } else {
            None
        };

        // Phase-8 entry-point contract Slice B: `fn main() -> ExitCode`.
        // `ExitCode` is `distinct type = i32` and `main` lowers to the C
        // entry `i32`, so the tail-return site `ret`s the body's value
        // (the exit code) coerced to i32 rather than the plain-`fn main()`
        // `ret i32 0`. Recognised by the bare `Path("ExitCode")` return
        // annotation. Distinct from — and mutually exclusive with —
        // `main_result_err_te` (the `Result[(), E]` adaptation).
        self.main_returns_exitcode = func.name == "main"
            && matches!(
                func.return_type.as_ref().map(|t| &t.kind),
                Some(TypeKind::Path(path))
                    if path.segments.len() == 1 && path.segments[0] == "ExitCode"
            );

        // Borrow-returning function (`-> ref T` / `-> mut ref T`): the
        // tail / explicit-`return` sites emit the borrow's ADDRESS via
        // `compile_ref_return_ptr` rather than its materialized value
        // (B-2026-06-07-5). A `ref BorrowedStruct` return is EXCLUDED — it
        // returns the struct BY VALUE (see `llvm_return_type`), so the tail
        // expr flows through the ordinary value-return path; routing it
        // through `compile_ref_return_ptr` would try to take the address of a
        // struct-literal temporary (dangling) and mismatch the by-value
        // signature.
        self.current_fn_returns_ref = matches!(
            func.return_type.as_ref().map(|t| &t.kind),
            Some(TypeKind::Ref(_) | TypeKind::MutRef(_))
        ) && !self.return_type_ref_is_value_abi(&func.return_type);
        // C-ABI auto-boxed aggregate return (additive-interop Slice 4 Path
        // B) — declared return type is `ptr` (above); the return sites box
        // the `{data,len,cap}` value and return the box pointer. A Slice-2a
        // tagged-union `#[repr(C)]` enum return boxes the same way.
        self.current_fn_boxes_return = crate::cheader::boxed_return_of(func).is_some()
            || self.boxed_enum_export_names.contains(&func.name);
        // AArch64 `#[repr(C)]` struct-by-value return coercion (B-2026-07-09-2
        // Slice 2): if set, every return site reinterprets its struct value
        // into this register type. `None` on x86-64 / non-coerced returns.
        self.current_fn_arm64_return_coercion =
            self.arm64_coerced_struct_returns.get(&func.name).copied();
        // `sret` return (B-2026-07-09-2 Slice 3b/3c): if this fn returns a
        // larger-than-16 B `#[repr(C)]` struct, its leading LLVM param is the
        // caller's result pointer (x8 on AArch64, rdi on x86-64). Capture it so
        // every return site stores through it; its presence also shifts each
        // Kāra param index by +1 in the binding loop below. `None` for
        // register/HFA returns.
        self.current_fn_sret_param = if self.sret_struct_returns.contains_key(&func.name) {
            Some(fn_val.get_nth_param(0).unwrap().into_pointer_value())
        } else {
            None
        };

        let entry = self.context.append_basic_block(fn_val, "entry");
        self.builder.position_at_end(entry);

        // Level 2 crash diagnostics — Part 2: open this function's DWARF
        // `DISubprogram` and make it the active scope (no-op unless debug info
        // is enabled). `fn_val` is the real LLVM function; the DWARF display
        // name is the user-facing `func.name`. `func.span.line` is 1-indexed.
        self.di_enter_function(fn_val, &func.name, func.span.line as u32);

        // Run the Map/Set module-binding static-init prologue before any
        // user statement. `__karac_static_init` was declared (signature
        // only) in `declare_module_bindings`; its body is filled at
        // `finalize_module_binding_static_init` after all bodies compile.
        // Placed after `di_enter_function` so the call carries a valid
        // `!dbg` location when debug info is on. `main` only.
        if func.name == "main" {
            if let Some(init_fn) = self.static_init_fn {
                self.builder.build_call(init_fn, &[], "").unwrap();
            }
        }

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
            // AArch64 `sret` return (Slice 3b) prepends a leading result-pointer
            // param, so every Kāra param sits one slot to the right. `sret_base`
            // is 1 when this fn returns via sret, 0 otherwise (the common case).
            let sret_base = u32::from(self.current_fn_sret_param.is_some());
            for (i, param) in func.params.iter().enumerate() {
                let param_name = self.param_name(param);
                let param_val = fn_val.get_nth_param(i as u32 + sret_base).unwrap();
                // AArch64 `#[repr(C)]` struct-by-value reconstruction
                // (B-2026-07-09-2): the LLVM param is the AAPCS-coerced type
                // (`[N x i64]` / `[N x fp]` / `i64`) rather than the raw struct.
                // Reinterpret its bytes as the original struct value — store to
                // a temp, reload as the struct type (both have identical size +
                // layout) — so every downstream field read is byte-identical to
                // the un-coerced path. Empty map on x86-64, so this is inert
                // there.
                let coerced_struct = self
                    .arm64_coerced_struct_params
                    .get(&func.name)
                    .and_then(|v| v.iter().find(|(idx, _)| *idx == i).map(|(_, n)| n.clone()));
                // AArch64 indirect struct param (B-2026-07-09-2 Slice 3a): a
                // > 16 B `#[repr(C)]` struct arrives as a `ptr` to the caller's
                // copy. Load the struct value through it — the alloca+store
                // below then owns an independent copy (AAPCS lets the callee
                // treat the indirect argument as its own).
                let indirect_struct = self
                    .indirect_struct_params
                    .get(&func.name)
                    .and_then(|v| v.iter().find(|(idx, _)| *idx == i).map(|(_, n)| n.clone()));
                let param_val = if let Some(struct_name) = coerced_struct {
                    let struct_ty = *self.struct_types.get(struct_name.as_str()).unwrap();
                    let tmp = self.create_entry_alloca(fn_val, "kabi.coerce", param_val.get_type());
                    self.builder.build_store(tmp, param_val).unwrap();
                    self.builder
                        .build_load(struct_ty, tmp, "kabi.reload")
                        .unwrap()
                } else if let Some(struct_name) = indirect_struct {
                    let struct_ty = *self.struct_types.get(struct_name.as_str()).unwrap();
                    self.builder
                        .build_load(struct_ty, param_val.into_pointer_value(), "kabi.indirect")
                        .unwrap()
                } else {
                    param_val
                };
                // Niche-ABI param unpack: an `Option[shared T]` param
                // declared `ptr`-shaped (see `declare_function`) is
                // rebuilt into the conventional 4-i64 Option struct here,
                // so the alloca below — and every downstream consumer
                // (`track_rc_option_var`, the Assign arms, pattern
                // matches, the RC-fallback boxing exclusion) — sees the
                // exact shape it saw before the ABI niche existed.
                let param_val = if self
                    .fn_niche_abi
                    .get(&func.name)
                    .is_some_and(|abi| abi.params.get(i).copied().unwrap_or(false))
                {
                    self.niche_ptr_to_option_value(param_val.into_pointer_value(), &param_name)
                } else {
                    param_val
                };
                // The base symbol lowers every `Vec[E]` param as the AoS
                // `{ptr,len,cap}` struct (slice 5); a SoA-laid-out argument is
                // routed to a per-layout monomorph by the call dispatch, never
                // to this base body. The old name-keyed SoA-param spill (slice 1,
                // `soa_value_param_layout`) was retired here — see the matching
                // note at the signature site in `declare_function`. A mono's SoA
                // by-value param is spilled in `compile_mono_function`'s prologue
                // (keyed on `layout_subst`, not the param name).
                let alloca = self.create_entry_alloca(fn_val, &param_name, param_val.get_type());
                self.builder.build_store(alloca, param_val).unwrap();
                // Track ref params: alloca holds a pointer-to-data.
                if let Some(inner_ty) = self.inner_type_of_ref(&param.ty) {
                    self.ref_params.insert(param_name.clone(), inner_ty);
                }
                // B-2026-06-20-1: a `Fn(...)`-typed parameter is a closure fat
                // pointer (lowered by `llvm_type_for_type_expr`'s `FnType`
                // arm). Register its env-first closure-call ABI fn type so a
                // body call `f(x)` routes through `compile_closure_call` (an
                // indirect call through the fat pointer) instead of the
                // unknown-callee fall-through. A bare named fn passed in for
                // `f` is reified into a matching `{trampoline, null}` fat
                // pointer at the call site (`reify_named_fn_as_fn_value`).
                if let TypeKind::FnType {
                    params,
                    return_type,
                    ..
                } = &param.ty.kind
                {
                    let fn_type = self.closure_abi_fn_type(params, return_type.as_deref());
                    self.closure_fn_types.insert(param_name.clone(), fn_type);
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
                    // Record an instantiated generic-enum param (`opt: Option[String]`)
                    // by name for heap-payload `==` routing — collision-free across
                    // f-string interpolations, unlike the span-keyed table.
                    if self.is_generic_named_enum_type_expr(te) {
                        self.enum_inst_var_types
                            .insert(param_name.clone(), te.clone());
                    }
                }
                // Record owned (bare, non-ref) `String` / `Vec[T]` params.
                // The registrar above put String/Vec params into
                // `vec_elem_types` (Slice params land in `slice_elem_types`
                // instead, so they're naturally excluded); intersect with
                // "not a ref/slice mode" to get the owned-header set that
                // retaining consume sites must deep-copy — see the field
                // doc on `owned_vecstr_params`.
                if !matches!(
                    param.ty.kind,
                    TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
                ) && self.vec_elem_types.contains_key(&param_name)
                {
                    self.owned_vecstr_params.insert(param_name.clone());
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
                        // Route through `record_var_type_name` so a refinement-
                        // typed param (`rows: NonEmpty[EnrichedRow]`) records its
                        // *base* name (`Vec`), not the alias — a raw insert here
                        // would clobber the correct base recorded by
                        // `register_var_from_type_expr` above and break field/
                        // method lookups on refinement-over-struct/-collection
                        // params.
                        self.record_var_type_name(param_name.clone(), type_name.clone());
                        // Owned (bare, non-ref Path) struct param whose struct
                        // has a heap (`Vec`/`String`) field — the field-move-out
                        // double-free set (B-2026-06-10-2). A `ref Struct` param
                        // doesn't take ownership, so it's excluded by the
                        // `Path(_)`-only guard.
                        if matches!(&param.ty.kind, TypeKind::Path(_))
                            && self
                                .struct_field_type_names
                                .get(type_name.as_str())
                                .is_some_and(|fields| {
                                    fields.iter().any(|f| {
                                        matches!(
                                            f.as_deref(),
                                            Some("Vec") | Some("VecDeque") | Some("String")
                                        )
                                    })
                                })
                        {
                            self.owned_struct_params.insert(param_name.clone());
                        }
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
                        // #14 — an owned (bare Path, non-ref) by-value aggregate
                        // (`struct` / `enum`) param: deep-copy its heap fields at
                        // entry and register its scope-exit drop so it is
                        // callee-owned. This closes the by-value-aggregate-param
                        // transfer-out double-free without a caller-side move
                        // (which Kāra's non-rejecting move-checker can't make
                        // sound). No-op for shared / Map-bearing aggregates
                        // (left on caller-retains). See param_own.rs.
                        if matches!(&param.ty.kind, TypeKind::Path(_))
                            && self.make_aggregate_param_callee_owned(type_name, alloca)
                        {
                            // #17 gap 1 — the param is now a callee-owned local:
                            // its heap fields are INDEPENDENT (entry-copied) and
                            // its scope-exit struct drop is registered. The
                            // caller-retains `owned_struct_params` field-move
                            // band-aid (`deep_copy_owned_struct_param_field_move`,
                            // stmts.rs) would now deep-copy a SECOND time on every
                            // `let x = p.field` move-out AND suppress the
                            // source-cap zeroing the normal local field-move-out
                            // performs — so the callee-owned drop and the moved-out
                            // binding both free the buffer. Retire the band-aid
                            // entry so field move-out routes through the standard
                            // local source-cap suppression.
                            self.owned_struct_params.remove(&param_name);
                        }
                    }
                }
                // #21 — a bare (non-ref) by-value TUPLE param carrying an enum /
                // nested-struct heap leaf: deep-copy it at entry so it is
                // callee-owned (independent of the caller's tuple), closing the
                // cross-boundary P5/P6 double-free where the callee consumes a
                // leaf internally while the caller's `NestedTuple` struct drop
                // frees the same shared buffer. Mirrors the named-aggregate
                // entry-copy above; copy-unsupported leaves (Map/shared) bail to
                // caller-retains.
                if let TypeKind::Tuple(elems) = &param.ty.kind {
                    // Record per-element type names so a `match p.0` on the param
                    // resolves the element's enum (the tuple-var arm of
                    // `place_chain_type_name`) — needed for the in-callee match
                    // suppression once the param is callee-owned below.
                    let elem_names: Vec<Option<String>> = elems
                        .iter()
                        .map(|e| match &e.kind {
                            TypeKind::Path(p) => p.segments.first().cloned(),
                            _ => None,
                        })
                        .collect();
                    self.tuple_var_elem_type_names
                        .insert(param_name.clone(), elem_names);
                    if let BasicTypeEnum::StructType(agg_ty) = param_val.get_type() {
                        self.make_tuple_param_callee_owned(elems, agg_ty, alloca);
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
                    // Phase C2b: borrowed-family params of a reconciled
                    // headerless type skip the exit dec AND the
                    // var_option_shared_heap registration — the caller
                    // skipped the arg inc symmetrically
                    // (`borrowed_arg_skip`), and a headerless node has
                    // no rc word to touch. Walk traffic was already
                    // count-free via the C2a family roles.
                    if !self.borrowed_param_dec_skip(&param_name) {
                        let option_ty = self.enum_layouts["Option"].llvm_type;
                        self.track_rc_option_var(&param_name, alloca, option_ty, info.heap_type);
                    }
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
                // `Option[shared T]` params are excluded for the same reason
                // as the let-site boxing skip in stmts.rs: the inner node is
                // already RC-managed and the `var_option_shared_heap` paths
                // address the slot as a raw Option struct — boxing it
                // redirects the slot to a heap ptr those paths misread.
                let is_option_shared_param = self
                    .option_inner_shared_type_for_type_expr(&param.ty)
                    .is_some();
                if !is_ref_param
                    && !is_vec_param
                    && !is_shared_param
                    && !is_option_shared_param
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
                // Coroutine-handler owned user-`Drop` param ownership (the
                // `ws_idle_holder` connection-reap leak class). A coroutine-
                // compiled fn cannot follow the normal by-value caller-drops
                // model for its owned params: at a spawn boundary the caller
                // ramps and returns (or moved the value into the task) *before*
                // the coroutine finishes using it, so the caller cannot be the
                // one to drop. Make the coroutine the owner instead — register
                // owned user-`Drop` params here so `emit_scope_cleanup` runs
                // their `Drop` on body-end completion and
                // `emit_coro_destroy_edge_cleanup` runs it on the per-park
                // destroy/cancel edge. Every caller of a coroutine fn suppresses
                // its own drop of the owned arg (`call_dispatch` /
                // `method_call`), keeping it a single drop — without that
                // suppression a synchronous (ramp+wait) caller would double-drop.
                //
                // Gated tightly: ONLY for coroutine-compiled fns, ONLY owned
                // (`Path`, non-ref) params, ONLY non-shared types (shared structs
                // drop through the RC path, never the value-type UserDrop drain —
                // see the `track_user_drop_var` gate in `compile_stmt`), and ONLY
                // types with a real `impl Drop` (`drop_method_keys`). This is the
                // user-`Drop` (resource) case only — `StructDrop`-only owned
                // params (heap-field cleanup with no user `Drop`) are NOT
                // registered here, because `suppress_user_drop_for_var` removes
                // only `UserDrop` actions, so a `StructDrop` param that is then
                // moved onward could not be suppressed and would double-free
                // (the failure mode that broke the tracing-builder E2E when the
                // general param loop tried to drop every owned struct param).
                if self.is_coroutine_compiled(&func.name) {
                    if let TypeKind::Path(path) = &param.ty.kind {
                        if let Some(struct_name) = path.segments.first() {
                            let has_user_drop = self
                                .program_snapshot
                                .as_deref()
                                .map(|p| p.drop_method_keys.contains_key(struct_name))
                                .unwrap_or(false);
                            if has_user_drop
                                && !self.shared_types.contains_key(struct_name.as_str())
                            {
                                self.track_user_drop_var(struct_name, &param_name, alloca);
                            }
                            // Channel-end (`Sender`/`Receiver`) sibling of the
                            // owned user-`Drop` transfer above. A channel end
                            // moved into a coroutine handler must be dropped BY
                            // THE COROUTINE at its completion — and for a
                            // `Sender` that drop is what CLOSES the channel
                            // (waking blocked receivers). At a non-blocking spawn
                            // boundary the wrapper ramps and returns while the
                            // coroutine is still parked, so it cannot be the one
                            // to drop: dropping `tx` there closes the channel
                            // *before* the resumed coroutine runs `tx.send(..)`,
                            // and the receiver sees the closed-sentinel instead of
                            // the sent value. Registering it here (drained at
                            // body-end completion, AFTER the send, and on the
                            // per-park destroy/cancel edge) makes the coroutine
                            // the unique owner; every caller suppresses its own
                            // `DropChannelEnd` at the move site
                            // (`suppress_channel_drop_for_var` in `call_dispatch`
                            // / `method_call`), keeping it a single close.
                            if struct_name == "Sender" || struct_name == "Receiver" {
                                self.track_channel_var(alloca, struct_name == "Sender");
                            }
                        }
                    }
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
            // Return type for the `result` binding in `emit_ensures_checks`
            // (so `result.field` resolves its struct field index).
            self.current_contract_result_type = func.return_type.clone();

            // Struct/impl `invariant` setup (rule 3): resolve the receiver
            // type's invariants for this method and stash them so
            // `emit_invariant_checks` can fire them inline before each `ret`
            // (same exit points as `ensures`), with `self` bound. The synthetic
            // method function carries `Type.method` as its name and the
            // method's `is_pub` flag — both consumed by `method_invariants_for`.
            // Free functions and invariant-free structs yield an empty list.
            self.current_method_invariants = self.method_invariants_for(&func.name, func.is_pub);
            self.constructor_invariant_self_type = None;
            // `method_invariants_for` keys purely off the `Type.method` name, so
            // it also matches associated functions (which `make_impl_method_function`
            // names `Type.method` but gives no `self` parameter). For those:
            //   - A *constructor* — returns `Self`/the type — checks the invariants
            //     against its RETURN value (the construction boundary). Record the
            //     type so `emit_invariant_checks` binds the return value as `self`.
            //   - Any other associated function (e.g. `Type.parse() -> i64`) is NOT
            //     a constructor: clear the invariants so we don't try to evaluate
            //     `self.field` against a non-receiver (which previously aborted
            //     codegen with `Undefined variable 'self'`).
            if !self.current_method_invariants.is_empty() {
                let has_self_param = func.params.first().is_some_and(|p| {
                    matches!(&p.pattern.kind, crate::ast::PatternKind::Binding(n) if n == "self")
                });
                if !has_self_param {
                    match func.name.split_once('.') {
                        // Constructor (returns `Self`/the type): bind the return
                        // value as `self` and enforce the invariants against it.
                        // Works for owned and shared (RC) structs alike — for a
                        // shared struct the return value is the heap pointer, and
                        // `self.field` resolves through the shared heap-GEP path
                        // because `shared_type_for_expr` accepts the constructor's
                        // `SelfValue` binding (gated to non-`ref`-param `self`).
                        Some((type_name, _))
                            if returns_self_or_type(func.return_type.as_ref(), type_name) =>
                        {
                            self.constructor_invariant_self_type = Some(type_name.to_string());
                        }
                        // Any other associated function (e.g. `Type.parse() -> i64`)
                        // is NOT a constructor: clear the name-resolved invariants
                        // so we don't evaluate `self.field` against a non-receiver
                        // (which would abort codegen with `Undefined variable 'self'`).
                        _ => self.current_method_invariants.clear(),
                    }
                }
            }
        }

        // Borrow-elision (B-2026-06-19-6): per-function set of `let r = v[i]`
        // RHS spans whose binding is a provably read-only, non-escaping borrow
        // of a container that is not mutated in scope. Consulted by the Let arm
        // to skip the heap-element deep-clone and the binding's scope-exit free.
        // Recomputed (overwritten) per function so it never leaks across fns.
        self.vec_index_borrow_spans =
            crate::codegen::borrow_elision::compute_vec_index_borrow_spans(&func.body);

        // Vec-length pins (bce_length_pin.rs): the rolling-DP bounds-check
        // elision. Recompute per function (both are function-scoped);
        // `vec_len_pins` starts empty and each pin activates when `compile_while`
        // / `compile_for_range_with_step` finishes emitting the matching fill
        // loop (so it is live only after).
        self.vec_len_pins.clear();
        self.pending_vec_len_pins =
            crate::codegen::bce_length_pin::compute_vec_length_pins(&func.body);

        // Slice 2 (auto-par codegen MVP): route the function body through
        // `compile_function_body`, which dispatches inferred parallel
        // groups to `karac_par_run` when a `ConcurrencyAnalysis` was
        // threaded into codegen. With no analysis, `compile_function_body`
        // falls through to `compile_block` and behavior is unchanged.
        let mut result = self.compile_function_body(&func.body)?;
        self.tail_ret_inner = None;

        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            // Owned String/Vec PARAM in tail position (`fn id(s: String)
            // -> String { s }`): the by-value header ABI leaves the
            // buffer's free with the caller that passed `s`, while the
            // caller receiving this return binds-and-frees the value we
            // hand back — return a deep copy so each frees its own
            // buffer (the alias double-freed: k22n repro, 2026-06-06).
            // Mirrors the explicit-`return` arm in `compile_expr`.
            if let (Some(final_expr), Some(v)) = (func.body.final_expr.as_deref(), result) {
                result = Some(self.maybe_defensive_copy_param_arg(final_expr, v));
            }
            // Contract `ensures` checks at the tail return (design.md
            // § Contracts), with `result` bound to the tail value — before
            // scope cleanup, so the postcondition sees live params / result.
            self.emit_ensures_checks(result)?;
            // Struct/impl `invariant` checks at the tail return (rule 3),
            // with `self` bound to the (possibly mutated) receiver — same
            // exit point as `ensures`, inert for non-method functions. For a
            // constructor, `result` is bound as `self` (it has no receiver).
            self.emit_invariant_checks(result)?;

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
                // B-2026-06-19: a non-unit coroutine carries its tail value to
                // the inline-drive caller through the completion slot (same as
                // an explicit `return v`); unit returns store nothing.
                if let Some(val) = result {
                    self.emit_coro_return_value_store(val);
                }
                self.builder
                    .build_unconditional_branch(ctx.coro_return_bb)
                    .unwrap();
            } else if func.name == "main" {
                // `main() -> Result[(), E]`: adapt the tail Result value to a
                // process exit code (Ok→0, Err→print+1) rather than discarding
                // it and returning 0 — B-2026-06-12-9. A plain `fn main()`
                // (no Result return) keeps the unconditional `ret i32 0`.
                if self.main_result_err_te.is_some() {
                    if let Some(val) = result {
                        self.emit_main_result_return(val);
                    } else {
                        let zero = self.context.i32_type().const_int(0, false);
                        self.builder.build_return(Some(&zero)).unwrap();
                    }
                } else if self.main_returns_exitcode {
                    // `fn main() -> ExitCode`: the tail value IS the exit
                    // code (ExitCode is transparently i32). Coerce to the
                    // `i32` C-entry signature and `ret` it — Slice B. A
                    // bodiless tail (every path interior-`return`s) keeps
                    // the safety `ret i32 0`.
                    if let Some(val) = result {
                        let val = self.coerce_to_current_ret_type(val);
                        self.builder.build_return(Some(&val)).unwrap();
                    } else {
                        let zero = self.context.i32_type().const_int(0, false);
                        self.builder.build_return(Some(&zero)).unwrap();
                    }
                } else {
                    let zero = self.context.i32_type().const_int(0, false);
                    self.builder.build_return(Some(&zero)).unwrap();
                }
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
                // Borrow return (`-> ref T`): emit the ADDRESS of the
                // tail borrow source, not the materialized `val` (which
                // would be `ret {ptr,i64,i64}/ptr` etc. — B-2026-06-07-5).
                // The already-compiled `val` is a pure, dead load for the
                // admitted shapes (ref-param identifier / field-of-ref-param).
                //
                // Chained borrow return (tail `echo(t)`): `val` IS already the
                // borrow `ptr` — `compile_tail_final_expr` compiled the call
                // once with the direct-use gate bypassed. Return it directly;
                // re-deriving via `compile_ref_return_ptr` would emit the call
                // a second time (wrong for any effectful callee).
                let tail_is_borrow_call = self.current_fn_returns_ref
                    && func
                        .body
                        .final_expr
                        .as_deref()
                        .is_some_and(|e| self.is_borrow_returning_call_expr(e));
                let ref_ret_ptr = if !self.current_fn_returns_ref {
                    None
                } else if tail_is_borrow_call {
                    Some(val.into_pointer_value())
                } else {
                    func.body
                        .final_expr
                        .as_deref()
                        .and_then(|e| self.compile_ref_return_ptr(e))
                };
                if let Some(sret_ptr) = self.current_fn_sret_param {
                    // AArch64 `sret` return (Slice 3b): store the struct value
                    // through the caller's result pointer and `ret void`. Checked
                    // BEFORE `fn_returns_void` — the LLVM signature IS void here,
                    // but the struct must be stored through x8 first, so the bare
                    // `ret void` path would silently drop the result.
                    self.builder.build_store(sret_ptr, val).unwrap();
                    self.builder.build_return(None).unwrap();
                } else if fn_returns_void {
                    self.builder.build_return(None).unwrap();
                } else if let Some(ptr) = ref_ret_ptr {
                    self.builder.build_return(Some(&ptr)).unwrap();
                } else if self.current_fn_boxes_return {
                    // C-ABI auto-boxed aggregate return (Slice 4 Path B).
                    let boxed = self.box_return_value(val);
                    self.builder.build_return(Some(&boxed)).unwrap();
                } else if let Some(coerced_ty) = self.current_fn_arm64_return_coercion {
                    // AArch64 `#[repr(C)]` struct-by-value return (Slice 2):
                    // reinterpret the struct value as the AAPCS register type.
                    let coerced = self.reinterpret_value_as(val, coerced_ty);
                    self.builder.build_return(Some(&coerced)).unwrap();
                } else if self.current_fn_ret_is_niche() {
                    // Niche-ABI return: pack the conventional 4-i64
                    // Option value into the single nullable ptr the
                    // signature declares. Tag-aware select — a `None`
                    // tail must yield null even though its w0 is undef.
                    let packed = self.option_value_to_niche_ptr(val);
                    self.builder.build_return(Some(&packed)).unwrap();
                } else {
                    // Scalar width coercion at the tail-ret boundary —
                    // mirrors the explicit-`return` site in `exprs.rs`
                    // (`fn f() -> i32 { 0 }` would otherwise emit
                    // `ret i64 0`). See `coerce_scalar_to_type`.
                    let val = self.coerce_to_current_ret_type(val);
                    self.builder.build_return(Some(&val)).unwrap();
                }
            } else {
                // No tail value. For a void function this is the normal
                // `ret void`. For a VALUE-returning function, reaching
                // here means the body's tail produced nothing — e.g. the
                // final expression is a `loop { … return v; … }` whose
                // every exit is an interior `return` (kata-22
                // closure_number, 2026-06-06). The typechecker has
                // already proven all paths return a value, so this tail
                // is dead code: emit `unreachable` instead of the
                // type-mismatched `ret void` that fails module
                // verification ("Function return type does not match
                // operand type of return inst").
                let fn_returns_void = self
                    .current_fn
                    .and_then(|f| f.get_type().get_return_type())
                    .is_none();
                if fn_returns_void {
                    self.builder.build_return(None).unwrap();
                } else {
                    self.builder.build_unreachable().unwrap();
                }
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
        self.current_contract_result_type = None;
        self.contract_old_snapshots.clear();
        self.current_method_invariants.clear();
        self.constructor_invariant_self_type = None;
        Ok(())
    }

    /// `main() -> Result[(), E]` value-return adaptation (B-2026-06-12-9).
    /// `result_val` is the Result `{tag, …}` aggregate produced by the tail
    /// expression or an explicit `return`. `main`'s LLVM signature is the C
    /// entry `i32`, so we branch on the tag rather than `ret`-ing the
    /// aggregate: `Ok` (tag 1) exits 0; `Err` (tag 0) reconstructs E from the
    /// payload words and routes to `emit_main_result_err_exit` (prints
    /// `Error: {e}\n` to stderr, exits 1). Per design.md § Entry Point.
    /// Terminates the current block (both arms `ret`).
    pub(super) fn emit_main_result_return(&mut self, result_val: BasicValueEnum<'ctx>) {
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let sv = result_val.into_struct_value();
        let st = sv.get_type();
        let tag = self
            .builder
            .build_extract_value(sv, 0, "main_res_tag")
            .unwrap()
            .into_int_value();
        let fn_val = self.current_fn.unwrap();
        let ok_bb = self.context.append_basic_block(fn_val, "main_res_ok");
        let err_bb = self.context.append_basic_block(fn_val, "main_res_err");
        // Ok tag == 1 (Err == 0); see `declarations.rs` Result tag assignment.
        let is_ok = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                tag,
                i64_t.const_int(1, false),
                "main_is_ok",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)
            .unwrap();

        // Ok → exit 0.
        self.builder.position_at_end(ok_bb);
        self.builder
            .build_return(Some(&i32_t.const_int(0, false)))
            .unwrap();

        // Err → reconstruct the source-typed error value from the aggregate's
        // payload words (w0/w1/w2 at fields 1/2/3, synthesizing 0 past the
        // struct's field count) and exit 1 after printing it.
        self.builder.position_at_end(err_bb);
        let n = st.count_fields();
        let zero = i64_t.const_int(0, false);
        let extract_word = |s: &Self, idx: u32, name: &str| {
            s.builder
                .build_extract_value(sv, idx, name)
                .unwrap()
                .into_int_value()
        };
        let w0 = if n >= 2 {
            extract_word(self, 1, "main_res_w0")
        } else {
            zero
        };
        let w1 = if n >= 3 {
            extract_word(self, 2, "main_res_w1")
        } else {
            zero
        };
        let w2 = if n >= 4 {
            extract_word(self, 3, "main_res_w2")
        } else {
            zero
        };
        let err_val = match self.main_result_err_te.clone() {
            Some(te) => {
                let e_ty = self.llvm_type_for_type_expr(&te);
                self.rebuild_value_from_payload_words(e_ty, w0, w1, w2)
                    .unwrap_or_else(|_| w0.into())
            }
            None => w0.into(),
        };
        self.emit_main_result_err_exit(err_val);
    }

    /// Emit the `main() -> Result` error exit: print `Error: {e}\n` to stderr
    /// (E rendered via its `Display`) then `ret i32 1` (B-2026-06-12-9,
    /// design.md § Entry Point error display format). `err_val` is the
    /// already-reconstructed source-typed error. Terminates the current block.
    ///
    /// Rendering reuses the *expression-driven* f-string Display path: the
    /// reconstructed error is spilled to a stack slot, registered as a private
    /// synthetic local via `register_var_from_type_expr` (so the side-tables
    /// the f-string renderer consults — `var_type_names`, the collection/String
    /// elem maps — see it), then `f"Error: {e}\n"` is compiled. That path
    /// already handles primitives, `String`, collections, user structs, and
    /// all-unit enums; the value-driven `emit_display_fn_for_type_expr` does
    /// NOT cover user struct/enum types yet (Display-floor subtask 5), so the
    /// bridge is what makes a user error type render here.
    /// Box a `pub extern "C"` export's aggregate return value on the heap
    /// and return the pointer (additive-interop Slice 4 Path B). `malloc`s
    /// `sizeof(val)`, stores the value, and yields the box pointer — a
    /// scalar return the C ABI passes in `rax`, unlike the multi-register
    /// `{data,len,cap}` return that mismatches SysV. The value was moved
    /// out of the body at the return (no Kāra drop), so ownership transfers
    /// cleanly to C, which frees it via the auto-emitted `karac_free_<name>`.
    /// Reinterpret `val`'s bytes as type `ty` via a stack round-trip (store the
    /// value, load `ty` from the same slot). Used for the AArch64 struct-return
    /// coercion (B-2026-07-09-2 Slice 2): a `#[repr(C)]` struct value is
    /// reinterpreted as its AAPCS register type (`i64` / `[2 x i64]`) at the
    /// return site. The slot is sized to `ty`, which is ≥ the struct size for
    /// every coercion (a ≤ 8 B struct widens to `i64`), so the store never
    /// overruns; any surplus high bits are unspecified, as AAPCS permits.
    pub(super) fn reinterpret_value_as(
        &mut self,
        val: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let fn_val = self.current_fn.unwrap();
        let tmp = self.create_entry_alloca(fn_val, "kabi.ret", ty);
        self.builder.build_store(tmp, val).unwrap();
        self.builder.build_load(ty, tmp, "kabi.retload").unwrap()
    }

    pub(super) fn box_return_value(&mut self, val: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        // Size the box from the value's own type. For the Path-B Vec/String
        // returns this is the `{data,len,cap}` `vec_struct_type` (24 B); for a
        // Slice-2a tagged-union `#[repr(C)]` enum it is the `{ i64 tag, i64 w0,
        // … }` enum struct. Both are struct values — size by `size_of` on the
        // concrete type. A non-struct (defensive) falls back to the vec shape.
        let raw_size = match val {
            BasicValueEnum::StructValue(sv) => sv
                .get_type()
                .size_of()
                .expect("boxed return struct is sized"),
            _ => self
                .vec_struct_type()
                .size_of()
                .expect("vec struct is sized"),
        };
        let size = if raw_size.get_type().get_bit_width() == 64 {
            raw_size
        } else {
            self.builder
                .build_int_z_extend(raw_size, i64_t, "kbox.sz64")
                .unwrap()
        };
        let box_ptr = self
            .builder
            .build_call(self.malloc_fn, &[size.into()], "kret.box")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder.build_store(box_ptr, val).unwrap();
        box_ptr.into()
    }

    pub(super) fn emit_main_result_err_exit(&mut self, err_val: BasicValueEnum<'ctx>) {
        use crate::ast::ParsedInterpolationPart as P;
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();

        // Spill + register the error as a synthetic local. The name is private
        // (never user-visible) and the per-function side-table clear at the
        // next `compile_function` entry removes it.
        let synth = "__kara_main_err";
        let fn_val = self.current_fn.unwrap();
        let slot = self.create_entry_alloca(fn_val, synth, err_val.get_type());
        self.builder.build_store(slot, err_val).unwrap();
        self.variables.insert(
            synth.to_string(),
            VarSlot {
                ptr: slot,
                ty: err_val.get_type(),
            },
        );
        if let Some(te) = self.main_result_err_te.clone() {
            self.register_var_from_type_expr(synth, &te);
        }

        // Compile `f"Error: {e}\n"` → owning String, write to stderr, free it.
        // The f-string registers a scope-exit free for its buffer, but the
        // function's cleanup drain has already run on this error path (the `?`
        // / tail / explicit-return sites cleaned up before dispatching here)
        // and we `ret` immediately below, so that registration never fires —
        // the manual free here is the sole, correct release.
        let id_expr = Expr {
            kind: ExprKind::Identifier(synth.to_string()),
            span: crate::token::Span::default(),
        };
        let lit = Expr {
            kind: ExprKind::InterpolatedStringLit(vec![
                P::Text("Error: ".to_string()),
                P::Expr(Box::new(id_expr)),
                P::Text("\n".to_string()),
            ]),
            span: crate::token::Span::default(),
        };
        match self.compile_expr(&lit) {
            Ok(sval) => self.emit_write_and_free_string(sval, "", true),
            Err(_) => {
                // Display unsupported for this E (e.g. a data-carrying enum,
                // still subtask-5 territory): emit the bare prefix + newline so
                // the exit is still observable and well-formed.
                let prefix = self
                    .builder
                    .build_global_string_ptr("Error: \n", "main_err_prefix")
                    .unwrap();
                self.emit_nul_safe_write(
                    prefix.as_pointer_value(),
                    i64_t.const_int(8, false),
                    "",
                    true,
                );
            }
        }
        self.builder
            .build_return(Some(&i32_t.const_int(1, false)))
            .unwrap();
    }
}
