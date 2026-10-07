//! The borrow check (`docs/spikes/mir-types.md` §3, the `Checked` phase):
//! core semantics §5.5 and §5.6 inside one body.
//!
//! - Every `Rvalue::Ref` creates a **loan** of its place.
//! - A forward dataflow gives, at each point, the loans each local's value
//!   may hold (its **origins**, §5.3). A copy or move of a place carries the
//!   origins of its local; a call result carries the origins of its
//!   receiver when the callee takes `ref self` / `mut ref self`, else of
//!   every argument (§5.4); a reborrow `&(*r)` keeps `r`'s loans as well
//!   as its own. MIR does not record receivers, so the caller says which
//!   callees have one.
//!   Only a value whose type can hold a reference has origins. A call
//!   with a whole-local `mut ref` argument may store the other arguments'
//!   borrows into its pointee, so the pointee's origins grow by theirs
//!   (§5.3; `TaskGroup.spawn` is the §9.5 case); so does `(*r) = v`.
//! - §5.9: a write, mutable borrow or non-`Copy` move through a `ref` is
//!   an error unless a `shared` handle is projected after that deref.
//! - A backward liveness gives the locals that may still be used. A loan
//!   is live where some live local may hold it (non-lexical, §5.6).
//! - An access to a place that overlaps a live loan's place conflicts when
//!   the loan is `mut`, or when the loan is shared and the access writes,
//!   moves, mutably borrows, drops or ends the place's storage.
//! - At `return`, the result may hold no loan of a place this function
//!   owns; a loan reached through a reference parameter is fine (§5.4).
//!
//! Accesses through a reference are rooted at the reference's local, so
//! they never overlap the loan that created it: `*r = v` through
//! `r = &mut a` is not an access to `a`. Two-phase borrows are not
//! modelled; the builder takes a `mut ref self` receiver's borrow after
//! the arguments (§5.6).

use super::place_ty::place_ty;
use super::pretty::place as show_place;
use super::syntax::*;
use super::ty::{IntrinsicTy, Ty, TyInterner, TyKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Access {
    Read,
    Write,
    Move,
    Borrow(BorrowKind),
    Drop,
    StorageDead,
}

struct Loan {
    kind: BorrowKind,
    place: Place,
    /// Where it was created: block and statement.
    at: (usize, usize),
}

/// Per local, the loans its value may hold.
type Origins = Vec<Vec<bool>>;

/// Checks `body`; the errors name each conflicting access by block and
/// statement. `has_receiver` says whether a callee takes `ref self` or
/// `mut ref self` as its first argument. Read-only: `check_moves` is what
/// marks a body `Checked`.
pub fn check_borrows(
    body: &Body,
    tys: &TyInterner,
    has_receiver: &dyn Fn(&InstanceId) -> bool,
) -> Result<(), Vec<String>> {
    let mut errs = read_only_errors(body, tys);
    let loans = collect_loans(body);
    if loans.is_empty() {
        return if errs.is_empty() { Ok(()) } else { Err(errs) };
    }
    let n_locals = body.locals.len();
    let entry: Origins = vec![vec![false; loans.len()]; n_locals];
    let cx = Cx {
        body,
        tys,
        loans: &loans,
        has_receiver,
    };
    let origins = origins_dataflow(&cx, entry);
    let live_out = liveness(body);

    for (bi, block) in body.blocks.iter().enumerate() {
        let Some(mut o) = origins[bi].clone() else {
            continue; // unreachable
        };
        // Liveness before each statement, walking back from the block end.
        let n = block.statements.len();
        let mut live_before = vec![Vec::new(); n + 1];
        let mut live = live_out[bi].clone();
        let (u, d) = terminator_use_def(&block.terminator.kind);
        transfer_live(&mut live, &u, &d);
        live_before[n] = live.clone();
        for si in (0..n).rev() {
            let (u, d) = statement_use_def(&block.statements[si].kind);
            transfer_live(&mut live, &u, &d);
            live_before[si] = live.clone();
        }

        for (si, live) in live_before.iter().enumerate() {
            let at = (bi, si);
            let accesses = if si < n {
                statement_accesses(&block.statements[si].kind)
            } else {
                terminator_accesses(&block.terminator.kind)
            };
            for (place, access) in accesses {
                for (li, loan) in loans.iter().enumerate() {
                    if loan.at == at || !loan_live(&o, li, live) {
                        continue;
                    }
                    if conflicts(loan, &place, access) {
                        errs.push(format!(
                            "{}: {} of {} while it is borrowed{} (borrow at {})",
                            loc(at, n),
                            access_name(access),
                            show_place(body, tys, &place),
                            if loan.kind == BorrowKind::Mut {
                                " mutably"
                            } else {
                                ""
                            },
                            loc(loan.at, body.blocks[loan.at.0].statements.len()),
                        ));
                    }
                }
            }
            if si < n {
                cx.transfer(&mut o, &block.statements[si].kind, at);
            } else if let TerminatorKind::Return = block.terminator.kind {
                for (li, loan) in loans.iter().enumerate() {
                    if o[0][li] && !loan.place.projection.contains(&ProjElem::Deref) {
                        errs.push(format!(
                            "{}: the result borrows {}, which this function owns (borrow at {})",
                            loc(at, n),
                            show_place(body, tys, &loan.place),
                            loc(loan.at, body.blocks[loan.at.0].statements.len()),
                        ));
                    }
                }
            }
        }
    }
    errs.dedup();
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// §5.9: nothing is written through a `ref`. A write, a mutable borrow
/// or a move of a non-`Copy` value whose place goes through a `ref` is an
/// error, unless a projection on a `shared` handle comes after that
/// dereference: a `shared` value's fields are reached through the handle,
/// and which of them are `mut` is the type checker's question.
fn read_only_errors(body: &Body, tys: &TyInterner) -> Vec<String> {
    let mut errs = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        let n = block.statements.len();
        for si in 0..=n {
            let accesses = if si < n {
                statement_accesses(&block.statements[si].kind)
            } else {
                terminator_accesses(&block.terminator.kind)
            };
            for (place, access) in accesses {
                let what = match access {
                    Access::Write => "write",
                    Access::Borrow(BorrowKind::Mut) => "mutable borrow",
                    Access::Move
                        if place_ty(body, tys, &place).is_ok_and(|pt| !tys.is_copy(pt.ty)) =>
                    {
                        "move"
                    }
                    _ => continue,
                };
                if through_shared_ref(body, tys, &place) {
                    errs.push(format!(
                        "{}: {} of {} through a shared reference",
                        loc((bi, si), n),
                        what,
                        show_place(body, tys, &place),
                    ));
                }
            }
        }
    }
    errs
}

/// Does `place` reach its target through a `ref` dereference with no
/// `shared` handle projected after it?
fn through_shared_ref(body: &Body, tys: &TyInterner, place: &Place) -> bool {
    let mut read_only = false;
    for i in 0..place.projection.len() {
        let prefix = Place {
            local: place.local,
            projection: place.projection[..i].to_vec(),
        };
        let Ok(pt) = place_ty(body, tys, &prefix) else {
            return false;
        };
        match tys.kind(pt.ty) {
            TyKind::Ref(_) if place.projection[i] == ProjElem::Deref => read_only = true,
            TyKind::Shared(_) => read_only = false,
            _ => {}
        }
    }
    read_only
}

fn loc((bi, si): (usize, usize), n_stmts: usize) -> String {
    if si == n_stmts {
        format!("bb{bi}[term]")
    } else {
        format!("bb{bi}[{si}]")
    }
}

fn access_name(a: Access) -> &'static str {
    match a {
        Access::Read => "read",
        Access::Write => "write",
        Access::Move => "move",
        Access::Borrow(BorrowKind::Shared) => "shared borrow",
        Access::Borrow(BorrowKind::Mut) => "mutable borrow",
        Access::Drop => "drop",
        Access::StorageDead => "end of storage",
    }
}

