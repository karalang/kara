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

    /// Whether `tr` has `Clone` among its supertraits, at any depth.
    fn clones_under(&self, tr: &str) -> bool {
        let mut seen = FxHashSet::default();
        let mut todo = vec![tr.to_string()];
        while let Some(t) = todo.pop() {
            if t == "Clone" {
                return true;
            }
            if !seen.insert(t.clone()) {
                continue;
            }
            let def = self
                .lcx
                .trait_defs
                .get(t.as_str())
                .copied()
                .or_else(|| checker_library_trait(&t));
            if let Some(def) = def {
                todo.extend(
                    def.supertraits
                        .iter()
                        .filter_map(|st| st.path.last().cloned()),
                );
            }
        }
        false
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
        let Some(def) = self
            .lcx
            .trait_defs
            .get(tr)
            .copied()
            .or_else(|| checker_library_trait(tr))
        else {
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
        // The dyn type's own trait's slots come from the impl its arguments
        // name; a supertrait's from its first impl.
        // Its arguments lead the `dyn` type's, before the associated types.
        let (own_trait, mut own_args) = match self.tys().tcx().kind(dyn_ty) {
            HK::Dyn { trait_, args, .. } => {
                (self.lcx.def_name(trait_), self.tys().tcx().list(args))
            }
            _ => (String::new(), Vec::new()),
        };
        let own_params = self
            .lcx
            .trait_defs
            .get(own_trait.as_str())
            .copied()
            .or_else(|| checker_library_trait(&own_trait))
            .and_then(|d| d.generic_params.as_ref())
            .map_or(0, |g| g.params.len());
        own_args.truncate(own_params);
        let mut slots = Vec::new();
        for (tr, m) in self.dyn_slots(dyn_ty) {
            // A type with no `impl Display` of its own (a number, a
            // `String`) fills `to_string`'s slot with the library's
            // conversion (B-2026-10-29-31).
            let shim = (tr == "Display" && m.name == "to_string")
                .then(|| self.lcx.defs.lookup(0, "dyn_display_to_string"))
                .flatten()
                .filter(|d| self.lcx.fns.contains_key(d));
            let trait_args: &[Ty] = if tr == own_trait { &own_args } else { &[] };
            let found = self
                .trait_impl_method(concrete, &tr, &m.name, trait_args)
                .or_else(|| self.lcx.blanket_impl_method(concrete, &tr, &m.name))
                .or_else(|| shim.map(|d| (d, vec![concrete])));
            let Some((d, inst)) = found else {
                // Named as the program writes them, not by def id.
                let named = |t: Ty| self.tys().tcx().display(t, &|d| self.lcx.def_name(d));
                let what = format!(
                    "`{}` as `{}` (no `{tr}.{}` of it to put in the method table)",
                    named(concrete),
                    named(dyn_ty),
                    m.name
                );
                return self.unsupported(m.span, &what);
            };
            if self.lcx.fns[&d].impl_params == 0 && Some(d) != shim {
                self.lcx.self_tys.entry(d).or_insert(concrete);
            }
            let target = self.lcx.instance(d, inst);
            // An impl method weaker than the trait's (§4.7) is called
            // through a shim in the trait's convention (B-2026-11-19-126).
            let weaker = Some(d) != shim && modes_differ(m, self.lcx.fns[&d].f);
            if weaker {
                let name = format!("{target}#dyn");
                if self.lcx.queued.insert(name.clone()) {
                    self.lcx
                        .dyn_shim_queue
                        .push((name.clone(), target, m, concrete));
                }
                slots.push(name);
            } else {
                slots.push(target);
            }
        }
        // A trait under `Clone` makes the `dyn` value `Clone`: the table
        // carries the concrete value's clone, which a clone of the `dyn`
        // value boxes (design.md § Which traits can be used as `dyn`).
        let clone = match self.tys().tcx().kind(dyn_ty) {
            HK::Dyn { trait_, .. } if self.clones_under(&self.lcx.def_name(trait_)) => {
                let d = self
                    .lcx
                    .defs
                    .lookup(0, "dyn_clone_value")
                    .filter(|d| self.lcx.fns.contains_key(d));
                d.map(|d| self.lcx.instance(d, vec![concrete]))
            }
            _ => None,
        };
        let starts = vec![None; slots.len()];
        let name = self.tys().tcx().display(dyn_ty, &|d| self.lcx.def_name(d));
        self.lcx.program.vtables.push(Vtable {
            dyn_ty,
            name,
            concrete,
            slots,
            clone,
            starts,
        });
        Ok((self.lcx.program.vtables.len() - 1) as u32)
    }

    /// The body of the `dyn` slot shim for an impl method `f` weaker than
    /// the trait's `m` (`Lcx::lower_dyn_mode_shim`): each argument taken
    /// as the trait declares it and passed to `callee`, whose parameters
    /// are `to`, as the impl takes it.
    pub(super) fn dyn_mode_shim_body(
        &mut self,
        f: Option<&'a Function>,
        m: &'a ast::TraitMethod,
        concrete: Ty,
        to: &[Ty],
        callee: InstanceId,
    ) -> R<()> {
        let span = f.map_or(m.span, |f| f.span);
        let Some(f) = f.filter(|f| f.params.len() + 1 == to.len()) else {
            return self.unsupported(span, "a `dyn` slot shim whose method has no receiver");
        };
        let tcx = self.tys().tcx();
        let recv = if m.self_is_own {
            concrete
        } else {
            match m.self_param {
                Some(ast::SelfParam::MutRef) => tcx.reference(concrete, true),
                Some(ast::SelfParam::Ref) => tcx.reference(concrete, false),
                _ if self.is_copy(concrete) => concrete,
                _ => tcx.reference(concrete, false),
            }
        };
        let mut from = vec![recv];
        for ((tp, ip), &t) in m.params.iter().zip(&f.params).zip(&to[1..]) {
            // The type under the impl's borrow, written or (D5) bare.
            let base = match tcx.kind(t) {
                HK::Ref(inner) | HK::MutRef(inner)
                    if matches!(ip.ty.kind, ast::TypeKind::Ref(_) | ast::TypeKind::MutRef(_))
                        || (!ip.is_own && self.param_mode_ty(ip, inner) == t) =>
                {
                    inner
                }
                _ => t,
            };
            from.push(match &tp.ty.kind {
                ast::TypeKind::Ref(_) => tcx.reference(base, false),
                ast::TypeKind::MutRef(_) => tcx.reference(base, true),
                _ => self.param_mode_ty(tp, base),
            });
        }
        self.push_scope();
        let mut ops = Vec::with_capacity(to.len());
        // Every argument is declared before the first temporary.
        let mut locals = Vec::with_capacity(from.len());
        for (i, &ft) in from.iter().enumerate() {
            let l = self.b.arg(if i == 0 { "self" } else { "arg" }, ft);
            if self.needs_drop(ft) {
                self.schedule(ScopeEntry::Drop(Place::local(l)));
            }
            locals.push(l);
        }
        for ((&l, &ft), &tt) in locals.iter().zip(&from).zip(to) {
            let p = Place::local(l);
            let op = if ft == tt {
                Operand::Move(p)
            } else {
                let kinds = {
                    let tcx = self.tys().tcx();
                    (tcx.kind(ft), tcx.kind(tt))
                };
                match kinds {
                    // Owned, lent: the shim drops it after the call.
                    (_, HK::Ref(inner)) if inner == ft => {
                        let r = self.temp(tt);
                        self.assign(r, Rvalue::Ref(BorrowKind::Shared, p));
                        Operand::Move(Place::local(r))
                    }
                    (HK::MutRef(a), HK::Ref(b)) if a == b => {
                        let r = self.temp(tt);
                        self.assign(
                            r,
                            Rvalue::Ref(BorrowKind::Shared, p.project(ProjElem::Deref)),
                        );
                        Operand::Move(Place::local(r))
                    }
                    // A borrow of a `Copy` value the impl takes by value.
                    (HK::Ref(a) | HK::MutRef(a), _) if a == tt && self.is_copy(tt) => {
                        Operand::Copy(p.project(ProjElem::Deref))
                    }
                    _ => {
                        let what = format!(
                            "a `dyn` call passing `{}` to an impl taking `{}`",
                            self.tys().display(ft),
                            self.tys().display(tt)
                        );
                        return self.unsupported(span, &what);
                    }
                }
            };
            ops.push(op);
        }
        let func = self.fn_operand(&callee.name.clone(), callee.def, callee.args);
        let next = self.b.new_block();
        self.goto_with(
            TerminatorKind::Call {
                func,
                args: ops,
                destination: Place::local(Local::RETURN_PLACE),
                target: Some(next),
                unwind: UnwindAction::Abort,
            },
            next,
        );
        self.return_exit()?;
        self.scopes.clear();
        Ok(())
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
            let pt = self.dyn_param_ty(p, at, dyn_ty, &slots[slot].0);
            rest.push(self.arg_pending(&a.value, pt)?);
        }
        let mut rest = self.finish_args(rest)?;
        let mut ops = vec![self.recv_borrow_after(recv, &mut rest)?];
        ops.extend(rest);
        // A receiver whose type writes a `with` clause: the call has the
        // clause's effects, narrowed to the method's own (design.md § `dyn
        // Trait`), kept like a function-typed field's clause.
        let recv_place = ops[0].place().cloned();
        if let (Some(declared), Some(p)) = (self.dyn_clause_effects(object, m), recv_place) {
            // A view read out of a place, such as a `par` body's capture
            // (`(*env).0`), is copied into a temporary the clause is kept
            // on: the clause is kept per local.
            let l = if p.projection.is_empty() {
                p.local
            } else {
                let ty = self.operand_ty(&ops[0]);
                let r = self.temp(ty);
                self.assign(r, Rvalue::Use(ops[0].clone()));
                ops[0] = match ops[0] {
                    Operand::Move(_) => Operand::Move(Place::local(r)),
                    _ => Operand::Copy(Place::local(r)),
                };
                r
            };
            self.b.fn_value_effects.push((l, declared));
        }
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

    /// The effects a call of `m` through `object` declares when the
    /// receiver's type writes a `with` clause of plain verbs: the clause's,
    /// less any outside the method's own `with`. `None` with no clause, or
    /// one naming an effect variable, which keeps every effect.
    fn dyn_clause_effects(
        &self,
        object: &Expr,
        m: &ast::TraitMethod,
    ) -> Option<Vec<crate::mir::syntax::DeclaredEffect>> {
        let (ty, _) = self.lcx.tc.node_types.get(&object.id)?;
        let mut ty = ty;
        while let crate::typechecker::Type::Ref(i) | crate::typechecker::Type::MutRef(i) = ty {
            ty = i;
        }
        let crate::typechecker::Type::Dyn {
            clause: crate::typechecker::types::DynClause(clause),
            ..
        } = ty
        else {
            return None;
        };
        let ceiling = m.effects.as_ref().map(super::declared_effect_list);
        // No clause on the type: the method's own clause, a ceiling every
        // concrete type's method stays within, when it writes one.
        let Some(list) = clause else {
            return ceiling.flatten();
        };
        let mut declared = super::declared_effect_list(list)?;
        if let Some(Some(ceiling)) = ceiling {
            declared.retain(|d| ceiling.contains(d));
        }
        Some(declared)
    }

    /// The default `method` of the trait `tr` (the `dyn` type's own or a
    /// supertrait) run on the `dyn` type `t`, with its `Self` and trait
    /// arguments. `None` when the trait declares no such default body.
    pub(super) fn dyn_impl_method(
        &self,
        t: Ty,
        tr: &str,
        method: &str,
    ) -> Option<(DefId, Vec<Ty>)> {
        let tcx = self.tys().tcx();
        let HK::Dyn { trait_, args, .. } = tcx.kind(t) else {
            return None;
        };
        let d = self.lcx.defs.lookup(0, tr)?;
        let mut path = self.lcx.defs.table.get(d).path.segments.clone();
        path.push(method.to_string());
        let m = self.lcx.defs.table.lookup(&DefPath::new(path))?;
        let item = self.lcx.fns.get(&m).filter(|i| i.impl_dyn.is_some())?;
        // The trait's own arguments: the `dyn` type's leading ones for its
        // own trait; a supertrait's are not written on the `dyn` type, so
        // only one with none is reached.
        let own = item.impl_params - 1;
        let targs = if self.lcx.def_name(trait_) == tr {
            tcx.list(args).get(..own)?.to_vec()
        } else if own == 0 {
            Vec::new()
        } else {
            return None;
        };
        let mut inst = vec![t];
        inst.extend(targs);
        Some((m, inst))
    }

    /// `object.method(args)` of a `dyn` receiver whose method is in no slot
    /// (a default method such as `map`, which takes `Self` by value or is
    /// generic): the trait's default body, instanced at the `dyn` type.
    /// `callee` is the trait's declaration of the method, `own` the
    /// method's own type arguments.
    pub(super) fn dyn_default_call(
        &mut self,
        e: &'a Expr,
        object: &'a Expr,
        callee: DefId,
        own: Vec<Ty>,
        args: &'a [CallArg],
        dest: Place,
    ) -> R<bool> {
        if !self
            .lcx
            .fns
            .get(&callee)
            .is_some_and(|i| i.impl_dyn.is_some())
        {
            return Ok(false);
        }
        let rt = self.expr_ty(object)?;
        let Some(dyn_ty) = self.dyn_under(rt) else {
            return Ok(false);
        };
        let segs = &self.lcx.defs.table.get(callee).path.segments;
        let (Some(method), Some(tr)) = (segs.last().cloned(), segs.iter().rev().nth(1).cloned())
        else {
            return Ok(false);
        };
        let Some((d, mut inst)) = self.dyn_impl_method(dyn_ty, &tr, &method) else {
            return Ok(false);
        };
        inst.extend(own);
        self.user_method_call(e, object, d, inst, args, dest)?;
        Ok(true)
    }

    /// In a generic body instanced at a `dyn` type (`[I: Iterator]` at a
    /// `dyn Iterator`), where the call records no callee: the trait's
    /// default `method` for the receiver `rt`, its own type arguments read
    /// off the arguments and the result `ret`.
    pub(super) fn dyn_default_by_receiver(
        &mut self,
        rt: Ty,
        method: &str,
        args: &'a [CallArg],
        ret: Option<Ty>,
    ) -> R<Option<(DefId, Vec<Ty>)>> {
        let Some(dyn_ty) = self.dyn_under(rt) else {
            return Ok(None);
        };
        let HK::Dyn { trait_, .. } = self.tys().tcx().kind(dyn_ty) else {
            return Ok(None);
        };
        let mut frontier = std::collections::VecDeque::from([self.lcx.def_name(trait_)]);
        let mut seen = FxHashSet::default();
        let mut owner = None;
        while let Some(tr) = frontier.pop_front() {
            if !seen.insert(tr.clone()) {
                continue;
            }
            let Some(&def) = self.lcx.trait_defs.get(&tr) else {
                continue;
            };
            let declares = def.items.iter().any(|it| {
                matches!(it, ast::TraitItem::Method(m) if m.name == method && m.body.is_some())
            });
            if declares {
                owner = Some(tr);
                break;
            }
            frontier.extend(
                def.supertraits
                    .iter()
                    .filter_map(|b| b.path.last().cloned()),
            );
        }
        let Some((d, inst)) = owner.and_then(|tr| self.dyn_impl_method(dyn_ty, &tr, method)) else {
            return Ok(None);
        };
        let generic = self.lcx.fns[&d]
            .f
            .generic_params
            .as_ref()
            .is_some_and(|g| !g.params.is_empty());
        if generic {
            return self.own_generics_from_args(d, inst, args, ret);
        }
        Ok(Some((d, inst)))
    }

    /// `next` for a loop over an owned `dyn` value of an `Iterator`: its
    /// slot, which [`Self::dyn_next_func`] turns the returned `NextFn` into.
    pub(super) fn dyn_iterator_next(&mut self, iterable: &'a Expr) -> R<Option<NextFn>> {
        let before = self.errors.len();
        let Ok(t) = self.expr_ty(iterable) else {
            self.errors.truncate(before);
            return Ok(None);
        };
        let tcx = self.tys().tcx();
        let HK::Dyn { trait_, args, .. } = tcx.kind(t) else {
            return Ok(None);
        };
        let has_next = self
            .dyn_slots(t)
            .iter()
            .any(|(tr, m)| tr == "Iterator" && m.name == "next");
        let tr = self.lcx.def_name(trait_);
        let item = self
            .lcx
            .dyn_assoc_index(&tr, "Item")
            .and_then(|i| tcx.list(args).get(i).copied());
        let (true, Some(item)) = (has_next, item) else {
            return Ok(None);
        };
        let Some(opt_t) = self.option_of(item) else {
            return Ok(None);
        };
        let Some(next) = self.lcx.defs.lookup(0, "Iterator").and_then(|d| {
            let mut path = self.lcx.defs.table.get(d).path.segments.clone();
            path.push("next".to_string());
            self.lcx.defs.table.lookup(&DefPath::new(path))
        }) else {
            return Ok(None);
        };
        Ok(Some((next, vec![t], String::new(), opt_t)))
    }

    /// The callee of a loop's `next`: the `dyn` value's slot for the
    /// `NextFn` [`Self::dyn_iterator_next`] made (no instance name, the
    /// `dyn` type its one argument), else the instance.
    pub(super) fn dyn_next_func(&self, (d, args, name, _): &NextFn) -> Operand {
        if let ([t], true) = (args.as_slice(), name.is_empty()) {
            let slot = self
                .dyn_slots(*t)
                .iter()
                .position(|(tr, m)| tr == "Iterator" && m.name == "next");
            if let Some(slot) = slot {
                return Operand::Const(Const {
                    ty: self.unit(),
                    kind: ConstKind::DynSlot {
                        dyn_ty: *t,
                        slot: slot as u32,
                    },
                });
            }
        }
        self.fn_operand(name, *d, args.clone())
    }

    /// A slot method's parameter as its instances take it, from the
    /// argument's type: every impl's parameter has the trait's type, so its
    /// mode follows the declaration (`ref`, `mut ref`, or D5's borrow of a
    /// bare non-`Copy` parameter).
    fn dyn_param_ty(&self, p: &ast::Param, at: Ty, dyn_ty: Ty, tr: &str) -> Ty {
        let tcx = self.tys().tcx();
        let base = match tcx.kind(at) {
            HK::Ref(inner) | HK::MutRef(inner) => inner,
            _ => at,
        };
        // A `Str` or scalar parameter is passed as the slot's impl takes
        // it, whatever the argument was typed before it met the parameter:
        // a string literal is a `String` constant and `0.5` an `f64` until
        // then (B-2026-11-19-128).
        match &p.ty.kind {
            ast::TypeKind::Ref(w) => {
                tcx.reference(self.written_ty(w, base, dyn_ty, tr).unwrap_or(base), false)
            }
            ast::TypeKind::MutRef(w) => {
                tcx.reference(self.written_ty(w, base, dyn_ty, tr).unwrap_or(base), true)
            }
            _ => self.param_mode_ty(p, self.written_ty(&p.ty, at, dyn_ty, tr).unwrap_or(at)),
        }
    }

    /// `written_scalar`, and a tuple of them part by part: `("abc", 0.5)`
    /// for a `(Str, f32)` parameter is a `(String, f64)` literal until it
    /// meets the slot (B-2026-11-20-3). `at` is the argument's type, which
    /// a part the parameter does not write keeps.
    fn written_ty(&self, te: &ast::TypeExpr, at: Ty, dyn_ty: Ty, tr: &str) -> Option<Ty> {
        if let Some(t) = self.written_scalar(te, dyn_ty, tr) {
            return Some(t);
        }
        let ast::TypeKind::Tuple(parts) = &te.kind else {
            return None;
        };
        let tcx = self.tys().tcx();
        let HK::Tuple(found) = tcx.kind(at) else {
            return None;
        };
        let found = tcx.list(found).to_vec();
        if found.len() != parts.len() {
            return None;
        }
        let written: Vec<Ty> = parts
            .iter()
            .zip(&found)
            .map(|(w, &f)| self.written_ty(w, f, dyn_ty, tr).unwrap_or(f))
            .collect();
        (written != found).then(|| tcx.intern(HK::Tuple(tcx.intern_list(&written))))
    }

    /// The `Str` or scalar type `te` writes, by name or as a type
    /// parameter of `dyn_ty`'s own trait `tr`.
    fn written_scalar(&self, te: &ast::TypeExpr, dyn_ty: Ty, tr: &str) -> Option<Ty> {
        let tcx = self.tys().tcx();
        let ast::TypeKind::Path(q) = &te.kind else {
            return None;
        };
        let [n] = q.segments.as_slice() else {
            return None;
        };
        if q.generic_args.is_some() || n == "String" {
            return None;
        }
        if n == "Str" {
            return Some(tcx.intern(HK::StaticStr));
        }
        self.lcx
            .primitive(n)
            .or_else(|| self.dyn_trait_arg(dyn_ty, tr, n))
            // `Str` as a type argument (`dyn Cv[Str]`) is named by the
            // library's `StringSlice`, which is the view (B-2026-11-20-13).
            .map(|t| match tcx.kind(t) {
                HK::Adt { def, args }
                    if tcx.list(args).is_empty() && self.lcx.def_name(def) == "StringSlice" =>
                {
                    tcx.intern(HK::StaticStr)
                }
                _ => t,
            })
            .filter(|&t| {
                matches!(
                    tcx.kind(t),
                    HK::Int(_) | HK::UInt(_) | HK::Float(_) | HK::Bool | HK::Char | HK::StaticStr
                )
            })
    }

    /// The argument `dyn_ty` gives its own trait `tr`'s type parameter
    /// `name`: `f32` for `X` in `dyn G[f32]` of `trait G[X]`.
    fn dyn_trait_arg(&self, dyn_ty: Ty, tr: &str, name: &str) -> Option<Ty> {
        let tcx = self.tys().tcx();
        let HK::Dyn { trait_, args, .. } = tcx.kind(dyn_ty) else {
            return None;
        };
        if self.lcx.def_name(trait_) != tr {
            return None;
        }
        let def = self
            .lcx
            .trait_defs
            .get(tr)
            .copied()
            .or_else(|| checker_library_trait(tr))?;
        let i = def
            .generic_params
            .as_ref()?
            .params
            .iter()
            .position(|g| g.name == name)?;
        tcx.list(args).get(i).copied()
    }
}

