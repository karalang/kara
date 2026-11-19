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
use crate::ids::DefId;
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

    /// Removes `other`'s loans.
    fn remove_all(&mut self, other: &LoanSet) {
        self.0.retain(|l| other.0.binary_search(l).is_err());
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
    /// What every argument holds, except that each argument whose position
    /// is set in the mask, a view parameter, gives what its value views,
    /// not its place: `fn parse_until(c: mut ref Cursor, stop: char) ->
    /// Str` returns a view of the text `c.src` views (core semantics §5.4,
    /// "Results borrow what a view parameter views"; B-2026-11-13-79).
    ViewArgs(u64),
    /// What every argument after the receiver holds: a task group's
    /// `spawn` hands back a handle holding what the task's closure holds,
    /// for `join` to pass on, not a borrow of the group, which the
    /// task-group scope already bounds (B-2026-11-19-95).
    ArgsAfterReceiver,
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
    /// The binding behind the borrow, when it holds a `mut ref` it only
    /// reads through: a pattern binding of a part of a `mut ref` scrutinee,
    /// which `ref` turns into a shared borrow, so `karac fix` writes `ref`
    /// before it (B-2026-11-17-152).
    pub holder_read_only: Option<String>,
    /// For an E0509 that stores a view where none may go: the program's
    /// struct field whose declared type is the view (`E.kind: Str`), as
    /// (struct, field, declared type), so the message can name it and
    /// `karac fix` can give the field its owned type (B-2026-11-18-76).
    pub view_field: Option<(String, String, String)>,
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
            holder_read_only: None,
            temporary: None,
            view_field: None,
        }
    }
}

/// [`check_borrows`]'s errors, each with its places.
pub fn borrow_errors(
    body: &Body,
    tys: &TyInterner,
    has_receiver: &dyn Fn(&InstanceId) -> ResultBorrows,
) -> Vec<BorrowError> {
    borrow_errors_with(body, tys, has_receiver, &|_| None)
}

/// A closure's parameter types (its environment left out) and result
/// type, by the closure's `DefId`: `None` when the program has no single
/// signature for it, which the check reads as a closure that may hand
/// back anything and fit any function slot.
pub type ClosureSig<'a> = dyn Fn(DefId) -> Option<(Vec<Ty>, Ty)> + 'a;

/// [`borrow_errors`], told each closure's signature ([`closure_sig`]),
/// which the §5.3 rule for function arguments reads. To [`borrow_errors`]
/// no signature is known.
pub fn borrow_errors_with(
    body: &Body,
    tys: &TyInterner,
    has_receiver: &dyn Fn(&InstanceId) -> ResultBorrows,
    closure_sig: &ClosureSig<'_>,
) -> Vec<BorrowError> {
    borrow_errors_in(body, tys, has_receiver, closure_sig, &|_| false)
}

/// The functions whose result carries a view one of their parameters
/// holds, by name: [`view_results`]. A call of one hands back a value
/// that holds what its arguments view, as a closure made in the caller
/// would.
pub type ViewResults<'a> = dyn Fn(&str) -> bool + 'a;

