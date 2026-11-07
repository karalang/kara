//! Halves a value known non-negative with a shift (B-2026-11-06-8).
//!
//! `x / 2` on a signed integer truncates toward zero, so it compiles to a
//! shift plus a fixup that adds the sign bit first (`add x, x, lsr #63;
//! asr #1` on arm64), and the binary search `(lo + hi) / 2` of kata 354
//! is not taken by [`super::midpoint`] at all: on a negative sum the
//! truncation rounds up, onto `hi` (B-2026-08-30-6). When `x` cannot be
//! negative, truncating and flooring agree, so `x / 2^k` is `x >> k`
//! exactly: this pass rewrites the division to the shift, which drops the
//! fixup and lets the midpoint's range facts fold the search's index
//! check. A shift by `k < bits` never sets the checked form's flag, which
//! folds as well.
//!
//! "Known non-negative" is an optimistic fixpoint over the body's locals:
//! every local starts assumed non-negative, and one loses that the moment
//! some write of it is not built from non-negative values (a literal
//! `>= 0`, an unsigned value widened, a native `.len`, and sums, products,
//! quotients and remainders of non-negative values, the checked forms
//! aborting before an overflowed sum exists). A parameter, a local lent
//! mutably and a local written through a projection lose it outright. The
//! loop-carried `lo = mid + 1` / `hi = mid` of a search starting at `0`
//! and `len` keeps it, which a single forward pass could not show.

use super::syntax::*;
use super::ty::{Ty, TyInterner, TyKind};

/// Rewrites each signed `x / 2^k` (`k >= 1`) whose `x` is non-negative to
/// `x >> k`, in place; `native(name)` says whether a callee is the
/// library's own, so a `.len` is a count. Returns how many it rewrote.
pub fn halve_non_negative(
    body: &mut Body,
    tys: &TyInterner,
    native: &dyn Fn(&str) -> bool,
) -> usize {
    let nonneg = non_negative(body, tys, native);
    let mut n = 0;
    for b in body.blocks.iter_mut() {
        for s in b.statements.iter_mut() {
            let StatementKind::Assign(_, rv) = &mut s.kind else {
                continue;
            };
            let (Rvalue::BinaryOp(op, a, c) | Rvalue::CheckedBinaryOp(op, a, c)) = rv else {
                continue;
            };
            if *op != BinOp::Div {
                continue;
            }
            let Operand::Const(Const {
                ty,
                kind: ConstKind::Scalar(v),
            }) = c
            else {
                continue;
            };
            let TyKind::Int(it) = tys.kind(*ty) else {
                continue;
            };
            if !it.signed() || *v < 2 || !v.is_power_of_two() || *v >> (it.bits() - 1) != 0 {
                continue;
            }
            if !operand_nonneg(a, &nonneg, tys) {
                continue;
            }
            *op = BinOp::Shr;
            *v = v.trailing_zeros() as u128;
            n += 1;
        }
    }
    n
}

/// What [`non_negative`] found, with each local's type for the operands
/// read straight out of an unsigned local.
struct NonNeg {
    yes: Vec<bool>,
    tys: Vec<Ty>,
}

