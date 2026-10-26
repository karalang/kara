//! Count nothing for a frozen handle (core semantics §6, design.md §
//! Frozen handles).
//!
//! The ownership check's frozen walk proves two kinds of `let` count-free
//! and hands them over by initializer span ([`super::lower::FrozenHints`]),
//! which lowering turns into each body's [`FrozenLocals`]:
//!
//! - an *alias* of a frozen place (`let k = n.kids[i];`,
//!   `let g = freeze root;`), which cannot outlive the value it reads;
//! - a *container*, a `Vec` local whose every element is such an alias
//!   (`let mut q: Vec[Node] = Vec.new(); q.push(g);`), touched only by
//!   `push`, `len`, `is_empty` and indexing.
//!
//! Counting either one is a non-atomic write to a header other tasks
//! read the same graph through, so §6 says they are never counted, which
//! is not the same as counted atomically. This pass makes the MIR say so:
//!
//! - an alias `x = retain(p)` becomes `x = &p`, `x` a `ref` to the
//!   handle, every place through `x` reads through the borrow and the
//!   drops of `x` are `goto`s (as `rc_borrow` does for a handle out of a
//!   borrowed parameter); a use of `x` whole takes a count of its own,
//!   `t = retain((*x))`, which a container push then gives up as below;
//! - a container's pushes of `t = retain(q)` push `&q` through
//!   `Vec[T].__push_frozen`, a copy of the handle that takes no count,
//!   and each drop of the container first runs `Vec[T].__forget_items`,
//!   which empties it without releasing what it never counted, so the
//!   drop frees only the buffer.
//!
//! The MIR is checked again before either rewrite: a local whose shape
//! is not the one above keeps its counts, which is correct, only not
//! count-free.

use super::rc_borrow::{deref_through, statement_places, terminator_places};
use super::syntax::*;
use super::ty::{IntrinsicTy, Ty, TyInterner, TyKind};
use crate::ids::DefId;
use crate::mir::interp::split_method;

/// One body's frozen `let`s, as lowering bound them.
#[derive(Default, Debug, Clone)]
pub struct FrozenLocals {
    pub aliases: Vec<Local>,
    pub containers: Vec<Local>,
}

/// Rewrites `body`'s frozen aliases to borrows and its frozen containers'
/// pushes and drops to count nothing.
pub fn uncount(body: &mut Body, tys: &TyInterner, f: &FrozenLocals) {
    for &x in &f.aliases {
        if let Some((bi, si)) = alias_def(body, tys, x) {
            borrow_alias(body, tys, x, bi, si);
        }
    }
    for &c in &f.containers {
        if let Some(plan) = container_plan(body, tys, c) {
            apply(body, tys, c, plan);
        }
    }
}

fn is_shared(tys: &TyInterner, t: Ty) -> bool {
    matches!(tys.kind(t), TyKind::Shared(_))
}

fn fn_name(func: &Operand) -> Option<&str> {
    match func {
        Operand::Const(Const {
            kind: ConstKind::FnDef(i),
            ..
        }) => Some(i.name.as_str()),
        _ => None,
    }
}

/// The one `x = retain(p)` that defines the alias `x`, when nothing else
/// writes `x` or borrows it `mut`.
fn alias_def(body: &Body, tys: &TyInterner, x: Local) -> Option<(usize, usize)> {
    if x.index() <= body.arg_count || !is_shared(tys, body.local(x).ty) {
        return None;
    }
    let mut def = None;
    for (bi, data) in body.blocks.iter().enumerate() {
        for (si, st) in data.statements.iter().enumerate() {
            match &st.kind {
                StatementKind::Assign(d, rv) => {
                    if d.local == x {
                        let Rvalue::Retain(p) = rv else {
                            return None;
                        };
                        if !d.projection.is_empty() || p.local == x || def.is_some() {
                            return None;
                        }
                        def = Some((bi, si));
                    }
                    if matches!(rv, Rvalue::Ref(BorrowKind::Mut, p) if p.local == x) {
                        return None;
                    }
                }
                StatementKind::SetDiscriminant(p, _) | StatementKind::FreeBox(p)
                    if p.local == x =>
                {
                    return None
                }
                _ => {}
            }
        }
        if let TerminatorKind::Call { destination, .. } = &data.terminator.kind {
            if destination.local == x {
                return None;
            }
        }
    }
    def
}

