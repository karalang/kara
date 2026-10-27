//! Count pairs that cancel: a `retain` whose source is dropped unused.
//!
//! `x = retain(P)` copies the handle in `P` with a count. When `P`'s local
//! is not used again before a `drop` of it, on every path, that drop gives
//! back the count the retain just took: together they move the handle.
//! This pass rewrites such a retain to `x = move P` and the drops it
//! reaches to `goto`s, so a list walk (`while let Some(n) = cur { ...;
//! cur = n.next }`) counts once per step rather than twice.
//!
//! It runs on elaborated bodies, where every drop is explicit and a panic
//! aborts (no unwind edge reaches a drop). `P` is the local itself when it
//! is a `shared` handle, or the payload of an `Option` local, so dropping
//! the local drops only the handle. A drop is rewritten only when every
//! path to it comes through the retain, so no other path loses its drop.
//! A local that is ever mutably borrowed, a body with `par` regions, and a
//! coroutine (whose frame drops what it holds by drop flags) are left as
//! they are.

use super::syntax::*;
use super::ty::{TyInterner, TyKind};

/// Rewrites every cancelling retain/drop pair in `body`, which must not be
/// a coroutine.
pub fn elide_retains(body: &mut Body, tys: &TyInterner) {
    if !body.par_regions.is_empty() {
        return;
    }
    let mut_borrowed = mut_borrowed_locals(body);
    let preds = predecessors(body);
    for bi in 0..body.blocks.len() {
        let mut si = 0;
        while si < body.blocks[bi].statements.len() {
            if let Some(drops) = candidate(body, tys, &mut_borrowed, &preds, bi, si) {
                let StatementKind::Assign(_, rv) = &mut body.blocks[bi].statements[si].kind else {
                    unreachable!("a candidate is an assignment")
                };
                let Rvalue::Retain(p) = rv else {
                    unreachable!("a candidate is a retain")
                };
                *rv = Rvalue::Use(Operand::Move(p.clone()));
                for d in drops {
                    let t = &mut body.blocks[d.index()].terminator.kind;
                    let TerminatorKind::Drop { target, .. } = *t else {
                        unreachable!("a candidate's drops are drops")
                    };
                    *t = TerminatorKind::Goto { target };
                }
            }
            si += 1;
        }
    }
}

/// The blocks whose `drop` of the retain's source the retain at `bi`/`si`
/// cancels, when it cancels one.
fn candidate(
    body: &Body,
    tys: &TyInterner,
    mut_borrowed: &[bool],
    preds: &[Vec<usize>],
    bi: usize,
    si: usize,
) -> Option<Vec<BasicBlock>> {
    let StatementKind::Assign(dest, Rvalue::Retain(p)) = &body.blocks[bi].statements[si].kind
    else {
        return None;
    };
    let l = p.local;
    let d = dest.local;
    if d == l
        || !dest.projection.is_empty()
        || mut_borrowed[l.index()]
        || mut_borrowed[d.index()]
        || !drops_only_handle(body, tys, p)
    {
        return None;
    }
    // The rest of the retain's block, then the blocks after it.
    if body.blocks[bi].statements[si + 1..]
        .iter()
        .any(|s| statement_mentions(&s.kind, l) || statement_releases(&s.kind, d))
    {
        return None;
    }
    let mut region = vec![bi];
    let mut drops = Vec::new();
    let mut work = vec![bi];
    while let Some(b) = work.pop() {
        let t = &body.blocks[b].terminator.kind;
        match t {
            TerminatorKind::Drop { place, .. } if *place == Place::local(l) => {
                drops.push(BasicBlock(b as u32));
                continue;
            }
            TerminatorKind::Return => return None,
            _ if terminator_mentions(t, l) || terminator_releases(t, d) => return None,
            _ => {}
        }
        for s in t.successors() {
            let s = s.index();
            if s == bi {
                return None;
            }
            if region.contains(&s) {
                continue;
            }
            if region.len() >= 64
                || body.blocks[s]
                    .statements
                    .iter()
                    .any(|st| statement_mentions(&st.kind, l) || statement_releases(&st.kind, d))
            {
                return None;
            }
            region.push(s);
            work.push(s);
        }
    }
    // Every block after the retain is reached only through it.
    if region[1..]
        .iter()
        .any(|&b| preds[b].iter().any(|q| !region.contains(q)))
    {
        return None;
    }
    (!drops.is_empty()).then_some(drops)
}

