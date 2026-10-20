//! The borrow-flag pass: core semantics §6.2's run-time check, inserted
//! after drop elaboration.
//!
//! A `mut` field of a `shared` value whose type is neither `Copy` nor a
//! handle aggregate carries a borrow flag. Through one handle place the
//! borrow checker finds every conflict; two different handles may name
//! one object, so the flag catches the rest at run time, as a panic.
//!
//! - Each loan of such a field (a `Ref` whose place projects it) takes
//!   the flag right after the `Ref`, with the loan's kind, and gives it
//!   back where the borrow checker's liveness says no local can hold the
//!   loan any more. A `shared` field reached through another flagged
//!   field takes that outer flag as a reader too.
//! - A write or drop of such a field, and a read below one, checks the
//!   flag without taking it.
//! - A frame gives back what it still holds when it returns.
//!
//! MIR does not record which fields are `mut`; the type checker rejects
//! a write or `mut` borrow of any other field, so flagging every field
//! of the right type changes nothing for those.
//!
//! A reference a function returns into a flagged field of a parameter's
//! object holds no flag in the caller: the loan was the callee's. That is
//! the gap §6.2's same-handle rule does not cover, and it stays open
//! until loans cross calls.

use super::borrowck::{
    loan_liveness, statement_accesses, terminator_accesses, Access, ResultBorrows,
};
use super::place_ty::place_ty;
use super::syntax::*;
use super::ty::{IntrinsicTy, Ty, TyInterner, TyKind};

/// Inserts the borrow-flag operations into an elaborated `body`.
/// `has_receiver` is the borrow checker's.
pub fn insert_borrow_flags(
    body: &mut Body,
    tys: &TyInterner,
    has_receiver: &dyn Fn(&InstanceId) -> ResultBorrows,
) {
    let (loans, live) = loan_liveness(body, tys, has_receiver);
    let held: Vec<Vec<(Place, BorrowKind)>> = loans
        .iter()
        .map(|(k, p, _)| flagged_prefixes(body, tys, p, *k))
        .collect();
    // Per block: (point, order, op). A point is a statement index, or the
    // statement count for the terminator; the op goes just before it. At
    // one point a loan made by the previous statement takes its flag
    // first, then dead loans give theirs back, then the point's own
    // accesses are checked.
    let mut ins: Vec<Vec<(usize, u8, FlagOp)>> = vec![Vec::new(); body.blocks.len()];
    let preds = predecessors(body);
    for (li, (_, _, (bi, si))) in loans.iter().enumerate() {
        if held[li].is_empty() {
            continue;
        }
        for (place, kind) in &held[li] {
            ins[*bi].push((
                si + 1,
                0,
                FlagOp::Acquire {
                    place: place.clone(),
                    kind: *kind,
                    loan: li as u32,
                },
            ));
        }
        for (b, points) in live.iter().enumerate() {
            for (p, now) in points.iter().enumerate() {
                let before = if p == 0 {
                    preds[b].iter().any(|&pb| {
                        let pts = &live[pb];
                        pts[pts.len() - 1][li]
                    })
                } else {
                    points[p - 1][li] || (b, p - 1) == (*bi, *si)
                };
                if before && !now[li] {
                    ins[b].push((p, 1, FlagOp::Release { loan: li as u32 }));
                }
            }
        }
    }
    for (bi, block) in body.blocks.iter().enumerate() {
        let n = block.statements.len();
        for p in 0..=n {
            let accesses = if p < n {
                statement_accesses(&block.statements[p].kind)
            } else {
                terminator_accesses(&block.terminator.kind)
            };
            for (place, access) in accesses {
                let kind = match access {
                    Access::Read => BorrowKind::Shared,
                    Access::Write | Access::Move | Access::Drop => BorrowKind::Mut,
                    // A borrow takes the flag; the end of a local's
                    // storage touches no object.
                    Access::Borrow(_) | Access::StorageDead => continue,
                };
                for (fp, k) in flagged_prefixes(body, tys, &place, kind) {
                    ins[bi].push((p, 2, FlagOp::Check { place: fp, kind: k }));
                }
            }
        }
    }
    for (bi, mut ops) in ins.into_iter().enumerate() {
        if ops.is_empty() {
            continue;
        }
        ops.sort_by_key(|(p, order, _)| (*p, *order));
        let block = &mut body.blocks[bi];
        let info = block.terminator.source_info;
        let old = std::mem::take(&mut block.statements);
        let mut ops = ops.into_iter().peekable();
        for (si, s) in old.into_iter().enumerate() {
            while let Some((_, _, op)) = ops.next_if(|(p, _, _)| *p == si) {
                block.statements.push(Statement {
                    kind: StatementKind::BorrowFlag(op),
                    source_info: s.source_info,
                });
            }
            block.statements.push(s);
        }
        for (_, _, op) in ops {
            block.statements.push(Statement {
                kind: StatementKind::BorrowFlag(op),
                source_info: info,
            });
        }
    }
}

