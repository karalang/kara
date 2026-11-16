//! An error's `?` frames end with the error value (design.md § Error
//! return traces, B-2026-11-16-223).
//!
//! The trace is one buffer per thread, so a value cannot carry its frames;
//! instead the buffer is cleared where an `Err` it could belong to goes
//! away unhandled. Lowering marks the end of each `Result` temporary and
//! binding in traced code with `error_trace_dropped(n)`, `n` the local's
//! index as a constant, so the marker uses nothing the move and borrow
//! checks could object to. This pass, run after those checks and before
//! drop elaboration, rewrites a marker into "if the local is `Err`, clear
//! the trace" where the local still owns the value it was given on every
//! path, and removes it everywhere else: a local whose value moved or was
//! copied anywhere (a `?` that took its payload, a `return r`, a call
//! argument) or was mutably borrowed may have handed the error on, and its
//! frames with it.
//!
//! Lowering emits no marker on an exit that carries an error out (a `?`,
//! or a `return` whose value is `Err`), so a held `Err` dropped on the way
//! out never clears the frames of the error being returned.

use super::syntax::*;
use super::ty::TyInterner;

/// The native lowering emits at the end of a `Result` local's scope.
pub const MARKER: &str = "error_trace_dropped";

/// Resolves every marker in `body`.
pub fn resolve(body: &mut Body, tys: &TyInterner) {
    let markers: Vec<(usize, Local)> = body
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(bi, b)| marker_local(&b.terminator.kind).map(|l| (bi, l)))
        .collect();
    if markers.is_empty() {
        return;
    }
    let mut tracked: Vec<Local> = markers.iter().map(|m| m.1).collect();
    tracked.sort();
    tracked.dedup();
    let slot = |l: Local| tracked.binary_search(&l).ok();
    let owned = must_own(body, &tracked, &slot);
    for (bi, l) in markers {
        let TerminatorKind::Call {
            func,
            args,
            destination,
            target: Some(target),
            unwind,
        } = body.blocks[bi].terminator.kind.clone()
        else {
            continue;
        };
        let err = err_variant(body, tys, l);
        let own = owned[bi].as_ref().is_some_and(|st| {
            let mut st = st.clone();
            for s in &body.blocks[bi].statements {
                statement(&slot, &mut st, &s.kind);
            }
            slot(l).is_some_and(|i| st[i])
        });
        let (Some(err), true, [Operand::Const(c)]) = (err, own, args.as_slice()) else {
            body.blocks[bi].terminator.kind = TerminatorKind::Goto { target };
            continue;
        };
        // `d = discriminant(l); if d == Err { error_trace_clear() }`.
        let d = Local(body.locals.len() as u32);
        body.locals.push(LocalDecl {
            ty: c.ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Temp,
            source_info: body.blocks[bi].terminator.source_info,
        });
        let info = body.blocks[bi].terminator.source_info;
        let clear_func = match func {
            Operand::Const(Const {
                ty,
                kind: ConstKind::FnDef(id),
            }) => Operand::Const(Const {
                ty,
                kind: ConstKind::FnDef(InstanceId {
                    name: "error_trace_clear".to_string(),
                    ..id
                }),
            }),
            other => other,
        };
        let clear = BasicBlock(body.blocks.len() as u32);
        body.blocks.push(BasicBlockData {
            statements: Vec::new(),
            terminator: Terminator {
                source_info: info,
                kind: TerminatorKind::Call {
                    func: clear_func,
                    args: Vec::new(),
                    destination,
                    target: Some(target),
                    unwind,
                },
            },
        });
        let block = &mut body.blocks[bi];
        block.statements.push(Statement {
            source_info: info,
            kind: StatementKind::Assign(Place::local(d), Rvalue::Discriminant(Place::local(l))),
        });
        block.terminator.kind = TerminatorKind::SwitchInt {
            discr: Operand::Copy(Place::local(d)),
            targets: SwitchTargets {
                values: vec![(err as u128, clear)],
                otherwise: target,
            },
        };
    }
}

