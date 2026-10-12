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
use crate::mir::coroutine;
use crate::mir::layout::{self, SHARED_COUNTS};
use crate::mir::place_ty::place_ty;
use crate::mir::ty::{AdtId, IntrinsicTy, TyKind};
use crate::mir::{BasicBlock, Place, Ty};

/// How the runtime drops a library cell.
#[derive(Clone, Copy)]
enum CellDrop {
    /// `void(ptr handle)`: the runtime owns everything behind the handle.
    /// A channel end's undelivered payloads are dropped through the glue
    /// `karac_runtime_channel_set_elem_drop` recorded on the channel.
    Free(&'static str),
    /// A `OnceLock` / `OnceCell`: the held value, read through
    /// `karac_runtime_once_get` (null while unset), drops through its own
    /// glue, then `karac_runtime_once_free` frees the cell.
    Once,
    /// An `Arena`: its items drop through their glue, last pushed first,
    /// then `karac_mir_arena_free` frees it.
    Arena,
}

/// The library cells whose contents live in the runtime, behind a one-word
/// handle at offset 0 (the interpreter drops them by hand in
/// `Interp::drop_adt`), with how the runtime drops one. A null handle (a
/// hand-built literal) owns nothing. An unjoined `TaskHandle` is detached:
/// the task runs on and reaps itself. `Atomic[T]` holds a `T` in place,
/// which is never a type that owns anything.
const LIBRARY_CELLS: &[(&str, CellDrop)] = &[
    (
        "Sender",
        CellDrop::Free("karac_runtime_channel_drop_sender"),
    ),
    (
        "Receiver",
        CellDrop::Free("karac_runtime_channel_drop_receiver"),
    ),
    ("File", CellDrop::Free("karac_runtime_file_close")),
    ("OnceLock", CellDrop::Once),
    ("OnceCell", CellDrop::Once),
    ("TaskHandle", CellDrop::Free("karac_runtime_task_detach")),
    ("Arena", CellDrop::Arena),
];

const ONCE_GET: &str = "karac_runtime_once_get";
const ONCE_FREE: &str = "karac_runtime_once_free";

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

