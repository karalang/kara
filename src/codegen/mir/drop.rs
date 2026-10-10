//! The elaborated `Drop` terminator and each type's drop glue.
//!
//! Elaboration (`crate::mir::elaborate`) has already decided every drop:
//! a `Drop` terminator in an elaborated body drops an initialized place,
//! and a maybe-initialized place is guarded by its flag, an ordinary `bool`
//! local that `body.rs` lowers like any other. So this file only has to
//! drop a value, and it does that the way the MIR interpreter's `drop_at`
//! does (core semantics §7.8):
//!
//! - an ADT with a `Drop` body runs it first, with every field alive, then
//!   drops its fields last to first (an enum: its active variant's);
//! - a tuple or closure drops its parts last to first, an array its
//!   elements first to last;
//! - a `String` frees its buffer, a `Vec` drops its elements first to last
//!   and then frees its buffer;
//! - a `shared` handle releases its box: at strong count 0 the body is
//!   dropped like the ADT's, and the box is freed when no weak handle
//!   keeps it; a `weak` handle (null when empty) frees a box whose strong
//!   count is already 0 when it was the last weak one;
//! - a `VecDeque` drops its elements front to back, across the wrap, then
//!   frees its buffer; a map or set drops each entry's key then its value,
//!   in the runtime table's order, then frees the table;
//! - an erased function value drops its captures through its own glue
//!   pointer and frees their block;
//! - a library cell hands its handle to the runtime (`LIBRARY_CELLS`).
//!
//! The glue of a type `T` is `void @"drop.T"(ptr)`, built once per type
//! and on demand, so recursive types (through a box or a buffer) are fine.

use std::collections::HashMap;

use inkwell::module::Linkage;
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::{fn_symbol, Cx, R};
use crate::mir::layout::{self, SHARED_COUNTS};
use crate::mir::place_ty::place_ty;
use crate::mir::ty::{AdtId, IntrinsicTy, TyKind};
use crate::mir::{BasicBlock, Place, Ty};

/// The library cells whose contents live in the runtime, behind a one-word
/// handle at offset 0 (the interpreter drops them by hand in
/// `Interp::drop_adt`), with the runtime entry point that drops one:
/// `void(i64 handle, ptr held_glue)`, where `held_glue` is the glue of the
/// held type (the cell's first type argument), or null when it owns
/// nothing. A zero handle (a hand-built literal) owns nothing. `Atomic[T]`
/// holds a `T` in place, which is never a type that owns anything.
const LIBRARY_CELLS: &[(&str, &str)] = &[
    ("Sender", "karac_sender_drop"),
    ("Receiver", "karac_receiver_drop"),
    ("File", "karac_file_drop"),
    ("OnceLock", "karac_once_lock_drop"),
    ("OnceCell", "karac_once_cell_drop"),
    ("TaskHandle", "karac_task_handle_drop"),
    ("Arena", "karac_arena_drop"),
];

/// The runtime's table behind a map or set handle: its entry count, a
/// pointer to entry `i` (in iteration order, which is key order for the
/// sorted kinds), and the free of the table itself. A map entry is laid
/// out as the tuple `(K, V)`, a set entry as `K`.
const TABLE_LEN: &str = "karac_table_len";
const TABLE_ENTRY: &str = "karac_table_entry";
const TABLE_FREE: &str = "karac_table_free";

/// The glue built or requested so far.
#[derive(Default)]
pub(super) struct Glue<'ctx> {
    fns: HashMap<Ty, FunctionValue<'ctx>>,
    /// Requested but not built yet.
    todo: Vec<(Ty, FunctionValue<'ctx>)>,
}

fn err<T>(e: impl ToString) -> R<T> {
    Err(e.to_string())
}

