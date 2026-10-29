//! Let `a + b` take a left `String` that nothing reads again.
//!
//! `String.add` borrows both sides (core semantics §3.1), so `a + b`
//! copies `a` and appends `b`. When `a` is a temporary, as every operand
//! but the first of a chain `x.clone() + "+" + y` is, the copy is the
//! only thing done with it before its drop. This pass rewrites
//! `_r = &_t; .. String.add(move _r, b)` to `String.add(move _t, b)` and
//! the drops of `_t` after it to `goto`s, so the add appends to `_t`'s
//! buffer in place: kata 282's expression search made twice legacy's
//! allocations without it.
//!
//! Like `rc_pairs` it runs on elaborated bodies, where every drop is
//! explicit and a panic aborts, and leaves coroutines and `par` bodies
//! alone. `_t` is taken only when nothing but the borrow mentions it
//! before the add, no other borrow of it can be read at the add, and
//! after it either nothing but its drops mention it, on blocks reached
//! only through the add, or (for a `_t` with a drop flag, such as
//! `piece` in `if out == "" { out = piece } else { out = piece + " " + out }`)
//! nothing but drops its flag guards, up to `_t`'s next value: clearing
//! the flag at the add skips them.
//!
//! [`fuse_concat_chains`] then folds `(x + a) + b` into one
//! `String.add(x, a, b)`, which the backend grows once by the summed
//! lengths where two adds grew twice (kata 273's `piece + " " + out`,
//! B-2026-10-28-42).

use super::borrowck::holds_borrow;
use super::rc_pairs::{rvalue_mentions, statement_mentions, terminator_mentions};
use super::syntax::*;
use super::ty::{IntrinsicTy, TyInterner, TyKind};

/// Rewrites every `String.add` of `body` whose left side is a temporary
/// borrowed for it alone.
pub fn take_concat_lefts(body: &mut Body, tys: &TyInterner) {
    if !body.par_regions.is_empty() {
        return;
    }
    let preds = super::rc_pairs::predecessors(body);
    for bi in 0..body.blocks.len() {
        let Some((r, t, def, after)) = candidate(body, tys, &preds, bi) else {
            continue;
        };
        let (db, ds) = def;
        body.blocks[db].statements[ds].kind = StatementKind::Nop;
        if let TerminatorKind::Call { args, .. } = &mut body.blocks[bi].terminator.kind {
            debug_assert!(matches!(&args[0], Operand::Move(p) if p.local == r));
            args[0] = Operand::Move(Place::local(t));
        }
        match after {
            After::Drops(drops) => {
                for d in drops {
                    let k = &mut body.blocks[d].terminator.kind;
                    if let TerminatorKind::Drop { target, .. } = *k {
                        *k = TerminatorKind::Goto { target };
                    }
                }
            }
            After::Flag(f) => {
                let source_info = body.blocks[bi].terminator.source_info;
                let ty = body.local(f).ty;
                body.blocks[bi].statements.push(Statement {
                    kind: StatementKind::Assign(
                        Place::local(f),
                        Rvalue::Use(Operand::Const(Const {
                            ty,
                            kind: ConstKind::Scalar(0),
                        })),
                    ),
                    source_info,
                });
            }
        }
    }
}

/// What makes `_t`'s drops after the add redundant.
enum After {
    /// The drops to turn into `goto`s.
    Drops(Vec<usize>),
    /// The drop flag to clear at the add.
    Flag(Local),
}

type Candidate = (Local, Local, (usize, usize), After);

