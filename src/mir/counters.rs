//! Overflow checks a counting loop proves dead.
//!
//! Kāra traps on integer overflow, so `cnt = cnt + 1` checks. Inside a
//! guarded counting loop the check is what stops LLVM turning
//! `if bit { cnt = cnt + 1 }` into a branchless add: a checked add may
//! trap, so it cannot be speculated, and a data-dependent branch stays.
//! The legacy backend removes the same check for the same shape
//! (`codegen/accum_overflow.rs`, where the measurements are).
//!
//! The shape, on an elaborated body, for a loop with header `h`:
//!
//! - a guard block `G`, run once per trip and on every trip (it dominates
//!   every latch and sits in no loop nested in this one), whose switch
//!   leaves the loop when `i < bound` is false;
//! - `i` an integer local written twice in the whole body: `i = k` with
//!   `k >= 0` just before the loop, and `i = i + c` (checked, `c >= 1`)
//!   in a block that dominates every latch;
//! - `acc`, of `i`'s type, written twice: `acc = 0` just before the loop,
//!   and `acc = acc + 1` in a block `G` dominates, in no nested loop.
//!
//! "Just before" means on the chain of single-predecessor blocks that
//! ends at the loop's one entry edge, so every entry resets both.
//!
//! Then `i` is at least `k >= 0` and grows on every trip, and every trip
//! that reaches the increment passed `i < bound <= MAX`, so there are at
//! most `MAX` such trips and `acc <= MAX` throughout: the increment's
//! overflow branch is dead, and becomes a `goto` to its no-overflow side.
//! `i`'s own check stays, so an out-of-range step still traps.

use super::syntax::*;
use super::ty::{TyInterner, TyKind};

/// Turns the dead overflow branches of `body`'s counting loops into gotos.
pub fn elide_counter_checks(body: &mut Body, tys: &TyInterner) {
    let n = body.blocks.len();
    if n == 0 {
        return;
    }
    let preds = predecessors(body);
    let idom = dominators(body, &preds);
    let loops = natural_loops(body, &preds, &idom);
    let writes = local_writes(body);
    let mut dead = Vec::new();
    for lp in &loops {
        dead.extend(counter_branches(
            body, tys, &preds, &idom, &loops, lp, &writes,
        ));
    }
    for (bb, ok) in dead {
        body.blocks[bb].terminator.kind = TerminatorKind::Goto { target: ok };
    }
}

struct Loop {
    header: usize,
    blocks: Vec<bool>,
    latches: Vec<usize>,
}

/// Where each local is written: plain assignments to the whole local, and
/// whether anything else (a projection, a call result, a `mut` borrow, a
/// discriminant) may write it.
struct Writes {
    assigns: Vec<Vec<(usize, usize)>>,
    other: Vec<bool>,
}

fn local_writes(body: &Body) -> Writes {
    let mut w = Writes {
        assigns: vec![Vec::new(); body.locals.len()],
        other: vec![false; body.locals.len()],
    };
    for (bi, b) in body.blocks.iter().enumerate() {
        for (si, s) in b.statements.iter().enumerate() {
            match &s.kind {
                StatementKind::Assign(p, rv) => {
                    if p.projection.is_empty() {
                        w.assigns[p.local.index()].push((bi, si));
                    } else {
                        w.other[p.local.index()] = true;
                    }
                    if let Rvalue::Ref(BorrowKind::Mut, q) = rv {
                        w.other[q.local.index()] = true;
                    }
                }
                StatementKind::SetDiscriminant(p, _) => w.other[p.local.index()] = true,
                _ => {}
            }
        }
        if let TerminatorKind::Call { destination, .. } = &b.terminator.kind {
            w.other[destination.local.index()] = true;
        }
    }
    w
}

