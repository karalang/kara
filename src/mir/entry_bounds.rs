//! Index calls on a counter that only falls from a value already checked
//! against the same vector (B-2026-11-26-79).
//!
//! Kata 1409 turns the part of `p` up to a found place one step along:
//!
//! ```text
//! let x = p[at];                  // checks at as u64 <u len(p)
//! let mut i = at;
//! while i > 0 { p[i] = p[i - 1]; i -= 1; }
//! ```
//!
//! Both checks in the loop pass: `i` starts at `at`, which `p[at]` just
//! proved below the length, and only falls, and `p`'s length stays as it
//! is (`index_mut` keeps it). LLVM does not see it: the counter's start is
//! a value it cannot bound by the length (rustc's loop is a `memmove` only
//! because its `position` states `at < len`, and not when `at` is a
//! parameter). With a check left per element, the loop is a scalar copy;
//! with the upper halves proved here, the lower ones (`i >= 0`, and
//! `i - 1 >= 0` under `i > 0`) fold, and LLVM makes the loop the `memmove`
//! it is. Kata 1409's bench went from 0.094 s to 0.057 s (Rust 0.038 s).
//!
//! The shape, for an index call in block `B` on `X` at `s as u64`, with
//! `s` a signed local:
//!
//! - `s` is a counter `c` of a loop `L` holding `B` that only falls
//!   ([`super::monotone`]), or is written once, in `L`, in a block that
//!   dominates `B`, by `copy c` or by `copy t.0` of `t = CheckedSub(c, k)`
//!   (`k >= 0`) past its overflow test, through a few copies. Every value
//!   `c` takes in `L` is at most `c0`, its value as `L` was last entered,
//!   so `s <= c0`;
//! - `L`'s one entry block sets `c = copy a`, and walking up from it
//!   through one-predecessor blocks that write neither `a` nor `X` reaches
//!   the return edge of an index call on `X` at `a as u64`: `0 <= a <
//!   len(X)` there, and `c0 = a`;
//! - `X`'s length is the same from that check to `B`: `X` is fixed while
//!   it can be indexed ([`super::guard_bounds`]' sense), or it is a local
//!   vector that is only ever borrowed into the receiver of its block's own
//!   call, and that, in `L` and on the walk, nothing assigns, moves, drops
//!   or ends, and only a method that keeps the length borrows mutably.
//!
//! Then `s <s len(X)`, and codegen checks only `s >= 0`.
//! `KARAC_MIR_ENTRY_BOUNDS=0` proves nothing here.

use super::guard_bounds::{defining, fn_name, frozen, operand_local, writes};
use super::monotone::{monotones, only_write, Monotone};
use super::range_bounds::KEEPS_LENGTH;
use super::rc_pairs::{statement_mentions, terminator_mentions};
use super::syntax::*;
use super::ty::{IntrinsicTy, TyInterner, TyKind};

/// How far the walk from a loop's entry up to the check goes.
const WALK: usize = 16;
/// How many copies `s` is followed through to the counter.
const COPIES: usize = 4;

/// The blocks of `body` whose terminator is an index call whose upper
/// bound the counter's checked start proves.
pub fn upper_proven_indexes(body: &Body, tys: &TyInterner) -> Vec<usize> {
    if body.blocks.is_empty() || std::env::var("KARAC_MIR_ENTRY_BOUNDS").as_deref() == Ok("0") {
        return Vec::new();
    }
    let falling: Vec<Monotone> = monotones(body, tys)
        .into_iter()
        .filter(|m| !m.up && m.signed)
        .collect();
    if falling.is_empty() {
        return Vec::new();
    }
    let preds = super::counters::predecessors(body);
    let idom = super::counters::dominators(body, &preds);
    (0..body.blocks.len())
        .filter(|&bi| proven(body, tys, &preds, &idom, &falling, bi).is_some())
        .collect()
}

/// An index call ending `b`: the method's type (`Vec[i32]`), the place
/// indexed, the local cast to the index, the statement that casts it, and
/// where the call returns.
struct IndexCall<'b> {
    base: &'b str,
    x: Place,
    h: Local,
    cast: usize,
    target: usize,
}