/// A flag held only across one library call is narrowed to a check: a loan
/// whose every `Acquire` sits just before a call to a native, with no
/// function value or user `Drop` in reach of its arguments, and whose
/// every `Release` opens that call's target block. Nothing can touch the
/// flag while it is held, so taking it is the check it would make (§6.2):
/// `b.items.push(x)` checks `b.items` once instead of taking and giving
/// back its flag. `is_native` says which callees have no body.
pub fn narrow_call_flags(body: &mut Body, tys: &TyInterner, is_native: &dyn Fn(&str) -> bool) {
    let n = body.blocks.len();
    let preds = predecessors(body);
    let mut narrow: Vec<(u32, usize, usize)> = Vec::new();
    for bi in 0..n {
        let block = &body.blocks[bi];
        let TerminatorKind::Call {
            func:
                Operand::Const(Const {
                    kind: ConstKind::FnDef(callee),
                    ..
                }),
            args,
            target: Some(target),
            ..
        } = &block.terminator.kind
        else {
            continue;
        };
        if !is_native(&callee.name) || preds[target.index()].len() != 1 {
            continue;
        }
        let plain_args = args.iter().all(|a| match a {
            Operand::Copy(p) | Operand::Move(p) => {
                place_ty(body, tys, p).is_ok_and(|pt| plain(tys, pt.ty, &mut Vec::new()))
            }
            Operand::Const(c) => plain(tys, c.ty, &mut Vec::new()),
        });
        if !plain_args {
            continue;
        }
        // The trailing acquires, all of one loan.
        let mut loan = None;
        let mut first = block.statements.len();
        for (si, st) in block.statements.iter().enumerate().rev() {
            match &st.kind {
                StatementKind::BorrowFlag(FlagOp::Acquire { loan: l, .. })
                    if loan.is_none_or(|x| x == *l) =>
                {
                    loan = Some(*l);
                    first = si;
                }
                _ => break,
            }
        }
        let Some(loan) = loan else { continue };
        // The target's leading releases include this loan's.
        let releases_here = body.blocks[target.index()]
            .statements
            .iter()
            .take_while(|st| matches!(st.kind, StatementKind::BorrowFlag(FlagOp::Release { .. })))
            .any(|st| matches!(st.kind, StatementKind::BorrowFlag(FlagOp::Release { loan: l }) if l == loan));
        if releases_here {
            narrow.push((loan, bi, first));
        }
    }
    for (loan, bi, first) in narrow {
        // The loan is acquired nowhere else and released nowhere else.
        let elsewhere = body.blocks.iter().enumerate().any(|(b, block)| {
            block
                .statements
                .iter()
                .enumerate()
                .any(|(si, st)| match &st.kind {
                    StatementKind::BorrowFlag(FlagOp::Acquire { loan: l, .. }) => {
                        *l == loan && !(b == bi && si >= first)
                    }
                    _ => false,
                })
        });
        if elsewhere {
            continue;
        }
        let target = match &body.blocks[bi].terminator.kind {
            TerminatorKind::Call {
                target: Some(t), ..
            } => t.index(),
            _ => continue,
        };
        let only_there = body.blocks.iter().enumerate().all(|(b, block)| {
            b == target
                || !block.statements.iter().any(|st| {
                    matches!(st.kind, StatementKind::BorrowFlag(FlagOp::Release { loan: l }) if l == loan)
                })
        });
        if !only_there {
            continue;
        }
        for st in &mut body.blocks[bi].statements[first..] {
            if let StatementKind::BorrowFlag(FlagOp::Acquire { place, kind, .. }) = &st.kind {
                st.kind = StatementKind::BorrowFlag(FlagOp::Check {
                    place: place.clone(),
                    kind: *kind,
                });
            }
        }
        for st in &mut body.blocks[target].statements {
            if matches!(st.kind, StatementKind::BorrowFlag(FlagOp::Release { loan: l }) if l == loan)
            {
                st.kind = StatementKind::Nop;
            }
        }
    }
}

