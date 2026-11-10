//! Loop counters that only move one way, for the backend to state so
//! (B-2026-11-01-19; legacy's monotone tier in
//! `codegen/control_flow_bce.rs`).
//!
//! In a merge `while i < m && j < n { if a[i] <= b[j] { .. i = i + 1; }
//! else { .. j = j + 1; } }` each cursor is stepped conditionally, so its
//! phi is no recurrence and neither scalar evolution nor value ranges say
//! anything about it: the check on `k - i` or `a[i]` survives because
//! nothing tells LLVM that `i` never drops below its value on entry.
//! Stated as `assume(x >= init)` at the loop header, with `init` the value
//! `x` held when the loop was entered, LLVM combines it with what only it
//! knows (inlined constants, argument ranges) and drops the check. Legacy
//! stated it for every such counter; without it kata 88's bench ran 384.6M
//! instructions against legacy's 301.4M, and with it 304.6M. A counter
//! stepped unconditionally gains nothing: its fact is a trivially true
//! recurrence that EarlyCSE folds away (kata 216 measured so).
//!
//! The fact holds because every write to `x` inside the loop is a checked
//! `x = x + c` (or `x - c`, decreasing) with a constant `c >= 0`, all in
//! one direction, and a step that overflows aborts before the value lands:
//! each value `x` takes in the loop is an earlier one moved the same way,
//! starting from `init`. Anything else that could write `x` in the loop
//! (a plain assignment, a call's result, a projection, its storage ending)
//! or anywhere (a `mut` borrow, which may write it from inside) states
//! nothing. The loop needs one entry edge, from a block that does not
//! itself write `x`, where `init` is taken.

use super::counters::{dominators, mask, natural_loops, predecessors, Loop};
use super::syntax::*;
use super::ty::{TyInterner, TyKind};

/// `local` never moves below (`up`) or above its value on entry to the
/// loop headed by `header`, whose one entry edge leaves `entry`; compared
/// as `signed` integers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monotone {
    pub header: usize,
    pub entry: usize,
    pub local: Local,
    pub up: bool,
    pub signed: bool,
    /// The blocks of the loop, by index.
    pub blocks: Vec<bool>,
}

/// Every monotone counter of every loop of `body`.
pub fn monotones(body: &Body, tys: &TyInterner) -> Vec<Monotone> {
    // A coroutine's resume body re-enters its loops from the frame, where
    // a value on entry is not the one a header first saw.
    if body.blocks.is_empty() || body.suspends || super::coroutine::is_resume(&body.instance.name) {
        return Vec::new();
    }
    let preds = predecessors(body);
    let idom = dominators(body, &preds);
    // `KARAC_MIR_MIDPOINT_MONO=0` leaves a binary search's bounds unstated.
    let mids = if std::env::var("KARAC_MIR_MIDPOINT_MONO").as_deref() == Ok("0") {
        Vec::new()
    } else {
        super::midpoint::midpoints(body, tys)
    };
    let loops = natural_loops(body, &preds, &idom);
    if loops.is_empty() {
        return Vec::new();
    }
    // A `mut` borrow may write its local from anywhere it reaches.
    let mut borrowed = vec![false; body.locals.len()];
    for b in &body.blocks {
        for s in &b.statements {
            if let StatementKind::Assign(_, Rvalue::Ref(BorrowKind::Mut, q)) = &s.kind {
                borrowed[q.local.index()] = true;
            }
        }
    }
    let mut out = Vec::new();
    for lp in &loops {
        let outside: Vec<usize> = preds[lp.header]
            .iter()
            .copied()
            .filter(|&p| !lp.blocks[p])
            .collect();
        let [entry] = outside.as_slice() else {
            continue;
        };
        // Each local's direction so far, or poisoned.
        let mut dirs: Vec<Option<Option<bool>>> = vec![None; body.locals.len()];
        for (bi, b) in body.blocks.iter().enumerate() {
            if !lp.blocks[bi] {
                continue;
            }
            for (si, s) in b.statements.iter().enumerate() {
                match &s.kind {
                    StatementKind::Assign(p, _) if !p.projection.is_empty() => {
                        poison(&mut dirs, p.local)
                    }
                    StatementKind::Assign(p, _) => {
                        match (
                            step(body, tys, &idom, lp, &mids, p.local, bi, si),
                            dirs[p.local.index()],
                        ) {
                            (Some(up), None) => dirs[p.local.index()] = Some(Some(up)),
                            (Some(up), Some(Some(d))) if d == up => {}
                            _ => poison(&mut dirs, p.local),
                        }
                    }
                    StatementKind::SetDiscriminant(p, _) => poison(&mut dirs, p.local),
                    StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => {
                        poison(&mut dirs, *l)
                    }
                    _ => {}
                }
            }
            if let TerminatorKind::Call { destination, .. } = &b.terminator.kind {
                poison(&mut dirs, destination.local);
            }
        }
        // The entry block's own call result lands after `init` is taken.
        if let TerminatorKind::Call { destination, .. } = &body.blocks[*entry].terminator.kind {
            poison(&mut dirs, destination.local);
        }
        for (i, d) in dirs.iter().enumerate() {
            let Some(Some(up)) = *d else {
                continue;
            };
            let local = Local(i as u32);
            if borrowed[i] || local == Local::RETURN_PLACE {
                continue;
            }
            let TyKind::Int(it) = tys.kind(body.local(local).ty) else {
                continue;
            };
            out.push(Monotone {
                header: lp.header,
                entry: *entry,
                local,
                up,
                signed: it.signed(),
                blocks: lp.blocks.clone(),
            });
        }
    }
    out
}

