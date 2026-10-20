//! Count nothing for a handle read out of a borrowed parameter.
//!
//! `x = retain((*a) as Some).0)`, where `a` is a `ref` parameter, copies
//! a handle the caller holds for the whole call. When the body only reads
//! through `x` and drops it, and nothing it runs can overwrite a `mut`
//! field of a `shared` value (the one way the caller's handle could go
//! away while the call runs), the count `x` takes is given back unused.
//! This pass rewrites such a retain to a borrow of the place it reads
//! (`x` becomes a `ref` to the handle) and the drops of `x` to `goto`s, so a read-only tree walk (`is_mirror(an.left, bn.right)`,
//! kata 101) counts nothing per node, as legacy's RC elision of read-only
//! `ref` parameters does.
//!
//! A body is *count-safe* when it writes no place through a `shared`
//! handle, takes no `mut` borrow through one, drops nothing whose drop
//! can run user code, calls only count-safe bodies and natives whose
//! operands hold no user `Drop`, cell or function value, and has no
//! coroutine frame or `par` region. A retain is elided only in a
//! count-safe body with no `mut ref` parameter (its caller could alias
//! one with the borrowed parameter through a `shared` value).

use std::collections::{BTreeMap, BTreeSet};

use super::interp::Program;
use super::place_ty::{place_ty, project, PlaceTy};
use super::syntax::*;
use super::ty::{Ty, TyInterner, TyKind};
use crate::ty::TyKind as SK;

/// Elides every retain of a borrowed parameter's handle in `program` that
/// the body gives back unused. `skip` names the coroutines.
pub fn elide_borrowed_retains(program: &mut Program, tys: &TyInterner, skip: &BTreeSet<String>) {
    let safe = count_safe(program, tys, skip);
    for body in program.bodies.values_mut() {
        if !safe.get(&body.instance.name).copied().unwrap_or(false) {
            continue;
        }
        let has_mut_param = (1..=body.arg_count).any(|i| {
            matches!(
                tys.tcx().kind(body.locals[i].ty),
                SK::MutRef(_) | SK::Slice { mutable: true, .. }
            )
        });
        if has_mut_param {
            continue;
        }
        rewrite(body, tys);
    }
}

fn rewrite(body: &mut Body, tys: &TyInterner) {
    let mut found = Vec::new();
    for (bi, data) in body.blocks.iter().enumerate() {
        for (si, st) in data.statements.iter().enumerate() {
            if let StatementKind::Assign(dest, Rvalue::Retain(p)) = &st.kind {
                if dest.projection.is_empty() && candidate(body, tys, dest.local, p) {
                    found.push((bi, si, dest.local));
                }
            }
        }
    }
    for (bi, si, x) in found {
        // `x` becomes a borrow of the place the handle was copied from,
        // and every place through `x` reads through the borrow.
        let StatementKind::Assign(_, rv) = &mut body.blocks[bi].statements[si].kind else {
            unreachable!("a candidate is an assignment")
        };
        let Rvalue::Retain(p) = rv else {
            unreachable!("a candidate is a retain")
        };
        *rv = Rvalue::Ref(BorrowKind::Shared, p.clone());
        let old = body.locals[x.index()].ty;
        body.locals[x.index()].ty = tys.intern(TyKind::Ref(old));
        for (b, data) in body.blocks.iter_mut().enumerate() {
            for (s, st) in data.statements.iter_mut().enumerate() {
                if (b, s) != (bi, si) {
                    statement_places(&mut st.kind, &mut |q| deref_through(q, x));
                }
            }
            let k = &mut data.terminator.kind;
            if let TerminatorKind::Drop { place, target, .. } = k {
                if *place == Place::local(x) {
                    *k = TerminatorKind::Goto { target: *target };
                    continue;
                }
            }
            terminator_places(k, &mut |q| deref_through(q, x));
        }
    }
}

/// `x.f..` as `(*x).f..`.
fn deref_through(q: &mut Place, x: Local) {
    if q.local == x {
        q.projection.insert(0, ProjElem::Deref);
    }
}

fn operand_places(o: &mut Operand, f: &mut dyn FnMut(&mut Place)) {
    if let Operand::Copy(p) | Operand::Move(p) = o {
        f(p);
    }
}