/// The `(block, no-overflow target)` of each dead check in loop `lp`.
fn counter_branches(
    body: &Body,
    tys: &TyInterner,
    preds: &[Vec<usize>],
    idom: &[usize],
    loops: &[Loop],
    lp: &Loop,
    writes: &Writes,
) -> Vec<(usize, BasicBlock)> {
    let mut out = Vec::new();
    let once_per_trip =
        |b: usize| lp.blocks[b] && loops.iter().all(|o| !o.blocks[b] || o.blocks[lp.header]);
    let dominates_latches = |b: usize| lp.latches.iter().all(|&t| dominates(idom, b, t));
    // The chain of statements every entry runs just before the loop.
    let Some(chain) = entry_chain(body, preds, lp) else {
        return out;
    };
    let init_of = |l: Local| -> Option<u128> {
        let ws = &writes.assigns[l.index()];
        ws.iter().find_map(|&(bi, si)| {
            if !chain.contains(&(bi, si)) {
                return None;
            }
            match &body.blocks[bi].statements[si].kind {
                StatementKind::Assign(
                    _,
                    Rvalue::Use(Operand::Const(Const {
                        kind: ConstKind::Scalar(v),
                        ..
                    })),
                ) => Some(*v),
                _ => None,
            }
        })
    };
    for (g, gb) in body.blocks.iter().enumerate() {
        if !once_per_trip(g) || !dominates_latches(g) {
            continue;
        }
        let Some((i, it)) = guard(body, tys, lp, gb) else {
            continue;
        };
        let (bits, signed) = it;
        let nonneg = |v: u128| {
            let v = mask(v, bits);
            !signed || v >> (bits - 1) == 0
        };
        // `i`: written once before the loop and once, growing, in it.
        if writes.other[i.index()] || writes.assigns[i.index()].len() != 2 {
            continue;
        }
        if !init_of(i).is_some_and(nonneg) {
            continue;
        }
        let Some(&(sb, _)) = writes.assigns[i.index()]
            .iter()
            .find(|&&(bi, _)| lp.blocks[bi])
        else {
            continue;
        };
        let grows = increment(body, writes, i).is_some_and(|(_, _, c)| {
            let c = mask(c, bits);
            c != 0 && nonneg(c)
        });
        if !grows || !dominates_latches(sb) {
            continue;
        }
        // Each `acc` of `i`'s type that counts from zero by one.
        for acc in 0..body.locals.len() {
            let acc = Local(acc as u32);
            if acc == i
                || body.local(acc).ty != body.local(i).ty
                || writes.other[acc.index()]
                || writes.assigns[acc.index()].len() != 2
                || init_of(acc).map(|v| mask(v, bits)) != Some(0)
            {
                continue;
            }
            let Some((cb, ok, c)) = increment(body, writes, acc) else {
                continue;
            };
            if mask(c, bits) == 1 && once_per_trip(cb) && dominates(idom, g, cb) {
                out.push((cb, ok));
            }
        }
    }
    out
}

/// `G`'s switch leaves `lp` when `i < bound` is false: `i` and its
/// width and signedness.
fn guard(
    body: &Body,
    tys: &TyInterner,
    lp: &Loop,
    gb: &BasicBlockData,
) -> Option<(Local, (u32, bool))> {
    let TerminatorKind::SwitchInt {
        discr: Operand::Copy(c),
        targets,
    } = &gb.terminator.kind
    else {
        return None;
    };
    let [(0, exit)] = targets.values.as_slice() else {
        return None;
    };
    if !c.projection.is_empty() || lp.blocks[exit.index()] || !lp.blocks[targets.otherwise.index()]
    {
        return None;
    }
    let cmp = gb.statements.iter().rev().find_map(|s| match &s.kind {
        StatementKind::Assign(d, rv) if d == c => Some(rv),
        _ => None,
    })?;
    let Rvalue::BinaryOp(BinOp::Lt, Operand::Copy(ip), _) = cmp else {
        return None;
    };
    if !ip.projection.is_empty() {
        return None;
    }
    match tys.kind(body.local(ip.local).ty) {
        TyKind::Int(it) => Some((ip.local, (it.bits(), it.signed()))),
        _ => None,
    }
}

