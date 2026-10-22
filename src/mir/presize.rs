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
//! and the measurements are).
//!
//! The shape, on an elaborated body, for a loop with no loop inside it:
//!
//! - a guard block `G` that dominates every latch and leaves the loop when
//!   `i < bound` (or `i <= bound`) is false, and no other way out of the
//!   loop than `G`'s exit and aborts;
//! - `i` an integer local written twice: `i = k` (`k >= 0`) just before
//!   the loop and `i = i + 1`, checked, in a block that dominates every
//!   latch;
//! - `bound` a literal, or a local written once (or a parameter never
//!   written) where that write dominates `Vec.new()`, so it holds the same
//!   value there and at `G`;
//! - every path through one trip, from the header to a latch, runs exactly
//!   one `Vec.push(&mut v, _)` and none before `G`; inside the loop `v` is
//!   otherwise only borrowed shared;
//! - `v = Vec.new()` on the chain of single-predecessor blocks before the
//!   loop, with nothing between it and the loop mentioning `v` but pushes
//!   (seeds, each one more element).
//!
//! Then the loop runs `bound - k` trips (`+ 1` for `<=`), one push each.

use super::counters::{
    dominates, dominators, increment, local_writes, mask, natural_loops, predecessors, Loop, Writes,
};
use super::rc_pairs::{statement_mentions, terminator_mentions};
use super::syntax::*;
use super::ty::{IntrinsicTy, TyInterner, TyKind};
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
}

/// Adds a `presize` after each `Vec.new()` a counting loop fills.
pub fn presize_pushes(body: &mut Body, tys: &TyInterner) {
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
        for p in plan(body, tys, &preds, &idom, lp, &writes) {
            if !plans.iter().any(|q| q.new_block == p.new_block) {
                plans.push(p);
            }
        }
    }
    for p in plans {
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
    if method != "push" || !base.starts_with("Vec[") {
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
/// `switchInt(c) -> [0: stay, otherwise: out]` with `c = Eq(copy i, bound)`.
fn leaves_at_bound(
    body: &Body,
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
    matches!(cmp, Some(Rvalue::BinaryOp(BinOp::Eq, x, y))
        if (*x == iv && y == bound) || (x == bound && *y == iv))
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
                if op != BinOp::Le || !leaves_at_bound(body, b, i, &bound, &in_loop) {
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
    if bits > 64 || writes.other[i.index()] || writes.assigns[i.index()].len() != 2 {
        return out;
    }
    let step = match increment(body, writes, i) {
        Some((cb, _, c)) => mask(c, bits) == 1 && dominates_latches(cb),
        // A range `for` steps with an unchecked add: its guard keeps `i`
        // below the bound.
        None => plain_increment(body, writes, i)
            .is_some_and(|(cb, c)| mask(c, bits) == 1 && dominates_latches(cb)),
    };
    if !step {
        return out;
    }
    // The single-predecessor chain into the loop, nearest block first.
    let outside: Vec<usize> = preds[lp.header]
        .iter()
        .copied()
        .filter(|&p| !in_loop(p))
        .collect();
    let [entry] = outside.as_slice() else {
        return out;
    };
    let mut chain = vec![*entry];
    while chain.len() < 16 {
        let last = *chain.last().expect("non-empty");
        match preds[last].as_slice() {
            [p] if *p != last && !in_loop(*p) && !chain.contains(p) => chain.push(*p),
            _ => break,
        }
    }
    let k = writes.assigns[i.index()].iter().find_map(|&(bi, si)| {
        if !chain.contains(&bi) {
            return None;
        }
        match &body.blocks[bi].statements[si].kind {
            StatementKind::Assign(_, Rvalue::Use(o)) => int_const(o),
            _ => None,
        }
    });
    let Some(k) = k else {
        return out;
    };
    let k = signed_value(k, bits, signed);
    if k < 0 {
        return out;
    }
    // Each `v` some block of the loop pushes to.
    let mut vs: Vec<Local> = Vec::new();
    for b in (0..n).filter(|&b| in_loop(b)) {
        for s in &body.blocks[b].statements {
            if let Some((_, BorrowKind::Mut, v)) = borrow_of(s) {
                if pushes(body, b, v) && !vs.contains(&v) {
                    vs.push(v);
                }
            }
        }
    }
    // The trip's blocks in an order that has each after its predecessors
    // (edges back to the header left out).
    let order = {
        let mut post = Vec::new();
        let mut seen = vec![false; n];
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
    };
    'v: for v in vs {
        if !matches!(
            tys.kind(body.local(v).ty),
            TyKind::Intrinsic(IntrinsicTy::Vec(_))
        ) {
            continue;
        }
        if !(0..n)
            .filter(|&b| in_loop(b))
            .all(|b| mentions_only_reads(body, b, v, true))
        {
            continue;
        }
        // One push per trip, on every path, none before the guard.
        let mut range: Vec<Option<(u32, u32)>> = vec![None; n];
        range[lp.header] = Some((0, 0));
        for &b in &order {
            let Some((lo, hi)) = range[b] else {
                continue 'v;
            };
            if b == g && (lo, hi) != (0, 0) {
                continue 'v;
            }
            let p = u32::from(pushes(body, b, v));
            let (lo, hi) = (lo + p, hi + p);
            if (lp.latches.contains(&b) || last_exits.contains(&b)) && (lo, hi) != (1, 1) {
                continue 'v;
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
                    let fresh = fn_name(func)
                        .map(split_method)
                        .is_some_and(|(base, m)| m == "new" && base.starts_with("Vec["));
                    if !fresh || !args.is_empty() {
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
        let Some(nb) = new_block else {
            continue;
        };
        let adj = i128::from(op == BinOp::Le);
        let delta = seeds + adj - k;
        // The bound, holding at `Vec.new()` what it holds at the guard.
        // A range `for` copies a literal bound into a local.
        let lit = match &bound {
            Operand::Const(_) => int_const(&bound),
            Operand::Copy(p) if p.projection.is_empty() => copied_const(body, p.local),
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
                    let Some(b) = settled(body, idom, nb, p.local)
                        .or_else(|| copied_from(body, p.local, |x| settled(body, idom, nb, x)))
                    else {
                        continue;
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
        });
    }
    out
}

/// `v = Vec.new() -> t` becomes `v = Vec.new() -> X; X: r = &mut v;
/// Vec.presize(move r, bound, delta) -> t`.
fn apply(body: &mut Body, tys: &TyInterner, p: Plan) {
    let vty = body.local(p.v).ty;
    let x = BasicBlock(body.blocks.len() as u32);
    let info = body.blocks[p.new_block].terminator.source_info;
    let TerminatorKind::Call {
        target: Some(target),
        ..
    } = &mut body.blocks[p.new_block].terminator.kind
    else {
        return;
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
    let unit = fresh(tys.intern(TyKind::Unit));
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
                args: vec![Operand::Move(Place::local(r)), p.bound, delta],
                destination: Place::local(unit),
                target: Some(after),
                unwind: UnwindAction::Abort,
            },
            source_info: info,
        },
    });
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
}
