//! A value whose type holds `Never` where its slot's type holds a type
//! (design.md § Subtyping and variance: `Never ≤ T`, and `Option`, `Result`
//! and tuples are covariant, so an `Option[Never]` is an `Option[String]`).
//! The two are different layouts, so the value is rebuilt at the slot's
//! type rather than assigned: each inhabited variant is made again from its
//! own fields, and a variant with a `Never` field cannot be the one the
//! value holds (B-2026-11-19-68).

use super::*;

impl<'l, 'a> Bx<'l, 'a> {
    /// Whether `from` is `to` with some parts `Never`, and not `to` itself.
    pub(super) fn never_widens(&self, from: Ty, to: Ty) -> bool {
        from != to && self.never_parts(from, to)
    }

    fn never_parts(&self, from: Ty, to: Ty) -> bool {
        if from == to {
            return true;
        }
        let tcx = self.tys().tcx();
        let all = |x: crate::ty::TyList, y: crate::ty::TyList| {
            let (x, y) = (tcx.list(x), tcx.list(y));
            x.len() == y.len() && x.iter().zip(&y).all(|(&a, &b)| self.never_parts(a, b))
        };
        match (tcx.kind(from), tcx.kind(to)) {
            (HK::Never, _) => true,
            (HK::Adt { def: d, args: x }, HK::Adt { def: e, args: y }) => d == e && all(x, y),
            (HK::Tuple(x), HK::Tuple(y)) => all(x, y),
            _ => false,
        }
    }

    /// Whether no value has type `t`.
    fn uninhabited(&self, t: Ty, depth: u32) -> bool {
        if depth > 8 {
            return false;
        }
        let tcx = self.tys().tcx();
        match tcx.kind(t) {
            HK::Never => true,
            HK::Tuple(l) => tcx.list(l).iter().any(|&x| self.uninhabited(x, depth + 1)),
            HK::Adt { .. } => {
                let Some((adt, _)) = tcx.adt_of(t) else {
                    return false;
                };
                let variant_empty = |v: u32| {
                    let n = adt.variants[v as usize].fields.len() as u32;
                    let at = adt.is_enum.then_some(v);
                    (0..n).any(|i| {
                        tcx.field_ty(t, at, i)
                            .is_some_and(|f| self.uninhabited(f, depth + 1))
                    })
                };
                if adt.is_enum {
                    (0..adt.variants.len() as u32).all(variant_empty)
                } else {
                    !adt.variants.is_empty() && variant_empty(0)
                }
            }
            _ => false,
        }
    }

    /// `op` as a value of `t`, rebuilt when its type is `t` with some parts
    /// `Never`; `None` when it needs no rebuilding.
    pub(super) fn never_widen(&mut self, op: &Operand, t: Ty) -> Option<Operand> {
        let ot = self.operand_ty(op);
        if !self.never_widens(ot, t) {
            return None;
        }
        let src = match op {
            Operand::Copy(p) | Operand::Move(p) => p.clone(),
            _ => {
                let l = self.temp(ot);
                self.assign(Place::local(l), Rvalue::Use(op.clone()));
                Place::local(l)
            }
        };
        let l = self.temp(t);
        self.rebuild_widened(src, ot, Place::local(l), t);
        Some(Operand::Move(Place::local(l)))
    }

    /// Writes the value at `src` (of type `st`) into `dest` (of type `dt`,
    /// `st` with `Never` parts widened), moving its fields across.
    pub(super) fn rebuild_widened(&mut self, src: Place, st: Ty, dest: Place, dt: Ty) {
        if st == dt {
            let op = self.use_place(src, st);
            self.assign(dest, Rvalue::Use(op));
            return;
        }
        let tcx = self.tys().tcx();
        match (tcx.kind(st), tcx.kind(dt)) {
            (HK::Never, _) => {
                let bb = self.cur;
                self.b.terminate(bb, TerminatorKind::Unreachable);
                self.cur = self.b.new_block();
            }
            (HK::Tuple(x), HK::Tuple(y)) => {
                let (x, y) = (tcx.list(x), tcx.list(y));
                let ops = x
                    .iter()
                    .zip(&y)
                    .enumerate()
                    .map(|(i, (&a, &b))| self.widened_part(src.field(i as u32, a), a, b))
                    .collect();
                self.assign(dest, Rvalue::Aggregate(AggregateKind::Tuple, ops));
            }
            (HK::Adt { .. }, HK::Adt { .. }) => {
                let Some((adt, _)) = tcx.adt_of(st) else {
                    let op = self.use_place(src, st);
                    self.assign(dest, Rvalue::Use(op));
                    return;
                };
                let fields: Vec<u32> = adt.variants.iter().map(|v| v.fields.len() as u32).collect();
                if !adt.is_enum {
                    let ops = self.widened_fields(&src, None, st, dt, fields[0]);
                    self.build_variant(dt, 0, ops, dest);
                    return;
                }
                let it = tcx.intern(HK::Int(IntSize::I64));
                let live: Vec<u32> = (0..fields.len() as u32)
                    .filter(|&v| {
                        (0..fields[v as usize]).all(|i| {
                            tcx.field_ty(st, Some(v), i)
                                .is_none_or(|f| !self.uninhabited(f, 0))
                        })
                    })
                    .collect();
                let tag = self.temp(it);
                self.assign(tag, Rvalue::Discriminant(src.clone()));
                let arms: Vec<(u32, BasicBlock)> =
                    live.iter().map(|&v| (v, self.b.new_block())).collect();
                let (dead, join) = (self.b.new_block(), self.b.new_block());
                let bb = self.cur;
                self.b.terminate(
                    bb,
                    TerminatorKind::SwitchInt {
                        discr: Operand::Copy(Place::local(tag)),
                        targets: SwitchTargets {
                            values: arms.iter().map(|&(v, b)| (u128::from(v), b)).collect(),
                            otherwise: dead,
                        },
                    },
                );
                self.b.terminate(dead, TerminatorKind::Unreachable);
                for (v, arm) in arms {
                    self.cur = arm;
                    let ops = self.widened_fields(&src, Some(v), st, dt, fields[v as usize]);
                    self.build_variant(dt, v, ops, dest.clone());
                    self.goto(join);
                }
                self.cur = join;
            }
            _ => {
                let op = self.use_place(src, st);
                self.assign(dest, Rvalue::Use(op));
            }
        }
    }

    fn widened_fields(
        &mut self,
        src: &Place,
        variant: Option<u32>,
        st: Ty,
        dt: Ty,
        n: u32,
    ) -> Vec<Operand> {
        let base = match variant {
            Some(v) => src.project(ProjElem::Downcast(VariantIdx(v))),
            None => src.clone(),
        };
        (0..n)
            .filter_map(|i| {
                let tcx = self.tys().tcx();
                let a = tcx.field_ty(st, variant, i)?;
                let b = tcx.field_ty(dt, variant, i)?;
                Some(self.widened_part(base.field(i, a), a, b))
            })
            .collect()
    }

    fn widened_part(&mut self, p: Place, a: Ty, b: Ty) -> Operand {
        if a == b {
            return self.use_place(p, a);
        }
        let l = self.temp(b);
        self.rebuild_widened(p, a, Place::local(l), b);
        Operand::Move(Place::local(l))
    }
}