/// `l = l + c`, checked, as its one write inside a loop: the block holding
/// the checked add, that block's no-overflow successor, and `c`.
fn increment(body: &Body, writes: &Writes, l: Local) -> Option<(usize, BasicBlock, u128)> {
    // `l = copy t` (or `copy pair.0`) for the write that is not the init.
    for &(bi, si) in &writes.assigns[l.index()] {
        let StatementKind::Assign(_, Rvalue::Use(Operand::Copy(src))) =
            &body.blocks[bi].statements[si].kind
        else {
            continue;
        };
        let pair = if src.projection.is_empty() {
            // `t = copy pair.0`, `t` written once.
            let ws = &writes.assigns[src.local.index()];
            if ws.len() != 1 || writes.other[src.local.index()] {
                return None;
            }
            let (tb, ts) = ws[0];
            match &body.blocks[tb].statements[ts].kind {
                StatementKind::Assign(_, Rvalue::Use(Operand::Copy(p))) => p.clone(),
                _ => return None,
            }
        } else {
            src.clone()
        };
        let [ProjElem::Field(FieldIdx(0), _)] = pair.projection.as_slice() else {
            return None;
        };
        let pl = pair.local;
        let ws = &writes.assigns[pl.index()];
        if ws.len() != 1 || writes.other[pl.index()] {
            return None;
        }
        let (cb, cs) = ws[0];
        let block = &body.blocks[cb];
        let StatementKind::Assign(_, Rvalue::CheckedBinaryOp(BinOp::Add, a, b)) =
            &block.statements[cs].kind
        else {
            return None;
        };
        let c = match (a, b) {
            (Operand::Copy(p), Operand::Const(k)) | (Operand::Const(k), Operand::Copy(p))
                if *p == Place::local(l) =>
            {
                match k.kind {
                    ConstKind::Scalar(v) => v,
                    _ => return None,
                }
            }
            _ => return None,
        };
        // The block's switch on the overflow flag.
        let TerminatorKind::SwitchInt {
            discr: Operand::Copy(f),
            targets,
        } = &block.terminator.kind
        else {
            return None;
        };
        let [(0, ok)] = targets.values.as_slice() else {
            return None;
        };
        if f.local != pl || !matches!(f.projection.as_slice(), [ProjElem::Field(FieldIdx(1), _)]) {
            return None;
        }
        return Some((cb, *ok, c));
    }
    None
}

fn mask(v: u128, bits: u32) -> u128 {
    if bits >= 128 {
        v
    } else {
        v & ((1u128 << bits) - 1)
    }
}

/// The statements on the chain of single-predecessor blocks that ends at
/// `lp`'s one entry edge, as `(block, statement)`; `None` when the loop
/// has more than one entry edge or the chain enters a loop block.
fn entry_chain(body: &Body, preds: &[Vec<usize>], lp: &Loop) -> Option<Vec<(usize, usize)>> {
    let outside: Vec<usize> = preds[lp.header]
        .iter()
        .copied()
        .filter(|&p| !lp.blocks[p])
        .collect();
    let [mut b] = outside.as_slice() else {
        return None;
    };
    let mut out = Vec::new();
    for _ in 0..16 {
        if lp.blocks[b] {
            return None;
        }
        out.extend((0..body.blocks[b].statements.len()).map(|s| (b, s)));
        match preds[b].as_slice() {
            [p] if *p != b => b = *p,
            _ => break,
        }
    }
    Some(out)
}

fn predecessors(body: &Body) -> Vec<Vec<usize>> {
    let mut preds = vec![Vec::new(); body.blocks.len()];
    for (i, b) in body.blocks.iter().enumerate() {
        for s in b.terminator.kind.successors() {
            preds[s.index()].push(i);
        }
    }
    preds
}

/// Immediate dominators (Cooper, Harvey and Kennedy); `usize::MAX` for a
/// block the entry does not reach, and the entry is its own.
fn dominators(body: &Body, preds: &[Vec<usize>]) -> Vec<usize> {
    let n = body.blocks.len();
    // Reverse postorder from the entry.
    let mut order = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    let mut stack = vec![(0usize, 0usize)];
    seen[0] = true;
    while let Some(&mut (b, ref mut k)) = stack.last_mut() {
        let succ = body.blocks[b].terminator.kind.successors();
        if let Some(s) = succ.get(*k) {
            *k += 1;
            let s = s.index();
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
            stack.pop();
        }
    }
    order.reverse();
    let mut rpo = vec![usize::MAX; n];
    for (i, &b) in order.iter().enumerate() {
        rpo[b] = i;
    }
    let mut idom = vec![usize::MAX; n];
    idom[0] = 0;
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().skip(1) {
            let mut new = usize::MAX;
            for &p in &preds[b] {
                if idom[p] == usize::MAX {
                    continue;
                }
                new = if new == usize::MAX {
                    p
                } else {
                    let (mut x, mut y) = (p, new);
                    while x != y {
                        while rpo[x] > rpo[y] {
                            x = idom[x];
                        }
                        while rpo[y] > rpo[x] {
                            y = idom[y];
                        }
                    }
                    x
                };
            }
            if new != idom[b] {
                idom[b] = new;
                changed = true;
            }
        }
    }
    idom
}

/// Does `a` dominate `b`?
fn dominates(idom: &[usize], a: usize, mut b: usize) -> bool {
    if idom[b] == usize::MAX {
        return false;
    }
    loop {
        if a == b {
            return true;
        }
        if b == 0 {
            return false;
        }
        b = idom[b];
    }
}

