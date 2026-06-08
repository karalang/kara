//! Component Model **Canonical ABI** export surface (phase-10 "WASM
//! entry-point discovery", sub-slice D).
//!
//! On a `--bindings component` build, each discovered WASM entry-point
//! export must present a core export whose name + ABI match the embedded
//! WIT world (`crate::wit::render_embed_wit`), so `wasm-tools component
//! new` can lift it:
//!
//!   - **Scalar exports** (primitives / opaque handles) lower 1:1 to the
//!     canonical ABI, so we only fix the export *name*: attach a
//!     `wasm-export-name` (kebab) attribute to the real function so the
//!     core export matches the kebab WIT function name (`add_two` ⇒
//!     `add-two`).
//!   - **Record-returning exports** (a user struct of scalar fields) need
//!     a tiny trampoline: the Kāra function returns the aggregate by
//!     value (LLVM lowers that to an `sret` out-param on wasm), but the
//!     canonical ABI for a flattened-result type returns a single `i32`
//!     pointer to a return area. The trampoline calls the real function,
//!     stores the result into a static return area, and returns its
//!     address. The Kāra struct layout (natural alignment, declaration
//!     order) coincides with the canonical `record` layout for scalar
//!     fields, so no field-by-field repack is needed.
//!
//! Only the surface [`crate::wasm_exports::ExportSig::component_lowerable`]
//! reports is handled here; record *params*, `option`/`result`,
//! `string`/`list` are later steps (their WIT is likewise withheld until
//! the matching lowering lands, so the WIT never names a core export
//! that does not exist).

use inkwell::attributes::AttributeLoc;
use inkwell::module::Linkage;
use inkwell::values::BasicValue;

use crate::ast::Program;

impl<'ctx> super::Codegen<'ctx> {
    /// Emit the component export surface (see module docs). No-op unless
    /// this is a wasm **component** build (signalled by
    /// [`crate::target::wasm_component_host_package`]).
    pub(super) fn emit_wasm_component_export_surface(
        &mut self,
        program: &Program,
    ) -> Result<(), String> {
        if !crate::target::active_target_is_wasm()
            || crate::target::wasm_component_host_package().is_none()
        {
            return Ok(());
        }
        let target = crate::target::active_target();
        let exports = crate::wasm_exports::collect_wasm_exports(program, target);
        for e in exports.iter().filter(|e| e.component_lowerable()) {
            let kebab = crate::wit::host_import_name(&e.name);
            if e.needs_trampoline() {
                self.emit_export_trampoline(e, &kebab)?;
            } else if kebab != e.name {
                // Pure-scalar export: rename the core export to the kebab
                // WIT name (the real function IS the canonical export).
                if let Some(f) = self.module.get_function(&e.name) {
                    f.add_attribute(
                        AttributeLoc::Function,
                        self.context
                            .create_string_attribute("wasm-export-name", &kebab),
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
            } else {
                canon_params.push(real_params[i]);
                plans.push(Plan::Scalar);
            }
        }

        // Canonical return: record ⇒ i32 (return-area pointer); scalar ⇒
        // the real return type; unit ⇒ void.
        let ret_is_record = e.ret.as_ref().is_some_and(|r| r.is_record());
        let real_ret = real.get_type().get_return_type();
        let fn_ty = if ret_is_record {
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
}
