//! Kernighan's bit-count loop as one population count.
//!
//! `while x != 0 { x = x & (x - 1); c = c + 1 }` clears the lowest set bit
//! per trip. LLVM turns that loop into `llvm.ctpop` only when it is one
//! block, and Kāra's two overflow checks split it, so it stays a loop. The
//! legacy backend rewrites the same shape (`codegen/popcount_idiom.rs`,
//! where the argument and the measurements are); this is that rewrite on
//! an elaborated MIR body:
//!
//! ```text
//!   if x < 0 { the loop as it was }   // signed x only
//!   c = c + x.count_ones()            // checked, the `c + 1` overflow
//!   x = 0
//! ```
//!
//! For `x == 0` the loop never runs and the rewrite adds 0. A positive
//! `x` takes exactly `popcount(x)` trips, none of which overflows
//! `x - 1`, and `c` overflows on some trip exactly when `c + popcount(x)`
//! does. A negative `x` keeps its sign bit until it is the only bit left,
//! so the loop reaches `MIN - 1` and aborts, after `c` may already have
//! overflowed; it stays a loop, which aborts where it always did. (The
//! legacy rewrite guards only `x == MIN` and so counts a negative `x`
//! whose loop aborts.)
//!
//! The shape, all four blocks reached only as below:
//!
//! ```text
//!   H: f = Ne(copy x, const 0); switchInt(copy f) -> [0: EXIT, otherwise: B]
//!   B: t = CheckedSub(copy x, const 1); switchInt(copy t.1) -> [0: C, otherwise: abort]
//!   C: a = copy t.0; b = BitAnd(copy x, copy a); x = copy b;
//!      u = CheckedAdd(copy c, const 1); switchInt(copy u.1) -> [0: D, otherwise: abort]
//!   D: v = copy u.0; c = copy v; (unit assignments); goto H
//! ```
//!
//! with `f`, `t`, `a`, `b`, `u`, `v` mentioned nowhere else.

use super::rc_pairs::{statement_mentions, terminator_mentions};
use super::syntax::*;
use super::ty::{TyInterner, TyKind};
use crate::ids::DefId;

/// Rewrites each Kernighan loop in `body` into a population count.
pub fn count_bit_loops(body: &mut Body, tys: &TyInterner) {
    let mut h = 0;
    while h < body.blocks.len() {
        if let Some(m) = matched(body, tys, h) {
            rewrite(body, tys, m);
        }
        h += 1;
    }
}

struct Match {
    blocks: [usize; 4],
    exit: BasicBlock,
    /// The `x - 1` and `c + 1` overflow aborts.
    abort_sub: BasicBlock,
    abort_add: BasicBlock,
    x: Local,
    c: Local,
    f: Local,
}

fn int_const(o: &Operand) -> Option<u128> {
    match o {
        Operand::Const(Const {
            kind: ConstKind::Scalar(v),
            ..
        }) => Some(*v),
        _ => None,
    }
}

fn whole(o: &Operand) -> Option<Local> {
    match o {
        Operand::Copy(p) if p.projection.is_empty() => Some(p.local),
        _ => None,
    }
}

/// `switchInt(copy l.1) -> [0: ok, otherwise: abort]`: `(l, ok, abort)`.
fn on_flag(t: &TerminatorKind) -> Option<(Local, BasicBlock, BasicBlock)> {
    let TerminatorKind::SwitchInt {
        discr: Operand::Copy(p),
        targets,
    } = t
    else {
        return None;
    };
    let [(0, ok)] = targets.values.as_slice() else {
        return None;
    };
    matches!(p.projection.as_slice(), [ProjElem::Field(FieldIdx(1), _)]).then_some((
        p.local,
        *ok,
        targets.otherwise,
    ))
}

fn assign(s: &Statement) -> Option<(&Place, &Rvalue)> {
    match &s.kind {
        StatementKind::Assign(p, rv) if p.projection.is_empty() => Some((p, rv)),
        _ => None,
    }
}

/// `copy l.0`.
fn first_of(rv: &Rvalue, l: Local) -> bool {
    matches!(rv, Rvalue::Use(Operand::Copy(p))
        if p.local == l && matches!(p.projection.as_slice(), [ProjElem::Field(FieldIdx(0), _)]))
}