/// A type a native can be handed without running user code: no function
/// value and no type whose drop may run a user `Drop`.
fn plain(tys: &TyInterner, t: Ty, seen: &mut Vec<Ty>) -> bool {
    if seen.contains(&t) {
        return true;
    }
    seen.push(t);
    match tys.kind(t) {
        TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::Bool
        | TyKind::Char
        | TyKind::Unit
        | TyKind::Str
        | TyKind::Intrinsic(IntrinsicTy::String) => true,
        TyKind::Ref(t) | TyKind::MutRef(t) | TyKind::Slice(t) | TyKind::Array(t, _) => {
            plain(tys, t, seen)
        }
        TyKind::Tuple(ts) => ts.iter().all(|&t| plain(tys, t, seen)),
        TyKind::Intrinsic(
            IntrinsicTy::Vec(e)
            | IntrinsicTy::Set(e)
            | IntrinsicTy::VecDeque(e)
            | IntrinsicTy::SortedSet(e),
        ) => plain(tys, e, seen),
        TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
            plain(tys, k, seen) && plain(tys, v, seen)
        }
        TyKind::Weak(_) => true,
        TyKind::Adt(a) | TyKind::Shared(a) => {
            if tys.has_drop_impl(t) {
                return false;
            }
            let adt = tys.adt(a);
            let is_enum = adt.is_enum;
            let counts: Vec<usize> = adt.variants.iter().map(|v| v.fields.len()).collect();
            counts.iter().enumerate().all(|(v, n)| {
                (0..*n as u32).all(|f| {
                    tys.field_ty(t, is_enum.then_some(v as u32), f)
                        .is_some_and(|ft| plain(tys, ft, seen))
                })
            })
        }
        _ => false,
    }
}

/// Is `place` a field of a `shared` value whose type takes a flag?
pub fn is_flagged_place(body: &Body, tys: &TyInterner, place: &Place) -> bool {
    let Some((ProjElem::Field(..), base)) = place.projection.split_last() else {
        return false;
    };
    let base = Place {
        local: place.local,
        projection: base.to_vec(),
    };
    let (Ok(b), Ok(f)) = (place_ty(body, tys, &base), place_ty(body, tys, place)) else {
        return false;
    };
    matches!(tys.kind(b.ty), TyKind::Shared(_)) && takes_flag(tys, f.ty)
}

/// The flagged fields `place` reaches its target through, outermost
/// first: an access of kind `kind` holds the innermost one that way and
/// the others as a reader.
fn flagged_prefixes(
    body: &Body,
    tys: &TyInterner,
    place: &Place,
    kind: BorrowKind,
) -> Vec<(Place, BorrowKind)> {
    let mut out: Vec<(Place, BorrowKind)> = (1..=place.projection.len())
        .map(|i| Place {
            local: place.local,
            projection: place.projection[..i].to_vec(),
        })
        .filter(|p| is_flagged_place(body, tys, p))
        .map(|p| (p, BorrowKind::Shared))
        .collect();
    if let Some(last) = out.last_mut() {
        last.1 = kind;
    }
    out
}

/// §6.2: a field is read as a value, and so takes no flag, when its type
/// is `Copy` or counted (a handle, or a handle aggregate).
fn takes_flag(tys: &TyInterner, t: Ty) -> bool {
    !tys.is_copy(t) && !counted(tys, t)
}

/// A `shared` or `weak` handle, or an `Option` or tuple of only handles, `Copy`
/// parts and further such aggregates with at least one handle (§6.1).
fn counted(tys: &TyInterner, t: Ty) -> bool {
    let parts: Vec<Ty> = match tys.kind(t) {
        TyKind::Shared(_) | TyKind::Weak(_) => return true,
        TyKind::Tuple(ts) => ts.to_vec(),
        TyKind::Adt(a) if tys.adt(a).name == "Option" => {
            let adt = tys.adt(a);
            let Some(some) = adt.variants.iter().position(|v| v.name == "Some") else {
                return false;
            };
            match tys.field_ty(t, Some(some as u32), 0) {
                Some(p) => vec![p],
                None => return false,
            }
        }
        _ => return false,
    };
    parts.iter().all(|&p| counted(tys, p) || tys.is_copy(p))
        && parts.iter().any(|&p| counted(tys, p))
}

