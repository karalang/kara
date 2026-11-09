//! A `sort_by` comparator that is exactly an element type's own order, or
//! its reverse (`|a, b| a.cmp(b)`, `|a, b| b.cmp(a)`), over integers,
//! `char`s or `bool`s (B-2026-11-06-34).
//!
//! Such a comparator reads nothing but its two elements and calls nothing,
//! so the order in which a sort calls it cannot be observed, and two equal
//! elements of these types are the same bits, so a stable sort and an
//! unstable one leave the same sequence. The vector is then sorted by the
//! runtime's integer sort (`sort_own_order`) and, for the reverse order,
//! reversed: the sequence the merge sort and the interpreter's schedule
//! produce, without a comparator call per pair.
//!
//! The comparator is recognised in the shape MIR lowering gives
//! `x.cmp(y)` on two scalars once inlined into the closure: each parameter
//! read through its reference, `Gt` and `Lt` of the two cast to `i8` and
//! subtracted, a `switchInt` on that difference, and one block per answer
//! that writes `Ordering.Less` / `Equal` / `Greater` and returns. Anything
//! else, a call, a write elsewhere, another constant, is not recognised and
//! keeps the merge sort.

use std::collections::HashMap;

use super::Cx;
use crate::mir::syntax::{
    AggregateKind, BasicBlock, BinOp, Body, CastKind, Local, Operand, Place, ProjElem, Rvalue,
    StatementKind, TerminatorKind,
};
use crate::mir::Ty;

/// What a local holds, in terms of the comparator's two elements (`0` is
/// the first parameter's element, `1` the second's).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Val {
    Elem(u8),
    Cmp(BinOp, u8, u8),
    CmpInt(BinOp, u8, u8),
    /// `x.cmp(y)` as `-1` / `0` / `1`.
    Order(u8, u8),
}

impl<'ctx> Cx<'ctx, '_> {
    /// `Some(false)` for a comparator `fty` that is `a.cmp(b)` of its two
    /// elements, `Some(true)` for `b.cmp(a)`, `None` for anything else.
    pub(super) fn plain_order_comparator(&self, fty: Ty) -> Option<bool> {
        let body = self.body_of_def(fty).ok()?;
        let ord_ty = body.locals.first()?.ty;
        let less = self.variant_of(ord_ty, "Less").ok()?;
        let equal = self.variant_of(ord_ty, "Equal").ok()?;
        let greater = self.variant_of(ord_ty, "Greater").ok()?;
        let order = order_of(body)?;
        let (x, y, answers) = order;
        // `answers` is the variant each of `-1`, `0`, `1` writes.
        if answers != [less, equal, greater] {
            return None;
        }
        match (x, y) {
            (0, 1) => Some(false),
            (1, 0) => Some(true),
            _ => None,
        }
    }
}

