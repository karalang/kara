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
//!   callees have one ([`ResultBorrows`]).
//! - A result whose references come only through a type parameter or an
//!   associated type of the receiver's type (`next(mut ref self) ->
//!   Option[Self.Item]`) borrows what the receiver's value borrows, not
//!   the receiver's place (§5.4), so two `it.next()` results can be live
//!   together. Such a body may return no borrow of a place its receiver
//!   owns: an iterator whose items point into itself is an error. A call
//!   of a closure through a reference likewise borrows what the closure
//!   borrows, not the closure.
//!   Only a value whose type can hold a reference has origins. A call
//!   with a whole-local `mut ref` argument may store the other arguments'
//!   borrows into its pointee, so the pointee's origins grow by theirs
//!   (§5.3); so does `(*r) = v`. `TaskGroup.spawn` stores its closure
//!   through `ref self` (§9.5), so a whole-local `ref TaskGroup` argument
//!   does the same.
//! - §5.9: a write, mutable borrow or non-`Copy` move through a `ref` is
//!   an error unless a `shared` handle is projected after that deref.
//! - §6.2: a `mut` field of a shared value is a place rooted at the handle
//!   local, so a conflict through the same handle is found like any other.
//!   Through another handle the places differ and the run-time borrow flag
//!   decides.
//! - Each error ends with its code, shared with `karac check`: E0516 for
//!   an access that conflicts with a live borrow, E0517 for §5.9 and E0509
//!   for a result that borrows a place this function owns.
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

use std::collections::BTreeMap;

use super::place_ty::{place_ty, PlaceTy};
use super::pretty::place as show_place;
use super::syntax::*;
use super::ty::{IntrinsicTy, Ty, TyInterner, TyKind};
use crate::ty::TyKind as SharedKind;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Access {
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
    /// The place is reached through a shared reference held in its local
    /// (`&(*r).0` with `r: ref T`): the loan is of the pointee, so ending
    /// or overwriting `r` itself leaves it valid; its value keeps `r`'s
    /// loans instead.
    through_shared_ref: bool,
    /// The place is reached through a `mut ref` held in its local.
    through_mut_ref: bool,
    /// Where it was created: block and statement.
    at: (usize, usize),
}

/// A set of loans, by index: sorted and without repeats. Most values hold
/// none or one, so a set is a short list rather than a row of flags, and an
/// empty one allocates nothing (B-2026-10-15-57).
#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct LoanSet(Vec<u32>);

impl LoanSet {
    fn insert(&mut self, li: usize) -> bool {
        match self.0.binary_search(&(li as u32)) {
            Ok(_) => false,
            Err(i) => {
                self.0.insert(i, li as u32);
                true
            }
        }
    }

    /// Adds `other`'s loans; whether any was new.
    fn union(&mut self, other: &LoanSet) -> bool {
        if other.0.is_empty() {
            return false;
        }
        let before = self.0.len();
        if before == 0 {
            self.0.clone_from(&other.0);
            return true;
        }
        let mut out = Vec::with_capacity(before + other.0.len());
        let (mut i, mut j) = (0, 0);
        while i < self.0.len() && j < other.0.len() {
            let (a, b) = (self.0[i], other.0[j]);
            out.push(a.min(b));
            i += usize::from(a <= b);
            j += usize::from(b <= a);
        }
        out.extend_from_slice(&self.0[i..]);
        out.extend_from_slice(&other.0[j..]);
        let grew = out.len() != before;
        self.0 = out;
        grew
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().map(|&l| l as usize)
    }
}

static NO_LOANS: LoanSet = LoanSet(Vec::new());

/// Per local, the loans its value may hold; only the locals that hold any
/// are kept, so a state costs what it holds, not locals times loans
/// (B-2026-10-15-57).
#[derive(Clone, Default, Debug)]
struct Origins(BTreeMap<u32, LoanSet>);

impl Origins {
    fn get(&self, l: Local) -> &LoanSet {
        self.0.get(&(l.index() as u32)).unwrap_or(&NO_LOANS)
    }

    fn set(&mut self, l: Local, v: LoanSet) {
        if v.is_empty() {
            self.0.remove(&(l.index() as u32));
        } else {
            self.0.insert(l.index() as u32, v);
        }
    }

    fn clear(&mut self, l: Local) {
        self.0.remove(&(l.index() as u32));
    }

    fn insert(&mut self, l: Local, li: usize) {
        self.0.entry(l.index() as u32).or_default().insert(li);
    }

    fn union_into(&mut self, l: Local, v: &LoanSet) {
        if !v.is_empty() {
            self.0.entry(l.index() as u32).or_default().union(v);
        }
    }

    /// Adds `other`'s origins; whether anything was new.
    fn join(&mut self, other: &Origins) -> bool {
        let mut changed = false;
        for (l, v) in &other.0 {
            changed |= self.0.entry(*l).or_default().union(v);
        }
        changed
    }

    /// The loans some local of `live` may hold, and the loans a borrowed
    /// local holds while a borrow of it is live: a reference to a `Lend`
    /// that holds `mut ref o` reads `o` through it, whether or not the
    /// `Lend` itself is used again (B-2026-10-24-5).
    fn live_loans(&self, live: &[Local], loans: &[Loan]) -> LoanSet {
        let mut out = LoanSet::default();
        for l in live {
            out.union(self.get(*l));
        }
        let mut work: Vec<usize> = out.iter().collect();
        while let Some(li) = work.pop() {
            let place = &loans[li].place;
            // A reborrow through `r` (`&(*r).f`) already holds `r`'s loans
            // itself: `transfer` adds them at the `Ref`.
            if place.projection.contains(&ProjElem::Deref) {
                continue;
            }
            for held in self.get(place.local).iter() {
                if out.insert(held) {
                    work.push(held);
                }
            }
        }
        out
    }
}

/// What a call's result borrows (§5.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResultBorrows {
    /// What every argument holds.
    Args,
    /// What the first argument, a `ref self` / `mut ref self`, holds.
    Receiver,
    /// What the value the first argument refers to holds: the result's
    /// references come only through the receiver type's parameters or
    /// associated types.
    ReceiverValue,
    /// What the values the receiver's value refers to hold: a view whose
    /// receiver holds its parameter only through references
    /// (`IterByRef.next`), so the result borrows what the referenced
    /// iterator holds, not the reference to it (B-2026-10-21-59).
    ReceiverPointeeValue,
    /// A closure body that returns a closure, called with its environment
    /// first: what the environment holds and what the other arguments
    /// hold, never the environment itself, since the returned closure
    /// owns its captures (core semantics §9.3; B-2026-10-24-9).
    EnvAndArgs,
}

/// Checks `body`; the errors name each conflicting access by block and
/// statement. `has_receiver` says what each callee's result borrows, this
/// body's own included. Read-only: `check_moves` is what marks a body
/// `Checked`.
pub fn check_borrows(
    body: &Body,
    tys: &TyInterner,
    has_receiver: &dyn Fn(&InstanceId) -> ResultBorrows,
) -> Result<(), Vec<String>> {
    let errs = borrow_errors(body, tys, has_receiver);
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.into_iter().map(|e| e.text).collect())
    }
}

/// Whether blocks `a` and `b` lie in two different branches of one `par`
/// region of `body`.
fn in_sibling_branches(body: &Body, a: usize, b: usize) -> bool {
    let (a, b) = (BasicBlock(a as u32), BasicBlock(b as u32));
    body.par_regions.iter().any(|r| {
        let of = |x| r.branches.iter().position(|br| br.contains(&x));
        matches!((of(a), of(b)), (Some(i), Some(j)) if i != j)
    })
}

/// One error of [`check_borrows`], with what a report in the program's own
/// terms needs (`karac check`, B-2026-10-19-7): where it is, its code, and
/// the places it names as the program spells them, where it does.
#[derive(Clone, Debug)]
pub struct BorrowError {
    /// The error as [`check_borrows`] reports it.
    pub text: String,
    /// `E0516`, `E0517` or `E0509`; empty for the §9.3 function-value one.
    pub code: &'static str,
    /// The source `line:column` of the access, when known.
    pub at: Option<(usize, usize)>,
    /// The access (`write`, `mutable borrow`, ...), or empty for E0509.
    pub access: &'static str,
    /// The place accessed (the one borrowed, for E0509), in source terms.
    pub place: Option<String>,
    /// The source `line:column` of the conflicting borrow, when known.
    pub loan_at: Option<(usize, usize)>,
    /// A named local still holding that borrow at the access.
    pub holder: Option<String>,
    /// When the place is a temporary's: the source `line:column` of the
    /// expression whose value it holds, which the program does not name
    /// (B-2026-10-24-10).
    pub temporary: Option<(usize, usize)>,
}