fn statement_places(s: &mut StatementKind, f: &mut dyn FnMut(&mut Place)) {
    match s {
        StatementKind::Assign(d, rv) => {
            f(d);
            match rv {
                Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => {
                    operand_places(o, f)
                }
                Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
                    operand_places(a, f);
                    operand_places(b, f);
                }
                Rvalue::Aggregate(_, os) => os.iter_mut().for_each(|o| operand_places(o, f)),
                Rvalue::Ref(_, p)
                | Rvalue::Retain(p)
                | Rvalue::Discriminant(p)
                | Rvalue::Len(p) => f(p),
                Rvalue::NullaryOp(..) => {}
            }
        }
        StatementKind::SetDiscriminant(p, _) | StatementKind::FreeBox(p) => f(p),
        StatementKind::BorrowFlag(FlagOp::Acquire { place, .. } | FlagOp::Check { place, .. }) => {
            f(place)
        }
        StatementKind::BorrowFlag(FlagOp::Release { .. })
        | StatementKind::StorageLive(_)
        | StatementKind::StorageDead(_)
        | StatementKind::Nop => {}
    }
}

fn terminator_places(t: &mut TerminatorKind, f: &mut dyn FnMut(&mut Place)) {
    match t {
        TerminatorKind::SwitchInt { discr, .. } => operand_places(discr, f),
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => {
            operand_places(func, f);
            args.iter_mut().for_each(|o| operand_places(o, f));
            f(destination);
        }
        TerminatorKind::Drop { place, .. } => f(place),
        TerminatorKind::Goto { .. }
        | TerminatorKind::Return
        | TerminatorKind::Abort { .. }
        | TerminatorKind::Unreachable => {}
    }
}

/// Is `x = retain(p)` a handle the body only reads through, copied out of
/// a `ref` parameter nothing reassigns?
fn candidate(body: &Body, tys: &TyInterner, x: Local, p: &Place) -> bool {
    let a = p.local;
    if !matches!(tys.kind(body.local(x).ty), TyKind::Shared(_))
        || a.index() == 0
        || a.index() > body.arg_count
        || !matches!(tys.kind(body.local(a).ty), TyKind::Ref(_))
        || p.projection.first() != Some(&ProjElem::Deref)
        // The result, or a parameter, leaves with its count.
        || x.index() <= body.arg_count
    {
        return false;
    }
    let mut defs = 0;
    for data in &body.blocks {
        for st in &data.statements {
            match &st.kind {
                StatementKind::Assign(d, rv) => {
                    if d.local == a {
                        return false;
                    }
                    if d.local == x {
                        if !d.projection.is_empty() {
                            return false;
                        }
                        defs += 1;
                    }
                    if !rvalue_reads_through(rv, x) {
                        return false;
                    }
                }
                StatementKind::SetDiscriminant(d, _) if d.local == a || d.local == x => {
                    return false
                }
                _ => {}
            }
        }
        match &data.terminator.kind {
            TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } => {
                if destination.local == x || destination.local == a {
                    return false;
                }
                if !operand_reads_through(func, x)
                    || !args.iter().all(|o| operand_reads_through(o, x))
                {
                    return false;
                }
            }
            TerminatorKind::SwitchInt { discr, .. } if !operand_reads_through(discr, x) => {
                return false
            }
            _ => {}
        }
    }
    defs == 1
}

/// Does `o` use `x` only through a projection, or not at all?
fn operand_reads_through(o: &Operand, x: Local) -> bool {
    match o {
        Operand::Copy(p) | Operand::Move(p) => {
            !(p.local == x && p.projection.is_empty())
                && !p
                    .projection
                    .iter()
                    .any(|e| matches!(e, ProjElem::Index(i) if *i == x))
                && (p.local != x || matches!(o, Operand::Copy(_)))
        }
        Operand::Const(_) => true,
    }
}

fn rvalue_reads_through(rv: &Rvalue, x: Local) -> bool {
    match rv {
        Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => {
            operand_reads_through(o, x)
        }
        Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
            operand_reads_through(a, x) && operand_reads_through(b, x)
        }
        Rvalue::Aggregate(_, os) => os.iter().all(|o| operand_reads_through(o, x)),
        // A new count, or a read: `x` is alive for both.
        Rvalue::Ref(BorrowKind::Shared, _)
        | Rvalue::Retain(_)
        | Rvalue::Discriminant(_)
        | Rvalue::Len(_)
        | Rvalue::NullaryOp(..) => true,
        Rvalue::Ref(_, p) => p.local != x,
    }
}

