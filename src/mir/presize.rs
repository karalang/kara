//! Reserves the room a counting loop's pushes will take.
//!
//! `let mut v = Vec.new()` filled by `while i < n { ...; v.push(x); ... }`
//! grows `0, 4, 8, 16, ...` in a malloc, copy and free per step. When the
//! loop pushes exactly once per trip, the trips are known on entry, and
//! nothing else touches `v` on the way in, a `Vec[T].presize(&mut v, n,
//! delta)` right after `Vec.new()` reserves `n + delta` elements (clamped
//! to `0..=` a limit) up front. It is a capacity hint: the pushes still
//! grow `v` if the count is off, so output never changes. The legacy
//! backend does the same on the AST (`src/presize.rs`, where the patterns
//! and the measurements are). A `String.new()` filled by `String.push`
//! gets the same, `String.presize`, in bytes: each char is at least one.
//!
//! The shape, on an elaborated body, for a loop with no loop inside it:
//!
//! - a guard block `G` that dominates every latch and leaves the loop when
//!   `i < bound` (or `i <= bound`) is false, and no other way out of the
//!   loop than `G`'s exit and aborts;
//! - `i` an integer local the loop writes once, `i = i + 1`, checked, in a
//!   block that dominates every latch, and whose last write on the way in
//!   is `i = k` (`k >= 0`); writes elsewhere (a reset for another loop)
//!   do not matter;
//! - `bound` a literal, or a local written once (or a parameter never
//!   written) where that write dominates `Vec.new()`, or one neither the
//!   loop nor the blocks after `Vec.new()` write, so it holds the same
//!   value there and at `G`; a bound `n - c` the guard works out on every
//!   trip reserves for `n`, less `c`;
//! - every path through one trip, from the header to a latch, runs exactly
//!   one `Vec.push(&mut v, _)` and none before `G`; inside the loop `v` is
//!   otherwise only borrowed shared;
//! - `v = Vec.new()` (or `Vec.with_capacity(n)`) on the chain of
//!   single-predecessor blocks before the loop, with nothing between it
//!   and the loop mentioning `v` but pushes (seeds, each one more element).
//!
//! Then the loop runs `bound - k` trips (`+ 1` for `<=`), one push each.
//! A start that is a local `s` rather than a constant counts when the
//! bound is `s + n` (`for i in low..low + k`): the loop then runs `n`
//! trips, and `n` is reserved for.
//! A vector with no `Vec.new()` on the chain (a second fill of it) is
//! reserved for the trips at the loop's way in instead.
//!
//! The reservation returns the length it leaves room for, and the loop's
//! pushes become `Vec.__push_within(&mut v, x, reach)`: inside the loop
//! `v` is only read besides them, so below `reach` there is room, and the
//! compiled push skips its capacity check there, which lets LLVM drop the
//! growth path from the fill (a fill of one value becomes a `memset`).
//!
//! A `for c in s.chars()` loop pushing once a trip is reserved `s.len()`
//! more, right after the iterator is made (see [`plan_chars`]).

use super::counters::{
    dominates, dominators, increment, local_writes, mask, natural_loops, predecessors, Loop, Writes,
};
use super::rc_pairs::{statement_mentions, terminator_mentions};
use super::syntax::*;
use super::ty::{IntrinsicTy, Ty, TyInterner, TyKind};
use crate::ids::DefId;
use crate::mir::interp::split_method;

/// The largest literal trip count a reservation is made for, as legacy's
/// `MAX_LITERAL_BOUND`: a loop counting to a big constant may well stop
/// early, and the reservation would be held for nothing.
const MAX_LITERAL_ROOM: i128 = 1 << 20;

/// The most bytes a reservation asks for.
const MAX_ROOM_BYTES: i128 = 1 << 30;

/// The elements `presize(v, bound, delta)` reserves for elements of
/// `size` bytes: `bound + delta`, at least 0 and at most
/// [`MAX_ROOM_BYTES`] worth.
pub fn room(bound: i128, delta: i128, size: u64) -> i128 {
    let most = MAX_ROOM_BYTES / i128::from(size.max(1));
    bound.saturating_add(delta).clamp(0, most)
}

struct Plan {
    /// The block whose terminator is `v = Vec.new()`.
    new_block: usize,
    v: Local,
    bound: Operand,
    delta: i128,
    /// The loop's blocks that end in the push.
    fills: Vec<usize>,
    /// A text whose `len()` is the bound, worked out where the
    /// reservation is made (`plan_chars`), and the type of a borrow of it.
    len_of: Option<(Place, Ty)>,
    /// What [`version`] needs to copy the loop for a reservation that
    /// covers every trip.
    version: Option<Version>,
}

/// A presized loop [`version`] can copy.
struct Version {
    /// The loop's blocks.
    blocks: Vec<bool>,
    header: usize,
    /// The seed pushes `delta` counts.
    seeds: i128,
    /// The counter and the block of its unchecked `i = i + 1`, when the
    /// guard `i < bound` runs before the step on every trip: the step
    /// then cannot overflow, and the copy takes it as a checked add,
    /// which the backend emits as `add nsw` (B-2026-11-09-26).
    step: Option<(Local, usize)>,
}

/// Adds a `presize` after each `Vec.new()` a counting loop fills, and
/// after the iterator of each chars walk that fills a vector. With
/// `copies`, a fill whose reservation can cover every trip also gets the
/// loop copy [`version`] makes; auto-par's recording build leaves it out,
/// since each copy would be a loop of its own to judge.
pub fn presize_pushes(body: &mut Body, tys: &TyInterner, copies: bool) {
    if body.blocks.is_empty() {
        return;
    }
    let preds = predecessors(body);
    let idom = dominators(body, &preds);
    let loops = natural_loops(body, &preds, &idom);
    let writes = local_writes(body);
    let mut plans: Vec<Plan> = Vec::new();
    for lp in &loops {
        if loops
            .iter()
            .any(|o| o.header != lp.header && lp.blocks[o.header])
        {
            continue;
        }
        let mut found = plan(body, tys, &preds, &idom, lp, &writes);
        if found.is_empty() {
            found = plan_chars(body, tys, &preds, lp);
        }
        for p in found {
            if !plans.iter().any(|q| q.new_block == p.new_block) {
                plans.push(p);
            }
        }
    }
    for mut p in plans {
        if !copies {
            p.version = None;
        }
        apply(body, tys, p);
    }
}

/// The local `t` in `t = &mut v` (or `&v`) for a borrow of a whole local.
fn borrow_of(s: &Statement) -> Option<(Local, BorrowKind, Local)> {
    match &s.kind {
        StatementKind::Assign(t, Rvalue::Ref(k, q))
            if t.projection.is_empty() && q.projection.is_empty() =>
        {
            Some((t.local, *k, q.local))
        }
        _ => None,
    }
}

/// Whether block `b` ends in `Vec.push(move t, _)` with `t = &mut v` in it.
fn pushes(body: &Body, b: usize, v: Local) -> bool {
    let data = &body.blocks[b];
    let TerminatorKind::Call { func, args, .. } = &data.terminator.kind else {
        return false;
    };
    let Some(name) = fn_name(func) else {
        return false;
    };
    let (base, method) = split_method(name);
    if method != "push" || !fills(base) {
        return false;
    }
    let Some(Operand::Move(t)) = args.first() else {
        return false;
    };
    t.projection.is_empty()
        && data
            .statements
            .iter()
            .any(|s| borrow_of(s) == Some((t.local, BorrowKind::Mut, v)))
}

/// Whether `base` names a collection this pass reserves for: a `Vec`, or a
/// `String`, whose `push` appends one char (at least one byte, so a
/// reservation of the trip count is a floor, as legacy's was).
fn fills(base: &str) -> bool {
    base.starts_with("Vec[") || base == "String"
}

fn fn_name(func: &Operand) -> Option<&str> {
    match func {
        Operand::Const(Const {
            kind: ConstKind::FnDef(i),
            ..
        }) => Some(i.name.as_str()),
        _ => None,
    }
}

/// Whether block `b` mentions `v` only as a push's `&mut v` or a shared
/// borrow (`push_ok`: pushes allowed).
fn mentions_only_reads(body: &Body, b: usize, v: Local, push_ok: bool) -> bool {
    let data = &body.blocks[b];
    let push = push_ok && pushes(body, b, v);
    let mut pushed = false;
    for s in &data.statements {
        if !statement_mentions(&s.kind, v) {
            continue;
        }
        match borrow_of(s) {
            Some((_, BorrowKind::Shared, q)) if q == v => {}
            Some((t, BorrowKind::Mut, q)) if q == v && push && !pushed => {
                let TerminatorKind::Call { args, .. } = &data.terminator.kind else {
                    return false;
                };
                if args.first() != Some(&Operand::Move(Place::local(t))) {
                    return false;
                }
                pushed = true;
            }
            _ => return false,
        }
    }
    !terminator_mentions(&data.terminator.kind, v)
}