/// For a `String.add` ending block `bi`: its borrow's local, the borrowed
/// temporary, where the borrow is made, and the drops of the temporary
/// that the add makes redundant.
fn candidate(body: &Body, tys: &TyInterner, preds: &[Vec<usize>], bi: usize) -> Option<Candidate> {
    let TerminatorKind::Call {
        func,
        args,
        destination,
        target: Some(next),
        ..
    } = &body.blocks[bi].terminator.kind
    else {
        return None;
    };
    let Operand::Const(Const {
        kind: ConstKind::FnDef(inst),
        ..
    }) = func
    else {
        return None;
    };
    if inst.name != "String.add" {
        return None;
    }
    let [Operand::Move(rp), rhs] = args.as_slice() else {
        return None;
    };
    if !rp.projection.is_empty() {
        return None;
    }
    let r = rp.local;
    // `_r`'s one definition, `_r = &_t`.
    let mut def = None;
    for (b, data) in body.blocks.iter().enumerate() {
        for (s, st) in data.statements.iter().enumerate() {
            if statement_mentions(&st.kind, r) {
                match &st.kind {
                    StatementKind::Assign(d, Rvalue::Ref(BorrowKind::Shared, p))
                        if *d == Place::local(r) && p.projection.is_empty() && def.is_none() =>
                    {
                        def = Some((b, s, p.local));
                    }
                    StatementKind::StorageLive(_) | StatementKind::StorageDead(_) => {}
                    _ => return None,
                }
            }
        }
        if b != bi && terminator_mentions(&data.terminator.kind, r) {
            return None;
        }
    }
    let (db, ds, t) = def?;
    if !matches!(
        tys.kind(body.local(t).ty),
        TyKind::Intrinsic(IntrinsicTy::String)
    ) || t == destination.local
        || operand_mentions(rhs, t)
        || destination.local == r
        || t == Local::RETURN_PLACE
        || !borrows_end_at_calls(body, tys, t, r)
    {
        return None;
    }
    // Between the borrow and the add, in the borrow's block: nothing else
    // touches `_t`. The borrow's block must be the add's, or lead straight
    // to it through blocks that do not mention `_t`.
    let mut b = db;
    let mut from = ds + 1;
    loop {
        if body.blocks[b].statements[from..]
            .iter()
            .any(|s| statement_mentions(&s.kind, t))
        {
            return None;
        }
        if b == bi {
            break;
        }
        let term = &body.blocks[b].terminator.kind;
        // A check on the way (`a + (n - x).to_string()`'s overflow test)
        // branches to an abort, which drops nothing: the way on is the
        // other branch.
        let succ: Vec<_> = term
            .successors()
            .into_iter()
            .filter(|s| !aborts(body, s.index(), t))
            .collect();
        if succ.len() != 1 || terminator_mentions(term, t) || preds[succ[0].index()].len() != 1 {
            return None;
        }
        b = succ[0].index();
        from = 0;
        if b == db {
            return None;
        }
    }
    let start = next.index();
    let after = match drops_after(body, preds, bi, db, start, t) {
        Some(drops) if !drops.is_empty() => After::Drops(drops),
        _ => After::Flag(flag_after(body, bi, start, t)?),
    };
    Some((r, t, (db, ds), after))
}

/// After the add: only drops of `_t`, on blocks reached only through it.
fn drops_after(
    body: &Body,
    preds: &[Vec<usize>],
    bi: usize,
    db: usize,
    start: usize,
    t: Local,
) -> Option<Vec<usize>> {
    let mut region = vec![bi];
    let mut drops = Vec::new();
    let mut work = vec![start];
    if preds[start].len() != 1 {
        return None;
    }
    region.push(start);
    while let Some(b) = work.pop() {
        let data = &body.blocks[b];
        if data.statements.iter().any(|s| {
            statement_mentions(&s.kind, t) && !matches!(s.kind, StatementKind::StorageDead(_))
        }) {
            return None;
        }
        match &data.terminator.kind {
            TerminatorKind::Drop { place, .. } if *place == Place::local(t) => {
                drops.push(b);
                continue;
            }
            TerminatorKind::Return => return None,
            k if terminator_mentions(k, t) => return None,
            k => {
                for s in k.successors() {
                    let s = s.index();
                    if s == bi || s == db {
                        return None;
                    }
                    if region.contains(&s) {
                        continue;
                    }
                    if region.len() >= 64 {
                        return None;
                    }
                    region.push(s);
                    work.push(s);
                }
            }
        }
    }
    if region[1..]
        .iter()
        .any(|&b| preds[b].iter().any(|q| !region.contains(q)))
    {
        return None;
    }
    Some(drops)
}

