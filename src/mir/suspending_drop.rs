//! A `Drop` body may suspend (design.md § Drop, B-2026-11-17-35): it may
//! sleep, wait on a channel or take a lock. Such a body is a coroutine, so
//! the drop glue that calls it has to be a suspending call too: the
//! backends' glue is a plain function that cannot wait.
//!
//! Before the coroutine transform, each `Drop` of a value whose drop
//! reaches a suspending `Drop` body becomes a call of that type's drop
//! glue written as a MIR body, `T#sdrop(self: mut ref T)`, which drops the
//! value in place the way the backends' glue does: the type's own `Drop`
//! body first, then its fields last to first (an enum's active variant's),
//! a tuple's or a closure's parts last to first, an array's or a `Vec`'s
//! elements in index order (core semantics §4). Each part whose drop
//! reaches a suspending body is dropped through its own glue body, and
//! every other part by a plain `Drop`. The glue bodies call the suspending
//! `Drop` body, so the transform makes them coroutines, and with them
//! every body that drops such a value: the drop suspends its caller.
//!
//! A drop that reaches a suspending body only through a part MIR cannot
//! reach (a `shared` value's last handle, a map, a `dyn` or function
//! value, a library cell) keeps the backends' glue, which reports that the
//! drop cannot wait there (B-2026-11-17-101).
//!
//! In a cancelled task's teardown the drop runs to completion: its waits
//! are not cancellation points. The teardown's own calls are not
//! cancelled already (`lower::cancel_exit`); a suspending `Drop` body
//! also loses its cancel exits here, so a drop that was waiting when its
//! task was cancelled finishes before the teardown goes on.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::coroutine;
use super::syntax::*;
use super::ty::{IntTy, IntrinsicTy, Ty, TyInterner, TyKind};

/// `__dropped(mut ref v)`, the last call of every glue body: `v`'s parts
/// are dropped, and what is left of it (its `Copy` parts) is no value
/// any more. The interpreter marks the place uninitialized, as its own
/// drop does; the compiled backends do nothing.
pub const DROPPED: &str = "__dropped";

/// The name of the glue body that drops a `ty` in place.
pub fn glue_name(tys: &TyInterner, ty: Ty) -> String {
    format!("{}#sdrop", tys.display(ty))
}

/// Whether `name` is a glue body this pass made.
pub fn is_glue(name: &str) -> bool {
    name.ends_with("#sdrop")
}

/// Rewrites `bodies` as the module docs say. `drop_body` names the `Drop`
/// body of a type, and `seed` is the set the coroutine transform starts
/// from. Nothing changes when no `Drop` body suspends.
pub fn lower(
    bodies: &mut Vec<Body>,
    tys: &TyInterner,
    seed: &BTreeSet<String>,
    drop_body: &dyn Fn(Ty) -> Option<String>,
) -> Result<(), String> {
    let set = coroutine::coroutines_from(bodies.iter(), seed);
    let by_name: BTreeMap<String, usize> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| (b.instance.name.clone(), i))
        .collect();
    let mut pass = Pass {
        tys,
        drop_body,
        suspending: BTreeMap::new(),
        memo: HashMap::new(),
        deciding: BTreeSet::new(),
        leaned: false,
        queue: Vec::new(),
        made: BTreeSet::new(),
        set: &set,
        by_name: &by_name,
        bodies,
    };
    // The suspending `Drop` bodies, found by the types the bodies drop.
    let mut dropped: Vec<Ty> = Vec::new();
    for b in pass.bodies.iter() {
        for d in &b.blocks {
            if let TerminatorKind::Drop { place, .. } = &d.terminator.kind {
                if let Ok(t) = super::place_ty::place_ty(b, tys, place) {
                    dropped.push(t.ty);
                }
            }
        }
    }
    if !dropped.iter().any(|&t| pass.reaches(t)) {
        return Ok(());
    }
    for i in 0..pass.bodies.len() {
        let placeholder = empty_like(&pass.bodies[i]);
        let mut b = std::mem::replace(&mut pass.bodies[i], placeholder);
        pass.rewrite_drops(&mut b)?;
        pass.bodies[i] = b;
    }
    let template = pass
        .suspending
        .values()
        .find_map(|(_, n)| pass.by_name.get(n).copied())
        .map(|i| pass.bodies[i].clone());
    let Some(template) = template else {
        return Ok(());
    };
    while let Some(ty) = pass.queue.pop() {
        let mut g = pass.glue(ty, &template)?;
        pass.rewrite_drops(&mut g)?;
        pass.bodies.push(g);
    }
    // A suspending `Drop` body runs to completion.
    let names: BTreeSet<String> = pass.suspending.values().map(|(_, n)| n.clone()).collect();
    for b in pass.bodies.iter_mut() {
        if names.contains(&b.instance.name) {
            remove_cancel_checks(b, tys);
        }
    }
    Ok(())
}

