//! Component Model **Canonical ABI** export surface (phase-10 "WASM
//! entry-point discovery", sub-slice D). Emitted for any wasm build that
//! marshals rich exports (`--bindings browser` or `component`, signalled
//! by [`crate::target::wasm_export_marshalling`]); `--bindings none`
//! keeps raw core exports.
//!
//! Each discovered WASM entry-point export presents a core export whose
//! name + ABI match the canonical layout. A **component** build names it
//! by the kebab WIT name so `wasm-tools component new` lifts it from the
//! embedded WIT world (`crate::wit::render_embed_wit`); a **browser**
//! build keeps the bare Kāra name and the generated JS glue
//! (`wasm_glue::karaBuildExports`) marshals JS objects against the *same*
//! canonical layout. Either way:
//!
//!   - **Scalar exports** (primitives / opaque handles) lower 1:1 to the
//!     canonical ABI, so we only fix the export *name*: attach a
//!     `wasm-export-name` (kebab) attribute to the real function so the
//!     core export matches the kebab WIT function name (`add_two` ⇒
//!     `add-two`).
//!   - **Record params/returns** (a user struct of scalar fields) need a
//!     trampoline: a record param flattens to its scalar fields, which the
//!     trampoline reconstructs into the struct the Kāra fn expects by
//!     value; a record return is written to an alignment-correct static
//!     return area whose `i32` pointer the trampoline returns (the Kāra
//!     struct layout coincides with the canonical `record` layout for
//!     scalar fields, so no field repack is needed).
//!   - **`Option`/`Result` returns** over scalar inners lower to the
//!     canonical `option`/`result` return area: discriminant remapped to
//!     the canonical case order, payload bytes copied from the enum's
//!     word 0 (see [`Self::emit_variant_return_area`]).
//!   - **`String` / `Vec[T]` params/returns** share the Kāra
//!     `{ptr, len, cap}` repr and the canonical `(ptr, len)` slice ABI: a
//!     param lifts the host-allocated `(ptr, len)` into the owned Kāra
//!     value, a return lowers it back to a `(ptr, len)` return area (see
//!     [`Self::emit_string_return_area`]). The host allocates param bytes
//!     through the exported `cabi_realloc` (`runtime/wasm_alloc.rs`).
//!
//! Only the surface [`crate::wasm_exports::ExportSig::component_lowerable`]
//! reports is handled here; variant *params* and nested aggregates are
//! later steps (their WIT is likewise withheld until the matching lowering
//! lands, so the WIT never names a core export that does not exist).

use inkwell::attributes::AttributeLoc;
use inkwell::module::Linkage;
use inkwell::values::BasicValue;

use crate::ast::Program;

impl<'ctx> super::Codegen<'ctx> {
    /// Emit the WASM export surface (see module docs). No-op unless this
    /// is a wasm build that marshals rich exports — `--bindings browser`
    /// or `--bindings component` (signalled by
    /// [`crate::target::wasm_export_marshalling`]); `--bindings none`
    /// keeps raw core exports.
    ///
    /// The export *name* differs by binding: a **component** build
    /// (`wasm_component_host_package().is_some()`) uses the kebab WIT name;
    /// a **browser** build keeps the bare Kāra name (a valid JS identifier
    /// the `.d.ts` / glue reference). Either way a record/variant/slice
    /// export gets a canonical trampoline (the browser glue marshals JS
    /// objects against the same canonical layout the component WIT
    /// describes); a pure-scalar export is the real function directly
    /// (renamed via attribute only when the component kebab differs).
    pub(super) fn emit_wasm_component_export_surface(
        &mut self,
        program: &Program,
    ) -> Result<(), String> {
        if !crate::target::active_target_is_wasm() || !crate::target::wasm_export_marshalling() {
            return Ok(());
        }
        let component = crate::target::wasm_component_host_package().is_some();
        let target = crate::target::active_target();
        let exports = crate::wasm_exports::collect_wasm_exports(program, target);
        for e in exports.iter().filter(|e| e.component_lowerable()) {
            let export_name = if component {
                crate::wit::host_import_name(&e.name)
            } else {
                e.name.clone()
            };
            if e.needs_trampoline() {
                self.emit_export_trampoline(e, &export_name)?;
            } else if export_name != e.name {
                // Pure-scalar component export: rename the core export to
                // the kebab WIT name (the real function IS the canonical
                // export). Browser scalars keep their bare name (no-op).
                if let Some(f) = self.module.get_function(&e.name) {
                    f.add_attribute(
                        AttributeLoc::Function,
                        self.context
                            .create_string_attribute("wasm-export-name", &export_name),
                    );
                }
            }
        }
        Ok(())
    }