fn collect_loans(body: &Body) -> Vec<Loan> {
    let mut loans = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        for (si, s) in block.statements.iter().enumerate() {
            if let StatementKind::Assign(_, Rvalue::Ref(kind, place)) = &s.kind {
                loans.push(Loan {
                    kind: *kind,
                    place: place.clone(),
                    at: (bi, si),
                });
            }
        }
    }
    loans
}

/// What the origins transfer needs to know about the body.
struct Cx<'a> {
    body: &'a Body,
    tys: &'a TyInterner,
    loans: &'a [Loan],
    has_receiver: &'a dyn Fn(&InstanceId) -> bool,
}

impl Cx<'_> {
    /// Can a value of `place`'s type hold a borrow? Only those carry
    /// origins: `x = *r` with `x: i64` holds nothing of `r`.
    fn holds(&self, place: &Place) -> bool {
        place_ty(self.body, self.tys, place).map_or(true, |pt| holds_borrow(self.tys, pt.ty))
    }

    /// The origins of a value read from `place`: what its local holds,
    /// when its type can hold any.
    fn place_origins(&self, o: &Origins, place: &Place, into: &mut [bool]) {
        if self.holds(place) {
            for (d, s) in into.iter_mut().zip(&o[place.local.index()]) {
                *d |= *s;
            }
        }
    }

    fn operand_origins(&self, o: &Origins, op: &Operand, into: &mut [bool]) {
        if let Some(p) = op.place() {
            self.place_origins(o, p, into);
        }
    }

    /// `dest = <value with origins val>`.
    fn assign(&self, o: &mut Origins, dest: &Place, mut val: Vec<bool>) {
        if !self.holds(dest) {
            val.iter_mut().for_each(|b| *b = false);
        }
        if dest.projection.is_empty() {
            o[dest.local.index()] = val;
        } else if !dest.projection.contains(&ProjElem::Deref) {
            // A part of the local: what the rest held stays.
            for (d, s) in o[dest.local.index()].iter_mut().zip(val) {
                *d |= s;
            }
        } else {
            // A store through a reference: the places the reference
            // mutably borrows now hold `val` too.
            self.store_through(o, dest.local, &val);
        }
    }

    /// A value with origins `val` is stored through the reference held in
    /// local `r`: every local place `r` mutably borrows may now hold it.
    fn store_through(&self, o: &mut Origins, r: Local, val: &[bool]) {
        if !val.iter().any(|b| *b) {
            return;
        }
        let targets: Vec<Local> = self
            .loans
            .iter()
            .enumerate()
            .filter(|(li, l)| {
                o[r.index()][*li]
                    && l.kind == BorrowKind::Mut
                    && !l.place.projection.contains(&ProjElem::Deref)
                    && self.holds(&l.place)
            })
            .map(|(_, l)| l.place.local)
            .collect();
        // A value never holds a borrow of its own local.
        for t in targets {
            for (li, s) in val.iter().enumerate() {
                let own = self.loans[li].place.local == t
                    && !self.loans[li].place.projection.contains(&ProjElem::Deref);
                if *s && !own {
                    o[t.index()][li] = true;
                }
            }
        }
    }

    fn transfer(&self, o: &mut Origins, s: &StatementKind, at: (usize, usize)) {
        let StatementKind::Assign(dest, rv) = s else {
            if let StatementKind::StorageDead(l) = s {
                o[l.index()].iter_mut().for_each(|b| *b = false);
            }
            return;
        };
        let mut val = vec![false; self.loans.len()];
        match rv {
            Rvalue::Ref(_, place) => {
                if let Some(li) = self.loans.iter().position(|l| l.at == at) {
                    val[li] = true;
                }
                // A reborrow through a reference keeps that reference's
                // loans, whatever the type of the place reborrowed.
                if place.projection.contains(&ProjElem::Deref) {
                    for (d, s) in val.iter_mut().zip(&o[place.local.index()]) {
                        *d |= *s;
                    }
                }
            }
            Rvalue::Use(op) | Rvalue::UnaryOp(_, op) | Rvalue::Cast(_, op, _) => {
                self.operand_origins(o, op, &mut val)
            }
            Rvalue::Aggregate(_, ops) => {
                for op in ops {
                    self.operand_origins(o, op, &mut val);
                }
            }
            Rvalue::Retain(p) => self.place_origins(o, p, &mut val),
            Rvalue::BinaryOp(..)
            | Rvalue::CheckedBinaryOp(..)
            | Rvalue::Discriminant(_)
            | Rvalue::Len(_) => {}
        }
        self.assign(o, dest, val);
    }

    /// A call: the result borrows from the receiver when the callee takes
    /// one, else from every argument (§5.4); and a callee given a `mut ref`
    /// may store any other argument's borrows into its pointee, as
    /// `group.spawn(closure)` does (§9.5).
    fn transfer_call(&self, o: &mut Origins, func: &Operand, args: &[Operand], dest: &Place) {
        let receiver = matches!(
            func,
            Operand::Const(Const { kind: ConstKind::FnDef(callee), .. })
                if !args.is_empty() && (self.has_receiver)(callee)
        );
        // Each whole-local `mut ref` argument may store the other operands'
        // borrows into its pointee (§9.5).
        let before = o.clone();
        for (i, a) in args.iter().enumerate() {
            let Some(p) = a.place() else { continue };
            let is_mut_ref = place_ty(self.body, self.tys, p)
                .is_ok_and(|pt| matches!(self.tys.kind(pt.ty), TyKind::MutRef(_)));
            if !is_mut_ref || !p.projection.is_empty() {
                continue;
            }
            let mut others = vec![false; self.loans.len()];
            let rest = args.iter().enumerate().filter(|(j, _)| *j != i);
            for a in std::iter::once(func).chain(rest.map(|(_, a)| a)) {
                self.operand_origins(&before, a, &mut others);
            }
            self.store_through(o, p.local, &others);
        }
        let mut val = vec![false; self.loans.len()];
        let from = if receiver { &args[..1] } else { args };
        for a in std::iter::once(func).chain(from) {
            self.operand_origins(o, a, &mut val);
        }
        self.assign(o, dest, val);
    }
}