/// Whether block `b` leaves the loop exactly when `i == bound`: it ends in
/// `switchInt(c) -> [0: stay, otherwise: out]` with `c = Eq(copy i, bound)`,
/// or `c = Eq(copy i, MAX)` with `MAX` the largest value of `i`'s type, which
/// the guard `i <= bound` makes the same test (an inclusive range `for`
/// steps that way, B-2026-11-04-39).
fn leaves_at_bound(
    body: &Body,
    tys: &TyInterner,
    b: usize,
    i: Local,
    bound: &Operand,
    in_loop: &impl Fn(usize) -> bool,
) -> bool {
    let data = &body.blocks[b];
    let TerminatorKind::SwitchInt {
        discr: Operand::Copy(c),
        targets,
    } = &data.terminator.kind
    else {
        return false;
    };
    let [(0, stay)] = targets.values.as_slice() else {
        return false;
    };
    if !c.projection.is_empty() || !in_loop(stay.index()) || in_loop(targets.otherwise.index()) {
        return false;
    }
    let cmp = data.statements.iter().rev().find_map(|s| match &s.kind {
        StatementKind::Assign(d, rv) if d == c => Some(rv),
        _ => None,
    });
    let iv = Operand::Copy(Place::local(i));
    let top = match tys.kind(body.local(i).ty) {
        TyKind::Int(it) => Some(u128::MAX >> (128 - it.bits() + u32::from(it.signed()))),
        _ => None,
    };
    let at_top = |o: &Operand| matches!(o, Operand::Const(Const { kind: ConstKind::Scalar(v), .. }) if Some(*v) == top);
    matches!(cmp, Some(Rvalue::BinaryOp(BinOp::Eq, x, y))
        if (*x == iv && (y == bound || at_top(y))) || ((x == bound || at_top(x)) && *y == iv))
}

/// `l` when it holds at `Vec.new()` (block `nb`'s call) the value it holds
/// anywhere after: a parameter never written, or a local written once where
/// that write dominates the call.
fn settled(body: &Body, idom: &[usize], nb: usize, l: Local) -> Option<Local> {
    let ds = defs(body, l)?;
    let ok = match ds.as_slice() {
        [] => (1..=body.arg_count).contains(&l.index()),
        [(bi, Some(_))] => *bi == nb || dominates(idom, *bi, nb),
        [(bi, None)] => *bi != nb && dominates(idom, *bi, nb),
        _ => false,
    };
    ok.then_some(l)
}

/// `l` when every write of it is a whole-local one outside the loop and
/// outside the chain blocks between `Vec.new()`'s block `nb` and the loop
/// (`chain` is nearest the loop first): no path from `Vec.new()` to the
/// guard then writes it, so it holds one value along it.
fn unwritten_since(
    body: &Body,
    l: Local,
    nb: usize,
    chain: &[usize],
    in_loop: &impl Fn(usize) -> bool,
) -> Option<Local> {
    let ds = defs(body, l)?;
    let at = chain.iter().position(|&b| b == nb)?;
    let between = &chain[..at];
    let ok = !ds.is_empty()
        && ds
            .iter()
            .all(|&(bi, si)| !in_loop(bi) && !between.contains(&bi) && (bi != nb || si.is_some()));
    ok.then_some(l)
}

/// A bound `t.0` for `t = n + c` (or `n - c`), checked, with `c` a small
/// constant: `n` and the `c` that `n` is off by.
fn offset_bound(body: &Body, b: &Operand, bits: u32, signed: bool) -> Option<(Operand, i128)> {
    let Operand::Copy(p) = b else {
        return None;
    };
    if !p.projection.is_empty() {
        return None;
    }
    let Rvalue::Use(Operand::Copy(f)) = sole_assign(body, p.local)? else {
        return None;
    };
    let [ProjElem::Field(FieldIdx(0), _)] = f.projection.as_slice() else {
        return None;
    };
    let Rvalue::CheckedBinaryOp(op, Operand::Copy(n), Operand::Const(c)) =
        sole_assign(body, f.local)?
    else {
        return None;
    };
    let ConstKind::Scalar(raw) = c.kind else {
        return None;
    };
    if !n.projection.is_empty() {
        return None;
    }
    let c = signed_value(raw, bits, signed);
    if c.abs() > MAX_LITERAL_ROOM {
        return None;
    }
    let off = match op {
        BinOp::Sub => -c,
        BinOp::Add => c,
        _ => return None,
    };
    Some((Operand::Copy(n.clone()), off))
}

/// For a loop counting from `copy s` (written in chain block
/// `start_block`) to a `bound` that copies, through whole-local copies,
/// `t.0` of `t = s + n` (checked, or not, either order): `n`, when `s`
/// holds at the add what it holds at the start, and `n` holds there what
/// it holds where the reservation is made (chain block `nb`). Neither may
/// then be written in the loop, nor `s` in the chain from the nearer of
/// the two writes in, nor `n` from `nb` or the add, whichever is
/// farther from the loop.
fn counted_past(
    body: &Body,
    bound: &Operand,
    s: Local,
    start_block: usize,
    nb: usize,
    chain: &[usize],
    in_loop: &impl Fn(usize) -> bool,
) -> Option<Local> {
    let Operand::Copy(p) = bound else {
        return None;
    };
    if !p.projection.is_empty() {
        return None;
    }
    let mut l = p.local;
    let mut sum = None;
    for _ in 0..4 {
        let [(bi, Some(si))] = defs(body, l)?[..] else {
            return None;
        };
        let StatementKind::Assign(_, rv) = &body.blocks[bi].statements[si].kind else {
            return None;
        };
        match rv {
            Rvalue::Use(Operand::Copy(x)) if x.projection.is_empty() => l = x.local,
            Rvalue::Use(Operand::Copy(f)) => {
                let [ProjElem::Field(FieldIdx(0), _)] = f.projection.as_slice() else {
                    return None;
                };
                let [(ab, Some(asi))] = defs(body, f.local)?[..] else {
                    return None;
                };
                let StatementKind::Assign(_, Rvalue::CheckedBinaryOp(BinOp::Add, a, b)) =
                    &body.blocks[ab].statements[asi].kind
                else {
                    return None;
                };
                sum = Some((ab, a, b));
                break;
            }
            Rvalue::BinaryOp(BinOp::Add, a, b) => {
                sum = Some((bi, a, b));
                break;
            }
            _ => return None,
        }
    }
    let (add_block, a, b) = sum?;
    let whole = |o: &Operand| match o {
        Operand::Copy(x) if x.projection.is_empty() => Some(x.local),
        _ => None,
    };
    let n = match (whole(a)?, whole(b)?) {
        (x, n) if x == s => n,
        (n, x) if x == s => n,
        _ => return None,
    };
    let at = |b: usize| chain.iter().position(|&c| c == b);
    let (pa, ps, pn) = (at(add_block)?, at(start_block)?, at(nb)?);
    // No write of `x` in the loop, nor in the chain blocks from the loop
    // out to position `upto`.
    let still = |x: Local, upto: usize| {
        defs(body, x).is_some_and(|ds| {
            ds.iter()
                .all(|&(bi, _)| !in_loop(bi) && at(bi).is_none_or(|q| q > upto))
        })
    };
    (still(s, pa.max(ps)) && still(n, pa.max(pn))).then_some(n)
}

/// The one write of `l`, when it is a statement `l = rv`.
fn sole_assign(body: &Body, l: Local) -> Option<&Rvalue> {
    let [(bi, Some(si))] = defs(body, l)?[..] else {
        return None;
    };
    match &body.blocks[bi].statements[si].kind {
        StatementKind::Assign(_, rv) => Some(rv),
        _ => None,
    }
}

/// For `l` written once, by `l = copy x` with `x` a whole local, `f(x)`:
/// `l` then holds `x`'s value wherever `x` holds one value.
fn copied_from(body: &Body, l: Local, f: impl Fn(Local) -> Option<Local>) -> Option<Local> {
    match sole_assign(body, l)? {
        Rvalue::Use(Operand::Copy(x)) if x.projection.is_empty() => f(x.local),
        _ => None,
    }
}

/// The constant `l` is written once with, by `l = const k`.
fn copied_const(body: &Body, l: Local) -> Option<u128> {
    match sole_assign(body, l)? {
        Rvalue::Use(o) => int_const(o),
        _ => None,
    }
}