/// Which bodies of `program` are count-safe: a fixpoint from all of them.
fn count_safe(
    program: &Program,
    tys: &TyInterner,
    skip: &BTreeSet<String>,
) -> BTreeMap<String, bool> {
    let mut callees: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut safe: BTreeMap<String, bool> = BTreeMap::new();
    let mut risk = Risk::default();
    for (name, body) in &program.bodies {
        let mut calls = Vec::new();
        let ok = !skip.contains(name)
            && body.par_regions.is_empty()
            && local_safe(body, tys, program, &mut risk, &mut calls);
        safe.insert(name.clone(), ok);
        callees.insert(name.clone(), calls);
    }
    loop {
        let mut changed = false;
        for (name, calls) in &callees {
            if safe[name] && calls.iter().any(|c| !safe.get(c).copied().unwrap_or(false)) {
                safe.insert(name.clone(), false);
                changed = true;
            }
        }
        if !changed {
            return safe;
        }
    }
}

/// The body's own part of count-safety; the bodies it calls go in `calls`.
fn local_safe(
    body: &Body,
    tys: &TyInterner,
    program: &Program,
    risk: &mut Risk,
    calls: &mut Vec<String>,
) -> bool {
    for data in &body.blocks {
        for st in &data.statements {
            match &st.kind {
                StatementKind::Assign(d, rv) => {
                    if through_shared(body, tys, d) {
                        return false;
                    }
                    if let Rvalue::Ref(BorrowKind::Mut, p) = rv {
                        if through_shared(body, tys, p) {
                            return false;
                        }
                    }
                }
                StatementKind::SetDiscriminant(d, _) if through_shared(body, tys, d) => {
                    return false
                }
                _ => {}
            }
        }
        match &data.terminator.kind {
            TerminatorKind::Drop { place, .. } => {
                if through_shared(body, tys, place) {
                    return false;
                }
                let Ok(pt) = place_ty(body, tys, place) else {
                    return false;
                };
                if risk.runs_user_code(tys, program, pt.ty) {
                    return false;
                }
            }
            TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } => {
                let Operand::Const(Const {
                    kind: ConstKind::FnDef(inst),
                    ..
                }) = func
                else {
                    return false;
                };
                if through_shared(body, tys, destination) {
                    return false;
                }
                if program.bodies.contains_key(&inst.name) {
                    calls.push(inst.name.clone());
                    continue;
                }
                // A native: what it can write is what its `mut` borrows
                // reach, checked where they are taken, unless an operand
                // holds a value whose drop or call runs user code.
                for o in args {
                    let ty = match o {
                        Operand::Copy(p) | Operand::Move(p) => match place_ty(body, tys, p) {
                            Ok(pt) => pt.ty,
                            Err(_) => return false,
                        },
                        Operand::Const(c) => c.ty,
                    };
                    if risk.runs_user_code(tys, program, ty) {
                        return false;
                    }
                }
            }
            _ => {}
        }
    }
    true
}

/// Does the path to `place` pass through a `shared` handle, or a raw
/// pointer? An unknown step answers yes.
fn through_shared(body: &Body, tys: &TyInterner, place: &Place) -> bool {
    let mut cur = PlaceTy {
        ty: body.locals[place.local.index()].ty,
        variant: None,
    };
    for elem in &place.projection {
        if matches!(tys.kind(cur.ty), TyKind::Shared(_) | TyKind::RawPtr { .. }) {
            return true;
        }
        match project(body, tys, cur, elem) {
            Ok(next) => cur = next,
            Err(_) => return true,
        }
    }
    false
}

/// Types whose drop, or whose use by a native, can run user code or
/// replace a value in place: a user `Drop` anywhere inside, a function
/// value, a library cell or handle.
#[derive(Default)]
struct Risk {
    memo: BTreeMap<Ty, bool>,
}