/// One loop per header, over all its back edges.
fn natural_loops(body: &Body, preds: &[Vec<usize>], idom: &[usize]) -> Vec<Loop> {
    let n = body.blocks.len();
    let mut loops: Vec<Loop> = Vec::new();
    for t in 0..n {
        if idom[t] == usize::MAX {
            continue;
        }
        for h in body.blocks[t].terminator.kind.successors() {
            let h = h.index();
            if !dominates(idom, h, t) {
                continue;
            }
            let lp = match loops.iter_mut().find(|l| l.header == h) {
                Some(l) => l,
                None => {
                    let mut blocks = vec![false; n];
                    blocks[h] = true;
                    loops.push(Loop {
                        header: h,
                        blocks,
                        latches: Vec::new(),
                    });
                    loops.last_mut().expect("just pushed")
                }
            };
            lp.latches.push(t);
            let mut work = vec![t];
            while let Some(b) = work.pop() {
                if lp.blocks[b] {
                    continue;
                }
                lp.blocks[b] = true;
                work.extend(preds[b].iter().copied());
            }
        }
    }
    loops
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::lower::tests::run_source;
    use crate::mir::{BinOp, Rvalue, StatementKind, TerminatorKind};

    /// How many checked adds in `f` lost their overflow branch.
    fn elided(src: &str, f: &str) -> usize {
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
                matches!(b.terminator.kind, TerminatorKind::Goto { .. })
                    && b.statements.iter().any(|s| {
                        matches!(
                            s.kind,
                            StatementKind::Assign(_, Rvalue::CheckedBinaryOp(BinOp::Add, ..))
                        )
                    })
            })
            .count()
    }

    fn count_fn(body: &str) -> String {
        format!(
            "fn count(v: ref Vec[i64]) -> i64 {{\n{body}\n}}\n\
fn main() {{\n    let mut v: Vec[i64] = Vec.new();\n    v.push(1);\n    v.push(2);\n    v.push(3);\n    println(count(v));\n}}\n"
        )
    }

    /// `cnt` counts from zero by one, once per trip of `while i < n`.
    #[test]
    fn mir_counters_guarded_count_drops_its_check() {
        let src = count_fn(
            "    let mut cnt = 0;\n    let mut i = 0;\n    while i < v.len() {\n        if v[i] % 2 == 1 {\n            cnt = cnt + 1;\n        }\n        i = i + 1;\n    }\n    cnt",
        );
        assert_eq!(elided(&src, "count"), 1);
        assert_eq!(run_source(&src).unwrap(), ("2\n".into(), Some(0)));
        // The unconditional form the cases below vary.
        let plain = count_fn(
            "    let mut cnt = 0;\n    let mut i = 0;\n    while i < v.len() {\n        cnt = cnt + 1;\n        i = i + 1;\n    }\n    cnt",
        );
        assert_eq!(elided(&plain, "count"), 1);
        assert_eq!(run_source(&plain).unwrap(), ("3\n".into(), Some(0)));
    }

    /// Each of these breaks one step of the argument, so the check stays.
    #[test]
    fn mir_counters_keeps_the_check_off_the_shape() {
        let cases = [
            // Starts at one: `acc <= trip + 1`.
            "    let mut cnt = 1;\n    let mut i = 0;\n    while i < v.len() {\n        cnt = cnt + 1;\n        i = i + 1;\n    }\n    cnt",
            // Two increments a trip.
            "    let mut cnt = 0;\n    let mut i = 0;\n    while i < v.len() {\n        cnt = cnt + 1;\n        cnt = cnt + 1;\n        i = i + 1;\n    }\n    cnt",
            // A trip that skips the step does not grow `i`.
            "    let mut cnt = 0;\n    let mut i = 0;\n    while i < v.len() {\n        cnt = cnt + 1;\n        if cnt > 100 {\n            i = i + 1;\n        }\n    }\n    cnt",
            // Not reset per entry: the outer loop runs the inner one again.
            "    let mut cnt = 0;\n    let mut k = 0;\n    while k < 3 {\n        let mut i = 0;\n        while i < v.len() {\n            cnt = cnt + 1;\n            i = i + 1;\n        }\n        k = k + 1;\n    }\n    cnt",
            // A negative start.
            "    let mut cnt = 0;\n    let mut i = 0 - 5;\n    while i < v.len() {\n        cnt = cnt + 1;\n        i = i + 1;\n    }\n    cnt",
        ];
        for body in cases {
            assert_eq!(elided(&count_fn(body), "count"), 0, "{body}");
        }
    }
}