    /// Emit the canonical-ABI trampoline for an export with flat-record
    /// params and/or a flat-record return (`fn area(r: Rect) -> f64`,
    /// `fn make_point(x: f64, y: f64) -> Point`, …). The trampoline is the
    /// component export (named `kebab`); the real Kāra function stays
    /// under its bare name (reachable for any direct caller).
    ///
    /// Canonical lowering for the supported surface:
    ///   - a **record param** flattens to its scalar fields in
    ///     declaration order; the trampoline takes those flats and
    ///     reconstructs the LLVM struct (`insertvalue`) the real function
    ///     expects by value;
    ///   - a **scalar param** passes through unchanged;
    ///   - a **record return** is written to an alignment-correct static
    ///     return area whose pointer the trampoline returns as `i32`;
    ///   - a **scalar return** is returned directly; a unit return is void.
    fn emit_export_trampoline(
        &mut self,
        e: &crate::wasm_exports::ExportSig,
        kebab: &str,
    ) -> Result<(), String> {
        let Some(real) = self.module.get_function(&e.name) else {
            return Ok(());
        };
        let real_params = real.get_type().get_param_types();
        let i32_ty = self.context.i32_type();

        // Walk export params, building (a) the trampoline's canonical
        // param types and (b) a reconstruction plan per real-fn argument.
        // `Plan::Scalar` consumes one trampoline param; `Plan::Record`
        // consumes one per field and rebuilds the struct.
        enum Plan<'c> {
            Scalar,
            Record(inkwell::types::StructType<'c>, usize),
            /// `String` param: canonical `(ptr i32, len i32)` flats are
            /// reconstructed into the Kāra `{ptr, len, cap}` struct (the
            /// real fn's param type); the guest owns the host-allocated
            /// bytes (`cap = len`).
            StringArg(inkwell::types::StructType<'c>),
        }
        let mut canon_params: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = Vec::new();
        let mut plans: Vec<Plan<'ctx>> = Vec::new();
        for (i, p) in e.params.iter().enumerate() {
            if p.ty.is_record() {
                let st = *self.struct_types.get(&p.ty.kara_ty).ok_or_else(|| {
                    format!(
                        "wasm export trampoline: struct `{}` (param of `{}`) has no layout",
                        p.ty.kara_ty, e.name
                    )
                })?;
                let n = st.count_fields() as usize;
                for fi in 0..n {
                    canon_params.push(st.get_field_type_at_index(fi as u32).unwrap().into());
                }
                plans.push(Plan::Record(st, n));
            } else if p.ty.is_slice_like() {
                // Kāra `String` / `Vec[T]` is passed by value as
                // `{ptr, len, cap}` — the real fn's param type here. Both
                // lower from a canonical `(ptr i32, len i32)` slice.
                let st = real_params[i].into_struct_type();
                canon_params.push(i32_ty.into()); // ptr
                canon_params.push(i32_ty.into()); // len (elem count)
                plans.push(Plan::StringArg(st));
            } else {
                canon_params.push(real_params[i]);
                plans.push(Plan::Scalar);
            }
        }

        // Canonical return: record / variant / string ⇒ i32 (return-area
        // pointer); scalar ⇒ the real return type; unit ⇒ void.
        let ret_is_record = e.ret.as_ref().is_some_and(|r| r.is_record());
        let ret_is_variant = e.ret.as_ref().is_some_and(|r| r.is_variant());
        let ret_is_slice = e.ret.as_ref().is_some_and(|r| r.is_slice_like());
        let ret_via_area = ret_is_record || ret_is_variant || ret_is_slice;
        let real_ret = real.get_type().get_return_type();
        let fn_ty = if ret_via_area {
            i32_ty.fn_type(&canon_params, false)
        } else {
            match real_ret {
                Some(inkwell::types::BasicTypeEnum::IntType(t)) => t.fn_type(&canon_params, false),
                Some(inkwell::types::BasicTypeEnum::FloatType(t)) => {
                    t.fn_type(&canon_params, false)
                }
                Some(inkwell::types::BasicTypeEnum::PointerType(t)) => {
                    t.fn_type(&canon_params, false)
                }
                Some(inkwell::types::BasicTypeEnum::StructType(t)) => {
                    t.fn_type(&canon_params, false)
                }
                Some(inkwell::types::BasicTypeEnum::ArrayType(t)) => {
                    t.fn_type(&canon_params, false)
                }
                Some(inkwell::types::BasicTypeEnum::VectorType(t)) => {
                    t.fn_type(&canon_params, false)
                }
                Some(inkwell::types::BasicTypeEnum::ScalableVectorType(t)) => {
                    t.fn_type(&canon_params, false)
                }
                None => self.context.void_type().fn_type(&canon_params, false),
            }
        };

        // Distinct symbol (the real function may already own the kebab
        // name when a name equals its kebab form, e.g. `area`); the
        // trampoline is surfaced under the kebab WIT name via the
        // `wasm-export-name` attribute, and the link step
        // (`link_export_names`) `--export`s this same symbol.
        let tramp_symbol = crate::wasm_exports::export_trampoline_symbol(&e.name);
        let tramp = self
            .module
            .add_function(&tramp_symbol, fn_ty, Some(Linkage::External));
        tramp.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-export-name", kebab),
        );
        let entry = self.context.append_basic_block(tramp, "entry");
        self.builder.position_at_end(entry);

        // Reconstruct the real-fn arguments from the trampoline's params.
        let mut k = 0u32;
        let mut next = || {
            let p = tramp.get_nth_param(k).unwrap();
            k += 1;
            p
        };
        let mut args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();
        for plan in &plans {
            match plan {
                Plan::Scalar => args.push(next().into()),
                Plan::Record(st, n) => {
                    let mut agg = st.get_undef();
                    for fi in 0..*n {
                        let v = next();
                        agg = self
                            .builder
                            .build_insert_value(agg, v, fi as u32, "fld")
                            .map_err(|e| format!("wasm export trampoline insert: {e}"))?
                            .into_struct_value();
                    }
                    args.push(agg.into());
                }
                Plan::StringArg(st) => {
                    let ptr_i32 = next().into_int_value();
                    let len_i32 = next().into_int_value();
                    let ptr = self
                        .builder
                        .build_int_to_ptr(
                            ptr_i32,
                            self.context.ptr_type(inkwell::AddressSpace::default()),
                            "str_ptr",
                        )
                        .map_err(|e| format!("wasm export trampoline str ptr: {e}"))?;
                    let len64 = self
                        .builder
                        .build_int_z_extend(len_i32, self.context.i64_type(), "str_len")
                        .map_err(|e| format!("wasm export trampoline str len: {e}"))?;
                    // Kāra String `{ ptr, len, cap }`; the guest takes
                    // ownership of the host-allocated buffer, cap = len.
                    let mut agg = st.get_undef();
                    for (idx, v) in [
                        ptr.as_basic_value_enum(),
                        len64.as_basic_value_enum(),
                        len64.as_basic_value_enum(),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        agg = self
                            .builder
                            .build_insert_value(agg, v, idx as u32, "strfld")
                            .map_err(|e| format!("wasm export trampoline str insert: {e}"))?
                            .into_struct_value();
                    }
                    args.push(agg.into());
                }
            }
        }

        let call = self
            .builder
            .build_call(real, &args, "call_real")
            .map_err(|e| format!("wasm export trampoline call: {e}"))?;

        if ret_is_record {
            let ret = e.ret.as_ref().unwrap();
            let struct_ty = *self.struct_types.get(&ret.kara_ty).ok_or_else(|| {
                format!(
                    "wasm export trampoline: struct `{}` (return of `{}`) has no layout",
                    ret.kara_ty, e.name
                )
            })?;
            // Alignment-correct static return area (an `[N x i8]` global is
            // align-1 by default, which traps as "return pointer not
            // aligned"). The canonical ABI reads it right after the call
            // (a `cabi_post_*` free is unnecessary for a static buffer),
            // so a single buffer is safe for the non-reentrant WASI-command
            // export surface.
            let (size, align) = {
                let td = self
                    .ensure_target_data()
                    .map_err(|e| format!("wasm export trampoline: {e}"))?;
                (
                    td.get_abi_size(&struct_ty),
                    td.get_abi_alignment(&struct_ty),
                )
            };
            let area_ty = self.context.i8_type().array_type(size as u32);
            let area = self
                .module
                .add_global(area_ty, None, &format!("__kara_ret_{kebab}"));
            area.set_linkage(Linkage::Internal);
            area.set_initializer(&area_ty.const_zero());
            area.set_alignment(align);
            let area_ptr = area.as_pointer_value();
            self.builder
                .build_store(area_ptr, call.try_as_basic_value().unwrap_basic())
                .map_err(|e| format!("wasm export trampoline store: {e}"))?;
            let addr = self
                .builder
                .build_ptr_to_int(area_ptr, i32_ty, "ret_addr")
                .map_err(|e| format!("wasm export trampoline ptrtoint: {e}"))?;
            self.builder
                .build_return(Some(&addr.as_basic_value_enum()))
                .map_err(|e| format!("wasm export trampoline ret: {e}"))?;
        } else if ret_is_variant {
            let shape = e.ret.as_ref().unwrap().variant.as_ref().unwrap();
            let addr = self.emit_variant_return_area(
                call.try_as_basic_value().unwrap_basic(),
                shape,
                kebab,
            )?;
            self.builder
                .build_return(Some(&addr.as_basic_value_enum()))
                .map_err(|e| format!("wasm export trampoline ret: {e}"))?;
        } else if ret_is_slice {
            // `String` / `Vec[T]` share the `{ptr, len, cap}` repr; both
            // lower to a canonical `(ptr, len)` return area.
            let addr =
                self.emit_string_return_area(call.try_as_basic_value().unwrap_basic(), kebab)?;
            self.builder
                .build_return(Some(&addr.as_basic_value_enum()))
                .map_err(|e| format!("wasm export trampoline ret: {e}"))?;
        } else if real_ret.is_some() {
            let v = call.try_as_basic_value().unwrap_basic();
            self.builder
                .build_return(Some(&v))
                .map_err(|e| format!("wasm export trampoline ret: {e}"))?;
        } else {
            self.builder
                .build_return(None)
                .map_err(|e| format!("wasm export trampoline ret: {e}"))?;
        }
        Ok(())
    }