/// `l = l + c`, unchecked, as `l`'s one write other than its init: the
/// block holding it and `c`.
fn plain_increment(body: &Body, writes: &Writes, l: Local) -> Option<(usize, u128)> {
    writes.assigns[l.index()].iter().find_map(|&(bi, si)| {
        let StatementKind::Assign(_, Rvalue::BinaryOp(BinOp::Add, a, b)) =
            &body.blocks[bi].statements[si].kind
        else {
            return None;
        };
        match (a, b) {
            (Operand::Copy(p), Operand::Const(k)) | (Operand::Const(k), Operand::Copy(p))
                if *p == Place::local(l) =>
            {
                match k.kind {
                    ConstKind::Scalar(v) => Some((bi, v)),
                    _ => None,
                }
            }
            _ => None,
        }
    })
}

/// The writes of `l`: `Some(sites)` when each is a whole-local assignment
/// or call result, `None` when something else may write it.
fn defs(body: &Body, l: Local) -> Option<Vec<(usize, Option<usize>)>> {
    let mut out = Vec::new();
    for (bi, b) in body.blocks.iter().enumerate() {
        for (si, s) in b.statements.iter().enumerate() {
            match &s.kind {
                StatementKind::Assign(p, rv) => {
                    if p.local == l {
                        if !p.projection.is_empty() {
                            return None;
                        }
                        out.push((bi, Some(si)));
                    }
                    if let Rvalue::Ref(BorrowKind::Mut, q) = rv {
                        if q.local == l {
                            return None;
                        }
                    }
                }
                StatementKind::SetDiscriminant(p, _) if p.local == l => return None,
                _ => {}
            }
        }
        if let TerminatorKind::Call { destination, .. } = &b.terminator.kind {
            if destination.local == l {
                if !destination.projection.is_empty() {
                    return None;
                }
                out.push((bi, None));
            }
        }
    }
    Some(out)
}

fn int_const(o: &Operand) -> Option<u128> {
    match o {
        Operand::Const(Const {
            kind: ConstKind::Scalar(v),
            ..
        }) => Some(*v),
        _ => None,
    }
}

/// The value of an integer constant of `bits` bits, sign-extended when
/// `signed`.
fn signed_value(v: u128, bits: u32, signed: bool) -> i128 {
    let v = mask(v, bits);
    if signed && bits < 128 && v >> (bits - 1) == 1 {
        (v | !((1u128 << bits) - 1)) as i128
    } else {
        v as i128
    }
}

fn plan(
    body: &Body,
    tys: &TyInterner,
    preds: &[Vec<usize>],
    idom: &[usize],
    lp: &Loop,
    writes: &Writes,
) -> Vec<Plan> {
    let mut out = Vec::new();
    let n = body.blocks.len();
    let dominates_latches = |b: usize| lp.latches.iter().all(|&t| dominates(idom, b, t));
    let in_loop = |b: usize| lp.blocks[b];
    let is_abort = |b: usize| {
        matches!(
            body.blocks[b].terminator.kind,
            TerminatorKind::Abort { .. } | TerminatorKind::Unreachable
        )
    };
    // The guard, and no other way out.
    let mut guard = None;
    for g in (0..n).filter(|&g| in_loop(g) && dominates_latches(g)) {
        let TerminatorKind::SwitchInt {
            discr: Operand::Copy(c),
            targets,
        } = &body.blocks[g].terminator.kind
        else {
            continue;
        };
        let [(0, exit)] = targets.values.as_slice() else {
            continue;
        };
        if !c.projection.is_empty() || in_loop(exit.index()) || !in_loop(targets.otherwise.index())
        {
            continue;
        }
        let cmp = body.blocks[g]
            .statements
            .iter()
            .rev()
            .find_map(|s| match &s.kind {
                StatementKind::Assign(d, rv) if d == c => Some(rv),
                _ => None,
            });
        let Some(Rvalue::BinaryOp(op @ (BinOp::Lt | BinOp::Le), Operand::Copy(ip), bound)) = cmp
        else {
            continue;
        };
        if !ip.projection.is_empty() {
            continue;
        }
        guard = Some((g, *exit, *op, ip.local, bound.clone()));
        break;
    }
    let Some((g, exit, op, i, bound)) = guard else {
        return out;
    };
    // An inclusive range `for` also leaves after the trip `i == bound`,
    // the one that trip would have been the last of anyway.
    let mut last_exits = Vec::new();
    for b in (0..n).filter(|&b| in_loop(b)) {
        if matches!(body.blocks[b].terminator.kind, TerminatorKind::Return) {
            return out;
        }
        for s in body.blocks[b].terminator.kind.successors() {
            let s = s.index();
            if !in_loop(s) && !(b == g && s == exit.index()) && !is_abort(s) {
                if op != BinOp::Le || !leaves_at_bound(body, tys, b, i, &bound, &in_loop) {
                    return out;
                }
                last_exits.push(b);
            }
        }
    }
    // `i`: from a constant `k >= 0`, by one per trip.
    let TyKind::Int(it) = tys.kind(body.local(i).ty) else {
        return out;
    };
    let (bits, signed) = (it.bits(), it.signed());
    if bits > 64 || writes.other[i.index()] {
        return out;
    }
    // The loop writes `i` once, its step. Writes outside it (a counter
    // reset for a second loop, B-2026-10-31-42) do not reach a trip: the
    // way in is the chain, whose nearest write of `i` is its start.
    let inside: Vec<(usize, usize)> = writes.assigns[i.index()]
        .iter()
        .copied()
        .filter(|&(bi, _)| in_loop(bi))
        .collect();
    if inside.len() != 1 {
        return out;
    }
    let steps = Writes {
        assigns: {
            let mut a = writes.assigns.clone();
            a[i.index()] = inside;
            a
        },
        other: writes.other.clone(),
    };
    let unchecked = plain_increment(body, &steps, i)
        .filter(|&(cb, c)| {
            mask(c, bits) == 1 && op == BinOp::Lt && cb != g && dominates(idom, g, cb)
        })
        .map(|(cb, _)| (i, cb));
    let step = match increment(body, &steps, i) {
        Some((cb, _, c)) => mask(c, bits) == 1 && dominates_latches(cb),
        // A range `for` steps with an unchecked add: its guard keeps `i`
        // below the bound.
        None => plain_increment(body, &steps, i)
            .is_some_and(|(cb, c)| mask(c, bits) == 1 && dominates_latches(cb)),
    };
    if !step {
        return out;
    }
    let Some(chain) = chain_into(preds, lp) else {
        return out;
    };
    // The start: the last write of `i` on the chain, nearest the loop.
    let start = chain.iter().find_map(|&cb| {
        writes.assigns[i.index()]
            .iter()
            .filter(|&&(bi, _)| bi == cb)
            .max_by_key(|&&(_, si)| si)
            .copied()
    });
    let start_op = start.and_then(|(bi, si)| match &body.blocks[bi].statements[si].kind {
        StatementKind::Assign(_, Rvalue::Use(o)) => Some((bi, o)),
        _ => None,
    });
    // `i = copy s` from a local: the trips are counted only off a bound
    // `s + n` (below), and the loop starts at `s`, so at `k = 0` of `n`.
    let (k, from) = match start_op {
        Some((_, o)) if int_const(o).is_some() => (int_const(o).unwrap_or(0), None),
        Some((bi, Operand::Copy(p))) if p.projection.is_empty() => (0, Some((p.local, bi))),
        _ => return out,
    };
    let k = signed_value(k, bits, signed);
    if k < 0 {
        return out;
    }
    let vs = pushed_vecs(body, lp);
    let order = trip_order(body, lp);
    'v: for v in vs {
        if !matches!(
            tys.kind(body.local(v).ty),
            TyKind::Intrinsic(IntrinsicTy::Vec(_) | IntrinsicTy::String)
        ) {
            continue;
        }
        if !(0..n)
            .filter(|&b| in_loop(b))
            .all(|b| mentions_only_reads(body, b, v, true))
        {
            continue;
        }
        let Some(fill_blocks) = trip_fills(body, lp, &order, g, v, &last_exits) else {
            continue 'v;
        };
        // `v = Vec.new()` on the chain, and only seed pushes after it.
        let mut seeds = 0i128;
        let mut new_block = None;
        for &b in &chain {
            let data = &body.blocks[b];
            if let TerminatorKind::Call {
                func,
                destination,
                args,
                target: Some(_),
                ..
            } = &data.terminator.kind
            {
                if *destination == Place::local(v) {
                    // `with_capacity(n)` starts empty too: the reserve
                    // then asks for what `n` does not already hold
                    // (B-2026-11-02-26).
                    let fresh = fn_name(func).map(split_method).is_some_and(|(base, m)| {
                        fills(base)
                            && match m {
                                "new" => args.is_empty(),
                                "with_capacity" => args.len() == 1,
                                _ => false,
                            }
                    });
                    if !fresh {
                        continue 'v;
                    }
                    if data.statements.iter().any(|s| {
                        statement_mentions(&s.kind, v)
                            && !matches!(s.kind, StatementKind::StorageLive(_))
                    }) {
                        continue 'v;
                    }
                    new_block = Some(b);
                    break;
                }
            }
            if !mentions_only_reads(body, b, v, true) {
                continue 'v;
            }
            seeds += i128::from(pushes(body, b, v));
        }
        // A vector the loop goes on filling (a second run of pushes into
        // it, B-2026-10-31-42) is reserved for the loop's trips at the
        // way in.
        let mut nb = match new_block {
            Some(nb) => nb,
            None => {
                seeds = 0;
                chain[0]
            }
        };
        let adj = i128::from(op == BinOp::Le);
        let delta = seeds + adj - k;
        let mut seeds_in = seeds;
        // `for i in s..s + n`: the bound less the start is `n`
        // (B-2026-11-09-6).
        let bound = match from {
            Some((s, start_block)) => {
                match counted_past(body, &bound, s, start_block, nb, &chain, &in_loop) {
                    Some(n) => Operand::Copy(Place::local(n)),
                    None => continue,
                }
            }
            None => bound.clone(),
        };
        // `i < n - 1`: the guard works its bound out of a still `n` on
        // every trip, so `n` is reserved for, less the constant.
        let (bound, mut delta) = match offset_bound(body, &bound, bits, signed) {
            Some((b, c)) if from.is_none() => (b, delta + c),
            _ => (bound, delta),
        };
        // The bound, holding at `Vec.new()` what it holds at the guard.
        // A range `for` copies a literal bound into a local.
        // A named binding is a variable bound even when a literal sets
        // it (`let n = 2000000`), as it is on legacy's AST: the limit is
        // for a loop counting to a literal written in its own condition.
        let lit = match &bound {
            Operand::Const(_) => int_const(&bound),
            Operand::Copy(p)
                if p.projection.is_empty()
                    && !matches!(
                        body.local(p.local).kind,
                        LocalKind::User { .. } | LocalKind::Arg { .. }
                    ) =>
            {
                copied_const(body, p.local)
            }
            _ => None,
        };
        // (`if let` rather than a guard: the MSRV floor has no `if let` guards.)
        let bound = if let Some(raw) = lit {
            let room = signed_value(raw, bits, signed) + delta;
            if room <= 0 || room > MAX_LITERAL_ROOM {
                continue;
            }
            Operand::Const(Const {
                ty: tys.intern(TyKind::Int(super::ty::IntTy::I64)),
                kind: ConstKind::Scalar((room - delta) as u128),
            })
        } else {
            match &bound {
                Operand::Copy(p) if p.projection.is_empty() => {
                    // A bound the loop and the blocks after `Vec.new()`
                    // never write holds there what it holds at the guard
                    // too, however often it is written elsewhere: Pascal's
                    // row `while j <= i` under an outer `i = i + 1`
                    // (B-2026-11-01-21).
                    let (chain, in_loop) = (&chain, &in_loop);
                    let found = |at: usize| {
                        let held = |x: Local| {
                            settled(body, idom, at, x)
                                .or_else(|| unwritten_since(body, x, at, chain, in_loop))
                        };
                        held(p.local).or_else(|| copied_from(body, p.local, held))
                    };
                    // A bound worked out after `Vec.new()` (`for _ in
                    // 0..row.len()` with `seat_of` made first) holds its
                    // value at the loop's way in, so the vector is
                    // reserved for the trips there, past its seeds, as a
                    // second fill is (B-2026-11-09-10).
                    let b = match found(nb) {
                        Some(b) => b,
                        None if new_block.is_some() && chain[0] != nb => match found(chain[0]) {
                            Some(b) => {
                                nb = chain[0];
                                delta -= seeds;
                                seeds_in = 0;
                                b
                            }
                            None => continue,
                        },
                        None => continue,
                    };
                    if !matches!(tys.kind(body.local(b).ty), TyKind::Int(t) if t.bits() <= 64) {
                        continue;
                    }
                    Operand::Copy(Place::local(b))
                }
                _ => continue,
            }
        };
        out.push(Plan {
            new_block: nb,
            v,
            bound,
            delta,
            fills: fill_blocks,
            len_of: None,
            version: Some(Version {
                blocks: lp.blocks.clone(),
                header: lp.header,
                seeds: seeds_in,
                step: unchecked,
            }),
        });
    }
    out
}

