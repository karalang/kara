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
//! before the add, and nothing but its drops after, on blocks reached
//! only through the add.

use super::rc_pairs::{statement_mentions, terminator_mentions};
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
        let Some((r, t, def, drops)) = candidate(body, tys, &preds, bi) else {
            continue;
        };
        let (db, ds) = def;
        body.blocks[db].statements[ds].kind = StatementKind::Nop;
        if let TerminatorKind::Call { args, .. } = &mut body.blocks[bi].terminator.kind {
            debug_assert!(matches!(&args[0], Operand::Move(p) if p.local == r));
            args[0] = Operand::Move(Place::local(t));
        }
        for d in drops {
            let k = &mut body.blocks[d].terminator.kind;
            if let TerminatorKind::Drop { target, .. } = *k {
                *k = TerminatorKind::Goto { target };
            }
        }
    }
}

type Candidate = (Local, Local, (usize, usize), Vec<usize>);

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
    // After the add: only drops of `_t`, on blocks reached only through it.
    let start = next.index();
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
    (!drops.is_empty()).then_some((r, t, (db, ds), drops))
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
        assert_eq!(moved_lefts(src, "f"), (2, 2));
        assert_eq!(run_source(src).expect("runs").0, "ab+cd\nab\n");
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