/// Can a value of type `ty` hold a borrow? References and slices can, and
/// so can anything with a part that can; a `shared` handle cannot (§5.7).
/// A type MIR cannot see into is assumed to, and so is `TaskGroup`.
fn holds_borrow(tys: &TyInterner, ty: Ty) -> bool {
    fn go(tys: &TyInterner, ty: Ty, seen: &mut Vec<Ty>) -> bool {
        if seen.contains(&ty) {
            return false;
        }
        seen.push(ty);
        match tys.kind(ty) {
            TyKind::Ref(_) | TyKind::MutRef(_) | TyKind::Slice(_) | TyKind::Other => true,
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => ts.iter().any(|t| go(tys, *t, seen)),
            TyKind::Array(t, _) => go(tys, t, seen),
            TyKind::Intrinsic(
                IntrinsicTy::Vec(e)
                | IntrinsicTy::Set(e)
                | IntrinsicTy::VecDeque(e)
                | IntrinsicTy::SortedSet(e),
            ) => go(tys, e, seen),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                go(tys, k, seen) || go(tys, v, seen)
            }
            TyKind::Adt(a) => {
                let adt = tys.adt(a);
                // A task group holds its tasks' closures, which the
                // runtime keeps out of its declared fields (§9.5).
                if adt.name == "TaskGroup" {
                    return true;
                }
                let counts: Vec<usize> = adt.variants.iter().map(|v| v.fields.len()).collect();
                let is_enum = adt.is_enum;
                counts.iter().enumerate().any(|(v, n)| {
                    (0..*n as u32).any(|f| {
                        tys.field_ty(ty, is_enum.then_some(v as u32), f)
                            .is_some_and(|t| go(tys, t, seen))
                    })
                })
            }
            _ => false,
        }
    }
    go(tys, ty, &mut Vec::new())
}