fn is_abort(body: &Body, b: BasicBlock) -> bool {
    matches!(
        body.blocks[b.index()].terminator.kind,
        TerminatorKind::Abort { .. }
    )
}

fn matched(body: &Body, tys: &TyInterner, h: usize) -> Option<Match> {
    let hb = &body.blocks[h];
    // H: `f = Ne(copy x, const 0)`, leaving on false.
    let [fs] = hb.statements.as_slice() else {
        return None;
    };
    let (fp, Rvalue::BinaryOp(BinOp::Ne, xo, zero)) = assign(fs)? else {
        return None;
    };
    let (f, x) = (fp.local, whole(xo)?);
    if int_const(zero)? != 0 {
        return None;
    }
    let TerminatorKind::SwitchInt {
        discr: Operand::Copy(fd),
        targets,
    } = &hb.terminator.kind
    else {
        return None;
    };
    let [(0, exit)] = targets.values.as_slice() else {
        return None;
    };
    if *fd != Place::local(f) {
        return None;
    }
    let b = targets.otherwise.index();
    // B: `t = CheckedSub(copy x, const 1)`.
    let bb = &body.blocks[b];
    let [ts] = bb.statements.as_slice() else {
        return None;
    };
    let (tp, Rvalue::CheckedBinaryOp(BinOp::Sub, xo, one)) = assign(ts)? else {
        return None;
    };
    let t = tp.local;
    if whole(xo)? != x || int_const(one)? != 1 {
        return None;
    }
    let (tf, cb, abort_sub) = on_flag(&bb.terminator.kind)?;
    if tf != t || !is_abort(body, abort_sub) {
        return None;
    }
    let c_ = cb.index();
    // C: `a = copy t.0; b = BitAnd(..); x = copy b; u = CheckedAdd(copy c, const 1)`.
    let cbd = &body.blocks[c_];
    let [s0, s1, s2, s3] = cbd.statements.as_slice() else {
        return None;
    };
    let (ap, arv) = assign(s0)?;
    let a = ap.local;
    if !first_of(arv, t) {
        return None;
    }
    let (bp, Rvalue::BinaryOp(BinOp::BitAnd, l, r)) = assign(s1)? else {
        return None;
    };
    let bl = bp.local;
    let (l, r) = (whole(l)?, whole(r)?);
    if !((l == x && r == a) || (l == a && r == x)) {
        return None;
    }
    let (xp, Rvalue::Use(bo)) = assign(s2)? else {
        return None;
    };
    if xp.local != x || whole(bo)? != bl {
        return None;
    }
    let (up, Rvalue::CheckedBinaryOp(BinOp::Add, co, one)) = assign(s3)? else {
        return None;
    };
    let (u, c) = (up.local, whole(co)?);
    if int_const(one)? != 1 || c == x {
        return None;
    }
    let (uf, db, abort_add) = on_flag(&cbd.terminator.kind)?;
    if uf != u || !is_abort(body, abort_add) {
        return None;
    }
    let d = db.index();
    // D: `v = copy u.0; c = copy v`, unit assignments, back to H.
    let dbd = &body.blocks[d];
    if !matches!(dbd.terminator.kind, TerminatorKind::Goto { target } if target.index() == h) {
        return None;
    }
    let [v0, v1, rest @ ..] = dbd.statements.as_slice() else {
        return None;
    };
    let (vp, vrv) = assign(v0)?;
    let v = vp.local;
    if !first_of(vrv, u) {
        return None;
    }
    let (cp, Rvalue::Use(vo)) = assign(v1)? else {
        return None;
    };
    if cp.local != c || whole(vo)? != v {
        return None;
    }
    let unit = |s: &Statement| {
        assign(s).is_some_and(|(p, rv)| {
            matches!(tys.kind(body.local(p.local).ty), TyKind::Unit)
                && matches!(rv, Rvalue::Use(Operand::Const(_)))
        })
    };
    if !rest.iter().all(unit) {
        return None;
    }
    // Integers, `c` of any integer type.
    let (TyKind::Int(_), TyKind::Int(_)) = (tys.kind(body.local(x).ty), tys.kind(body.local(c).ty))
    else {
        return None;
    };
    let blocks = [h, b, c_, d];
    if blocks
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != 4
    {
        return None;
    }
    // Reached only as the shape says: B from H, C from B, D from C.
    for (i, bd) in body.blocks.iter().enumerate() {
        for s in bd.terminator.kind.successors() {
            let s = s.index();
            let from = blocks.iter().position(|&k| k == i);
            let ok = match blocks.iter().position(|&k| k == s) {
                None | Some(0) => true,
                Some(k) => from == Some(k - 1),
            };
            if !ok {
                return None;
            }
        }
    }
    // The temporaries live only here.
    for (i, bd) in body.blocks.iter().enumerate() {
        if blocks.contains(&i) {
            continue;
        }
        for l in [f, t, a, bl, u, v] {
            if bd.statements.iter().any(|s| statement_mentions(&s.kind, l))
                || terminator_mentions(&bd.terminator.kind, l)
            {
                return None;
            }
        }
    }
    if [f, t, a, bl, u, v]
        .iter()
        .any(|l| l.index() <= body.arg_count)
    {
        return None;
    }
    Some(Match {
        blocks,
        exit: *exit,
        abort_sub,
        abort_add,
        x,
        c,
        f,
    })
}