fn predecessors(body: &Body) -> Vec<Vec<usize>> {
    let mut preds = vec![Vec::new(); body.blocks.len()];
    for (bi, block) in body.blocks.iter().enumerate() {
        for s in block.terminator.kind.successors() {
            preds[s.index()].push(bi);
        }
    }
    preds
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::tests::run_source;
    use crate::mir::parse::parse_module;
    use crate::mir::validate;

    const BAG: &str = "shared struct Bag { mut items: Vec[i64], mut n: i64 }\n";

    fn run(src: &str) -> (String, Option<i32>) {
        run_source(&format!("{BAG}{src}")).unwrap_or_else(|e| panic!("{e}"))
    }

    /// §6.2 `panic_shared_field_alias`: pushing to a field through one
    /// handle while a loop borrows it through another panics.
    #[test]
    fn mir_flags_conflict_through_another_handle_panics() {
        let fill =
            "fn fill(a: ref Bag, b: ref Bag) {\n    for x in a.items { b.items.push(x); }\n}\n";
        let main = |args: &str| {
            format!(
                "{fill}fn main() {{\n    let a = Bag {{ items: Vec.new(), n: 0 }};\n    a.items.push(1);\n    let b = Bag {{ items: Vec.new(), n: 0 }};\n    let c = a;\n    println(\"start\");\n    fill({args});\n    println(f\"{{a.items.len()}} {{b.items.len()}}\");\n}}\n"
            )
        };
        assert_eq!(run(&main("a, c")), ("start\n".into(), Some(101)));
        // Two objects: no conflict.
        assert_eq!(run(&main("a, b")), ("start\n1 1\n".into(), Some(0)));
    }

    /// A write through another handle checks the flag; a `Copy` field
    /// takes none, and a borrow that has ended holds nothing.
    #[test]
    fn mir_flags_writes_copy_fields_and_ended_borrows() {
        let reset = "fn reset(a: ref Bag, b: ref Bag) {\n    for x in a.items { b.n = b.n + x; b.items = Vec.new(); }\n}\n";
        let src = |args: &str| {
            format!(
                "{reset}fn main() {{\n    let a = Bag {{ items: Vec.new(), n: 0 }};\n    a.items.push(7);\n    let b = Bag {{ items: Vec.new(), n: 0 }};\n    reset({args});\n    println(f\"{{a.n}} {{b.n}}\");\n}}\n"
            )
        };
        assert_eq!(run(&src("a, a")), ("".into(), Some(101)));
        assert_eq!(run(&src("a, b")), ("0 7\n".into(), Some(0)));
        // Copy fields through two handles, and a loop whose borrow ended
        // before the write through the other handle.
        let seq = "fn main() {\n    let a = Bag { items: Vec.new(), n: 0 };\n    let c = a;\n    a.items.push(2);\n    for x in a.items { c.n = c.n + x; a.n = a.n + 1; }\n    c.items.push(3);\n    println(f\"{a.items.len()} {a.n}\");\n}\n";
        assert_eq!(run(seq), ("2 3\n".into(), Some(0)));
    }

    /// The text form round-trips, and the validator refuses a flag on a
    /// place that is not a flagged field.
    #[test]
    fn mir_flags_text_form_and_validation() {
        let text = "\
struct Bag { items: Vec[i64], n: i64 }

fn main(_1: shared Bag) -> () {
    let mut _0: ();
    bb0: {
        flag_acquire(&_1.0, L0);
        flag_acquire(&mut _1.0, L1);
        flag_release(L0);
        flag_check(&mut _1.0);
        flag_check(&_1.1);
        _0 = const ();
        return;
    }
}
";
        let m = parse_module(text).unwrap();
        let body = &m.bodies[0];
        let printed = crate::mir::pretty_body(body, &m.tys);
        for line in [
            "flag_acquire(&_1.0, L0);",
            "flag_acquire(&mut _1.0, L1);",
            "flag_release(L0);",
            "flag_check(&mut _1.0);",
        ] {
            assert!(printed.contains(line), "{printed}");
        }
        let errs = validate(body, &m.tys);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            errs[0].contains("borrow flag on _1.1, of type i64"),
            "{errs:?}"
        );
    }
}