fn fresh(body: &mut Body, ty: Ty, info: SourceInfo) -> Local {
    let l = Local(body.locals.len() as u32);
    body.locals.push(LocalDecl {
        ty,
        mutability: Mutability::Mut,
        kind: LocalKind::Temp,
        source_info: info,
    });
    l
}

fn rvalue_operands(rv: &mut Rvalue, f: &mut dyn FnMut(&mut Operand)) {
    match rv {
        Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => f(o),
        Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
            f(a);
            f(b);
        }
        Rvalue::Aggregate(_, os) => os.iter_mut().for_each(f),
        _ => {}
    }
}

fn terminator_operands(t: &mut TerminatorKind, f: &mut dyn FnMut(&mut Operand)) {
    match t {
        TerminatorKind::SwitchInt { discr, .. } => f(discr),
        TerminatorKind::Call { func, args, .. } => {
            f(func);
            args.iter_mut().for_each(f);
        }
        _ => {}
    }
}

/// `x = retain(p)` at `(bi, si)` becomes `x = &p`; see the module docs.
fn borrow_alias(body: &mut Body, tys: &TyInterner, x: Local, bi: usize, si: usize) {
    let old = body.local(x).ty;
    let StatementKind::Assign(_, rv) = &mut body.blocks[bi].statements[si].kind else {
        unreachable!("an alias's def is an assignment")
    };
    let Rvalue::Retain(p) = rv else {
        unreachable!("an alias's def is a retain")
    };
    *rv = Rvalue::Ref(BorrowKind::Shared, p.clone());
    body.locals[x.index()].ty = tys.intern(TyKind::Ref(old));
    let through = Place {
        local: x,
        projection: vec![ProjElem::Deref],
    };
    // The temporaries each whole use of `x` takes its count in.
    let mut temps = Vec::new();
    for b in 0..body.blocks.len() {
        let statements = std::mem::take(&mut body.blocks[b].statements);
        let mut out = Vec::with_capacity(statements.len());
        for (s, mut st) in statements.into_iter().enumerate() {
            if (b, s) == (bi, si) {
                out.push(st);
                continue;
            }
            let mut wholes = 0;
            if let StatementKind::Assign(_, rv) = &mut st.kind {
                rvalue_operands(rv, &mut |o| wholes += whole_use(o, x));
            }
            for _ in 0..wholes {
                let t = fresh(body, old, st.source_info);
                temps.push(t);
                out.push(Statement {
                    kind: StatementKind::Assign(Place::local(t), Rvalue::Retain(through.clone())),
                    source_info: st.source_info,
                });
            }
            let mut next = temps.len() - wholes;
            if let StatementKind::Assign(_, rv) = &mut st.kind {
                rvalue_operands(rv, &mut |o| {
                    if whole_use(o, x) == 1 {
                        *o = Operand::Move(Place::local(temps[next]));
                        next += 1;
                    }
                });
            }
            statement_places(&mut st.kind, &mut |q| deref_through(q, x));
            out.push(st);
        }
        body.blocks[b].statements = out;
        let info = body.blocks[b].terminator.source_info;
        let mut k = std::mem::replace(
            &mut body.blocks[b].terminator.kind,
            TerminatorKind::Unreachable,
        );
        if let TerminatorKind::Drop { place, target, .. } = &k {
            if *place == Place::local(x) {
                k = TerminatorKind::Goto { target: *target };
            }
        }
        let mut wholes = 0;
        terminator_operands(&mut k, &mut |o| wholes += whole_use(o, x));
        let first = temps.len();
        for _ in 0..wholes {
            let t = fresh(body, old, info);
            temps.push(t);
            body.blocks[b].statements.push(Statement {
                kind: StatementKind::Assign(Place::local(t), Rvalue::Retain(through.clone())),
                source_info: info,
            });
        }
        let mut next = first;
        terminator_operands(&mut k, &mut |o| {
            if whole_use(o, x) == 1 {
                *o = Operand::Move(Place::local(temps[next]));
                next += 1;
            }
        });
        terminator_places(&mut k, &mut |q| deref_through(q, x));
        body.blocks[b].terminator.kind = k;
    }
}