impl BorrowError {
    fn new(text: String, code: &'static str, at: Option<(usize, usize)>) -> Self {
        BorrowError {
            text,
            code,
            at,
            access: "",
            place: None,
            loan_at: None,
            holder: None,
            temporary: None,
        }
    }
}

/// [`check_borrows`]'s errors, each with its places.
pub fn borrow_errors(
    body: &Body,
    tys: &TyInterner,
    has_receiver: &dyn Fn(&InstanceId) -> ResultBorrows,
) -> Vec<BorrowError> {
    let mut errs = read_only_errors(body, tys);
    errs.extend(view_into_shared_errors(body, tys));
    let loans = collect_loans(body, tys);
    if loans.is_empty() {
        return errs;
    }
    let entry = Origins::default();
    let cx = Cx {
        body,
        tys,
        loans: &loans,
        has_receiver,
    };
    let origins = origins_dataflow(&cx, entry);
    let drop_uses = |p: &Place| drop_reads_borrows(body, tys, p);
    let live_out = liveness_with(body, &drop_uses);
    let view = body.arg_count > 0
        && matches!(
            has_receiver(&body.instance),
            ResultBorrows::ReceiverValue | ResultBorrows::ReceiverPointeeValue
        );

    for (bi, block) in body.blocks.iter().enumerate() {
        let Some(mut o) = origins[bi].clone() else {
            continue; // unreachable
        };
        // Liveness before each statement, walking back from the block end.
        let n = block.statements.len();
        let mut live_before = vec![Vec::new(); n + 1];
        let mut live = live_out[bi].clone();
        let (u, d) = terminator_use_def_with(&block.terminator.kind, &drop_uses);
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
            let live_loans = if accesses.is_empty() {
                LoanSet::default()
            } else {
                o.live_loans(live, &loans)
            };
            for (place, access) in accesses {
                for li in live_loans.iter() {
                    let loan = &loans[li];
                    if loan.at == at {
                        continue;
                    }
                    // Two branches of one `par` run as tasks of one group,
                    // so a branch's borrow lives while the others run; the
                    // `par` check refuses that conflict, naming both branches
                    // (core semantics §11.2; B-2026-10-21-68).
                    if in_sibling_branches(body, loan.at.0, bi) {
                        continue;
                    }
                    if conflicts(loan, &place, access) {
                        let text = format!(
                            "{}: {} of {} while it is borrowed{} (borrow at {}) [E0516]",
                            site(body, at, Some(loan.at)),
                            access_name(access),
                            show_place(body, tys, &place),
                            if loan.kind == BorrowKind::Mut {
                                " mutably"
                            } else {
                                ""
                            },
                            borrow_site(body, loan.at),
                        );
                        // Of several named holders, the one declared
                        // last, whatever order the live set is in.
                        let holder = live
                            .iter()
                            .filter(|l| o.get(**l).iter().any(|h| h == li))
                            .filter_map(|l| local_name(body, *l).map(|n| (*l, n)))
                            .max_by_key(|(l, _)| *l)
                            .map(|(_, n)| n);
                        errs.push(BorrowError {
                            access: access_name(access),
                            place: source_place(body, tys, &place),
                            loan_at: source_at(body, loan.at),
                            holder,
                            temporary: temporary_at(body, &place),
                            ..BorrowError::new(text, "E0516", site_at(body, at, Some(loan.at)))
                        });
                    }
                }
            }
            // A view stored where no borrow is kept, in an escaping
            // closure or a `shared` value (§5.7, §9.3), would outlive
            // what it views unchecked.
            let unkept = if si < n {
                cx.unkept_loans(&o, &block.statements[si].kind)
            } else if let TerminatorKind::Call { func, args, .. } = &block.terminator.kind {
                cx.unkept_call_loans(&o, func, args)
            } else {
                LoanSet::default()
            };
            for li in unkept.iter() {
                let loan = &loans[li];
                let text = format!(
                        "{}: a view of {} is stored in an escaping closure, a `shared` value or a channel, where its borrow is not kept (borrow at {}); store an owned copy instead (core semantics §5.7, §9.3) [E0509]",
                        site(body, at, Some(loan.at)),
                        show_place(body, tys, &loan.place),
                        borrow_site(body, loan.at),
                    );
                errs.push(BorrowError {
                    place: source_place(body, tys, &loan.place),
                    loan_at: source_at(body, loan.at),
                    ..BorrowError::new(text, "E0509", site_at(body, at, Some(loan.at)))
                });
            }
            if si < n {
                cx.transfer(&mut o, &block.statements[si].kind, at);
            } else if let TerminatorKind::Return = block.terminator.kind {
                for li in o.get(Local::RETURN_PLACE).clone().iter() {
                    let loan = &loans[li];
                    // A slice is a view: a borrow of one borrows what it
                    // views, as a borrow through a reference does.
                    let view_of = place_ty(body, tys, &loan.place)
                        .is_ok_and(|pt| matches!(tys.kind(pt.ty), TyKind::Slice { .. }));
                    let derefs = loan
                        .place
                        .projection
                        .iter()
                        .filter(|p| **p == ProjElem::Deref)
                        .count()
                        + usize::from(view_of);
                    let owned = if derefs == 0 {
                        "this function owns"
                    } else if view && derefs == 1 && loan.place.local.index() == 1 {
                        "the receiver owns; an item may borrow only what its `ref` fields borrow"
                    } else {
                        continue;
                    };
                    let text = format!(
                        "{}: the result borrows {}, which {} (borrow at {}) [E0509]",
                        site(body, at, Some(loan.at)),
                        show_place(body, tys, &loan.place),
                        owned,
                        borrow_site(body, loan.at),
                    );
                    errs.push(BorrowError {
                        place: source_place(body, tys, &loan.place),
                        loan_at: source_at(body, loan.at),
                        ..BorrowError::new(text, "E0509", site_at(body, at, Some(loan.at)))
                    });
                }
            }
        }
    }
    errs.dedup_by(|a, b| a.text == b.text);
    errs
}

/// A loan as the flag pass sees it: kind, place and `Ref` site.
pub(super) type LoanSite = (BorrowKind, Place, (usize, usize));

/// The loans of `body`, by `Ref` site in block order, and for each block
/// and each point in it (every statement, then the terminator) the loans
/// live just before that point: those some live local may hold. The flag
/// pass releases a run-time borrow flag where its loan stops being live.
/// When `track` holds for no loan, nothing is computed and the liveness is
/// empty: the points times loans table is what makes a body with many
/// exits and many loans slow (B-2026-10-21-33).
pub(super) fn loan_liveness(
    body: &Body,
    tys: &TyInterner,
    has_receiver: &dyn Fn(&InstanceId) -> ResultBorrows,
    track: &dyn Fn(BorrowKind, &Place) -> bool,
) -> (Vec<LoanSite>, Vec<Vec<Vec<bool>>>) {
    let loans = collect_loans(body, tys);
    if !loans.iter().any(|l| track(l.kind, &l.place)) {
        let loans = loans.into_iter().map(|l| (l.kind, l.place, l.at)).collect();
        return (loans, Vec::new());
    }
    let entry = Origins::default();
    let cx = Cx {
        body,
        tys,
        loans: &loans,
        has_receiver,
    };
    let origins = origins_dataflow(&cx, entry);
    let drop_uses = |p: &Place| drop_reads_borrows(body, tys, p);
    let live_out = liveness_with(body, &drop_uses);
    let mut out = Vec::with_capacity(body.blocks.len());
    for (bi, block) in body.blocks.iter().enumerate() {
        let n = block.statements.len();
        let Some(mut o) = origins[bi].clone() else {
            out.push(vec![vec![false; loans.len()]; n + 1]); // unreachable
            continue;
        };
        let mut live_before = vec![Vec::new(); n + 1];
        let mut live = live_out[bi].clone();
        let (u, d) = terminator_use_def_with(&block.terminator.kind, &drop_uses);
        transfer_live(&mut live, &u, &d);
        live_before[n] = live.clone();
        for si in (0..n).rev() {
            let (u, d) = statement_use_def(&block.statements[si].kind);
            transfer_live(&mut live, &u, &d);
            live_before[si] = live.clone();
        }
        let mut points = Vec::with_capacity(n + 1);
        for (si, live) in live_before.iter().enumerate() {
            let mut point = vec![false; loans.len()];
            for li in o.live_loans(live, &loans).iter() {
                point[li] = true;
            }
            points.push(point);
            if si < n {
                cx.transfer(&mut o, &block.statements[si].kind, (bi, si));
            }
        }
        out.push(points);
    }
    let loans = loans.into_iter().map(|l| (l.kind, l.place, l.at)).collect();
    (loans, out)
}

