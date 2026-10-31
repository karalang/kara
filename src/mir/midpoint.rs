//! Finds the midpoint of a binary search, for the backend to state its
//! range (B-2026-10-28-41; legacy's B-2026-06-16-1).
//!
//! In `while lo < hi { let mid = lo + (hi - lo) / 2; ... nums[mid] ... }`
//! the `nums[mid]` check survives because folding `mid < len` needs the
//! relation `mid < hi`, which LLVM's interval-based range analysis cannot
//! derive from `mid`'s definition. Stated as `assume(mid >= lo)` and
//! `assume(mid < hi)` right after `mid` is computed, it folds: kata 153's
//! search loop was 14 instructions an iteration to legacy's 9.
//!
//! Both facts hold locally. The guard `lo < hi` makes `hi - lo >= 1`, and
//! halving that (truncating or flooring, which agree on a positive value)
//! lands in `[0, hi - lo - 1]`, so `lo <= mid <= hi - 1`. The sum form
//! `(lo + hi) / 2` is in `[lo, hi)` only when the halving floors: `/ 2` on
//! a negative signed sum rounds up, and at `lo = -3, hi = -2` gives `-2 ==
//! hi` (B-2026-08-30-6, where legacy's `assume(mid < hi)` became
//! `assume(false)`). So it is taken with `>> 1`, or with `/ 2` on an
//! unsigned type. A checked step that overflows aborts before `mid` exists,
//! and `lo` and `hi` are not written between the guard and `mid`, so they
//! still hold the values the facts are about.

use super::syntax::*;
use super::ty::{TyInterner, TyKind};

/// A midpoint: after statement `statement` of block `block`, `mid` holds
/// a value in `[lo, hi)`, compared as `signed` integers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Midpoint {
    pub block: usize,
    pub statement: usize,
    pub mid: Local,
    pub lo: Local,
    pub hi: Local,
    pub signed: bool,
}

/// What a local holds, in terms of the guard's `lo` and `hi`; `Checked`
/// wraps a `(value, overflowed)` pair whose field 0 is the value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sym {
    /// `hi - lo`.
    Diff,
    /// `lo + hi`.
    Sum,
    /// `(hi - lo) / 2`, either halving.
    Half,
    /// A value in `[lo, hi)`.
    Mid,
}

/// Every midpoint of `body`: one per `lo < hi` guard whose way in
/// computes one.
pub fn midpoints(body: &Body, tys: &TyInterner) -> Vec<Midpoint> {
    let preds = super::counters::predecessors(body);
    let mut out = Vec::new();
    for (g, data) in body.blocks.iter().enumerate() {
        let Some((lo, hi, signed, inside)) = guard(body, tys, data) else {
            continue;
        };
        if let Some(m) = walk(body, &preds, g, inside, lo, hi, signed) {
            out.push(m);
        }
    }
    out
}

/// For a block ending `switchInt(_g)` on `_g = Lt(lo, hi)` (two distinct
/// integer locals of one type, neither written after the compare): `lo`,
/// `hi`, whether they are signed, and the block taken when `lo < hi`.
fn guard(
    body: &Body,
    tys: &TyInterner,
    data: &BasicBlockData,
) -> Option<(Local, Local, bool, usize)> {
    let TerminatorKind::SwitchInt {
        discr: Operand::Copy(d),
        targets,
    } = &data.terminator.kind
    else {
        return None;
    };
    if !d.projection.is_empty() || targets.values.len() != 1 || targets.values[0].0 != 0 {
        return None;
    }
    let at = data
        .statements
        .iter()
        .rposition(|s| matches!(&s.kind, StatementKind::Assign(p, _) if *p == *d))?;
    let StatementKind::Assign(_, Rvalue::BinaryOp(BinOp::Lt, Operand::Copy(l), Operand::Copy(h))) =
        &data.statements[at].kind
    else {
        return None;
    };
    if !l.projection.is_empty() || !h.projection.is_empty() || l.local == h.local {
        return None;
    }
    let (lo, hi) = (l.local, h.local);
    let ty = body.local(lo).ty;
    let TyKind::Int(it) = tys.kind(ty) else {
        return None;
    };
    if body.local(hi).ty != ty
        || data.statements[at + 1..]
            .iter()
            .any(|s| writes(&s.kind, lo, hi))
    {
        return None;
    }
    Some((lo, hi, it.signed(), targets.otherwise.index()))
}