/// `_t`'s drop flag, when every way on from the add (`start`) reaches a
/// new value of `_t`, or the end, having met `_t` only in drops that
/// flag guards: with the flag cleared at the add, each takes its `0`
/// branch, as it would had `_t` been moved there.
fn flag_after(body: &Body, bi: usize, start: usize, t: Local) -> Option<Local> {
    let f = body
        .drop_flags
        .iter()
        .find(|(p, _)| *p == Place::local(t))?
        .1;
    let mut seen = vec![false; body.blocks.len()];
    let mut work = vec![start];
    let mut visited = 0;
    'blocks: while let Some(b) = work.pop() {
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        visited += 1;
        if b == bi || visited > 256 {
            return None;
        }
        let data = &body.blocks[b];
        for s in &data.statements {
            match &s.kind {
                StatementKind::Assign(d, rv) if *d == Place::local(t) => {
                    if rvalue_mentions(rv, t) {
                        return None;
                    }
                    continue 'blocks;
                }
                StatementKind::Assign(d, Rvalue::Use(Operand::Const(c)))
                    if *d == Place::local(f) && matches!(c.kind, ConstKind::Scalar(0)) => {}
                StatementKind::StorageLive(_) | StatementKind::StorageDead(_) => {}
                k if statement_mentions(k, t) || statement_mentions(k, f) => return None,
                _ => {}
            }
        }
        match &data.terminator.kind {
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(p) | Operand::Move(p),
                targets,
            } if *p == Place::local(f) => {
                let off = targets
                    .values
                    .iter()
                    .find(|(v, _)| *v == 0)
                    .map_or(targets.otherwise, |(_, b)| *b);
                work.push(off.index());
            }
            // `_t = f(..)`: a new value, and the way ends.
            TerminatorKind::Call {
                args, destination, ..
            } if *destination == Place::local(t)
                && !args.iter().any(|a| operand_mentions(a, t)) => {}
            k if terminator_mentions(k, t) || terminator_mentions(k, f) => return None,
            k => work.extend(k.successors().into_iter().map(|s| s.index())),
        }
    }
    Some(f)
}

/// Whether every borrow of `_t` but `_r` ends at the call it is made
/// for: `_b = &_t` with `_b` moved into its block's call and mentioned
/// nowhere else, and a result that holds no borrow. A borrow that lives
/// on, such as a closure's capture (`let f = || println(a)`), would read
/// `_t` after the add has taken it.
fn borrows_end_at_calls(body: &Body, tys: &TyInterner, t: Local, r: Local) -> bool {
    for data in &body.blocks {
        for st in &data.statements {
            let StatementKind::Assign(b, Rvalue::Ref(_, p)) = &st.kind else {
                continue;
            };
            if p.local != t || b.local == r {
                continue;
            }
            let b = b.local;
            let TerminatorKind::Call {
                args, destination, ..
            } = &data.terminator.kind
            else {
                return false;
            };
            if !destination.projection.is_empty()
                || holds_borrow(tys, body.local(destination.local).ty)
                || !args
                    .iter()
                    .any(|a| matches!(a, Operand::Move(q) if *q == Place::local(b)))
            {
                return false;
            }
            let uses = body
                .blocks
                .iter()
                .map(|d| {
                    d.statements
                        .iter()
                        .filter(|s| {
                            statement_mentions(&s.kind, b)
                                && !matches!(
                                    s.kind,
                                    StatementKind::StorageLive(_) | StatementKind::StorageDead(_)
                                )
                        })
                        .count()
                        + usize::from(terminator_mentions(&d.terminator.kind, b))
                })
                .sum::<usize>();
            if uses != 2 {
                return false;
            }
        }
    }
    true
}

/// Folds `_a = String.add(l, ..xs) -> next; ..; _b = String.add(move _a, ..ys)`
/// into `_b = String.add(l, ..xs, ..ys)` when `_a` is used for nothing
/// else and the way from `next` to the second add is a straight line
/// that neither touches what the first add took nor writes what it
/// borrowed, so its operands read the same at the second add.
pub fn fuse_concat_chains(body: &mut Body, tys: &TyInterner) {
    if !body.par_regions.is_empty() {
        return;
    }
    let preds = super::rc_pairs::predecessors(body);
    loop {
        let mut fused = false;
        for b2 in 0..body.blocks.len() {
            let Some((b1, next)) = chain_link(body, tys, &preds, b2) else {
                continue;
            };
            let goto = TerminatorKind::Goto { target: next };
            let first = std::mem::replace(&mut body.blocks[b1].terminator.kind, goto);
            let TerminatorKind::Call { args: xs, .. } = first else {
                unreachable!("chain_link checked the call");
            };
            if let TerminatorKind::Call { args, .. } = &mut body.blocks[b2].terminator.kind {
                args.splice(0..1, xs);
            }
            fused = true;
        }
        if !fused {
            return;
        }
    }
}

/// The `String.add` call ending a block, with its arguments, destination
/// and target.
fn string_add(t: &TerminatorKind) -> Option<(&[Operand], &Place, BasicBlock)> {
    let TerminatorKind::Call {
        func:
            Operand::Const(Const {
                kind: ConstKind::FnDef(inst),
                ..
            }),
        args,
        destination,
        target: Some(next),
        ..
    } = t
    else {
        return None;
    };
    (inst.name == "String.add").then_some((args.as_slice(), destination, *next))
}