/// Forward dataflow of origins; `None` for an unreachable block.
fn origins_dataflow(cx: &Cx, entry: Origins) -> Vec<Option<Origins>> {
    let body = cx.body;
    let mut states: Vec<Option<Origins>> = vec![None; body.blocks.len()];
    states[0] = Some(entry);
    let mut work = vec![0usize];
    while let Some(bi) = work.pop() {
        let mut o = states[bi].clone().expect("queued blocks have a state");
        let block = &body.blocks[bi];
        for (si, s) in block.statements.iter().enumerate() {
            cx.transfer(&mut o, &s.kind, (bi, si));
        }
        if let TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } = &block.terminator.kind
        {
            cx.transfer_call(&mut o, func, args, destination);
        }
        for s in block.terminator.kind.successors() {
            let si = s.index();
            let changed = match &mut states[si] {
                None => {
                    states[si] = Some(o.clone());
                    true
                }
                Some(cur) => {
                    let mut changed = false;
                    for (cl, nl) in cur.iter_mut().zip(&o) {
                        for (c, n) in cl.iter_mut().zip(nl) {
                            if *n && !*c {
                                *c = true;
                                changed = true;
                            }
                        }
                    }
                    changed
                }
            };
            if changed && !work.contains(&si) {
                work.push(si);
            }
        }
    }
    states
}

fn loan_live(o: &Origins, li: usize, live: &[Local]) -> bool {
    live.iter().any(|l| o[l.index()][li])
}

// ---- liveness ----

fn place_uses(p: &Place, out: &mut Vec<Local>) {
    out.push(p.local);
    for e in &p.projection {
        if let ProjElem::Index(i) = e {
            out.push(*i);
        }
    }
}

/// Locals a statement uses and the local it defines outright.
fn statement_use_def(s: &StatementKind) -> (Vec<Local>, Vec<Local>) {
    let mut uses = Vec::new();
    let mut defs = Vec::new();
    if let StatementKind::Assign(dest, rv) = s {
        rvalue_places(rv, &mut |p| place_uses(p, &mut uses));
        if dest.projection.is_empty() {
            defs.push(dest.local);
        } else {
            place_uses(dest, &mut uses);
        }
    } else if let StatementKind::SetDiscriminant(p, _) = s {
        place_uses(p, &mut uses);
    }
    (uses, defs)
}

fn terminator_use_def(t: &TerminatorKind) -> (Vec<Local>, Vec<Local>) {
    let mut uses = Vec::new();
    let mut defs = Vec::new();
    match t {
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => {
            for o in std::iter::once(func).chain(args) {
                if let Some(p) = o.place() {
                    place_uses(p, &mut uses);
                }
            }
            if destination.projection.is_empty() {
                defs.push(destination.local);
            } else {
                place_uses(destination, &mut uses);
            }
        }
        TerminatorKind::SwitchInt { discr, .. } => {
            if let Some(p) = discr.place() {
                place_uses(p, &mut uses);
            }
        }
        TerminatorKind::Drop { place, .. } => place_uses(place, &mut uses),
        TerminatorKind::Return => uses.push(Local::RETURN_PLACE),
        _ => {}
    }
    (uses, defs)
}

/// `live` goes from after a statement to before it.
fn transfer_live(live: &mut Vec<Local>, uses: &[Local], defs: &[Local]) {
    live.retain(|l| !defs.contains(l));
    for u in uses {
        if !live.contains(u) {
            live.push(*u);
        }
    }
}

/// The locals live at the end of each block.
fn liveness(body: &Body) -> Vec<Vec<Local>> {
    let n = body.blocks.len();
    let mut live_in: Vec<Vec<Local>> = vec![Vec::new(); n];
    let mut live_out: Vec<Vec<Local>> = vec![Vec::new(); n];
    let mut changed = true;
    while changed {
        changed = false;
        for bi in (0..n).rev() {
            let block = &body.blocks[bi];
            let mut out = Vec::new();
            for s in block.terminator.kind.successors() {
                for l in &live_in[s.index()] {
                    if !out.contains(l) {
                        out.push(*l);
                    }
                }
            }
            let mut live = out.clone();
            let (u, d) = terminator_use_def(&block.terminator.kind);
            transfer_live(&mut live, &u, &d);
            for s in block.statements.iter().rev() {
                let (u, d) = statement_use_def(&s.kind);
                transfer_live(&mut live, &u, &d);
            }
            if live.len() != live_in[bi].len() || out.len() != live_out[bi].len() {
                changed = true;
            }
            live_in[bi] = live;
            live_out[bi] = out;
        }
    }
    live_out
}

// ---- accesses ----

fn rvalue_places(rv: &Rvalue, f: &mut impl FnMut(&Place)) {
    let mut op = |o: &Operand| {
        if let Some(p) = o.place() {
            f(p)
        }
    };
    match rv {
        Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => op(o),
        Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
            op(a);
            op(b)
        }
        Rvalue::Aggregate(_, ops) => ops.iter().for_each(op),
        Rvalue::Ref(_, p) | Rvalue::Retain(p) | Rvalue::Discriminant(p) | Rvalue::Len(p) => f(p),
    }
}

fn operand_access(o: &Operand) -> Option<(Place, Access)> {
    match o {
        Operand::Copy(p) => Some((p.clone(), Access::Read)),
        Operand::Move(p) => Some((p.clone(), Access::Move)),
        Operand::Const(_) => None,
    }
}

fn statement_accesses(s: &StatementKind) -> Vec<(Place, Access)> {
    match s {
        StatementKind::Assign(dest, rv) => {
            let mut v: Vec<(Place, Access)> = match rv {
                Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => {
                    operand_access(o).into_iter().collect()
                }
                Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
                    [a, b].into_iter().filter_map(operand_access).collect()
                }
                Rvalue::Aggregate(_, ops) => ops.iter().filter_map(operand_access).collect(),
                Rvalue::Ref(k, p) => vec![(p.clone(), Access::Borrow(*k))],
                Rvalue::Retain(p) | Rvalue::Discriminant(p) | Rvalue::Len(p) => {
                    vec![(p.clone(), Access::Read)]
                }
            };
            v.push((dest.clone(), Access::Write));
            v
        }
        StatementKind::SetDiscriminant(p, _) => vec![(p.clone(), Access::Write)],
        StatementKind::StorageDead(l) => vec![(Place::local(*l), Access::StorageDead)],
        StatementKind::StorageLive(_) | StatementKind::Nop => Vec::new(),
    }
}

