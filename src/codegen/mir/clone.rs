//! Clone glue: `clone.<T>(dst, src)` writes a clone of the value of type
//! `T` at `src` to `dst`, as the interpreter's `clone_value` does
//! (`src/mir/interp.rs`): a `Copy` value by its bytes, a `String` or `Vec`
//! into a buffer of its own, a `shared` handle as one more strong count, a
//! `weak` handle as one more weak count, and a struct or enum field by
//! field, through the type's own `clone` body when it has one.

use std::collections::HashMap;

use inkwell::module::Linkage;
use inkwell::values::{FunctionValue, PointerValue};

use super::{Cx, R};
use crate::mir::layout;
use crate::mir::ty::IntrinsicTy;
use crate::mir::{Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

#[derive(Default)]
pub(super) struct Clones<'ctx> {
    fns: HashMap<Ty, FunctionValue<'ctx>>,
    todo: Vec<(Ty, FunctionValue<'ctx>)>,
}

impl<'ctx> Cx<'ctx, '_> {
    pub(super) fn clone_pending(&self) -> bool {
        !self.clones.todo.is_empty()
    }

    /// The clone glue of `ty`, declared now and built by
    /// [`Cx::finish_clones`].
    fn clone_fn(&mut self, ty: Ty) -> FunctionValue<'ctx> {
        if let Some(&f) = self.clones.fns.get(&ty) {
            return f;
        }
        let p = self.ptr_ty();
        let fn_ty = self.llcx.void_type().fn_type(&[p.into(), p.into()], false);
        let name = format!("clone.{}", self.tys.display(ty));
        let f = self
            .module
            .add_function(&name, fn_ty, Some(Linkage::Internal));
        self.clones.fns.insert(ty, f);
        self.clones.todo.push((ty, f));
        f
    }

    pub(super) fn finish_clones(&mut self) -> R<()> {
        while let Some((ty, f)) = self.clones.todo.pop() {
            let entry = self.llcx.append_basic_block(f, "entry");
            self.builder.position_at_end(entry);
            let dst = f
                .get_nth_param(0)
                .ok_or("clone glue has no destination")?
                .into_pointer_value();
            let src = f
                .get_nth_param(1)
                .ok_or("clone glue has no source")?
                .into_pointer_value();
            self.clone_value(dst, src, ty)?;
            self.builder.build_return(None).map_err(e)?;
        }
        Ok(())
    }

    /// Writes a clone of the `ty` at `src` to `dst`, at the builder's
    /// position.
    pub(super) fn clone_into(
        &mut self,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        ty: Ty,
    ) -> R<()> {
        if self.tys.is_copy(ty) {
            return self.copy(dst, src, ty);
        }
        let f = self.clone_fn(ty);
        self.builder
            .build_call(f, &[dst.into(), src.into()], "")
            .map_err(e)?;
        Ok(())
    }

    /// The pointer to element `i` of `size` bytes from `base`.
    pub(super) fn elem_ptr(
        &self,
        base: PointerValue<'ctx>,
        i: inkwell::values::IntValue<'ctx>,
        size: u64,
    ) -> R<PointerValue<'ctx>> {
        let at = self
            .builder
            .build_int_mul(i, self.i64(size), "")
            .map_err(e)?;
        // SAFETY: element `i` of a buffer the caller has bounds-checked.
        unsafe {
            self.builder
                .build_in_bounds_gep(self.llcx.i8_type(), base, &[at], "")
                .map_err(e)
        }
    }

    /// Clones `n` elements of `el` from `src` to `dst`, first to last.
    fn clone_elems(
        &mut self,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        n: inkwell::values::IntValue<'ctx>,
        el: Ty,
    ) -> R<()> {
        let size = self.layout(el)?.size;
        if self.tys.is_copy(el) {
            let bytes = self
                .builder
                .build_int_mul(n, self.i64(size), "")
                .map_err(e)?;
            self.builder
                .build_memmove(dst, 1, src, 1, bytes)
                .map_err(e)?;
            return Ok(());
        }
        self.count_loop(n, |cx, i| {
            let d = cx.elem_ptr(dst, i, size)?;
            let s = cx.elem_ptr(src, i, size)?;
            cx.clone_into(d, s, el)
        })
    }

    fn clone_value(&mut self, dst: PointerValue<'ctx>, src: PointerValue<'ctx>, ty: Ty) -> R<()> {
        if self.tys.is_copy(ty) {
            return self.copy(dst, src, ty);
        }
        match self.tys.kind(ty) {
            TyKind::Intrinsic(IntrinsicTy::String) => {
                let s = self.load_ptr(src)?;
                let n = self.load_i64(self.offset(src, 8)?)?;
                self.rt("string_from", &[dst.into(), s.into(), n.into()])
            }
            TyKind::Intrinsic(IntrinsicTy::Vec(el)) => {
                let n = self.load_i64(self.offset(src, 8)?)?;
                let size = self.layout(el)?.size;
                self.rt("vec_alloc", &[dst.into(), n.into(), self.i64(size).into()])?;
                let db = self.load_ptr(dst)?;
                let sb = self.load_ptr(src)?;
                self.clone_elems(db, sb, n, el)
            }
            TyKind::Intrinsic(IntrinsicTy::VecDeque(el)) => {
                let n = self.load_i64(self.offset(src, 16)?)?;
                let size = self.layout(el)?.size;
                self.rt(
                    "deque_alloc",
                    &[dst.into(), n.into(), self.i64(size).into()],
                )?;
                let db = self.load_ptr(dst)?;
                self.count_loop(n, |cx, i| {
                    let s =
                        cx.mrt_ptr("deque_slot", &[src.into(), i.into(), cx.i64(size).into()])?;
                    let d = cx.elem_ptr(db, i, size)?;
                    cx.clone_into(d, s, el)
                })
            }
            TyKind::Array(el, n) => self.clone_elems(dst, src, self.i64(n), el),
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => {
                self.copy(dst, src, ty)?;
                for (i, t) in ts.iter().enumerate() {
                    if !self.tys.is_copy(*t) {
                        let off = self.field_offset(ty, None, i as u32)?;
                        let (d, s) = (self.offset(dst, off)?, self.offset(src, off)?);
                        self.clone_into(d, s, *t)?;
                    }
                }
                Ok(())
            }
            TyKind::Shared(_) => {
                let h = self.load_ptr(src)?;
                self.retain(h)?;
                self.store(dst, h.into())
            }
            TyKind::Weak(_) => {
                let h = self.load_ptr(src)?;
                self.store(dst, h.into())?;
                let some = self.builder.build_is_not_null(h, "").map_err(e)?;
                self.if_then(some, |cx| {
                    let wp = cx.offset(h, 8)?;
                    let w = cx.load_i64(wp)?;
                    let w = cx.builder.build_int_add(w, cx.i64(1), "").map_err(e)?;
                    cx.store(wp, w.into())
                })
            }
            TyKind::Adt(_) => self.clone_adt(dst, src, ty),
            TyKind::Intrinsic(
                IntrinsicTy::Map(..)
                | IntrinsicTy::SortedMap(..)
                | IntrinsicTy::Set(_)
                | IntrinsicTy::SortedSet(_),
            ) => self.clone_table(dst, src, ty),
            _ => Err(format!(
                "MIR to LLVM: cloning a `{}` is not lowered yet",
                self.tys.display(ty)
            )),
        }
    }

    fn clone_adt(&mut self, dst: PointerValue<'ctx>, src: PointerValue<'ctx>, ty: Ty) -> R<()> {
        let program = self.program;
        let user = format!("{}.clone", self.tys.adt_name(ty));
        if program.bodies.contains_key(&user) {
            // `fn T.clone(ref self) -> T`: its argument slot holds the
            // reference.
            let f = self.function(&user)?;
            let arg = self.entry_slot(8, "self")?;
            self.builder.build_store(arg, src).map_err(e)?;
            self.builder
                .build_call(f, &[dst.into(), arg.into()], "")
                .map_err(e)?;
            return Ok(());
        }
        let Some((adt, _)) = self.tys.tcx().adt_of(ty) else {
            return Err(format!("`{}` is not an ADT", self.tys.display(ty)));
        };
        self.copy(dst, src, ty)?;
        if !adt.is_enum {
            return self.clone_fields(dst, src, ty, None, adt.variants[0].fields.len());
        }
        let Some(tag) = layout::enum_tag(self.tys, ty) else {
            return Ok(());
        };
        let tag_ty = self.int_ty(tag.size as u32 * 8);
        let f = self.cur_fn()?;
        let done = self.llcx.append_basic_block(f, "cloned");
        let mut cases = Vec::new();
        for (v, var) in adt.variants.iter().enumerate() {
            let owns = (0..var.fields.len() as u32).any(|i| {
                self.tys
                    .field_ty(ty, Some(v as u32), i)
                    .is_some_and(|t| !self.tys.is_copy(t))
            });
            if owns {
                let bb = self.llcx.append_basic_block(f, &format!("variant{v}"));
                cases.push((tag_ty.const_int(v as u64, false), bb, v, var.fields.len()));
            }
        }
        let tag_v = self.tag_at(src, ty)?;
        let arms: Vec<_> = cases.iter().map(|(k, bb, _, _)| (*k, *bb)).collect();
        self.builder.build_switch(tag_v, done, &arms).map_err(e)?;
        for (_, bb, v, n) in cases {
            self.builder.position_at_end(bb);
            self.clone_fields(dst, src, ty, Some(v as u32), n)?;
            self.builder.build_unconditional_branch(done).map_err(e)?;
        }
        self.builder.position_at_end(done);
        Ok(())
    }

    fn clone_fields(
        &mut self,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        ty: Ty,
        v: Option<u32>,
        n: usize,
    ) -> R<()> {
        for i in 0..n as u32 {
            let Some(fty) = self.tys.field_ty(ty, v, i) else {
                return Err(format!("`{}` has no field {i}", self.tys.display(ty)));
            };
            if layout::boxed_field(self.tys, ty, v, i) {
                // The copy has a box of its own.
                let off = self.field_offset(ty, v, i)?;
                let d = self.offset(dst, off)?;
                let d = self.new_box(d, fty)?;
                let s = self.load_ptr(self.offset(src, off)?)?;
                self.clone_into(d, s, fty)?;
            } else if !self.tys.is_copy(fty) {
                let off = self.field_offset(ty, v, i)?;
                let (d, s) = (self.offset(dst, off)?, self.offset(src, off)?);
                self.clone_into(d, s, fty)?;
            }
        }
        Ok(())
    }
}
