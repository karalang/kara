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
                self.emit_record_return_trampoline(e, &kebab)?;
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

    /// Emit the canonical-ABI trampoline for an export with scalar params
    /// and a flat-record return (`fn make_point(x: f64, y: f64) -> Point`).
    /// The trampoline is the component export (named `kebab`); the real
    /// function stays internal-facing (still emitted under its bare name,
    /// reachable for any direct caller, but the canonical export the
    /// component world declares is this trampoline).
    fn emit_record_return_trampoline(
        &mut self,
        e: &crate::wasm_exports::ExportSig,
        kebab: &str,
    ) -> Result<(), String> {
        let Some(real) = self.module.get_function(&e.name) else {
            return Ok(());
        };
        let ret = e
            .ret
            .as_ref()
            .expect("needs_trampoline implies a record return");
        let struct_ty = *self.struct_types.get(&ret.kara_ty).ok_or_else(|| {
            format!(
                "wasm export trampoline: struct `{}` (return of `{}`) has no layout",
                ret.kara_ty, e.name
            )
        })?;

        // Trampoline signature: real function's params verbatim (all
        // scalar in this step), returning an i32 return-area pointer.
        let param_types = real.get_type().get_param_types();
        let i32_ty = self.context.i32_type();
        let fn_ty = i32_ty.fn_type(&param_types, false);
        let tramp = self
            .module
            .add_function(kebab, fn_ty, Some(Linkage::External));
        tramp.add_attribute(
            AttributeLoc::Function,
            self.context
                .create_string_attribute("wasm-export-name", kebab),
        );

        // Static return area: a zero-initialized internal global sized to
        // the struct's ABI size. The canonical ABI reads it immediately
        // after the call (and calls `cabi_post_*` — which we leave
        // implicit; a static area needs no free), so a single buffer is
        // safe for the WASI-command, non-reentrant export surface.
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
        // The canonical ABI requires the return-area pointer aligned to
        // the record's alignment (an `[N x i8]` global is align-1 by
        // default, which traps as "return pointer not aligned").
        area.set_alignment(align);

        let entry = self.context.append_basic_block(tramp, "entry");
        self.builder.position_at_end(entry);
        let args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
            tramp.get_param_iter().map(|p| p.into()).collect();
        let result = self
            .builder
            .build_call(real, &args, "call_real")
            .map_err(|e| format!("wasm export trampoline call: {e}"))?
            .try_as_basic_value()
            .unwrap_basic();
        let area_ptr = area.as_pointer_value();
        self.builder
            .build_store(area_ptr, result)
            .map_err(|e| format!("wasm export trampoline store: {e}"))?;
        let addr = self
            .builder
            .build_ptr_to_int(area_ptr, i32_ty, "ret_addr")
            .map_err(|e| format!("wasm export trampoline ptrtoint: {e}"))?;
        self.builder
            .build_return(Some(&addr.as_basic_value_enum()))
            .map_err(|e| format!("wasm export trampoline ret: {e}"))?;
        Ok(())
    }
}
