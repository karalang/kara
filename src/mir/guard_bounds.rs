//! Index calls whose upper bound a guard already proved (B-2026-10-28-33).
//!
//! `v[i]` lowers to `Vec[T].index(&(*r), i as usize)`, which codegen checks
//! as `i as usize <u len`: one compare covers both `i < 0` and `i >= len`.
//! In `while lo >= 0 and hi < n and v[lo] == v[hi]` with `n = v.len()`,
//! the guard has just proved `hi <s n`, but LLVM does not see `hi >= 0`, so
//! the unsigned compare stays in the loop. With the upper half known, the
//! check that is left is `i >= 0`, which LLVM drops for a counter it can
//! see never goes negative, as the legacy backend's `vidx.oob.neg` was.
//!
//! The shape, for an index call in block `B` on the vector `X` at `h as
//! usize`, `h` a signed local:
//!
//! - `X` is `*r` with `r` a shared reference no statement writes after its
//!   one assignment (a parameter has none), so `X`'s length cannot change
//!   while `r` lives;
//! - `n` is assigned once in the body, by `len` on `&X` with the index's
//!   receiver type;
//! - walking up from `B` through blocks with one predecessor, an edge taken
//!   only when a `bool` `c` holds, where `c` implies `h <s n` (`c = Lt(h, n)`,
//!   or a copy or `&` of one, through the arms of a join that set it), and
//!   nothing writes `h` between that compare and the call.
//!
//! Then `h <s n = len`, so `h as usize <u len` exactly when `h >= 0`.

use super::syntax::*;
use super::ty::{TyInterner, TyKind};

/// How far a proof walks: blocks up from the call, and joins into a flag.
const WALK: usize = 32;
const JOIN_DEPTH: usize = 4;

/// The blocks of `body` whose terminator is an index call whose upper
/// bound is proved; codegen checks only the lower one there.
pub fn upper_proven_indexes(body: &Body, tys: &TyInterner) -> Vec<usize> {
    let preds = super::counters::predecessors(body);
    let mut out = Vec::new();
    for (bi, b) in body.blocks.iter().enumerate() {
        if proven(body, tys, &preds, bi, b).is_some() {
            out.push(bi);
        }
    }
    out
}

fn proven(
    body: &Body,
    tys: &TyInterner,
    preds: &[Vec<usize>],
    bi: usize,
    b: &BasicBlockData,
) -> Option<()> {
    let TerminatorKind::Call { func, args, .. } = &b.terminator.kind else {
        return None;
    };
    let name = fn_name(func)?;
    let (base, method) = name.rsplit_once('.')?;
    if !matches!(method, "index" | "index_mut") || args.len() != 2 {
        return None;
    }
    let recv = operand_local(&args[0])?;
    let idx = operand_local(&args[1])?;
    let (_, x) = defining(b, b.statements.len(), recv, |rv| match rv {
        Rvalue::Ref(_, p) => Some(p.clone()),
        _ => None,
    })?;
    let (ci, h) = defining(b, b.statements.len(), idx, |rv| match rv {
        Rvalue::Cast(CastKind::IntToInt, Operand::Copy(p), _) if p.projection.is_empty() => {
            Some(p.local)
        }
        _ => None,
    })?;
    match tys.kind(body.locals[h.index()].ty) {
        TyKind::Int(i) if i.signed() => {}
        _ => return None,
    }
    if !frozen(body, tys, &x) {
        return None;
    }
    // The value the call casts is the one the guard compared.
    if b.statements[..ci].iter().any(|s| writes(s, h)) {
        return None;
    }
    let n = length_of(body, base, &x)?;
    // Up from the call, through one-predecessor blocks.
    let mut w = bi;
    for _ in 0..WALK {
        let [p] = preds[w].as_slice() else {
            return None;
        };
        let p = *p;
        let pb = &body.blocks[p];
        match &pb.terminator.kind {
            TerminatorKind::Goto { .. } | TerminatorKind::Drop { .. } => {}
            TerminatorKind::Call { destination, .. } => {
                if destination.local == h {
                    return None;
                }
            }
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(c) | Operand::Move(c),
                targets,
            } if c.projection.is_empty() => {
                let taken_when_set = matches!(targets.values.as_slice(), [(0, other)]
                    if targets.otherwise.index() == w && other.index() != w);
                if taken_when_set {
                    let facts = implied(body, preds, p, pb.statements.len(), c.local, 0);
                    if facts.is_none_or(|f| f.contains(&(h, n))) {
                        return Some(());
                    }
                }
            }
            _ => return None,
        }
        if pb.statements.iter().any(|s| writes(s, h)) {
            return None;
        }
        w = p;
    }
    None
}