/// `l` states nothing in this loop.
fn poison(dirs: &mut [Option<Option<bool>>], l: Local) {
    dirs[l.index()] = Some(None);
}

/// Statement `si` of block `bi`, inside `lp`, as a step of `x`: `x = copy
/// t` or `x = copy pair.0`, `t = copy pair.0` written once, and `pair =
/// CheckedAdd(x, c)` / `CheckedAdd(c, x)` (true: up) or `CheckedSub(x, c)`
/// (false: down) written once inside the loop, with `c >= 0` in `x`'s type
/// and the value taken only past the overflow test. `None` for any other
/// write.
///
/// A binary search's bound moved to its midpoint is a step too
/// (B-2026-11-10-37): `mid` lies in `[lo, hi)` for the `lo` and `hi` of
/// this trip's guard (`super::midpoint`), so `hi = mid` moves `hi` down
/// and `lo = mid` or `lo = mid + c` moves `lo` up, when `mid` is written
/// only at its midpoint, inside the loop, before the step.
#[allow(clippy::too_many_arguments)]
fn step(
    body: &Body,
    tys: &TyInterner,
    idom: &[usize],
    lp: &Loop,
    mids: &[super::midpoint::Midpoint],
    x: Local,
    bi: usize,
    si: usize,
) -> Option<bool> {
    let TyKind::Int(it) = tys.kind(body.local(x).ty) else {
        return None;
    };
    let StatementKind::Assign(_, Rvalue::Use(Operand::Copy(src) | Operand::Move(src))) =
        &body.blocks[bi].statements[si].kind
    else {
        return None;
    };
    // The midpoint `o` copies, of a guard on `x`, settled before `(b, s)`.
    let mid_of = |o: &Operand, b: usize, s: usize| {
        let (Operand::Copy(q) | Operand::Move(q)) = o else {
            return None;
        };
        if !q.projection.is_empty() {
            return None;
        }
        mids.iter().find(|m| {
            m.mid == q.local
                && (m.lo == x || m.hi == x)
                && lp.blocks[m.block]
                && only_write(body, m.mid) == Some((m.block, m.statement))
                && if m.block == b {
                    m.statement < s
                } else {
                    super::counters::dominates(idom, m.block, b)
                }
        })
    };
    if let Some(m) = mid_of(&Operand::Copy(src.clone()), bi, si) {
        return Some(m.lo == x);
    }
    let pair = if src.projection.is_empty() {
        let (tb, ts) = only_write(body, src.local)?;
        if !lp.blocks[tb] {
            return None;
        }
        match &body.blocks[tb].statements[ts].kind {
            StatementKind::Assign(_, Rvalue::Use(Operand::Copy(p) | Operand::Move(p))) => p.clone(),
            _ => return None,
        }
    } else {
        src.clone()
    };
    let [ProjElem::Field(FieldIdx(0), _)] = pair.projection.as_slice() else {
        return None;
    };
    let (cb, cs) = only_write(body, pair.local)?;
    if !lp.blocks[cb] {
        return None;
    }
    let block = &body.blocks[cb];
    let StatementKind::Assign(_, Rvalue::CheckedBinaryOp(op, a, b)) = &block.statements[cs].kind
    else {
        return None;
    };
    let me =
        |o: &Operand| matches!(o, Operand::Copy(p) | Operand::Move(p) if *p == Place::local(x));
    let k = |o: &Operand| match o {
        Operand::Const(k) => match k.kind {
            ConstKind::Scalar(v) => Some(v),
            _ => None,
        },
        _ => None,
    };
    let low = |o: &Operand| mid_of(o, cb, cs).is_some_and(|m| m.lo == x);
    let (up, c) = match op {
        BinOp::Add if me(a) || low(a) => (true, k(b)?),
        BinOp::Add if me(b) || low(b) => (true, k(a)?),
        BinOp::Sub if me(a) => (false, k(b)?),
        _ => return None,
    };
    let bits = it.bits();
    if it.signed() && bits > 0 && mask(c, bits) >> (bits - 1) != 0 {
        return None;
    }
    // Past the overflow test: the test's no-overflow side (or its goto,
    // once a counting loop proved the check dead) dominates the write, and
    // its overflow side aborts.
    let ok = match &block.terminator.kind {
        TerminatorKind::Goto { target } => *target,
        TerminatorKind::SwitchInt {
            discr: Operand::Copy(f),
            targets,
        } => {
            let [(0, ok)] = targets.values.as_slice() else {
                return None;
            };
            if f.local != pair.local
                || !matches!(f.projection.as_slice(), [ProjElem::Field(FieldIdx(1), _)])
                || !matches!(
                    body.blocks[targets.otherwise.index()].terminator.kind,
                    TerminatorKind::Abort { .. }
                )
            {
                return None;
            }
            *ok
        }
        _ => return None,
    };
    super::counters::dominates(idom, ok.index(), bi).then_some(up)
}