struct Pass<'a, 'b> {
    tys: &'a TyInterner,
    drop_body: &'a dyn Fn(Ty) -> Option<String>,
    /// Per type with a `Drop` body: its instance and name, when it suspends.
    suspending: BTreeMap<Ty, (InstanceId, String)>,
    memo: HashMap<Ty, bool>,
    /// The types [`Self::reaches`] is deciding, and whether an answer
    /// leaned on one of them.
    deciding: BTreeSet<Ty>,
    leaned: bool,
    queue: Vec<Ty>,
    made: BTreeSet<Ty>,
    set: &'a BTreeSet<String>,
    by_name: &'a BTreeMap<String, usize>,
    bodies: &'b mut Vec<Body>,
}

/// The library types whose parts MIR reaches as a user type's.
fn plain_library(name: &str) -> bool {
    matches!(name, "Option" | "Result")
}

impl Pass<'_, '_> {
    /// The suspending `Drop` body of `ty` itself, if it has one.
    fn own_suspending(&mut self, ty: Ty) -> Option<(InstanceId, String)> {
        if let Some(s) = self.suspending.get(&ty) {
            return Some(s.clone());
        }
        if !self.tys.has_drop_impl(ty) {
            return None;
        }
        let name = (self.drop_body)(ty)?;
        if !self.set.contains(&name) {
            return None;
        }
        let i = *self.by_name.get(&name)?;
        let inst = self.bodies[i].instance.clone();
        self.suspending.insert(ty, (inst.clone(), name.clone()));
        Some((inst, name))
    }

    /// The `Drop` body of `ty` itself, suspending or not.
    fn own_drop(&self, ty: Ty) -> Option<InstanceId> {
        if !self.tys.has_drop_impl(ty) {
            return None;
        }
        let name = (self.drop_body)(ty)?;
        let i = *self.by_name.get(&name)?;
        Some(self.bodies[i].instance.clone())
    }