    /// Whether glue has been requested and not built yet.
    pub(super) fn glue_pending(&self) -> bool {
        !self.glue.todo.is_empty()
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

    pub(super) fn call_glue(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<()> {
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
                let name = self.tys.adt(a).name.clone();
                if let Some(co) = coroutine::frame_owner(&name) {
                    // A coroutine frame's fields are initialized only as its
                    // state says, so its own `drop_frame` drops it.
                    let slot = self.entry_slot(8, "frame")?;
                    self.builder
                        .build_store(slot, p)
                        .map_err(|e| e.to_string())?;
                    let f = self.function(&coroutine::drop_frame_name(co))?;
                    let ret = self.entry_slot(0, "unit")?;
                    self.builder
                        .build_call(f, &[ret.into(), slot.into()], "")
                        .map_err(|e| e.to_string())?;
                    return Ok(());
                }
                if let Some(rt) = self.cell_drop(a) {
                    return self.drop_cell(p, ty, rt);
                }
                if self.tys.adt(a).name == "Atomic" {
                    return Ok(());
                }
                if self.tys.adt(a).name == "Mutex" {
                    // A `Mutex[T]` holds its `T` in place.
                    if let Some(held) = self
                        .tys
                        .tcx()
                        .adt_of(ty)
                        .and_then(|(_, a)| a.first().copied())
                    {
                        if self.tys.needs_drop(held) {
                            self.call_glue(p, held)?;
                        }
                    }
                    return Ok(());
                }
                // The guard of `critical_section.acquire()` restores
                // nothing: interrupts were never masked.
                if self.tys.adt(a).name == "CriticalSectionGuard" {
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
                self.free_buffer_at(p, 24)
            }
            TyKind::Intrinsic(
                IntrinsicTy::Map(..)
                | IntrinsicTy::SortedMap(..)
                | IntrinsicTy::Set(_)
                | IntrinsicTy::SortedSet(_),
            ) => self.drop_table(p, ty),
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

    /// How the runtime drops the library cell `a`, if it is one.
    fn cell_drop(&self, a: AdtId) -> Option<CellDrop> {
        let name = self.tys.adt(a).name.clone();
        LIBRARY_CELLS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|&(_, d)| d)
    }

    /// Drops the library cell of type `ty` at `p` through the runtime.
    fn drop_cell(&mut self, p: PointerValue<'ctx>, ty: Ty, how: CellDrop) -> R<()> {
        let pt = self.ptr_ty();
        let void = self.llcx.void_type();
        let h = self.load_ptr(p)?;
        let null = self
            .builder
            .build_is_null(h, "")
            .map_err(|e| e.to_string())?;
        let some = self
            .builder
            .build_not(null, "")
            .map_err(|e| e.to_string())?;
        match how {
            CellDrop::Free(rt) => {
                let f = self.extern_fn(rt, void.fn_type(&[pt.into()], false));
                self.if_then(some, |cx| {
                    cx.builder
                        .build_call(f, &[h.into()], "")
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })
            }
            CellDrop::Arena => {
                let held = self
                    .tys
                    .tcx()
                    .adt_of(ty)
                    .and_then(|(_, args)| args.first().copied())
                    .ok_or("an Arena has no item type")?;
                let free =
                    self.extern_fn("karac_mir_arena_free", void.fn_type(&[pt.into()], false));
                self.if_then(some, |cx| {
                    let n = cx.load_i64(cx.offset(h, 8)?)?;
                    cx.drop_back_to(h, cx.i64(0), n, held)?;
                    cx.builder
                        .build_call(free, &[h.into()], "")
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })
            }
            CellDrop::Once => {
                let held = self
                    .tys
                    .tcx()
                    .adt_of(ty)
                    .and_then(|(_, args)| args.first().copied())
                    .filter(|&t| self.tys.needs_drop(t));
                let get = self.extern_fn(ONCE_GET, pt.fn_type(&[pt.into()], false));
                let free = self.extern_fn(ONCE_FREE, void.fn_type(&[pt.into()], false));
                self.if_then(some, |cx| {
                    if let Some(t) = held {
                        let v = cx
                            .builder
                            .build_call(get, &[h.into()], "held")
                            .map_err(|e| e.to_string())?
                            .try_as_basic_value()
                            .basic()
                            .ok_or("karac_runtime_once_get returns nothing")?
                            .into_pointer_value();
                        let set = cx
                            .builder
                            .build_is_not_null(v, "")
                            .map_err(|e| e.to_string())?;
                        cx.if_then(set, |cx| cx.call_glue(v, t))?;
                    }
                    cx.builder
                        .build_call(free, &[h.into()], "")
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })
            }
        }
    }

    /// Drops the items `from..to` of the vector at `v`, last first.
    pub(super) fn drop_back_to(
        &mut self,
        v: PointerValue<'ctx>,
        from: IntValue<'ctx>,
        to: IntValue<'ctx>,
        e: Ty,
    ) -> R<()> {
        if !self.tys.needs_drop(e) {
            return Ok(());
        }
        let size = self.layout(e)?.size;
        let buf = self.load_ptr(v)?;
        let n = self
            .builder
            .build_int_sub(to, from, "")
            .map_err(|e| e.to_string())?;
        self.count_loop(n, |cx, i| {
            let j = cx
                .builder
                .build_int_sub(to, i, "")
                .map_err(|e| e.to_string())?;
            let j = cx
                .builder
                .build_int_sub(j, cx.i64(1), "")
                .map_err(|e| e.to_string())?;
            let x = cx.elem_ptr(buf, j, size)?;
            cx.call_glue(x, e)
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
        self.free_buffer_at(p, 16)
    }

    /// Frees the buffer at offset 0 of `p` when the capacity at `cap_at`
    /// is not zero.
    fn free_buffer_at(&mut self, p: PointerValue<'ctx>, cap_at: u64) -> R<()> {
        let cap = self.load_i64(self.offset(p, cap_at)?)?;
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
    pub(super) fn count_loop(
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

    pub(super) fn field_offset(&self, ty: Ty, v: Option<u32>, f: u32) -> R<u64> {
        layout::field_offset(self.tys, ty, v, f)
            .ok_or_else(|| format!("`{}` has no field {f}", self.tys.display(ty)))
    }

    pub(super) fn cur_fn(&self) -> R<FunctionValue<'ctx>> {
        self.builder
            .get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or_else(|| "the builder is not in a function".to_string())
    }

    /// An 8-aligned stack slot of `size` bytes in the current function's
    /// entry block, so a loop does not grow the stack.
    pub(super) fn entry_slot(&self, size: u32, name: &str) -> R<PointerValue<'ctx>> {
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

    pub(super) fn load_ptr(&self, p: PointerValue<'ctx>) -> R<PointerValue<'ctx>> {
        Ok(self
            .builder
            .build_load(self.ptr_ty(), p, "")
            .map_err(|e| e.to_string())?
            .into_pointer_value())
    }

    pub(super) fn load_i64(&self, p: PointerValue<'ctx>) -> R<IntValue<'ctx>> {
        Ok(self
            .builder
            .build_load(self.llcx.i64_type(), p, "")
            .map_err(|e| e.to_string())?
            .into_int_value())
    }

    pub(super) fn is_zero(&self, v: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        self.builder
            .build_int_compare(IntPredicate::EQ, v, v.get_type().const_zero(), "")
            .map_err(|e| e.to_string())
    }

    pub(super) fn free(&self, p: PointerValue<'ctx>) -> R<()> {
        let free = self.libc_fn("free");
        self.builder
            .build_call(free, &[p.into()], "")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// `if cond { then }`, leaving the builder after it.
    pub(super) fn if_then(
        &mut self,
        cond: IntValue<'ctx>,
        then: impl FnOnce(&mut Self) -> R<()>,
    ) -> R<()> {
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
        /// A library cell's runtime free, and its handle.
        Cell(&'static str, usize),
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

    extern "C" fn closed(h: usize) {
        LOG.with(|l| l.borrow_mut().push(Ev::Cell("close", h)));
    }

    extern "C" fn once_free(h: usize) {
        LOG.with(|l| l.borrow_mut().push(Ev::Cell("once_free", h)));
    }

    /// A `OnceLock` stand-in whose handle is the address of its held value,
    /// or of a zero word while unset.
    extern "C" fn once_get(h: *mut u64) -> *mut u64 {
        // SAFETY: the test's handle points at a readable word.
        if unsafe { *h } == 0 {
            std::ptr::null_mut()
        } else {
            h
        }
    }

    /// The real runtime table: keys hashed and compared by their first word.
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
fn cells(o: own OnceLock[R], f: own Option[File], x: own Atomic[i64], q: own VecDeque[R], m: own Map[String, R], s: own Set[String]) {}
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
        use karac_runtime::mir_map as rt;
        let stubs: [(&str, usize); 8] = [
            (
                "karac_mir_map_len",
                rt::karac_mir_map_len as *const () as usize,
            ),
            (
                "karac_mir_map_at",
                rt::karac_mir_map_at as *const () as usize,
            ),
            (
                "karac_mir_map_free",
                rt::karac_mir_map_free as *const () as usize,
            ),
            (ONCE_GET, once_get as *const () as usize),
            (ONCE_FREE, once_free as *const () as usize),
            ("karac_runtime_file_close", closed as *const () as usize),
            (
                "karac_runtime_channel_drop_sender",
                closed as *const () as usize,
            ),
            ("karac_runtime_task_detach", closed as *const () as usize),
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

    /// A map drops each entry's key then its value, then the table; a set
    /// drops its keys; a null handle owns nothing. The table is the real
    /// runtime's, whose order is unspecified, so only the pairing is
    /// asserted.
    #[test]
    fn mir_llvm_glue_drops_map_and_set_entries() {
        use karac_runtime::mir_map as rt;
        let l = build_source(SRC).unwrap();
        let (m, st) = (param_ty(&l, "cells", 5), param_ty(&l, "cells", 6));
        assert_eq!(l.tys.display(m), "Map[String, R]");
        let c = Context::create();
        let (_e, g) = jit(&c, &l, &[m, st]);
        let h = alloc(8);
        let kv = alloc(32);

        // Map[String, R]: entries in insertion order, each key before its
        // value.
        put(h, 0, 0);
        let mut keys = Vec::new();
        for id in 1..=2u64 {
            keys.push(string_at(kv));
            put(kv, 24, id);
            // SAFETY: `h` holds a table handle; `kv` is one 32-byte entry.
            unsafe { rt::karac_mir_map_insert(h as *mut _, 32, kv, None, None) };
        }
        assert_eq!(
            run(g[0], h),
            vec![
                Ev::Free(keys[0]),
                Ev::Drop(1),
                Ev::Free(keys[1]),
                Ev::Drop(2)
            ]
        );

        // Set[String].
        put(h, 0, 0);
        let k = string_at(kv);
        // SAFETY: as above, with 24-byte entries.
        unsafe { rt::karac_mir_map_insert(h as *mut _, 24, kv, None, None) };
        assert_eq!(run(g[1], h), vec![Ev::Free(k)]);

        put(h, 0, 0);
        assert_eq!(run(g[0], h), vec![]);
        unsafe { libc::free(kv as *mut c_void) };
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

    /// A library cell hands its handle to the runtime's free; a
    /// `OnceLock` drops its held value first, when it is set; a null handle
    /// and an `Atomic` own nothing.
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
        let (h, cell) = (alloc(8), alloc(8));
        put(h, 0, cell as u64);
        let c = cell as usize;

        // OnceLock[R], set to R { id: 7 }, then unset.
        put(cell, 0, 7);
        assert_eq!(run(g[0], h), vec![Ev::Drop(7), Ev::Cell("once_free", c)]);
        put(cell, 0, 0);
        assert_eq!(run(g[0], h), vec![Ev::Cell("once_free", c)]);
        assert_eq!(run(g[1], h), vec![Ev::Cell("close", c)]);
        assert_eq!(run(g[2], h), vec![]);
        put(h, 0, 0);
        assert_eq!(run(g[0], h), vec![]);
        assert_eq!(run(g[1], h), vec![]);
        unsafe { libc::free(cell as *mut c_void) };
        unsafe { libc::free(h as *mut c_void) };
    }
}