impl Risk {
    fn runs_user_code(&mut self, tys: &TyInterner, program: &Program, ty: Ty) -> bool {
        if let Some(&r) = self.memo.get(&ty) {
            return r;
        }
        // A type met again inside itself adds nothing: the answer is the
        // outer one.
        self.memo.insert(ty, false);
        let tcx = tys.tcx();
        let r = match tcx.kind(ty) {
            SK::Fn { .. } | SK::FnDef { .. } | SK::Closure { .. } | SK::Opaque { .. } => true,
            SK::Param(_) | SK::Proj { .. } | SK::RawPtr { .. } | SK::Error => true,
            SK::Ref(i) | SK::MutRef(i) | SK::Weak(i) => self.runs_user_code(tys, program, i),
            SK::Tuple(l) | SK::Intrinsic { args: l, .. } => tcx
                .list(l)
                .into_iter()
                .any(|t| self.runs_user_code(tys, program, t)),
            SK::Array { elem, .. } | SK::Slice { elem, .. } => {
                self.runs_user_code(tys, program, elem)
            }
            SK::Adt { def, .. } | SK::Shared { def, .. } => match tcx.adt_of(ty) {
                None => true,
                Some((adt, args)) => {
                    let lib = adt.library_name().unwrap_or("");
                    adt.has_drop_impl
                        || program.drop_impls.contains_key(&super::ty::AdtId(def.0))
                        || crate::ty::HANDLES.contains(&lib)
                        || crate::ty::LIBRARY_CELLS.contains(&lib)
                        || adt.variants.iter().any(|v| {
                            v.fields.iter().any(|(_, t)| {
                                let t = tcx.subst(*t, &args, &[]);
                                self.runs_user_code(tys, program, t)
                            })
                        })
                }
            },
            _ => false,
        };
        self.memo.insert(ty, r);
        r
    }
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::lower::tests::run_source;
    use crate::mir::{Rvalue, StatementKind};

    const SRC: &str = "shared struct N { val: i64, mut left: Option[N] }\n\
fn cut(h: N) {\n    h.left = None;\n}\n\
fn glance(a: Option[N], h: N) -> i64 {\n    match a {\n        Some(x) => {\n            cut(h);\n            x.val\n        }\n        None => 0\n    }\n}\n\
fn walk(a: Option[N]) -> i64 {\n    match a {\n        Some(x) => x.val + walk(x.left),\n        None => 0\n    }\n}\n\
fn bump(a: Option[N], h: mut ref i64) -> i64 {\n    match a {\n        Some(x) => {\n            *h = *h + 1;\n            x.val\n        }\n        None => 0\n    }\n}\n\
fn first(a: Option[N]) -> N {\n    match a {\n        Some(x) => x,\n        None => N { val: 0, left: None }\n    }\n}\n\
fn left_of(h: N) -> Option[N] {\n    return h.left;\n}\n\
fn main() {\n    let h = N { val: 1, left: Some(N { val: 7, left: Some(N { val: 9, left: None }) }) };\n    println(walk(h.left));\n    println(glance(h.left, h));\n    println(walk(h.left));\n    let mut c = 0;\n    println(bump(h.left, mut c));\n    println(first(h.left).val);\n    println(left_of(h).is_some());\n}\n";

    /// The retains left in `name`.
    fn retains(name: &str) -> usize {
        let l = build_source(SRC).expect("MIR");
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == name)
            .expect("the body");
        body.blocks
            .iter()
            .flat_map(|b| &b.statements)
            .filter(|s| matches!(&s.kind, StatementKind::Assign(_, Rvalue::Retain(_))))
            .count()
    }

    #[test]
    fn a_read_only_walk_counts_nothing() {
        assert_eq!(retains("walk"), 0);
        assert_eq!(run_source(SRC).expect("runs").0, "16\n7\n0\n0\n0\nfalse\n");
    }

    #[test]
    fn a_handle_kept_across_a_shared_write_is_counted() {
        // `cut` drops the caller's last count of the node `x` reads.
        assert_eq!(retains("glance"), 1);
    }

    #[test]
    fn a_handle_returned_keeps_its_count() {
        assert_eq!(retains("first"), 1);
    }

    #[test]
    fn a_body_with_a_mut_ref_parameter_is_left_alone() {
        assert_eq!(retains("bump"), 1);
    }
}