impl<'ctx> Cx<'ctx, '_> {
    /// `drop(place) -> target`: the place's glue when its type owns
    /// anything, then on to `target`. Unwinding is not lowered: a panic
    /// in a `Drop` body aborts (core semantics §10).
    pub(super) fn drop_terminator(&mut self, place: &Place, target: BasicBlock) -> R<()> {
        let ty = place_ty(self.cur()?, self.tys, place)?.ty;
        if self.tys.needs_drop(ty) {
            let p = self.place_ptr(place)?;
            self.call_glue(p, ty)?;
        }
        self.builder
            .build_unconditional_branch(self.blocks[target.index()])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The drop glue of `ty`, declared now and built by [`Cx::finish_glue`].
    pub fn glue_fn(&mut self, ty: Ty) -> FunctionValue<'ctx> {
        if let Some(&f) = self.glue.fns.get(&ty) {
            return f;
        }
        let fn_ty = self
            .llcx
            .void_type()
            .fn_type(&[self.ptr_ty().into()], false);
        let name = format!("drop.{}", self.tys.display(ty));
        let f = self
            .module
            .add_function(&name, fn_ty, Some(Linkage::Internal));
        self.glue.fns.insert(ty, f);
        self.glue.todo.push((ty, f));
        f
    }

    /// Builds every glue function requested so far, and those they request.
    pub fn finish_glue(&mut self) -> R<()> {
        while let Some((ty, f)) = self.glue.todo.pop() {
            let entry = self.llcx.append_basic_block(f, "entry");
            self.builder.position_at_end(entry);
            let p = f
                .get_first_param()
                .ok_or("glue has no parameter")?
                .into_pointer_value();
            self.drop_value(p, ty)?;
            self.builder.build_return(None).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn call_glue(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<()> {
        let f = self.glue_fn(ty);
        self.builder
            .build_call(f, &[p.into()], "")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Drops the value of type `ty` at `p`, at the builder's position.
    fn drop_value(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<()> {
        match self.tys.kind(ty) {
            TyKind::Adt(a) => {
                if let Some(rt) = self.cell_drop(a) {
                    return self.drop_cell(p, ty, rt);
                }
                if self.tys.adt(a).name == "Atomic" {
                    return Ok(());
                }
                if self.adt_has_drop_body(a)? {
                    // `fn T.drop(mut ref self)`: its argument slot holds
                    // the reference.
                    let slot = self.entry_slot(8, "self")?;
                    self.builder
                        .build_store(slot, p)
                        .map_err(|e| e.to_string())?;
                    self.call_drop_body(a, ty, slot)?;
                }
                self.drop_fields(p, ty)
            }
            TyKind::Shared(a) => self.release(p, ty, a),
            TyKind::Weak(_) => self.release_weak(p),
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => {
                for i in (0..ts.len()).rev() {
                    if self.tys.needs_drop(ts[i]) {
                        let off = self.field_offset(ty, None, i as u32)?;
                        let q = self.offset(p, off)?;
                        self.call_glue(q, ts[i])?;
                    }
                }
                Ok(())
            }
            TyKind::Array(e, n) => {
                if self.tys.needs_drop(e) {
                    self.each_elem(p, self.i64(n), e)?;
                }
                Ok(())
            }
            TyKind::Intrinsic(IntrinsicTy::String) => self.free_buffer(p),
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) => {
                if self.tys.needs_drop(e) {
                    let buf = self.load_ptr(p)?;
                    let len = self.load_i64(self.offset(p, 8)?)?;
                    self.each_elem(buf, len, e)?;
                }
                self.free_buffer(p)
            }
            TyKind::Intrinsic(IntrinsicTy::VecDeque(e)) => {
                if self.tys.needs_drop(e) {
                    self.drop_ring(p, e)?;
                }
                self.free_buffer(p)
            }
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                self.drop_table(p, k, Some(v))
            }
            TyKind::Intrinsic(IntrinsicTy::Set(k) | IntrinsicTy::SortedSet(k)) => {
                self.drop_table(p, k, None)
            }
            TyKind::Fn { .. } => self.drop_erased_fn(p),
            _ => err(format!(
                "MIR to LLVM: dropping a `{}` is not lowered yet",
                self.tys.display(ty)
            )),
        }
    }

    fn adt_has_drop_body(&self, a: AdtId) -> R<bool> {
        Ok(self.tys.adt(a).has_drop_impl)
    }

    /// The runtime entry point that drops the library cell `a`, if it is one.
    fn cell_drop(&self, a: AdtId) -> Option<&'static str> {
        let name = self.tys.adt(a).name.clone();
        LIBRARY_CELLS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, rt)| *rt)
    }

    /// Drops the library cell of type `ty` at `p` through the runtime.
    fn drop_cell(&mut self, p: PointerValue<'ctx>, ty: Ty, rt: &str) -> R<()> {
        let held = self
            .tys
            .tcx()
            .adt_of(ty)
            .and_then(|(_, args)| args.first().copied())
            .filter(|&t| self.tys.needs_drop(t));
        let glue = match held {
            Some(t) => self.glue_fn(t).as_global_value().as_pointer_value(),
            None => self.ptr_ty().const_null(),
        };
        let i64t = self.llcx.i64_type();
        let f = self.extern_fn(
            rt,
            self.llcx
                .void_type()
                .fn_type(&[i64t.into(), self.ptr_ty().into()], false),
        );
        let h = self.load_i64(p)?;
        let zero = self.is_zero(h)?;
        let some = self
            .builder
            .build_not(zero, "")
            .map_err(|e| e.to_string())?;
        self.if_then(some, |cx| {
            cx.builder
                .build_call(f, &[h.into(), glue.into()], "")
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    /// Drops the elements of the `VecDeque` ring at `p`: `len` of them from
    /// slot `head`, wrapping at `cap`, front to back.
    fn drop_ring(&mut self, p: PointerValue<'ctx>, e: Ty) -> R<()> {
        let size = self.layout(e)?.size;
        let buf = self.load_ptr(p)?;
        let head = self.load_i64(self.offset(p, 8)?)?;
        let len = self.load_i64(self.offset(p, 16)?)?;
        let cap = self.load_i64(self.offset(p, 24)?)?;
        let b = &self.builder;
        let room = b.build_int_sub(cap, head, "").map_err(|e| e.to_string())?;
        let fits = b
            .build_int_compare(IntPredicate::ULE, len, room, "")
            .map_err(|e| e.to_string())?;
        let first = b
            .build_select(fits, len, room, "first")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let wrapped = b
            .build_int_sub(len, first, "wrapped")
            .map_err(|e| e.to_string())?;
        let at = b
            .build_int_mul(head, self.i64(size), "")
            .map_err(|e| e.to_string())?;
        // SAFETY: slot `head < cap` of a buffer of `cap` elements (or the
        // end of an empty one, which is not read).
        let front = unsafe {
            b.build_in_bounds_gep(self.llcx.i8_type(), buf, &[at], "")
                .map_err(|e| e.to_string())?
        };
        self.each_elem(front, first, e)?;
        self.each_elem(buf, wrapped, e)
    }

    /// Drops the map or set at `p`: each entry's key, then its value, in
    /// the table's order, then the table. A null handle owns nothing.
    fn drop_table(&mut self, p: PointerValue<'ctx>, k: Ty, v: Option<Ty>) -> R<()> {
        let (pt, i64t) = (self.ptr_ty(), self.llcx.i64_type());
        let len_f = self.extern_fn(TABLE_LEN, i64t.fn_type(&[pt.into()], false));
        let entry_f = self.extern_fn(TABLE_ENTRY, pt.fn_type(&[pt.into(), i64t.into()], false));
        let free_f = self.extern_fn(
            TABLE_FREE,
            self.llcx.void_type().fn_type(&[pt.into()], false),
        );
        let (kd, vd) = (
            self.tys.needs_drop(k),
            v.is_some_and(|v| self.tys.needs_drop(v)),
        );
        let voff = match v {
            Some(v) => {
                let (kl, vl) = (self.layout(k)?, self.layout(v)?);
                kl.size.div_ceil(vl.align) * vl.align
            }
            None => 0,
        };
        let h = self.load_ptr(p)?;
        let null = self
            .builder
            .build_is_null(h, "")
            .map_err(|e| e.to_string())?;
        let some = self
            .builder
            .build_not(null, "")
            .map_err(|e| e.to_string())?;
        self.if_then(some, |cx| {
            if kd || vd {
                let n = cx
                    .builder
                    .build_call(len_f, &[h.into()], "n")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("karac_table_len returns nothing")?
                    .into_int_value();
                cx.count_loop(n, |cx, i| {
                    let ent = cx
                        .builder
                        .build_call(entry_f, &[h.into(), i.into()], "entry")
                        .map_err(|e| e.to_string())?
                        .try_as_basic_value()
                        .basic()
                        .ok_or("karac_table_entry returns nothing")?
                        .into_pointer_value();
                    if kd {
                        cx.call_glue(ent, k)?;
                    }
                    if let (true, Some(v)) = (vd, v) {
                        cx.call_glue(cx.offset(ent, voff)?, v)?;
                    }
                    Ok(())
                })?;
            }
            cx.builder
                .build_call(free_f, &[h.into()], "")
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    /// Drops the erased function value `{code, env, drop}` at `p`: its
    /// captures through `drop`, then their block. A null `env` owns
    /// nothing; a null `drop` has captures that own nothing.
    fn drop_erased_fn(&mut self, p: PointerValue<'ctx>) -> R<()> {
        let env = self.load_ptr(self.offset(p, 8)?)?;
        let null = self
            .builder
            .build_is_null(env, "")
            .map_err(|e| e.to_string())?;
        let some = self
            .builder
            .build_not(null, "")
            .map_err(|e| e.to_string())?;
        self.if_then(some, |cx| {
            let d = cx.load_ptr(cx.offset(p, 16)?)?;
            let no = cx.builder.build_is_null(d, "").map_err(|e| e.to_string())?;
            let has = cx.builder.build_not(no, "").map_err(|e| e.to_string())?;
            cx.if_then(has, |cx| {
                let ty = cx.llcx.void_type().fn_type(&[cx.ptr_ty().into()], false);
                cx.builder
                    .build_indirect_call(ty, d, &[env.into()], "")
                    .map_err(|e| e.to_string())?;
                Ok(())
            })?;
            cx.free(env)
        })
    }

    /// Calls the `Drop` body of the ADT `a` (instance `ty`), whose argument
    /// slot `slot` holds the `mut ref` to the value.
    fn call_drop_body(&mut self, a: AdtId, ty: Ty, slot: PointerValue<'ctx>) -> R<()> {
        let program = self.program;
        let Some(name) = program
            .drop_impls
            .get(&a)
            .or_else(|| program.drop_by_ty.get(&ty))
        else {
            return err(format!(
                "no Drop body registered for `{}`",
                self.tys.display(ty)
            ));
        };
        let f = self.function(name)?;
        let ret = self.entry_slot(0, "unit")?;
        self.builder
            .build_call(f, &[ret.into(), slot.into()], "")
            .map_err(|e| e.to_string())?;
        debug_assert_eq!(f.get_name().to_str().ok(), Some(fn_symbol(name).as_str()));
        Ok(())
    }

    /// Drops the fields of the ADT body at `p` of type `ty` (plain or
    /// `shared`), last to first; for an enum, those of the active variant.
    fn drop_fields(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<()> {
        let Some((adt, _)) = self.tys.tcx().adt_of(ty) else {
            return err(format!("`{}` is not an ADT", self.tys.display(ty)));
        };
        if !adt.is_enum {
            return self.drop_variant_fields(p, ty, None, adt.variants[0].fields.len());
        }
        let Some(tag) = layout::enum_tag(self.tys, ty) else {
            return Ok(());
        };
        let tag_ty = self.int_ty(tag.size as u32 * 8);
        let f = self.cur_fn()?;
        let done = self.llcx.append_basic_block(f, "dropped");
        let mut cases = Vec::new();
        for (v, var) in adt.variants.iter().enumerate() {
            let owns = (0..var.fields.len() as u32).any(|i| {
                self.tys
                    .field_ty(ty, Some(v as u32), i)
                    .is_some_and(|t| self.tys.needs_drop(t))
            });
            if owns {
                let bb = self.llcx.append_basic_block(f, &format!("variant{v}"));
                cases.push((tag_ty.const_int(v as u64, false), bb, v, var.fields.len()));
            }
        }
        let tag_v = self
            .builder
            .build_load(tag_ty, p, "tag")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let arms: Vec<_> = cases.iter().map(|(k, bb, _, _)| (*k, *bb)).collect();
        self.builder
            .build_switch(tag_v, done, &arms)
            .map_err(|e| e.to_string())?;
        for (_, bb, v, n) in cases {
            self.builder.position_at_end(bb);
            self.drop_variant_fields(p, ty, Some(v as u32), n)?;
            self.builder
                .build_unconditional_branch(done)
                .map_err(|e| e.to_string())?;
        }
        self.builder.position_at_end(done);
        Ok(())
    }

    fn drop_variant_fields(
        &mut self,
        p: PointerValue<'ctx>,
        ty: Ty,
        v: Option<u32>,
        n: usize,
    ) -> R<()> {
        for i in (0..n as u32).rev() {
            let Some(fty) = self.tys.field_ty(ty, v, i) else {
                return err(format!("`{}` has no field {i}", self.tys.display(ty)));
            };
            if self.tys.needs_drop(fty) {
                let q = self.offset(p, self.field_offset(ty, v, i)?)?;
                self.call_glue(q, fty)?;
            }
        }
        Ok(())
    }

    /// Releases the `shared` handle at `p`.
    fn release(&mut self, p: PointerValue<'ctx>, ty: Ty, a: AdtId) -> R<()> {
        let b = self.load_ptr(p)?;
        let strong = self.load_i64(b)?;
        let left = self
            .builder
            .build_int_sub(strong, self.i64(1), "strong")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(b, left)
            .map_err(|e| e.to_string())?;
        let last = self.is_zero(left)?;
        self.if_then(last, |cx| {
            // Hold a weak count across the drop: the body may hold the last
            // weak handle to its own box, whose release must not free it.
            let wp = cx.offset(b, 8)?;
            let w = cx.load_i64(wp)?;
            let held = cx
                .builder
                .build_int_add(w, cx.i64(1), "")
                .map_err(|e| e.to_string())?;
            cx.builder
                .build_store(wp, held)
                .map_err(|e| e.to_string())?;
            if cx.adt_has_drop_body(a)? {
                // The body takes a reference to a handle, as every read
                // through one does: the handle at `p`.
                let slot = cx.entry_slot(8, "self")?;
                cx.builder.build_store(slot, p).map_err(|e| e.to_string())?;
                cx.call_drop_body(a, ty, slot)?;
            }
            let off = layout::shared_body_offset(cx.tys, ty)
                .ok_or_else(|| format!("`{}` has no layout", cx.tys.display(ty)))?;
            cx.drop_fields(cx.offset(b, off)?, ty)?;
            let weak = cx.load_i64(wp)?;
            let weak = cx
                .builder
                .build_int_sub(weak, cx.i64(1), "")
                .map_err(|e| e.to_string())?;
            cx.builder
                .build_store(wp, weak)
                .map_err(|e| e.to_string())?;
            let no_weak = cx.is_zero(weak)?;
            cx.if_then(no_weak, |cx| cx.free(b))
        })
    }

    /// Drops the `weak` handle at `p`.
    fn release_weak(&mut self, p: PointerValue<'ctx>) -> R<()> {
        let b = self.load_ptr(p)?;
        let empty = self
            .builder
            .build_is_null(b, "empty")
            .map_err(|e| e.to_string())?;
        let some = self
            .builder
            .build_not(empty, "")
            .map_err(|e| e.to_string())?;
        self.if_then(some, |cx| {
            let wp = cx.offset(b, 8)?;
            let weak = cx.load_i64(wp)?;
            let left = cx
                .builder
                .build_int_sub(weak, cx.i64(1), "weak")
                .map_err(|e| e.to_string())?;
            cx.builder
                .build_store(wp, left)
                .map_err(|e| e.to_string())?;
            let strong = cx.load_i64(b)?;
            let both = cx
                .builder
                .build_or(left, strong, "")
                .map_err(|e| e.to_string())?;
            let dead = cx.is_zero(both)?;
            cx.if_then(dead, |cx| cx.free(b))
        })?;
        debug_assert_eq!(SHARED_COUNTS, 16);
        Ok(())
    }

    /// Frees the buffer of the `String` or `Vec` at `p` when it has one.
    fn free_buffer(&mut self, p: PointerValue<'ctx>) -> R<()> {
        let cap = self.load_i64(self.offset(p, 16)?)?;
        let has = self
            .builder
            .build_int_compare(IntPredicate::NE, cap, self.i64(0), "")
            .map_err(|e| e.to_string())?;
        self.if_then(has, |cx| {
            let buf = cx.load_ptr(p)?;
            cx.free(buf)
        })
    }

    /// Drops `n` elements of type `e` laid out from `base`, first to last.
    fn each_elem(&mut self, base: PointerValue<'ctx>, n: IntValue<'ctx>, e: Ty) -> R<()> {
        let size = self.layout(e)?.size;
        self.count_loop(n, |cx, i| {
            let at = cx
                .builder
                .build_int_mul(i, cx.i64(size), "")
                .map_err(|e| e.to_string())?;
            // SAFETY: element `i < n` of a buffer of `n` elements of `size` bytes.
            let q = unsafe {
                cx.builder
                    .build_in_bounds_gep(cx.llcx.i8_type(), base, &[at], "")
                    .map_err(|e| e.to_string())?
            };
            cx.call_glue(q, e)
        })
    }

    /// `for i in 0..n { body(i) }`, leaving the builder after it.
    fn count_loop(
        &mut self,
        n: IntValue<'ctx>,
        body: impl FnOnce(&mut Self, IntValue<'ctx>) -> R<()>,
    ) -> R<()> {
        let f = self.cur_fn()?;
        let pre = self.builder.get_insert_block().ok_or("no insert block")?;
        let head = self.llcx.append_basic_block(f, "elem");
        let each = self.llcx.append_basic_block(f, "elem.drop");
        let done = self.llcx.append_basic_block(f, "elems.dropped");
        self.builder
            .build_unconditional_branch(head)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_phi(self.llcx.i64_type(), "i")
            .map_err(|e| e.to_string())?;
        i.add_incoming(&[(&self.i64(0), pre)]);
        let iv = i.as_basic_value().into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, n, "")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_conditional_branch(more, each, done)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(each);
        body(self, iv)?;
        let next = self
            .builder
            .build_int_add(iv, self.i64(1), "")
            .map_err(|e| e.to_string())?;
        let end = self.builder.get_insert_block().ok_or("no insert block")?;
        i.add_incoming(&[(&next, end)]);
        self.builder
            .build_unconditional_branch(head)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(done);
        Ok(())
    }

    // ── helpers ─────────────────────────────────────────────────────

    fn field_offset(&self, ty: Ty, v: Option<u32>, f: u32) -> R<u64> {
        layout::field_offset(self.tys, ty, v, f)
            .ok_or_else(|| format!("`{}` has no field {f}", self.tys.display(ty)))
    }

    fn cur_fn(&self) -> R<FunctionValue<'ctx>> {
        self.builder
            .get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or_else(|| "the builder is not in a function".to_string())
    }

    /// An 8-aligned stack slot of `size` bytes in the current function's
    /// entry block, so a loop does not grow the stack.
    fn entry_slot(&self, size: u32, name: &str) -> R<PointerValue<'ctx>> {
        let f = self.cur_fn()?;
        let entry = f.get_first_basic_block().ok_or("no entry block")?;
        let b = self.llcx.create_builder();
        match entry.get_first_instruction() {
            Some(i) => b.position_before(&i),
            None => b.position_at_end(entry),
        }
        let a = b
            .build_alloca(self.llcx.i8_type().array_type(size), name)
            .map_err(|e| e.to_string())?;
        a.as_instruction()
            .ok_or("an alloca is not an instruction")?
            .set_alignment(8)
            .map_err(|e| e.to_string())?;
        Ok(a)
    }

    fn load_ptr(&self, p: PointerValue<'ctx>) -> R<PointerValue<'ctx>> {
        Ok(self
            .builder
            .build_load(self.ptr_ty(), p, "")
            .map_err(|e| e.to_string())?
            .into_pointer_value())
    }

    fn load_i64(&self, p: PointerValue<'ctx>) -> R<IntValue<'ctx>> {
        Ok(self
            .builder
            .build_load(self.llcx.i64_type(), p, "")
            .map_err(|e| e.to_string())?
            .into_int_value())
    }

    fn is_zero(&self, v: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        self.builder
            .build_int_compare(IntPredicate::EQ, v, v.get_type().const_zero(), "")
            .map_err(|e| e.to_string())
    }

    fn free(&self, p: PointerValue<'ctx>) -> R<()> {
        let free = self.libc_fn("free");
        self.builder
            .build_call(free, &[p.into()], "")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// `if cond { then }`, leaving the builder after it.
    fn if_then(&mut self, cond: IntValue<'ctx>, then: impl FnOnce(&mut Self) -> R<()>) -> R<()> {
        let f = self.cur_fn()?;
        let yes = self.llcx.append_basic_block(f, "then");
        let done = self.llcx.append_basic_block(f, "endif");
        self.builder
            .build_conditional_branch(cond, yes, done)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(yes);
        then(self)?;
        self.builder
            .build_unconditional_branch(done)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(done);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::lower::{build_source, Lowered};
    use inkwell::context::Context;
    use inkwell::execution_engine::ExecutionEngine;
    use inkwell::targets::{InitializationConfig, Target};
    use inkwell::OptimizationLevel;
    use std::cell::RefCell;
    use std::ffi::c_void;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Ev {
        Free(usize),
        Drop(i64),
        /// A library cell's runtime drop: its handle, and whether it was
        /// handed the held type's glue.
        Cell(i64, bool),
    }

    thread_local! {
        static LOG: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
    }

    extern "C" fn logged_free(p: *mut c_void) {
        LOG.with(|l| l.borrow_mut().push(Ev::Free(p as usize)));
        // SAFETY: `p` came from `malloc` in the test that built the value.
        unsafe { libc::free(p) }
    }

    extern "C" fn logged_drop(id: i64) {
        LOG.with(|l| l.borrow_mut().push(Ev::Drop(id)));
    }

    /// A stand-in for the runtime's table: `n` entries of `size` bytes.
    #[repr(C)]
    struct Table {
        n: u64,
        size: u64,
        entries: *mut u8,
    }

    extern "C" fn table_len(t: *mut Table) -> u64 {
        // SAFETY: the test built `t`.
        unsafe { (*t).n }
    }

    extern "C" fn table_entry(t: *mut Table, i: u64) -> *mut u8 {
        // SAFETY: as above; `i < n`.
        unsafe { (*t).entries.add((i * (*t).size) as usize) }
    }

    extern "C" fn table_free(t: *mut Table) {
        LOG.with(|l| l.borrow_mut().push(Ev::Free(t as usize)));
    }

    extern "C" fn cell_drop(h: i64, glue: usize) {
        LOG.with(|l| l.borrow_mut().push(Ev::Cell(h, glue != 0)));
    }

    extern "C" fn env_drop(env: *mut u8) {
        // SAFETY: the test's env block holds an id at offset 0.
        let id = unsafe { (env as *mut i64).read() };
        LOG.with(|l| l.borrow_mut().push(Ev::Drop(id)));
    }

    const SRC: &str = "
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f\"{self.id}\") } }
enum E { A(String), B(i64), C(R, String) }
shared struct Node { name: String, mut next: Option[Node], mut parent: weak Node }
fn main() {
    let a: (String, Vec[String]) = (String.from(\"x\"), Vec.new());
    let r: Vec[R] = Vec.new();
    let e: E = E.B(1);
    let n: Node = Node { name: String.from(\"n\"), next: None, parent: None };
    let arr: Array[String, 2] = [String.from(\"a\"), String.from(\"b\")];
    cells(OnceLock.new(), None, Atomic.new(0), VecDeque.new(), Map.new(), Set.new());
}
fn cells(o: OnceLock[R], f: Option[File], x: Atomic[i64], q: VecDeque[R], m: Map[String, R], s: Set[String]) {}
";

    /// The type displayed as `name` among the program's locals.
    fn ty_named(l: &Lowered, name: &str) -> Ty {
        l.program
            .bodies
            .values()
            .flat_map(|b| b.locals.iter())
            .map(|d| d.ty)
            .find(|&t| l.tys.display(t) == name)
            .unwrap_or_else(|| panic!("no local of type {name}"))
    }

    /// Builds the glue of each of `tys` under MCJIT, with `free` and the
    /// `R.drop` body logged; returns the engine and each glue's address.
    fn jit<'c>(c: &'c Context, l: &Lowered, tys: &[Ty]) -> (ExecutionEngine<'c>, Vec<usize>) {
        Target::initialize_native(&InitializationConfig::default()).unwrap();
        let mut cx = Cx::new(c, "glue", &l.program, &l.tys);
        cx.declare_bodies().unwrap();
        // `R.drop`, by hand (its body is `body.rs`'s): log the id.
        let (_, name) = l.program.drop_impls.iter().next().unwrap();
        let drop_r = cx.function(name).unwrap();
        let i64t = c.i64_type();
        let log = cx.module.add_function(
            "log_drop",
            c.void_type().fn_type(&[i64t.into()], false),
            Some(Linkage::External),
        );
        cx.builder
            .position_at_end(c.append_basic_block(drop_r, "entry"));
        let slot = drop_r.get_nth_param(1).unwrap().into_pointer_value();
        let this = cx.load_ptr(slot).unwrap();
        let id = cx.load_i64(this).unwrap();
        cx.builder.build_call(log, &[id.into()], "").unwrap();
        cx.builder.build_return(None).unwrap();
        // Every other body stays a declaration; give each an empty body
        // so the module verifies.
        for f in cx.fns.values() {
            if f.count_basic_blocks() == 0 {
                cx.builder
                    .position_at_end(c.append_basic_block(*f, "entry"));
                cx.builder.build_unreachable().unwrap();
            }
        }
        let glue: Vec<_> = tys.iter().map(|&t| cx.glue_fn(t)).collect();
        cx.finish_glue().unwrap();
        cx.module.verify().unwrap();
        let free = cx.libc_fn("free");
        let names: Vec<String> = glue
            .iter()
            .map(|f| f.get_name().to_str().unwrap().to_string())
            .collect();
        let engine = cx
            .module
            .create_jit_execution_engine(OptimizationLevel::None)
            .unwrap();
        engine.add_global_mapping(&free, logged_free as *const () as usize);
        engine.add_global_mapping(&log, logged_drop as *const () as usize);
        let stubs: [(&str, usize); 6] = [
            (TABLE_LEN, table_len as *const () as usize),
            (TABLE_ENTRY, table_entry as *const () as usize),
            (TABLE_FREE, table_free as *const () as usize),
            ("karac_once_lock_drop", cell_drop as *const () as usize),
            ("karac_file_drop", cell_drop as *const () as usize),
            ("karac_sender_drop", cell_drop as *const () as usize),
        ];
        for (n, a) in stubs {
            if let Some(f) = cx.module.get_function(n) {
                engine.add_global_mapping(&f, a);
            }
        }
        let addrs = names
            .iter()
            .map(|n| engine.get_function_address(n).unwrap())
            .collect();
        (engine, addrs)
    }

    fn run(glue: usize, p: *mut u8) -> Vec<Ev> {
        LOG.with(|l| l.borrow_mut().clear());
        // SAFETY: `glue` is a `void(ptr)` drop glue and `p` holds a value
        // of its type.
        let f: extern "C" fn(*mut u8) = unsafe { std::mem::transmute(glue) };
        f(p);
        LOG.with(|l| l.borrow().clone())
    }

    fn alloc(n: usize) -> *mut u8 {
        // SAFETY: plain allocation; freed by the glue under test.
        unsafe { libc::calloc(1, n.max(1)) as *mut u8 }
    }

    fn put(p: *mut u8, off: u64, v: u64) {
        // SAFETY: `off` is within the value, from its layout.
        unsafe { (p.add(off as usize) as *mut u64).write_unaligned(v) }
    }

    fn put_u8(p: *mut u8, off: u64, v: u8) {
        // SAFETY: as `put`.
        unsafe { *p.add(off as usize) = v }
    }

    /// A `String` with a heap buffer, written at `p`; the buffer.
    fn string_at(p: *mut u8) -> usize {
        let buf = alloc(4);
        put(p, 0, buf as u64);
        put(p, 8, 1);
        put(p, 16, 4);
        buf as usize
    }

    fn off(l: &Lowered, t: Ty, v: Option<u32>, f: u32) -> u64 {
        layout::field_offset(&l.tys, t, v, f).unwrap()
    }

    fn variant(l: &Lowered, t: Ty, name: &str) -> u32 {
        let (adt, _) = l.tys.tcx().adt_of(t).unwrap();
        adt.variants.iter().position(|v| v.name == name).unwrap() as u32
    }

    /// Parts drop last to first, a `Vec`'s elements first to last before
    /// its buffer, an array's first to last, and a `Drop` body runs on
    /// each element it is declared for.
    #[test]
    fn mir_llvm_glue_drops_parts_in_order() {
        let l = build_source(SRC).unwrap();
        let tup = ty_named(&l, "(String, Vec[String])");
        let vr = ty_named(&l, "Vec[R]");
        let arr = ty_named(&l, "Array[String, 2]");
        let c = Context::create();
        let (_e, g) = jit(&c, &l, &[tup, vr, arr]);

        // (String, Vec[String]): the Vec (its elements, then its buffer),
        // then the String.
        let t = alloc(48);
        let s = string_at(t);
        let v = unsafe { t.add(off(&l, tup, None, 1) as usize) };
        let buf = alloc(48);
        let (a, b) = (string_at(buf), string_at(unsafe { buf.add(24) }));
        put(v, 0, buf as u64);
        put(v, 8, 2);
        put(v, 16, 2);
        assert_eq!(
            run(g[0], t),
            vec![
                Ev::Free(a),
                Ev::Free(b),
                Ev::Free(buf as usize),
                Ev::Free(s)
            ]
        );

        // Vec[R]: each R's Drop body, first to last, then the buffer.
        let v = alloc(24);
        let buf = alloc(24);
        for i in 0..3u64 {
            put(buf, i * 8, i + 1);
        }
        put(v, 0, buf as u64);
        put(v, 8, 3);
        put(v, 16, 3);
        assert_eq!(
            run(g[1], v),
            vec![
                Ev::Drop(1),
                Ev::Drop(2),
                Ev::Drop(3),
                Ev::Free(buf as usize)
            ]
        );

        // Array[String, 2]: first to last.
        let p = alloc(48);
        let (a, b) = (string_at(p), string_at(unsafe { p.add(24) }));
        assert_eq!(run(g[2], p), vec![Ev::Free(a), Ev::Free(b)]);
        // SAFETY: the glue dropped the contents, not the memory.
        unsafe { libc::free(t as *mut c_void) };
        unsafe { libc::free(v as *mut c_void) };
        unsafe { libc::free(p as *mut c_void) };
    }

    /// An enum drops its active variant's fields, last to first; a
    /// fieldless or `Copy` variant owns nothing.
    #[test]
    fn mir_llvm_glue_drops_the_active_variant() {
        let l = build_source(SRC).unwrap();
        let e = ty_named(&l, "E");
        let c = Context::create();
        let (_e, g) = jit(&c, &l, &[e]);
        let size = layout::layout(&l.tys, e).unwrap().size as usize;

        let p = alloc(size);
        let cv = variant(&l, e, "C");
        put_u8(p, 0, cv as u8);
        put(p, off(&l, e, Some(cv), 0), 7);
        let s = string_at(unsafe { p.add(off(&l, e, Some(cv), 1) as usize) });
        assert_eq!(run(g[0], p), vec![Ev::Free(s), Ev::Drop(7)]);

        let av = variant(&l, e, "A");
        put_u8(p, 0, av as u8);
        let s = string_at(unsafe { p.add(off(&l, e, Some(av), 0) as usize) });
        assert_eq!(run(g[0], p), vec![Ev::Free(s)]);

        put_u8(p, 0, variant(&l, e, "B") as u8);
        assert_eq!(run(g[0], p), vec![]);
        unsafe { libc::free(p as *mut c_void) };
    }

    /// A `shared` handle frees its body and box at the last strong handle,
    /// and a box a weak handle still holds is freed by the last weak one.
    #[test]
    fn mir_llvm_glue_releases_shared_and_weak() {
        let l = build_source(SRC).unwrap();
        let node = ty_named(&l, "shared Node");
        let weak = l.tys.field_ty(node, None, 2).unwrap();
        let c = Context::create();
        let (_e, g) = jit(&c, &l, &[node, weak]);
        let body = layout::shared_body_offset(&l.tys, node).unwrap();
        let size = layout::body_layout(&l.tys, node).unwrap().size;
        let next_ty = l.tys.field_ty(node, None, 1).unwrap();
        let none = variant(&l, next_ty, "None");

        let new_box = |strong: u64, weak: u64| {
            let b = alloc((body + size) as usize);
            put(b, 0, strong);
            put(b, 8, weak);
            let name = string_at(unsafe { b.add((body + off(&l, node, None, 0)) as usize) });
            put_u8(b, body + off(&l, node, None, 1), none as u8);
            (b, name)
        };
        let handle = alloc(8);

        // Two strong handles: the first release frees nothing.
        let (b, name) = new_box(2, 0);
        put(handle, 0, b as u64);
        assert_eq!(run(g[0], handle), vec![]);
        assert_eq!(
            run(g[0], handle),
            vec![Ev::Free(name), Ev::Free(b as usize)]
        );

        // A weak handle keeps the box past the last strong one.
        let (b, name) = new_box(1, 1);
        put(handle, 0, b as u64);
        assert_eq!(run(g[0], handle), vec![Ev::Free(name)]);
        assert_eq!(run(g[1], handle), vec![Ev::Free(b as usize)]);

        // A body whose weak field points at its own box: releasing that
        // handle during the drop must not free the box under it.
        let (b, name) = new_box(1, 1);
        put(b, body + off(&l, node, None, 2), b as u64);
        put(handle, 0, b as u64);
        assert_eq!(
            run(g[0], handle),
            vec![Ev::Free(name), Ev::Free(b as usize)]
        );

        // An empty weak handle is null and owns nothing.
        put(handle, 0, 0);
        assert_eq!(run(g[1], handle), vec![]);
        unsafe { libc::free(handle as *mut c_void) };
    }

    fn param_ty(l: &Lowered, body: &str, i: usize) -> Ty {
        let keys: Vec<_> = l
            .program
            .bodies
            .keys()
            .filter(|k| !k.contains('.'))
            .collect();
        l.program
            .bodies
            .get(body)
            .unwrap_or_else(|| panic!("no body {body} in {keys:?}"))
            .locals[i]
            .ty
    }

    /// A `VecDeque` drops its elements front to back, across the wrap,
    /// then its buffer; an empty one with no buffer owns nothing.
    #[test]
    fn mir_llvm_glue_drops_a_ring_front_to_back() {
        let l = build_source(SRC).unwrap();
        let q = param_ty(&l, "cells", 4);
        assert_eq!(l.tys.display(q), "VecDeque[R]");
        assert_eq!(layout::layout(&l.tys, q).unwrap().size, 32);
        let c = Context::create();
        let (_e, g) = jit(&c, &l, &[q]);

        // cap 4, head 3, len 3: slots 3, 0, 1.
        let p = alloc(32);
        let buf = alloc(32);
        for (slot, id) in [(3u64, 10u64), (0, 11), (1, 12), (2, 99)] {
            put(buf, slot * 8, id);
        }
        put(p, 0, buf as u64);
        put(p, 8, 3);
        put(p, 16, 3);
        put(p, 24, 4);
        assert_eq!(
            run(g[0], p),
            vec![
                Ev::Drop(10),
                Ev::Drop(11),
                Ev::Drop(12),
                Ev::Free(buf as usize)
            ]
        );

        // No wrap: head 1, len 2.
        let buf = alloc(32);
        for (slot, id) in [(1u64, 20u64), (2, 21)] {
            put(buf, slot * 8, id);
        }
        put(p, 0, buf as u64);
        put(p, 8, 1);
        put(p, 16, 2);
        assert_eq!(
            run(g[0], p),
            vec![Ev::Drop(20), Ev::Drop(21), Ev::Free(buf as usize)]
        );

        for i in 0..4 {
            put(p, i * 8, 0);
        }
        assert_eq!(run(g[0], p), vec![]);
        unsafe { libc::free(p as *mut c_void) };
    }

    /// A map drops each entry's key then its value, in the table's order,
    /// then the table; a set drops its keys; a null handle owns nothing.
    #[test]
    fn mir_llvm_glue_drops_map_and_set_entries() {
        let l = build_source(SRC).unwrap();
        let (m, st) = (param_ty(&l, "cells", 5), param_ty(&l, "cells", 6));
        assert_eq!(l.tys.display(m), "Map[String, R]");
        let c = Context::create();
        let (_e, g) = jit(&c, &l, &[m, st]);

        // Map[String, R]: entries `(String, R)`, 32 bytes.
        let entries = alloc(64);
        let k0 = string_at(entries);
        put(entries, 24, 1);
        let k1 = string_at(unsafe { entries.add(32) });
        put(entries, 56, 2);
        let mut t = Table {
            n: 2,
            size: 32,
            entries,
        };
        let tp = &mut t as *mut Table;
        let h = alloc(8);
        put(h, 0, tp as u64);
        assert_eq!(
            run(g[0], h),
            vec![
                Ev::Free(k0),
                Ev::Drop(1),
                Ev::Free(k1),
                Ev::Drop(2),
                Ev::Free(tp as usize)
            ]
        );

        // Set[String].
        let k0 = string_at(entries);
        // SAFETY: `tp` points at `t`, which outlives the call.
        unsafe {
            (*tp).n = 1;
            (*tp).size = 24;
        }
        assert_eq!(run(g[1], h), vec![Ev::Free(k0), Ev::Free(tp as usize)]);

        put(h, 0, 0);
        assert_eq!(run(g[0], h), vec![]);
        unsafe { libc::free(entries as *mut c_void) };
        unsafe { libc::free(h as *mut c_void) };
    }

    /// An erased function drops its captures through its `drop` and frees
    /// their block; no captures (a null env) owns nothing, and captures
    /// with no glue are only freed.
    #[test]
    fn mir_llvm_glue_drops_an_erased_fn() {
        let l = build_source(SRC).unwrap();
        let i64t = l.tys.int(crate::mir::ty::IntTy::I64);
        let f = l.tys.intern(TyKind::Fn {
            params: vec![i64t],
            ret: i64t,
            kind: crate::mir::ty::FnKind::Fn,
        });
        assert_eq!(layout::layout(&l.tys, f).unwrap().size, 24);
        let c = Context::create();
        let (_e, g) = jit(&c, &l, &[f]);

        let p = alloc(24);
        let env = alloc(8);
        put(env, 0, 5);
        put(p, 8, env as u64);
        put(p, 16, env_drop as *const () as u64);
        assert_eq!(run(g[0], p), vec![Ev::Drop(5), Ev::Free(env as usize)]);

        let env = alloc(8);
        put(p, 8, env as u64);
        put(p, 16, 0);
        assert_eq!(run(g[0], p), vec![Ev::Free(env as usize)]);

        put(p, 8, 0);
        assert_eq!(run(g[0], p), vec![]);
        unsafe { libc::free(p as *mut c_void) };
    }

    /// A library cell hands its handle, and the held type's glue when it
    /// owns anything, to the runtime; a zero handle and an `Atomic` own
    /// nothing.
    #[test]
    fn mir_llvm_glue_drops_library_cells_through_the_runtime() {
        let l = build_source(SRC).unwrap();
        let (o, fl, at) = (
            param_ty(&l, "cells", 1),
            l.tys.tcx().adt_of(param_ty(&l, "cells", 2)).unwrap().1[0],
            param_ty(&l, "cells", 3),
        );
        assert_eq!(l.tys.display(fl), "File");
        assert_eq!(layout::layout(&l.tys, fl).unwrap().size, 8);
        assert_eq!(layout::layout(&l.tys, at).unwrap().size, 8);
        let c = Context::create();
        let (_e, g) = jit(&c, &l, &[o, fl, at]);
        let h = alloc(8);
        put(h, 0, 7);
        assert_eq!(run(g[0], h), vec![Ev::Cell(7, true)]);
        assert_eq!(run(g[1], h), vec![Ev::Cell(7, false)]);
        assert_eq!(run(g[2], h), vec![]);
        put(h, 0, 0);
        assert_eq!(run(g[0], h), vec![]);
        unsafe { libc::free(h as *mut c_void) };
    }
}