/// The two elements `body` orders, `(x, y)` for `x.cmp(y)`, and the
/// variant it returns for each of `-1`, `0`, `1`.
fn order_of(body: &Body) -> Option<(u8, u8, [u32; 3])> {
    // `(env, a, b)` for a closure, `(a, b)` for a function: the elements
    // are read through the last two parameters.
    let first = match body.arg_count {
        3 => 2,
        2 => 1,
        _ => return None,
    };
    let param = |p: &Place| -> Option<u8> {
        match p.projection.as_slice() {
            [ProjElem::Deref] if p.local == Local(first) => Some(0),
            [ProjElem::Deref] if p.local == Local(first + 1) => Some(1),
            _ => None,
        }
    };
    let mut vals: HashMap<Local, Val> = HashMap::new();
    let read = |vals: &HashMap<Local, Val>, o: &Operand| -> Option<Val> {
        let p = o.place()?;
        if let Some(i) = param(p) {
            return Some(Val::Elem(i));
        }
        if !p.projection.is_empty() {
            return None;
        }
        vals.get(&p.local).copied()
    };
    let mut bb = BasicBlock(0);
    let mut seen = 0;
    loop {
        seen += 1;
        if seen > body.blocks.len() {
            return None;
        }
        let data = body.blocks.get(bb.index())?;
        for s in &data.statements {
            match &s.kind {
                StatementKind::StorageLive(_)
                | StatementKind::StorageDead(_)
                | StatementKind::Nop => {}
                StatementKind::Assign(dest, rv) => {
                    if !dest.projection.is_empty() || dest.local.index() <= body.arg_count {
                        return None;
                    }
                    let v =
                        match rv {
                            Rvalue::Use(o) => read(&vals, o)?,
                            Rvalue::BinaryOp(op @ (BinOp::Gt | BinOp::Lt), a, b) => {
                                match (read(&vals, a)?, read(&vals, b)?) {
                                    (Val::Elem(x), Val::Elem(y)) if x != y => Val::Cmp(*op, x, y),
                                    _ => return None,
                                }
                            }
                            Rvalue::Cast(CastKind::BoolToInt, o, _) => match read(&vals, o)? {
                                Val::Cmp(op, x, y) => Val::CmpInt(op, x, y),
                                _ => return None,
                            },
                            Rvalue::BinaryOp(BinOp::Sub, a, b) => {
                                match (read(&vals, a)?, read(&vals, b)?) {
                                    (
                                        Val::CmpInt(BinOp::Gt, x, y),
                                        Val::CmpInt(BinOp::Lt, x2, y2),
                                    ) if (x, y) == (x2, y2) => Val::Order(x, y),
                                    (
                                        Val::CmpInt(BinOp::Lt, x, y),
                                        Val::CmpInt(BinOp::Gt, x2, y2),
                                    ) if (x, y) == (x2, y2) => Val::Order(y, x),
                                    _ => return None,
                                }
                            }
                            _ => return None,
                        };
                    vals.insert(dest.local, v);
                }
                _ => return None,
            }
        }
        match &data.terminator.kind {
            TerminatorKind::Goto { target } => bb = *target,
            TerminatorKind::SwitchInt { discr, targets } => {
                let Val::Order(x, y) = read(&vals, discr)? else {
                    return None;
                };
                // The `i8` difference: `-1` reads as 255.
                let answer = |v: u128| -> Option<u32> {
                    let to = targets
                        .values
                        .iter()
                        .find(|(k, _)| *k == v)
                        .map_or(targets.otherwise, |(_, b)| *b);
                    answer_of(body, to)
                };
                let answers = [answer(255)?, answer(0)?, answer(1)?];
                // Every value the switch names is one of the three.
                if targets
                    .values
                    .iter()
                    .any(|(k, _)| !matches!(k, 0 | 1 | 255))
                {
                    return None;
                }
                return Some((x, y, answers));
            }
            _ => return None,
        }
    }
}

/// The variant a block that writes a fieldless `Ordering` into `_0` and
/// then returns (through blocks that only end storage) writes.
fn answer_of(body: &Body, bb: BasicBlock) -> Option<u32> {
    let data = body.blocks.get(bb.index())?;
    let mut variant = None;
    for s in &data.statements {
        match &s.kind {
            StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {}
            StatementKind::Assign(
                dest,
                Rvalue::Aggregate(AggregateKind::Adt { variant: v, .. }, ops),
            ) if dest.local == Local(0) && dest.projection.is_empty() && ops.is_empty() => {
                variant = Some(v.0);
            }
            _ => return None,
        }
    }
    let variant = variant?;
    let mut next = &data.terminator.kind;
    for _ in 0..body.blocks.len() {
        match next {
            TerminatorKind::Return => return Some(variant),
            TerminatorKind::Goto { target } => {
                let d = body.blocks.get(target.index())?;
                if !d.statements.iter().all(|s| {
                    matches!(
                        s.kind,
                        StatementKind::StorageLive(_)
                            | StatementKind::StorageDead(_)
                            | StatementKind::Nop
                    )
                }) {
                    return None;
                }
                next = &d.terminator.kind;
            }
            _ => return None,
        }
    }
    None
}
