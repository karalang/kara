//! `std.tracing`'s natives: the minimum level, the registered exporter,
//! and the stack of spans `with_span` made active.
//!
//! The exporter is a heap copy of the value `Log.set_exporter` was given,
//! held in a global beside two functions made for its type at that call:
//! one hands it an event (`T.export_event`, or the default printing when
//! the type has none) and one drops it. An event emitted with no exporter
//! is printed as `StdoutExporter` prints it.

use inkwell::module::Linkage;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    /// The global `name`, zeroed, of `size` bytes.
    fn tracing_global(&self, name: &str) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g.as_pointer_value();
        }
        let ty = self.llcx.i64_type();
        let g = self.module.add_global(ty, None, name);
        g.set_initializer(&ty.const_zero());
        g.set_linkage(Linkage::Internal);
        g.as_pointer_value()
    }

    /// The natives of `std.tracing`, or `None` for one not lowered here.
    pub(super) fn tracing_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        match (base, method, args) {
            ("tracing_active_span", "", []) => {
                let v = self.rt_int("span_top", &[], 64)?;
                self.store_int_as(dest, v, true)?;
            }
            ("tracing_level_enabled", "", [r]) => {
                let r = self.int_arg(r)?;
                let min = self.load_i64(self.tracing_global("karac.tracing.min"))?;
                let on = self
                    .builder
                    .build_int_compare(IntPredicate::SGE, r, min, "")
                    .map_err(e)?;
                self.store_bool(dest, on)?;
            }
            ("tracing_set_min_level", "", [r]) => {
                let r = self.int_arg(r)?;
                let g = self.tracing_global("karac.tracing.min");
                self.store(g, r.into())?;
            }
            ("tracing_reset", "", []) => {
                let g = self.tracing_global("karac.tracing.min");
                self.store(g, self.i64(0).into())?;
                self.drop_sink()?;
            }
            ("Log", "set_exporter", [v]) => {
                let t = self.operand_type(v)?;
                if matches!(self.tys.kind(t), TyKind::Ref(_) | TyKind::MutRef(_)) {
                    return Ok(None);
                }
                let size = self.layout(t)?.size.max(1);
                let malloc = self.libc_fn("malloc");
                let bx = self
                    .builder
                    .build_call(malloc, &[self.i64(size).into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let vp = self.operand_ptr(v)?;
                self.copy(bx, vp, t)?;
                self.drop_sink()?;
                // A type without `export_event` leaves the default printing.
                let emit = match self.sink_emit_fn(t)? {
                    Some(f) => f.as_global_value().as_pointer_value(),
                    None => self.ptr_ty().const_null(),
                };
                let drop = self.sink_drop_fn(t)?;
                let ptr_of = |f: FunctionValue<'ctx>| f.as_global_value().as_pointer_value();
                self.store(self.tracing_global("karac.tracing.sink"), bx.into())?;
                self.store(self.tracing_global("karac.tracing.emit"), emit.into())?;
                self.store(
                    self.tracing_global("karac.tracing.drop"),
                    ptr_of(drop).into(),
                )?;
            }
            ("tracing_emit_event", "", [ev]) => {
                let et = self.operand_type(ev)?;
                let (p, by_ref) = match self.tys.kind(et) {
                    TyKind::Ref(_) | TyKind::MutRef(_) => {
                        let q = self.operand_ptr(ev)?;
                        (self.load_ptr(q)?, true)
                    }
                    _ => (self.operand_ptr(ev)?, false),
                };
                let event_ty = match self.tys.kind(et) {
                    TyKind::Ref(t) | TyKind::MutRef(t) => t,
                    _ => et,
                };
                let flag = self.llcx.i8_type().const_int(u64::from(by_ref), false);
                let sink = self.load_ptr(self.tracing_global("karac.tracing.sink"))?;
                let code = self.load_ptr(self.tracing_global("karac.tracing.emit"))?;
                let none = self.builder.build_is_null(code, "").map_err(e)?;
                let default = self.default_emit_fn(event_ty)?;
                let f = self.cur_fn()?;
                let (db, sb, done) = (
                    self.llcx.append_basic_block(f, "emit.default"),
                    self.llcx.append_basic_block(f, "emit.sink"),
                    self.llcx.append_basic_block(f, "emit.done"),
                );
                self.builder
                    .build_conditional_branch(none, db, sb)
                    .map_err(e)?;
                self.builder.position_at_end(db);
                self.builder
                    .build_call(default, &[p.into(), flag.into()], "")
                    .map_err(e)?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(sb);
                let pt = self.ptr_ty();
                let fty = self
                    .llcx
                    .void_type()
                    .fn_type(&[pt.into(), pt.into(), self.llcx.i8_type().into()], false);
                self.builder
                    .build_indirect_call(fty, code, &[sink.into(), p.into(), flag.into()], "")
                    .map_err(e)?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(done);
            }
            ("with_span", "", [span, body]) => {
                // The body runs with the span's id active; `LogEvent`'s
                // constructors stamp it.
                let st = self.operand_type(span)?;
                let (sp, sty) = match self.tys.kind(st) {
                    TyKind::Ref(t) | TyKind::MutRef(t) => {
                        let q = self.operand_ptr(span)?;
                        (self.load_ptr(q)?, t)
                    }
                    _ => (self.operand_ptr(span)?, st),
                };
                let id = self.load_i64(self.offset(sp, self.field_offset(sty, None, 1)?)?)?;
                self.rt("span_push", &[id.into()])?;
                let c = self.callee_of(body)?;
                let d = self.place_ptr(dest)?;
                self.call_callee(&c, d, &[])?;
                self.rt("span_pop", &[])?;
                if !Self::consumes(&c) {
                    self.drop_moved(std::slice::from_ref(body))?;
                }
                self.drop_moved(std::slice::from_ref(span))?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// Drops the registered exporter, if any, and clears it.
    fn drop_sink(&mut self) -> R<()> {
        let g = self.tracing_global("karac.tracing.sink");
        let sink = self.load_ptr(g)?;
        let set = self.builder.build_is_not_null(sink, "").map_err(e)?;
        self.if_then(set, |cx| {
            let code = cx.load_ptr(cx.tracing_global("karac.tracing.drop"))?;
            let pt = cx.ptr_ty();
            let fty = cx.llcx.void_type().fn_type(&[pt.into()], false);
            cx.builder
                .build_indirect_call(fty, code, &[sink.into()], "")
                .map_err(e)?;
            cx.store(g, pt.const_null().into())
        })
    }

    /// Runs `build` in the new function `name` of type `void(ptrs..., i8?)`,
    /// then puts the builder back where it was.
    fn side_fn(
        &mut self,
        name: &str,
        ptrs: usize,
        flag: bool,
        build: impl FnOnce(&mut Self, FunctionValue<'ctx>) -> R<()>,
    ) -> R<FunctionValue<'ctx>> {
        if let Some(f) = self.module.get_function(name) {
            return Ok(f);
        }
        let pt = self.ptr_ty();
        let mut ps: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![pt.into(); ptrs];
        if flag {
            ps.push(self.llcx.i8_type().into());
        }
        let f = self.module.add_function(
            name,
            self.llcx.void_type().fn_type(&ps, false),
            Some(Linkage::Internal),
        );
        let back = self.builder.get_insert_block();
        self.builder
            .position_at_end(self.llcx.append_basic_block(f, "entry"));
        build(self, f)?;
        self.builder.build_return(None).map_err(e)?;
        if let Some(b) = back {
            self.builder.position_at_end(b);
        }
        Ok(f)
    }

    /// `void(sink, event, by_ref)`: the exporter of type `t` receives the
    /// event through `t.export_event`, or the default printing when `t`
    /// has none.
    fn sink_emit_fn(&mut self, t: Ty) -> R<Option<FunctionValue<'ctx>>> {
        let name = format!("{}.export_event", self.tys.adt_name(t));
        let Some(body) = self.program.bodies.get(&name) else {
            return Ok(None);
        };
        let param_ty = body.locals[2].ty;
        let takes_ref = matches!(self.tys.kind(param_ty), TyKind::Ref(_) | TyKind::MutRef(_));
        let event_ty = match self.tys.kind(param_ty) {
            TyKind::Ref(i) | TyKind::MutRef(i) => i,
            _ => param_ty,
        };
        let callee = self.function(&name)?;
        self.side_fn(&format!("tracing.emit.{name}"), 2, true, |cx, f| {
            let sink = f.get_nth_param(0).unwrap().into_pointer_value();
            let ev = f.get_nth_param(1).unwrap().into_pointer_value();
            let by_ref = f.get_nth_param(2).unwrap().into_int_value();
            let owned = cx.is_zero(by_ref)?;
            let unit = cx.entry_slot(1, "unit")?;
            let recv = cx.entry_slot(8, "self")?;
            cx.store(recv, sink.into())?;
            let arg = if takes_ref {
                let slot = cx.entry_slot(8, "event")?;
                cx.store(slot, ev.into())?;
                slot
            } else {
                // An owned event moves in; a borrowed one is cloned first.
                let tmp = cx.temp(event_ty, "event")?;
                let f2 = cx.cur_fn()?;
                let (mv, cl, go) = (
                    cx.llcx.append_basic_block(f2, "move"),
                    cx.llcx.append_basic_block(f2, "clone"),
                    cx.llcx.append_basic_block(f2, "go"),
                );
                cx.builder
                    .build_conditional_branch(owned, mv, cl)
                    .map_err(e)?;
                cx.builder.position_at_end(mv);
                cx.copy(tmp, ev, event_ty)?;
                cx.builder.build_unconditional_branch(go).map_err(e)?;
                cx.builder.position_at_end(cl);
                cx.clone_into(tmp, ev, event_ty)?;
                cx.builder.build_unconditional_branch(go).map_err(e)?;
                cx.builder.position_at_end(go);
                tmp
            };
            let vals: Vec<BasicMetadataValueEnum> = vec![unit.into(), recv.into(), arg.into()];
            cx.builder.build_call(callee, &vals, "").map_err(e)?;
            if takes_ref && cx.tys.needs_drop(event_ty) {
                cx.if_then(owned, |cx| cx.call_glue(ev, event_ty))?;
            }
            Ok(())
        })
        .map(Some)
    }

    /// `void(sink)`: drops the exporter of type `t` and frees its box.
    fn sink_drop_fn(&mut self, t: Ty) -> R<FunctionValue<'ctx>> {
        let name = format!("tracing.drop.{}", self.tys.display(t));
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

    /// `void(event, by_ref)`: prints the event as `StdoutExporter` does,
    /// `[level] message`, each ` key=value`, then a non-zero ` span_id=`,
    /// and drops an owned one.
    fn default_emit_fn(&mut self, event_ty: Ty) -> R<FunctionValue<'ctx>> {
        self.side_fn("tracing.emit.default", 1, true, |cx, f| {
            let ev = f.get_nth_param(0).unwrap().into_pointer_value();
            let by_ref = f.get_nth_param(1).unwrap().into_int_value();
            let field = |cx: &mut Self, i: u32| -> R<(PointerValue<'ctx>, Ty)> {
                let off = cx.field_offset(event_ty, None, i)?;
                let t = cx
                    .tys
                    .field_ty(event_ty, None, i)
                    .ok_or("a LogEvent field")?;
                Ok((cx.offset(ev, off)?, t))
            };
            let (level, lt) = field(cx, 0)?;
            let (msg, mt) = field(cx, 1)?;
            let (fields, ft) = field(cx, 2)?;
            let (span, _) = field(cx, 3)?;
            cx.rt("fmt_begin", &[])?;
            cx.lit("[")?;
            cx.fmt_call(level, lt)?;
            cx.lit("] ")?;
            cx.fmt_call(msg, mt)?;
            let TyKind::Intrinsic(crate::mir::ty::IntrinsicTy::Vec(sf)) = cx.tys.kind(ft) else {
                return Err("LogEvent.fields is not a Vec".into());
            };
            let size = cx.layout(sf)?.size;
            let (kt, vt) = (
                cx.tys.field_ty(sf, None, 0).ok_or("a SpanField key")?,
                cx.tys.field_ty(sf, None, 1).ok_or("a SpanField value")?,
            );
            let (ko, vo) = (cx.field_offset(sf, None, 0)?, cx.field_offset(sf, None, 1)?);
            let buf = cx.load_ptr(fields)?;
            let n = cx.load_i64(cx.offset(fields, 8)?)?;
            cx.count_loop(n, |cx, i| {
                let x = cx.elem_ptr(buf, i, size)?;
                cx.lit(" ")?;
                cx.fmt_call(cx.offset(x, ko)?, kt)?;
                cx.lit("=")?;
                cx.fmt_call(cx.offset(x, vo)?, vt)
            })?;
            let id = cx.load_i64(span)?;
            let some = cx
                .builder
                .build_int_compare(IntPredicate::NE, id, cx.i64(0), "")
                .map_err(e)?;
            let i64t = cx.tys.intern(TyKind::Int(crate::mir::IntTy::I64));
            cx.if_then(some, |cx| {
                cx.lit(" span_id=")?;
                cx.fmt_call(span, i64t)
            })?;
            let flags = cx.llcx.i32_type().const_int(1, false);
            cx.rt("print_end", &[flags.into()])?;
            if cx.tys.needs_drop(event_ty) {
                let owned = cx.is_zero(by_ref)?;
                cx.if_then(owned, |cx| cx.call_glue(ev, event_ty))?;
            }
            Ok(())
        })
    }
}