/// Per local: whether every value it can hold is `>= 0` (for a checked
/// pair, its field 0 whenever the flag is clear).
fn non_negative(body: &Body, tys: &TyInterner, native: &dyn Fn(&str) -> bool) -> NonNeg {
    let count = body.locals.len();
    let local_tys: Vec<Ty> = (0..count).map(|i| body.local(Local(i as u32)).ty).collect();
    // Each local's writes; `None` marks one that disqualifies it outright.
    let mut defs: Vec<Vec<Option<&Rvalue>>> = vec![Vec::new(); count];
    let mut lens: Vec<bool> = vec![false; count];
    for a in body.args() {
        defs[a.index()].push(None);
    }
    defs[0].push(None);
    for b in &body.blocks {
        for s in &b.statements {
            match &s.kind {
                StatementKind::Assign(d, rv) => {
                    defs[d.local.index()].push(d.projection.is_empty().then_some(rv));
                    if let Rvalue::Ref(BorrowKind::Mut, p) = rv {
                        defs[p.local.index()].push(None);
                    }
                }
                StatementKind::SetDiscriminant(d, _) => defs[d.local.index()].push(None),
                _ => {}
            }
        }
        if let TerminatorKind::Call {
            func, destination, ..
        } = &b.terminator.kind
        {
            let len = matches!(func, Operand::Const(Const { kind: ConstKind::FnDef(i), .. })
                if i.name.ends_with(".len") && native(&i.name));
            if len && destination.projection.is_empty() {
                lens[destination.local.index()] = true;
            } else {
                defs[destination.local.index()].push(None);
            }
        }
    }
    let mut n = NonNeg {
        yes: (0..count)
            .map(|i| !defs[i].contains(&None) && (lens[i] || !defs[i].is_empty()))
            .collect(),
        tys: local_tys,
    };
    loop {
        let mut changed = false;
        for (i, d) in defs.iter().enumerate() {
            if n.yes[i] && !d.iter().all(|rv| rv.is_some_and(|rv| keeps(rv, &n, tys))) {
                n.yes[i] = false;
                changed = true;
            }
        }
        if !changed {
            return n;
        }
    }
}

/// Whether `rv` builds a non-negative value from what `n` assumes.
fn keeps(rv: &Rvalue, n: &NonNeg, tys: &TyInterner) -> bool {
    let nn = |o: &Operand| operand_nonneg(o, n, tys);
    match rv {
        Rvalue::Use(o) => nn(o),
        Rvalue::Len(_) => true,
        Rvalue::CheckedBinaryOp(op, a, c) | Rvalue::BinaryOp(op, a, c) => {
            let checked = matches!(rv, Rvalue::CheckedBinaryOp(..));
            match op {
                // Unchecked, only the builder's counter step `i + 1`, whose
                // loop guard keeps it below the bound.
                BinOp::Add if checked => nn(a) && nn(c),
                BinOp::Add => nn(a) && nn(c) && (is_const(a) || is_const(c)),
                BinOp::Mul if checked => nn(a) && nn(c),
                BinOp::Div | BinOp::Rem => nn(a) && positive(c, tys),
                BinOp::Shr => nn(a),
                BinOp::BitAnd => nn(a) || nn(c),
                _ => false,
            }
        }
        Rvalue::Cast(CastKind::IntToInt, Operand::Copy(p) | Operand::Move(p), to) => {
            p.projection.is_empty()
                && match (tys.kind(n.tys[p.local.index()]), tys.kind(*to)) {
                    (TyKind::Int(from), TyKind::Int(to)) => {
                        !from.signed() && (from.bits() < to.bits() || !to.signed())
                    }
                    _ => false,
                }
        }
        _ => false,
    }
}

fn operand_nonneg(o: &Operand, n: &NonNeg, tys: &TyInterner) -> bool {
    match o {
        Operand::Const(Const {
            ty,
            kind: ConstKind::Scalar(v),
        }) => match tys.kind(*ty) {
            TyKind::Int(it) => !it.signed() || v >> (it.bits() - 1) & 1 == 0,
            _ => false,
        },
        Operand::Const(_) => false,
        Operand::Copy(p) | Operand::Move(p) => match p.projection.as_slice() {
            [] => {
                n.yes[p.local.index()]
                    || matches!(tys.kind(n.tys[p.local.index()]), TyKind::Int(it) if !it.signed())
            }
            [ProjElem::Field(f, _)] if f.index() == 0 => n.yes[p.local.index()],
            _ => false,
        },
    }
}

fn is_const(o: &Operand) -> bool {
    matches!(o, Operand::Const(_))
}