/// What `c` holding at statement `k` of block `bi` implies: `(a, b)` for
/// each `a <s b` it carries, `None` when `c` cannot hold there.
fn implied(
    body: &Body,
    preds: &[Vec<usize>],
    bi: usize,
    k: usize,
    c: Local,
    depth: usize,
) -> Option<Vec<(Local, Local)>> {
    let b = &body.blocks[bi];
    let found = b.statements[..k]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(j, s)| match &s.kind {
            StatementKind::Assign(p, rv) if p.local == c => Some((j, p.projection.is_empty(), rv)),
            _ => None,
        });
    let unchanged = |facts: Vec<(Local, Local)>, from: usize| {
        facts
            .into_iter()
            .filter(|&(x, y)| {
                !b.statements[from..k]
                    .iter()
                    .any(|s| writes(s, x) || writes(s, y))
            })
            .collect::<Vec<_>>()
    };
    match found {
        Some((_, false, _)) => Some(Vec::new()),
        Some((j, true, rv)) => match rv {
            Rvalue::BinaryOp(BinOp::Lt, Operand::Copy(x), Operand::Copy(y))
                if x.projection.is_empty() && y.projection.is_empty() =>
            {
                Some(unchanged(vec![(x.local, y.local)], j + 1))
            }
            Rvalue::BinaryOp(BinOp::Gt, Operand::Copy(y), Operand::Copy(x))
                if x.projection.is_empty() && y.projection.is_empty() =>
            {
                Some(unchanged(vec![(x.local, y.local)], j + 1))
            }
            Rvalue::Use(Operand::Copy(d) | Operand::Move(d)) if d.projection.is_empty() => {
                let f = implied(body, preds, bi, j, d.local, depth)?;
                Some(unchanged(f, j + 1))
            }
            Rvalue::Use(Operand::Const(Const {
                kind: ConstKind::Scalar(0),
                ..
            })) => None,
            Rvalue::BinaryOp(BinOp::BitAnd, Operand::Copy(l), Operand::Copy(r))
                if l.projection.is_empty() && r.projection.is_empty() =>
            {
                let mut f = implied(body, preds, bi, j, l.local, depth)?;
                f.extend(implied(body, preds, bi, j, r.local, depth)?);
                Some(unchanged(f, j + 1))
            }
            _ => Some(Vec::new()),
        },
        // Set in each block that reaches this one.
        None if depth < JOIN_DEPTH && !preds[bi].is_empty() => {
            let mut all: Option<Vec<(Local, Local)>> = None;
            for &q in &preds[bi] {
                if q >= bi {
                    // A back edge: what the loop's latch sets is not seen.
                    return Some(Vec::new());
                }
                let qb = &body.blocks[q];
                if !matches!(qb.terminator.kind, TerminatorKind::Goto { .. }) {
                    return Some(Vec::new());
                }
                let Some(f) = implied(body, preds, q, qb.statements.len(), c, depth + 1) else {
                    continue;
                };
                all = Some(match all {
                    None => f,
                    Some(a) => a.into_iter().filter(|x| f.contains(x)).collect(),
                });
            }
            Some(unchanged(all?, 0))
        }
        None => Some(Vec::new()),
    }
}

/// The local `n` assigned once in the body, by `{base}.len(&x)`.
fn length_of(body: &Body, base: &str, x: &Place) -> Option<Local> {
    let want = format!("{base}.len");
    let mut found = None;
    for b in &body.blocks {
        let TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } = &b.terminator.kind
        else {
            continue;
        };
        if fn_name(func) != Some(want.as_str()) || !destination.projection.is_empty() {
            continue;
        }
        let [a] = args.as_slice() else { continue };
        let Some(r) = operand_local(a) else { continue };
        let Some((_, p)) = defining(b, b.statements.len(), r, |rv| match rv {
            Rvalue::Ref(BorrowKind::Shared, p) => Some(p.clone()),
            _ => None,
        }) else {
            continue;
        };
        if p == *x && assigned_once(body, destination.local) {
            found = Some(destination.local);
        }
    }
    found
}

/// Whether `x` is `*r` for a shared reference `r` written at most once.
fn frozen(body: &Body, tys: &TyInterner, x: &Place) -> bool {
    let [ProjElem::Deref] = x.projection.as_slice() else {
        return false;
    };
    let r = x.local;
    let param = (1..=body.arg_count).contains(&r.index());
    matches!(tys.kind(body.locals[r.index()].ty), TyKind::Ref(_))
        && assigns(body, r) == usize::from(!param)
}

fn assigned_once(body: &Body, l: Local) -> bool {
    assigns(body, l) == 1
}

/// How many statements and calls write `l`, or more than one for any
/// other write (a projection, a `&mut`).
fn assigns(body: &Body, l: Local) -> usize {
    let mut n = 0;
    for b in &body.blocks {
        for s in &b.statements {
            if let StatementKind::Assign(p, rv) = &s.kind {
                if p.local == l {
                    n += if p.projection.is_empty() { 1 } else { 2 };
                }
                if matches!(rv, Rvalue::Ref(BorrowKind::Mut, q) if q.local == l) {
                    n += 2;
                }
            }
        }
        if let TerminatorKind::Call { destination, .. } = &b.terminator.kind {
            if destination.local == l {
                n += 1;
            }
        }
    }
    n
}

/// The last statement of `b` before `k` that assigns `l` whole, with what
/// `pick` makes of its value.
fn defining<T>(
    b: &BasicBlockData,
    k: usize,
    l: Local,
    pick: impl Fn(&Rvalue) -> Option<T>,
) -> Option<(usize, T)> {
    b.statements[..k]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(j, s)| match &s.kind {
            StatementKind::Assign(p, rv) if p.local == l => Some((j, p, rv)),
            _ => None,
        })
        .and_then(|(j, p, rv)| {
            if !p.projection.is_empty() {
                return None;
            }
            Some((j, pick(rv)?))
        })
}

/// Whether `s` writes `l`, whole, through a projection or by `&mut`.
fn writes(s: &Statement, l: Local) -> bool {
    match &s.kind {
        StatementKind::Assign(p, rv) => {
            p.local == l || matches!(rv, Rvalue::Ref(BorrowKind::Mut, q) if q.local == l)
        }
        StatementKind::SetDiscriminant(p, _) => p.local == l,
        _ => false,
    }
}

fn operand_local(o: &Operand) -> Option<Local> {
    match o {
        Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => Some(p.local),
        _ => None,
    }
}

fn fn_name(func: &Operand) -> Option<&str> {
    match func {
        Operand::Const(Const {
            kind: ConstKind::FnDef(inst),
            ..
        }) => Some(inst.name.as_str()),
        _ => None,
    }
}