    /// The parts of `ty` MIR drops one by one, as glue does.
    fn parts(&self, ty: Ty) -> Option<Vec<Ty>> {
        let tys = self.tys;
        match tys.kind(ty) {
            TyKind::Adt(a) => {
                let adt = tys.adt(a);
                if coroutine::is_frame(&adt.name)
                    || adt.library_name().is_some_and(|n| !plain_library(n))
                {
                    return None;
                }
                let mut out = Vec::new();
                for (v, var) in adt.variants.iter().enumerate() {
                    let v = adt.is_enum.then_some(v as u32);
                    for f in 0..var.fields.len() as u32 {
                        out.extend(tys.field_ty(ty, v, f));
                    }
                }
                Some(out)
            }
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => Some(ts),
            TyKind::Array(e, n) if n > 0 => Some(vec![e]),
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) => Some(vec![e]),
            _ => None,
        }
    }

    /// Whether dropping a `ty` reaches a suspending `Drop` body through
    /// parts MIR can drop one by one.
    fn reaches(&mut self, ty: Ty) -> bool {
        if let Some(&r) = self.memo.get(&ty) {
            return r;
        }
        // A type that holds itself (through a `Vec`) is not reached again
        // while it is being decided, and a `false` that leaned on such an
        // undecided type is not kept: `Vec[Tree]` inside `Tree` reaches
        // whatever `Tree` does.
        if !self.deciding.insert(ty) {
            self.leaned = true;
            return false;
        }
        let outer = std::mem::take(&mut self.leaned);
        let r = self.parts(ty).is_some_and(|parts| {
            (matches!(self.tys.kind(ty), TyKind::Adt(_)) && self.own_suspending(ty).is_some())
                || parts.into_iter().any(|p| self.reaches(p))
        });
        self.deciding.remove(&ty);
        if r || !self.leaned {
            self.memo.insert(ty, r);
        }
        self.leaned |= outer;
        r
    }

    /// The glue body of `ty`, queued to be made if it is not yet.
    fn glue_of(&mut self, ty: Ty) -> InstanceId {
        if self.made.insert(ty) {
            self.queue.push(ty);
        }
        InstanceId {
            def: crate::ids::DefId(0),
            args: vec![],
            name: glue_name(self.tys, ty),
        }
    }

    /// Each `Drop` in `b` of a value whose drop reaches a suspending body
    /// becomes `r = &mut place; _ = T#sdrop(move r) -> target`.
    fn rewrite_drops(&mut self, b: &mut Body) -> Result<(), String> {
        for i in 0..b.blocks.len() {
            let t = &b.blocks[i].terminator;
            let TerminatorKind::Drop {
                place,
                target,
                unwind,
            } = &t.kind
            else {
                continue;
            };
            // A glue body's own last drop of its emptied `Vec` is the
            // backends' glue.
            if is_glue(&b.instance.name)
                && place.local == Local(1)
                && place.projection == [ProjElem::Deref]
            {
                continue;
            }
            let pt = super::place_ty::place_ty(b, self.tys, place)?.ty;
            if !self.reaches(pt) {
                continue;
            }
            let (place, target, unwind, si) = (place.clone(), *target, *unwind, t.source_info);
            let glue = self.glue_of(pt);
            let r = push_local(b, self.tys.intern(TyKind::MutRef(pt)), si);
            let u = push_local(b, self.tys.unit(), si);
            b.blocks[i].statements.push(Statement {
                kind: StatementKind::Assign(Place::local(r), Rvalue::Ref(BorrowKind::Mut, place)),
                source_info: si,
            });
            b.blocks[i].terminator = Terminator {
                kind: TerminatorKind::Call {
                    func: fn_const(self.tys, glue),
                    args: vec![Operand::Move(Place::local(r))],
                    destination: Place::local(u),
                    target: Some(target),
                    unwind,
                },
                source_info: si,
            };
        }
        Ok(())
    }

    /// `T#sdrop(_1: mut ref T)`: drops `*_1` in place. Its parts' drops are
    /// plain `Drop`s, which [`Self::rewrite_drops`] then rewrites.
    fn glue(&mut self, ty: Ty, template: &Body) -> Result<Body, String> {
        let tys = self.tys;
        let si = SourceInfo {
            span: template.span,
            scope: SourceScope(0),
        };
        let mut g = Glue {
            tys,
            si,
            locals: vec![
                LocalDecl {
                    ty: tys.unit(),
                    mutability: Mutability::Mut,
                    kind: LocalKind::ReturnPlace,
                    source_info: si,
                },
                LocalDecl {
                    ty: tys.intern(TyKind::MutRef(ty)),
                    mutability: Mutability::Mut,
                    kind: LocalKind::Arg {
                        name: "self".into(),
                        node: crate::ids::NodeId(0),
                    },
                    source_info: si,
                },
            ],
            blocks: Vec::new(),
        };
        let this = Place::local(Local(1)).project(ProjElem::Deref);
        // The entry, pointed at the drop's first block once it is made.
        g.block(
            vec![],
            TerminatorKind::Goto {
                target: BasicBlock(0),
            },
        );
        let finish = g.block(
            vec![Statement {
                kind: StatementKind::Assign(
                    Place::local(Local::RETURN_PLACE),
                    Rvalue::Use(Operand::Const(Const {
                        ty: tys.unit(),
                        kind: ConstKind::Unit,
                    })),
                ),
                source_info: si,
            }],
            TerminatorKind::Return,
        );
        // The value reads as dropped once its parts are: [`DROPPED`].
        let left = g.local(tys.intern(TyKind::MutRef(ty)));
        let u = g.local(tys.unit());
        let ret = g.block(
            vec![Statement {
                kind: StatementKind::Assign(
                    Place::local(left),
                    Rvalue::Ref(BorrowKind::Mut, this.clone()),
                ),
                source_info: si,
            }],
            TerminatorKind::Call {
                func: fn_const(
                    tys,
                    InstanceId {
                        def: crate::ids::DefId(u32::MAX),
                        args: Vec::new(),
                        name: DROPPED.to_string(),
                    },
                ),
                args: vec![Operand::Move(Place::local(left))],
                destination: Place::local(u),
                target: Some(finish),
                unwind: UnwindAction::Abort,
            },
        );
        let body = match tys.kind(ty) {
            TyKind::Adt(a) => {
                let adt = tys.adt(a);
                let rest = if adt.is_enum {
                    let disc = g.local(tys.int(IntTy::I64));
                    let mut arms = Vec::new();
                    for (v, var) in adt.variants.iter().enumerate() {
                        let parts: Vec<(Place, Ty)> = (0..var.fields.len() as u32)
                            .rev()
                            .filter_map(|f| {
                                let ft = tys.field_ty(ty, Some(v as u32), f)?;
                                Some((
                                    this.project(ProjElem::Downcast(VariantIdx(v as u32)))
                                        .field(f, ft),
                                    ft,
                                ))
                            })
                            .filter(|(_, ft)| tys.needs_drop(*ft))
                            .collect();
                        if !parts.is_empty() {
                            arms.push((v as u128, g.drops(parts, ret)));
                        }
                    }
                    g.block(
                        vec![Statement {
                            kind: StatementKind::Assign(
                                Place::local(disc),
                                Rvalue::Discriminant(this.clone()),
                            ),
                            source_info: si,
                        }],
                        TerminatorKind::SwitchInt {
                            discr: Operand::Copy(Place::local(disc)),
                            targets: SwitchTargets {
                                values: arms,
                                otherwise: ret,
                            },
                        },
                    )
                } else {
                    let n = adt.variants.first().map_or(0, |v| v.fields.len()) as u32;
                    let parts: Vec<(Place, Ty)> = (0..n)
                        .rev()
                        .filter_map(|f| {
                            let ft = tys.field_ty(ty, None, f)?;
                            Some((this.field(f, ft), ft))
                        })
                        .filter(|(_, ft)| tys.needs_drop(*ft))
                        .collect();
                    g.drops(parts, ret)
                };
                match self.own_drop(ty) {
                    Some(inst) => g.call_on(inst, this.clone(), ty, rest),
                    None => rest,
                }
            }
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => {
                let parts: Vec<(Place, Ty)> = (0..ts.len())
                    .rev()
                    .filter(|&i| tys.needs_drop(ts[i]))
                    .map(|i| (this.field(i as u32, ts[i]), ts[i]))
                    .collect();
                g.drops(parts, ret)
            }
            TyKind::Array(e, n) => {
                let parts: Vec<(Place, Ty)> = (0..n)
                    .map(|i| (this.project(ProjElem::ConstIndex(i)), e))
                    .collect();
                g.drops(parts, ret)
            }
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) => g.vec_elems(this.clone(), ty, e, ret),
            _ => {
                return Err(format!(
                    "no drop glue in MIR for `{}` (B-2026-11-17-35)",
                    tys.display(ty)
                ))
            }
        };
        let mut blocks = g.blocks;
        blocks[0].terminator.kind = TerminatorKind::Goto { target: body };
        Ok(Body {
            instance: InstanceId {
                def: crate::ids::DefId(0),
                args: vec![],
                name: glue_name(tys, ty),
            },
            locals: g.locals,
            arg_count: 1,
            blocks,
            scopes: template.scopes.clone(),
            phase: template.phase,
            span: template.span,
            auto_par_candidates: Vec::new(),
            par_regions: vec![],
            provided: Vec::new(),
            suspends: false,
            drop_flags: vec![],
            fn_value_effects: Vec::new(),
            clause_names_var: false,
        })
    }
}