/// The single-predecessor chain into loop `lp`, nearest block first, from
/// its one entry from outside.
fn chain_into(preds: &[Vec<usize>], lp: &Loop) -> Option<Vec<usize>> {
    let in_loop = |b: usize| lp.blocks[b];
    let outside: Vec<usize> = preds[lp.header]
        .iter()
        .copied()
        .filter(|&p| !in_loop(p))
        .collect();
    let [entry] = outside.as_slice() else {
        return None;
    };
    let mut chain = vec![*entry];
    while chain.len() < 16 {
        let last = *chain.last().expect("non-empty");
        match preds[last].as_slice() {
            [p] if *p != last && !in_loop(*p) && !chain.contains(p) => chain.push(*p),
            _ => break,
        }
    }
    Some(chain)
}

/// Each `v` some block of loop `lp` pushes to.
fn pushed_vecs(body: &Body, lp: &Loop) -> Vec<Local> {
    let mut vs: Vec<Local> = Vec::new();
    for b in (0..body.blocks.len()).filter(|&b| lp.blocks[b]) {
        for s in &body.blocks[b].statements {
            if let Some((_, BorrowKind::Mut, v)) = borrow_of(s) {
                if pushes(body, b, v) && !vs.contains(&v) {
                    vs.push(v);
                }
            }
        }
    }
    vs
}

/// Loop `lp`'s blocks in an order that has each after its predecessors
/// (edges back to the header left out).
fn trip_order(body: &Body, lp: &Loop) -> Vec<usize> {
    let in_loop = |b: usize| lp.blocks[b];
    let mut post = Vec::new();
    let mut seen = vec![false; body.blocks.len()];
    let mut stack = vec![(lp.header, 0usize)];
    seen[lp.header] = true;
    while let Some((b, k)) = stack.pop() {
        let succ: Vec<usize> = body.blocks[b]
            .terminator
            .kind
            .successors()
            .iter()
            .map(|s| s.index())
            .filter(|&s| in_loop(s) && s != lp.header)
            .collect();
        if k < succ.len() {
            stack.push((b, k + 1));
            let s = succ[k];
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(b);
        }
    }
    post.reverse();
    post
}

/// The blocks of loop `lp` that end in a push to `v`, when every path
/// through one trip (from the header to a latch, or to one of
/// `last_exits`) runs exactly one, and none runs before the guard `g`.
fn trip_fills(
    body: &Body,
    lp: &Loop,
    order: &[usize],
    g: usize,
    v: Local,
    last_exits: &[usize],
) -> Option<Vec<usize>> {
    let in_loop = |b: usize| lp.blocks[b];
    let mut fill_blocks = Vec::new();
    let mut range: Vec<Option<(u32, u32)>> = vec![None; body.blocks.len()];
    range[lp.header] = Some((0, 0));
    for &b in order {
        let (lo, hi) = range[b]?;
        if b == g && (lo, hi) != (0, 0) {
            return None;
        }
        let p = u32::from(pushes(body, b, v));
        if p == 1 {
            fill_blocks.push(b);
        }
        let (lo, hi) = (lo + p, hi + p);
        if (lp.latches.contains(&b) || last_exits.contains(&b)) && (lo, hi) != (1, 1) {
            return None;
        }
        for s in body.blocks[b].terminator.kind.successors() {
            let s = s.index();
            if !in_loop(s) || s == lp.header {
                continue;
            }
            range[s] = Some(match range[s] {
                None => (lo, hi),
                Some((a, z)) => (a.min(lo), z.max(hi)),
            });
        }
    }
    Some(fill_blocks)
}

