//! Index calls a value-range proof shows in bounds (B-2026-10-28-40).
//!
//! [`super::guard_bounds`] proves `h <s n` from a guard against a length
//! read once. Kata 139's DP indexes a local vector filled by a counting
//! loop and never resized, at an index bounded by the loops around it, not
//! by a compare with its length:
//!
//! ```text
//! while d <= win { dp.push(false); d = d + 1 }    // len(dp) = win + 1
//! ...
//! let mut j = ii - 1;                              // ii <= win
//! while j >= low { ... if dp[j] { ... } j = j - 1 }
//! ```
//!
//! The legacy backend proved these on the AST (`bce_length_pin.rs`,
//! B-2026-07-17-1). Here a forward dataflow over the elaborated body keeps
//! an interval for each integer local, the length of each local vector it
//! can follow, and, for each such length, the locals it equals up to a
//! constant (`len(v) = i + c`, which is how a push loop's length tracks its
//! counter). An index call on `&v` at `h as usize` is upper-proven when
//! `h`'s interval ends below the least length `v` can have there.
//!
//! What is followed:
//!
//! - a local vector (not a parameter) every mention of which is a
//!   `Vec.new()`, `Vec.with_capacity(_)` or `Vec.filled(n, _)` that makes
//!   it, a `&v` / `&mut v` into a temporary that is only the receiver of
//!   the block's call, its `Drop`, or its storage markers. Through `&v` no
//!   call changes its length; through `&mut v`, `push` adds one, a short
//!   list of methods (`index_mut`, `presize`, `reserve`, ...) keep it, and
//!   any other call forgets it;
//! - an integer local of at most 64 bits whose address is never taken
//!   mutably, and the result of a checked operation (`t.0` read after the
//!   `t.1 == 0` edge, where it is the exact sum, difference or product);
//! - a compare feeding the block's `switchInt` narrows its operands on
//!   each edge, and a narrowed counter narrows the lengths that equal it.
//!
//! Loops widen to the type's range after a few visits, then a few
//! narrowing passes recover the bounds the guards give.

use super::rc_pairs::{statement_mentions, terminator_mentions};
use super::syntax::*;
use super::ty::{IntrinsicTy, Ty, TyInterner, TyKind};
use crate::mir::interp::split_method;

/// The most vectors followed in one body.
const MAX_VECS: usize = 32;
/// The largest `blocks * slots` a body is analysed at.
const MAX_CELLS: usize = 1 << 19;
/// A block's entry state widens after this many changes.
const WIDEN_AFTER: u32 = 3;
/// The most passes without widening once the widened states are stable.
const NARROW_PASSES: usize = 16;
/// Rounds over the body before the analysis gives up.
const MAX_ROUNDS: usize = 64;

/// The `&mut v` methods that leave `v`'s length as it is.
const KEEPS_LENGTH: &[&str] = &[
    "index_mut",
    "get_mut",
    "presize",
    "reserve",
    "reserve_exact",
    "shrink_to_fit",
    "swap",
    "sort",
    "reverse",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Iv {
    lo: i128,
    hi: i128,
}

impl Iv {
    fn k(c: i128) -> Iv {
        Iv { lo: c, hi: c }
    }

    fn konst(self) -> Option<i128> {
        (self.lo == self.hi).then_some(self.lo)
    }

    fn hull(self, o: Iv) -> Iv {
        Iv {
            lo: self.lo.min(o.lo),
            hi: self.hi.max(o.hi),
        }
    }

    fn meet(self, o: Iv) -> Option<Iv> {
        let r = Iv {
            lo: self.lo.max(o.lo),
            hi: self.hi.min(o.hi),
        };
        (r.lo <= r.hi).then_some(r)
    }

    fn within(self, r: Iv) -> bool {
        r.lo <= self.lo && self.hi <= r.hi
    }

    fn shift(self, c: i128) -> Iv {
        Iv {
            lo: self.lo.saturating_add(c),
            hi: self.hi.saturating_add(c),
        }
    }
}

/// The values an integer type holds, for types of at most 64 bits.
fn int_range(tys: &TyInterner, ty: Ty) -> Option<Iv> {
    let TyKind::Int(i) = tys.kind(ty) else {
        return None;
    };
    let bits = i.bits();
    if bits > 64 {
        return None;
    }
    Some(if i.signed() {
        Iv {
            lo: -(1i128 << (bits - 1)),
            hi: (1i128 << (bits - 1)) - 1,
        }
    } else {
        Iv {
            lo: 0,
            hi: (1i128 << bits) - 1,
        }
    })
}

/// A constant's value, sign-extended for a signed type.
fn const_value(tys: &TyInterner, c: &Const) -> Option<i128> {
    let ConstKind::Scalar(u) = c.kind else {
        return None;
    };
    let TyKind::Int(i) = tys.kind(c.ty) else {
        return None;
    };
    let bits = i.bits();
    if bits > 64 {
        return None;
    }
    let u = u & ((1u128 << bits) - 1);
    Some(if i.signed() && u >> (bits - 1) == 1 {
        u as i128 - (1i128 << bits)
    } else {
        u as i128
    })
}

#[derive(Clone, Debug, PartialEq)]
struct State {
    iv: Vec<Iv>,
    /// For each followed vector, `(s, c)` with `len = slot s + c`, by `s`.
    rels: Vec<Vec<(u32, i128)>>,
    /// Checked results not yet past their overflow test: the interval is
    /// the exact one, but `t.0` reads as anything until the test.
    pending: Vec<u32>,
}

/// How a block's terminator uses a followed vector.
#[derive(Clone, Copy)]
struct Recv {
    vec: usize,
    kind: BorrowKind,
}

struct Analysis<'a> {
    body: &'a Body,
    tys: &'a TyInterner,
    /// The slot of each followed local.
    slot: Vec<Option<u32>>,
    /// The range of each slot's type.
    range: Vec<Iv>,
    /// The followed vectors' locals, and the slot of each one's length.
    vecs: Vec<Local>,
    len_slot: Vec<u32>,
    /// Per block, the followed vector its call takes by reference.
    recv: Vec<Option<Recv>>,
    preds: Vec<Vec<usize>>,
    /// Per block, the loop it heads.
    heads: Vec<Option<Head>>,
}

