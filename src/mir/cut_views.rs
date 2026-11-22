//! The `String` locals a cut of a text fills that live only to be lent
//! (B-2026-11-21-7): codegen points the `String` at the cut bytes of the
//! text it was cut from, with no capacity, rather than copy them into an
//! allocation. `w.push_str("abcde".substring(k, k + 1))` allocated and
//! freed a one-char `String` per call.
//!
//! A local other than the result and the arguments qualifies when its type
//! is `String` and every mention of it is a `substring` or `index_range`
//! call that fills it, a shared borrow of the whole local, its drop, or its
//! storage markers, as [`super::char_strs`] asks of a char's `to_string()`.
//! Never moved or mutated, its value cannot leave the frame, no borrow of
//! it outlives it, and with no capacity its drop frees nothing.
//!
//! The text it views must then stay as it is for as long as the local is
//! live. Each fill's text is a constant, the referent of a shared reference
//! argument (frozen by the caller's loan for the whole call), or a place
//! rooted at a local of this body that nothing writes, moves, mutably
//! borrows, drops or ends between the fill and the local's last mention.

use super::place_ty::place_ty;
use super::rc_pairs::{statement_mentions, terminator_mentions};
use super::syntax::*;
use super::ty::{IntrinsicTy, TyInterner, TyKind};

/// Whether each local of `body` is such a `String`.
pub fn borrowed_cuts(body: &Body, tys: &TyInterner) -> Vec<bool> {
    let n = body.locals.len();
    let mut fills: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (bi, b) in body.blocks.iter().enumerate() {
        if let TerminatorKind::Call {
            func, destination, ..
        } = &b.terminator.kind
        {
            if is_cut(func) && destination.projection.is_empty() {
                fills[destination.local.index()].push(bi);
            }
        }
    }
    let mut out = vec![false; n];
    for (l, at) in fills.iter().enumerate() {
        // The result leaves by `Return`, which names no local, and an
        // argument's slot is the caller's.
        if l <= body.arg_count || at.is_empty() {
            continue;
        }
        if !matches!(
            tys.kind(body.locals[l].ty),
            TyKind::Intrinsic(IntrinsicTy::String)
        ) {
            continue;
        }
        let local = Local(l as u32);
        let lent = body.blocks.iter().all(|b| {
            terminator_ok(&b.terminator.kind, local)
                && b.statements.iter().all(|s| statement_ok(&s.kind, local))
        });
        out[l] = lent && at.iter().all(|&bi| text_stays(body, tys, bi, local));
    }
    out
}

fn is_cut(func: &Operand) -> bool {
    matches!(
        func,
        Operand::Const(Const { kind: ConstKind::FnDef(i), .. })
            if matches!(i.name.as_str(), "String.substring" | "String.index_range")
    )
}

fn terminator_ok(t: &TerminatorKind, l: Local) -> bool {
    if !terminator_mentions(t, l) {
        return true;
    }
    match t {
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => {
            is_cut(func)
                && *destination == Place::local(l)
                && args.iter().all(|a| a.place().is_none_or(|p| p.local != l))
        }
        TerminatorKind::Drop { place, .. } => *place == Place::local(l),
        _ => false,
    }
}

fn statement_ok(s: &StatementKind, l: Local) -> bool {
    if !statement_mentions(s, l) {
        return true;
    }
    match s {
        StatementKind::Assign(d, Rvalue::Ref(BorrowKind::Shared, p)) => {
            d.local != l && *p == Place::local(l)
        }
        StatementKind::StorageLive(_) | StatementKind::StorageDead(_) => true,
        _ => false,
    }
}

/// Whether the text the cut ending block `fill` reads stays unchanged for
/// as long as `l` is live.
fn text_stays(body: &Body, tys: &TyInterner, fill: usize, l: Local) -> bool {
    let TerminatorKind::Call { args, .. } = &body.blocks[fill].terminator.kind else {
        return false;
    };
    let Some(text) = args.first() else {
        return false;
    };
    let q = match text {
        Operand::Const(_) => return true,
        Operand::Copy(q) | Operand::Move(q) if q.projection.is_empty() => q.local,
        _ => return false,
    };
    // The text is a shared reference argument itself, or a temporary
    // assigned once, from a shared borrow.
    let place = if is_arg(body, q) {
        Place {
            local: q,
            projection: vec![ProjElem::Deref],
        }
    } else {
        match only_def(body, q) {
            Some(p) => p,
            None => return false,
        }
    };
    match root(body, tys, &place) {
        Root::Frozen => true,
        Root::Watch(x) => untouched(body, tys, fill, l, x),
        Root::Unknown => false,
    }
}

fn is_arg(body: &Body, q: Local) -> bool {
    (1..=body.arg_count).contains(&q.index())
}