struct Glue<'a> {
    tys: &'a TyInterner,
    si: SourceInfo,
    locals: Vec<LocalDecl>,
    blocks: Vec<BasicBlockData>,
}

impl Glue<'_> {
    fn local(&mut self, ty: Ty) -> Local {
        self.locals.push(LocalDecl {
            ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Temp,
            source_info: self.si,
        });
        Local(self.locals.len() as u32 - 1)
    }

    fn block(&mut self, statements: Vec<Statement>, kind: TerminatorKind) -> BasicBlock {
        self.blocks.push(BasicBlockData {
            statements,
            terminator: Terminator {
                kind,
                source_info: self.si,
            },
        });
        BasicBlock(self.blocks.len() as u32 - 1)
    }

    /// Drops `parts` in order, then goes to `next`; the first block.
    fn drops(&mut self, parts: Vec<(Place, Ty)>, next: BasicBlock) -> BasicBlock {
        let mut next = next;
        for (p, _) in parts.into_iter().rev() {
            next = self.block(
                vec![],
                TerminatorKind::Drop {
                    place: p,
                    target: next,
                    unwind: UnwindAction::Abort,
                },
            );
        }
        next
    }

    /// `r = &mut place; _ = f(move r) -> next`; the block.
    fn call_on(&mut self, f: InstanceId, place: Place, ty: Ty, next: BasicBlock) -> BasicBlock {
        let r = self.local(self.tys.intern(TyKind::MutRef(ty)));
        let u = self.local(self.tys.unit());
        self.block(
            vec![Statement {
                kind: StatementKind::Assign(Place::local(r), Rvalue::Ref(BorrowKind::Mut, place)),
                source_info: self.si,
            }],
            TerminatorKind::Call {
                func: fn_const(self.tys, f),
                args: vec![Operand::Move(Place::local(r))],
                destination: Place::local(u),
                target: Some(next),
                unwind: UnwindAction::Abort,
            },
        )
    }

    /// Drops the elements of the `Vec` at `v` in index order, in place,
    /// then forgets them and drops the emptied `Vec`, as `mir::frozen`
    /// does; the first block.
    ///
    /// ```text
    /// start: r0 = &v; n = Vec[E].len(move r0) -> init
    /// init:  i = 0; goto head
    /// head:  c = Lt(i, n); switchInt(c) -> [0: forget, otherwise: elem]
    /// elem:  rm = &mut v; re = Vec[E].index_mut(move rm, i) -> drop
    /// drop:  drop(*re) -> step
    /// step:  i = Add(i, 1); goto head
    /// forget: rf = &mut v; _ = Vec[E].__forget_items(move rf) -> done
    /// done:  drop(v) -> next
    /// ```
    fn vec_elems(&mut self, v: Place, vt: Ty, e: Ty, next: BasicBlock) -> BasicBlock {
        let tys = self.tys;
        let (i64t, boolt, unit) = (tys.int(IntTy::I64), tys.bool(), tys.unit());
        let n = self.local(i64t);
        let i = self.local(i64t);
        let c = self.local(boolt);
        let r0 = self.local(tys.intern(TyKind::Ref(vt)));
        let rm = self.local(tys.intern(TyKind::MutRef(vt)));
        let re = self.local(tys.intern(TyKind::MutRef(e)));
        let rf = self.local(tys.intern(TyKind::MutRef(vt)));
        let u = self.local(unit);
        let int = |k: u128| {
            Operand::Const(Const {
                ty: i64t,
                kind: ConstKind::Scalar(k),
            })
        };
        let native = |m: &str| {
            Operand::Const(Const {
                ty: unit,
                kind: ConstKind::FnDef(InstanceId {
                    def: crate::ids::DefId(u32::MAX),
                    args: Vec::new(),
                    name: format!("{}.{m}", tys.display(vt)),
                }),
            })
        };
        let [start, init, head, elem, drop_e, step, forget, done] =
            [(); 8].map(|_| self.block(vec![], TerminatorKind::Unreachable));
        let si = self.si;
        let assign = |p: Local, rv: Rvalue| Statement {
            kind: StatementKind::Assign(Place::local(p), rv),
            source_info: si,
        };
        let call = |func: Operand, args: Vec<Operand>, dest: Local, target: BasicBlock| {
            TerminatorKind::Call {
                func,
                args,
                destination: Place::local(dest),
                target: Some(target),
                unwind: UnwindAction::Abort,
            }
        };
        let mv = |l: Local| Operand::Move(Place::local(l));
        let cp = |l: Local| Operand::Copy(Place::local(l));
        let plan: Vec<(BasicBlock, Vec<Statement>, TerminatorKind)> = vec![
            (
                start,
                vec![assign(r0, Rvalue::Ref(BorrowKind::Shared, v.clone()))],
                call(native("len"), vec![mv(r0)], n, init),
            ),
            (
                init,
                vec![assign(i, Rvalue::Use(int(0)))],
                TerminatorKind::Goto { target: head },
            ),
            (
                head,
                vec![assign(c, Rvalue::BinaryOp(BinOp::Lt, cp(i), cp(n)))],
                TerminatorKind::SwitchInt {
                    discr: cp(c),
                    targets: SwitchTargets::if_else(elem, forget),
                },
            ),
            (
                elem,
                vec![assign(rm, Rvalue::Ref(BorrowKind::Mut, v.clone()))],
                call(native("index_mut"), vec![mv(rm), cp(i)], re, drop_e),
            ),
            (
                drop_e,
                vec![],
                TerminatorKind::Drop {
                    place: Place::local(re).project(ProjElem::Deref),
                    target: step,
                    unwind: UnwindAction::Abort,
                },
            ),
            (
                step,
                vec![assign(i, Rvalue::BinaryOp(BinOp::Add, cp(i), int(1)))],
                TerminatorKind::Goto { target: head },
            ),
            (
                forget,
                vec![assign(rf, Rvalue::Ref(BorrowKind::Mut, v.clone()))],
                call(native("__forget_items"), vec![mv(rf)], u, done),
            ),
            (
                done,
                vec![],
                TerminatorKind::Drop {
                    place: v,
                    target: next,
                    unwind: UnwindAction::Abort,
                },
            ),
        ];
        for (b, stmts, kind) in plan {
            let d = &mut self.blocks[b.index()];
            d.statements = stmts;
            d.terminator.kind = kind;
        }
        start
    }
}