/// §5.9: nothing is written through a `ref`. A write, a mutable borrow
/// or a move of a non-`Copy` value whose place goes through a `ref` is an
/// error, unless a projection on a `shared` handle comes after that
/// dereference: a `shared` value's fields are reached through the handle,
/// and which of them are `mut` is the type checker's question.
fn read_only_errors(body: &Body, tys: &TyInterner) -> Vec<BorrowError> {
    let mut errs = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        let n = block.statements.len();
        for si in 0..=n {
            let accesses = if si < n {
                statement_accesses(&block.statements[si].kind)
            } else {
                terminator_accesses(&block.terminator.kind)
            };
            // §9.3: an erased function value is owned and may be stored,
            // so erasing one reached through a reference stores a borrowed
            // closure (a non-escaping parameter), whether or not it is `Copy`.
            if si < n {
                if let StatementKind::Assign(
                    _,
                    Rvalue::Cast(CastKind::Erase, Operand::Copy(p) | Operand::Move(p), _),
                ) = &block.statements[si].kind
                {
                    if p.projection.contains(&ProjElem::Deref) {
                        let text = format!(
                            "{}: {} is borrowed, so it cannot be stored as a function value; declare the parameter `escaping` (core semantics §9.3)",
                            site(body, (bi, si), None),
                            show_place(body, tys, p),
                        );
                        errs.push(BorrowError {
                            place: source_place(body, tys, p),
                            ..BorrowError::new(text, "", site_at(body, (bi, si), None))
                        });
                    }
                }
            }
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
                    let text = format!(
                        "{}: {} of {} through a shared reference [E0517]",
                        site(body, (bi, si), None),
                        what,
                        show_place(body, tys, &place),
                    );
                    errs.push(BorrowError {
                        access: what,
                        place: source_place(body, tys, &place),
                        ..BorrowError::new(text, "E0517", site_at(body, (bi, si), None))
                    });
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

/// Does `place` name a field of a `shared` value (`s.items`, `(*s).items`
/// through a `ref` handle)? A view may not go there (core semantics §5.7).
fn in_shared_field(body: &Body, tys: &TyInterner, place: &Place) -> bool {
    (0..place.projection.len()).any(|i| {
        let prefix = Place {
            local: place.local,
            projection: place.projection[..i].to_vec(),
        };
        matches!(place.projection[i], ProjElem::Field(..))
            && place_ty(body, tys, &prefix)
                .is_ok_and(|pt| matches!(tys.kind(pt.ty), TyKind::Shared(_)))
    })
}

/// Whether a value of type `ty` is or holds a view: a `ref`, a `Str`, a
/// slice, or a collection, tuple or struct with one in it. A `shared`
/// handle is not a view, and a function value's captures are the by-loan
/// check's (§9.3).
fn holds_view(tys: &TyInterner, ty: Ty) -> bool {
    fn go(tys: &TyInterner, ty: Ty, seen: &mut Vec<Ty>) -> bool {
        if seen.contains(&ty) {
            return false;
        }
        seen.push(ty);
        match tys.kind(ty) {
            TyKind::Ref(_) | TyKind::MutRef(_) | TyKind::Str | TyKind::Slice(_) => true,
            TyKind::Tuple(ts) => ts.iter().any(|t| go(tys, *t, seen)),
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
                let is_enum = adt.is_enum;
                adt.variants.iter().enumerate().any(|(v, var)| {
                    (0..var.fields.len() as u32).any(|f| {
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

/// The field of a `shared` value statement `s` stores a view-typed value
/// in, with that value's type.
fn stored_view(body: &Body, tys: &TyInterner, s: &StatementKind) -> Option<(Place, Ty)> {
    let StatementKind::Assign(dest, rv) = s else {
        return None;
    };
    if !in_shared_field(body, tys, dest) {
        return None;
    }
    let op_ty = |op: &Operand| match op {
        Operand::Copy(p) | Operand::Move(p) => place_ty(body, tys, p).ok().map(|pt| pt.ty),
        Operand::Const(c) => Some(c.ty),
    };
    let t = match rv {
        Rvalue::Ref(..) => place_ty(body, tys, dest).ok().map(|pt| pt.ty),
        Rvalue::Use(op) | Rvalue::Cast(_, op, _) => op_ty(op),
        Rvalue::Aggregate(_, ops) => ops.iter().filter_map(op_ty).find(|t| holds_view(tys, *t)),
        _ => None,
    }?;
    holds_view(tys, t).then(|| (dest.clone(), t))
}

/// A view-typed value stored in a field of a `shared` value (core
/// semantics §5.7): assigned there, or passed to a call beside a `mut ref`
/// lending that field, whose type can hold it (`s.items.push(r)`). Decided
/// by type, so it holds where the view arrives as a parameter and the body
/// takes no loan of it, as in a generic instance (`put[ref String]`, §5.8;
/// B-2026-10-19-43).
fn view_into_shared_errors(body: &Body, tys: &TyInterner) -> Vec<BorrowError> {
    let mut errs = Vec::new();
    // What a `mut ref` temporary lends: the place of its one `&mut`.
    let lent = |l: Local| {
        body.blocks
            .iter()
            .flat_map(|b| &b.statements)
            .find_map(|s| match &s.kind {
                StatementKind::Assign(d, Rvalue::Ref(BorrowKind::Mut, p))
                    if d.local == l && d.projection.is_empty() =>
                {
                    Some(p.clone())
                }
                _ => None,
            })
    };
    // A generic instance is named, as §5.8 does: its body is the generic
    // one, which stores no view until a view type fills its parameter.
    let subject = |t: Ty| {
        if body.instance.args.is_empty() {
            format!("a view (`{}`) is stored", tys.display(t))
        } else {
            format!(
                "the instance {} stores a view (`{}`)",
                body.instance.name,
                tys.display(t)
            )
        }
    };
    for (bi, block) in body.blocks.iter().enumerate() {
        let n = block.statements.len();
        for si in 0..=n {
            let stored = if si < n {
                stored_view(body, tys, &block.statements[si].kind)
            } else if let TerminatorKind::Call { args, .. } = &block.terminator.kind {
                args.iter().enumerate().find_map(|(i, a)| {
                    let p = a.place().filter(|p| p.projection.is_empty())?;
                    let pt = place_ty(body, tys, p).ok()?;
                    let TyKind::MutRef(holder) = tys.kind(pt.ty) else {
                        return None;
                    };
                    let field = lent(p.local).filter(|f| in_shared_field(body, tys, f))?;
                    args.iter().enumerate().find_map(|(j, b)| {
                        let q = b.place().filter(|_| j != i)?;
                        let t = place_ty(body, tys, q).ok()?.ty;
                        (holds_view(tys, t) && contains_ty(tys, holder, t))
                            .then(|| (field.clone(), t))
                    })
                })
            } else {
                None
            };
            let Some((field, t)) = stored else {
                continue;
            };
            let text = format!(
                "{}: {} in {}, a field of a `shared` value, where a view may not go; store an owned copy instead (core semantics §5.7, §5.8) [E0509]",
                site(body, (bi, si), None),
                subject(t),
                source_place(body, tys, &field)
                    .map_or_else(|| show_place(body, tys, &field), |p| format!("`{p}`")),
            );
            errs.push(BorrowError {
                place: source_place(body, tys, &field),
                ..BorrowError::new(text, "E0509", site_at(body, (bi, si), None))
            });
        }
    }
    errs
}

/// The source `line:column` of statement `si` of block `bi` (its
/// terminator past the last statement), when the builder recorded one.
fn source_at(body: &Body, (bi, si): (usize, usize)) -> Option<(usize, usize)> {
    let block = &body.blocks[bi];
    let span = match block.statements.get(si) {
        Some(s) => s.source_info.span,
        None => block.terminator.source_info.span,
    };
    (span.line > 0).then_some((span.line, span.column))
}

/// Where an error is: `line:column: bbN[i]`, or the block position alone
/// in a body with no source (MIR text), as every error names its source
/// line (B-2026-10-15-16). A drop the builder put at a scope's end has no
/// span of its own; the line is then the last one before it in its block,
/// else that of `near` (the borrow it conflicts with).
fn site(body: &Body, at: (usize, usize), near: Option<(usize, usize)>) -> String {
    let l = loc(at, body.blocks[at.0].statements.len());
    match site_at(body, at, near) {
        Some((line, col)) => format!("{line}:{col}: {l}"),
        None => l,
    }
}

/// The source `line:column` [`site`] names.
fn site_at(
    body: &Body,
    at: (usize, usize),
    near: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    source_at(body, at)
        .or_else(|| (0..at.1).rev().find_map(|si| source_at(body, (at.0, si))))
        .or_else(|| near.and_then(|n| source_at(body, n)))
}

/// The name the program gives local `l`, if it gives one.
fn local_name(body: &Body, l: Local) -> Option<String> {
    match &body.local(l).kind {
        LocalKind::Arg { name, .. } | LocalKind::User { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// Where the expression a temporary `place` holds is, for a place the
/// program has no name for.
fn temporary_at(body: &Body, p: &Place) -> Option<(usize, usize)> {
    let decl = body.local(p.local);
    let span = decl.source_info.span;
    (matches!(decl.kind, LocalKind::Temp) && p.projection.is_empty() && span.line > 0)
        .then_some((span.line, span.column))
}

/// `place` as the program would write it (`self.index`), when its local is
/// one the program names: a dereference of a reference is implicit, and a
/// field is named. `None` for a temporary's place, or an indexed one.
fn source_place(body: &Body, tys: &TyInterner, p: &Place) -> Option<String> {
    let mut s = local_name(body, p.local)?;
    let mut cur = Place::local(p.local);
    for elem in &p.projection {
        match elem {
            ProjElem::Deref => {}
            ProjElem::Downcast(_) => {}
            ProjElem::Field(f, _) => {
                let PlaceTy { ty, variant } = place_ty(body, tys, &cur).ok()?;
                let name = match tys.kind(ty) {
                    TyKind::Adt(a) | TyKind::Shared(a) => tys
                        .adt(a)
                        .variants
                        .get(variant.unwrap_or(0) as usize)
                        .and_then(|v| v.fields.get(f.0 as usize))
                        .map(|(n, _)| n.clone()),
                    TyKind::Tuple(_) => Some(f.0.to_string()),
                    _ => None,
                }?;
                s = format!("{s}.{name}");
            }
            ProjElem::Index(_) | ProjElem::ConstIndex(_) => return None,
        }
        cur = cur.project(*elem);
    }
    Some(s)
}

/// Where a borrow was taken: `bbN[i]`, then ` at line:column` when known.
fn borrow_site(body: &Body, at: (usize, usize)) -> String {
    let l = loc(at, body.blocks[at.0].statements.len());
    match source_at(body, at) {
        Some((line, col)) => format!("{l} at {line}:{col}"),
        None => l,
    }
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

fn collect_loans(body: &Body, tys: &TyInterner) -> Vec<Loan> {
    let mut loans = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        for (si, s) in block.statements.iter().enumerate() {
            if let StatementKind::Assign(_, Rvalue::Ref(kind, place)) = &s.kind {
                let through_shared_ref = place.projection.first() == Some(&ProjElem::Deref)
                    && matches!(tys.kind(body.local(place.local).ty), TyKind::Ref(_));
                let through_mut_ref = place.projection.first() == Some(&ProjElem::Deref)
                    && matches!(tys.kind(body.local(place.local).ty), TyKind::MutRef(_));
                loans.push(Loan {
                    kind: *kind,
                    place: place.clone(),
                    through_shared_ref,
                    through_mut_ref,
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
    has_receiver: &'a dyn Fn(&InstanceId) -> ResultBorrows,
}

impl Cx<'_> {
    /// Can a value of `place`'s type hold a borrow? Only those carry
    /// origins: `x = *r` with `x: i64` holds nothing of `r`.
    fn holds(&self, place: &Place) -> bool {
        place_ty(self.body, self.tys, place).map_or(true, |pt| holds_borrow(self.tys, pt.ty))
    }

    /// The origins of a value read from `place`: what its local holds,
    /// when its type can hold any.
    fn place_origins(&self, o: &Origins, place: &Place, into: &mut LoanSet) {
        if !self.holds(place) {
            return;
        }
        // A value read through one reference (`*r`, `(*r).f`) holds what
        // the places `r` borrows hold, not `r`'s borrow of them: a `Str`
        // copied out of `v[0]` views what the `Vec`'s elements view, and
        // outlives the `Vec` (B-2026-10-18-50).
        let through_one = place.projection.first() == Some(&ProjElem::Deref)
            && !place.projection[1..].contains(&ProjElem::Deref);
        if through_one {
            for li in o.get(place.local).iter() {
                let l = &self.loans[li];
                if l.place.projection.contains(&ProjElem::Deref) {
                    // A borrow of something behind another reference:
                    // what it holds came from outside, as before.
                    into.insert(li);
                } else {
                    self.place_origins(o, &l.place, into);
                }
            }
            return;
        }
        into.union(o.get(place.local));
    }

    fn operand_origins(&self, o: &Origins, op: &Operand, into: &mut LoanSet) {
        if let Some(p) = op.place() {
            self.place_origins(o, p, into);
        }
    }

    /// What the values `op`'s references point at may hold: for each loan
    /// `op` holds of a place not reached through a reference, what that
    /// place's local holds. A place behind a reference holds only what
    /// came from outside this body.
    /// Whether `op` is a reference to a `str`.
    fn refers_to_str(&self, op: &Operand) -> bool {
        op.place()
            .and_then(|p| place_ty(self.body, self.tys, p).ok())
            .is_some_and(|pt| match self.tys.kind(pt.ty) {
                TyKind::Ref(t) | TyKind::MutRef(t) => matches!(self.tys.kind(t), TyKind::Str),
                _ => false,
            })
    }

    fn pointee_origins(&self, o: &Origins, op: &Operand, into: &mut LoanSet) {
        let Some(p) = op.place() else { return };
        for li in o.get(p.local).iter() {
            let l = &self.loans[li];
            if !l.place.projection.contains(&ProjElem::Deref) {
                self.place_origins(o, &l.place, into);
            }
        }
    }

    /// `dest = <value with origins val>`.
    fn assign(&self, o: &mut Origins, dest: &Place, mut val: LoanSet) {
        if !self.holds(dest) {
            val = LoanSet::default();
        }
        if dest.projection.is_empty() {
            o.set(dest.local, val);
        } else if !dest.projection.contains(&ProjElem::Deref) {
            // A part of the local: what the rest held stays.
            o.union_into(dest.local, &val);
        } else {
            // A store through a reference: the places the reference
            // mutably borrows now hold `val` too.
            self.store_through(o, dest.local, &val, false);
        }
    }

    /// A value with origins `val` is stored through the reference held in
    /// local `r`: every local place `r` mutably borrows may now hold it, or
    /// every place it borrows at all when `through_shared` (a receiver that
    /// stores through `ref self`).
    fn store_through(&self, o: &mut Origins, r: Local, val: &LoanSet, through_shared: bool) {
        if val.is_empty() {
            return;
        }
        let targets: Vec<Local> = o
            .get(r)
            .iter()
            .map(|li| &self.loans[li])
            .filter(|l| {
                (through_shared || l.kind == BorrowKind::Mut)
                    && !l.place.projection.contains(&ProjElem::Deref)
                    && self.holds(&l.place)
            })
            .map(|l| l.place.local)
            .collect();
        // A value never holds a borrow of its own local.
        for t in targets {
            for li in val.iter() {
                let own = self.loans[li].place.local == t
                    && !self.loans[li].place.projection.contains(&ProjElem::Deref);
                if !own {
                    o.insert(t, li);
                }
            }
        }
    }

    /// The loans an assignment's value holds that its destination cannot
    /// keep: a value holding a borrow moved into a place whose local's
    /// type holds none, which happens only where a closure escapes (a
    /// function-typed field or element) or in a `shared` value.
    fn unkept_loans(&self, o: &Origins, s: &StatementKind) -> LoanSet {
        let mut val = LoanSet::default();
        let StatementKind::Assign(dest, rv) = s else {
            return val;
        };
        if dest.projection.contains(&ProjElem::Deref)
            || stored_view(self.body, self.tys, s).is_some()
        {
            return val;
        }
        match rv {
            Rvalue::Use(op) | Rvalue::Cast(CastKind::Erase, op, _) => {
                self.operand_origins(o, op, &mut val)
            }
            Rvalue::Aggregate(_, ops) => {
                for op in ops {
                    self.operand_origins(o, op, &mut val);
                }
            }
            _ => return val,
        }
        if self.holds(dest) && self.holds(&Place::local(dest.local)) {
            val = LoanSet::default();
        }
        val
    }

    /// The loans a call stores where they are not kept: a function value
    /// holding a borrow passed with a `mut ref` whose pointee holds no
    /// borrow but can hold that function type, as `v.push(f)` stores `f`
    /// in a `Vec[Fn() -> i64]` (§9.3). A closure passed down, as to
    /// `retain`, is not of a type its receiver holds, so is not stored.
    fn unkept_call_loans(&self, o: &Origins, func: &Operand, args: &[Operand]) -> LoanSet {
        let mut val = LoanSet::default();
        // A view sent through a channel (§5.7) is kept by nothing.
        if let Operand::Const(Const {
            kind: ConstKind::FnDef(callee),
            ..
        }) = func
        {
            let n = &callee.name;
            let channel = n.starts_with("Sender[") || n.starts_with("BoundedChannel[");
            if channel && (n.ends_with(".send") || n.ends_with(".try_send")) {
                for a in args.iter().skip(1) {
                    self.operand_origins(o, a, &mut val);
                }
                return val;
            }
        }
        for (i, a) in args.iter().enumerate() {
            let Some(p) = a.place().filter(|p| p.projection.is_empty()) else {
                continue;
            };
            let Ok(pt) = place_ty(self.body, self.tys, p) else {
                continue;
            };
            let TyKind::MutRef(t) = self.tys.kind(pt.ty) else {
                continue;
            };
            if holds_borrow(self.tys, t) {
                continue;
            }
            for (j, b) in args.iter().enumerate() {
                let Some(q) = b.place().filter(|_| j != i) else {
                    continue;
                };
                let Ok(qt) = place_ty(self.body, self.tys, q) else {
                    continue;
                };
                if matches!(self.tys.kind(qt.ty), TyKind::Fn { .. })
                    && contains_ty(self.tys, t, qt.ty)
                {
                    self.operand_origins(o, b, &mut val);
                }
            }
        }
        val
    }

    fn transfer(&self, o: &mut Origins, s: &StatementKind, at: (usize, usize)) {
        let StatementKind::Assign(dest, rv) = s else {
            if let StatementKind::StorageDead(l) = s {
                o.clear(*l);
            }
            return;
        };
        let mut val = LoanSet::default();
        match rv {
            Rvalue::Ref(_, place) => {
                // Loans are collected in block order, one per `Ref` site.
                if let Ok(li) = self.loans.binary_search_by(|l| l.at.cmp(&at)) {
                    val.insert(li);
                }
                // A reborrow through a reference keeps that reference's
                // loans, whatever the type of the place reborrowed.
                if place.projection.contains(&ProjElem::Deref) {
                    val.union(o.get(place.local));
                }
            }
            // A raw pointer holds no loan (`unsafe` code answers for it).
            Rvalue::Cast(CastKind::RefToPtr, ..) => {}
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
            | Rvalue::Len(_)
            | Rvalue::NullaryOp(..) => {}
        }
        let moved: Vec<&Operand> = match rv {
            Rvalue::Use(op) | Rvalue::Cast(_, op, _) => vec![op],
            Rvalue::Aggregate(_, ops) => ops.iter().collect(),
            _ => Vec::new(),
        };
        clear_moved(o, moved);
        self.assign(o, dest, val);
    }

    /// A call: the result borrows from the receiver when the callee takes
    /// one, else from every argument (§5.4); and a callee given a `mut ref`
    /// may store any other argument's borrows into its pointee, as
    /// `group.spawn(closure)` does (§9.5).
    fn transfer_call(&self, o: &mut Origins, func: &Operand, args: &[Operand], dest: &Place) {
        let borrows = match func {
            Operand::Const(Const {
                kind: ConstKind::FnDef(callee),
                ..
            }) if !args.is_empty() => (self.has_receiver)(callee),
            _ => ResultBorrows::Args,
        };
        // Each whole-local `mut ref` argument may store the other operands'
        // borrows into its pointee (§9.5).
        let before = o.clone();
        for (i, a) in args.iter().enumerate() {
            let Some(p) = a.place() else { continue };
            let Ok(pt) = place_ty(self.body, self.tys, p) else {
                continue;
            };
            let stores = match self.tys.kind(pt.ty) {
                TyKind::MutRef(_) => Some(false),
                // `TaskGroup.spawn` takes `ref self` and still stores its
                // closure's borrows in the group (§9.5).
                TyKind::Ref(t) if is_task_group(self.tys, t) => Some(true),
                _ => None,
            };
            let Some(through_shared) = stores.filter(|_| p.projection.is_empty()) else {
                continue;
            };
            let mut others = LoanSet::default();
            let rest = args.iter().enumerate().filter(|(j, _)| *j != i);
            for a in std::iter::once(func).chain(rest.map(|(_, a)| a)) {
                self.operand_origins(&before, a, &mut others);
            }
            self.store_through(o, p.local, &others, through_shared);
        }
        let mut val = LoanSet::default();
        // A closure called through a reference: what the closure holds.
        let callee_ref = func.place().is_some_and(|p| {
            place_ty(self.body, self.tys, p)
                .is_ok_and(|pt| matches!(self.tys.kind(pt.ty), TyKind::Ref(_) | TyKind::MutRef(_)))
        });
        if callee_ref {
            self.pointee_origins(o, func, &mut val);
        } else {
            self.operand_origins(o, func, &mut val);
        }
        match borrows {
            ResultBorrows::Args => {
                for a in args {
                    self.operand_origins(o, a, &mut val);
                }
            }
            // A `str` is itself a view (core semantics §5.2): what a native
            // makes from a reference to one borrows the `String` it views,
            // not the place holding the view.
            ResultBorrows::Receiver if self.refers_to_str(&args[0]) => {
                self.pointee_origins(o, &args[0], &mut val)
            }
            ResultBorrows::Receiver => self.operand_origins(o, &args[0], &mut val),
            ResultBorrows::EnvAndArgs => {
                let env_ref = args[0].place().is_some_and(|p| {
                    place_ty(self.body, self.tys, p).is_ok_and(|pt| {
                        matches!(self.tys.kind(pt.ty), TyKind::Ref(_) | TyKind::MutRef(_))
                    })
                });
                if env_ref {
                    self.pointee_origins(o, &args[0], &mut val);
                } else {
                    self.operand_origins(o, &args[0], &mut val);
                }
                for a in &args[1..] {
                    self.operand_origins(o, a, &mut val);
                }
            }
            ResultBorrows::ReceiverValue => self.pointee_origins(o, &args[0], &mut val),
            ResultBorrows::ReceiverPointeeValue => {
                let mut held = LoanSet::default();
                self.pointee_origins(o, &args[0], &mut held);
                for li in held.iter() {
                    let l = &self.loans[li];
                    if l.place.projection.contains(&ProjElem::Deref) {
                        // A place behind a reference: what it holds came
                        // from outside, so the loan stands for it.
                        val.insert(li);
                    } else {
                        self.place_origins(o, &l.place, &mut val);
                    }
                }
            }
        }
        clear_moved(o, std::iter::once(func).chain(args));
        self.assign(o, dest, val);
    }
}

/// A local moved out whole holds nothing afterwards, so it holds no borrow
/// either: its borrows went with the value. Its scope-end `drop`, which
/// elaboration removes, then keeps no loan alive.
fn clear_moved<'o>(o: &mut Origins, ops: impl IntoIterator<Item = &'o Operand>) {
    for op in ops {
        if let Operand::Move(p) = op {
            if p.projection.is_empty() {
                o.clear(p.local);
            }
        }
    }
}

/// Whether dropping `place` may read a borrow its value holds, so the drop
/// keeps those borrows live. Freeing a `Vec[Str]`, a `String` or a tuple of
/// references reads no view it holds (Rust's `#[may_dangle]` case,
/// B-2026-10-19-6); anything with a struct or enum in it may run a user
/// `Drop` and is taken to.
fn drop_reads_borrows(body: &Body, tys: &TyInterner, place: &Place) -> bool {
    fn go(tys: &TyInterner, ty: Ty) -> bool {
        match tys.kind(ty) {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Char
            | TyKind::Unit
            | TyKind::Str
            | TyKind::Ref(_)
            | TyKind::MutRef(_)
            | TyKind::Slice(_)
            | TyKind::Intrinsic(IntrinsicTy::String) => false,
            TyKind::Tuple(ts) => ts.iter().any(|t| go(tys, *t)),
            TyKind::Array(t, _)
            | TyKind::Intrinsic(IntrinsicTy::Vec(t) | IntrinsicTy::VecDeque(t)) => go(tys, t),
            _ => true,
        }
    }
    place_ty(body, tys, place).map_or(true, |pt| go(tys, pt.ty))
}

/// Can a value of type `ty` hold a borrow? References and slices can, and
/// so can anything with a part that can; a `shared` handle cannot (§5.7).
/// A type MIR cannot see into is assumed to, and so is `TaskGroup`.
pub(crate) fn holds_borrow(tys: &TyInterner, ty: Ty) -> bool {
    // `stored`: `ty` sits in a struct or enum field or a collection, where
    // a closure escapes (§9.3) and so holds no borrow.
    fn go(tys: &TyInterner, ty: Ty, stored: bool, seen: &mut Vec<(Ty, bool)>) -> bool {
        if seen.contains(&(ty, stored)) {
            return false;
        }
        seen.push((ty, stored));
        match tys.kind(ty) {
            TyKind::Ref(_) | TyKind::MutRef(_) | TyKind::Str | TyKind::Slice(_) | TyKind::Other => {
                true
            }
            // An erased function value may hold whatever its closure
            // captured, unless it was stored, which made it capture by move.
            TyKind::Fn { .. } => !stored,
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => {
                ts.iter().any(|t| go(tys, *t, stored, seen))
            }
            TyKind::Array(t, _) => go(tys, t, stored, seen),
            TyKind::Intrinsic(
                IntrinsicTy::Vec(e)
                | IntrinsicTy::Set(e)
                | IntrinsicTy::VecDeque(e)
                | IntrinsicTy::SortedSet(e),
            ) => go(tys, e, true, seen),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                go(tys, k, true, seen) || go(tys, v, true, seen)
            }
            TyKind::Adt(a) => {
                let adt = tys.adt(a);
                // A task group holds its tasks' closures, which the
                // runtime keeps out of its declared fields (§9.5).
                if is_task_group(tys, ty) {
                    return true;
                }
                let counts: Vec<usize> = adt.variants.iter().map(|v| v.fields.len()).collect();
                let is_enum = adt.is_enum;
                // A field declared as a type parameter holds what the
                // instance put there, a view included (§5.8): an adaptor
                // holding a borrowing closure is a local view. A field
                // declared as a function type holds an escaped closure.
                let decl = tys.tcx().adt_of(ty).map(|(d, _)| d);
                let is_param = |v: usize, f: usize| {
                    decl.as_ref()
                        .and_then(|d| d.variants.get(v)?.fields.get(f).map(|(_, t)| *t))
                        .is_some_and(|t| matches!(tys.tcx().kind(t), SharedKind::Param(_)))
                };
                counts.iter().enumerate().any(|(v, n)| {
                    (0..*n as u32).any(|f| {
                        let stored = !is_param(v, f as usize);
                        tys.field_ty(ty, is_enum.then_some(v as u32), f)
                            .is_some_and(|t| go(tys, t, stored, seen))
                    })
                })
            }
            _ => false,
        }
    }
    go(tys, ty, false, &mut Vec::new())
}

/// Whether a value of type `outer` can have a `inner` inside it: as
/// itself, an element, a key or value, or a field.
fn contains_ty(tys: &TyInterner, outer: Ty, inner: Ty) -> bool {
    fn go(tys: &TyInterner, ty: Ty, inner: Ty, seen: &mut Vec<Ty>) -> bool {
        if ty == inner {
            return true;
        }
        if seen.contains(&ty) {
            return false;
        }
        seen.push(ty);
        match tys.kind(ty) {
            TyKind::Tuple(ts) => ts.iter().any(|t| go(tys, *t, inner, seen)),
            TyKind::Array(t, _) => go(tys, t, inner, seen),
            TyKind::Intrinsic(
                IntrinsicTy::Vec(e)
                | IntrinsicTy::Set(e)
                | IntrinsicTy::VecDeque(e)
                | IntrinsicTy::SortedSet(e),
            ) => go(tys, e, inner, seen),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                go(tys, k, inner, seen) || go(tys, v, inner, seen)
            }
            TyKind::Adt(a) => {
                let adt = tys.adt(a);
                let is_enum = adt.is_enum;
                let counts: Vec<usize> = adt.variants.iter().map(|v| v.fields.len()).collect();
                counts.iter().enumerate().any(|(v, n)| {
                    (0..*n as u32).any(|f| {
                        tys.field_ty(ty, is_enum.then_some(v as u32), f)
                            .is_some_and(|t| go(tys, t, inner, seen))
                    })
                })
            }
            _ => false,
        }
    }
    go(tys, outer, inner, &mut Vec::new())
}

/// `TaskGroup` holds its tasks' closures outside its declared fields and
/// stores them through `ref self` (§9.5), so it is the one type MIR treats
/// as holding borrows whatever it declares.
fn is_task_group(tys: &TyInterner, ty: Ty) -> bool {
    matches!(tys.kind(ty), TyKind::Adt(a) if tys.adt(a).name == "TaskGroup")
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
                Some(cur) => cur.join(&o),
            };
            if changed && !work.contains(&si) {
                work.push(si);
            }
        }
    }
    states
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

/// The locals a terminator uses and defines, where a `Drop` of `place` is
/// a use only when `drop_uses(place)`.
fn terminator_use_def_with(
    t: &TerminatorKind,
    drop_uses: &dyn Fn(&Place) -> bool,
) -> (Vec<Local>, Vec<Local>) {
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
        TerminatorKind::Drop { place, .. } if drop_uses(place) => place_uses(place, &mut uses),
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
pub(super) fn liveness(body: &Body) -> Vec<Vec<Local>> {
    liveness_with(body, &|_| true)
}

/// The blocks of `body` in postorder from the entry, then the ones it
/// does not reach, in block order.
fn postorder(body: &Body) -> Vec<usize> {
    let n = body.blocks.len();
    let mut order = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    let mut stack: Vec<(usize, Vec<usize>)> = Vec::new();
    if n > 0 {
        seen[0] = true;
        let succ = |b: usize| -> Vec<usize> {
            body.blocks[b]
                .terminator
                .kind
                .successors()
                .into_iter()
                .map(|s| s.index())
                .collect()
        };
        stack.push((0, succ(0)));
        while let Some((b, rest)) = stack.last_mut() {
            match rest.pop() {
                Some(s) if !seen[s] => {
                    seen[s] = true;
                    let next = succ(s);
                    stack.push((s, next));
                }
                Some(_) => {}
                None => {
                    order.push(*b);
                    stack.pop();
                }
            }
        }
    }
    order.extend((0..n).filter(|&b| !seen[b]));
    order
}

/// [`liveness`], where a `Drop` of `place` keeps it live only when
/// `drop_uses(place)`. The sets are bits over the locals, and each block's
/// effect is folded once into `in = gen | (out & !kill)`, so a round of the
/// fixpoint costs a few words a block: searching lists made a large body
/// (mini_inflect's `new_tab` as an auto-par coroutine, 26,420 blocks over
/// 1,700 locals) take most of a minute (B-2026-10-28-6).
fn liveness_with(body: &Body, drop_uses: &dyn Fn(&Place) -> bool) -> Vec<Vec<Local>> {
    let n = body.blocks.len();
    // Each block's uses and defs, from its end back to its start.
    let mut nl = body.locals.len();
    let steps: Vec<Vec<(Vec<Local>, Vec<Local>)>> = body
        .blocks
        .iter()
        .map(|block| {
            let mut s = Vec::with_capacity(block.statements.len() + 1);
            s.push(terminator_use_def_with(&block.terminator.kind, drop_uses));
            for st in block.statements.iter().rev() {
                s.push(statement_use_def(&st.kind));
            }
            for (u, d) in &s {
                for l in u.iter().chain(d) {
                    nl = nl.max(l.index() + 1);
                }
            }
            s
        })
        .collect();
    let w = nl.div_ceil(64).max(1);
    let bit = |l: &Local| (l.index() / 64, 1u64 << (l.index() % 64));
    let mut gen = vec![0u64; n * w];
    let mut kill = vec![0u64; n * w];
    for (bi, s) in steps.iter().enumerate() {
        let (g, k) = (&mut gen[bi * w..][..w], &mut kill[bi * w..][..w]);
        // Walking back: `live = (live - defs) | uses`.
        for (u, d) in s {
            for l in d {
                let (i, m) = bit(l);
                g[i] &= !m;
                k[i] |= m;
            }
            for l in u {
                let (i, m) = bit(l);
                g[i] |= m;
            }
        }
    }
    let mut live_in = vec![0u64; n * w];
    let mut live_out = vec![0u64; n * w];
    let mut out = vec![0u64; w];
    // Successors before predecessors: a postorder from the entry, then
    // any block it does not reach. In reverse block order that body took
    // 491 rounds to settle; in postorder, 3.
    let order = postorder(body);
    let mut changed = true;
    while changed {
        changed = false;
        for &bi in &order {
            out.fill(0);
            for s in body.blocks[bi].terminator.kind.successors() {
                let from = &live_in[s.index() * w..][..w];
                for (o, x) in out.iter_mut().zip(from) {
                    *o |= x;
                }
            }
            for i in 0..w {
                let v = gen[bi * w + i] | (out[i] & !kill[bi * w + i]);
                if v != live_in[bi * w + i] {
                    live_in[bi * w + i] = v;
                    changed = true;
                }
                live_out[bi * w + i] = out[i];
            }
        }
    }
    live_out
        .chunks(w)
        .map(|words| {
            let mut ls = Vec::new();
            for (i, &x) in words.iter().enumerate() {
                let mut x = x;
                while x != 0 {
                    ls.push(Local((i * 64 + x.trailing_zeros() as usize) as u32));
                    x &= x - 1;
                }
            }
            ls
        })
        .collect()
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
        Rvalue::NullaryOp(..) => {}
    }
}

fn operand_access(o: &Operand) -> Option<(Place, Access)> {
    match o {
        Operand::Copy(p) => Some((p.clone(), Access::Read)),
        Operand::Move(p) => Some((p.clone(), Access::Move)),
        Operand::Const(_) => None,
    }
}

pub(super) fn statement_accesses(s: &StatementKind) -> Vec<(Place, Access)> {
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
                Rvalue::NullaryOp(..) => Vec::new(),
            };
            v.push((dest.clone(), Access::Write));
            v
        }
        StatementKind::SetDiscriminant(p, _) => vec![(p.clone(), Access::Write)],
        StatementKind::StorageDead(l) => vec![(Place::local(*l), Access::StorageDead)],
        StatementKind::StorageLive(_)
        | StatementKind::BorrowFlag(_)
        | StatementKind::FreeBox(_)
        | StatementKind::Nop => Vec::new(),
    }
}

pub(super) fn terminator_accesses(t: &TerminatorKind) -> Vec<(Place, Access)> {
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
    if loan.through_shared_ref && place.projection.is_empty() {
        return false;
    }
    // A reborrow through a `mut ref` (`&mut (*x)`) borrows the pointee, so
    // ending the reference's storage, or dropping it, leaves it valid:
    // `Some(x) => x` returning a binding out of a `mut ref` scrutinee.
    if loan.through_mut_ref
        && place.projection.is_empty()
        && matches!(access, Access::StorageDead | Access::Drop)
    {
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
    fn receivers(m: &crate::mir::MirModule) -> impl Fn(&InstanceId) -> ResultBorrows + '_ {
        |callee| {
            let recv = m.bodies.iter().any(|b| {
                b.instance.name == callee.name
                    && b.instance.name.contains('.')
                    && b.arg_count > 0
                    && matches!(
                        m.tys.kind(b.locals[1].ty),
                        crate::mir::TyKind::Ref(_) | crate::mir::TyKind::MutRef(_)
                    )
            });
            if recv {
                ResultBorrows::Receiver
            } else {
                ResultBorrows::Args
            }
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

    /// A reborrow through a `mut ref` outlives the reference's storage (a
    /// binding returned out of a `mut ref` match), but not its pointee's.
    #[test]
    fn mir_borrowck_mut_reborrow_outlives_its_reference() {
        let locals =
            "    let mut _1: i64;\n    let mut _2: mut ref i64;\n    let mut _3: mut ref i64;\n";
        let ok = main_with(
            locals,
            "        StorageLive(_1);
        _1 = const 1_i64;
        StorageLive(_2);
        _2 = &mut _1;
        _3 = &mut (*_2);
        StorageDead(_2);
        (*_3) = const 2_i64;
",
        );
        assert!(ok.is_empty(), "{ok:?}");
        let bad = main_with(
            locals,
            "        StorageLive(_1);
        _1 = const 1_i64;
        _2 = &mut _1;
        _3 = &mut (*_2);
        StorageDead(_1);
        (*_3) = const 2_i64;
",
        );
        one(&bad, "bb0[4]: end of storage of _1");
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
        let errs = check_borrows(main, &m.tys, &|_| ResultBorrows::Args).unwrap_err();
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

    /// `TaskGroup` declares only an id but holds its tasks' closures, and
    /// `spawn` stores them through `ref self`, so a spawn keeps the
    /// closure's borrows live while the group lives (§9.5).
    #[test]
    fn mir_borrowck_task_group_holds_spawned_borrows() {
        let src = |tail: &str| {
            format!(
                "
struct TaskGroup {{ id: i64 }}

fn TaskGroup.spawn(_1: ref TaskGroup, _2: closure#1(ref i64)) -> () {{
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
    let _5: ref TaskGroup;
    let _6: ();
    let _7: i64;
    bb0: {{
        _1 = const 1_i64;
        _2 = TaskGroup {{ const 0_i64 }};
        _3 = &_1;
        _4 = closure#1(ref i64) [move _3];
        _5 = &_2;
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
            "bb1[0]: write of _1 while it is borrowed (borrow at bb0[2]) [E0516]",
        );
        assert_eq!(errors(&src("")), Vec::<String>::new());
    }

    /// §6.2: a `mut` field of a shared value borrowed through one handle
    /// local cannot be written through that local while the borrow is
    /// live; through a second handle to the same object the places differ,
    /// so the run-time flag decides. A `Copy` field read as a value holds
    /// no borrow.
    #[test]
    fn mir_borrowck_shared_field_borrows() {
        let src = |stmts: &str| {
            format!(
                "
struct Bag {{ n: i64, items: Vec[i64] }}

fn main() -> () {{
    let mut _0: ();
    let _1: shared Bag;
    let _2: shared Bag;
    let _3: ref Vec[i64];
    let _4: mut ref Vec[i64];
    let _5: i64;
    let _6: ref Vec[i64];
    let _7: Vec[i64];
    bb0: {{
        _7 = Vec[i64].new() -> bb1;
    }}
    bb1: {{
        _1 = shared Bag {{ const 0_i64, move _7 }};
        _2 = retain(_1);
{stmts}        _0 = const ();
        return;
    }}
}}
"
            )
        };
        one(
            &errors(&src("        _3 = &_1.1;
        _4 = &mut _1.1;
        _6 = copy _3;
")),
            "bb1[3]: mutable borrow of _1.1 while it is borrowed (borrow at bb1[2]) [E0516]",
        );
        let other_handle = errors(&src("        _3 = &_1.1;
        _4 = &mut _2.1;
        _6 = copy _3;
"));
        assert_eq!(other_handle, Vec::<String>::new());
        let copy_field = errors(&src("        _5 = copy _1.0;
        _1.0 = const 7_i64;
        _5 = copy _5;
"));
        assert_eq!(copy_field, Vec::<String>::new());
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
        // D5, as `karac check` reads the pin: a bare parameter borrows, of a
        // function and of a function type alike.
        if crate::d5_params::on() {
            crate::d5_params::borrow_bare_params(&mut program);
        }
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
            src,
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
            if methods
                .iter()
                .find(|(n, _)| *n == c.name)
                .is_none_or(|(_, r)| *r)
            {
                ResultBorrows::Receiver
            } else {
                ResultBorrows::Args
            }
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
            ("err_escaping_capture_reused", true, "move check main:"),
            ("err_store_nonescaping_param", true, "borrow check keep{"),
            ("err_taskgroup_origin_declared_after", false, "borrow check"),
            ("err_taskgroup_write_while_borrowed", false, "borrow check"),
        ];
        // Rules checked before MIR: `par` effects, Drop-type moves, §6.3 and
        // the closure kinds (§9.6).
        let not_mir = [
            "err_par_conflict",
            "err_partial_move_drop_type",
            "err_enum_payload_drop_type_into_constructor",
            "err_fn_value_shared_crosses_task",
            "err_fn_kind_mismatch",
            // `std.log`'s fields and its attribute (B-2026-10-27-20).
            "err_log_reserved_label",
            "err_log_duplicate_label",
            "err_log_unlabeled_field",
            "err_log_field_not_serialize",
            "err_observability_attr",
            // A generic item's own parameter is rigid (B-2026-10-27-19).
            "err_rigid_type_param",
        ];
        // Pins that call Kāra library functions, which `check_built` does
        // not append (a `select` desugars to calls into sync.kara;
        // `TaskGroup.isolating` is declared in isolating.kara; a `weak`
        // local reads as the library's `Option`; `collect` is iter.kara's).
        // The build in `built_core_pins_match_their_expected_output` runs
        // them with the library, through the same move and borrow checks.
        let needs_library = [
            "select_source_order",
            "isolating_panic_ends_its_task",
            "ok_weak_local_reads_as_option",
            "ok_ref_handle_collects_as_the_handle",
            "ok_weak_slot_upgrade",
            "ok_closure_reads_method_args_handles_and_matches",
            // `into_iter`, `iter` and `vec!` are the library's.
            "for_into_iter_consumes",
            "ok_ref_copied_from_a_tuple_element",
            "ok_error_anyerror",
            // `std.log` is spliced from the library with `std.json`.
            "log_custom_backend",
            "log_filtering",
            "log_json_format",
            "log_text_format",
            "log_transparent",
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/core");
        let mut pins: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        pins.sort();
        let (mut ok_checked, mut bad) = (0, Vec::new());
        for dir in pins {
            let pin = dir.file_name().unwrap().to_str().unwrap().to_string();
            if not_mir.contains(&pin.as_str()) || needs_library.contains(&pin.as_str()) {
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
            // A pin another thread adds before it reaches this table only
            // has to be refused by something, or not build yet.
            let Some((_, must_build, want)) = err_pins.iter().find(|(p, ..)| *p == pin) else {
                if matches!(&verdict, Ok(refused) if refused.is_empty()) {
                    bad.push(format!("{pin}: accepted (and not in this test's table)"));
                }
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

    /// B-2026-10-13-29: a `Str` view of a `String` borrows it, so writing
    /// the `String` while the view, or a view cut from it, is live is
    /// refused; a view cut from a view held in a field leaves that field
    /// free to be written.
    #[test]
    fn mir_borrowck_a_str_view_borrows_its_string() {
        let run = |body: &str| {
            crate::mir::lower::tests::run_source(&format!("fn main() {{\n{body}\n}}\n"))
        };
        let grow = "    owned.push_str(\"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\");\n";
        for view in [
            "let v: Str = owned.as_str();",
            "let v = owned.as_str().slice(0, 1);",
        ] {
            let src = format!(
                "    let mut owned = String.from(\"ab\");\n    {view}\n{grow}    println(v);"
            );
            let err = run(&src).expect_err(view);
            assert!(err.contains("[E0516]"), "{view}: {err}");
            // Its last use before the write ends the borrow.
            let want = if view.contains("slice") {
                "a\n42\n"
            } else {
                "ab\n42\n"
            };
            let ok = format!("    let mut owned = String.from(\"ab\");\n    {view}\n    println(v);\n{grow}    println(owned.len());");
            assert_eq!(run(&ok).unwrap(), (want.into(), Some(0)));
        }
        let field = "struct It { rest: Str }\n\
impl It { fn step(mut ref self) -> Str { let head = self.rest.slice(0, 1); self.rest = self.rest.slice(1, self.rest.len()); head } }\n\
fn main() {\n    let s = String.from(\"xyz\");\n    let mut it = It { rest: s.as_str() };\n    println(it.step());\n    println(it.step());\n}\n";
        assert_eq!(
            crate::mir::lower::tests::run_source(field).unwrap(),
            ("x\ny\n".into(), Some(0))
        );
    }

    /// A `Str` copied out of a `Vec[Str]` views what the elements view,
    /// so it outlives the `Vec` (B-2026-10-18-50); a reference into the
    /// `Vec`, a view of a `String` in a `Vec`, and a view of a `String`
    /// that dies are still refused.
    #[test]
    fn mir_borrowck_a_str_copied_out_of_a_vec_outlives_it() {
        let run = crate::mir::lower::tests::run_source;
        let ok = r#"fn first(s: Str) -> Str {
    let mut w = s;
    if s.contains(",") {
        let parts: Vec[Str] = s.split(",").collect();
        w = parts[0];
    }
    w
}
fn main() {
    println(first("ab,cd"));
    println(first("xy"));
    let s2 = "ab,cd";
    let parts2: Vec[Str] = s2.split(",").collect();
    let x: Str = parts2[1];
    let moved = parts2;
    println(x);
    println(moved.len());
}
"#;
        assert_eq!(run(ok).unwrap(), ("ab\nxy\ncd\n2\n".into(), Some(0)));
        for bad in [
            r#"fn main() {
    let v: Vec[String] = vec!["ab".to_string(), "cd".to_string()];
    let t: Str = v[0].as_str();
    let moved = v;
    println(t);
    println(moved.len());
}
"#,
            r#"fn pick(v: ref Vec[Str]) -> ref Str { v.get(0).unwrap() }
fn main() {
    let s = "ab,cd";
    let parts: Vec[Str] = s.split(",").collect();
    let r = pick(parts);
    let moved = parts;
    println(r);
    println(moved.len());
}
"#,
            r#"fn main() {
    let owned = "ab,cd".to_string();
    let mut w: Str = "";
    {
        let parts: Vec[Str] = owned.as_str().split(",").collect();
        w = parts[1];
    }
    let gone = owned;
    println(w);
    println(gone);
}
"#,
        ] {
            let err = run(bad).expect_err(bad);
            assert!(err.contains("[E0516]"), "{bad}: {err}");
        }
    }
    /// B-2026-10-19-6: dropping a `Vec[Str]` reads none of its views, so a
    /// `String` split into one can be reassigned while it is still in scope.
    /// A use of the views after the write, and a drop that runs user code
    /// with a view in hand, are still refused.
    #[test]
    fn mir_borrowck_a_dead_vec_of_views_does_not_pin_its_string() {
        let run = crate::mir::lower::tests::run_source;
        let ok = r#"fn main() {
    let mut s = "a-b".to_string();
    let parts: Vec[Str] = s.split("-").collect();
    let n = parts.len();
    s = "c".to_string();
    println(f"{n} {s}");
    let mut t = "x,y,z".to_string();
    let pairs: Vec[(Str, i64)] = t.split(",").map(|p| (p, 1)).collect();
    let m = pairs.len();
    t = "w".to_string();
    println(f"{m} {t}");
}
"#;
        assert_eq!(run(ok).unwrap(), ("2 c\n3 w\n".into(), Some(0)));
        for bad in [
            r#"fn main() {
    let mut s = "a-b".to_string();
    let parts: Vec[Str] = s.split("-").collect();
    s = "c".to_string();
    println(parts[0]);
    println(s);
}
"#,
            r#"struct Holder {
    v: Str,
}
impl Drop for Holder {
    fn drop(mut ref self) {
        println(self.v);
    }
}
fn main() {
    let mut s = "a-b".to_string();
    let h = Holder { v: s.as_str() };
    s = "c".to_string();
    println(s);
}
"#,
        ] {
            let err = run(bad).expect_err(bad);
            assert!(err.contains("[E0516]"), "{bad}: {err}");
        }
    }
}
