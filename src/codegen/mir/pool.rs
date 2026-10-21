//! `Pool[T]` and `PooledConnection[T]`, with the interpreter's
//! `pool_method` semantics (`src/mir/interp.rs`). A `Pool` holds a pointer
//! to the runtime's pool (`karac_mir_pool_*`, null for a hand-built
//! literal, which every `acquire` answers `PoolClosed`); the pool keeps
//! the factory, the health check and the idle values as bytes, and
//! codegen calls the functions it holds. A checked-out connection holds a
//! reference on its pool, so a pool outlives every connection it lent.

use inkwell::values::{IntValue, PointerValue};
use inkwell::IntPredicate;

use super::callable::Callee;
use super::{Cx, R};
use crate::mir::ty::FnKind;
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

/// The connection id of a connection lent to `release` by reference,
/// whose value went back to the pool: its drop does nothing.
const RELEASED: u64 = u64::MAX;

impl<'ctx> Cx<'ctx, '_> {
    /// `None` for a method not lowered here.
    pub(super) fn pool_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        match (method, args) {
            ("new", [create, max, _]) => {
                let el = self.type_arg(dty)?;
                // The factory moves into the pool, erased.
                let to = self.tys.intern(TyKind::Fn {
                    params: vec![],
                    ret: el,
                    kind: FnKind::Fn,
                });
                let cp = self.erased_operand(create, to)?;
                let max = self.int_arg(max)?;
                let size = self.i64(self.layout(el)?.size);
                let glue = self.glue_or_null(el);
                let h = self.mrt_ptr(
                    "pool_new",
                    &[size.into(), max.into(), cp.into(), glue.into()],
                )?;
                let d = self.place_ptr(dest)?;
                self.store(d, h.into())?;
            }
            ("with_health_check", [recv, check]) => {
                let h = self.pool_handle(recv)?;
                let el = self.type_arg(dty)?;
                let kt = self.check_ty(el);
                // A check whose parameter takes the connection by value
                // would own the idle value, which the interpreter refuses
                // (`pool_check`); every other check is lent it.
                if self.tys.needs_drop(el) && self.check_takes_value(check)? {
                    self.abort_with("a health check that takes the connection by value")?;
                    let f = self.cur_fn()?;
                    let dead = self.llcx.append_basic_block(f, "pool.dead");
                    self.builder.position_at_end(dead);
                }
                let kp = self.erased_operand(check, kt)?;
                let d = self.place_ptr(dest)?;
                self.store(d, h.into())?;
                let closed = self.builder.build_is_null(h, "").map_err(e)?;
                let f = self.cur_fn()?;
                let (none, some, done) = (
                    self.llcx.append_basic_block(f, "pool.closed"),
                    self.llcx.append_basic_block(f, "pool.share"),
                    self.llcx.append_basic_block(f, "pool"),
                );
                self.builder
                    .build_conditional_branch(closed, none, some)
                    .map_err(e)?;
                // A closed pool keeps no check: it drops.
                self.builder.position_at_end(none);
                self.call_glue(kp, kt)?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(some);
                self.mrt_ptr("pool_share", &[h.into(), kp.into()])?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(done);
            }
            ("acquire", [recv, _]) => self.pool_acquire(recv, dest)?,
            // B-2026-10-21-45: a hand-built pool has nothing to close.
            ("close", [recv]) => {
                let h = self.pool_handle(recv)?;
                let live = self.builder.build_is_not_null(h, "").map_err(e)?;
                self.if_then(live, |cx| cx.rt("pool_close", &[h.into()]))?;
            }
            ("release", [recv, conn]) => {
                let into = self.pool_handle(recv)?;
                let mut t = self.operand_type(conn)?;
                let mut p = self.operand_ptr(conn)?;
                let mut lent = false;
                while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
                    p = self.load_ptr(p)?;
                    t = i;
                    lent = true;
                }
                self.pool_conn_return(p, t, into)?;
                if lent {
                    let at = self.offset(p, self.field_offset(t, None, 1)?)?;
                    let gone = self.llcx.i64_type().const_int(RELEASED, false);
                    self.store(at, gone.into())?;
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// The first type argument of the ADT `t`.
    fn type_arg(&self, t: Ty) -> R<Ty> {
        self.tys
            .tcx()
            .adt_of(t)
            .and_then(|(_, a)| a.first().copied())
            .ok_or_else(|| format!("`{}` has no type argument", self.tys.display(t)))
    }

    fn glue_or_null(&mut self, t: Ty) -> PointerValue<'ctx> {
        if self.tys.needs_drop(t) {
            self.glue_fn(t).as_global_value().as_pointer_value()
        } else {
            self.ptr_ty().const_null()
        }
    }

    /// The runtime pool a `Pool` operand (or a reference to one) holds.
    fn pool_handle(&mut self, a: &Operand) -> R<PointerValue<'ctx>> {
        let mut t = self.operand_type(a)?;
        let mut p = self.operand_ptr(a)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            p = self.load_ptr(p)?;
            t = i;
        }
        self.load_ptr(p)
    }

    /// `pool.acquire(timeout)`: an idle value that passes the health check
    /// (a failing one is evicted), else a fresh one from the factory while
    /// the pool is below its bound, else `Timeout`; `PoolClosed` for a
    /// hand-built or closed pool.
    fn pool_acquire(&mut self, recv: &Operand, dest: &Place) -> R<()> {
        let dty = self.place_type(dest)?;
        let h = self.pool_handle(recv)?;
        let d = self.place_ptr(dest)?;
        let okv = self.variant_of(dty, "Ok")?;
        let errv = self.variant_of(dty, "Err")?;
        let ct = self
            .tys
            .field_ty(dty, Some(okv), 0)
            .ok_or("an `Ok` without a payload")?;
        let et = self
            .tys
            .field_ty(dty, Some(errv), 0)
            .ok_or("an `Err` without a payload")?;
        let el = self.type_arg(ct)?;
        let conn = self.offset(d, self.field_offset(dty, Some(okv), 0)?)?;
        let val = self.offset(conn, self.field_offset(ct, None, 2)?)?;
        let create_ty = self.tys.intern(TyKind::Fn {
            params: vec![],
            ret: el,
            kind: FnKind::Fn,
        });
        let check_ty = self.check_ty(el);
        let f = self.cur_fn()?;
        let blocks = [
            "pool.closed",
            "pool.idle",
            "pool.pop",
            "pool.check",
            "pool.evict",
            "pool.mint",
            "pool.timeout",
            "pool.out",
            "pool.acquired",
        ]
        .map(|n| self.llcx.append_basic_block(f, n));
        let [closed, idle, pop, check, evict, mint, timeout, out, done] = blocks;
        let open = self.llcx.append_basic_block(f, "pool.open");
        let null = self.builder.build_is_null(h, "").map_err(e)?;
        self.builder
            .build_conditional_branch(null, closed, open)
            .map_err(e)?;
        // A closed pool lends nothing (B-2026-10-21-45).
        self.builder.position_at_end(open);
        let shut = self.rt_flag("pool_closed", &[h.into()])?;
        self.builder
            .build_conditional_branch(shut, closed, idle)
            .map_err(e)?;

        let fail = |cx: &mut Self, why: &str| -> R<()> {
            let q = cx.offset(d, cx.field_offset(dty, Some(errv), 0)?)?;
            let v = cx.variant_of(et, why)?;
            cx.store_tag(q, et, v)?;
            cx.store_tag(d, dty, errv)?;
            cx.builder.build_unconditional_branch(done).map_err(e)?;
            Ok(())
        };
        self.builder.position_at_end(closed);
        fail(self, "PoolClosed")?;
        self.builder.position_at_end(timeout);
        fail(self, "Timeout")?;

        // Idle values first, newest first, each through the check.
        self.builder.position_at_end(idle);
        let got = self.rt_flag("pool_pop", &[h.into(), val.into()])?;
        self.builder
            .build_conditional_branch(got, pop, mint)
            .map_err(e)?;
        self.builder.position_at_end(pop);
        let one = self.llcx.i8_type().const_int(1, false);
        let kp = self.mrt_ptr("pool_fn", &[h.into(), one.into()])?;
        let unchecked = self.builder.build_is_null(kp, "").map_err(e)?;
        self.builder
            .build_conditional_branch(unchecked, out, check)
            .map_err(e)?;
        self.builder.position_at_end(check);
        self.pool_check(kp, check_ty, val, el, out, evict)?;
        // A failing value is evicted: dropped, and its slot freed.
        self.builder.position_at_end(evict);
        if self.tys.needs_drop(el) {
            self.call_glue(val, el)?;
        }
        self.mrt_i8("pool_count", &[h.into(), self.i64(u64::MAX).into()])?;
        self.builder.build_unconditional_branch(idle).map_err(e)?;

        // None idle: a fresh one, below the bound.
        self.builder.position_at_end(mint);
        let room = self.rt_flag("pool_count", &[h.into(), self.i64(0).into()])?;
        let make = self.llcx.append_basic_block(f, "pool.make");
        self.builder
            .build_conditional_branch(room, make, timeout)
            .map_err(e)?;
        self.builder.position_at_end(make);
        let zero = self.llcx.i8_type().const_zero();
        let cp = self.mrt_ptr("pool_fn", &[h.into(), zero.into()])?;
        let c = Callee::Erased {
            at: cp,
            ty: create_ty,
            consume: false,
        };
        self.call_callee(&c, val, &[])?;
        self.mrt_i8("pool_count", &[h.into(), self.i64(1).into()])?;
        self.builder.build_unconditional_branch(out).map_err(e)?;

        self.builder.position_at_end(out);
        let id = self.mrt_i64("pool_check_out", &[h.into()])?;
        self.store(
            self.offset(conn, self.field_offset(ct, None, 0)?)?,
            h.into(),
        )?;
        self.store(
            self.offset(conn, self.field_offset(ct, None, 1)?)?,
            id.into(),
        )?;
        self.store_tag(d, dty, okv)?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(done);
        Ok(())
    }

    /// Hands the value of the connection at `p` (of type `t`) back to its
    /// pool, or to `into` when not null (a connection from another pool is
    /// not taken), dropping it when the pool does not take it. A
    /// connection already released does nothing.
    pub(super) fn pool_conn_return(
        &mut self,
        p: PointerValue<'ctx>,
        t: Ty,
        into: PointerValue<'ctx>,
    ) -> R<()> {
        let el = self.type_arg(t)?;
        let h = self.load_ptr(self.offset(p, self.field_offset(t, None, 0)?)?)?;
        let id = self.load_i64(self.offset(p, self.field_offset(t, None, 1)?)?)?;
        let val = self.offset(p, self.field_offset(t, None, 2)?)?;
        let live = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                id,
                self.llcx.i64_type().const_int(RELEASED, false),
                "",
            )
            .map_err(e)?;
        self.if_then(live, |cx| {
            let took = cx.rt_flag(
                "pool_return",
                &[h.into(), into.into(), id.into(), val.into()],
            )?;
            let refused = cx.builder.build_not(took, "").map_err(e)?;
            if cx.tys.needs_drop(el) {
                cx.if_then(refused, |cx| cx.call_glue(val, el))?;
            }
            Ok(())
        })
    }

    /// Branches to `pass` when the erased check at `kp` passes the value
    /// at `val`, else to `fail`.
    fn pool_check(
        &mut self,
        kp: PointerValue<'ctx>,
        check_ty: Ty,
        val: PointerValue<'ctx>,
        el: Ty,
        pass: inkwell::basic_block::BasicBlock<'ctx>,
        fail: inkwell::basic_block::BasicBlock<'ctx>,
    ) -> R<()> {
        let c = Callee::Erased {
            at: kp,
            ty: check_ty,
            consume: false,
        };
        let boolt = self.tys.intern(TyKind::Bool);
        let verdict = self.temp(boolt, "healthy")?;
        self.call_callee(&c, verdict, &[(val, el)])?;
        let healthy = self.load_bool(verdict)?;
        self.builder
            .build_conditional_branch(healthy, pass, fail)
            .map_err(e)?;
        Ok(())
    }

    /// Whether the health check `op`, a closure or function, takes the
    /// connection (its last parameter) by value. An already erased check
    /// says nothing of its body, and is lent the value as a bare closure
    /// parameter borrows it (D5).
    fn check_takes_value(&self, op: &Operand) -> R<bool> {
        let t = self.operand_type(op)?;
        if !matches!(self.tys.kind(t), TyKind::Closure(..) | TyKind::FnDef(_)) {
            return Ok(false);
        }
        let Ok(body) = self.body_of_def(t) else {
            return Ok(false);
        };
        Ok(body.args().last().is_some_and(|l| {
            !matches!(
                self.tys.kind(body.local(l).ty),
                TyKind::Ref(_) | TyKind::MutRef(_)
            )
        }))
    }

    /// `Fn(T) -> bool`, a health check's type.
    fn check_ty(&self, el: Ty) -> Ty {
        let boolt = self.tys.intern(TyKind::Bool);
        self.tys.intern(TyKind::Fn {
            params: vec![el],
            ret: boolt,
            kind: FnKind::Fn,
        })
    }

    /// Where the function operand `a` is as an erased value of type `to`:
    /// in place when it already is one, else erased into a temporary.
    fn erased_operand(&mut self, a: &Operand, to: Ty) -> R<PointerValue<'ctx>> {
        let from = self.operand_type(a)?;
        if matches!(self.tys.kind(from), TyKind::Fn { .. }) {
            return self.operand_ptr(a);
        }
        let tmp = self.temp(to, "erased")?;
        self.erase_into(tmp, a, from, to)?;
        Ok(tmp)
    }

    fn load_bool(&self, p: PointerValue<'ctx>) -> R<IntValue<'ctx>> {
        let i8t = self.llcx.i8_type();
        let v = self
            .builder
            .build_load(i8t, p, "")
            .map_err(e)?
            .into_int_value();
        self.builder
            .build_int_compare(IntPredicate::NE, v, i8t.const_zero(), "")
            .map_err(e)
    }

    fn mrt_i8(
        &self,
        name: &str,
        args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
    ) -> R<IntValue<'ctx>> {
        self.rt_int(name, args, 8)
    }
}