/// 1 when `o` is `x` whole, which after the rewrite needs a count.
fn whole_use(o: &Operand, x: Local) -> usize {
    match o {
        Operand::Copy(p) | Operand::Move(p) if p.local == x && p.projection.is_empty() => 1,
        _ => 0,
    }
}

/// What converting one container changes.
struct Plan {
    /// Each push: its block and the `t = retain(q)` it pushes.
    pushes: Vec<(usize, usize, usize)>,
    /// The blocks that end in a drop of the container.
    drops: Vec<usize>,
}

/// The pushes and drops of the container `c`, when every mention of it is
/// one this pass understands and every push is of a fresh count.
fn container_plan(body: &Body, tys: &TyInterner, c: Local) -> Option<Plan> {
    if c.index() <= body.arg_count
        || !matches!(
            tys.kind(body.local(c).ty),
            TyKind::Intrinsic(IntrinsicTy::Vec(_))
        )
    {
        return None;
    }
    // The `r = &mut c` receivers, each to be a push's and nothing else.
    let mut receivers = Vec::new();
    let mut drops = Vec::new();
    for (bi, data) in body.blocks.iter().enumerate() {
        for st in &data.statements {
            match &st.kind {
                StatementKind::Assign(d, Rvalue::Ref(k, p)) if p.local == c => {
                    if !p.projection.is_empty() || !d.projection.is_empty() || d.local == c {
                        return None;
                    }
                    if *k == BorrowKind::Mut {
                        receivers.push(d.local);
                    }
                }
                StatementKind::StorageLive(_) | StatementKind::StorageDead(_) => {}
                _ => {
                    let mut st = st.kind.clone();
                    let mut seen = false;
                    statement_places(&mut st, &mut |q| seen |= q.local == c);
                    if seen {
                        return None;
                    }
                }
            }
        }
        match &data.terminator.kind {
            TerminatorKind::Drop { place, .. } if place.local == c => {
                if !place.projection.is_empty() {
                    return None;
                }
                drops.push(bi);
            }
            TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } if destination.local == c => {
                let made = fn_name(func).is_some_and(|n| {
                    let (base, method) = split_method(n);
                    base.starts_with("Vec[") && matches!(method, "new" | "with_capacity")
                });
                if !made || !destination.projection.is_empty() || mentions(args, c) {
                    return None;
                }
            }
            k => {
                let mut k = k.clone();
                let mut seen = false;
                terminator_places(&mut k, &mut |q| seen |= q.local == c);
                if seen {
                    return None;
                }
            }
        }
    }
    let mut pushes = Vec::new();
    for r in receivers {
        if count_mentions(body, r) != 2 {
            return None;
        }
        let at = body
            .blocks
            .iter()
            .position(|data| match &data.terminator.kind {
                TerminatorKind::Call { args, .. } => {
                    matches!(args.first(), Some(Operand::Move(p)) if *p == Place::local(r))
                }
                _ => false,
            })?;
        let TerminatorKind::Call { func, args, .. } = &body.blocks[at].terminator.kind else {
            return None;
        };
        let push = fn_name(func).is_some_and(|n| {
            let (base, method) = split_method(n);
            base.starts_with("Vec[") && method == "push"
        });
        let [_, Operand::Move(t)] = args.as_slice() else {
            return None;
        };
        if !push || !t.projection.is_empty() || count_mentions(body, t.local) != 2 {
            return None;
        }
        let def = body.blocks.iter().enumerate().find_map(|(bi, data)| {
            data.statements.iter().position(|st| {
                matches!(&st.kind, StatementKind::Assign(d, Rvalue::Retain(_)) if *d == *t)
            }).map(|si| (bi, si))
        })?;
        pushes.push((at, def.0, def.1));
    }
    (!pushes.is_empty()).then_some(Plan { pushes, drops })
}