/// `for c in s.chars() { ...; v.push(x); ... }`: a walk over a `String`'s
/// chars runs at most `s.len()` trips (a char is at least one byte), so a
/// loop pushing to `v` once on every trip has `v` reserved `s.len()` more
/// right after the iterator is made, as legacy's `iterable_len_bound`
/// does (B-2026-11-02-23). The shape:
///
/// - the header ends in `o = StrChars.next(move m)` with `m = &mut it` in
///   it, and the block after ends in a `switchInt` on `discriminant(o)`
///   with one target out of the loop: the guard, and with aborts the
///   loop's only way out;
/// - `it = String.chars(move r)`, with `r = &p` in its block, ends a block
///   `C` on the chain into the loop, and from `C` on nothing mentions `v`
///   before the loop;
/// - every path through one trip runs exactly one push to `v`, none before
///   the guard, and the loop otherwise only reads `v`.
///
/// `v` may hold anything at `C`: the reservation is for that many more.
fn plan_chars(body: &Body, tys: &TyInterner, preds: &[Vec<usize>], lp: &Loop) -> Vec<Plan> {
    let mut out = Vec::new();
    let n = body.blocks.len();
    let in_loop = |b: usize| lp.blocks[b];
    let h = lp.header;
    let TerminatorKind::Call {
        func,
        args,
        destination: o,
        target: Some(g),
        ..
    } = &body.blocks[h].terminator.kind
    else {
        return out;
    };
    if fn_name(func) != Some("StrChars.next") || !o.projection.is_empty() {
        return out;
    }
    let [Operand::Move(m)] = args.as_slice() else {
        return out;
    };
    let it = body.blocks[h]
        .statements
        .iter()
        .find_map(|s| match borrow_of(s) {
            Some((t, BorrowKind::Mut, it)) if t == m.local && m.projection.is_empty() => Some(it),
            _ => None,
        });
    let Some(it) = it else {
        return out;
    };
    // The guard: a test of `o`'s variant, one way out of the loop.
    let g = g.index();
    let TerminatorKind::SwitchInt {
        discr: Operand::Copy(d),
        ..
    } = &body.blocks[g].terminator.kind
    else {
        return out;
    };
    let tests_o = body.blocks[g].statements.iter().any(|s| {
        matches!(&s.kind, StatementKind::Assign(p, Rvalue::Discriminant(q))
            if p == d && *q == Place::local(o.local))
    });
    let succ: Vec<usize> = body.blocks[g]
        .terminator
        .kind
        .successors()
        .iter()
        .map(|s| s.index())
        .collect();
    let [a, b] = succ.as_slice() else {
        return out;
    };
    let exit = match (in_loop(*a), in_loop(*b)) {
        (true, false) => *b,
        (false, true) => *a,
        _ => return out,
    };
    if !tests_o || !in_loop(g) {
        return out;
    }
    let is_abort = |b: usize| {
        matches!(
            body.blocks[b].terminator.kind,
            TerminatorKind::Abort { .. } | TerminatorKind::Unreachable
        )
    };
    for b in (0..n).filter(|&b| in_loop(b)) {
        if matches!(body.blocks[b].terminator.kind, TerminatorKind::Return) {
            return out;
        }
        for s in body.blocks[b].terminator.kind.successors() {
            let s = s.index();
            if !in_loop(s) && !(b == g && s == exit) && !is_abort(s) {
                return out;
            }
        }
    }
    // `C`: the iterator made from a borrow of the text, on the chain.
    let Some(chain) = chain_into(preds, lp) else {
        return out;
    };
    let Some(at) = chain.iter().position(|&cb| {
        matches!(&body.blocks[cb].terminator.kind,
            TerminatorKind::Call { destination, target: Some(_), .. }
                if *destination == Place::local(it))
    }) else {
        return out;
    };
    let c = chain[at];
    let TerminatorKind::Call { func, args, .. } = &body.blocks[c].terminator.kind else {
        return out;
    };
    if fn_name(func) != Some("String.chars") {
        return out;
    }
    let [Operand::Move(r)] = args.as_slice() else {
        return out;
    };
    let text = body.blocks[c]
        .statements
        .iter()
        .find_map(|s| match &s.kind {
            StatementKind::Assign(t, Rvalue::Ref(BorrowKind::Shared, p))
                if *t == Place::local(r.local) && r.projection.is_empty() =>
            {
                Some(p.clone())
            }
            _ => None,
        });
    let Some(text) = text else {
        return out;
    };
    let order = trip_order(body, lp);
    for v in pushed_vecs(body, lp) {
        if !matches!(
            tys.kind(body.local(v).ty),
            TyKind::Intrinsic(IntrinsicTy::Vec(_) | IntrinsicTy::String)
        ) || v == it
            || text.local == v
        {
            continue;
        }
        let quiet = |b: usize| {
            !body.blocks[b]
                .statements
                .iter()
                .any(|s| statement_mentions(&s.kind, v))
                && !terminator_mentions(&body.blocks[b].terminator.kind, v)
        };
        if !chain[..=at].iter().all(|&b| quiet(b))
            || !(0..n)
                .filter(|&b| in_loop(b))
                .all(|b| mentions_only_reads(body, b, v, true))
        {
            continue;
        }
        let Some(fills) = trip_fills(body, lp, &order, g, v, &[]) else {
            continue;
        };
        out.push(Plan {
            new_block: c,
            v,
            // Set where the reservation is made, from `len_of`.
            bound: Operand::Const(Const {
                ty: tys.intern(TyKind::Int(super::ty::IntTy::I64)),
                kind: ConstKind::Scalar(0),
            }),
            delta: 0,
            fills,
            len_of: Some((text.clone(), body.local(r.local).ty)),
            version: None,
        });
    }
    out
}

/// `v = Vec.new() -> t` becomes `v = Vec.new() -> X; X: r = &mut v;
/// Vec.presize(move r, bound, delta) -> t`. A chars walk's reservation
/// follows `it = String.chars(..)` instead, after `t = &text;
/// n = String.len(move t)` works out its bound.
fn apply(body: &mut Body, tys: &TyInterner, mut p: Plan) {
    let vty = body.local(p.v).ty;
    // A copy of the loop needs the bound as an `i64` to add to the length
    // (a text's length is worked out here, and its fill is not a `Vec`'s).
    let i64t = tys.intern(TyKind::Int(super::ty::IntTy::I64));
    let versioned_bound = match &p.bound {
        Operand::Const(c) if c.ty == i64t => Some(p.bound.clone()),
        Operand::Copy(q) if q.projection.is_empty() && body.local(q.local).ty == i64t => {
            Some(p.bound.clone())
        }
        _ => None,
    }
    .filter(|_| p.len_of.is_none());
    let x = BasicBlock(body.blocks.len() as u32);
    let info = body.blocks[p.new_block].terminator.source_info;
    // After `Vec.new()`'s call, or on a filled vector's way into the loop.
    let target = match &mut body.blocks[p.new_block].terminator.kind {
        TerminatorKind::Call {
            target: Some(target),
            ..
        }
        | TerminatorKind::Goto { target } => target,
        _ => return,
    };
    let after = *target;
    *target = x;
    let mut fresh = |ty| {
        let l = Local(body.locals.len() as u32);
        body.locals.push(LocalDecl {
            ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Temp,
            source_info: info,
        });
        l
    };
    let r = fresh(tys.intern(TyKind::MutRef(vty)));
    let room = fresh(tys.intern(TyKind::Int(super::ty::IntTy::I64)));
    // A text's length, worked out first: `t = &text; n = String.len(move t)`.
    let mut bound = p.bound.clone();
    let mut added = vec![x];
    let text = p.len_of.take().map(|(text, ref_ty)| {
        (
            text,
            fresh(ref_ty),
            fresh(tys.intern(TyKind::Int(super::ty::IntTy::I64))),
        )
    });
    if let Some((text, t, n)) = text {
        let y = BasicBlock(x.0 + 1);
        body.blocks.push(BasicBlockData {
            statements: vec![Statement {
                kind: StatementKind::Assign(Place::local(t), Rvalue::Ref(BorrowKind::Shared, text)),
                source_info: info,
            }],
            terminator: Terminator {
                kind: TerminatorKind::Call {
                    func: Operand::Const(Const {
                        ty: tys.intern(TyKind::Unit),
                        kind: ConstKind::FnDef(InstanceId {
                            def: DefId(u32::MAX),
                            args: Vec::new(),
                            name: "String.len".to_string(),
                        }),
                    }),
                    args: vec![Operand::Move(Place::local(t))],
                    destination: Place::local(n),
                    target: Some(y),
                    unwind: UnwindAction::Abort,
                },
                source_info: info,
            },
        });
        bound = Operand::Copy(Place::local(n));
        added.push(y);
    }
    let func = Operand::Const(Const {
        ty: tys.intern(TyKind::Unit),
        kind: ConstKind::FnDef(InstanceId {
            def: DefId(u32::MAX),
            args: Vec::new(),
            name: format!("{}.presize", tys.display(vty)),
        }),
    });
    let delta = Operand::Const(Const {
        ty: tys.intern(TyKind::Int(super::ty::IntTy::I64)),
        kind: ConstKind::Scalar(p.delta as u128),
    });
    body.blocks.push(BasicBlockData {
        statements: vec![Statement {
            kind: StatementKind::Assign(
                Place::local(r),
                Rvalue::Ref(BorrowKind::Mut, Place::local(p.v)),
            ),
            source_info: info,
        }],
        terminator: Terminator {
            kind: TerminatorKind::Call {
                func,
                args: vec![Operand::Move(Place::local(r)), bound, delta],
                destination: Place::local(room),
                target: Some(after),
                unwind: UnwindAction::Abort,
            },
            source_info: info,
        },
    });
    // The new block is the tail of the one it follows: a region branch
    // that holds that block holds it too, or auto-par reads it as a way
    // out of a loop body (a `break`) when the reserve is inside one.
    let from = BasicBlock(p.new_block as u32);
    for region in body
        .par_regions
        .iter_mut()
        .chain(body.auto_par_candidates.iter_mut())
    {
        for branch in &mut region.branches {
            if branch.contains(&from) {
                branch.extend(added.iter().copied());
            }
        }
    }
    // The fill's pushes know the length the room reaches: inside the loop
    // `v` is only read besides them, so its length only grows and its
    // capacity stays at least that, and a push below it has space.
    for &b in &p.fills {
        let TerminatorKind::Call { func, args, .. } = &mut body.blocks[b].terminator.kind else {
            continue;
        };
        let Operand::Const(Const {
            kind: ConstKind::FnDef(inst),
            ..
        }) = func
        else {
            continue;
        };
        // A `String`'s room is in bytes and its push takes up to four, so
        // only a `Vec`'s pushes know theirs.
        let Some(base) = inst.name.strip_suffix(".push") else {
            continue;
        };
        if !base.starts_with("Vec[") {
            continue;
        }
        inst.name = format!("{base}.__push_within");
        inst.def = DefId(u32::MAX);
        args.push(Operand::Copy(Place::local(room)));
    }
    if let (Some(v), Some(bound)) = (p.version.take(), versioned_bound) {
        version(body, tys, &p, v, bound, room, info);
    }
}

