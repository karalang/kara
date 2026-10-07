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

use super::pretty::place as show_place;
use super::syntax::*;
use super::ty::TyInterner;

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
    let loans = collect_loans(body);
    if loans.is_empty() {
        return Ok(());
    }
    let n_locals = body.locals.len();
    let entry: Origins = vec![vec![false; loans.len()]; n_locals];
    let origins = origins_dataflow(body, &loans, entry, has_receiver);
    let live_out = liveness(body);

    let mut errs = Vec::new();
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
                transfer_origins(&mut o, &loans, &block.statements[si].kind, at);
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

/// The origins of a value read from `place`: everything its local holds.
fn place_origins(o: &Origins, place: &Place, into: &mut [bool]) {
    for (d, s) in into.iter_mut().zip(&o[place.local.index()]) {
        *d |= *s;
    }
}

fn operand_origins(o: &Origins, op: &Operand, into: &mut [bool]) {
    if let Some(p) = op.place() {
        place_origins(o, p, into);
    }
}

fn assign_origins(o: &mut Origins, dest: &Place, val: Vec<bool>) {
    let slot = &mut o[dest.local.index()];
    if dest.projection.is_empty() {
        *slot = val;
    } else if !dest.projection.contains(&ProjElem::Deref) {
        // A part of the local: what the rest held stays.
        for (d, s) in slot.iter_mut().zip(val) {
            *d |= s;
        }
    }
    // A store through a reference changes what the pointee holds, which
    // this body only reaches through the reference: no local changes.
}

fn transfer_origins(o: &mut Origins, loans: &[Loan], s: &StatementKind, at: (usize, usize)) {
    let StatementKind::Assign(dest, rv) = s else {
        if let StatementKind::StorageDead(l) = s {
            o[l.index()].iter_mut().for_each(|b| *b = false);
        }
        return;
    };
    let mut val = vec![false; loans.len()];
    match rv {
        Rvalue::Ref(_, place) => {
            if let Some(li) = loans.iter().position(|l| l.at == at) {
                val[li] = true;
            }
            // A reborrow through a reference keeps that reference's loans.
            if place.projection.contains(&ProjElem::Deref) {
                place_origins(o, place, &mut val);
            }
        }
        Rvalue::Use(op) | Rvalue::UnaryOp(_, op) | Rvalue::Cast(_, op, _) => {
            operand_origins(o, op, &mut val)
        }
        Rvalue::Aggregate(_, ops) => {
            for op in ops {
                operand_origins(o, op, &mut val);
            }
        }
        Rvalue::Retain(p) => place_origins(o, p, &mut val),
        Rvalue::BinaryOp(..)
        | Rvalue::CheckedBinaryOp(..)
        | Rvalue::Discriminant(_)
        | Rvalue::Len(_) => {}
    }
    assign_origins(o, dest, val);
}

/// Forward dataflow of origins; `None` for an unreachable block.
fn origins_dataflow(
    body: &Body,
    loans: &[Loan],
    entry: Origins,
    has_receiver: &dyn Fn(&InstanceId) -> bool,
) -> Vec<Option<Origins>> {
    let mut states: Vec<Option<Origins>> = vec![None; body.blocks.len()];
    states[0] = Some(entry);
    let mut work = vec![0usize];
    while let Some(bi) = work.pop() {
        let mut o = states[bi].clone().expect("queued blocks have a state");
        let block = &body.blocks[bi];
        for (si, s) in block.statements.iter().enumerate() {
            transfer_origins(&mut o, loans, &s.kind, (bi, si));
        }
        if let TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } = &block.terminator.kind
        {
            let mut val = vec![false; loans.len()];
            let receiver = matches!(
                func,
                Operand::Const(Const { kind: ConstKind::FnDef(callee), .. })
                    if !args.is_empty() && has_receiver(callee)
            );
            let from = if receiver { &args[..1] } else { &args[..] };
            for a in std::iter::once(func).chain(from) {
                operand_origins(&o, a, &mut val);
            }
            assign_origins(&mut o, destination, val);
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
}