fn index_call(b: &BasicBlockData) -> Option<IndexCall<'_>> {
    let TerminatorKind::Call {
        func, args, target, ..
    } = &b.terminator.kind
    else {
        return None;
    };
    let (base, method) = fn_name(func)?.rsplit_once('.')?;
    if !matches!(method, "index" | "index_mut") || args.len() != 2 {
        return None;
    }
    let recv = operand_local(&args[0])?;
    let idx = operand_local(&args[1])?;
    let k = b.statements.len();
    let (_, x) = defining(b, k, recv, |rv| match rv {
        Rvalue::Ref(_, p) => Some(p.clone()),
        _ => None,
    })?;
    let (cast, h) = defining(b, k, idx, |rv| match rv {
        Rvalue::Cast(CastKind::IntToInt, Operand::Copy(p), _) if p.projection.is_empty() => {
            Some(p.local)
        }
        _ => None,
    })?;
    Some(IndexCall {
        base,
        x,
        h,
        cast,
        target: target.as_ref()?.index(),
    })
}

fn proven(
    body: &Body,
    tys: &TyInterner,
    preds: &[Vec<usize>],
    idom: &[usize],
    falling: &[Monotone],
    bi: usize,
) -> Option<()> {
    let call = index_call(&body.blocks[bi])?;
    if !signed(body, tys, call.h) {
        return None;
    }
    let m = bounding_counter(body, idom, falling, bi, call.h)?;
    // `c = copy a` in the entry block, `a` unwritten before it there.
    let e = &body.blocks[m.entry];
    let (j, a) = defining(e, e.statements.len(), m.local, |rv| match rv {
        Rvalue::Use(Operand::Copy(p) | Operand::Move(p)) if p.projection.is_empty() => {
            Some(p.local)
        }
        _ => None,
    })?;
    if !signed(body, tys, a) || e.statements[..j].iter().any(|s| writes(s, a)) {
        return None;
    }
    let mut region = m.blocks.clone();
    region[m.entry] = true;
    // Up to the check of `a` against the same `X`.
    let mut w = m.entry;
    let mut checked = false;
    for _ in 0..WALK {
        let [p] = preds[w].as_slice() else {
            return None;
        };
        let p = *p;
        let pb = &body.blocks[p];
        if let Some(c) = index_call(pb) {
            if c.target == w
                && c.base == call.base
                && c.x == call.x
                && c.h == a
                && !pb.statements[c.cast + 1..].iter().any(|s| writes(s, a))
            {
                checked = true;
                break;
            }
        }
        if pb.statements.iter().any(|s| writes(s, a)) {
            return None;
        }
        match &pb.terminator.kind {
            TerminatorKind::Goto { .. } => {}
            TerminatorKind::Call { destination, .. } if destination.local != a => {}
            TerminatorKind::Drop { place, .. } if place.local != call.x.local => {}
            _ => return None,
        }
        region[p] = true;
        w = p;
    }
    (checked && length_kept(body, tys, &call.x, &region)).then_some(())
}

fn signed(body: &Body, tys: &TyInterner, l: Local) -> bool {
    matches!(tys.kind(body.local(l).ty), TyKind::Int(i) if i.signed())
}

/// The falling counter of a loop holding `bi` that `s` is at most.
fn bounding_counter<'m>(
    body: &Body,
    idom: &[usize],
    falling: &'m [Monotone],
    bi: usize,
    s: Local,
) -> Option<&'m Monotone> {
    let counter = |c: Local, at: usize| {
        falling
            .iter()
            .find(|m| m.local == c && m.blocks[bi] && m.blocks[at])
    };
    if let Some(m) = counter(s, bi) {
        return Some(m);
    }
    let mut s = s;
    for _ in 0..COPIES {
        let (db, ds) = only_write(body, s)?;
        if !super::counters::dominates(idom, db, bi) {
            return None;
        }
        let StatementKind::Assign(_, Rvalue::Use(Operand::Copy(src) | Operand::Move(src))) =
            &body.blocks[db].statements[ds].kind
        else {
            return None;
        };
        match src.projection.as_slice() {
            [] => {
                if let Some(m) = counter(src.local, db) {
                    return Some(m);
                }
                s = src.local;
            }
            [ProjElem::Field(FieldIdx(0), _)] => {
                let c = falling_from(body, idom, src.local, db)?;
                return counter(c, db);
            }
            _ => return None,
        }
    }
    None
}