/// Whether dropping `p`'s local drops only the handle at `p`: `p` is a
/// `shared` local, or the payload of an `Option` local.
fn drops_only_handle(body: &Body, tys: &TyInterner, p: &Place) -> bool {
    let lt = body.local(p.local).ty;
    match p.projection.as_slice() {
        [] => matches!(tys.kind(lt), TyKind::Shared(_)),
        [ProjElem::Downcast(v), ProjElem::Field(f, ft)] => {
            f.0 == 0
                && matches!(tys.kind(*ft), TyKind::Shared(_))
                && matches!(tys.kind(lt), TyKind::Adt(_))
                && tys.tcx().adt_of(lt).is_some_and(|(adt, _)| {
                    adt.name == "Option"
                        && !adt.has_drop_impl
                        && adt
                            .variants
                            .get(v.index())
                            .is_some_and(|var| var.name == "Some")
                })
        }
        _ => false,
    }
}

/// Whether `s` may give back the count `d` holds: it writes, moves out
/// of or ends `d`. Until then the retain's object outlives its source's
/// drop either way, so a `Drop` body, or a `weak` upgrade, sees no
/// difference when the pair goes.
///
/// Moving `d` into the value the body returns (`_0 = Split { move d, .. }`)
/// is not a release: the caller gets the count, after every drop in the
/// body. The return place is then held to the same rule as `d`, so a drop
/// or a move out of it in between still keeps the retain.
fn statement_releases(s: &StatementKind, d: Local) -> bool {
    let held = |l: Local| l == d || l == Local::RETURN_PLACE;
    let moves = |o: &Operand| matches!(o, Operand::Move(p) if held(p.local));
    match s {
        StatementKind::Assign(dest, rv) => {
            let into_return = dest.local == Local::RETURN_PLACE && dest.projection.is_empty();
            dest.local == d
                || match rv {
                    Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => {
                        moves(o) && !into_return
                    }
                    Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
                        moves(a) || moves(b)
                    }
                    Rvalue::Aggregate(_, ops) => !into_return && ops.iter().any(moves),
                    _ => false,
                }
        }
        StatementKind::StorageDead(x) => held(*x),
        StatementKind::SetDiscriminant(p, _) | StatementKind::FreeBox(p) => held(p.local),
        _ => false,
    }
}

fn terminator_releases(t: &TerminatorKind, d: Local) -> bool {
    let held = |l: Local| l == d || l == Local::RETURN_PLACE;
    match t {
        TerminatorKind::Call {
            args, destination, ..
        } => {
            destination.local == d
                || args
                    .iter()
                    .any(|o| matches!(o, Operand::Move(p) if held(p.local)))
        }
        TerminatorKind::Drop { place, .. } => held(place.local),
        _ => false,
    }
}

fn mut_borrowed_locals(body: &Body) -> Vec<bool> {
    let mut out = vec![false; body.locals.len()];
    for b in &body.blocks {
        for s in &b.statements {
            if let StatementKind::Assign(_, Rvalue::Ref(BorrowKind::Mut, p)) = &s.kind {
                out[p.local.index()] = true;
            }
        }
    }
    out
}

pub(super) fn predecessors(body: &Body) -> Vec<Vec<usize>> {
    let mut preds = vec![Vec::new(); body.blocks.len()];
    for (i, b) in body.blocks.iter().enumerate() {
        for s in b.terminator.kind.successors() {
            preds[s.index()].push(i);
        }
    }
    preds
}

fn place_mentions(p: &Place, l: Local) -> bool {
    p.local == l
        || p.projection
            .iter()
            .any(|e| matches!(e, ProjElem::Index(i) if *i == l))
}

fn operand_mentions(o: &Operand, l: Local) -> bool {
    match o {
        Operand::Copy(p) | Operand::Move(p) => place_mentions(p, l),
        Operand::Const(_) => false,
    }
}

fn rvalue_mentions(rv: &Rvalue, l: Local) -> bool {
    match rv {
        Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => operand_mentions(o, l),
        Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
            operand_mentions(a, l) || operand_mentions(b, l)
        }
        Rvalue::Aggregate(_, ops) => ops.iter().any(|o| operand_mentions(o, l)),
        Rvalue::Ref(_, p) | Rvalue::Retain(p) | Rvalue::Discriminant(p) | Rvalue::Len(p) => {
            place_mentions(p, l)
        }
        Rvalue::NullaryOp(..) => false,
    }
}

pub(super) fn statement_mentions(s: &StatementKind, l: Local) -> bool {
    match s {
        StatementKind::Assign(d, rv) => place_mentions(d, l) || rvalue_mentions(rv, l),
        StatementKind::StorageLive(x) | StatementKind::StorageDead(x) => *x == l,
        StatementKind::SetDiscriminant(p, _) | StatementKind::FreeBox(p) => place_mentions(p, l),
        StatementKind::BorrowFlag(op) => match op {
            FlagOp::Acquire { place, .. } | FlagOp::Check { place, .. } => place_mentions(place, l),
            FlagOp::Release { .. } => false,
        },
        StatementKind::Nop => false,
    }
}