    /// Lower a returned Kāra `Option`/`Result` enum value into the
    /// canonical-ABI `option`/`result` return area, and return the area's
    /// `i32` address.
    ///
    /// Kāra lays an enum out as `{ i64 tag, i64 w0, … }` with the scalar
    /// payload's raw bits in word 0 (an `i32` in the low half, an `f64`
    /// bit-cast to `i64`, etc.). The canonical variant lays the payload as
    /// those same little-endian bytes, with the discriminant selecting how
    /// the lifter reads them — so we can store `tag → u8 discriminant` and
    /// `w0 → payload (truncated to the payload byte width)` with **no
    /// per-case branch or bit-cast**; the low bytes are always the active
    /// case's value, and the lifter ignores the rest.
    fn emit_variant_return_area(
        &mut self,
        enum_val: inkwell::values::BasicValueEnum<'ctx>,
        shape: &crate::wasm_exports::VariantShape,
        kebab: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        use crate::wasm_exports::{scalar_size_align, variant_layout, VariantShape};
        let (payload_bytes, payload_align) = match shape {
            VariantShape::Option(t) => scalar_size_align(&t.kara_ty),
            VariantShape::Result(t, e) => {
                let (ts, ta) = scalar_size_align(&t.kara_ty);
                let (es, ea) = scalar_size_align(&e.kara_ty);
                (ts.max(es), ta.max(ea))
            }
        };
        // Discriminant is one byte (≤256 cases); payload follows at its
        // own alignment; the area is sized/aligned to hold both.
        let (payload_off, total, variant_align) = variant_layout(payload_bytes, payload_align);

        let area_ty = self.context.i8_type().array_type(total as u32);
        let area = self
            .module
            .add_global(area_ty, None, &format!("__kara_ret_{kebab}"));
        area.set_linkage(Linkage::Internal);
        area.set_initializer(&area_ty.const_zero());
        area.set_alignment(variant_align);
        let area_ptr = area.as_pointer_value();

        let sv = enum_val.into_struct_value();
        let tag = self
            .builder
            .build_extract_value(sv, 0, "tag")
            .map_err(|e| format!("wasm export trampoline variant tag: {e}"))?
            .into_int_value();
        let w0 = self
            .builder
            .build_extract_value(sv, 1, "w0")
            .map_err(|e| format!("wasm export trampoline variant payload: {e}"))?
            .into_int_value();

        // Map the Kāra discriminant onto the canonical variant
        // discriminant. Kāra's `Option` is seeded `None=0, Some=1`, which
        // already matches canonical `option` (`none=0, some=1`). Kāra's
        // `Result` is seeded `Err=0, Ok=1` — the REVERSE of canonical
        // `result` (`ok=0, err=1`) — so it remaps as `1 - tag`.
        let canon_tag = match shape {
            VariantShape::Option(_) => tag,
            VariantShape::Result(_, _) => {
                let one = self.context.i64_type().const_int(1, false);
                self.builder
                    .build_int_sub(one, tag, "canon_tag")
                    .map_err(|e| format!("wasm export trampoline disc remap: {e}"))?
            }
        };
        let disc = self
            .builder
            .build_int_truncate(canon_tag, self.context.i8_type(), "disc")
            .map_err(|e| format!("wasm export trampoline disc: {e}"))?;
        self.builder
            .build_store(area_ptr, disc)
            .map_err(|e| format!("wasm export trampoline disc store: {e}"))?;

        // payload (raw low bytes) at payload_off
        let payload_ty = match payload_bytes {
            1 => self.context.i8_type(),
            2 => self.context.i16_type(),
            4 => self.context.i32_type(),
            _ => self.context.i64_type(),
        };
        let payload = if payload_bytes >= 8 {
            w0
        } else {
            self.builder
                .build_int_truncate(w0, payload_ty, "payload")
                .map_err(|e| format!("wasm export trampoline payload trunc: {e}"))?
        };
        let payload_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(
                    self.context.i8_type(),
                    area_ptr,
                    &[self.context.i32_type().const_int(payload_off, false)],
                    "payload_ptr",
                )
                .map_err(|e| format!("wasm export trampoline payload gep: {e}"))?
        };
        self.builder
            .build_store(payload_ptr, payload)
            .map_err(|e| format!("wasm export trampoline payload store: {e}"))?;