/// The local a marker names.
fn marker_local(t: &TerminatorKind) -> Option<Local> {
    let TerminatorKind::Call {
        func:
            Operand::Const(Const {
                kind: ConstKind::FnDef(id),
                ..
            }),
        args,
        ..
    } = t
    else {
        return None;
    };
    match (id.name.as_str(), args.as_slice()) {
        (
            MARKER,
            [Operand::Const(Const {
                kind: ConstKind::Scalar(n),
                ..
            })],
        ) => Some(Local(*n as u32)),
        _ => None,
    }
}

/// The index of `Err` in the type of local `l`, when it is a `Result`.
fn err_variant(body: &Body, tys: &TyInterner, l: Local) -> Option<u32> {
    let t = body.locals.get(l.index())?.ty;
    let (adt, _) = tys.tcx().adt_of(t)?;
    if adt.name != "Result" {
        return None;
    }
    adt.variants
        .iter()
        .position(|v| v.name == "Err")
        .map(|i| i as u32)
}

/// At the entry of each block the entry reaches, which tracked locals hold,
/// on every path, the value last assigned to them, not moved, copied or
/// mutably borrowed since.
fn must_own(
    body: &Body,
    tracked: &[Local],
    slot: &dyn Fn(Local) -> Option<usize>,
) -> Vec<Option<Vec<bool>>> {
    let n = body.blocks.len();
    let mut at: Vec<Option<Vec<bool>>> = vec![None; n];
    if n == 0 {
        return at;
    }
    at[0] = Some(vec![false; tracked.len()]);
    let order = super::counters::reverse_postorder(body);
    let mut changed = true;
    while changed {
        changed = false;
        for &bi in &order {
            let Some(mut st) = at[bi].clone() else {
                continue;
            };
            let b = &body.blocks[bi];
            for s in &b.statements {
                statement(slot, &mut st, &s.kind);
            }
            terminator(slot, &mut st, &b.terminator.kind);
            for s in b.terminator.kind.successors() {
                let si = s.index();
                let next = match &at[si] {
                    None => st.clone(),
                    Some(old) => old.iter().zip(&st).map(|(a, b)| *a && *b).collect(),
                };
                if at[si].as_ref() != Some(&next) {
                    at[si] = Some(next);
                    changed = true;
                }
            }
        }
    }
    at
}

fn kill_operand(slot: &dyn Fn(Local) -> Option<usize>, st: &mut [bool], o: &Operand) {
    if let Operand::Copy(p) | Operand::Move(p) = o {
        if let Some(i) = slot(p.local) {
            st[i] = false;
        }
    }
}

fn statement(slot: &dyn Fn(Local) -> Option<usize>, st: &mut [bool], s: &StatementKind) {
    match s {
        StatementKind::Assign(dest, rv) => {
            match rv {
                Rvalue::Ref(BorrowKind::Shared, _)
                | Rvalue::Discriminant(_)
                | Rvalue::Len(_)
                | Rvalue::NullaryOp(..) => {}
                Rvalue::Ref(_, p) | Rvalue::Retain(p) => {
                    if let Some(i) = slot(p.local) {
                        st[i] = false;
                    }
                }
                _ => {
                    for o in super::elaborate::rvalue_operands(rv) {
                        kill_operand(slot, st, o);
                    }
                }
            }
            if let Some(i) = slot(dest.local) {
                // A whole new value is owned; a write into part of one
                // leaves what the local owned as it was.
                if dest.projection.is_empty() {
                    st[i] = true;
                }
            }
        }
        StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => {
            if let Some(i) = slot(*l) {
                st[i] = false;
            }
        }
        _ => {}
    }
}

fn terminator(slot: &dyn Fn(Local) -> Option<usize>, st: &mut [bool], t: &TerminatorKind) {
    match t {
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => {
            kill_operand(slot, st, func);
            for a in args {
                kill_operand(slot, st, a);
            }
            if let Some(i) = slot(destination.local) {
                st[i] = destination.projection.is_empty();
            }
        }
        TerminatorKind::SwitchInt { discr, .. } => kill_operand(slot, st, discr),
        TerminatorKind::Drop { place, .. } => {
            if let Some(i) = slot(place.local) {
                st[i] = false;
            }
        }
        _ => {}
    }
}