/// Follows the straight line from `start` (entered only from the guard
/// `g`, past checks that abort) to the first local assigned a midpoint.
fn walk(
    body: &Body,
    preds: &[Vec<usize>],
    g: usize,
    start: usize,
    lo: Local,
    hi: Local,
    signed: bool,
) -> Option<Midpoint> {
    // (local, what it holds, whether it is a checked pair)
    let mut known: Vec<(Local, Sym, bool)> = Vec::new();
    let mut b = start;
    let mut from = g;
    for _ in 0..32 {
        if preds[b] != [from] {
            return None;
        }
        for (si, s) in body.blocks[b].statements.iter().enumerate() {
            if writes(&s.kind, lo, hi) {
                return None;
            }
            let StatementKind::Assign(dest, rv) = &s.kind else {
                continue;
            };
            known.retain(|(l, ..)| *l != dest.local);
            if !dest.projection.is_empty() {
                continue;
            }
            let sym = |o: &Operand| -> Option<Sym> {
                match o {
                    Operand::Copy(p) | Operand::Move(p) => {
                        let unwrap = match p.projection.as_slice() {
                            [] => false,
                            [ProjElem::Field(f, _)] if f.index() == 0 => true,
                            _ => return None,
                        };
                        known
                            .iter()
                            .find(|(l, _, checked)| *l == p.local && *checked == unwrap)
                            .map(|(_, s, _)| *s)
                    }
                    Operand::Const(_) => None,
                }
            };
            let local = |o: &Operand, want: Local| matches!(o, Operand::Copy(p) | Operand::Move(p) if *p == Place::local(want));
            let (op, a, c, checked) = match rv {
                Rvalue::BinaryOp(op, a, c) => (*op, a, c, false),
                Rvalue::CheckedBinaryOp(op, a, c) => (*op, a, c, true),
                Rvalue::Use(a) => {
                    if let Some(s) = sym(a) {
                        known.push((dest.local, s, false));
                        if s == Sym::Mid {
                            return Some(Midpoint {
                                block: b,
                                statement: si,
                                mid: dest.local,
                                lo,
                                hi,
                                signed,
                            });
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            let got = match op {
                BinOp::Sub if local(a, hi) && local(c, lo) => Some(Sym::Diff),
                BinOp::Add if (local(a, lo) && local(c, hi)) || (local(a, hi) && local(c, lo)) => {
                    Some(Sym::Sum)
                }
                BinOp::Add
                    if (local(a, lo) && sym(c) == Some(Sym::Half))
                        || (sym(a) == Some(Sym::Half) && local(c, lo)) =>
                {
                    Some(Sym::Mid)
                }
                BinOp::Div if is_const(c, 2) => match sym(a) {
                    Some(Sym::Diff) => Some(Sym::Half),
                    // Truncation equals flooring only on unsigned values.
                    Some(Sym::Sum) if !signed => Some(Sym::Mid),
                    _ => None,
                },
                BinOp::Shr if is_const(c, 1) => match sym(a) {
                    Some(Sym::Diff) => Some(Sym::Half),
                    Some(Sym::Sum) => Some(Sym::Mid),
                    _ => None,
                },
                _ => None,
            };
            if let Some(s) = got {
                known.push((dest.local, s, checked));
                if s == Sym::Mid && !checked {
                    return Some(Midpoint {
                        block: b,
                        statement: si,
                        mid: dest.local,
                        lo,
                        hi,
                        signed,
                    });
                }
            }
        }
        let term = &body.blocks[b].terminator.kind;
        if !matches!(
            term,
            TerminatorKind::Goto { .. } | TerminatorKind::SwitchInt { .. }
        ) {
            return None;
        }
        let on: Vec<usize> = term
            .successors()
            .into_iter()
            .map(|s| s.index())
            .filter(|&s| !matches!(body.blocks[s].terminator.kind, TerminatorKind::Abort { .. }))
            .collect();
        let [next] = on.as_slice() else {
            return None;
        };
        from = b;
        b = *next;
    }
    None
}

/// Whether `s` writes `lo` or `hi`, or lends either mutably.
fn writes(s: &StatementKind, lo: Local, hi: Local) -> bool {
    match s {
        StatementKind::Assign(d, rv) => {
            d.local == lo
                || d.local == hi
                || matches!(rv, Rvalue::Ref(BorrowKind::Mut, p) if p.local == lo || p.local == hi)
        }
        StatementKind::SetDiscriminant(d, _) => d.local == lo || d.local == hi,
        _ => false,
    }
}

fn is_const(o: &Operand, v: u128) -> bool {
    matches!(o, Operand::Const(Const { kind: ConstKind::Scalar(x), .. }) if *x == v)
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;

    /// The midpoints found in `f` of `fn f(lo0, hi0) { while <guard> { let
    /// mid = <mid>; ... } }`, with `lo` and `hi` of type `ty`.
    fn found(ty: &str, guard: &str, mid: &str, before: &str) -> usize {
        let src = format!(
            concat!(
                "fn f(lo0: {ty}, hi0: {ty}) -> {ty} {{\n",
                "    let mut lo = lo0;\n",
                "    let mut hi = hi0;\n",
                "    while {guard} {{\n",
                "{before}",
                "        let mid = {mid};\n",
                "        if mid > lo {{ hi = mid; }} else {{ lo = mid + 1; }}\n",
                "    }}\n",
                "    lo\n",
                "}}\n",
                "fn main() {{\n",
                "    println(f(0, 9));\n",
                "}}\n",
            ),
            ty = ty,
            guard = guard,
            before = before,
            mid = mid,
        );
        let l = build_source(&src).expect("MIR");
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == "f")
            .expect("f");
        super::midpoints(body, &l.tys).len()
    }

    #[test]
    fn the_canonical_midpoint_is_found_signed_and_unsigned() {
        assert_eq!(found("i64", "lo < hi", "lo + (hi - lo) / 2", ""), 1);
        assert_eq!(found("u64", "lo < hi", "lo + (hi - lo) / 2u64", ""), 1);
        assert_eq!(found("i64", "lo < hi", "(hi - lo) / 2 + lo", ""), 1);
        assert_eq!(found("i64", "lo < hi", "lo + ((hi - lo) >> 1)", ""), 1);
    }

    /// `(lo + hi) / 2` rounds a negative signed sum up, onto `hi` at `hi ==
    /// lo + 1` (B-2026-08-30-6): only the flooring `>> 1`, or `/ 2` on an
    /// unsigned type, is taken.
    #[test]
    fn the_sum_form_needs_a_flooring_halve() {
        assert_eq!(found("i64", "lo < hi", "(lo + hi) / 2", ""), 0);
        assert_eq!(found("i64", "lo < hi", "(lo + hi) >> 1", ""), 1);
        assert_eq!(found("u64", "lo < hi", "(lo + hi) / 2u64", ""), 1);
    }

    /// `lo <= hi` admits `lo == hi`, where `mid == hi`; and a `hi` written
    /// between the guard and `mid` is no longer the one the guard compared.
    #[test]
    fn a_closed_guard_or_a_moved_bound_states_nothing() {
        assert_eq!(found("i64", "lo <= hi", "lo + (hi - lo) / 2", ""), 0);
        assert_eq!(
            found(
                "i64",
                "lo < hi",
                "lo + (hi - lo) / 2",
                "        hi = hi + 1;\n"
            ),
            0
        );
        assert_eq!(found("i64", "lo < hi", "lo + (hi - lo) / 3", ""), 0);
    }
}