/// The place `q` borrows when its only definition is `q = &place`.
fn only_def(body: &Body, q: Local) -> Option<Place> {
    let mut def = None;
    for b in &body.blocks {
        for s in &b.statements {
            if let StatementKind::Assign(d, rv) = &s.kind {
                if d.local != q {
                    continue;
                }
                match (d.projection.is_empty(), rv, &def) {
                    (true, Rvalue::Ref(BorrowKind::Shared, p), None) => def = Some(p.clone()),
                    _ => return None,
                }
            }
        }
        if let TerminatorKind::Call { destination, .. } = &b.terminator.kind {
            if destination.local == q {
                return None;
            }
        }
    }
    def
}

enum Root {
    /// Behind a shared reference argument: the caller's loan freezes it.
    Frozen,
    /// Owned by this local, which must be left alone.
    Watch(Local),
    Unknown,
}

fn root(body: &Body, tys: &TyInterner, place: &Place) -> Root {
    let derefs: Vec<usize> = (0..place.projection.len())
        .filter(|&i| place.projection[i] == ProjElem::Deref)
        .collect();
    if derefs.is_empty() {
        return Root::Watch(place.local);
    }
    // Every reference on the way is shared, and the first is an argument:
    // a reference held in a local of this body may stop being used while
    // its referent's owner changes it.
    let mut first_mut = false;
    for (k, &i) in derefs.iter().enumerate() {
        let prefix = Place {
            local: place.local,
            projection: place.projection[..i].to_vec(),
        };
        let Ok(t) = place_ty(body, tys, &prefix) else {
            return Root::Unknown;
        };
        match tys.kind(t.ty) {
            TyKind::Ref(_) => {}
            TyKind::MutRef(_) if k == 0 && i == 0 => first_mut = true,
            _ => return Root::Unknown,
        }
    }
    if derefs[0] != 0 || !is_arg(body, place.local) {
        return Root::Unknown;
    }
    if first_mut {
        // A `mut ref` argument: writes to its referent go through it.
        Root::Watch(place.local)
    } else {
        Root::Frozen
    }
}

/// Whether nothing changes `x` in a block reachable from the cut that
/// reaches a mention of `l`.
fn untouched(body: &Body, tys: &TyInterner, fill: usize, l: Local, x: Local) -> bool {
    let nb = body.blocks.len();
    let mut fwd = vec![false; nb];
    let mut stack: Vec<usize> = body.blocks[fill]
        .terminator
        .kind
        .successors()
        .iter()
        .map(|b| b.index())
        .collect();
    while let Some(b) = stack.pop() {
        if !std::mem::replace(&mut fwd[b], true) {
            stack.extend(
                body.blocks[b]
                    .terminator
                    .kind
                    .successors()
                    .iter()
                    .map(|s| s.index()),
            );
        }
    }
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); nb];
    for (bi, b) in body.blocks.iter().enumerate() {
        for s in b.terminator.kind.successors() {
            preds[s.index()].push(bi);
        }
    }
    let mut back = vec![false; nb];
    let mut stack: Vec<usize> = (0..nb)
        .filter(|&bi| {
            let b = &body.blocks[bi];
            terminator_mentions(&b.terminator.kind, l)
                || b.statements.iter().any(|s| statement_mentions(&s.kind, l))
        })
        .collect();
    while let Some(b) = stack.pop() {
        if !std::mem::replace(&mut back[b], true) {
            stack.extend(preds[b].iter().copied());
        }
    }
    let shared_ref = matches!(tys.kind(body.locals[x.index()].ty), TyKind::Ref(_));
    (0..nb).filter(|&b| fwd[b] && back[b]).all(|bi| {
        let b = &body.blocks[bi];
        b.statements
            .iter()
            .all(|s| reads_only(&s.kind, x, shared_ref))
            && terminator_reads_only(&b.terminator.kind, x, shared_ref)
    })
}

/// Whether operand `o` only reads `x`: a copy, or a move of `x` when it is
/// itself a shared reference.
fn read_of(o: &Operand, x: Local, shared_ref: bool) -> bool {
    match o {
        Operand::Copy(_) => true,
        Operand::Move(p) => p.local != x || (shared_ref && p.projection.is_empty()),
        Operand::Const(_) => true,
    }
}

fn reads_only(s: &StatementKind, x: Local, shared_ref: bool) -> bool {
    if !statement_mentions(s, x) {
        return true;
    }
    match s {
        StatementKind::Assign(d, Rvalue::Ref(BorrowKind::Shared, p)) => {
            d.local != x && p.local == x
        }
        StatementKind::Assign(d, Rvalue::Use(o)) => d.local != x && read_of(o, x, shared_ref),
        _ => false,
    }
}

fn terminator_reads_only(t: &TerminatorKind, x: Local, shared_ref: bool) -> bool {
    if !terminator_mentions(t, x) {
        return true;
    }
    match t {
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => {
            read_of(func, x, shared_ref)
                && args.iter().all(|a| read_of(a, x, shared_ref))
                && destination.local != x
        }
        TerminatorKind::SwitchInt { discr, .. } => read_of(discr, x, shared_ref),
        _ => false,
    }
}