pub(super) fn terminator_mentions(t: &TerminatorKind, l: Local) -> bool {
    match t {
        TerminatorKind::Goto { .. }
        | TerminatorKind::Return
        | TerminatorKind::Abort { .. }
        | TerminatorKind::Unreachable => false,
        TerminatorKind::SwitchInt { discr, .. } => operand_mentions(discr, l),
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => {
            operand_mentions(func, l)
                || args.iter().any(|a| operand_mentions(a, l))
                || place_mentions(destination, l)
        }
        TerminatorKind::Drop { place, .. } => place_mentions(place, l),
    }
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::lower::tests::run_source;
    use crate::mir::{pretty_body, Rvalue, StatementKind};

    const WALK: &str = "shared struct N { v: i64, next: Option[N] }\n\
fn sum(l: Option[N]) -> i64 {\n    let mut s = 0;\n    let mut c = l;\n    loop {\n        match c {\n            Some(n) => {\n                s = s + n.v;\n                c = n.next;\n            }\n            None => break,\n        }\n    }\n    s\n}\n\
fn main() {\n    let a = N { v: 2, next: None };\n    let b = N { v: 3, next: Some(a) };\n    println(sum(Some(b)));\n    println(b.v + a.v);\n}\n";

    fn retains(src: &str, f: &str) -> usize {
        let l = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == f)
            .unwrap_or_else(|| panic!("no body {f}"));
        let n = body
            .blocks
            .iter()
            .flat_map(|b| &b.statements)
            .filter(|s| matches!(s.kind, StatementKind::Assign(_, Rvalue::Retain(_))))
            .count();
        eprintln!("{}", pretty_body(body, &l.tys));
        n
    }

    /// A list walk's binding takes the handle `c` holds, which `c = n.next`
    /// drops unused: the pair goes, and the walk still sums and frees right.
    #[test]
    fn mir_rc_pairs_list_walk_moves_its_binding() {
        assert_eq!(retains(WALK, "sum"), 0);
        assert_eq!(run_source(WALK).unwrap(), ("5\n5\n".into(), Some(0)));
    }

    /// A handle read again after a `match` on a tuple of handles, then
    /// moved into the returned value, gives its count to the caller: the
    /// retains that build the result go (kata 558's `intersect`), the
    /// tuple is matched where its parts lie so it counts nothing
    /// (B-2026-10-24-11), and the `Drop` bodies still run once each, in
    /// order.
    #[test]
    fn mir_rc_pairs_moves_a_handle_into_the_returned_value() {
        let src = "shared enum T { L(i64), N(T, T) }\n\
shared struct H { id: i64 }\n\
impl Drop for H { fn drop(mut ref self) { println(f\"d{self.id}\") } }\n\
fn two(n: i64) -> T {\n    let x = T.L(n);\n    let y = T.L(n + 1);\n    \
match (x, y) {\n        (T.L(p), T.L(q)) => {\n            if p == q {\n                return T.L(p);\n            }\n        }\n        _ => {}\n    }\n    T.N(x, y)\n}\n\
fn keep(a: H) -> Option[H] {\n    let h = H { id: a.id + 1 };\n    match (h, a) {\n        _ => {}\n    }\n    Some(h)\n}\n\
fn main() {\n    match two(3) {\n        T.N(T.L(a), T.L(b)) => println(f\"{a} {b}\"),\n        _ => println(\"?\"),\n    }\n    \
let a = H { id: 1 };\n    match keep(a) {\n        Some(h) => println(f\"k{h.id}\"),\n        None => {}\n    }\n    println(\"end\");\n}\n";
        assert_eq!(retains(src, "two"), 0);
        assert_eq!(
            run_source(src).unwrap(),
            ("3 4\nk2\nd2\nend\nd1\n".into(), Some(0))
        );
    }

    /// A source used after the retain keeps it.
    #[test]
    fn mir_rc_pairs_keeps_a_retain_whose_source_is_read_again() {
        let src = "shared struct N { v: i64 }\n\
fn main() {\n    let a = Some(N { v: 4 });\n    match a {\n        Some(n) => println(n.v),\n        None => {}\n    }\n    match a {\n        Some(m) => println(m.v),\n        None => {}\n    }\n}\n";
        assert!(retains(src, "main") >= 1);
        assert_eq!(run_source(src).unwrap(), ("4\n4\n".into(), Some(0)));
    }

    /// The copy dies before its source would have: the object's `Drop`
    /// body runs when the source's drop says, so the retain stays.
    #[test]
    fn mir_rc_pairs_keeps_a_retain_whose_copy_dies_first() {
        let src = "shared struct H { id: i64 }\n\
impl Drop for H { fn drop(mut ref self) { println(f\"d{self.id}\") } }\n\
fn main() {\n    let a = H { id: 1 };\n    let mut c = a;\n    c = H { id: 2 };\n    println(f\"c{c.id}\");\n}\n";
        assert_eq!(retains(src, "main"), 1);
        assert_eq!(run_source(src).unwrap(), ("c2\nd2\nd1\n".into(), Some(0)));
    }
}