fn rewrite(body: &mut Body, tys: &TyInterner, m: Match) {
    let [h, b, c_, d] = m.blocks;
    let xt = body.local(m.x).ty;
    let TyKind::Int(it) = tys.kind(xt) else {
        return;
    };
    let ct = body.local(m.c).ty;
    let bool_ty = body.local(m.f).ty;
    // The `u.1` switch and the `u.0` read, to copy onto a fresh pair.
    let flag_term = body.blocks[c_].terminator.clone();
    let add = body.blocks[c_].statements[3].clone();
    let sum = body.blocks[d].statements[0].clone();
    let info = add.source_info;
    let StatementKind::Assign(pair, _) = &add.kind else {
        return;
    };
    let pair_ty = body.local(pair.local).ty;
    let mut fresh = |ty| {
        let l = Local(body.locals.len() as u32);
        body.locals.push(LocalDecl {
            ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Temp,
            source_info: info,
        });
        l
    };
    let g = fresh(bool_ty);
    let n = fresh(ct);
    let w = fresh(pair_ty);
    let copy = |l: Local| Operand::Copy(Place::local(l));
    let stmt = |kind| Statement {
        kind,
        source_info: info,
    };
    let next = |body: &Body, k: u32| BasicBlock(body.blocks.len() as u32 + k);
    let signed = it.signed();
    // P (signed only): a negative `x` keeps the loop, whose last trip
    // overflows `x - 1` at `MIN`; the count is never reached.
    let p = next(body, 0);
    let f = next(body, u32::from(signed));
    let (f2, f3) = (BasicBlock(f.0 + 1), BasicBlock(f.0 + 2));
    if signed {
        let zero = Operand::Const(Const {
            ty: xt,
            kind: ConstKind::Scalar(0),
        });
        body.blocks.push(BasicBlockData {
            statements: vec![stmt(StatementKind::Assign(
                Place::local(g),
                Rvalue::BinaryOp(BinOp::Lt, copy(m.x), zero),
            ))],
            terminator: Terminator {
                kind: TerminatorKind::SwitchInt {
                    discr: copy(g),
                    targets: SwitchTargets {
                        values: vec![(0, f)],
                        otherwise: BasicBlock(b as u32),
                    },
                },
                source_info: info,
            },
        });
    }
    // F: `n = x.count_ones()`.
    let func = Operand::Const(Const {
        ty: tys.intern(TyKind::Unit),
        kind: ConstKind::FnDef(InstanceId {
            def: DefId(u32::MAX),
            args: Vec::new(),
            name: format!("{}.count_ones", tys.display(xt)),
        }),
    });
    body.blocks.push(BasicBlockData {
        statements: Vec::new(),
        terminator: Terminator {
            kind: TerminatorKind::Call {
                func,
                args: vec![copy(m.x)],
                destination: Place::local(n),
                target: Some(f2),
                unwind: UnwindAction::Abort,
            },
            source_info: info,
        },
    });
    // F2: `w = CheckedAdd(copy c, copy n)`, aborting as `c + 1` does.
    let mut add = add;
    if let StatementKind::Assign(p, Rvalue::CheckedBinaryOp(_, _, k)) = &mut add.kind {
        p.local = w;
        *k = copy(n);
    }
    let mut flag_term = flag_term;
    if let TerminatorKind::SwitchInt {
        discr: Operand::Copy(q),
        targets,
    } = &mut flag_term.kind
    {
        q.local = w;
        targets.values = vec![(0, f3)];
        targets.otherwise = m.abort_add;
    }
    body.blocks.push(BasicBlockData {
        statements: vec![add],
        terminator: flag_term,
    });
    // F3: `c = copy w.0; x = 0`, out of the loop.
    let mut sum = sum;
    if let StatementKind::Assign(dst, Rvalue::Use(Operand::Copy(q))) = &mut sum.kind {
        dst.local = m.c;
        q.local = w;
    }
    body.blocks.push(BasicBlockData {
        statements: vec![
            sum,
            stmt(StatementKind::Assign(
                Place::local(m.x),
                Rvalue::Use(Operand::Const(Const {
                    ty: xt,
                    kind: ConstKind::Scalar(0),
                })),
            )),
        ],
        terminator: Terminator {
            kind: TerminatorKind::Goto { target: m.exit },
            source_info: info,
        },
    });
    // H enters the new path instead of the loop body.
    if let TerminatorKind::SwitchInt { targets, .. } = &mut body.blocks[h].terminator.kind {
        targets.otherwise = if signed { p } else { f };
    }
    let _ = (m.abort_sub, d);
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;
    use crate::mir::lower::tests::run_source;
    use crate::mir::TerminatorKind;

    /// Whether `f`'s body calls `count_ones`, which only the rewrite adds.
    fn counts(src: &str, f: &str) -> bool {
        let l = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == f)
            .unwrap_or_else(|| panic!("no body {f}"));
        body.blocks.iter().any(|b| {
            matches!(&b.terminator.kind, TerminatorKind::Call { func, .. }
                if format!("{func:?}").contains("count_ones"))
        })
    }

    const HW: &str = "fn hw(n: i64) -> i64 {\n    let mut count = 0;\n    let mut x = n;\n    while x != 0 {\n        x = x & (x - 1);\n        count = count + 1;\n    }\n    return count;\n}\n";

    #[test]
    fn mir_popcount_counts_kernighan_loops() {
        let src = format!(
            "{HW}fn hwu(n: u32) -> u8 {{\n    let mut c: u8 = 0;\n    let mut x = n;\n    while x != 0 {{\n        x = x & (x - 1);\n        c = c + 1;\n    }}\n    c\n}}\nfn main() {{\n    println(hw(11));\n    println(hw(0));\n    println(hw(9223372036854775807));\n    println(hwu(4294967295));\n}}\n"
        );
        assert!(counts(&src, "hw"));
        assert!(counts(&src, "hwu"));
        assert_eq!(
            run_source(&src).unwrap(),
            ("3\n0\n63\n32\n".into(), Some(0))
        );
    }

    /// A negative `x` aborts at `MIN - 1`, and a counter that overflows
    /// aborts at its add, as the loop does.
    #[test]
    fn mir_popcount_keeps_the_aborts() {
        let neg = format!("{HW}fn main() {{\n    println(hw(5));\n    println(hw(0 - 1));\n}}\n");
        let (out, code) = run_source(&neg).unwrap();
        assert_eq!((out.as_str(), code), ("2\n", Some(101)));
        let wide = "fn hw(n: i64) -> i8 {\n    let mut count: i8 = 120;\n    let mut x = n;\n    while x != 0 {\n        x = x & (x - 1);\n        count = count + 1;\n    }\n    return count;\n}\nfn main() {\n    println(hw(127));\n    println(hw(255));\n}\n";
        assert!(counts(wide, "hw"));
        let (out, code) = run_source(wide).unwrap();
        assert_eq!((out.as_str(), code), ("127\n", Some(101)));
    }

    /// Off the shape: the body does more than the two statements.
    #[test]
    fn mir_popcount_leaves_other_loops() {
        let src = "fn f(n: i64) -> i64 {\n    let mut count = 0;\n    let mut x = n;\n    while x != 0 {\n        x = x & (x - 1);\n        count = count + 2;\n    }\n    return count;\n}\nfn main() {\n    println(f(7));\n}\n";
        assert!(!counts(src, "f"));
        assert_eq!(run_source(src).unwrap(), ("6\n".into(), Some(0)));
    }
}
