//! `std.log`'s natives (runtime/stdlib/std_log.kara; B-2026-10-27-20) over
//! the runtime's `karac_mir_log_*` (runtime/src/mir_log.rs).
//!
//! `log_install` boxes the backend and hands the runtime two functions
//! made for its type at that call: one turns a kept call into the record
//! and gives it to `T.emit`, and one drops the backend. `log_dispatch`
//! hands the runtime the call and the default backend. Reference
//! semantics: `log_native` in src/mir/interp.rs.

use inkwell::values::{BasicMetadataValueEnum, FunctionValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    /// The natives of `std.log`, or `None` for one not lowered here.
    pub(super) fn log_native(
        &mut self,
        name: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        match (name, args) {
            ("log_enabled", [r]) => {
                let r = self.int_arg(r)?;
                let on = self.rt_int("log_enabled", &[r.into()], 64)?;
                let on = self
                    .builder
                    .build_int_compare(IntPredicate::NE, on, self.i64(0), "")
                    .map_err(e)?;
                self.store_bool(dest, on)?;
            }
            ("log_task_id", []) => {
                let id = self.rt_int("log_task_id", &[], 64)?;
                self.store_int_as(dest, id, true)?;
            }
            ("log_install", [backend, rank, record]) => {
                let t = self.operand_type(backend)?;
                if matches!(self.tys.kind(t), TyKind::Ref(_) | TyKind::MutRef(_)) {
                    return Ok(None);
                }
                // The library's own function, erased at the call.
                let _ = record;
                let record = self.log_body("__log_record")?;
                let size = self.layout(t)?.size.max(1);
                let malloc = self.libc_fn("malloc");
                let bx = self
                    .builder
                    .build_call(malloc, &[self.i64(size).into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let vp = self.operand_ptr(backend)?;
                self.copy(bx, vp, t)?;
                let emit = self.log_emit_fn(t, &record)?;
                let drop = self.log_drop_fn(t)?;
                let rank = self.int_arg(rank)?;
                let ptr_of = |f: FunctionValue<'ctx>| f.as_global_value().as_pointer_value();
                self.rt(
                    "log_install",
                    &[
                        bx.into(),
                        ptr_of(emit).into(),
                        ptr_of(drop).into(),
                        rank.into(),
                    ],
                )?;
            }
            ("log_dispatch", [call, default]) => {
                let _ = default;
                let default = self.log_body("__log_default")?;
                let p = self.operand_ptr(call)?;
                let f = self.log_default_fn(&default)?;
                self.rt(
                    "log_dispatch",
                    &[p.into(), f.as_global_value().as_pointer_value().into()],
                )?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// The body of the library function `stem`, under the name the splice
    /// gave it (`__log_record$std_log`).
    fn log_body(&self, stem: &str) -> R<String> {
        self.program
            .bodies
            .keys()
            .find(|k| *k == stem || k.strip_prefix(stem).is_some_and(|r| r.starts_with('$')))
            .cloned()
            .ok_or_else(|| format!("std.log: no body for `{stem}`"))
    }

    /// `void(sink, call)`: turns the call into the record through
    /// `record`, gives it to the backend of type `t` through `t.emit`, and
    /// drops it after a borrowing `emit`.
    fn log_emit_fn(&mut self, t: Ty, record: &str) -> R<FunctionValue<'ctx>> {
        let name = format!("{}.emit", self.tys.adt_name(t));
        let program = self.program;
        let body = program
            .bodies
            .get(&name)
            .ok_or_else(|| format!("std.log: no body for `{name}`"))?;
        let record_ty = program
            .bodies
            .get(record)
            .ok_or_else(|| format!("std.log: no body for `{record}`"))?
            .locals[0]
            .ty;
        let takes_ref = matches!(
            self.tys.kind(body.locals[2].ty),
            TyKind::Ref(_) | TyKind::MutRef(_)
        );
        let emit = self.function(&name)?;
        let build = self.function(record)?;
        self.side_fn(&format!("log.emit.{name}"), 2, false, |cx, f| {
            let sink = f.get_nth_param(0).unwrap().into_pointer_value();
            let call = f.get_nth_param(1).unwrap().into_pointer_value();
            let rec = cx.temp(record_ty, "record")?;
            cx.builder
                .build_call(build, &[rec.into(), call.into()], "")
                .map_err(e)?;
            let unit = cx.entry_slot(1, "unit")?;
            let recv = cx.entry_slot(8, "self")?;
            cx.store(recv, sink.into())?;
            let arg = if takes_ref {
                let slot = cx.entry_slot(8, "record")?;
                cx.store(slot, rec.into())?;
                slot
            } else {
                rec
            };
            let vals: Vec<BasicMetadataValueEnum> = vec![unit.into(), recv.into(), arg.into()];
            cx.builder.build_call(emit, &vals, "").map_err(e)?;
            if takes_ref && cx.tys.needs_drop(record_ty) {
                cx.call_glue(rec, record_ty)?;
            }
            Ok(())
        })
    }

    /// `void(sink)`: drops the backend of type `t` and frees its box.
    fn log_drop_fn(&mut self, t: Ty) -> R<FunctionValue<'ctx>> {
        let name = format!("log.drop.{}", self.tys.display(t));
        self.side_fn(&name, 1, false, |cx, f| {
            let p = f.get_nth_param(0).unwrap().into_pointer_value();
            if cx.tys.needs_drop(t) {
                cx.call_glue(p, t)?;
            }
            let free = cx.libc_fn("free");
            cx.builder.build_call(free, &[p.into()], "").map_err(e)?;
            Ok(())
        })
    }

    /// `void(call)`: the default backend `default`, consuming the call.
    fn log_default_fn(&mut self, default: &str) -> R<FunctionValue<'ctx>> {
        let body = self.function(default)?;
        self.side_fn(&format!("log.default.{default}"), 1, false, |cx, f| {
            let call = f.get_nth_param(0).unwrap().into_pointer_value();
            let unit = cx.entry_slot(1, "unit")?;
            cx.builder
                .build_call(body, &[unit.into(), call.into()], "")
                .map_err(e)?;
            Ok(())
        })
    }
}