fn is_ref_or_dyn(k: &HK) -> bool {
    matches!(k, HK::Ref(_) | HK::MutRef(_) | HK::Dyn { .. })
}

/// A trait the checker's library declares and the MIR library does not
/// (`Display`), for the slots of a `dyn` type of it: its declaration,
/// parsed once.
fn checker_library_trait(tr: &str) -> Option<&'static ast::TraitDef> {
    static TRAITS: std::sync::LazyLock<ast::Program> = std::sync::LazyLock::new(|| {
        crate::parse(include_str!("../../../runtime/stdlib/display.kara")).program
    });
    TRAITS.items.iter().find_map(|i| match i {
        ast::Item::TraitDef(t) if t.name == tr => Some(t),
        _ => None,
    })
}

/// Whether the impl method `f` takes its receiver or a parameter in
/// another mode than the trait's `m` declares (§4.7 allows a weaker one).
fn modes_differ(m: &ast::TraitMethod, f: &ast::Function) -> bool {
    let kind = |p: &ast::Param| match p.ty.kind {
        ast::TypeKind::Ref(_) => 1,
        ast::TypeKind::MutRef(_) => 2,
        _ => 0,
    };
    m.self_is_own != f.self_is_own
        || m.self_param != f.self_param
        || m.params
            .iter()
            .zip(&f.params)
            .any(|(t, i)| t.is_own != i.is_own || kind(t) != kind(i))
}