/// For the `String.add` ending `b2` whose left side is `move _a`: the
/// block of the `String.add` that made `_a`, and where it goes on, when
/// the two fuse.
fn chain_link(
    body: &Body,
    tys: &TyInterner,
    preds: &[Vec<usize>],
    b2: usize,
) -> Option<(usize, BasicBlock)> {
    let (args, _, _) = string_add(&body.blocks[b2].terminator.kind)?;
    let Some(Operand::Move(pa)) = args.first() else {
        return None;
    };
    if !pa.projection.is_empty() {
        return None;
    }
    let a = pa.local;
    // `_a` is the first add's result and the second's left, nothing more.
    let mut def = None;
    for (b, data) in body.blocks.iter().enumerate() {
        if data.statements.iter().any(|s| {
            statement_mentions(&s.kind, a)
                && !matches!(
                    s.kind,
                    StatementKind::StorageLive(_) | StatementKind::StorageDead(_)
                )
        }) {
            return None;
        }
        if b == b2 || !terminator_mentions(&data.terminator.kind, a) {
            continue;
        }
        match string_add(&data.terminator.kind) {
            Some((xs, d, next))
                if *d == Place::local(a)
                    && def.is_none()
                    && !xs.iter().any(|x| operand_mentions(x, a)) =>
            {
                def = Some((b, xs, next));
            }
            _ => return None,
        }
    }
    let (b1, xs, next) = def?;
    // What the first add read, and must read the same at the second:
    // locals it took or borrowed (not to be touched on the way), and the
    // places those borrows point at (not to be written).
    let mut held = Vec::new();
    let mut lent = Vec::new();
    for x in xs {
        match x {
            Operand::Const(_) => {}
            Operand::Move(p) if p.projection.is_empty() => {
                held.push(p.local);
                match tys.kind(body.local(p.local).ty) {
                    TyKind::Intrinsic(IntrinsicTy::String) => {}
                    TyKind::Ref(_) => lent.push(shared_borrow_of(body, p.local)?),
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
    // From the first add to the second: a straight line, past checks
    // whose failure aborts.
    if preds[next.index()].len() != 1 {
        return None;
    }
    let mut b = next.index();
    for _ in 0..64 {
        let data = &body.blocks[b];
        for s in &data.statements {
            if held.iter().any(|&l| statement_mentions(&s.kind, l)) {
                return None;
            }
            let reads = matches!(
                s.kind,
                StatementKind::Assign(_, Rvalue::Ref(BorrowKind::Shared, _))
            );
            if !reads && lent.iter().any(|&l| statement_mentions(&s.kind, l)) {
                return None;
            }
        }
        if b == b2 {
            return Some((b1, next));
        }
        let term = &data.terminator.kind;
        if held
            .iter()
            .chain(&lent)
            .any(|&l| terminator_mentions(term, l))
        {
            return None;
        }
        let succ: Vec<_> = term
            .successors()
            .into_iter()
            .filter(|s| {
                !matches!(
                    body.blocks[s.index()].terminator.kind,
                    TerminatorKind::Abort { .. }
                )
            })
            .collect();
        if succ.len() != 1 || preds[succ[0].index()].len() != 1 || succ[0].index() == b1 {
            return None;
        }
        b = succ[0].index();
    }
    None
}

/// The local `_r`'s one definition `_r = &place` borrows.
fn shared_borrow_of(body: &Body, r: Local) -> Option<Local> {
    let mut found = None;
    for data in &body.blocks {
        for s in &data.statements {
            if let StatementKind::Assign(d, rv) = &s.kind {
                if d.local != r {
                    continue;
                }
                match rv {
                    Rvalue::Ref(BorrowKind::Shared, p)
                        if found.is_none() && d.projection.is_empty() =>
                    {
                        found = Some(p.local)
                    }
                    _ => return None,
                }
            }
        }
        if matches!(&data.terminator.kind, TerminatorKind::Call { destination, .. } if destination.local == r)
        {
            return None;
        }
    }
    found
}

/// Whether block `b` only aborts, without touching `t`: a failed check's
/// exit, which runs no drop (`a panic aborts`, above).
fn aborts(body: &Body, b: usize, t: Local) -> bool {
    let data = &body.blocks[b];
    matches!(data.terminator.kind, TerminatorKind::Abort { .. })
        && !data
            .statements
            .iter()
            .any(|s| statement_mentions(&s.kind, t))
}

fn operand_mentions(o: &Operand, l: Local) -> bool {
    match o {
        Operand::Copy(p) | Operand::Move(p) => {
            p.local == l
                || p.projection
                    .iter()
                    .any(|e| matches!(e, ProjElem::Index(i) if *i == l))
        }
        Operand::Const(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::lower::tests::run_source;
    use crate::mir::{pretty_body, ConstKind, Operand, TerminatorKind};

    /// The `String.add` calls of `name` whose left side is moved in.
    fn moved_lefts(src: &str, name: &str) -> (usize, usize) {
        let l = build_source(src).expect("MIR");
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == name)
            .expect("the body");
        let mut adds = 0;
        let mut moved = 0;
        for b in &body.blocks {
            if let TerminatorKind::Call {
                func: Operand::Const(c),
                args,
                ..
            } = &b.terminator.kind
            {
                if matches!(&c.kind, ConstKind::FnDef(i) if i.name == "String.add") {
                    adds += 1;
                    let t = body.local(match &args[0] {
                        Operand::Move(p) | Operand::Copy(p) => p.local,
                        _ => continue,
                    });
                    if l.tys.display(t.ty) == "String" {
                        moved += 1;
                    }
                }
            }
        }
        eprintln!("{}", pretty_body(body, &l.tys));
        (adds, moved)
    }

    #[test]
    fn a_chain_appends_to_its_temporaries() {
        let src = "fn f(e: ref String, p: ref String) -> String {\n    e.clone() + \"+\" + p.clone()\n}\nfn main() {\n    let e = String.from(\"ab\");\n    println(f(e, String.from(\"cd\")));\n    println(e);\n}\n";
        // One add, fused: `String.add(move e', "+", p')`.
        assert_eq!(moved_lefts(src, "f"), (1, 1));
        assert_eq!(run_source(src).expect("runs").0, "ab+cd\nab\n");
    }

    /// `piece` has a drop flag (moved on one branch, added on the
    /// other): the add takes it and clears the flag, and the chain is one
    /// add of three operands (kata 273, B-2026-10-28-42).
    #[test]
    fn a_flagged_left_is_taken_and_its_chain_fused() {
        let src = "fn f(n: i64) -> String {\n    let mut out = n.to_string();\n    let piece = (n + 1).to_string();\n    if n > 5 {\n        out = piece;\n    } else {\n        out = piece + \" \" + out;\n    }\n    out\n}\nfn main() {\n    println(f(3));\n    println(f(7));\n}\n";
        assert_eq!(moved_lefts(src, "f"), (1, 1));
        let l = build_source(src).expect("MIR");
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == "f")
            .unwrap();
        assert!(body.blocks.iter().any(|b| matches!(&b.terminator.kind,
            TerminatorKind::Call { args, .. } if args.len() == 3)));
        assert_eq!(run_source(src).expect("runs").0, "4 3\n8\n");
    }

    /// A closure's capture of `a` is read after the add, so the add must
    /// copy `a`, not take it.
    #[test]
    fn a_left_a_closure_borrows_is_copied() {
        let src = "fn main() {\n    let a = 12.to_string();\n    let f = || println(a);\n    let b = a + \"y\";\n    f();\n    println(b);\n}\n";
        assert_eq!(moved_lefts(src, "main"), (1, 0));
        assert_eq!(run_source(src).expect("runs").0, "12\n12y\n");
    }

    /// `bump` writes `m` between the two adds, so they stay apart: the
    /// first reads `m` before the write.
    #[test]
    fn a_chain_whose_borrow_is_written_between_stays_apart() {
        let src = "fn bump(s: mut ref String) -> String {\n    s.push_str(\"!\");\n    s.clone()\n}\nfn main() {\n    let mut m = String.from(\"x\");\n    let s = m + \" \" + bump(mut m);\n    println(f\"{s} {m}\");\n}\n";
        assert_eq!(moved_lefts(src, "main").0, 2);
        assert_eq!(run_source(src).expect("runs").0, "x x! x!\n");
    }

    #[test]
    fn a_named_left_read_again_is_copied() {
        let src = "fn main() {\n    let a = String.from(\"x\");\n    let b = a + \"y\";\n    println(f\"{a} {b}\");\n}\n";
        assert_eq!(moved_lefts(src, "main"), (1, 0));
        assert_eq!(run_source(src).expect("runs").0, "x xy\n");
    }

    /// The right side's overflow check sits between the borrow of `s` and
    /// the add; its failure aborts, so `s` is still taken (kata 1006,
    /// B-2026-10-28-30).
    #[test]
    fn a_checked_right_side_still_appends_in_place() {
        let src = "fn f(n: i64) -> String {\n    let mut s = n.to_string();\n    for x in 1..n {\n        s = s + (n - x).to_string();\n    }\n    s\n}\nfn main() {\n    println(f(4));\n}\n";
        assert_eq!(moved_lefts(src, "f"), (1, 1));
        assert_eq!(run_source(src).expect("runs").0, "4321\n");
    }
}