/// `c` when `pair` is written once, by `CheckedSub(c, k)` with a constant
/// `k >= 0`, in a block whose overflow test's no-overflow side dominates
/// `read`, the block that reads `pair.0`: the difference is then exact and
/// at most `c`.
fn falling_from(body: &Body, idom: &[usize], pair: Local, read: usize) -> Option<Local> {
    let (pb, ps) = only_write(body, pair)?;
    let block = &body.blocks[pb];
    let StatementKind::Assign(
        _,
        Rvalue::CheckedBinaryOp(BinOp::Sub, Operand::Copy(c) | Operand::Move(c), Operand::Const(k)),
    ) = &block.statements[ps].kind
    else {
        return None;
    };
    let ConstKind::Scalar(k) = k.kind else {
        return None;
    };
    // A negative `k` is a large scalar; only a small one is a step down.
    if !c.projection.is_empty() || k > i64::MAX as u128 {
        return None;
    }
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
    let tested = f.local == pair
        && matches!(f.projection.as_slice(), [ProjElem::Field(FieldIdx(1), _)])
        && matches!(
            body.blocks[targets.otherwise.index()].terminator.kind,
            TerminatorKind::Abort { .. }
        );
    (tested && super::counters::dominates(idom, ok.index(), read)).then_some(c.local)
}

/// Whether `x`'s length is the same throughout `region`.
fn length_kept(body: &Body, tys: &TyInterner, x: &Place, region: &[bool]) -> bool {
    if frozen(body, tys, x) {
        return true;
    }
    let v = x.local;
    if !x.projection.is_empty()
        || !matches!(
            tys.kind(body.local(v).ty),
            TyKind::Intrinsic(IntrinsicTy::Vec(_))
        )
    {
        return false;
    }
    for (bi, b) in body.blocks.iter().enumerate() {
        for (si, st) in b.statements.iter().enumerate() {
            match &st.kind {
                StatementKind::Assign(d, Rvalue::Ref(kind, q)) if q.local == v => {
                    // `t = &x` or `t = &mut x` for this block's own call,
                    // `t` used by nothing else here.
                    let TerminatorKind::Call { func, args, .. } = &b.terminator.kind else {
                        return false;
                    };
                    if q != x
                        || !d.projection.is_empty()
                        || args.first().and_then(operand_local) != Some(d.local)
                        || args[1..].iter().any(|a| operand_local(a) == Some(d.local))
                        || b.statements[si + 1..]
                            .iter()
                            .any(|s| statement_mentions(&s.kind, d.local))
                    {
                        return false;
                    }
                    if region[bi] && *kind == BorrowKind::Mut {
                        let method = fn_name(func).and_then(|n| n.rsplit_once('.'));
                        if !method.is_some_and(|(_, m)| KEEPS_LENGTH.contains(&m)) {
                            return false;
                        }
                    }
                }
                k if region[bi] && statement_mentions(k, v) => return false,
                _ => {}
            }
        }
        if region[bi] && terminator_mentions(&b.terminator.kind, v) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;

    /// How many index calls of `f` this pass proves.
    fn proved(src: &str, f: &str) -> usize {
        let l = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == f)
            .unwrap_or_else(|| panic!("no body {f}"));
        super::upper_proven_indexes(body, &l.tys).len()
    }

    fn turn(prelude: &str, inside: &str) -> String {
        format!(
            "fn turn(q: i32, m: i32) -> i32 {{
    let mut p: Vec[i32] = (1..=m).collect();
    let at = p.iter().position(|x| x == q).unwrap();
{prelude}    let x = p[at];
    let mut i = at;
    while i > 0 {{
        p[i] = p[i - 1];
{inside}        i -= 1;
    }}
    p[0] = x;
    p[1]
}}
fn main() {{
    println(turn(3, 5));
}}
"
        )
    }

    /// Kata 1409's turn: both indexes in the loop are proved.
    #[test]
    fn a_falling_counter_from_a_checked_start_is_in_bounds() {
        assert_eq!(proved(&turn("", ""), "turn"), 2);
    }

    /// A push in the loop changes the length, and a start the check did not
    /// see proves nothing.
    #[test]
    fn a_changed_length_or_an_unchecked_start_proves_nothing() {
        assert_eq!(proved(&turn("", "        p.push(0);\n"), "turn"), 0);
        let unchecked = turn("", "").replace("let mut i = at;", "let mut i = at + 1;");
        assert_eq!(proved(&unchecked, "turn"), 0);
        // A pop between the check and the loop.
        let popped = turn("", "").replace("let mut i = at;", "p.pop();\n    let mut i = at;");
        assert_eq!(proved(&popped, "turn"), 0);
    }

    /// A counter that can rise is no bound.
    #[test]
    fn a_counter_that_rises_proves_nothing() {
        assert_eq!(
            proved(&turn("", "        if p[0] == 9 { i += 2; }\n"), "turn"),
            0
        );
    }
}