/// The one place `l` is written, as a whole local and by nothing else.
fn only_write(body: &Body, l: Local) -> Option<(usize, usize)> {
    let mut found = None;
    for (bi, b) in body.blocks.iter().enumerate() {
        for (si, s) in b.statements.iter().enumerate() {
            match &s.kind {
                StatementKind::Assign(p, rv) => {
                    if p.local == l {
                        if found.is_some() || !p.projection.is_empty() {
                            return None;
                        }
                        found = Some((bi, si));
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
                return None;
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::LocalKind;

    /// The monotone counters of `f`, as `name+` (up) or `name-` (down),
    /// sorted; `f` is `fn f(n: i64) -> i64 { <body> }`.
    fn found(body: &str) -> Vec<String> {
        let src = format!(
            concat!(
                "fn bump(x: mut ref i64) {{\n    *x = x + 1;\n}}\n",
                "fn f(n: i64) -> i64 {{\n{body}\n}}\nfn main() {{\n    println(f(9));\n}}\n"
            ),
            body = body
        );
        let l = build_source(&src).unwrap_or_else(|e| panic!("{e}"));
        let b = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == "f")
            .expect("f");
        let mut out: Vec<String> = super::monotones(b, &l.tys)
            .iter()
            .map(|m| {
                let name = match &b.local(m.local).kind {
                    LocalKind::User { name, .. } | LocalKind::Arg { name, .. } => name.clone(),
                    _ => format!("{}", m.local),
                };
                format!("{name}{}", if m.up { '+' } else { '-' })
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn monotone_counters_are_found_stepped_or_conditional() {
        // A counter, and a write head stepped only on some trips.
        assert_eq!(
            found(concat!(
                "    let mut i = 0;\n",
                "    let mut w = 0;\n",
                "    while i < n {\n",
                "        if i % 3 == 0 { w = w + 1; }\n",
                "        i = i + 1;\n",
                "    }\n",
                "    w",
            )),
            ["i+", "w+"]
        );
        // Counting down by a constant.
        assert_eq!(
            found(concat!(
                "    let mut k = n;\n",
                "    let mut s = 0;\n",
                "    while k > 0 {\n",
                "        s = s + k;\n",
                "        k = k - 2;\n",
                "    }\n",
                "    s",
            )),
            ["k-"]
        );
    }

    /// A plain assignment, steps both ways, a variable step and a `mut`
    /// borrow each leave the counter unstated; the other counter stays.
    #[test]
    fn any_other_write_states_nothing() {
        let shape = |extra: &str| {
            found(&format!(
                concat!(
                    "    let mut i = 0;\n",
                    "    let mut j = 0;\n",
                    "    while i < n {{\n",
                    "        j = j + 1;\n",
                    "{}",
                    "        i = i + 1;\n",
                    "    }}\n",
                    "    j",
                ),
                extra
            ))
        };
        assert_eq!(shape(""), ["i+", "j+"]);
        assert_eq!(shape("        if j > 5 { j = 0; }\n"), ["i+"]);
        assert_eq!(shape("        if j > 5 { j = j - 1; }\n"), ["i+"]);
        assert_eq!(shape("        j = j + i;\n"), ["i+"]);
        assert_eq!(shape("        bump(mut j);\n"), ["i+"]);
    }
}