fn fn_const(tys: &TyInterner, inst: InstanceId) -> Operand {
    Operand::Const(Const {
        ty: tys.unit(),
        kind: ConstKind::FnDef(inst),
    })
}

fn push_local(b: &mut Body, ty: Ty, si: SourceInfo) -> Local {
    b.locals.push(LocalDecl {
        ty,
        mutability: Mutability::Mut,
        kind: LocalKind::Temp,
        source_info: si,
    });
    Local(b.locals.len() as u32 - 1)
}

fn empty_like(b: &Body) -> Body {
    Body {
        instance: b.instance.clone(),
        locals: Vec::new(),
        arg_count: 0,
        blocks: Vec::new(),
        scopes: Vec::new(),
        phase: b.phase,
        span: b.span,
        auto_par_candidates: Vec::new(),
        par_regions: vec![],
        provided: Vec::new(),
        suspends: false,
        drop_flags: vec![],
        fn_value_effects: Vec::new(),
        clause_names_var: false,
    }
}

/// Makes each cancel check of `b` (`lower::cancel_exit`'s `__unwinding`
/// call) read `false`, so its waits are not cancellation points and the
/// readiness loops the transform puts around them ask nothing.
fn remove_cancel_checks(b: &mut Body, tys: &TyInterner) {
    for d in &mut b.blocks {
        let TerminatorKind::Call {
            func,
            destination,
            target: Some(next),
            ..
        } = &d.terminator.kind
        else {
            continue;
        };
        if coroutine::direct_callee(func) != Some(coroutine::UNWINDING) {
            continue;
        }
        let (dest, next) = (destination.clone(), *next);
        d.statements.push(Statement {
            kind: StatementKind::Assign(
                dest,
                Rvalue::Use(Operand::Const(Const {
                    ty: tys.bool(),
                    kind: ConstKind::Scalar(0),
                })),
            ),
            source_info: d.terminator.source_info,
        });
        d.terminator.kind = TerminatorKind::Goto { target: next };
    }
}
