//! `--enable-hot-swap` (deferred.md § Continuous PGO with Shared-Object
//! Hot-Swap, commitment 1): a call of a registered body goes through its
//! slot in `karac_hotswap_table` (`[N x ptr]`, external), which the
//! constructor `__karac_init_hot_swap_table` fills with the body's own
//! function before `main` runs. A v1 binary therefore calls the code it was
//! built with, at the cost of a load and an indirect call per call site;
//! the table is the artifact a later loader rewrites.

use inkwell::attributes::AttributeLoc;
use inkwell::module::Linkage;
use inkwell::values::{BasicMetadataValueEnum, CallSiteValue, FunctionValue};

use super::{Cx, R};

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

/// Function attributes that are inlining advice for the body, not facts
/// about the call, so a call through a slot does not take them.
const BODY_ONLY: [&str; 3] = ["alwaysinline", "inlinehint", "noinline"];

impl<'ctx> Cx<'ctx, '_> {
    /// Declares the table with a slot for each of `names` that is a body
    /// of the program, in order, and schedules each body: a registered body
    /// is compiled whether or not anything calls it, since a replacement
    /// may.
    pub(super) fn hot_swap_init(&mut self, names: &[String]) -> R<()> {
        let program = self.program;
        let mut seen = std::collections::HashSet::new();
        let names: Vec<&String> = names
            .iter()
            .filter(|n| program.bodies.contains_key(n.as_str()) && seen.insert(n.as_str()))
            .collect();
        if names.is_empty() {
            return Ok(());
        }
        let ty = self.ptr_ty().array_type(names.len() as u32);
        let table = self.module.add_global(ty, None, "karac_hotswap_table");
        table.set_linkage(Linkage::External);
        table.set_initializer(&ty.const_zero());
        self.hot_swap_table = Some(table);
        for (slot, name) in names.into_iter().enumerate() {
            // A slot holds a function of the plain convention, so a
            // registered body takes no place (`debug.rs`).
            self.located.insert(name.clone(), false);
            // The function a direct call would take (`call_body`).
            let f = match self.value_function(name)? {
                Some(v) => v,
                None => self.direct_function(name)?,
            };
            self.hot_swap.insert(name.clone(), (slot as u32, f));
        }
        Ok(())
    }

    /// The call of the body `name`, whose function is `f`, with `vals`:
    /// through its slot when it is registered, directly otherwise.
    pub(super) fn call_function(
        &mut self,
        name: &str,
        f: FunctionValue<'ctx>,
        vals: &[BasicMetadataValueEnum<'ctx>],
    ) -> R<CallSiteValue<'ctx>> {
        let (Some(&(slot, _)), Some(table)) = (self.hot_swap.get(name), self.hot_swap_table) else {
            return self.builder.build_call(f, vals, "").map_err(e);
        };
        let n = self.hot_swap.len() as u32;
        let p = unsafe {
            self.builder.build_in_bounds_gep(
                self.ptr_ty().array_type(n),
                table.as_pointer_value(),
                &[self.i64(0), self.i64(u64::from(slot))],
                "",
            )
        }
        .map_err(e)?;
        let callee = self
            .builder
            .build_load(self.ptr_ty(), p, "")
            .map_err(e)?
            .into_pointer_value();
        let call = self
            .builder
            .build_indirect_call(f.get_type(), callee, vals, "")
            .map_err(e)?;
        self.hot_swap_calls.push((call, f));
        Ok(call)
    }

    /// Gives each call through a slot its function's final attributes
    /// (`noalias` is marked after every body is lowered) and calling
    /// convention, which a direct call reads from the callee and an
    /// indirect one cannot, then builds the constructor that fills the
    /// table.
    pub(super) fn hot_swap_finish(&mut self) -> R<()> {
        let Some(table) = self.hot_swap_table else {
            return Ok(());
        };
        let body_only: Vec<u32> = BODY_ONLY
            .iter()
            .map(|n| inkwell::attributes::Attribute::get_named_enum_kind_id(n))
            .collect();
        for &(call, f) in &self.hot_swap_calls {
            call.set_call_convention(f.get_call_conventions());
            for i in 0..f.count_params() {
                for a in f.attributes(AttributeLoc::Param(i)) {
                    call.add_attribute(AttributeLoc::Param(i), a);
                }
            }
            for a in f.attributes(AttributeLoc::Return) {
                call.add_attribute(AttributeLoc::Return, a);
            }
            for a in f.attributes(AttributeLoc::Function) {
                if a.is_enum() && !body_only.contains(&a.get_enum_kind_id()) {
                    call.add_attribute(AttributeLoc::Function, a);
                }
            }
        }

        let ptr = self.ptr_ty();
        let ctor = self.module.add_function(
            "__karac_init_hot_swap_table",
            self.llcx.void_type().fn_type(&[], false),
            Some(Linkage::Internal),
        );
        let entry = self.llcx.append_basic_block(ctor, "entry");
        self.builder.position_at_end(entry);
        self.builder.unset_current_debug_location();
        let ty = ptr.array_type(self.hot_swap.len() as u32);
        let mut slots: Vec<(u32, FunctionValue<'ctx>)> = self.hot_swap.values().copied().collect();
        slots.sort_by_key(|s| s.0);
        for (slot, f) in slots {
            let p = unsafe {
                self.builder.build_in_bounds_gep(
                    ty,
                    table.as_pointer_value(),
                    &[self.i64(0), self.i64(u64::from(slot))],
                    "",
                )
            }
            .map_err(e)?;
            self.builder
                .build_store(p, f.as_global_value().as_pointer_value())
                .map_err(e)?;
        }
        self.builder.build_return(None).map_err(e)?;

        // `{ i32 priority, ptr ctor, ptr data }`, run before `main`.
        let i32t = self.llcx.i32_type();
        let entry_ty = self
            .llcx
            .struct_type(&[i32t.into(), ptr.into(), ptr.into()], false);
        let ctors = entry_ty.const_array(&[entry_ty.const_named_struct(&[
            i32t.const_int(65535, false).into(),
            ctor.as_global_value().as_pointer_value().into(),
            ptr.const_null().into(),
        ])]);
        let g = self
            .module
            .add_global(ctors.get_type(), None, "llvm.global_ctors");
        g.set_linkage(Linkage::Appending);
        g.set_initializer(&ctors);
        Ok(())
    }
}
