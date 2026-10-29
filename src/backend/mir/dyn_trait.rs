//! `dyn Trait` values (design.md § `dyn Trait`): the coercions, the calls
//! through a method table, and the drop of an owned value.
//!
//! A `dyn Trait`, and each of its views (`ref dyn`, `mut ref dyn`), is two
//! words: the address of the concrete value and the program's table for
//! its type (`Program.vtables`). An owned value lives in a malloc'd block
//! of its own, or, when it has no size, at a dangling address that is
//! never freed, so the first word is never null. The table is a constant:
//!
//! ```text
//! { ptr drop, i64 size, i64 align, ptr slot_0, ..., ptr slot_n }
//! ```
//!
//! `drop` is the concrete type's glue, or null when it owns nothing. Each
//! slot is a [`Shim::DynSlot`] taking `(ptr ret, ptr data, ptr arg...)`,
//! which hands the method its receiver the way its body takes it: the
//! address for a `ref self` or `mut ref self` body, a copy of the value
//! for one taking it by value (a Copy type, or `own self`). A call that
//! consumes the owned value frees its block after the slot returns; the
//! value itself went to the method. Reference semantics: the interpreter's
//! `call_dyn` and the `Dyn` arm of its drop (`src/mir/interp.rs`).

use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};

use super::callable::{Arg, Callee, Shim};
use super::{Cx, R};
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

/// Where the first slot is in a table: after `drop`, `size` and `align`.
const FIRST_SLOT: u64 = 24;