/// [`borrow_errors_with`], told which functions hand back a value holding
/// a view of what their arguments view ([`view_results`]): a closure such
/// a call returns, passed on to be stored where no borrow is kept, is
/// refused as one made in the body is (B-2026-11-17-92).
pub fn borrow_errors_in(
    body: &Body,
    tys: &TyInterner,
    has_receiver: &dyn Fn(&InstanceId) -> ResultBorrows,
    closure_sig: &ClosureSig<'_>,
    view_results: &ViewResults<'_>,
) -> Vec<BorrowError> {
    let mut errs = read_only_errors(body, tys);
    errs.extend(view_into_shared_errors(body, tys));
    let loans = collect_loans(body, tys);
    errs.extend(view_param_escape_errors(
        body,
        tys,
        &loans,
        has_receiver,
        closure_sig,
        view_results,
    ));
    if loans.is_empty() {
        return errs;
    }
    let entry = Origins::default();
    let cx = Cx {
        body,
        tys,
        loans: &loans,
        has_receiver,
        closure_sig,
        writes_slices: writes_slices(body, tys, &loans),
        pred_captures: predicate_captures(body, tys, &loans),
        made_here: closures_made_here(body, view_results),
    };
    let origins = origins_dataflow(&cx, entry);
    let scopes = scope_dataflow(body, &loans);
    let by_site: std::collections::HashMap<(usize, usize), usize> =
        loans.iter().enumerate().map(|(li, l)| (l.at, li)).collect();
    let drop_uses = |p: &Place| drop_reads_borrows(body, tys, p);
    let live_out = liveness_with(body, &drop_uses);
    let view = body.arg_count > 0
        && matches!(
            has_receiver(&body.instance),
            ResultBorrows::ReceiverValue | ResultBorrows::ReceiverPointeeValue
        );
    // The view parameters whose value the result may view (§5.4).
    let view_params = match has_receiver(&body.instance) {
        ResultBorrows::ViewArgs(mask) => mask,
        _ => 0,
    };
    let view_param =
        |l: usize| (1..=body.arg_count.min(64)).contains(&l) && view_params >> (l - 1) & 1 != 0;

    for (bi, block) in body.blocks.iter().enumerate() {
        let Some(mut o) = origins[bi].clone() else {
            continue; // unreachable
        };
        let mut in_scope = scopes[bi].clone().unwrap_or_default();
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
                    if loan.at == at || !in_scope.0.contains(&(li as u32)) {
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
                        // The result holds the borrow of a local this
                        // function drops on the way out: that is §5.4's
                        // refusal (the result outlives what it borrows),
                        // named as such rather than as a drop conflict.
                        if matches!(access, Access::Drop | Access::StorageDead)
                            && o.get(Local::RETURN_PLACE).iter().any(|h| h == li)
                            && !loan.place.projection.contains(&ProjElem::Deref)
                        {
                            let text = format!(
                                "{}: the result borrows {}, which this function owns (borrow at {}) [E0509]",
                                site(body, at, Some(loan.at)),
                                show_place(body, tys, &loan.place),
                                borrow_site(body, loan.at),
                            );
                            errs.push(BorrowError {
                                place: source_place(body, tys, &loan.place),
                                loan_at: source_at(body, loan.at),
                                ..BorrowError::new(text, "E0509", site_at(body, at, Some(loan.at)))
                            });
                            continue;
                        }
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
                            .max_by_key(|(l, _)| *l);
                        // The binding the fix would change: the named
                        // holder, or else a binding holding a reborrow
                        // through the holding temporary (`Some(d)` over a
                        // scrutinee reborrowed into one).
                        let holder_read_only = holder
                            .clone()
                            .or_else(|| {
                                let mut held = vec![li];
                                let mut holders: Vec<Local> = Vec::new();
                                while let Some(k) = held.pop() {
                                    for l in live.iter() {
                                        if !holders.contains(l) && o.get(*l).iter().any(|h| h == k)
                                        {
                                            holders.push(*l);
                                            held.extend((0..loans.len()).filter(|j| {
                                                loans[*j].place.local == *l
                                                    && loans[*j].place.projection.first()
                                                        == Some(&ProjElem::Deref)
                                            }));
                                        }
                                    }
                                }
                                // A temporary holding the loan was reborrowed
                                // from the binding (`_5 = &(*v)` for a call
                                // argument after `v`'s last use): walk back
                                // to the binding.
                                let mut i = 0;
                                while i < holders.len() {
                                    let h = holders[i];
                                    i += 1;
                                    if local_name(body, h).is_some() {
                                        continue;
                                    }
                                    for st in body.blocks.iter().flat_map(|b| &b.statements) {
                                        if let StatementKind::Assign(d, Rvalue::Ref(_, p)) =
                                            &st.kind
                                        {
                                            if d.local == h
                                                && d.projection.is_empty()
                                                && p.projection.first() == Some(&ProjElem::Deref)
                                                && !holders.contains(&p.local)
                                            {
                                                holders.push(p.local);
                                            }
                                        }
                                    }
                                }
                                holders
                                    .into_iter()
                                    .filter_map(|l| local_name(body, l).map(|n| (l, n)))
                                    .max_by_key(|(l, _)| *l)
                            })
                            .filter(|(l, _)| reads_only_through(body, tys, *l))
                            .map(|(_, n)| n);
                        errs.push(BorrowError {
                            access: access_name(access),
                            place: source_place(body, tys, &place),
                            loan_at: source_at(body, loan.at),
                            holder: holder.map(|(_, n)| n),
                            holder_read_only,
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
            } else if let TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } = &block.terminator.kind
            {
                cx.unkept_call_loans(&o, func, args, destination)
            } else {
                LoanSet::default()
            };
            for li in unkept.iter() {
                let loan = &loans[li];
                let text = format!(
                        "{}: a view of {} is stored in an escaping closure, a `dyn` value, a `shared` value or a channel, where its borrow is not kept (borrow at {}); store an owned copy instead (core semantics §5.7, §9.3) [E0509]",
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
                scope_transfer(
                    &mut in_scope,
                    &loans,
                    &by_site,
                    &block.statements[si].kind,
                    at,
                );
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
                    } else if derefs == 1 && view_param(loan.place.local.index()) {
                        "the parameter owns; a view result may borrow only what its `ref`, `Str` or `Slice` fields view"
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

/// A view a parameter holds, stored where no borrow is kept (§5.7, §9.3):
/// in an escaping closure that is not the result itself (one in a field,
/// an enum payload or a collection), a `dyn` value, a `shared`
/// value or a channel. The body takes no loan of what a parameter views,
/// so the by-loan check finds nothing to name; here each view parameter
/// stands for one loan taken before the body runs, and only the stores are
/// checked (B-2026-11-17-23).
fn view_param_escape_errors(
    body: &Body,
    tys: &TyInterner,
    body_loans: &[Loan],
    has_receiver: &dyn Fn(&InstanceId) -> ResultBorrows,
    closure_sig: &ClosureSig<'_>,
    view_results: &ViewResults<'_>,
) -> Vec<BorrowError> {
    let (params, loans) = view_param_loans(body, tys);
    if params.is_empty() {
        return Vec::new();
    }
    let cx = Cx {
        body,
        tys,
        loans: &loans,
        has_receiver,
        closure_sig,
        writes_slices: writes_slices(body, tys, body_loans),
        pred_captures: LoanSet::default(),
        made_here: closures_made_here(body, view_results),
    };
    let mut entry = Origins::default();
    for (i, &p) in params.iter().enumerate() {
        entry.insert(p, i);
    }
    let origins = origins_dataflow(&cx, entry);
    let mut errs = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        let Some(mut o) = origins[bi].clone() else {
            continue;
        };
        let n = block.statements.len();
        for si in 0..=n {
            let unkept = if si < n {
                cx.unkept_loans(&o, &block.statements[si].kind)
            } else if let TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } = &block.terminator.kind
            {
                cx.unkept_call_loans(&o, func, args, destination)
            } else {
                LoanSet::default()
            };
            for li in unkept.iter() {
                let p = Place::local(params[li]);
                let holder = local_name(body, params[li])
                    .filter(|n| !n.is_empty() && !n.starts_with('_'))
                    .map_or_else(
                        || "a captured value".to_string(),
                        |n| format!("the parameter `{n}`"),
                    );
                let text = format!(
                    "{}: a view {} holds is stored in an escaping closure, a `dyn` value, a `shared` value or a channel, where its borrow is not kept; store an owned copy instead, or make the closure the result itself (core semantics §5.7, §9.3) [E0509]",
                    site(body, (bi, si), None),
                    holder,
                );
                errs.push(BorrowError {
                    place: source_place(body, tys, &p),
                    ..BorrowError::new(text, "E0509", site_at(body, (bi, si), None))
                });
            }
            if si < n {
                cx.transfer(&mut o, &block.statements[si].kind, (bi, si));
            }
        }
    }
    errs.dedup_by(|a, b| a.text == b.text);
    errs
}

/// The parameters of `body` that hold a view, and for each one loan
/// standing for what it views, taken before the body runs: reached
/// through a reference, as what a caller lends is, so the body can
/// neither end it nor store into it.
fn view_param_loans(body: &Body, tys: &TyInterner) -> (Vec<Local>, Vec<Loan>) {
    let params: Vec<Local> = (1..=body.arg_count)
        .map(|i| Local(i as u32))
        .filter(|l| holds_view(tys, body.local(*l).ty))
        .collect();
    let loans = params
        .iter()
        .enumerate()
        .map(|(i, &p)| Loan {
            kind: BorrowKind::Shared,
            place: Place {
                local: p,
                projection: vec![ProjElem::Deref],
            },
            through_shared_ref: true,
            through_mut_ref: false,
            at: (usize::MAX, i),
        })
        .collect();
    (params, loans)
}

/// The bodies whose result holds a function value or a `dyn` that may
/// carry a view one of their parameters holds: at some `return`, the
/// result's origins include a parameter's ([`view_param_loans`]). A
/// closure made in the body and returned, capturing a view parameter, is
/// one (`mk(v: Slice[D]) -> Fn() -> i64 { || v[0].n }`); a function
/// choosing among closures that capture nothing is not (`op(name) ->
/// Fn(..)`). A call in the body counts as §5.4 has it, holding what its
/// arguments hold, so a factory that returns another's result is one too.
pub fn view_results<'b>(
    bodies: impl IntoIterator<Item = &'b Body>,
    tys: &TyInterner,
    has_receiver: &dyn Fn(&InstanceId) -> ResultBorrows,
    closure_sig: &ClosureSig<'_>,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for body in bodies {
        let mut erased = Vec::new();
        erased_parts(tys, body.return_ty(), &mut erased, false);
        if erased.is_empty() {
            continue;
        }
        let (params, loans) = view_param_loans(body, tys);
        if params.is_empty() {
            continue;
        }
        let body_loans = collect_loans(body, tys);
        let cx = Cx {
            body,
            tys,
            loans: &loans,
            has_receiver,
            closure_sig,
            writes_slices: writes_slices(body, tys, &body_loans),
            pred_captures: LoanSet::default(),
            made_here: std::collections::HashSet::new(),
        };
        let mut entry = Origins::default();
        for (i, &p) in params.iter().enumerate() {
            entry.insert(p, i);
        }
        let origins = origins_dataflow(&cx, entry);
        let returns_view = body.blocks.iter().enumerate().any(|(bi, block)| {
            if !matches!(block.terminator.kind, TerminatorKind::Return) {
                return false;
            }
            let Some(mut o) = origins[bi].clone() else {
                return false;
            };
            for (si, s) in block.statements.iter().enumerate() {
                cx.transfer(&mut o, &s.kind, (bi, si));
            }
            !o.get(Local::RETURN_PLACE).is_empty()
        });
        if returns_view {
            out.insert(body.instance.name.clone());
        }
    }
    out
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
        closure_sig: &|_| None,
        writes_slices: writes_slices(body, tys, &loans),
        pred_captures: predicate_captures(body, tys, &loans),
        made_here: closures_made_here(body, &|_| false),
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
            // The `mut ref` provider a `Fn` value that writes it carries is
            // written through its environment lent shared (B-2026-10-30-3).
            TyKind::MutRef(_)
                if place.projection[i] == ProjElem::Deref
                    && i > 0
                    && matches!(place.projection[i - 1], ProjElem::Field(..))
                    && place_ty(
                        body,
                        tys,
                        &Place {
                            local: place.local,
                            projection: place.projection[..i - 1].to_vec(),
                        },
                    )
                    .is_ok_and(|t| {
                        matches!(tys.kind(t.ty), TyKind::Closure(def, _)
                            if super::providers::is_carried(def))
                    }) =>
            {
                read_only = false
            }
            _ => {}
        }
    }
    read_only
}

/// Where a view may not go that `place` names (core semantics §5.7): a
/// field of a `shared` value (`s.items`, `(*s).items` through a `ref`
/// handle), or the value a `Mutex` holds, reached through its lock's guard
/// (`*g.value`), which a `ref` to the mutex writes, so no borrow of the
/// mutex keeps what is stored there (B-2026-11-17-132).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NoViewPlace {
    SharedField,
    MutexValue,
}

impl NoViewPlace {
    fn describe(self) -> &'static str {
        match self {
            NoViewPlace::SharedField => "a field of a `shared` value",
            NoViewPlace::MutexValue => "the value a `Mutex` holds",
        }
    }
}

fn no_view_place(body: &Body, tys: &TyInterner, place: &Place) -> Option<NoViewPlace> {
    (0..place.projection.len()).find_map(|i| {
        if !matches!(place.projection[i], ProjElem::Field(..)) {
            return None;
        }
        let prefix = Place {
            local: place.local,
            projection: place.projection[..i].to_vec(),
        };
        let pt = place_ty(body, tys, &prefix).ok()?;
        match tys.kind(pt.ty) {
            TyKind::Shared(_) => Some(NoViewPlace::SharedField),
            TyKind::Adt(a) if tys.adt(a).library_name() == Some("MutexGuard") => {
                Some(NoViewPlace::MutexValue)
            }
            _ => None,
        }
    })
}