/// A loop: its blocks, and the slots something in it writes.
struct Head {
    blocks: Vec<bool>,
    written: Vec<bool>,
}

/// The blocks of `body` whose terminator is an index call the ranges show
/// in bounds; codegen checks only the sign there.
pub fn upper_proven_indexes(body: &Body, tys: &TyInterner) -> Vec<usize> {
    let Some(a) = Analysis::new(body, tys) else {
        return Vec::new();
    };
    let Some(outs) = a.solve() else {
        return Vec::new();
    };
    let mut proven = Vec::new();
    for (bi, b) in body.blocks.iter().enumerate() {
        let Some(st) = &outs[bi] else { continue };
        if a.index_in_bounds(b, bi, st) {
            proven.push(bi);
        }
    }
    proven
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

/// The `Vec[...]` method a call names.
fn vec_method(func: &Operand) -> Option<&str> {
    let (base, method) = split_method(fn_name(func)?);
    base.starts_with("Vec[").then_some(method)
}

fn whole(o: &Operand) -> Option<Local> {
    match o {
        Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => Some(p.local),
        _ => None,
    }
}

impl<'a> Analysis<'a> {
    fn new(body: &'a Body, tys: &'a TyInterner) -> Option<Self> {
        let n = body.locals.len();
        let mut recv = vec![None; body.blocks.len()];
        let mut vecs = Vec::new();
        for v in (body.arg_count + 1)..n {
            if vecs.len() == MAX_VECS {
                break;
            }
            let ty = body.locals[v].ty;
            if !matches!(tys.kind(ty), TyKind::Intrinsic(IntrinsicTy::Vec(_))) {
                continue;
            }
            let v = Local(v as u32);
            if let Some(uses) = followed_vec(body, v) {
                for (b, kind) in uses {
                    recv[b] = Some(Recv {
                        vec: vecs.len(),
                        kind,
                    });
                }
                vecs.push(v);
            }
        }
        // Only worth it when some index call is on a followed vector.
        let indexed = body.blocks.iter().zip(&recv).any(|(b, r)| {
            r.is_some()
                && matches!(&b.terminator.kind, TerminatorKind::Call { func, .. }
                    if matches!(vec_method(func), Some("index" | "index_mut")))
        });
        if !indexed {
            return None;
        }
        // Integer locals whose address is never taken mutably, and the
        // results of checked operations.
        let mut escaped = vec![false; n];
        let mut checked = vec![None; n];
        for b in &body.blocks {
            for s in &b.statements {
                let StatementKind::Assign(p, rv) = &s.kind else {
                    continue;
                };
                match rv {
                    Rvalue::Ref(BorrowKind::Mut, q) => escaped[q.local.index()] = true,
                    Rvalue::CheckedBinaryOp(_, x, _) if p.projection.is_empty() => {
                        if let Some(r) = operand_ty(body, x).and_then(|t| int_range(tys, t)) {
                            checked[p.local.index()] = Some(r);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut slot = vec![None; n];
        let mut range = Vec::new();
        for l in 0..n {
            if escaped[l] {
                continue;
            }
            let r = int_range(tys, body.locals[l].ty).or(checked[l]);
            if let Some(r) = r {
                slot[l] = Some(range.len() as u32);
                range.push(r);
            }
        }
        let mut len_slot = Vec::new();
        for _ in &vecs {
            len_slot.push(range.len() as u32);
            range.push(Iv {
                lo: 0,
                hi: i64::MAX as i128,
            });
        }
        if body.blocks.len().saturating_mul(range.len()) > MAX_CELLS {
            return None;
        }
        let preds = super::counters::predecessors(body);
        let idom = super::counters::dominators(body, &preds);
        let mut heads: Vec<Option<Head>> = (0..body.blocks.len()).map(|_| None).collect();
        for lp in super::counters::natural_loops(body, &preds, &idom) {
            let mut written = vec![false; range.len()];
            for (bi, b) in body.blocks.iter().enumerate() {
                if !lp.blocks[bi] {
                    continue;
                }
                for s in &b.statements {
                    if let StatementKind::Assign(p, _) = &s.kind {
                        if let Some(s) = slot[p.local.index()] {
                            written[s as usize] = true;
                        }
                    }
                }
                if let TerminatorKind::Call { destination, .. } = &b.terminator.kind {
                    if let Some(s) = slot[destination.local.index()] {
                        written[s as usize] = true;
                    }
                    if let Some(k) = vecs.iter().position(|v| *v == destination.local) {
                        written[len_slot[k] as usize] = true;
                    }
                }
                if let Some(Recv {
                    vec: k,
                    kind: BorrowKind::Mut,
                }) = recv[bi]
                {
                    written[len_slot[k] as usize] = true;
                }
            }
            heads[lp.header] = Some(Head {
                blocks: lp.blocks,
                written,
            });
        }
        Some(Analysis {
            body,
            tys,
            slot,
            range,
            vecs,
            len_slot,
            recv,
            preds,
            heads,
        })
    }

    fn entry(&self) -> State {
        State {
            iv: self.range.clone(),
            rels: vec![Vec::new(); self.vecs.len()],
            pending: Vec::new(),
        }
    }

    fn slot_of(&self, l: Local) -> Option<u32> {
        self.slot[l.index()]
    }

    /// The fixpoint: each reachable block's state after its statements.
    fn solve(&self) -> Option<Vec<Option<State>>> {
        let n = self.body.blocks.len();
        let order = super::counters::reverse_postorder(self.body);
        let mut ins: Vec<Option<State>> = vec![None; n];
        let mut outs: Vec<Option<State>> = vec![None; n];
        let mut visits = vec![0u32; n];
        let mut stable = false;
        for _ in 0..MAX_ROUNDS {
            let mut changed = false;
            for &b in &order {
                let old = ins[b].as_ref().filter(|_| visits[b] >= WIDEN_AFTER);
                let new = self.incoming(b, &outs, old);
                if new != ins[b] {
                    outs[b] = new.as_ref().map(|s| self.transfer(b, s.clone()));
                    ins[b] = new;
                    visits[b] += 1;
                    changed = true;
                }
            }
            if !changed {
                stable = true;
                break;
            }
        }
        if !stable {
            return None;
        }
        for _ in 0..NARROW_PASSES {
            let mut changed = false;
            for &b in &order {
                let new = self.incoming(b, &outs, None);
                if new != ins[b] {
                    outs[b] = new.as_ref().map(|s| self.transfer(b, s.clone()));
                    ins[b] = new;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Some(outs)
    }

    /// The join of what reaches block `b` along each edge into it. At a
    /// loop header, a slot the loop never writes holds what enters the
    /// loop, and past `old` (once widening starts) a bound the loop's own
    /// edges push goes to its type's end.
    fn incoming(&self, b: usize, outs: &[Option<State>], old: Option<&State>) -> Option<State> {
        let head = self.heads[b].as_ref();
        let mut outer = (b == 0).then(|| self.entry());
        let mut back: Option<State> = None;
        for &p in &self.preds[b] {
            let Some(out) = &outs[p] else { continue };
            for (s, st) in self.edges(p, out) {
                if s != b {
                    continue;
                }
                let Some(st) = st else { continue };
                let acc = if head.is_some_and(|h| h.blocks[p]) {
                    &mut back
                } else {
                    &mut outer
                };
                *acc = Some(match acc.take() {
                    None => st,
                    Some(a) => self.join(&a, &st),
                });
            }
        }
        let Some(h) = head else { return outer };
        // A header reached only around its loop is not reached.
        let outer = outer?;
        let mut st = match &back {
            Some(bk) => self.join(&outer, bk),
            None => outer.clone(),
        };
        for (s, w) in h.written.iter().enumerate() {
            if !w {
                st.iv[s] = outer.iv[s];
            }
        }
        if let (Some(old), Some(bk)) = (old, &back) {
            st = self.widen(old, st, bk, &h.written);
        }
        Some(st)
    }

    /// Whether `len_k = y + d` holds in `st`, said or implied by constants.
    fn holds(&self, st: &State, k: usize, y: u32, d: i128) -> bool {
        if st.rels[k].contains(&(y, d)) {
            return true;
        }
        let l = st.iv[self.len_slot[k] as usize].konst();
        let v = st.iv[y as usize].konst();
        matches!((l, v), (Some(l), Some(v)) if !st.pending.contains(&y) && l - v == d)
    }

    fn join(&self, a: &State, b: &State) -> State {
        let iv = a.iv.iter().zip(&b.iv).map(|(x, y)| x.hull(*y)).collect();
        let mut rels = Vec::with_capacity(self.vecs.len());
        for k in 0..self.vecs.len() {
            let mut r: Vec<(u32, i128)> = Vec::new();
            for &(y, d) in a.rels[k].iter().chain(&b.rels[k]) {
                if !r.contains(&(y, d)) && self.holds(a, k, y, d) && self.holds(b, k, y, d) {
                    r.push((y, d));
                }
            }
            // A length and a counter both constant on each side, the same
            // distance apart: the relation a push loop keeps.
            let ls = self.len_slot[k] as usize;
            if let (Some(la), Some(lb)) = (a.iv[ls].konst(), b.iv[ls].konst()) {
                for y in 0..self.len_slot[0] as usize {
                    let (Some(va), Some(vb)) = (a.iv[y].konst(), b.iv[y].konst()) else {
                        continue;
                    };
                    let y32 = y as u32;
                    if la - va == lb - vb
                        && !a.pending.contains(&y32)
                        && !b.pending.contains(&y32)
                        && !r.iter().any(|&(s, _)| s == y32)
                    {
                        r.push((y32, la - va));
                    }
                }
            }
            r.sort_unstable();
            rels.push(r);
        }
        let mut pending: Vec<u32> = a.pending.iter().chain(&b.pending).copied().collect();
        pending.sort_unstable();
        pending.dedup();
        State { iv, rels, pending }
    }

    /// `new` widened from `old`: a bound of a slot the loop writes that
    /// its back edges (`back`) push past `old` goes to the type's end (past
    /// it, for a checked result's exact value), and only the relations
    /// `old` had and `new` keeps stay.
    fn widen(&self, old: &State, mut new: State, back: &State, written: &[bool]) -> State {
        for (s, _) in written.iter().enumerate().filter(|(_, w)| **w) {
            let (o, n, b, r) = (old.iv[s], new.iv[s], back.iv[s], self.range[s]);
            let lo = match () {
                _ if n.lo >= o.lo => o.lo,
                _ if b.lo >= o.lo => n.lo,
                _ if n.lo >= r.lo => r.lo,
                _ => i128::MIN,
            };
            let hi = match () {
                _ if n.hi <= o.hi => o.hi,
                _ if b.hi <= o.hi => n.hi,
                _ if n.hi <= r.hi => r.hi,
                _ => i128::MAX,
            };
            new.iv[s] = Iv { lo, hi };
        }
        for k in 0..self.vecs.len() {
            new.rels[k] = old.rels[k]
                .iter()
                .copied()
                .filter(|&(y, d)| self.holds(&new, k, y, d))
                .collect();
        }
        new.pending.extend(&old.pending);
        new.pending.sort_unstable();
        new.pending.dedup();
        new
    }

    /// Block `b`'s statements, and its call's effect, from `st`.
    fn transfer(&self, b: usize, mut st: State) -> State {
        let data = &self.body.blocks[b];
        for s in &data.statements {
            if let StatementKind::Assign(p, rv) = &s.kind {
                self.assign(&mut st, p, rv);
            }
        }
        if let TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } = &data.terminator.kind
        {
            self.call(&mut st, b, func, args, destination);
        }
        st
    }

    /// Forgets slot `s`: its value and every length that equals it.
    fn forget(&self, st: &mut State, s: u32) {
        st.iv[s as usize] = self.range[s as usize];
        for r in &mut st.rels {
            r.retain(|&(y, _)| y != s);
        }
        st.pending.retain(|&y| y != s);
    }

    /// Sets slot `s` to `iv`, equal to `from + c` when that is known.
    fn set(&self, st: &mut State, s: u32, iv: Iv, from: Option<(u32, i128)>, pending: bool) {
        let mut add = Vec::new();
        if let Some((y, c)) = from {
            for (k, r) in st.rels.iter().enumerate() {
                for &(z, d) in r {
                    if z == y {
                        add.push((k, d - c));
                    }
                }
            }
        }
        self.forget(st, s);
        st.iv[s as usize] = iv;
        for (k, d) in add {
            st.rels[k].push((s, d));
            st.rels[k].sort_unstable();
        }
        if pending {
            st.pending.push(s);
            st.pending.sort_unstable();
        }
    }

    /// An operand's interval, and its slot when it reads one exactly.
    fn operand(&self, st: &State, o: &Operand) -> Option<(Iv, Option<u32>)> {
        match o {
            Operand::Const(c) => Some((Iv::k(const_value(self.tys, c)?), None)),
            Operand::Copy(p) | Operand::Move(p) => {
                let s = self.slot_of(p.local)?;
                match p.projection.as_slice() {
                    [] if int_range(self.tys, self.body.locals[p.local.index()].ty).is_some() => {
                        Some((st.iv[s as usize], Some(s)))
                    }
                    [ProjElem::Field(f, _)] if f.index() == 0 => {
                        if st.pending.contains(&s) {
                            Some((self.range[s as usize], None))
                        } else {
                            Some((st.iv[s as usize], Some(s)))
                        }
                    }
                    _ => None,
                }
            }
        }
    }

    fn assign(&self, st: &mut State, p: &Place, rv: &Rvalue) {
        let Some(s) = self.slot_of(p.local) else {
            return;
        };
        if !p.projection.is_empty() {
            self.forget(st, s);
            return;
        }
        let r = self.range[s as usize];
        let (iv, from, pending) = match rv {
            Rvalue::Use(o) => match self.operand(st, o) {
                Some((iv, y)) => (iv, y.map(|y| (y, 0)), false),
                None => (r, None, false),
            },
            Rvalue::Cast(CastKind::IntToInt, o, _) => match self.operand(st, o) {
                Some((iv, y)) if iv.within(r) => (iv, y.map(|y| (y, 0)), false),
                _ => (r, None, false),
            },
            Rvalue::CheckedBinaryOp(op, x, y) => match self.arith(st, *op, x, y) {
                // The exact result, which `t.0` holds past the test.
                Some((iv, from)) => (iv, from, true),
                None => (r, None, false),
            },
            Rvalue::BinaryOp(op, x, y) => match self.arith(st, *op, x, y) {
                // Wrapping: exact only when it cannot wrap.
                Some((iv, from)) if iv.within(r) => (iv, from, false),
                _ => (r, None, false),
            },
            Rvalue::UnaryOp(UnOp::Neg, x) => match self.operand(st, x) {
                Some((iv, _)) => {
                    let n = Iv {
                        lo: iv.hi.saturating_neg(),
                        hi: iv.lo.saturating_neg(),
                    };
                    if n.within(r) {
                        (n, None, false)
                    } else {
                        (r, None, false)
                    }
                }
                None => (r, None, false),
            },
            _ => (r, None, false),
        };
        // A checked result keeps its exact interval for the test to clamp.
        let iv = if pending || iv.within(r) { iv } else { r };
        self.set(st, s, iv, from, pending);
    }

    /// `x op y`'s exact interval, with `x op y = z + c` when one side is
    /// the slot `z` and the other the constant `c`.
    fn arith(
        &self,
        st: &State,
        op: BinOp,
        x: &Operand,
        y: &Operand,
    ) -> Option<(Iv, Option<(u32, i128)>)> {
        let (a, sa) = self.operand(st, x)?;
        let (b, sb) = self.operand(st, y)?;
        let iv = match op {
            BinOp::Add => Iv {
                lo: a.lo.saturating_add(b.lo),
                hi: a.hi.saturating_add(b.hi),
            },
            BinOp::Sub => Iv {
                lo: a.lo.saturating_sub(b.hi),
                hi: a.hi.saturating_sub(b.lo),
            },
            BinOp::Mul => {
                let p = [
                    a.lo.saturating_mul(b.lo),
                    a.lo.saturating_mul(b.hi),
                    a.hi.saturating_mul(b.lo),
                    a.hi.saturating_mul(b.hi),
                ];
                Iv {
                    lo: *p.iter().min()?,
                    hi: *p.iter().max()?,
                }
            }
            BinOp::BitAnd => match (a.lo >= 0, b.lo >= 0) {
                (true, true) => Iv {
                    lo: 0,
                    hi: a.hi.min(b.hi),
                },
                (true, false) => Iv { lo: 0, hi: a.hi },
                (false, true) => Iv { lo: 0, hi: b.hi },
                (false, false) => return None,
            },
            BinOp::Shr if a.lo >= 0 => {
                let k = b.konst().filter(|k| (0..64).contains(k))?;
                Iv {
                    lo: a.lo >> k,
                    hi: a.hi >> k,
                }
            }
            BinOp::Rem if a.lo >= 0 && b.lo >= 1 => Iv {
                lo: 0,
                hi: a.hi.min(b.hi - 1),
            },
            BinOp::Div if a.lo >= 0 && b.lo >= 1 => Iv {
                lo: a.lo / b.hi,
                hi: a.hi / b.lo,
            },
            _ => return None,
        };
        let from = match (op, sa, sb, a.konst(), b.konst()) {
            (BinOp::Add, Some(z), None, _, Some(c)) => Some((z, c)),
            (BinOp::Add, None, Some(z), Some(c), _) => Some((z, c)),
            (BinOp::Sub, Some(z), None, _, Some(c)) => Some((z, -c)),
            _ => None,
        };
        Some((iv, from))
    }

    fn call(&self, st: &mut State, b: usize, func: &Operand, args: &[Operand], dest: &Place) {
        let method = vec_method(func);
        // The length a call into a followed vector leaves.
        if let Some(Recv { vec: k, kind }) = self.recv[b] {
            let ls = self.len_slot[k];
            match (kind, method) {
                (BorrowKind::Mut, Some("push")) => {
                    let l = &mut st.iv[ls as usize];
                    *l = l
                        .shift(1)
                        .meet(self.range[ls as usize])
                        .unwrap_or(self.range[ls as usize]);
                    for r in &mut st.rels[k] {
                        r.1 += 1;
                    }
                }
                (BorrowKind::Mut, Some(m)) if KEEPS_LENGTH.contains(&m) => {}
                (BorrowKind::Mut, _) => self.forget_len(st, k),
                (BorrowKind::Shared, _) => {}
            }
            if method == Some("len") && dest.projection.is_empty() {
                if let Some(s) = self.slot_of(dest.local) {
                    let iv = st.iv[ls as usize];
                    if iv.within(self.range[s as usize]) {
                        self.forget(st, s);
                        st.iv[s as usize] = iv;
                        st.rels[k].push((s, 0));
                        st.rels[k].sort_unstable();
                    } else {
                        self.forget(st, s);
                    }
                    return;
                }
            }
        }
        if !dest.projection.is_empty() {
            if let Some(s) = self.slot_of(dest.local) {
                self.forget(st, s);
            }
            return;
        }
        if let Some(k) = self.vecs.iter().position(|v| *v == dest.local) {
            // The calls that make a followed vector.
            let ls = self.len_slot[k] as usize;
            self.forget_len(st, k);
            match (method, args) {
                (Some("new" | "with_capacity"), _) => st.iv[ls] = Iv::k(0),
                (Some("filled"), [n, _]) => {
                    if let Some((iv, y)) = self.operand(st, n) {
                        // `n < 0` makes it empty.
                        let made = Iv {
                            lo: iv.lo.max(0),
                            hi: iv.hi.max(0),
                        };
                        st.iv[ls] = made.meet(self.range[ls]).unwrap_or(self.range[ls]);
                        if let (Some(y), true) = (y, iv.lo >= 0) {
                            st.rels[k].push((y, 0));
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if let Some(s) = self.slot_of(dest.local) {
            self.forget(st, s);
        }
    }

    /// Forgets vector `k`'s length and what it equals.
    fn forget_len(&self, st: &mut State, k: usize) {
        let ls = self.len_slot[k];
        self.forget(st, ls);
        st.rels[k].clear();
    }

    /// The states block `p`'s terminator sends to each successor, narrowed
    /// by what its branch tests; `None` on an edge that cannot be taken.
    fn edges(&self, p: usize, out: &State) -> Vec<(usize, Option<State>)> {
        let data = &self.body.blocks[p];
        let TerminatorKind::SwitchInt { discr, targets } = &data.terminator.kind else {
            return data
                .terminator
                .kind
                .successors()
                .into_iter()
                .map(|s| (s.index(), Some(out.clone())))
                .collect();
        };
        let mut v = Vec::new();
        for &(val, t) in &targets.values {
            v.push((t.index(), self.branch(data, discr, out, Some(val))));
        }
        v.push((
            targets.otherwise.index(),
            self.branch(data, discr, out, None),
        ));
        v
    }

    /// `out` on the edge taken when `discr` is `val` (`None`: otherwise).
    fn branch(
        &self,
        data: &BasicBlockData,
        discr: &Operand,
        out: &State,
        val: Option<u128>,
    ) -> Option<State> {
        let mut st = out.clone();
        let (Operand::Copy(d) | Operand::Move(d)) = discr else {
            return Some(st);
        };
        // A checked operation's overflow flag.
        if let [ProjElem::Field(f, _)] = d.projection.as_slice() {
            if f.index() == 1 {
                if let Some(s) = self.slot_of(d.local) {
                    if val == Some(0) {
                        st.pending.retain(|&y| y != s);
                        let iv = st.iv[s as usize].meet(self.range[s as usize])?;
                        st.iv[s as usize] = iv;
                        return self.reduce(st);
                    }
                    self.forget(&mut st, s);
                }
            }
            return Some(st);
        }
        if !d.projection.is_empty() {
            return Some(st);
        }
        // A switch on an integer: its value on a value edge.
        if let Some(s) = self.slot_of(d.local) {
            if let Some(val) = val {
                if st.pending.contains(&s) {
                    return Some(st);
                }
                let ty = self.body.locals[d.local.index()].ty;
                let c = const_value(
                    self.tys,
                    &Const {
                        ty,
                        kind: ConstKind::Scalar(val),
                    },
                )?;
                st.iv[s as usize] = st.iv[s as usize].meet(Iv::k(c))?;
                return self.reduce(st);
            }
            return Some(st);
        }
        if !matches!(
            self.tys.kind(self.body.locals[d.local.index()].ty),
            TyKind::Bool
        ) {
            return Some(st);
        }
        // A `bool` set by a compare in this block, its operands unchanged
        // since.
        let truth = match val {
            Some(0) => false,
            None => true,
            Some(_) => return Some(st),
        };
        let found = data
            .statements
            .iter()
            .enumerate()
            .rev()
            .find_map(|(j, s)| match &s.kind {
                StatementKind::Assign(p, rv) if p.local == d.local => Some((j, p, rv)),
                _ => None,
            });
        let Some((j, p, Rvalue::BinaryOp(op, x, y))) = found else {
            return Some(st);
        };
        if !p.projection.is_empty() {
            return Some(st);
        }
        let later = &data.statements[j + 1..];
        let unchanged = |o: &Operand| match o {
            Operand::Const(_) => true,
            Operand::Copy(q) | Operand::Move(q) => !later
                .iter()
                .any(|s| matches!(&s.kind, StatementKind::Assign(w, _) if w.local == q.local)),
        };
        if !unchanged(x) || !unchanged(y) {
            return Some(st);
        }
        let op = if truth {
            *op
        } else {
            match op {
                BinOp::Lt => BinOp::Ge,
                BinOp::Le => BinOp::Gt,
                BinOp::Gt => BinOp::Le,
                BinOp::Ge => BinOp::Lt,
                BinOp::Eq => BinOp::Ne,
                BinOp::Ne => BinOp::Eq,
                _ => return Some(st),
            }
        };
        let (Some((a, sa)), Some((b, sb))) = (self.operand(&st, x), self.operand(&st, y)) else {
            return Some(st);
        };
        let one = |iv: Iv, c: i128| iv.shift(c);
        let (na, nb) = match op {
            // a < b: a <= b.hi - 1, b >= a.lo + 1.
            BinOp::Lt => (
                Iv {
                    lo: a.lo,
                    hi: one(b, -1).hi,
                },
                Iv {
                    lo: one(a, 1).lo,
                    hi: b.hi,
                },
            ),
            BinOp::Le => (Iv { lo: a.lo, hi: b.hi }, Iv { lo: a.lo, hi: b.hi }),
            BinOp::Gt => (
                Iv {
                    lo: one(b, 1).lo,
                    hi: a.hi,
                },
                Iv {
                    lo: b.lo,
                    hi: one(a, -1).hi,
                },
            ),
            BinOp::Ge => (Iv { lo: b.lo, hi: a.hi }, Iv { lo: b.lo, hi: a.hi }),
            BinOp::Eq => (b, a),
            BinOp::Ne => {
                let shave = |v: Iv, c: Option<i128>| match c {
                    Some(c) if v.lo == c => Iv {
                        lo: c + 1,
                        hi: v.hi,
                    },
                    Some(c) if v.hi == c => Iv {
                        lo: v.lo,
                        hi: c - 1,
                    },
                    _ => v,
                };
                (shave(a, b.konst()), shave(b, a.konst()))
            }
            _ => return Some(st),
        };
        let na = a.meet(na)?;
        let nb = b.meet(nb)?;
        if let Some(sa) = sa {
            st.iv[sa as usize] = na;
        }
        if let Some(sb) = sb {
            st.iv[sb as usize] = nb;
        }
        self.reduce(st)
    }

    /// Narrows each length and the counters it equals by each other.
    fn reduce(&self, mut st: State) -> Option<State> {
        for k in 0..self.vecs.len() {
            let ls = self.len_slot[k] as usize;
            for &(y, d) in &st.rels[k] {
                let y = y as usize;
                let l = st.iv[ls].meet(st.iv[y].shift(d))?;
                let v = st.iv[y].meet(l.shift(-d))?;
                st.iv[ls] = l;
                st.iv[y] = v;
            }
        }
        Some(st)
    }

    /// Whether block `bi` ends in `v[h]` on a followed `v` with `h`'s
    /// interval below `v`'s least length.
    fn index_in_bounds(&self, b: &BasicBlockData, bi: usize, st: &State) -> bool {
        let Some(Recv { vec: k, .. }) = self.recv[bi] else {
            return false;
        };
        let TerminatorKind::Call { func, args, .. } = &b.terminator.kind else {
            return false;
        };
        if !matches!(vec_method(func), Some("index" | "index_mut")) || args.len() != 2 {
            return false;
        }
        let least = st.iv[self.len_slot[k] as usize].lo;
        let u = match &args[1] {
            Operand::Const(c) => return const_value(self.tys, c).is_some_and(|i| i < least),
            o => match whole(o) {
                Some(u) => u,
                None => return false,
            },
        };
        // `u = h as usize` in this block, `h` signed and unchanged since:
        // a negative `h` is a huge `u`, which the sign check still stops.
        let def = b
            .statements
            .iter()
            .enumerate()
            .rev()
            .find_map(|(j, s)| match &s.kind {
                StatementKind::Assign(p, rv) if p.local == u => Some((j, p, rv)),
                _ => None,
            });
        let h = match def {
            Some((
                j,
                p,
                Rvalue::Cast(CastKind::IntToInt, Operand::Copy(h) | Operand::Move(h), _),
            )) if p.projection.is_empty() && h.projection.is_empty() => {
                let signed = matches!(self.tys.kind(self.body.locals[h.local.index()].ty),
                    TyKind::Int(i) if i.signed());
                let later = b.statements[j + 1..]
                    .iter()
                    .any(|s| matches!(&s.kind, StatementKind::Assign(w, _) if w.local == h.local));
                if !signed || later {
                    return false;
                }
                h.local
            }
            _ => u,
        };
        let Some(s) = self.slot_of(h) else {
            return false;
        };
        if st.pending.contains(&s) {
            return false;
        }
        st.iv[s as usize].hi < least
    }
}

/// The blocks that make each followed use of `v`, with the borrow kind,
/// when every mention of `v` is one [`range_bounds`](self) can follow.
fn followed_vec(body: &Body, v: Local) -> Option<Vec<(usize, BorrowKind)>> {
    let mut uses = Vec::new();
    for (bi, b) in body.blocks.iter().enumerate() {
        for (si, s) in b.statements.iter().enumerate() {
            if !statement_mentions(&s.kind, v) {
                continue;
            }
            match &s.kind {
                StatementKind::StorageLive(_) | StatementKind::StorageDead(_) => {}
                StatementKind::Assign(t, Rvalue::Ref(kind, q))
                    if t.projection.is_empty() && q.local == v && q.projection.is_empty() =>
                {
                    if !receiver_only(body, bi, si, t.local) {
                        return None;
                    }
                    uses.push((bi, *kind));
                }
                _ => return None,
            }
        }
        if !terminator_mentions(&b.terminator.kind, v) {
            continue;
        }
        match &b.terminator.kind {
            TerminatorKind::Drop { place, .. } if place.projection.is_empty() => {}
            TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } if destination.local == v
                && destination.projection.is_empty()
                && !args
                    .iter()
                    .any(|a| matches!(a, Operand::Copy(p) | Operand::Move(p) if p.local == v))
                && matches!(vec_method(func), Some("new" | "with_capacity" | "filled")) => {}
            _ => return None,
        }
    }
    // Two borrows ending at one call would be two receivers.
    let mut blocks: Vec<usize> = uses.iter().map(|u| u.0).collect();
    blocks.sort_unstable();
    blocks.dedup();
    (blocks.len() == uses.len()).then_some(uses)
}

/// Whether `t`, assigned at statement `si` of block `bi`, is mentioned
/// nowhere else but as the receiver of that block's call.
fn receiver_only(body: &Body, bi: usize, si: usize, t: Local) -> bool {
    for (bj, b) in body.blocks.iter().enumerate() {
        for (sj, s) in b.statements.iter().enumerate() {
            if (bj, sj) != (bi, si)
                && statement_mentions(&s.kind, t)
                && !matches!(
                    s.kind,
                    StatementKind::StorageLive(_) | StatementKind::StorageDead(_)
                )
            {
                return false;
            }
        }
        if bj == bi {
            let TerminatorKind::Call {
                args, destination, ..
            } = &b.terminator.kind
            else {
                return false;
            };
            if args.first().and_then(whole) != Some(t)
                || args[1..]
                    .iter()
                    .any(|a| matches!(a, Operand::Copy(p) | Operand::Move(p) if p.local == t))
                || destination.local == t
            {
                return false;
            }
        } else if terminator_mentions(&b.terminator.kind, t) {
            return false;
        }
    }
    true
}

/// The type of what an operand reads.
fn operand_ty(body: &Body, o: &Operand) -> Option<Ty> {
    match o {
        Operand::Const(c) => Some(c.ty),
        Operand::Copy(p) | Operand::Move(p) => match p.projection.as_slice() {
            [] => Some(body.locals[p.local.index()].ty),
            [.., ProjElem::Field(_, ty)] => Some(*ty),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::lower::tests::run_source;
    use crate::mir::TerminatorKind;

    /// Kata 139's prefix DP over windows of `s` (B-2026-10-28-40).
    const WAYS: &str = "fn ways(seed: i64) -> i64 {\n    let n = 40;\n    let win = 6;\n    let mut s: Vec[i64] = Vec.new();\n    let mut i = 0;\n    while i < n {\n        s.push((seed + i * 7) % 3);\n        i = i + 1;\n    }\n    let mut dp: Vec[bool] = Vec.new();\n    let mut d = 0;\n    while d <= win {\n        dp.push(false);\n        d = d + 1;\n    }\n    let mut total = 0;\n    let mut off = 0;\n    while off < n - win {\n        let mut z = 0;\n        while z <= win {\n            dp[z] = false;\n            z = z + 1;\n        }\n        dp[0] = true;\n        let mut ii = 1;\n        while ii <= win {\n            let mut j = ii - 1;\n            while j >= 0 {\n                if dp[j] and s[off + j] != 1 {\n                    dp[ii] = true;\n                    break;\n                }\n                j = j - 1;\n            }\n            ii = ii + 1;\n        }\n        if dp[win] {\n            total = total + off + 1;\n        }\n        off = off + 1;\n    }\n    total\n}\nfn main() {\n    println(f\"{ways(0)} {ways(1)} {ways(2)}\");\n}\n";

    /// The index calls of `f`, and how many of them are proved.
    fn proved(src: &str, f: &str) -> (usize, usize) {
        let l = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == f)
            .unwrap_or_else(|| panic!("no body {f}"));
        let indexes = body
            .blocks
            .iter()
            .filter(|b| {
                matches!(&b.terminator.kind, TerminatorKind::Call { func, .. }
                    if matches!(super::vec_method(func), Some("index" | "index_mut")))
            })
            .count();
        (indexes, super::upper_proven_indexes(body, &l.tys).len())
    }

    /// Every index in the DP is proved: `s` and `dp` are filled by
    /// counting loops to 40 and 7, and `off + j <= 33 + 5`, `j <= 5`,
    /// `z, ii, win <= 6` by the loops around them.
    #[test]
    fn mir_range_bounds_proves_a_dp_over_filled_vectors() {
        assert_eq!(proved(WAYS, "ways"), (6, 6));
        assert_eq!(run_source(WAYS).unwrap(), ("408 385 397\n".into(), Some(0)));
    }

    /// Off the proof: a fill one short of the index, a `pop` after the
    /// fill, and a callee that truncates through `mut ref`; each index
    /// still panics.
    #[test]
    fn mir_range_bounds_leaves_what_it_cannot_follow() {
        for (src, f, out) in [
            (
                "fn last() -> bool {\n    let win = 4;\n    let mut dp: Vec[bool] = Vec.new();\n    let mut d = 0;\n    while d < win {\n        dp.push(true);\n        d = d + 1;\n    }\n    dp[win]\n}\nfn main() {\n    println(\"start\");\n    println(f\"{last()}\");\n}\n",
                "last",
                "start\n",
            ),
            (
                "fn popped() -> i64 {\n    let mut v: Vec[i64] = Vec.new();\n    let mut i = 0;\n    while i < 5 {\n        v.push(i);\n        i = i + 1;\n    }\n    let top = v.pop();\n    println(f\"{top.unwrap_or(-1)}\");\n    v[4]\n}\nfn main() {\n    println(f\"{popped()}\");\n}\n",
                "popped",
                "4\n",
            ),
            (
                "fn shrink(v: mut ref Vec[i64]) {\n    v.truncate(1);\n}\nfn shrunk() -> i64 {\n    let mut v: Vec[i64] = Vec.new();\n    let mut i = 0;\n    while i < 5 {\n        v.push(i);\n        i = i + 1;\n    }\n    shrink(mut v);\n    v[3]\n}\nfn main() {\n    println(\"start\");\n    println(f\"{shrunk()}\");\n}\n",
                "shrunk",
                "start\n",
            ),
        ] {
            assert_eq!(proved(src, f), (1, 0), "{f}");
            let (o, code) = run_source(src).unwrap();
            assert_eq!((o.as_str(), code), (out, Some(101)), "{f}");
        }
    }
}