impl<'ctx> Cx<'ctx, '_> {
    /// The table of the program's vtable `vt`, made on first use.
    fn vtable_ptr(&mut self, vt: u32) -> R<PointerValue<'ctx>> {
        let name = format!("vtable.{vt}");
        if let Some(g) = self.module.get_global(&name) {
            return Ok(g.as_pointer_value());
        }
        let program = self.program;
        let table = program
            .vtables
            .get(vt as usize)
            .ok_or_else(|| format!("no vtable {vt}"))?;
        let l = self.layout(table.concrete)?;
        let null = self.ptr_ty().const_null();
        let drop = if self.tys.needs_drop(table.concrete) {
            self.glue_fn(table.concrete)
                .as_global_value()
                .as_pointer_value()
        } else {
            null
        };
        let mut parts: Vec<BasicValueEnum> = vec![
            drop.into(),
            self.i64(l.size).into(),
            self.i64(l.align).into(),
        ];
        for slot in 0..table.slots.len() as u32 {
            let f = self.shim_fn(Shim::DynSlot { vtable: vt, slot });
            parts.push(f.as_global_value().as_pointer_value().into());
        }
        let init = self.llcx.const_struct(&parts, false);
        let g = self.module.add_global(init.get_type(), None, &name);
        g.set_initializer(&init);
        g.set_constant(true);
        g.set_linkage(Linkage::Internal);
        g.set_unnamed_addr(true);
        Ok(g.as_pointer_value())
    }

    /// `CastKind::ToDyn(vt)` into `dst`: an owned value moved into a block
    /// of its own (`to` is `Dyn`), or a reference paired with the table
    /// (`to` is a view), which allocates nothing.
    pub(super) fn coerce_dyn(
        &mut self,
        dst: PointerValue<'ctx>,
        a: &Operand,
        vt: u32,
        to: Ty,
    ) -> R<()> {
        let table = self.vtable_ptr(vt)?;
        let data = match self.tys.kind(to) {
            TyKind::Dyn { .. } => {
                let concrete = self.program.vtables[vt as usize].concrete;
                let l = self.layout(concrete)?;
                if l.align > 16 {
                    return Err(format!(
                        "MIR to LLVM: a `dyn` value of the {}-aligned `{}` is not lowered yet",
                        l.align,
                        self.tys.display(concrete)
                    ));
                }
                if l.size == 0 {
                    let at = self.i64(l.align.max(1));
                    self.builder
                        .build_int_to_ptr(at, self.ptr_ty(), "dangling")
                        .map_err(e)?
                } else {
                    let malloc = self.libc_fn("malloc");
                    let block = self
                        .builder
                        .build_call(malloc, &[self.i64(l.size).into()], "dyn")
                        .map_err(e)?
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_pointer_value();
                    self.store_operand(block, a)?;
                    block
                }
            }
            TyKind::Ref(_) | TyKind::MutRef(_) => {
                let p = self.operand_ptr(a)?;
                self.load_ptr(p)?
            }
            _ => {
                return Err(format!(
                    "MIR to LLVM: a coercion to `dyn` into a `{}`",
                    self.tys.display(to)
                ))
            }
        };
        self.store(dst, data.into())?;
        self.store(self.offset(dst, 8)?, table.into())
    }

    /// A call of the method at `slot` of the receiver `args[0]`'s table: a
    /// view, or an owned value moved in by an `own self` call, whose block
    /// is freed once the method has the value.
    pub(super) fn call_dyn(&mut self, slot: u32, args: &[Operand], dest: &Place) -> R<()> {
        let (recv, rest) = args
            .split_first()
            .ok_or("a call through a `dyn` value has no receiver")?;
        let owned = matches!(self.tys.kind(self.operand_type(recv)?), TyKind::Dyn { .. });
        let pair = self.operand_ptr(recv)?;
        let data = self.load_ptr(pair)?;
        let table = self.load_ptr(self.offset(pair, 8)?)?;
        let code = self.load_ptr(self.offset(table, FIRST_SLOT + 8 * u64::from(slot))?)?;
        let dty = self.place_type(dest)?;
        let ret = self.temp(dty, "ret")?;
        let mut vals: Vec<BasicMetadataValueEnum> = vec![ret.into(), data.into()];
        for a in rest {
            let t = self.operand_type(a)?;
            let at = self.temp(t, "arg")?;
            self.store_operand(at, a)?;
            vals.push(at.into());
        }
        let p = self.ptr_ty();
        let ps: Vec<BasicMetadataTypeEnum> = vals.iter().map(|_| p.into()).collect();
        let fn_ty = self.llcx.void_type().fn_type(&ps, false);
        self.builder
            .build_indirect_call(fn_ty, code, &vals, "")
            .map_err(e)?;
        if owned {
            self.free_dyn_block(data, table)?;
        }
        let d = self.place_ptr(dest)?;
        self.copy(d, ret, dty)
    }

    /// Frees the block of an owned `dyn` value at `data`, unless its type
    /// has no size (then `data` is a dangling address).
    fn free_dyn_block(&mut self, data: PointerValue<'ctx>, table: PointerValue<'ctx>) -> R<()> {
        let size = self.load_i64(self.offset(table, 8)?)?;
        let sized = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                size,
                size.get_type().const_zero(),
                "",
            )
            .map_err(e)?;
        self.if_then(sized, |cx| cx.free(data))
    }

    /// The drop of the owned `dyn` value at `p`: the concrete value's
    /// glue, then its block.
    pub(super) fn drop_dyn(&mut self, p: PointerValue<'ctx>) -> R<()> {
        let data = self.load_ptr(p)?;
        let table = self.load_ptr(self.offset(p, 8)?)?;
        let glue = self.load_ptr(table)?;
        let has = self.builder.build_is_not_null(glue, "").map_err(e)?;
        self.if_then(has, |cx| {
            let ty = cx.llcx.void_type().fn_type(&[cx.ptr_ty().into()], false);
            cx.builder
                .build_indirect_call(ty, glue, &[data.into()], "")
                .map_err(e)?;
            Ok(())
        })?;
        self.free_dyn_block(data, table)
    }

    /// The type of [`Shim::DynSlot`] `slot` of table `vt`: `void (ptr ret,
    /// ptr data, ptr arg...)`, one `arg` for each parameter after the
    /// receiver. Its name, too.
    pub(super) fn dyn_slot_sig(
        &self,
        vt: u32,
        slot: u32,
    ) -> (String, inkwell::types::FunctionType<'ctx>) {
        let program = self.program;
        let n = program
            .vtables
            .get(vt as usize)
            .and_then(|t| t.slots.get(slot as usize))
            .and_then(|name| program.bodies.get(name))
            .map_or(0, |b| b.arg_count.saturating_sub(1));
        let p = self.ptr_ty();
        let ps: Vec<BasicMetadataTypeEnum> = (0..n + 2).map(|_| p.into()).collect();
        (
            format!("dyn_slot.{vt}.{slot}"),
            self.llcx.void_type().fn_type(&ps, false),
        )
    }

    /// The body of [`Shim::DynSlot`] `slot` of table `vt`, the builder in
    /// its entry block: the slot's method called on the value at `data`.
    pub(super) fn build_dyn_slot(&mut self, f: FunctionValue<'ctx>, vt: u32, slot: u32) -> R<()> {
        let program = self.program;
        let table = program
            .vtables
            .get(vt as usize)
            .ok_or_else(|| format!("no vtable {vt}"))?;
        let name = table
            .slots
            .get(slot as usize)
            .ok_or_else(|| format!("no slot {slot} in vtable {vt}"))?
            .clone();
        let body = program
            .bodies
            .get(&name)
            .ok_or_else(|| format!("no body for the `dyn` slot `{name}`"))?;
        let param = |i: u32| -> R<PointerValue<'ctx>> {
            Ok(f.get_nth_param(i)
                .ok_or("a `dyn` slot is missing a parameter")?
                .into_pointer_value())
        };
        let (ret, data) = (param(0)?, param(1)?);
        let by_ref = matches!(
            self.tys.kind(body.locals[1].ty),
            TyKind::Ref(_) | TyKind::MutRef(_)
        );
        // By value: the method gets a copy it owns; the caller frees the
        // block of a value it moved in, and keeps a view's.
        let recv = if by_ref {
            data
        } else {
            let at = self.temp(table.concrete, "self")?;
            self.copy(at, data, table.concrete)?;
            at
        };
        let args: Vec<Arg> = (1..body.arg_count)
            .map(|i| Ok((param(1 + i as u32)?, body.locals[1 + i].ty)))
            .collect::<R<_>>()?;
        let c = Callee::Body {
            name,
            env: Some((recv, !by_ref)),
        };
        self.call_callee(&c, ret, &args)?;
        self.builder.build_return(None).map_err(e)?;
        Ok(())
    }
}