fn terminator_accesses(t: &TerminatorKind) -> Vec<(Place, Access)> {
    match t {
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => {
            let mut v: Vec<_> = std::iter::once(func)
                .chain(args)
                .filter_map(operand_access)
                .collect();
            v.push((destination.clone(), Access::Write));
            v
        }
        TerminatorKind::SwitchInt { discr, .. } => operand_access(discr).into_iter().collect(),
        TerminatorKind::Drop { place, .. } => vec![(place.clone(), Access::Drop)],
        _ => Vec::new(),
    }
}

/// Do two places overlap (§5.6)? A place overlaps its prefixes and its
/// extensions; distinct fields and distinct variants do not; an index
/// overlaps every element.
fn overlaps(a: &Place, b: &Place) -> bool {
    if a.local != b.local {
        return false;
    }
    for (x, y) in a.projection.iter().zip(&b.projection) {
        match (x, y) {
            (ProjElem::Field(f, _), ProjElem::Field(g, _)) if f != g => return false,
            (ProjElem::Downcast(v), ProjElem::Downcast(w)) if v != w => return false,
            (ProjElem::ConstIndex(i), ProjElem::ConstIndex(j)) if i != j => return false,
            _ => {}
        }
    }
    true
}

fn conflicts(loan: &Loan, place: &Place, access: Access) -> bool {
    if !overlaps(&loan.place, place) {
        return false;
    }
    match loan.kind {
        BorrowKind::Mut => true,
        BorrowKind::Shared => !matches!(access, Access::Read | Access::Borrow(BorrowKind::Shared)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::parse_module;

    /// The text form records no receivers: a body named `Type.method`
    /// whose first parameter is a reference is taken to have one.
    fn receivers(m: &crate::mir::MirModule) -> impl Fn(&InstanceId) -> bool + '_ {
        |callee| {
            m.bodies.iter().any(|b| {
                b.instance.name == callee.name
                    && b.instance.name.contains('.')
                    && b.arg_count > 0
                    && matches!(
                        m.tys.kind(b.locals[1].ty),
                        crate::mir::TyKind::Ref(_) | crate::mir::TyKind::MutRef(_)
                    )
            })
        }
    }

    /// The borrow errors of `main` in `src` (every other body must pass).
    fn errors(src: &str) -> Vec<String> {
        let m = parse_module(src).unwrap_or_else(|e| panic!("{e}"));
        let mut out = Vec::new();
        for b in &m.bodies {
            match check_borrows(b, &m.tys, &receivers(&m)) {
                Ok(()) => {}
                Err(es) if b.instance.name == "main" => out = es,
                Err(es) => panic!("{}: {es:?}", b.instance.name),
            }
        }
        out
    }

    /// `fn main` with the given locals and `bb0` statements, then return.
    fn main_with(locals: &str, stmts: &str) -> Vec<String> {
        errors(&format!(
            "
struct P {{ a: i64, b: i64 }}

fn id(_1: ref i64) -> ref i64 {{
    let mut _0: ref i64;
    bb0: {{
        _0 = copy _1;
        return;
    }}
}}

fn main() -> () {{
    let mut _0: ();
{locals}    bb0: {{
{stmts}        _0 = const ();
        return;
    }}
}}
"
        ))
    }

    fn one(errs: &[String], starts: &str) {
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].starts_with(starts), "{errs:?}");
    }

    #[test]
    fn mir_borrowck_write_while_shared_borrow_is_live() {
        let errs = main_with(
            "    let mut _1: i64;\n    let _2: ref i64;\n    let _3: i64;\n",
            "        _1 = const 1_i64;
        _2 = &_1;
        _1 = const 2_i64;
        _3 = copy (*_2);
",
        );
        one(
            &errs,
            "bb0[2]: write of _1 while it is borrowed (borrow at bb0[1])",
        );
    }

    /// Non-lexical: once the reference is no longer used, the place is free.
    #[test]
    fn mir_borrowck_borrow_ends_at_its_last_use() {
        let errs = main_with(
            "    let mut _1: i64;\n    let _2: ref i64;\n    let _3: i64;\n",
            "        _1 = const 1_i64;
        _2 = &_1;
        _3 = copy (*_2);
        _1 = const 2_i64;
",
        );
        assert_eq!(errs, Vec::<String>::new());
    }

    #[test]
    fn mir_borrowck_read_while_mut_borrow_is_live() {
        let errs = main_with(
            "    let mut _1: i64;\n    let _2: mut ref i64;\n    let _3: i64;\n",
            "        _1 = const 1_i64;
        _2 = &mut _1;
        _3 = copy _1;
        (*_2) = const 5_i64;
",
        );
        one(&errs, "bb0[2]: read of _1 while it is borrowed mutably");
        // Writing through the borrow, then reading the place, is fine.
        let ok = main_with(
            "    let mut _1: i64;\n    let _2: mut ref i64;\n    let _3: i64;\n",
            "        _1 = const 1_i64;
        _2 = &mut _1;
        (*_2) = const 5_i64;
        _3 = copy _1;
",
        );
        assert_eq!(ok, Vec::<String>::new());
    }

    /// Distinct fields do not overlap; the borrowed field does.
    #[test]
    fn mir_borrowck_disjoint_fields() {
        let locals = "    let mut _1: P;\n    let _2: ref i64;\n    let _3: i64;\n";
        let ok = main_with(
            locals,
            "        _1 = P { const 1_i64, const 2_i64 };
        _2 = &_1.0;
        _1.1 = const 3_i64;
        _3 = copy (*_2);
",
        );
        assert_eq!(ok, Vec::<String>::new());
        let errs = main_with(
            locals,
            "        _1 = P { const 1_i64, const 2_i64 };
        _2 = &_1.0;
        _1.0 = const 3_i64;
        _3 = copy (*_2);
",
        );
        one(&errs, "bb0[2]: write of _1.0");
        let whole = main_with(
            locals,
            "        _1 = P { const 1_i64, const 2_i64 };
        _2 = &_1.0;
        _1 = P { const 4_i64, const 5_i64 };
        _3 = copy (*_2);
",
        );
        one(&whole, "bb0[2]: write of _1 ");
    }

    /// A call's result borrows from its reference arguments (§5.4), so the
    /// loan stays live through it.
    #[test]
    fn mir_borrowck_call_result_carries_the_argument_loans() {
        let errs = errors(
            "
fn id(_1: ref i64) -> ref i64 {
    let mut _0: ref i64;
    bb0: {
        _0 = copy _1;
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let mut _1: i64;
    let _2: ref i64;
    let _3: ref i64;
    let _4: i64;
    bb0: {
        _1 = const 1_i64;
        _2 = &_1;
        _3 = id(move _2) -> bb1;
    }
    bb1: {
        _1 = const 2_i64;
        _4 = copy (*_3);
        _0 = const ();
        return;
    }
}
",
        );
        one(
            &errs,
            "bb1[0]: write of _1 while it is borrowed (borrow at bb0[1])",
        );
    }

    /// A reborrow keeps the original borrow live while it is in use, and
    /// using the original in between conflicts with it.
    #[test]
    fn mir_borrowck_reborrow() {
        let locals =
            "    let mut _1: i64;\n    let _2: mut ref i64;\n    let _3: mut ref i64;\n    let _4: i64;\n";
        let ok = main_with(
            locals,
            "        _1 = const 1_i64;
        _2 = &mut _1;
        _3 = &mut (*_2);
        (*_3) = const 2_i64;
        (*_2) = const 3_i64;
        _4 = copy _1;
",
        );
        assert_eq!(ok, Vec::<String>::new());
        let errs = main_with(
            locals,
            "        _1 = const 1_i64;
        _2 = &mut _1;
        _3 = &mut (*_2);
        (*_2) = const 3_i64;
        (*_3) = const 2_i64;
",
        );
        one(&errs, "bb0[3]: write of (*_2) while it is borrowed mutably");
        let through = main_with(
            locals,
            "        _1 = const 1_i64;
        _2 = &mut _1;
        _3 = &mut (*_2);
        _4 = copy _1;
        (*_3) = const 2_i64;
",
        );
        one(&through, "bb0[3]: read of _1 while it is borrowed mutably");
    }

    #[test]
    fn mir_borrowck_move_and_storage_end_while_borrowed() {
        let errs = errors(
            "
fn main() -> () {
    let mut _0: ();
    let _1: String;
    let _2: ref String;
    let _3: String;
    let _4: ();
    bb0: {
        _1 = String.from(const \"a\") -> bb1;
    }
    bb1: {
        _2 = &_1;
        _3 = move _1;
        _4 = println(copy _2) -> bb2;
    }
    bb2: {
        _0 = const ();
        return;
    }
}
",
        );
        one(&errs, "bb1[1]: move of _1 while it is borrowed");
        // §5.5: a borrow of a temporary must not outlive its storage.
        let tmp = main_with(
            "    let _1: i64;\n    let _2: ref i64;\n    let _3: i64;\n",
            "        StorageLive(_1);
        _1 = const 1_i64;
        _2 = &_1;
        StorageDead(_1);
        _3 = copy (*_2);
",
        );
        one(&tmp, "bb0[3]: end of storage of _1");
    }

    #[test]
    fn mir_borrowck_returning_a_borrow_of_a_local_is_an_error() {
        let errs = errors(
            "
fn main() -> ref i64 {
    let mut _0: ref i64;
    let _1: i64;
    bb0: {
        _1 = const 1_i64;
        _0 = &_1;
        return;
    }
}
",
        );
        one(
            &errs,
            "bb0[term]: the result borrows _1, which this function owns",
        );
        // Through a reference parameter it is the caller's place: fine.
        let ok = errors(
            "
struct P { a: i64, b: i64 }

fn main(_1: ref P) -> ref i64 {
    let mut _0: ref i64;
    bb0: {
        _0 = &(*_1).0;
        return;
    }
}
",
        );
        assert_eq!(ok, Vec::<String>::new());
    }

    /// §5.4: a method taking `ref self` returns a borrow of `self` only,
    /// so dropping the temporary argument is fine (`ok_ref_self_wins`).
    /// Without the receiver the result borrows every argument.
    #[test]
    fn mir_borrowck_ref_self_result_borrows_only_the_receiver() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src = std::fs::read_to_string(root.join("tests/mir/core-built/ok_ref_self_wins.mir"))
            .unwrap();
        let m = parse_module(&src).unwrap();
        let main = m.bodies.iter().find(|b| b.instance.name == "main").unwrap();
        assert_eq!(check_borrows(main, &m.tys, &receivers(&m)), Ok(()));
        let errs = check_borrows(main, &m.tys, &|_| false).unwrap_err();
        one(&errs, "bb4[term]: drop of _6 while it is borrowed");
    }

    /// The Built core and closure pins are valid programs.
    /// Only a value whose type can hold a reference carries a borrow: an
    /// `i64` read through a reference does not keep the place borrowed.
    #[test]
    fn mir_borrowck_origins_follow_the_type() {
        let errs = main_with(
            "    let mut _1: i64;\n    let _2: ref i64;\n    let _3: i64;\n    let _4: i64;\n",
            "        _1 = const 1_i64;
        _2 = &_1;
        _3 = copy (*_2);
        _1 = const 2_i64;
        _4 = copy _3;
",
        );
        assert_eq!(errs, Vec::<String>::new());
    }

    /// §9.5: a call taking a `mut ref` stores its other arguments' borrows
    /// into the pointee, so the group holds them while it is in use.
    #[test]
    fn mir_borrowck_mut_ref_argument_absorbs_the_other_loans() {
        let src = |tail: &str| {
            format!(
                "
struct G {{ r: ref i64 }}

fn G.put(_1: mut ref G, _2: ref i64) -> () {{
    let mut _0: ();
    bb0: {{
        _0 = const ();
        return;
    }}
}}

fn main() -> () {{
    let mut _0: ();
    let mut _1: i64;
    let mut _2: G;
    let _3: ref i64;
    let _4: mut ref G;
    let _5: ();
    let _6: i64;
    let _7: ref i64;
    let _8: ref i64;
    bb0: {{
        _1 = const 1_i64;
        _6 = const 0_i64;
        _7 = &_6;
        _2 = G {{ move _7 }};
        _3 = &_1;
        _4 = &mut _2;
        _5 = G.put(move _4, move _3) -> bb1;
    }}
    bb1: {{
        _1 = const 2_i64;
{tail}        _0 = const ();
        return;
    }}
}}
"
            )
        };
        one(
            &errors(&src("        _8 = copy _2.0;\n")),
            "bb1[0]: write of _1 while it is borrowed (borrow at bb0[4])",
        );
        assert_eq!(errors(&src("")), Vec::<String>::new());
    }

    /// `TaskGroup` declares only an id but holds its tasks' closures, so
    /// a spawn into it keeps the closure's borrows live (§9.5).
    #[test]
    fn mir_borrowck_task_group_holds_spawned_borrows() {
        let src = |tail: &str| {
            format!(
                "
struct TaskGroup {{ id: i64 }}

fn TaskGroup.spawn(_1: mut ref TaskGroup, _2: closure#1(ref i64)) -> () {{
    let mut _0: ();
    bb0: {{
        _0 = const ();
        return;
    }}
}}

fn main() -> () {{
    let mut _0: ();
    let mut _1: i64;
    let mut _2: TaskGroup;
    let _3: ref i64;
    let _4: closure#1(ref i64);
    let _5: mut ref TaskGroup;
    let _6: ();
    let _7: i64;
    bb0: {{
        _1 = const 1_i64;
        _2 = TaskGroup {{ const 0_i64 }};
        _3 = &_1;
        _4 = closure#1(ref i64) [move _3];
        _5 = &mut _2;
        _6 = TaskGroup.spawn(move _5, move _4) -> bb1;
    }}
    bb1: {{
        _1 = const 2_i64;
{tail}        _0 = const ();
        return;
    }}
}}
"
            )
        };
        one(
            &errors(&src("        _7 = copy _2.0;\n")),
            "bb1[0]: write of _1 while it is borrowed (borrow at bb0[2])",
        );
        assert_eq!(errors(&src("")), Vec::<String>::new());
    }

    /// §5.9: nothing is written or mutably borrowed through a `ref`.
    #[test]
    fn mir_borrowck_write_through_a_shared_ref_is_an_error() {
        let locals = "    let mut _1: i64;\n    let _2: ref i64;\n    let _3: mut ref i64;\n";
        one(
            &main_with(
                locals,
                "        _1 = const 1_i64;
        _2 = &_1;
        (*_2) = const 5_i64;
",
            ),
            "bb0[2]: write of (*_2) through a shared reference",
        );
        one(
            &main_with(
                locals,
                "        _1 = const 1_i64;
        _2 = &_1;
        _3 = &mut (*_2);
",
            ),
            "bb0[2]: mutable borrow of (*_2) through a shared reference",
        );
    }

    /// §5.9's exception: a `shared` value's field is reached through the
    /// handle, so writing it through a `ref` to the handle is allowed.
    #[test]
    fn mir_borrowck_shared_field_through_a_ref_is_allowed() {
        let errs = errors(
            "
struct S { a: i64 }

fn main() -> () {
    let mut _0: ();
    let _1: shared S;
    let _2: ref shared S;
    bb0: {
        _1 = shared S { const 1_i64 };
        _2 = &_1;
        (*_2).0 = const 2_i64;
        _0 = const ();
        return;
    }
}
",
        );
        assert_eq!(errs, Vec::<String>::new());
    }

    #[test]
    fn mir_borrowck_accepts_the_built_pins() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut n = 0;
        let mut bad = Vec::new();
        for dir in ["tests/mir/core-built", "tests/mir/closures-built"] {
            for e in std::fs::read_dir(root.join(dir)).unwrap() {
                let path = e.unwrap().path();
                let m = parse_module(&std::fs::read_to_string(&path).unwrap()).unwrap();
                for b in &m.bodies {
                    if let Err(es) = check_borrows(b, &m.tys, &receivers(&m)) {
                        bad.push(format!("{}: {}: {es:?}", path.display(), b.instance.name));
                    }
                }
                n += 1;
            }
        }
        assert!(n >= 25, "{n} pins");
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Builds `src` from source and runs the move and borrow checks on
    /// every body: `Err` names the stage that stopped before them, `Ok`
    /// holds one line per refused body. A callee with no body is a library
    /// native, whose result borrows only from its receiver.
    fn check_built(src: &str) -> Result<Vec<String>, String> {
        let parsed = crate::parse(src);
        if !parsed.errors.is_empty() {
            return Err("parse".into());
        }
        let mut program = parsed.program;
        crate::prepare_for_resolve(&mut program);
        let r = crate::resolve(&program);
        if !r.errors.is_empty() {
            return Err("resolve".into());
        }
        let tc = crate::typecheck(&program, &r);
        if !tc.errors.is_empty() {
            return Err(format!("typecheck: {}", tc.errors[0].message));
        }
        let defs = crate::def_table::ProgramDefs::build_for_program(&program);
        let res = crate::node_res::node_res(&r, &defs, 0, None);
        let hir =
            crate::typed_hir::build(&tc, &crate::typed_hir::ProgramHirDefs::new(&defs, 0, &res));
        let own = crate::ownershipcheck(&program, &tc);
        let mut lowered = crate::mir::lower::lower_program(
            &program,
            &tc,
            &defs,
            &res,
            &r,
            hir,
            &own.closure_captures,
        );
        if !lowered.errors.is_empty() {
            return Err(format!("build: {}", lowered.errors[0]));
        }
        let methods: Vec<(String, bool)> = lowered
            .program
            .bodies
            .values()
            .map(|b| {
                let recv = b.instance.name.contains('.')
                    && b.arg_count > 0
                    && matches!(
                        lowered.tys.kind(b.locals[1].ty),
                        TyKind::Ref(_) | TyKind::MutRef(_)
                    );
                (b.instance.name.clone(), recv)
            })
            .collect();
        let has_receiver = |c: &InstanceId| {
            methods
                .iter()
                .find(|(n, _)| *n == c.name)
                .is_none_or(|(_, r)| *r)
        };
        let mut refused = Vec::new();
        for body in lowered.program.bodies.values_mut() {
            let name = body.instance.name.clone();
            if let Err(e) = crate::mir::check_moves(body, &lowered.tys) {
                refused.push(format!("move check {name}: {}", e.join("; ")));
            } else if let Err(e) = check_borrows(body, &lowered.tys, &has_receiver) {
                refused.push(format!("borrow check {name}: {}", e.join("; ")));
            }
        }
        Ok(refused)
    }

    /// The core pins built from source: no runnable pin is refused, and
    /// each `err_` pin whose rule MIR checks is refused by the right check.
    /// A pin the builder cannot lower yet is skipped, so each table row
    /// names whether it must build today.
    #[test]
    fn mir_borrowck_core_pins_built_from_source() {
        // (pin, must build today, the start of its first refusal).
        let err_pins: &[(&str, bool, &str)] = &[
            ("err_maybe_moved", true, "move check f:"),
            ("err_moved_in_loop", true, "move check main:"),
            ("err_partial_then_whole", true, "move check main:"),
            ("err_use_after_move", true, "move check main:"),
            ("err_move_out_of_ref", true, "borrow check name_of:"),
            ("err_ref_from_temp", true, "borrow check main:"),
            ("err_write_through_ref", true, "borrow check add:"),
            ("err_escaping_capture_reused", false, "borrow check"),
            ("err_store_nonescaping_param", false, "borrow check"),
            ("err_taskgroup_origin_declared_after", false, "borrow check"),
            ("err_taskgroup_write_while_borrowed", false, "borrow check"),
        ];
        // Rules checked before MIR: `par` effects and Drop-type moves.
        let not_mir = ["err_par_conflict", "err_partial_move_drop_type"];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/core");
        let mut pins: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        pins.sort();
        let (mut ok_checked, mut bad) = (0, Vec::new());
        for dir in pins {
            let pin = dir.file_name().unwrap().to_str().unwrap().to_string();
            if not_mir.contains(&pin.as_str()) {
                continue;
            }
            let src = std::fs::read_to_string(dir.join("source.kara")).unwrap();
            let verdict = check_built(&src);
            if !pin.starts_with("err_") {
                // Runnable pins the builder cannot lower yet.
                if crate::mir::lower::tests::NOT_YET.contains(&pin.as_str()) {
                    continue;
                }
                match verdict {
                    Ok(refused) if refused.is_empty() => ok_checked += 1,
                    Ok(refused) => bad.push(format!("{pin}: {}", refused.join(" | "))),
                    Err(e) => bad.push(format!("{pin}: {e}")),
                }
                continue;
            }
            let Some((_, must_build, want)) = err_pins.iter().find(|(p, ..)| *p == pin) else {
                bad.push(format!("{pin}: not in this test's table"));
                continue;
            };
            match verdict {
                Ok(refused) if refused.first().is_some_and(|r| r.starts_with(want)) => {}
                Ok(refused) => bad.push(format!("{pin}: want {want}, got {refused:?}")),
                Err(e) if *must_build => bad.push(format!("{pin}: {e}")),
                Err(_) => {}
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        assert!(ok_checked >= 20, "only {ok_checked} runnable pins checked");
    }
}