        self.builder
            .build_ptr_to_int(area_ptr, self.context.i32_type(), "ret_addr")
            .map_err(|e| format!("wasm export trampoline variant ptrtoint: {e}"))
    }

    /// Lower a returned Kāra `String` (`{ ptr, len, cap }`) into the
    /// canonical-ABI `string` return area `{ i32 ptr, i32 len }` and
    /// return the area's `i32` address. The string bytes already live in
    /// the guest's linear memory (the `String`'s heap buffer), which the
    /// component lifter reads at `ptr`. (`cap` is dropped; a `cabi_post_*`
    /// free hook is a follow-up — a WASI command reads the result once and
    /// exits, so the buffer is reclaimed at instance teardown.)
    fn emit_string_return_area(
        &mut self,
        string_val: inkwell::values::BasicValueEnum<'ctx>,
        kebab: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let i32_ty = self.context.i32_type();
        let sv = string_val.into_struct_value();
        let ptr = self
            .builder
            .build_extract_value(sv, 0, "str_ptr")
            .map_err(|e| format!("wasm export trampoline str ret ptr: {e}"))?
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(sv, 1, "str_len")
            .map_err(|e| format!("wasm export trampoline str ret len: {e}"))?
            .into_int_value();
        let ptr_i32 = self
            .builder
            .build_ptr_to_int(ptr, i32_ty, "str_ptr_i32")
            .map_err(|e| format!("wasm export trampoline str ret ptrtoint: {e}"))?;
        let len_i32 = self
            .builder
            .build_int_truncate(len, i32_ty, "str_len_i32")
            .map_err(|e| format!("wasm export trampoline str ret len trunc: {e}"))?;

        // Return area `{ i32 ptr, i32 len }` — 8 bytes, align 4.
        let area_ty = self.context.i8_type().array_type(8);
        let area = self
            .module
            .add_global(area_ty, None, &format!("__kara_ret_{kebab}"));
        area.set_linkage(Linkage::Internal);
        area.set_initializer(&area_ty.const_zero());
        area.set_alignment(4);
        let area_ptr = area.as_pointer_value();
        self.builder
            .build_store(area_ptr, ptr_i32)
            .map_err(|e| format!("wasm export trampoline str ret store ptr: {e}"))?;
        let len_slot = unsafe {
            self.builder
                .build_in_bounds_gep(
                    self.context.i8_type(),
                    area_ptr,
                    &[i32_ty.const_int(4, false)],
                    "len_slot",
                )
                .map_err(|e| format!("wasm export trampoline str ret gep: {e}"))?
        };
        self.builder
            .build_store(len_slot, len_i32)
            .map_err(|e| format!("wasm export trampoline str ret store len: {e}"))?;
        self.builder
            .build_ptr_to_int(area_ptr, i32_ty, "ret_addr")
            .map_err(|e| format!("wasm export trampoline str ret area ptrtoint: {e}"))
    }
}