fn positive(o: &Operand, tys: &TyInterner) -> bool {
    match o {
        Operand::Const(Const {
            ty,
            kind: ConstKind::Scalar(v),
        }) => match tys.kind(*ty) {
            TyKind::Int(it) => *v != 0 && (!it.signed() || v >> (it.bits() - 1) & 1 == 0),
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::syntax::*;
    use crate::mir::lower::build_source;

    /// `f` of `src` as built, with the divisions and shifts left in it and
    /// the midpoints [`crate::mir::midpoint`] finds.
    fn shape(src: &str) -> (usize, usize, usize) {
        let l = build_source(src).expect("MIR");
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == "f")
            .expect("f");
        let mut divs = 0;
        let mut shrs = 0;
        for b in &body.blocks {
            for s in &b.statements {
                if let StatementKind::Assign(
                    _,
                    Rvalue::BinaryOp(op, ..) | Rvalue::CheckedBinaryOp(op, ..),
                ) = &s.kind
                {
                    divs += (*op == BinOp::Div) as usize;
                    shrs += (*op == BinOp::Shr) as usize;
                }
            }
        }
        let mids = crate::mir::midpoint::midpoints(body, &l.tys).len();
        (divs, shrs, mids)
    }

    fn search(init_lo: &str, hi: &str, halve: &str) -> String {
        format!(
            concat!(
                "struct Bag {{ n: i64 }}\n",
                "impl Bag {{\n",
                "    fn len(self) -> i64 {{ self.n - 9 }}\n",
                "}}\n",
                "fn f(v: Vec[i64], x: i64) -> i64 {{\n",
                "    let b = Bag {{ n: x }};\n",
                "    let mut lo = {init_lo};\n",
                "    let mut hi = {hi};\n",
                "    while lo < hi {{\n",
                "        let mid = (lo + hi) {halve};\n",
                "        if v[mid] < x {{ lo = mid + 1; }} else {{ hi = mid; }}\n",
                "    }}\n",
                "    lo\n",
                "}}\n",
                "fn main() {{\n",
                "    println(f([1, 3, 5, 7], 5));\n",
                "}}\n",
            ),
            init_lo = init_lo,
            hi = hi,
            halve = halve,
        )
    }

    /// Kata 354's search: `lo` from `0`, `hi` from `len`, each carried
    /// round the loop through `mid`. The sum is a shift, and the midpoint
    /// it now is gets its range facts.
    #[test]
    fn a_search_from_zero_to_len_halves_with_a_shift() {
        assert_eq!(shape(&search("0", "v.len()", "/ 2")), (0, 1, 1));
        assert_eq!(shape(&search("0", "v.len()", "/ 4")), (0, 1, 0));
    }

    /// A `lo` that can start negative, a parameter, a divisor that is not
    /// a power of two, and a user's own `.len` keep the division.
    #[test]
    fn a_value_that_may_be_negative_keeps_its_division() {
        assert_eq!(shape(&search("0 - 1", "v.len()", "/ 2")), (1, 0, 0));
        assert_eq!(shape(&search("x", "v.len()", "/ 2")), (1, 0, 0));
        assert_eq!(shape(&search("0", "v.len()", "/ 3")), (1, 0, 0));
        assert_eq!(shape(&search("0", "b.len()", "/ 2")), (1, 0, 0));
    }

    /// A mutable loan can write anything; so can a parameter's caller.
    #[test]
    fn a_lent_or_passed_value_keeps_its_division() {
        let src = concat!(
            "fn bump(x: mut ref i64) { *x = *x - 9; }\n",
            "fn f(a: i64) -> i64 {\n",
            "    let mut n = 4;\n",
            "    bump(mut n);\n",
            "    n / 2 + a / 2\n",
            "}\n",
            "fn main() {\n",
            "    println(f(3));\n",
            "}\n",
        );
        assert_eq!(shape(src), (2, 0, 0));
    }
}