/// Is `v` the receiver of an `index` or `index_mut` call anywhere in
/// `body`, through a borrow of the whole local?
fn indexed(body: &Body, v: Local) -> bool {
    let borrows: Vec<Local> = body
        .blocks
        .iter()
        .flat_map(|b| &b.statements)
        .filter_map(|s| match &s.kind {
            StatementKind::Assign(d, Rvalue::Ref(_, q))
                if d.projection.is_empty() && *q == Place::local(v) =>
            {
                Some(d.local)
            }
            _ => None,
        })
        .collect();
    body.blocks.iter().any(|b| {
        let TerminatorKind::Call { func, args, .. } = &b.terminator.kind else {
            return false;
        };
        let Operand::Const(Const {
            kind: ConstKind::FnDef(inst),
            ..
        }) = func
        else {
            return false;
        };
        (inst.name.ends_with(".index") || inst.name.ends_with(".index_mut"))
            && matches!(args.first(), Some(Operand::Move(r) | Operand::Copy(r))
                if r.projection.is_empty() && borrows.contains(&r.local))
    })
}

/// Copies a presized fill's loop for when the reservation covers every
/// trip (B-2026-11-09-26). `room` is the length the reservation reaches,
/// clamped to [`MAX_ROOM_BYTES`], so LLVM cannot show a push below it
/// from the loop's own bound, and each trip keeps both tests, which stops
/// the loop vectorizing (kata 484's fill, 7 instructions an element). On
/// the way in, `exact = len(v) + bound + delta - seeds` is the length the
/// last trip leaves; when `exact <= room` the copy runs, its pushes taking
/// `exact` as their reach, which the loop's bound implies. A push at or
/// past `exact` still tests the capacity, so a wrong count costs time,
/// not memory; and `exact` wrapping only makes it smaller.
fn version(
    body: &mut Body,
    tys: &TyInterner,
    p: &Plan,
    Version {
        blocks: in_loop,
        header,
        seeds,
        step,
    }: Version,
    bound: Operand,
    room: Local,
    info: SourceInfo,
) {
    if std::env::var("KARAC_MIR_PRESIZE_VERSION").as_deref() == Ok("0") {
        return;
    }
    // Only a vector of scalars can store several pushes at once; a text's
    // pushes encode a character each, and a copy of their loop is only
    // more code (kata 459's main spilled a register for one).
    let TyKind::Intrinsic(IntrinsicTy::Vec(et)) = tys.kind(body.local(p.v).ty) else {
        return;
    };
    if !matches!(
        tys.kind(et),
        TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Float(_)
    ) {
        return;
    }
    // Past the copies' join LLVM no longer knows the vector's length, which
    // a vector the function goes on to index needs to drop its checks
    // (kata 718's DP, `run[j]` with `j <= n`, ran 1.8x the instructions).
    if indexed(body, p.v) {
        return;
    }
    let i64t = tys.intern(TyKind::Int(super::ty::IntTy::I64));
    let n = in_loop.len();
    // The one way into the loop from outside it.
    let entries: Vec<usize> = (0..body.blocks.len())
        .filter(|&b| b >= n || !in_loop[b])
        .filter(|&b| {
            body.blocks[b]
                .terminator
                .kind
                .successors()
                .iter()
                .any(|t| t.index() == header)
        })
        .collect();
    let [entry] = entries[..] else {
        return;
    };
    let vty = body.local(p.v).ty;
    let step_ty = step.map(|(i, _)| body.local(i).ty);
    let mut fresh = |ty| {
        let l = Local(body.locals.len() as u32);
        body.locals.push(LocalDecl {
            ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Temp,
            source_info: info,
        });
        l
    };
    let r = fresh(tys.intern(TyKind::Ref(vty)));
    let len = fresh(i64t);
    let sum = fresh(i64t);
    let exact = fresh(i64t);
    let fits = fresh(tys.intern(TyKind::Bool));
    let pair = step_ty.map(|t| fresh(tys.intern(TyKind::Tuple(vec![t, tys.intern(TyKind::Bool)]))));
    // The copy of each loop block, by index.
    let mut copy = vec![usize::MAX; n];
    for (b, c) in copy.iter_mut().enumerate() {
        if in_loop[b] {
            *c = body.blocks.len();
            let data = body.blocks[b].clone();
            body.blocks.push(data);
        }
    }
    let to_copy = |t: &mut BasicBlock| {
        if t.index() < n && in_loop[t.index()] {
            *t = BasicBlock(copy[t.index()] as u32);
        }
    };
    for b in (0..n).filter(|&b| in_loop[b]) {
        let c = copy[b];
        match &mut body.blocks[c].terminator.kind {
            TerminatorKind::Goto { target } => to_copy(target),
            TerminatorKind::SwitchInt { targets, .. } => {
                for (_, t) in targets.values.iter_mut() {
                    to_copy(t);
                }
                to_copy(&mut targets.otherwise);
            }
            TerminatorKind::Call {
                target: Some(t), ..
            } => to_copy(t),
            TerminatorKind::Drop { target, .. } => to_copy(target),
            _ => {}
        }
        // The copy's pushes reach `exact`, as `__push_exact`.
        if p.fills.contains(&b) {
            if let TerminatorKind::Call {
                func:
                    Operand::Const(Const {
                        kind: ConstKind::FnDef(inst),
                        ..
                    }),
                args,
                ..
            } = &mut body.blocks[c].terminator.kind
            {
                if let Some(base) = inst.name.strip_suffix(".__push_within") {
                    inst.name = format!("{base}.__push_exact");
                    if let Some(last) = args.last_mut() {
                        *last = Operand::Copy(Place::local(exact));
                    }
                }
            }
        }
    }
    // The copy's step, `i = i + 1` under `i < bound`, as the checked
    // `t = i +? 1`, an overflow abort on `t.1`, and `i = t.0`: the add
    // cannot overflow, and a checked add of a constant is emitted as
    // `add nsw`, which lets LLVM count the copy's trips. The abort edge
    // keeps `t.0` a value `range_bounds` follows (kata 718's DP indexes
    // the filled vector off its counter).
    let mut split: Option<(usize, [usize; 2])> = None;
    if let (Some((i, cb)), Some(pair)) = (step, pair) {
        let ity = body.local(i).ty;
        let c = copy[cb];
        let at = body.blocks[c].statements.iter().position(|s| {
            matches!(&s.kind, StatementKind::Assign(d, Rvalue::BinaryOp(BinOp::Add, Operand::Copy(a), Operand::Const(_)))
                if *d == Place::local(i) && *a == Place::local(i))
        });
        if let Some(at) = at {
            let si = body.blocks[c].statements[at].source_info;
            let StatementKind::Assign(_, Rvalue::BinaryOp(_, a, k)) =
                body.blocks[c].statements[at].kind.clone()
            else {
                unreachable!("matched above");
            };
            let rest = body.blocks.len();
            let abort = rest + 1;
            let mut tail = body.blocks[c].statements.split_off(at);
            tail[0].kind = StatementKind::Assign(
                Place::local(i),
                Rvalue::Use(Operand::Copy(Place::local(pair).field(0, ity))),
            );
            let bool_t = tys.intern(TyKind::Bool);
            let head = &mut body.blocks[c];
            head.statements.push(Statement {
                kind: StatementKind::Assign(
                    Place::local(pair),
                    Rvalue::CheckedBinaryOp(BinOp::Add, a, k),
                ),
                source_info: si,
            });
            let term = std::mem::replace(
                &mut head.terminator,
                Terminator {
                    kind: TerminatorKind::SwitchInt {
                        discr: Operand::Copy(Place::local(pair).field(1, bool_t)),
                        targets: SwitchTargets::if_else(
                            BasicBlock(abort as u32),
                            BasicBlock(rest as u32),
                        ),
                    },
                    source_info: si,
                },
            );
            body.blocks.push(BasicBlockData {
                statements: tail,
                terminator: term,
            });
            body.blocks.push(BasicBlockData {
                statements: Vec::new(),
                terminator: Terminator {
                    kind: TerminatorKind::Abort {
                        reason: AbortReason::Overflow,
                    },
                    source_info: si,
                },
            });
            split = Some((cb, [rest, abort]));
        }
    }
    // `len = Vec.len(&v)`, then the test, on the edge into the loop.
    let check = body.blocks.len();
    let test = check + 1;
    let assign = |l: Local, rv: Rvalue| Statement {
        kind: StatementKind::Assign(Place::local(l), rv),
        source_info: info,
    };
    body.blocks.push(BasicBlockData {
        statements: vec![assign(
            r,
            Rvalue::Ref(BorrowKind::Shared, Place::local(p.v)),
        )],
        terminator: Terminator {
            kind: TerminatorKind::Call {
                func: Operand::Const(Const {
                    ty: tys.intern(TyKind::Unit),
                    kind: ConstKind::FnDef(InstanceId {
                        def: DefId(u32::MAX),
                        args: Vec::new(),
                        name: format!("{}.len", tys.display(vty)),
                    }),
                }),
                args: vec![Operand::Move(Place::local(r))],
                destination: Place::local(len),
                target: Some(BasicBlock(test as u32)),
                unwind: UnwindAction::Abort,
            },
            source_info: info,
        },
    });
    let konst = |v: i128| {
        Operand::Const(Const {
            ty: i64t,
            kind: ConstKind::Scalar(v as u64 as u128),
        })
    };
    body.blocks.push(BasicBlockData {
        statements: vec![
            assign(
                sum,
                Rvalue::BinaryOp(BinOp::Add, Operand::Copy(Place::local(len)), bound),
            ),
            assign(
                exact,
                Rvalue::BinaryOp(
                    BinOp::Add,
                    Operand::Copy(Place::local(sum)),
                    konst(p.delta - seeds),
                ),
            ),
            assign(
                fits,
                Rvalue::BinaryOp(
                    BinOp::Le,
                    Operand::Copy(Place::local(exact)),
                    Operand::Copy(Place::local(room)),
                ),
            ),
        ],
        terminator: Terminator {
            kind: TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(fits)),
                targets: SwitchTargets::if_else(
                    BasicBlock(copy[header] as u32),
                    BasicBlock(header as u32),
                ),
            },
            source_info: info,
        },
    });
    let into = |t: &mut BasicBlock| {
        if t.index() == header {
            *t = BasicBlock(check as u32);
        }
    };
    match &mut body.blocks[entry].terminator.kind {
        TerminatorKind::Goto { target } => into(target),
        TerminatorKind::SwitchInt { targets, .. } => {
            for (_, t) in targets.values.iter_mut() {
                into(t);
            }
            into(&mut targets.otherwise);
        }
        TerminatorKind::Call {
            target: Some(t), ..
        } => into(t),
        TerminatorKind::Drop { target, .. } => into(target),
        _ => {}
    }
    // A region branch holding a loop block holds its copy, and one holding
    // the entry holds the test.
    let entry_bb = BasicBlock(entry as u32);
    // A loop block's copy, and the blocks its step was split into.
    let copies_of = |b: usize| {
        let mut out = vec![BasicBlock(copy[b] as u32)];
        if let Some((_, extra)) = split.filter(|&(cb, _)| cb == b) {
            out.extend(extra.map(|x| BasicBlock(x as u32)));
        }
        out
    };
    for region in body
        .par_regions
        .iter_mut()
        .chain(body.auto_par_candidates.iter_mut())
    {
        for branch in &mut region.branches {
            let mut more = Vec::new();
            for b in branch.iter() {
                if b.index() < n && in_loop[b.index()] {
                    more.extend(copies_of(b.index()));
                }
                if *b == entry_bb {
                    more.extend([BasicBlock(check as u32), BasicBlock(test as u32)]);
                }
            }
            branch.extend(more);
        }
    }
    for region in &mut body.provided {
        let mut more = Vec::new();
        for b in &region.blocks {
            if b.index() < n && in_loop[b.index()] {
                more.extend(copies_of(b.index()));
            }
            if *b == entry_bb {
                more.extend([BasicBlock(check as u32), BasicBlock(test as u32)]);
            }
        }
        region.blocks.extend(more);
    }
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::lower::tests::run_source;
    use crate::mir::TerminatorKind;

    /// How many `presize` calls `f`'s body makes.
    fn reserved(src: &str, f: &str) -> usize {
        let l = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == f)
            .unwrap_or_else(|| panic!("no body {f}"));
        body.blocks
            .iter()
            .filter(|b| {
                matches!(&b.terminator.kind, TerminatorKind::Call { func, .. }
                    if format!("{func:?}").contains(".presize"))
            })
            .count()
    }

    fn fill(body: &str) -> String {
        format!(
            "fn fill(n: i64) -> Vec[i64] {{\n    let mut v: Vec[i64] = Vec.new();\n{body}    v\n}}\nfn main() {{\n    let v = fill(10);\n    println(f\"{{v.len()}} {{v.capacity()}} {{v[v.len() - 1]}}\");\n    println(fill(0 - 5).len());\n}}\n"
        )
    }

    /// A seed push, then one push per trip of `c <= n` from 1: `n + 1`
    /// reserved, the pushes on either side of an `if` counted as one.
    #[test]
    fn mir_presize_reserves_a_counting_fill() {
        let src = fill(
            "    v.push(7);\n    let mut c = 1;\n    while c <= n {\n        if c % 2 == 0 {\n            v.push(c);\n        } else {\n            let l = v[c - 1];\n            v.push(l + 1);\n        }\n        c = c + 1;\n    }\n",
        );
        assert_eq!(reserved(&src, "fill"), 1);
        assert_eq!(run_source(&src).unwrap(), ("11 11 10\n1\n".into(), Some(0)));
    }

    /// How many pushes in `f`'s body know their fill's room.
    fn within(src: &str, f: &str) -> usize {
        let l = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == f)
            .unwrap_or_else(|| panic!("no body {f}"));
        body.blocks
            .iter()
            .filter(|b| {
                matches!(&b.terminator.kind, TerminatorKind::Call { func, .. }
                    if format!("{func:?}").contains(".__push_within"))
            })
            .count()
    }

    /// A row filled up to an outer loop's counter (Pascal's triangle,
    /// B-2026-11-01-21), and a counter reset for a second fill of the same
    /// vector to `n - 1` (kata 20's brackets, B-2026-10-31-42): each fill
    /// is reserved, and its pushes know the room. The expected lines are
    /// the programs' arithmetic.
    #[test]
    fn mir_presize_reserves_rows_and_refills() {
        let rows = "fn tri(n: i64) -> i64 {\n    let mut s = 0;\n    let mut i = 0;\n    while i < n {\n        \
                    let mut row: Vec[i64] = Vec.new();\n        let mut j = 0;\n        while j <= i {\n            \
                    row.push(j);\n            j = j + 1;\n        }\n        s = s + row.len() * 100 + row[i];\n        \
                    i = i + 1;\n    }\n    s\n}\nfn main() {\n    println(tri(4));\n    println(tri(0));\n}\n";
        assert_eq!(reserved(rows, "tri"), 1);
        assert_eq!(within(rows, "tri"), 1);
        // Rows of 1..4 elements: 100 + 200 + 300 + 400, plus 0 + 1 + 2 + 3.
        assert_eq!(run_source(rows).unwrap(), ("1006\n0\n".into(), Some(0)));
        let refill = fill(
            "    let mut i = 0;\n    while i < n {\n        v.push(1);\n        i = i + 1;\n    }\n    \
             i = 0;\n    while i < n - 1 {\n        v.push(i + 2);\n        i = i + 1;\n    }\n",
        );
        assert_eq!(reserved(&refill, "fill"), 2);
        assert_eq!(within(&refill, "fill"), 2);
        // 10 ones, then 2..=10: 19 long, ending in 10, with room for all;
        // `fill(-5)` pushes nothing.
        assert_eq!(
            run_source(&refill).unwrap(),
            ("19 19 10\n0\n".into(), Some(0))
        );
    }

    /// A range `for` steps with an unchecked add and copies its bound into
    /// a local; an inclusive one also leaves at `i == bound`.
    #[test]
    fn mir_presize_reserves_a_range_for() {
        for (body, out) in [
            (
                "    for i in 0..n {\n        v.push(i + 1);\n    }\n",
                "10 10 10\n0\n",
            ),
            (
                "    for i in 0..=n {\n        v.push(i);\n    }\n",
                "11 11 10\n0\n",
            ),
            (
                "    for _ in 0..26 {\n        v.push(n);\n    }\n",
                "26 26 10\n26\n",
            ),
        ] {
            let src = fill(body);
            assert_eq!(reserved(&src, "fill"), 1, "{body}");
            assert_eq!(run_source(&src).unwrap(), (out.into(), Some(0)), "{body}");
        }
        // Leaving early is off the shape.
        let src = fill(
            "    for i in 0..n {\n        if i == 3 {\n            break;\n        }\n        v.push(i);\n    }\n",
        );
        assert_eq!(reserved(&src, "fill"), 0);
    }

    /// Off the shape: a push on some trips only, two pushes on a trip, a
    /// way out of the loop besides its guard, and a bound the loop moves.
    #[test]
    fn mir_presize_leaves_other_fills() {
        for (body, out) in [
            (
                "    let mut c = 0;\n    while c < n {\n        if c % 2 == 0 {\n            v.push(c);\n        }\n        c = c + 1;\n    }\n",
                "5 8",
            ),
            (
                "    let mut c = 0;\n    while c < n {\n        v.push(c);\n        v.push(c);\n        c = c + 1;\n    }\n",
                "20 9",
            ),
            (
                "    let mut c = 0;\n    while c < n {\n        if c == 3 {\n            break;\n        }\n        v.push(c);\n        c = c + 1;\n    }\n",
                "3 2",
            ),
            (
                "    let mut m = n;\n    let mut c = 0;\n    while c < m {\n        v.push(c);\n        m = m - 1;\n        c = c + 1;\n    }\n",
                "5 4",
            ),
        ] {
            let src = fill(body);
            assert_eq!(reserved(&src, "fill"), 0, "{body}");
            let (got, code) = run_source(&src).unwrap();
            assert_eq!(code, Some(0), "{body}");
            // Length and last element; the capacity is the interpreter's
            // own when nothing was reserved.
            let first: Vec<&str> = got.lines().next().unwrap_or("").split(' ').collect();
            assert_eq!(format!("{} {}", first[0], first[2]), out, "{body}");
            assert_eq!(got.lines().nth(1), Some("0"), "{body}");
        }
    }

    /// A `String` filled by one `push` per trip is reserved in bytes, and
    /// `chars().collect()` into a `Vec[char]` is reserved to the text's
    /// byte length (B-2026-11-02-11).
    #[test]
    fn mir_presize_reserves_a_string_fill_and_a_char_collect() {
        let src = "fn spell(n: i64) -> String {\n    let mut t = String.new();\n    let mut j = 0;\n    while j < n {\n        if j % 2 == 0 {\n            t.push('a');\n        } else {\n            t.push('\u{e9}');\n        }\n        j = j + 1;\n    }\n    t\n}\nfn chars_of(s: ref String) -> i64 {\n    let cs: Vec[char] = s.chars().collect();\n    cs.len()\n}\nfn main() {\n    let t = spell(5);\n    println(f\"{t} {t.len()} {chars_of(t)}\");\n    println(spell(0 - 2).len());\n}\n";
        assert_eq!(reserved(src, "spell"), 1);
        assert_eq!(reserved(src, "chars_of"), 1);
        assert_eq!(
            run_source(src).unwrap(),
            ("a\u{e9}a\u{e9}a 7 5\n0\n".into(), Some(0))
        );
    }

    /// A named binding a literal sets is a variable bound, as on legacy:
    /// `let n = 2000000` reserves although the literal is past the limit
    /// a loop counting to a literal in its own condition keeps
    /// (B-2026-11-02-9).
    #[test]
    fn mir_presize_takes_a_named_bound_past_the_literal_limit() {
        let named = "fn fill() -> i64 {\n    let n = 2000000;\n    let mut v: Vec[i64] = Vec.new();\n    let mut k = 0;\n    while k < n {\n        v.push(k);\n        k = k + 1;\n    }\n    v.len()\n}\nfn main() {\n    println(fill());\n}\n";
        assert_eq!(reserved(named, "fill"), 1);
        let literal = named
            .replace("    let n = 2000000;\n", "")
            .replace("k < n", "k < 2000000");
        assert_eq!(reserved(&literal, "fill"), 0);
    }

    /// `for c in s.chars()` pushing once a trip reserves the text's byte
    /// length after the iterator is made, with the vector's seeds kept,
    /// and its pushes know the room (B-2026-11-02-23). The capacity is
    /// the bytes: 5 chars of 7 bytes, plus the one seed.
    #[test]
    fn mir_presize_reserves_a_chars_walk() {
        let src = "fn snap(s: ref String) -> Vec[char] {\n    let mut cs: Vec[char] = Vec.new();\n    cs.push('<');\n    \
                   for c in s.chars() {\n        if c == 'a' {\n            cs.push('A');\n        } else {\n            \
                   cs.push(c);\n        }\n    }\n    cs\n}\nfn main() {\n    let t = \"a\u{e9}a\u{e9}a\";\n    \
                   let cs = snap(t);\n    println(f\"{cs.len()} {cs.capacity()} {cs[2]} {cs[5]}\");\n    \
                   println(snap(\"\").len());\n}\n";
        assert_eq!(reserved(src, "snap"), 1);
        assert_eq!(within(src, "snap"), 2);
        assert_eq!(
            run_source(src).unwrap(),
            ("6 8 \u{e9} A\n1\n".into(), Some(0))
        );
        // Off the shape: a push on some trips only, and a way out besides
        // the end of the text.
        for body in [
            "        if c != 'a' {\n            cs.push(c);\n        }\n",
            "        if c == 'b' {\n            break;\n        }\n        cs.push(c);\n",
        ] {
            let src = format!(
                "fn snap(s: ref String) -> i64 {{\n    let mut cs: Vec[char] = Vec.new();\n    \
                 for c in s.chars() {{\n{body}    }}\n    cs.len()\n}}\nfn main() {{\n    println(snap(\"abc\"));\n}}\n"
            );
            assert_eq!(reserved(&src, "snap"), 0, "{body}");
        }
    }

    /// A `with_capacity(n)` start is a fresh start too: its fill is
    /// reserved, which takes nothing more when `n` holds it, and its
    /// pushes know the room (B-2026-11-02-26).
    #[test]
    fn mir_presize_reserves_a_with_capacity_fill() {
        for (start, cap) in [("Vec.with_capacity(4)", 10), ("Vec.with_capacity(64)", 64)] {
            let src = fill(&format!(
                "    v = {start};\n    let mut i = 0;\n    while i < n {{\n        v.push(i + 1);\n        i = i + 1;\n    }}\n"
            ))
            .replace("let mut v: Vec[i64] = Vec.new();\n    v = ", "let mut v: Vec[i64] = ");
            assert_eq!(reserved(&src, "fill"), 1, "{start}");
            assert_eq!(within(&src, "fill"), 1, "{start}");
            assert_eq!(
                run_source(&src).unwrap(),
                (format!("10 {cap} 10\n0\n"), Some(0)),
                "{start}"
            );
        }
    }
}