/// Whether a value of type `ty` is or holds a view: a `ref`, a `Str`, a
/// slice, or a collection, tuple or struct with one in it. A `shared`
/// handle is not a view, and a function value's captures are the by-loan
/// check's (§9.3).
pub(crate) fn holds_view(tys: &TyInterner, ty: Ty) -> bool {
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

/// The program's own struct or enum field, inside a value of type `ty`,
/// whose declared type is itself a view (`Str`, a slice or a `ref`): the
/// part that makes `ty` hold one. Returns (type, field, declared type),
/// the field as `Variant.field` in an enum (B-2026-11-18-76).
fn view_field(tys: &TyInterner, ty: Ty) -> Option<(String, String, String)> {
    fn direct(tys: &TyInterner, t: Ty) -> bool {
        matches!(
            tys.kind(t),
            TyKind::Ref(_) | TyKind::MutRef(_) | TyKind::Str | TyKind::Slice(_)
        )
    }
    fn go(tys: &TyInterner, ty: Ty, seen: &mut Vec<Ty>) -> Option<(String, String, String)> {
        if seen.contains(&ty) {
            return None;
        }
        seen.push(ty);
        match tys.kind(ty) {
            TyKind::Tuple(ts) => ts.iter().find_map(|t| go(tys, *t, seen)),
            TyKind::Array(t, _)
            | TyKind::Intrinsic(
                IntrinsicTy::Vec(t)
                | IntrinsicTy::Set(t)
                | IntrinsicTy::VecDeque(t)
                | IntrinsicTy::SortedSet(t),
            ) => go(tys, t, seen),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                go(tys, k, seen).or_else(|| go(tys, v, seen))
            }
            TyKind::Adt(a) => {
                let adt = tys.adt(a);
                for (vi, var) in adt.variants.iter().enumerate() {
                    for (fi, (name, _)) in var.fields.iter().enumerate() {
                        let Some(t) = tys.field_ty(ty, adt.is_enum.then_some(vi as u32), fi as u32)
                        else {
                            continue;
                        };
                        if adt.user && direct(tys, t) {
                            let field = if adt.is_enum {
                                format!("{}.{name}", var.name)
                            } else {
                                name.clone()
                            };
                            return Some((adt.name.clone(), field, source_ty(tys, t)));
                        }
                        if let Some(found) = go(tys, t, seen) {
                            return Some(found);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }
    go(tys, ty, &mut Vec::new())
}

/// The place where a view may not go ([`no_view_place`]) statement `s`
/// stores a view-typed value in, with that value's type.
fn stored_view(
    body: &Body,
    tys: &TyInterner,
    s: &StatementKind,
) -> Option<(Place, Ty, NoViewPlace)> {
    let StatementKind::Assign(dest, rv) = s else {
        return None;
    };
    let kind = no_view_place(body, tys, dest)?;
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
    holds_view(tys, t).then(|| (dest.clone(), t, kind))
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
    let subject = |t: Ty, inside: &str| {
        if body.instance.args.is_empty() {
            format!("a view (`{}`{inside}) is stored", source_ty(tys, t))
        } else {
            format!(
                "the instance {} stores a view (`{}`{inside})",
                body.instance.name,
                source_ty(tys, t)
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
                    let field = lent(p.local)?;
                    let kind = no_view_place(body, tys, &field)?;
                    args.iter().enumerate().find_map(|(j, b)| {
                        let q = b.place().filter(|_| j != i)?;
                        let t = place_ty(body, tys, q).ok()?.ty;
                        (holds_view(tys, t) && contains_ty(tys, holder, t))
                            .then(|| (field.clone(), t, kind))
                    })
                })
            } else {
                None
            };
            let Some((field, t, kind)) = stored else {
                continue;
            };
            // A guard the program never names (`m.lock().value.push(r)`)
            // leaves the place unnamed: the mutex's value is enough.
            let place = match (source_place(body, tys, &field), kind) {
                (Some(p), _) => format!("`{p}`, {}", kind.describe()),
                (None, NoViewPlace::MutexValue) => kind.describe().to_string(),
                (None, _) => format!("{}, {}", show_place(body, tys, &field), kind.describe()),
            };
            // The struct field that makes the value a view, by name, when
            // the value is not that view itself (B-2026-11-18-76).
            let vf = view_field(tys, t);
            let inside = match &vf {
                Some((adt, f, fty)) => format!(", through its field `{adt}.{f}: {fty}`"),
                None => String::new(),
            };
            let remedy = match &vf {
                Some((adt, f, fty)) if fty == "Str" => {
                    format!("give `{adt}.{f}` an owned type (`String`)")
                }
                _ => "store an owned copy instead".to_string(),
            };
            let text = format!(
                "{}: {} in {place}, where a view may not go; {remedy} (core semantics §5.7, §5.8) [E0509]",
                site(body, (bi, si), None),
                subject(t, &inside),
            );
            errs.push(BorrowError {
                place: source_place(body, tys, &field),
                view_field: vf,
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
/// `t` as the program spells it: MIR's text form, with its string view
/// `str` written `Str` (B-2026-11-13-77).
fn source_ty(tys: &TyInterner, t: Ty) -> String {
    let text = tys.display(t);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(i) = rest.find("str") {
        let before = rest[..i].chars().next_back();
        let after = rest[i + 3..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(&rest[..i]);
        out.push_str(if word(before) || word(after) {
            "str"
        } else {
            "Str"
        });
        rest = &rest[i + 3..];
    }
    out.push_str(rest);
    out
}

/// Whether `l` is a user binding holding a `mut ref` that the body only
/// reads through: it is never written through, reborrowed mutably, or
/// passed on whole (where the `mut ref` itself could reach a write), so a
/// shared borrow serves it as well (B-2026-11-17-152).
fn reads_only_through(body: &Body, tys: &TyInterner, l: Local) -> bool {
    let decl = body.local(l);
    if !matches!(decl.kind, LocalKind::User { .. })
        || !matches!(tys.kind(decl.ty), TyKind::MutRef(_))
    {
        return false;
    }
    let whole =
        |o: &Operand| matches!(o.place(), Some(p) if p.local == l && p.projection.is_empty());
    let written = |p: &Place| p.local == l && !p.projection.is_empty();
    body.blocks.iter().all(|b| {
        b.statements.iter().all(|s| match &s.kind {
            StatementKind::Assign(dest, rv) => {
                !written(dest)
                    && !matches!(rv, Rvalue::Ref(BorrowKind::Mut, p) if p.local == l)
                    && !super::elaborate::rvalue_operands(rv).into_iter().any(whole)
            }
            StatementKind::SetDiscriminant(p, _) | StatementKind::FreeBox(p) => p.local != l,
            StatementKind::BorrowFlag(FlagOp::Acquire { place, kind, .. }) => {
                !(place.local == l && *kind == BorrowKind::Mut)
            }
            _ => true,
        }) && match &b.terminator.kind {
            TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } => !whole(func) && !args.iter().any(whole) && !written(destination),
            TerminatorKind::SwitchInt { discr, .. } => !whole(discr),
            _ => true,
        }
    })
}

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
                // The reference the place is first reached through, held
                // in its local itself or in a part of it (`(*(_15 as
                // Some).0).0`, a binding of a `for` loop's item):
                // overwriting the local writes the reference, not what it
                // points to (B-2026-11-15-15).
                let first_deref = place.projection.iter().position(|e| *e == ProjElem::Deref);
                let deref_of = first_deref.and_then(|i| {
                    let prefix = Place {
                        local: place.local,
                        projection: place.projection[..i].to_vec(),
                    };
                    place_ty(body, tys, &prefix).ok().map(|pt| tys.kind(pt.ty))
                });
                let through_shared_ref = matches!(deref_of, Some(TyKind::Ref(_)));
                let through_mut_ref = matches!(deref_of, Some(TyKind::MutRef(_)));
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
    /// The parameter and result types of the closure with this `DefId`,
    /// when one signature is known for it ([`ClosureSig`]).
    closure_sig: &'a ClosureSig<'a>,
    /// Whether the body borrows any `Slice` place mutably. A MIR slice
    /// type does not say whether it is a `mut Slice`, so in a body that
    /// writes through some slice no slice counts as read-only.
    writes_slices: bool,
    /// The loans a call result with no place for a closure does not hold:
    /// see [`predicate_captures`].
    pred_captures: LoanSet,
    /// The locals holding a closure this body makes: see
    /// [`closures_made_here`].
    made_here: std::collections::HashSet<Local>,
}

/// The locals that hold, whole or in a part, a closure the body itself
/// makes: the closure, its erased value, and an array, tuple or struct
/// built from one. A function value a call hands back is among them only
/// when the callee is one of `view_results`: whether it captured its
/// arguments' views is the callee's to say, and §5.4 alone can only
/// suppose it did (`op(name) -> Fn(..)` choosing among closures that
/// capture nothing; B-2026-11-17-92).
fn closures_made_here(
    body: &Body,
    view_results: &ViewResults<'_>,
) -> std::collections::HashSet<Local> {
    let mut made: std::collections::HashSet<Local> = body
        .blocks
        .iter()
        .filter_map(|b| match &b.terminator.kind {
            TerminatorKind::Call {
                func:
                    Operand::Const(Const {
                        kind: ConstKind::FnDef(callee),
                        ..
                    }),
                destination,
                ..
            } if destination.projection.is_empty() && view_results(&callee.name) => {
                Some(destination.local)
            }
            _ => None,
        })
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for s in body.blocks.iter().flat_map(|b| &b.statements) {
            let StatementKind::Assign(d, rv) = &s.kind else {
                continue;
            };
            if !d.projection.is_empty() || made.contains(&d.local) {
                continue;
            }
            let from = |op: &Operand| op.place().is_some_and(|p| made.contains(&p.local));
            let is = match rv {
                Rvalue::Aggregate(AggregateKind::Closure { .. }, _) => true,
                Rvalue::Aggregate(_, ops) => ops.iter().any(from),
                Rvalue::Use(op) | Rvalue::Cast(_, op, _) => from(op),
                _ => false,
            };
            if is {
                made.insert(d.local);
                changed = true;
            }
        }
    }
    made
}

/// The loans only a function that returns no borrow holds: a capture
/// `_t = &k` moved into a closure that is erased to a `Fn(..) -> bool`, as
/// `filter`'s predicate is. A call of the function hands back none of them,
/// so a call result with no place for a closure (`collect`'s
/// `Vec[ref i64]`, `next`'s `Option[ref i64]`) does not hold them, though
/// the adaptor that keeps the function does (B-2026-11-15-154).
fn predicate_captures(body: &Body, tys: &TyInterner, loans: &[Loan]) -> LoanSet {
    let mut uses: std::collections::HashMap<Local, usize> = std::collections::HashMap::new();
    for block in &body.blocks {
        for s in &block.statements {
            for l in statement_use_def(&s.kind).0 {
                *uses.entry(l).or_default() += 1;
            }
        }
        for l in terminator_use_def_with(&block.terminator.kind, &|_| false).0 {
            *uses.entry(l).or_default() += 1;
        }
    }
    let once = |l: Local| uses.get(&l) == Some(&1);
    let whole_move = |op: &Operand| match op {
        Operand::Move(p) if p.projection.is_empty() => Some(p.local),
        _ => None,
    };
    // Each closure erased, at its one use, to a function returning no borrow.
    let mut erased = Vec::new();
    for block in &body.blocks {
        for s in &block.statements {
            if let StatementKind::Assign(_, Rvalue::Cast(_, op, t)) = &s.kind {
                let returns_none =
                    matches!(tys.kind(*t), TyKind::Fn { ret, .. } if !holds_borrow(tys, ret));
                if let Some(c) = whole_move(op).filter(|&c| returns_none && once(c)) {
                    erased.push(c);
                }
            }
        }
    }
    // The captures moved into those closures, each used there alone.
    let mut captures = Vec::new();
    for block in &body.blocks {
        for s in &block.statements {
            if let StatementKind::Assign(d, Rvalue::Aggregate(AggregateKind::Closure { .. }, ops)) =
                &s.kind
            {
                if d.projection.is_empty() && erased.contains(&d.local) {
                    captures.extend(ops.iter().filter_map(whole_move).filter(|&t| once(t)));
                }
            }
        }
    }
    let mut out = LoanSet::default();
    for (li, l) in loans.iter().enumerate() {
        let (bi, si) = l.at;
        let assigned = body.blocks[bi]
            .statements
            .get(si)
            .and_then(|s| match &s.kind {
                StatementKind::Assign(d, Rvalue::Ref(..)) if d.projection.is_empty() => {
                    Some(d.local)
                }
                _ => None,
            });
        if assigned.is_some_and(|t| captures.contains(&t)) {
            out.insert(li);
        }
    }
    out
}

/// Whether some loan of the body is a `mut` borrow of a `Slice` place.
fn writes_slices(body: &Body, tys: &TyInterner, loans: &[Loan]) -> bool {
    loans.iter().any(|l| {
        l.kind == BorrowKind::Mut
            && place_ty(body, tys, &l.place)
                .is_ok_and(|pt| matches!(tys.kind(pt.ty), TyKind::Slice(_)))
    })
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

    /// A shared reference to a `Slice` that cannot be written through: a
    /// view, like a `str`, so what a native makes from it (`rest[1..]`)
    /// borrows what the slice borrows, not the local holding it, and the
    /// local may be bound to the result (`rest = rest[1..]`,
    /// B-2026-11-08-66).
    fn refers_to_read_only_slice(&self, op: &Operand) -> bool {
        !self.writes_slices
            && op
                .place()
                .and_then(|p| place_ty(self.body, self.tys, p).ok())
                .is_some_and(|pt| match self.tys.kind(pt.ty) {
                    TyKind::Ref(t) => matches!(self.tys.kind(t), TyKind::Slice(_)),
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

    /// What the value behind the reference `op` holds: each place it
    /// borrows contributes what that place holds, and a place behind
    /// another reference, whose contents came from outside, its loan.
    fn held_behind(&self, o: &Origins, op: &Operand, into: &mut LoanSet) {
        let Some(p) = op.place() else { return };
        for li in o.get(p.local).iter() {
            let l = &self.loans[li];
            if l.place.projection.contains(&ProjElem::Deref) {
                into.insert(li);
            } else {
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
        for t in self.store_targets(o, r, through_shared) {
            self.hold(o, t, val);
        }
    }

    /// The local places the reference held in `r` lets a store reach.
    fn store_targets(&self, o: &Origins, r: Local, through_shared: bool) -> Vec<Local> {
        o.get(r)
            .iter()
            .map(|li| &self.loans[li])
            .filter(|l| {
                (through_shared || l.kind == BorrowKind::Mut)
                    && !l.place.projection.contains(&ProjElem::Deref)
                    && self.holds(&l.place)
            })
            .map(|l| l.place.local)
            .collect()
    }

    /// Local `t` now holds `val`; a value never holds a borrow of its own
    /// local.
    fn hold(&self, o: &mut Origins, t: Local, val: &LoanSet) {
        for li in val.iter() {
            let own = self.loans[li].place.local == t
                && !self.loans[li].place.projection.contains(&ProjElem::Deref);
            if !own {
                o.insert(t, li);
            }
        }
    }

    /// What a callee given the reference in `r` may store `val` into. A
    /// closure is the exception to `store_through`: its captures' types are
    /// fixed when it is made, so a call stores nothing into the closure
    /// itself. It stores into what the closure's `mut ref` captures borrow,
    /// and into a capture it owns whose type holds borrows (§9.3;
    /// B-2026-11-08-56). So `|x: Vec[i64]| total += x.len()` keeps none of
    /// its arguments, and `|s: Str| keep.push(s)` puts `s`'s loans in
    /// `keep`.
    fn store_through_call(&self, o: &mut Origins, r: Local, val: &LoanSet, through_shared: bool) {
        if val.is_empty() {
            return;
        }
        let mut seen: Vec<Local> = Vec::new();
        let mut work = self.store_targets(o, r, through_shared);
        while let Some(t) = work.pop() {
            if seen.contains(&t) {
                continue;
            }
            seen.push(t);
            let captures = place_ty(self.body, self.tys, &Place::local(t))
                .ok()
                .and_then(|pt| match self.tys.kind(pt.ty) {
                    TyKind::Closure(_, ts) => Some(ts.to_vec()),
                    _ => None,
                });
            let Some(captures) = captures else {
                self.hold(o, t, val);
                continue;
            };
            let owns_a_holder = captures.iter().any(|c| {
                !matches!(self.tys.kind(*c), TyKind::Ref(_) | TyKind::MutRef(_))
                    && holds_borrow(self.tys, *c)
            });
            if owns_a_holder {
                self.hold(o, t, val);
            }
            work.extend(self.store_targets(o, t, false));
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
            // A `dyn` value owns its value (design.md § `dyn Trait`) and
            // keeps no borrow, so a view may not go in one (B-2026-11-17-25).
            Rvalue::Use(op)
            | Rvalue::Cast(CastKind::Erase, op, _)
            | Rvalue::Cast(CastKind::ToDyn(_), op, _) => self.operand_origins(o, op, &mut val),
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
    ///
    /// And a closure the body makes, holding a borrow, passed by value to
    /// a call whose result holds none but has a slot for it, as
    /// `Vec.from_array([f])` or a builder's `add(f) -> Self` keep `f` in a
    /// result that keeps no borrow (B-2026-11-17-24).
    fn unkept_call_loans(
        &self,
        o: &Origins,
        func: &Operand,
        args: &[Operand],
        dest: &Place,
    ) -> LoanSet {
        let mut val = LoanSet::default();
        if dest.projection.is_empty() && !self.holds(dest) {
            if let Ok(dt) = place_ty(self.body, self.tys, dest) {
                for a in args {
                    let Some(p) = a.place().filter(|p| self.made_here.contains(&p.local)) else {
                        continue;
                    };
                    let Ok(at) = place_ty(self.body, self.tys, p) else {
                        continue;
                    };
                    if matches!(self.tys.kind(at.ty), TyKind::Ref(_) | TyKind::MutRef(_)) {
                        continue;
                    }
                    let mut erased = Vec::new();
                    erased_parts(self.tys, at.ty, &mut erased, false);
                    if erased
                        .iter()
                        .any(|e| has_erased_slot(self.tys, dt.ty, *e, self.closure_sig))
                    {
                        self.operand_origins(o, a, &mut val);
                    }
                }
            }
        }
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
    /// What an argument gives a call's result. A closure lent to the call
    /// (`opt.map(|p| p.0)` passes `&closure`) gives what the closure
    /// holds, not a loan of the closure local, unless the result's type has
    /// a place for a closure: the callee calls the closure and drops the
    /// borrow, as a call through a reference does (B-2026-11-09-98).
    ///
    /// A function argument, a closure lent or moved or an erased function
    /// value, adds what it holds only where the result's type can hold a
    /// value of its type: a generic `F` the result mentions (`Map[I, F]`)
    /// or a function type inside it (core semantics §5.3, "An argument
    /// contributes only where its type can go"). So `it.find(|z| *z == t)`
    /// borrows only what `it` holds. A closure that may hand back a borrow
    /// of what it captures ([`fn_type_hands_back`]) still gives what it
    /// holds, since the callee can put that in the result.
    fn arg_origins(&self, o: &Origins, a: &Operand, dest: &Place, into: &mut LoanSet) {
        let Some((f, lent, sig)) = self.fn_arg(a) else {
            self.operand_origins(o, a, into);
            return;
        };
        let hands_back = sig
            .as_ref()
            .is_none_or(|(ps, r)| fn_type_hands_back(self.tys, ps, *r));
        let fits = place_ty(self.body, self.tys, dest)
            .map_or(true, |d| can_hold_fn(self.tys, d.ty, f, sig.as_ref()));
        if fits {
            self.operand_origins(o, a, into);
        } else if !hands_back {
            // Nothing the function holds can reach the result.
        } else if lent {
            self.held_behind(o, a, into);
        } else {
            self.operand_origins(o, a, into);
        }
    }

    /// A function argument's function type (the closure's, for a closure
    /// lent through a reference), whether it is lent, and its signature
    /// when known: a closure's from [`ClosureSig`], an erased function's
    /// from its type.
    #[allow(clippy::type_complexity)]
    fn fn_arg(&self, a: &Operand) -> Option<(Ty, bool, Option<(Vec<Ty>, Ty)>)> {
        let p = a.place()?;
        let t = place_ty(self.body, self.tys, p).ok()?.ty;
        let (f, lent) = match self.tys.kind(t) {
            TyKind::Ref(f) | TyKind::MutRef(f) => (f, true),
            _ => (t, false),
        };
        match self.tys.kind(f) {
            TyKind::Closure(def, _) => Some((f, lent, (self.closure_sig)(def))),
            TyKind::Fn { params, ret, .. } => Some((f, lent, Some((params.to_vec(), ret)))),
            _ => None,
        }
    }

    /// Whether `func` is a reference to an erased function value whose
    /// result holds a borrow that none of its arguments can supply.
    fn callee_hands_back_its_env(&self, func: &Operand) -> bool {
        let Some(p) = func.place() else {
            return false;
        };
        let Ok(pt) = place_ty(self.body, self.tys, p) else {
            return false;
        };
        let (TyKind::Ref(f) | TyKind::MutRef(f)) = self.tys.kind(pt.ty) else {
            return false;
        };
        match self.tys.kind(f) {
            // Only when no argument can carry a borrow does what comes back
            // have to come from the environment: an `Fn(ref T) -> ref U` (a
            // `map` adaptor's) may hand back a part of its argument.
            TyKind::Fn { params, ret, .. } => {
                holds_borrow(self.tys, ret) && params.iter().all(|p| !holds_borrow(self.tys, *p))
            }
            _ => false,
        }
    }

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
            // Into an erased function slot (`mem.swap(mut f, mut g)` on two
            // `Fn` locals) a callee stores a function value: one moved out
            // from behind another reference argument holds what that
            // pointee holds, never the reference itself, since a closure
            // stored there captures by move and cannot take a borrowed
            // parameter (§9.3; B-2026-11-02-19).
            let fn_slot = matches!(
                self.tys.kind(pt.ty),
                TyKind::MutRef(t) if matches!(self.tys.kind(t), TyKind::Fn { .. })
            );
            // A pointee whose type has no place for a closure (`VecIter[i64]`
            // given `find(ref closure)`) cannot keep the reference to one:
            // it may keep only what the closure holds, which a call of the
            // closure can hand back (B-2026-11-10-24).
            let pointee = match self.tys.kind(pt.ty) {
                TyKind::MutRef(t) | TyKind::Ref(t) => Some(t),
                _ => None,
            };
            let closure_free = pointee.is_some_and(|t| !may_hold_closure(self.tys, t));
            // A written pointee whose every slot `contains_ty` can see: a
            // collection, a tuple, an array or a declared type.
            let slotted = match self.tys.kind(pt.ty) {
                TyKind::MutRef(t)
                    if matches!(
                        self.tys.kind(t),
                        TyKind::Intrinsic(
                            IntrinsicTy::Vec(_)
                                | IntrinsicTy::Set(_)
                                | IntrinsicTy::VecDeque(_)
                                | IntrinsicTy::SortedSet(_)
                                | IntrinsicTy::Map(..)
                                | IntrinsicTy::SortedMap(..)
                        ) | TyKind::Tuple(_)
                            | TyKind::Array(..)
                            | TyKind::Adt(_)
                    ) =>
                {
                    Some(t)
                }
                _ => None,
            };
            let mut others = LoanSet::default();
            let rest = args.iter().enumerate().filter(|(j, _)| *j != i);
            for (k, a) in std::iter::once(func)
                .chain(rest.map(|(_, a)| a))
                .enumerate()
            {
                // A function argument the pointee's type has no place for
                // stores nothing there unless it can hand back a borrow of
                // what it holds (core semantics §5.3, "An argument
                // contributes only where its type can go"): `p.next_if(f)`
                // on a `Peekable` of another closure's adaptor. The callee
                // itself (`k == 0`) runs here, so what it holds may be
                // stored.
                if k > 0 {
                    if let (Some((f, _, Some(sig))), Some(t)) = (self.fn_arg(a), pointee) {
                        if !fn_type_hands_back(self.tys, &sig.0, sig.1)
                            && !can_hold_fn(self.tys, t, f, Some(&sig))
                        {
                            continue;
                        }
                    }
                }
                let behind_ref = a.place().is_some_and(|q| {
                    place_ty(self.body, self.tys, q).is_ok_and(|qt| match self.tys.kind(qt.ty) {
                        // A reference argument the pointee has no slot for,
                        // the key of `m.get_mut(k)` on a `Map[Str, i64]`, can
                        // only leave what its referent holds there, the
                        // `Str`'s own origin, never the reference to `k`
                        // (kata 2115).
                        TyKind::Ref(t) | TyKind::MutRef(t) => {
                            fn_slot
                                || (closure_free && matches!(self.tys.kind(t), TyKind::Closure(..)))
                                || slotted.is_some_and(|s| !contains_ty(self.tys, s, qt.ty))
                        }
                        _ => false,
                    })
                });
                if behind_ref {
                    self.held_behind(&before, a, &mut others);
                } else {
                    self.operand_origins(&before, a, &mut others);
                }
            }
            self.store_through_call(o, p.local, &others, through_shared);
        }
        let mut val = LoanSet::default();
        // A closure called through a reference: what the closure holds.
        let callee_ref = func.place().is_some_and(|p| {
            place_ty(self.body, self.tys, p)
                .is_ok_and(|pt| matches!(self.tys.kind(pt.ty), TyKind::Ref(_) | TyKind::MutRef(_)))
        });
        if callee_ref {
            self.pointee_origins(o, func, &mut val);
            // An erased function value that can hand back a borrow owns
            // what it captured (a closure stored in a field, an element, a
            // tuple, an `Option` or returned from a call captures by move,
            // §9.3), so what it hands back is a view of its own
            // environment: the result borrows the value called, through the
            // reference the call takes (§5.4; B-2026-11-19-96: `(h.f)()`
            // gave a slice of the capture no origins, and it outlived `h`).
            if self.callee_hands_back_its_env(func) {
                self.operand_origins(o, func, &mut val);
            }
        } else {
            self.operand_origins(o, func, &mut val);
        }
        match borrows {
            ResultBorrows::Args => {
                for a in args {
                    self.arg_origins(o, a, dest, &mut val);
                }
            }
            // A `str` is itself a view (core semantics §5.2): what a native
            // makes from a reference to one borrows the `String` it views,
            // not the place holding the view.
            ResultBorrows::Receiver
                if self.refers_to_str(&args[0]) || self.refers_to_read_only_slice(&args[0]) =>
            {
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
                    self.arg_origins(o, a, dest, &mut val);
                }
            }
            ResultBorrows::ReceiverValue => self.pointee_origins(o, &args[0], &mut val),
            ResultBorrows::ArgsAfterReceiver => {
                for a in args.iter().skip(1) {
                    self.arg_origins(o, a, dest, &mut val);
                }
            }
            ResultBorrows::ViewArgs(mask) => {
                for (i, a) in args.iter().enumerate() {
                    if i < 64 && mask >> i & 1 != 0 {
                        self.pointee_origins(o, a, &mut val);
                    } else {
                        self.arg_origins(o, a, dest, &mut val);
                    }
                }
            }
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
        if !self.pred_captures.is_empty()
            && place_ty(self.body, self.tys, dest)
                .is_ok_and(|pt| !may_hold_closure(self.tys, pt.ty))
        {
            val.remove_all(&self.pred_captures);
        }
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
/// B-2026-10-19-6), nor does a struct or enum with no `Drop` body of its
/// own whose parts read none; one with a `Drop` body, a `shared` handle or
/// a type MIR cannot see into is taken to read.
fn drop_reads_borrows(body: &Body, tys: &TyInterner, place: &Place) -> bool {
    fn go(tys: &TyInterner, ty: Ty) -> bool {
        go_seen(tys, ty, &mut Vec::new())
    }
    fn go_seen(tys: &TyInterner, ty: Ty, seen: &mut Vec<Ty>) -> bool {
        if seen.contains(&ty) {
            return false;
        }
        seen.push(ty);
        let go = |tys: &TyInterner, t: Ty| go_seen(tys, t, &mut seen.clone());
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
            // A struct or enum with no `Drop` body of its own runs no code
            // when it is freed: it frees its parts, so it reads a view only
            // where a part's drop does. `StrSplit` (a `Str` and its own
            // `String` pattern) dropped after the loop that walked it reads
            // nothing it borrows (B-2026-11-13-82).
            TyKind::Adt(a) if !tys.has_drop_impl(ty) && !is_task_group(tys, ty) => {
                let adt = tys.adt(a);
                adt.variants.iter().enumerate().any(|(v, var)| {
                    (0..var.fields.len() as u32).any(|f| {
                        tys.field_ty(ty, adt.is_enum.then_some(v as u32), f)
                            .is_none_or(|t| go(tys, t))
                    })
                })
            }
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
                // A task's handle holds what its closure holds, for `join`
                // to hand back as the task's result (B-2026-11-19-95).
                if adt.name == "TaskHandle" {
                    if let Some((_, args)) = tys.tcx().adt_of(ty) {
                        return args.iter().any(|t| go(tys, *t, false, seen));
                    }
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

/// A closure body's signature for [`ClosureSig`]: its parameter types,
/// the environment `_1` left out, and its result type.
pub fn closure_signature(body: &Body) -> (Vec<Ty>, Ty) {
    let params: Vec<Ty> = body
        .locals
        .iter()
        .skip(2)
        .take(body.arg_count.saturating_sub(1))
        .map(|l| l.ty)
        .collect();
    (params, body.locals[0].ty)
}

/// Whether a function of these parameter and result types may hand a
/// borrow what it holds back to its caller: through its result, or by
/// storing into a `mut ref` parameter whose pointee can hold a borrow. One
/// that cannot, a predicate returning `bool` or a key function returning
/// an `i64`, keeps its captures' loans to itself (core semantics §5.3).
fn fn_type_hands_back(tys: &TyInterner, params: &[Ty], ret: Ty) -> bool {
    holds_borrow(tys, ret)
        || params
            .iter()
            .any(|p| matches!(tys.kind(*p), TyKind::MutRef(t) if holds_borrow(tys, t)))
}

/// Can a value of type `holder` keep a value of the function type `f` (a
/// closure type or an erased function type, of signature `sig` when known),
/// or a reference to one: `f` itself anywhere in it, an erased function
/// slot of `f`'s signature, or a slot that takes anything (a `dyn` value, a
/// type MIR cannot see into, a `shared` handle or a `TaskGroup`). Another
/// closure's type cannot, nor an erased slot of another signature, as
/// `IterMap`'s `Fn(ref i64) -> i64` cannot take a `next_if` predicate
/// (core semantics §5.3).
fn can_hold_fn(tys: &TyInterner, holder: Ty, f: Ty, sig: Option<&(Vec<Ty>, Ty)>) -> bool {
    fn go(
        tys: &TyInterner,
        ty: Ty,
        f: Ty,
        sig: Option<&(Vec<Ty>, Ty)>,
        seen: &mut Vec<Ty>,
    ) -> bool {
        if ty == f {
            return true;
        }
        if seen.contains(&ty) {
            return false;
        }
        seen.push(ty);
        match tys.kind(ty) {
            TyKind::Fn { params, ret, .. } => {
                sig.is_none_or(|(ps, r)| params.as_slice() == ps.as_slice() && ret == *r)
            }
            TyKind::Dyn { .. } | TyKind::Other | TyKind::Shared(_) | TyKind::Weak(_) => true,
            TyKind::Closure(..) => false,
            TyKind::Ref(t)
            | TyKind::MutRef(t)
            | TyKind::Array(t, _)
            | TyKind::Slice(t)
            | TyKind::RawPtr { pointee: t, .. } => go(tys, t, f, sig, seen),
            TyKind::Tuple(ts) => ts.iter().any(|t| go(tys, *t, f, sig, seen)),
            TyKind::Intrinsic(
                IntrinsicTy::Vec(e)
                | IntrinsicTy::Set(e)
                | IntrinsicTy::VecDeque(e)
                | IntrinsicTy::SortedSet(e),
            ) => go(tys, e, f, sig, seen),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                go(tys, k, f, sig, seen) || go(tys, v, f, sig, seen)
            }
            TyKind::Intrinsic(_) => false,
            TyKind::Adt(a) => {
                if is_task_group(tys, ty) {
                    return true;
                }
                let adt = tys.adt(a);
                let is_enum = adt.is_enum;
                let counts: Vec<usize> = adt.variants.iter().map(|v| v.fields.len()).collect();
                counts.iter().enumerate().any(|(v, n)| {
                    (0..*n as u32).any(|i| {
                        tys.field_ty(ty, is_enum.then_some(v as u32), i)
                            .is_none_or(|t| go(tys, t, f, sig, seen))
                    })
                })
            }
            TyKind::Bool
            | TyKind::Char
            | TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Str
            | TyKind::FnDef(_) => false,
        }
    }
    go(tys, holder, f, sig, &mut Vec::new())
}

/// Can a value of type `ty` keep a closure, or a reference to one: a
/// closure, erased function or `dyn` value anywhere in it, a type MIR
/// cannot see into, a `shared` handle (whose object may hold anything its
/// fields declare) or a `TaskGroup`.
fn may_hold_closure(tys: &TyInterner, ty: Ty) -> bool {
    fn go(tys: &TyInterner, ty: Ty, seen: &mut Vec<Ty>) -> bool {
        if seen.contains(&ty) {
            return false;
        }
        seen.push(ty);
        match tys.kind(ty) {
            TyKind::Closure(..)
            | TyKind::Fn { .. }
            | TyKind::Dyn { .. }
            | TyKind::Other
            | TyKind::Shared(_)
            | TyKind::Weak(_) => true,
            TyKind::Ref(t)
            | TyKind::MutRef(t)
            | TyKind::Array(t, _)
            | TyKind::Slice(t)
            | TyKind::RawPtr { pointee: t, .. } => go(tys, t, seen),
            TyKind::Tuple(ts) => ts.iter().any(|t| go(tys, *t, seen)),
            TyKind::Intrinsic(
                IntrinsicTy::Vec(e)
                | IntrinsicTy::Set(e)
                | IntrinsicTy::VecDeque(e)
                | IntrinsicTy::SortedSet(e),
            ) => go(tys, e, seen),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                go(tys, k, seen) || go(tys, v, seen)
            }
            TyKind::Intrinsic(_) => false,
            TyKind::Adt(a) => {
                if is_task_group(tys, ty) {
                    return true;
                }
                let adt = tys.adt(a);
                let is_enum = adt.is_enum;
                let counts: Vec<usize> = adt.variants.iter().map(|v| v.fields.len()).collect();
                counts.iter().enumerate().any(|(v, n)| {
                    (0..*n as u32).any(|f| {
                        tys.field_ty(ty, is_enum.then_some(v as u32), f)
                            .is_none_or(|t| go(tys, t, seen))
                    })
                })
            }
            TyKind::Bool
            | TyKind::Char
            | TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Str
            | TyKind::FnDef(_) => false,
        }
    }
    go(tys, ty, &mut Vec::new())
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

/// The closures, function values and `dyn` values a value of type `ty`
/// carries by value: itself, or in its elements or fields.
fn erased_parts(tys: &TyInterner, ty: Ty, out: &mut Vec<Ty>, into_shared: bool) {
    fn go(tys: &TyInterner, ty: Ty, out: &mut Vec<Ty>, seen: &mut Vec<Ty>, into_shared: bool) {
        if seen.contains(&ty) {
            return;
        }
        seen.push(ty);
        match tys.kind(ty) {
            TyKind::Closure(..) | TyKind::Fn { .. } | TyKind::Dyn { .. } => out.push(ty),
            TyKind::Tuple(ts) => ts.iter().for_each(|t| go(tys, *t, out, seen, into_shared)),
            TyKind::Array(t, _)
            | TyKind::Intrinsic(
                IntrinsicTy::Vec(t)
                | IntrinsicTy::Set(t)
                | IntrinsicTy::VecDeque(t)
                | IntrinsicTy::SortedSet(t),
            ) => go(tys, t, out, seen, into_shared),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                go(tys, k, out, seen, into_shared);
                go(tys, v, out, seen, into_shared);
            }
            // A `shared` value's fields are parts of it too, as slots a
            // closure may be stored in (B-2026-11-17-92). As parts of a
            // value already made they hold no view: none may be stored in
            // a `shared` value (§5.7), so a `Vec` of handles whose type has
            // a function field carries none (B-2026-11-18-75).
            TyKind::Shared(_) if !into_shared => {}
            TyKind::Adt(a) | TyKind::Shared(a) => {
                let adt = tys.adt(a);
                let is_enum = adt.is_enum;
                let counts: Vec<usize> = adt.variants.iter().map(|v| v.fields.len()).collect();
                for (v, n) in counts.iter().enumerate() {
                    for f in 0..*n as u32 {
                        if let Some(t) = tys.field_ty(ty, is_enum.then_some(v as u32), f) {
                            go(tys, t, out, seen, into_shared);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    go(tys, ty, out, &mut Vec::new(), into_shared);
}

/// Whether a value of type `holder` has, by value, a slot the erased value
/// `e` (a closure, function or `dyn` type) can go in: a function type of
/// its signature, or a `dyn` of its trait.
///
/// A `dyn` holder is a slot for anything: the value it owns may be of any
/// type, holding `e` in a field (`boxit(f) -> dyn G { W { f: f } }`,
/// B-2026-11-17-92).
fn has_erased_slot(tys: &TyInterner, holder: Ty, e: Ty, closure_sig: &ClosureSig<'_>) -> bool {
    if matches!(tys.kind(holder), TyKind::Dyn { .. }) {
        return true;
    }
    let sig = match tys.kind(e) {
        TyKind::Closure(def, _) => closure_sig(def),
        TyKind::Fn { params, ret, .. } => Some((params.to_vec(), ret)),
        _ => None,
    };
    let fits = |t: Ty| match (tys.kind(e), tys.kind(t)) {
        (TyKind::Closure(..) | TyKind::Fn { .. }, TyKind::Fn { params, ret, .. }) => sig
            .as_ref()
            .is_none_or(|(ps, r)| params.as_slice() == ps.as_slice() && ret == *r),
        (TyKind::Dyn { trait_: a, .. }, TyKind::Dyn { trait_: b, .. }) => a == b,
        _ => false,
    };
    let mut parts = Vec::new();
    erased_parts(tys, holder, &mut parts, true);
    parts.into_iter().any(fits)
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

/// The loans in scope where each block starts: created on some path to it
/// and not killed since. Assigning a whole reference local `r` kills the
/// loans of places reached through `*r` (`&mut (*r).f`, a pattern binding
/// of `*r`): their path names what `r` held before, which no access
/// through the new `r` reaches, and which the reborrow keeps borrowed
/// through `r`'s own loans (§5.3, "A borrow through a reference is not a
/// borrow of the reference"; B-2026-11-09-45). Rust's NLL kills the same
/// loans. A loan out of scope conflicts with nothing.
fn scope_dataflow(body: &Body, loans: &[Loan]) -> Vec<Option<LoanSet>> {
    let mut states: Vec<Option<LoanSet>> = vec![None; body.blocks.len()];
    if body.blocks.is_empty() {
        return states;
    }
    states[0] = Some(LoanSet::default());
    let at: std::collections::HashMap<(usize, usize), usize> =
        loans.iter().enumerate().map(|(li, l)| (l.at, li)).collect();
    let mut work = vec![0usize];
    while let Some(bi) = work.pop() {
        let mut o = states[bi].clone().expect("queued blocks have a state");
        let block = &body.blocks[bi];
        for (si, st) in block.statements.iter().enumerate() {
            scope_transfer(&mut o, loans, &at, &st.kind, (bi, si));
        }
        if let TerminatorKind::Call { destination, .. } = &block.terminator.kind {
            kill_through(&mut o, loans, destination);
        }
        for s in block.terminator.kind.successors() {
            let si = s.index();
            let changed = match &mut states[si] {
                None => {
                    states[si] = Some(o.clone());
                    true
                }
                Some(cur) => cur.union(&o),
            };
            if changed && !work.contains(&si) {
                work.push(si);
            }
        }
    }
    states
}

/// Moves `in_scope` past statement `s` at `at` ([`scope_dataflow`]).
fn scope_transfer(
    in_scope: &mut LoanSet,
    loans: &[Loan],
    by_site: &std::collections::HashMap<(usize, usize), usize>,
    s: &StatementKind,
    at: (usize, usize),
) {
    if let StatementKind::Assign(dest, rv) = s {
        if matches!(rv, Rvalue::Ref(..)) {
            if let Some(&li) = by_site.get(&at) {
                in_scope.insert(li);
            }
        }
        kill_through(in_scope, loans, dest);
    }
}

/// Takes out of `in_scope` the loans reached through `*dest` when `dest`
/// is a whole local.
fn kill_through(in_scope: &mut LoanSet, loans: &[Loan], dest: &Place) {
    if !dest.projection.is_empty() || in_scope.is_empty() {
        return;
    }
    in_scope.0.retain(|&li| {
        let l = &loans[li as usize];
        !((l.through_mut_ref || l.through_shared_ref) && l.place.local == dest.local)
    });
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
    // Nor does assigning the reference a new value: what it held stays
    // borrowed, and the loan goes out of scope (`scope_dataflow`).
    if loan.through_mut_ref
        && place.projection.is_empty()
        && matches!(access, Access::StorageDead | Access::Drop | Access::Write)
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
        // As the build frontend reads it: a UFCS call is the receiver form.
        crate::lowering::rewrite_ufcs(&mut program, &tc.ufcs_calls);
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
            "err_par_user_resource",
            "err_par_pub_clause_contract",
            "err_par_bound_clause_ceiling",
            "err_frozen_closure_escapes",
            "err_sync_own_self",
            "err_weak_plain_target",
            "err_question_from_effects",
            "err_partial_move_drop_type",
            "err_enum_payload_drop_type_into_constructor",
            "err_fn_value_shared_crosses_task",
            "err_fn_param_unmarked_spawned",
            "err_effect_vars_par_unmarked",
            "err_fn_kind_mismatch",
            // The receiver rule's body check (§5.4, B-2026-11-02-28) and a
            // non-escaping parameter captured by a returned closure (§9.3,
            // B-2026-11-02-31).
            "err_receiver_method_returns_argument",
            "err_returned_closure_captures_non_escaping_param",
            // The same body check over a view-typed argument (B-2026-11-13-31).
            "err_receiver_method_returns_view_argument",
            // A function value whose environment reaches a `shared` handle
            // called from a `par` branch or `par for` body: refused by the
            // strict cross-task pass, before MIR (B-2026-11-03-8).
            "err_par_branch_uses_closure_over_shared",
            "err_par_for_uses_closure_over_shared",
            // A map's `iter_mut()` lends the value, not the key, and the
            // value binding is a `mut ref` (B-2026-11-03-17).
            "err_map_iter_mut_key_write",
            "err_map_iter_mut_value_plain_assign",
            // `std.log`'s fields and its attribute (B-2026-10-27-20).
            "err_log_reserved_label",
            "err_log_duplicate_label",
            "err_log_unlabeled_field",
            "err_log_field_not_serialize",
            "err_observability_attr",
            // A generic item's own parameter is rigid (B-2026-10-27-19).
            "err_rigid_type_param",
            // Effect variables, refused by the effect checker (design.md
            // § Effect variables).
            "err_effect_var_unfit",
            "err_effect_var_public_clause",
            "err_effect_vars_par_shared",
            // A default with state written outside an override (§14.14),
            // refused by the effect pass.
            "err_stateful_default",
            // Calls through `dyn` values (design.md § `dyn Trait`).
            "err_effect_dyn_par_shared",
            "err_effect_dyn_clause_par_shared",
            "err_effect_dyn_clause_exceeded",
            // The empty clause `with ()` on a function type (design.md § How a
            // call through a function value gets its effects).
            "err_effect_empty_clause_exceeded",
            // `#[no_effect]` over arithmetic and allocation, and clauses on
            // private functions, methods and trait defaults (design.md §12;
            // B-2026-11-04-27, -28, -29).
            "err_no_effect_panics_arithmetic",
            "err_no_effect_heap_allocations",
            "err_effect_clause_checked_everywhere",
            // A function's own drops charge its `Drop` bodies' effects
            // (core semantics §12 default 3, B-2026-11-17-13).
            "err_drop_effects_need_declaring",
            "err_no_effect_counts_drops",
            "err_drop_effects_on_a_path_no_move_takes",
            "err_drop_effects_of_an_arm_binding",
            "err_dyn_view_unmarked_spawned",
            "err_dyn_view_unmarked_par",
            "err_fn_vec_call_result_marked",
            "err_fn_vec_local_push_marked",
            "err_fn_generic_param_collect",
            "err_fn_generic_param_id",
            "err_fn_generic_param_replace",
            "err_fn_solved_closure_result",
            "err_fn_solved_impl_new",
            // Tasks sharing a written provider (§14.9), refused by the
            // effect pass.
            "err_provider_tasks_write",
            "err_provider_override_tasks_write",
            // `hash` into a type that is no `Hasher` (B-2026-10-31-25).
            "err_hash_into_non_hasher",
            // A task's read of a `Copy` place is a borrow (§9.1, §9.5),
            // refused by the strict task-group pass (B-2026-11-01-32).
            "err_taskgroup_copy_read_then_written",
            "err_taskgroup_copy_read_in_loop_then_written",
            // A compound assignment whose result would narrow back into the
            // place, and a `let` of a `Vec` into a `Slice` (B-2026-11-04-59).
            "err_compound_assign_narrows",
            "err_compound_assign_float_narrows",
            "err_let_does_not_convert_to_slice",
            // A `mut ref self` method on a constant (B-2026-11-06-21).
            "err_const_mut_method",
        ];
        // Pins that call Kāra library functions, which `check_built` does
        // not append (a `select` desugars to calls into sync.kara;
        // `TaskGroup.isolating` is declared in isolating.kara; a `weak`
        // local reads as the library's `Option`; `collect` is iter.kara's).
        // The build in `built_core_pins_match_their_expected_output` runs
        // them with the library, through the same move and borrow checks.
        let needs_library = [
            // Which task captures move is the typechecker's core_taskgroup
            // table, built only under the library-source check
            // (B-2026-11-19-107).
            "ok_taskgroup_pattern_binding_moved",
            // `self.slots[i]` indexes for writing through the library's
            // `Vec.index_mut` (B-2026-11-17-152).
            "match_element_through_mut_refs_binds_mut_ref",
            // A channel's `recv` gives an `Option` only with the library
            // (B-2026-11-17-35).
            "drop_body_suspends_in_a_cancelled_teardown",
            "drop_body_suspends_waits_on_a_channel",
            // `Mutex.lock` and a `weak` field's read are the library's
            // (B-2026-11-17-101).
            "drop_body_suspends_in_a_mutex",
            "drop_body_suspends_lets_other_tasks_run",
            "drop_body_suspends_while_a_weak_handle_outlives",
            // A channel's `recv` and `OnceCell.set` are the library's
            // (B-2026-11-17-127).
            "drop_body_suspends_in_a_channel_queue",
            "drop_body_suspends_in_a_once_cell",
            // `Mutex.lock` (B-2026-11-17-132).
            "err_generic_instance_view_into_a_mutex",
            "err_view_assigned_through_a_mutex_guard",
            "err_view_pushed_into_a_mutex_through_its_lock",
            "ok_owned_copies_pushed_into_a_mutex",
            // `ref a.child.items`, a `ref` of a field path: this check's
            // plain typecheck takes only `ref v[i]` (E_REF_OPERAND_UNSUPPORTED)
            // and the library build reads core places, so these run there
            // (B-2026-11-17-26, -32).
            "ok_shared_handle_field_closure_capture",
            "ok_shared_handle_field_local_alias",
            "ok_shared_handle_field_plain_struct_inside",
            "ok_shared_handle_field_ref",
            "ok_shared_handle_field_ref_in_block_tail",
            "ok_shared_handle_field_ref_in_if_arm",
            "ok_shared_handle_field_scalar_ref",
            "ok_shared_handle_field_two_levels",
            "ok_shared_handle_field_via_weak",
            // `Option` is option.kara's.
            "ok_mut_ref_cursor_walks_to_tail",
            "ok_ref_cursor_rebinds",
            "select_source_order",
            "isolating_panic_ends_its_task",
            "panic_track_caller_reports_the_call",
            "panic_range_from_overflows_after_the_type_max",
            "ok_weak_local_reads_as_option",
            "ok_ref_handle_collects_as_the_handle",
            "ok_weak_slot_upgrade",
            "ok_closure_reads_method_args_handles_and_matches",
            "par_branch_drops_a_capture",
            // `into_iter`, `iter` and `vec!` are the library's.
            "for_into_iter_consumes",
            "ok_ref_copied_from_a_tuple_element",
            "ok_error_anyerror",
            // `Result.or_else` is result.kara's.
            "ok_closure_literal_solves_callee_param",
            // `map`, `collect` and `fold` are iter.kara's.
            "ok_annotated_closure_param_borrows_from_owned_slot",
            "ok_collect_temporary_lent_to_mut_ref",
            // `std.log` is spliced from the library with `std.json`.
            "log_custom_backend",
            "log_filtering",
            "log_json_format",
            "log_text_format",
            "log_transparent",
            // `std.net`'s sockets are the library's.
            "par_real_clients",
            // `RandomProvider`, `ClockProvider` and their resources are the
            // library's (B-2026-11-17-14).
            "ok_provider_bound_inside_pub_fn",
            // `Vec.rotate_left` and `Map.get` are the library's.
            "ok_ref_number_as_native_argument",
            // `Option.get_or_insert_with` is option.kara's (B-2026-11-08-69).
            "ok_literal_takes_closure_and_entry_slots",
            // A `ref` binding over an owned local is a view, and a `match`
            // of such results a reference, in the check the build runs with
            // the library (core semantics §4.6, B-2026-10-28-25).
            "ok_match_result_is_a_ref",
            // A `mut ref` pattern binding writes the shared enum's payload
            // under the core ownership rules (§4.6), which the build applies
            // (B-2026-11-08-83).
            "panic_shared_enum_payload_alias",
            // `Option.replace` is option.kara's (B-2026-11-02-27).
            "ok_owned_place_into_ref_slot",
            // `Receiver.recv` is sync.kara's (B-2026-11-02-37).
            "ok_literal_into_ref_slot",
            // A part of a `shared` value binds a `ref`, and a bare `Fn`
            // parameter lends its argument, in the check the build runs
            // (core semantics §4.6, §9.3; B-2026-11-05-39, -40).
            "ok_fn_field_to_bare_fn_param",
            "ok_shared_part_bindings_are_refs",
            // A tuple holds an explicit `ref name` binding as a reference
            // (B-2026-11-06-4).
            "ok_ref_pattern_binding_into_tuple",
            // `impl .. for Vec[T]` is for the library's `Vec`
            // (B-2026-11-06-12).
            "err_poly_recursion_through_dispatch",
            "ok_recursion_through_dispatch_peels",
            // `i64.parse` and `ParseError`, and `split(..).collect()`, are
            // the library's (B-2026-11-07-36, -54).
            "ok_par_parse_branches",
            "ok_compare_literal_in_some",
            // `iter`, `max`, `SortedSet` and `PriorityQueue` are the
            // library's (B-2026-11-08-24).
            "ok_option_none_orders_first",
            // `fold` and `enumerate` are iter.kara's (kata 2021).
            "ok_fold_tuple_reads_a_borrowed_scalar_as_its_value",
            // A `Str`'s range index is the library's (B-2026-11-08-66).
            "ok_a_slice_rebinds_to_its_own_reslice",
            // `==` on a map or set calls the library's `map_eq` and its kin
            // (B-2026-11-08-82).
            "ok_maps_and_sets_compare_by_content",
            // `Vec.into_iter` is the library's (B-2026-11-09-16).
            "ok_a_let_else_branch_moves_what_the_rest_moves",
            // A `collect()` kept by a `ref` binding is refused by the
            // checker's temporary rule (§5.5, B-2026-11-08-43).
            "err_collect_temporary_kept_by_ref_let",
            // `Iterator`, whose `next` declares the item type an impl
            // spells out, and `collect` are the library's (B-2026-11-10-7).
            "ok_spelled_item_type_borrows_like_self_item",
            "err_spelled_item_type_lends_from_receiver",
            // A view parameter's result (§5.4) is told by the build's own
            // `result_borrows`, and `String.as_str` is the library's
            // (B-2026-11-13-79).
            "err_view_param_returns_owned_field",
            "ok_view_param_results_live_together",
            "ok_view_param_through_struct_field",
            "ok_view_receiver_results_live_together",
            // `find`, `position`, `peekable` and `next_if` are iter.kara's
            // (B-2026-11-16-42).
            "ok_find_closure_loan_not_in_result",
            "ok_next_if_closure_loan_not_stored",
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
