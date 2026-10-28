//! `dyn Trait` in the MIR builder (design.md § `dyn Trait`;
//! `thread-b/dyn-mir-shapes.md`).
//!
//! A value goes into a `dyn` slot through `CastKind::ToDyn` with its method
//! table, built here once per (concrete type, `dyn` type): an owned value
//! into `dyn Trait`, a borrow into `ref dyn Trait`. A view of an owned
//! `dyn` is a plain borrow. A call of a method in the table is a `Call` of
//! `ConstKind::DynSlot`, its receiver the `dyn` value's view (or the value
//! itself, moved, for `own self`).

use super::*;

impl<'l, 'a> Bx<'l, 'a> {
    /// The `dyn` type under `t`'s references, when there is one.
    pub(super) fn dyn_under(&self, t: Ty) -> Option<Ty> {
        let tcx = self.tys().tcx();
        let mut t = t;
        loop {
            match tcx.kind(t) {
                HK::Ref(inner) | HK::MutRef(inner) => t = inner,
                HK::Dyn { .. } => return Some(t),
                _ => return None,
            }
        }
    }

    /// Whether `t` is a view of a `dyn` value, `ref dyn T` or `mut ref dyn T`.
    pub(super) fn is_dyn_view(&self, t: Ty) -> bool {
        self.dyn_under(t).is_some_and(|d| d != t)
    }

    /// The methods of `dyn_ty`'s table in slot order: each supertrait's,
    /// depth-first in declaration order, then the trait's own, each the
    /// methods that can be dispatched (design.md § Which traits can be
    /// used as `dyn`). By declaring trait and method.
    pub(super) fn dyn_slots(&self, dyn_ty: Ty) -> Vec<(String, &'a ast::TraitMethod)> {
        let HK::Dyn { trait_, .. } = self.tys().tcx().kind(dyn_ty) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        self.collect_slots(&self.lcx.def_name(trait_), &mut seen, &mut out);
        out
    }