fn mentions(args: &[Operand], c: Local) -> bool {
    args.iter()
        .any(|o| matches!(o, Operand::Copy(p) | Operand::Move(p) if p.local == c))
}

/// How many places in `body` are rooted at `l`.
fn count_mentions(body: &Body, l: Local) -> usize {
    let mut n = 0;
    for data in &body.blocks {
        for st in &data.statements {
            if matches!(
                st.kind,
                StatementKind::StorageLive(_) | StatementKind::StorageDead(_)
            ) {
                continue;
            }
            let mut st = st.kind.clone();
            statement_places(&mut st, &mut |q| {
                n += usize::from(q.local == l)
                    + q.projection
                        .iter()
                        .filter(|e| matches!(e, ProjElem::Index(i) if *i == l))
                        .count();
            });
        }
        let mut k = data.terminator.kind.clone();
        terminator_places(&mut k, &mut |q| n += usize::from(q.local == l));
    }
    n
}

/// A native of the container's type, `Vec[T].{method}`.
fn native(tys: &TyInterner, vty: Ty, method: &str) -> Operand {
    Operand::Const(Const {
        ty: tys.intern(TyKind::Unit),
        kind: ConstKind::FnDef(InstanceId {
            def: DefId(u32::MAX),
            args: Vec::new(),
            name: format!("{}.{method}", tys.display(vty)),
        }),
    })
}

fn apply(body: &mut Body, tys: &TyInterner, c: Local, plan: Plan) {
    let vty = body.local(c).ty;
    for (at, bi, si) in plan.pushes {
        let StatementKind::Assign(d, rv) = &mut body.blocks[bi].statements[si].kind else {
            unreachable!("a push's element is assigned")
        };
        let Rvalue::Retain(q) = rv else {
            unreachable!("a push's element is a retain")
        };
        *rv = Rvalue::Ref(BorrowKind::Shared, q.clone());
        let t = d.local;
        let old = body.local(t).ty;
        body.locals[t.index()].ty = tys.intern(TyKind::Ref(old));
        if let TerminatorKind::Call { func, .. } = &mut body.blocks[at].terminator.kind {
            *func = native(tys, vty, "__push_frozen");
        }
    }
    for b in plan.drops {
        let drop = body.blocks[b].terminator.clone();
        let info = drop.source_info;
        let x = BasicBlock(body.blocks.len() as u32);
        body.blocks.push(BasicBlockData {
            statements: Vec::new(),
            terminator: drop,
        });
        let r = fresh(body, tys.intern(TyKind::MutRef(vty)), info);
        let unit = fresh(body, tys.intern(TyKind::Unit), info);
        body.blocks[b].statements.push(Statement {
            kind: StatementKind::Assign(
                Place::local(r),
                Rvalue::Ref(BorrowKind::Mut, Place::local(c)),
            ),
            source_info: info,
        });
        body.blocks[b].terminator = Terminator {
            kind: TerminatorKind::Call {
                func: native(tys, vty, "__forget_items"),
                args: vec![Operand::Move(Place::local(r))],
                destination: Place::local(unit),
                target: Some(x),
                unwind: UnwindAction::Abort,
            },
            source_info: info,
        };
    }
}