    fn collect_slots(
        &self,
        tr: &str,
        seen: &mut FxHashSet<String>,
        out: &mut Vec<(String, &'a ast::TraitMethod)>,
    ) {
        if !seen.insert(tr.to_string()) {
            return;
        }
        let Some(&def) = self.lcx.trait_defs.get(tr) else {
            return;
        };
        for st in &def.supertraits {
            if let Some(name) = st.path.last() {
                self.collect_slots(name, seen, out);
            }
        }
        for item in &def.items {
            if let ast::TraitItem::Method(m) = item {
                if m.is_dyn_dispatchable() {
                    out.push((tr.to_string(), m));
                }
            }
        }
    }

    /// The method table of `concrete` as `dyn_ty`, built (and each slot's
    /// instance queued) the first time.
    pub(super) fn vtable_for(&mut self, concrete: Ty, dyn_ty: Ty) -> R<u32> {
        if let Some(i) = self
            .lcx
            .program
            .vtables
            .iter()
            .position(|v| v.concrete == concrete && v.dyn_ty == dyn_ty)
        {
            return Ok(i as u32);
        }
        let mut slots = Vec::new();
        for (tr, m) in self.dyn_slots(dyn_ty) {
            let Some((d, inst)) = self.user_impl_method(concrete, &tr, &m.name) else {
                let what = format!(
                    "`{}` as `{}` (no `{tr}.{}` of it to put in the method table)",
                    self.tys().display(concrete),
                    self.tys().display(dyn_ty),
                    m.name
                );
                return self.unsupported(m.span, &what);
            };
            if self.lcx.fns[&d].impl_params == 0 {
                self.lcx.self_tys.entry(d).or_insert(concrete);
            }
            slots.push(self.lcx.instance(d, inst));
        }
        self.lcx.program.vtables.push(Vtable {
            dyn_ty,
            concrete,
            slots,
            clone: None,
        });
        Ok((self.lcx.program.vtables.len() - 1) as u32)
    }

    /// The value of `op` in the slot type `t` when that is a coercion into
    /// a `dyn` type: an owned value into `dyn Trait`, a borrow into the
    /// view of the same kind. `None` when it is none.
    pub(super) fn dyn_cast(&self, op: &Operand, t: Ty) -> Option<(Ty, Ty)> {
        let tcx = self.tys().tcx();
        let ot = self.operand_ty(op);
        if ot == t {
            return None;
        }
        match (tcx.kind(t), tcx.kind(ot)) {
            (HK::Dyn { .. }, k) if !is_ref_or_dyn(&k) => Some((ot, t)),
            (HK::Ref(d), HK::Ref(c) | HK::MutRef(c)) | (HK::MutRef(d), HK::MutRef(c))
                if matches!(tcx.kind(d), HK::Dyn { .. }) && !is_ref_or_dyn(&tcx.kind(c)) =>
            {
                Some((c, d))
            }
            _ => None,
        }
    }

    /// `op` coerced into the `dyn` slot type `t` through a temporary, or
    /// `op` as it is when that is no coercion.
    pub(super) fn coerce_to_dyn(&mut self, op: Operand, t: Ty) -> Operand {
        let Some((concrete, dyn_ty)) = self.dyn_cast(&op, t) else {
            return op;
        };
        let Ok(vt) = self.vtable_for(concrete, dyn_ty) else {
            return op;
        };
        let l = self.temp(t);
        let bb = self.cur;
        self.b.assign(
            bb,
            Place::local(l),
            Rvalue::Cast(CastKind::ToDyn(vt), op, t),
        );
        Operand::Move(Place::local(l))
    }

    /// The place `a` names (or the one its reference names) lent into the
    /// `dyn` view type `pt`, or `None` when `a` is a `dyn` value already.
    pub(super) fn place_into_dyn_view(&mut self, a: &'a Expr, pt: Ty) -> R<Option<Operand>> {
        let at = self.expr_ty(a)?;
        if self.dyn_under(at).is_some() {
            return Ok(None);
        }
        let mutable = matches!(self.tys().tcx().kind(pt), HK::MutRef(_));
        let mut p = self.expr_place(a, mutable)?;
        if matches!(self.tys().tcx().kind(at), HK::Ref(_) | HK::MutRef(_)) {
            p = p.project(ProjElem::Deref);
        }
        let kind = if mutable {
            BorrowKind::Mut
        } else {
            BorrowKind::Shared
        };
        let r = self.temp(pt);
        self.assign(r, Rvalue::Ref(kind, p));
        Ok(Some(Operand::Move(Place::local(r))))
    }

    /// `object.method(args)` through a `dyn` receiver, as a call of the
    /// method's slot. `false` when the receiver is no `dyn` value or the
    /// method is in no slot (a default method that cannot be dispatched).
    pub(super) fn dyn_method_call(
        &mut self,
        object: &'a Expr,
        method: &str,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<bool> {
        // A receiver whose type is not to be had here is no `dyn` value:
        // the ordinary path lowers it (or says why not).
        let errors = self.errors.len();
        let Ok(rt) = self.expr_ty(object) else {
            self.errors.truncate(errors);
            return Ok(false);
        };
        let Some(dyn_ty) = self.dyn_under(rt) else {
            return Ok(false);
        };
        let slots = self.dyn_slots(dyn_ty);
        let Some(slot) = slots.iter().position(|(_, m)| m.name == method) else {
            return Ok(false);
        };
        let m = slots[slot].1;
        if args.len() != m.params.len() {
            return self.unsupported(
                object.span,
                "a call through `dyn` that leaves out arguments",
            );
        }
        let tcx = self.tys().tcx();
        let owned = matches!(tcx.kind(rt), HK::Dyn { .. });
        let mutable = m.self_param == Some(ast::SelfParam::MutRef);
        let recv = if m.self_is_own {
            if !owned {
                return self.unsupported(object.span, "an `own self` call through a `dyn` view");
            }
            PendingRecv::Ready(self.owned_operand(object, dyn_ty)?)
        } else if owned {
            let p = self.expr_place(object, mutable)?;
            PendingRecv::Borrow(p, dyn_ty, mutable)
        } else if self.is_place(object) {
            // The view itself: a `dyn` view is never reborrowed through.
            PendingRecv::Ready(Operand::Copy(self.expr_place(object, false)?))
        } else {
            PendingRecv::Ready(self.expr_operand(object)?)
        };
        let mut rest = Vec::with_capacity(args.len());
        for (a, p) in args.iter().zip(&m.params) {
            let at = self.expr_ty(&a.value)?;
            let pt = self.dyn_param_ty(p, at);
            rest.push(self.arg_pending(&a.value, pt)?);
        }
        let mut rest = self.finish_args(rest)?;
        let mut ops = vec![self.recv_borrow_after(recv, &mut rest)?];
        ops.extend(rest);
        let unit = self.unit();
        let func = Operand::Const(Const {
            ty: unit,
            kind: ConstKind::DynSlot {
                dyn_ty,
                slot: slot as u32,
            },
        });
        let next = self.b.new_block();
        self.goto_with(
            TerminatorKind::Call {
                func,
                args: ops,
                destination: dest,
                target: Some(next),
                unwind: UnwindAction::Abort,
            },
            next,
        );
        Ok(true)
    }

    /// A slot method's parameter as its instances take it, from the
    /// argument's type: every impl's parameter has the trait's type, so its
    /// mode follows the declaration (`ref`, `mut ref`, or D5's borrow of a
    /// bare non-`Copy` parameter).
    fn dyn_param_ty(&self, p: &ast::Param, at: Ty) -> Ty {
        let tcx = self.tys().tcx();
        let base = match tcx.kind(at) {
            HK::Ref(inner) | HK::MutRef(inner) => inner,
            _ => at,
        };
        match &p.ty.kind {
            ast::TypeKind::Ref(_) => tcx.reference(base, false),
            ast::TypeKind::MutRef(_) => tcx.reference(base, true),
            _ => self.param_mode_ty(p, at),
        }
    }
}

fn is_ref_or_dyn(k: &HK) -> bool {
    matches!(k, HK::Ref(_) | HK::MutRef(_) | HK::Dyn { .. })
}
