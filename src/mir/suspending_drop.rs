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
//! The parts MIR cannot name are reached through natives (B-2026-11-17-101):
//!
//! - a `shared` value: `__shared_release` counts the handle off, and only
//!   the last one drops the value's parts, then `__shared_free` frees the
//!   box unless a `weak` handle keeps it;
//! - a `dyn` value: each table of its trait gets one more slot, after the
//!   methods, filled with its concrete type's glue (the backends' plain
//!   drop where that one does not suspend), which the `dyn` type's glue
//!   calls through like a method that may suspend, then `__dyn_free`
//!   frees the value's block;
//! - a `Map`, `Set`, `SortedMap`, `SortedSet` or `VecDeque`: its entries
//!   move out into a `Vec` (`drain_entries`) and drop from there, a map
//!   entry's key before its value;
//! - a `Mutex[T]`: its `T` drops in place, through `get_mut`;
//! - a `OnceCell[T]` / `OnceLock[T]`: its value, if set, moves out into a
//!   `Vec` (`drain_entries`) and drops from there, then the emptied cell
//!   is freed (B-2026-11-17-127);
//! - a `Sender[T]`, `Receiver[T]` or `BoundedChannel[T]`: when it is the
//!   channel's last end, the values still queued move out into a `Vec`
//!   (`drain_entries`) and drop from there, oldest first; then the end
//!   goes as before (B-2026-11-17-127).
//!
//! A drop that reaches a suspending body only through anything else (a
//! function value's environment) keeps the backends' glue, which reports
//! that the drop cannot wait there (B-2026-11-18-1). A `LazyLock` is
//! only ever a module-level binding, which is never dropped.
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

/// `__shared_release(mut ref h) -> bool`: counts the `shared` handle `h`
/// off and answers whether it was the last strong one. When it was, the
/// box stays, for the glue to drop its parts and then free it with
/// [`SHARED_FREE`], as the backends' `release` does in one go.
pub const SHARED_RELEASE: &str = "__shared_release";
/// `__shared_free(mut ref h)`: frees the box of the last handle `h`, whose
/// parts are dropped, unless a `weak` handle still keeps it.
pub const SHARED_FREE: &str = "__shared_free";
/// `__dyn_free(mut ref d)`: frees the block of the owned `dyn` value `d`,
/// whose concrete value its table's drop slot dropped in place.
pub const DYN_FREE: &str = "__dyn_free";

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
    vtables: &mut [Vtable],
) -> Result<(), String> {
    let set = coroutine::coroutines_from(bodies.iter(), seed);
    let by_name: BTreeMap<String, usize> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| (b.instance.name.clone(), i))
        .collect();
    let tables: Vec<(crate::ids::DefId, Ty, u32)> = vtables
        .iter()
        .filter_map(|v| match tys.kind(v.dyn_ty) {
            TyKind::Dyn { trait_, .. } => Some((trait_, v.concrete, v.slots.len() as u32)),
            _ => None,
        })
        .collect();
    let mut pass = Pass {
        tys,
        drop_body,
        tables,
        drop_slots: BTreeMap::new(),
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
    // A `dyn` type's glue calls the drop slot of its trait's tables, which
    // each table fills with its concrete type's glue.
    let mut filled: BTreeSet<crate::ids::DefId> = BTreeSet::new();
    loop {
        while let Some(ty) = pass.queue.pop() {
            let mut g = pass.glue(ty, &template)?;
            pass.rewrite_drops(&mut g)?;
            pass.bodies.push(g);
        }
        let new: Vec<(crate::ids::DefId, u32)> = pass
            .drop_slots
            .iter()
            .filter(|(t, _)| !filled.contains(t))
            .map(|(&t, &k)| (t, k))
            .collect();
        if new.is_empty() {
            break;
        }
        for (trait_, k) in new {
            filled.insert(trait_);
            for v in vtables.iter_mut() {
                if !matches!(tys.kind(v.dyn_ty), TyKind::Dyn { trait_: t, .. } if t == trait_) {
                    continue;
                }
                if v.slots.len() != k as usize {
                    return Err(format!(
                        "the `dyn` tables of `{}` disagree on their slots",
                        v.name
                    ));
                }
                let glue = pass.glue_of(v.concrete);
                v.slots.push(glue.name);
            }
        }
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
    /// The program's `dyn` tables: each one's trait, concrete type and
    /// number of slots.
    tables: Vec<(crate::ids::DefId, Ty, u32)>,
    /// Per trait whose `dyn` values a glue drops: the slot, after the
    /// trait's methods, that its tables fill with their concrete type's
    /// glue.
    drop_slots: BTreeMap<crate::ids::DefId, u32>,
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
        if !self.has_drop_impl(ty) {
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

    /// Whether `ty`, a struct or enum or its `shared` handle, has a `Drop`
    /// impl (`TyInterner::has_drop_impl` answers for plain ADTs only).
    fn has_drop_impl(&self, ty: Ty) -> bool {
        match self.tys.kind(ty) {
            TyKind::Adt(a) | TyKind::Shared(a) => self.tys.adt(a).has_drop_impl,
            _ => false,
        }
    }

    /// The `Drop` body of `ty` itself, suspending or not.
    fn own_drop(&self, ty: Ty) -> Option<InstanceId> {
        if !self.has_drop_impl(ty) {
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
            TyKind::Adt(_) if mutex_held(tys, ty).is_some() => mutex_held(tys, ty).map(|h| vec![h]),
            TyKind::Adt(_) if drained_cell(tys, ty).is_some() => {
                drained_cell(tys, ty).map(|h| vec![h])
            }
            TyKind::Adt(a) | TyKind::Shared(a) => {
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
            TyKind::Intrinsic(
                IntrinsicTy::Vec(e)
                | IntrinsicTy::Set(e)
                | IntrinsicTy::SortedSet(e)
                | IntrinsicTy::VecDeque(e),
            ) => Some(vec![e]),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                Some(vec![k, v])
            }
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
        let r = if let TyKind::Dyn { trait_, .. } = self.tys.kind(ty) {
            // A `dyn` value reaches what any concrete type behind it does.
            let behind: Vec<Ty> = self
                .tables
                .iter()
                .filter(|(t, _, _)| *t == trait_)
                .map(|&(_, c, _)| c)
                .collect();
            behind.into_iter().any(|c| self.reaches(c))
        } else {
            self.parts(ty).is_some_and(|parts| {
                (matches!(self.tys.kind(ty), TyKind::Adt(_) | TyKind::Shared(_))
                    && self.own_suspending(ty).is_some())
                    || parts.into_iter().any(|p| self.reaches(p))
            })
        };
        self.deciding.remove(&ty);
        if r || !self.leaned {
            self.memo.insert(ty, r);
        }
        self.leaned |= outer;
        r
    }

    /// The drop slot of the `dyn` tables of `trait_`: the first after its
    /// methods.
    fn drop_slot(&mut self, trait_: crate::ids::DefId) -> u32 {
        if let Some(&k) = self.drop_slots.get(&trait_) {
            return k;
        }
        let k = self
            .tables
            .iter()
            .find(|(t, _, _)| *t == trait_)
            .map_or(0, |&(_, _, n)| n);
        self.drop_slots.insert(trait_, k);
        k
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
            // A type behind a `dyn` whose drop does not suspend: the
            // backends' glue, as its table's drop slot.
            _ if !self.reaches(ty) => g.drops(vec![(this.clone(), ty)], finish),
            // The table's drop slot drops the concrete value in place, then
            // `__dyn_free` frees its block.
            TyKind::Dyn { trait_, .. } => {
                let k = self.drop_slot(trait_);
                let u = g.local(tys.unit());
                let free = g.block(
                    vec![],
                    TerminatorKind::Call {
                        func: fn_const(
                            tys,
                            InstanceId {
                                def: crate::ids::DefId(u32::MAX),
                                args: Vec::new(),
                                name: DYN_FREE.to_string(),
                            },
                        ),
                        args: vec![Operand::Move(Place::local(Local(1)))],
                        destination: Place::local(u),
                        target: Some(finish),
                        unwind: UnwindAction::Abort,
                    },
                );
                let u = g.local(tys.unit());
                g.block(
                    vec![],
                    TerminatorKind::Call {
                        func: Operand::Const(Const {
                            ty: tys.unit(),
                            kind: ConstKind::DynSlot {
                                dyn_ty: ty,
                                slot: k,
                            },
                        }),
                        args: vec![Operand::Copy(Place::local(Local(1)))],
                        destination: Place::local(u),
                        target: Some(free),
                        unwind: UnwindAction::Abort,
                    },
                )
            }
            // A `Mutex[T]` holds its `T` in place: dropped there, through
            // `get_mut`.
            TyKind::Adt(_) if mutex_held(tys, ty).is_some() => {
                let held = mutex_held(tys, ty).unwrap_or(ty);
                let r = g.local(tys.intern(TyKind::MutRef(ty)));
                let v = g.local(tys.intern(TyKind::MutRef(held)));
                let drop_v = g.drops(vec![(Place::local(v).project(ProjElem::Deref), held)], ret);
                g.block(
                    vec![Statement {
                        kind: StatementKind::Assign(
                            Place::local(r),
                            Rvalue::Ref(BorrowKind::Mut, this.clone()),
                        ),
                        source_info: si,
                    }],
                    TerminatorKind::Call {
                        func: Operand::Const(Const {
                            ty: tys.unit(),
                            kind: ConstKind::FnDef(InstanceId {
                                def: crate::ids::DefId(u32::MAX),
                                args: Vec::new(),
                                name: format!("{}.get_mut", tys.display(ty)),
                            }),
                        }),
                        args: vec![Operand::Move(Place::local(r))],
                        destination: Place::local(v),
                        target: Some(drop_v),
                        unwind: UnwindAction::Abort,
                    },
                )
            }
            // A once cell's value, or a channel's queue at its last end,
            // moved out into a `Vec` and dropped from there; then the
            // emptied cell or the end by a plain `Drop`.
            TyKind::Adt(_) if drained_cell(tys, ty).is_some() => {
                let held = drained_cell(tys, ty).unwrap_or(ty);
                g.drain_then(this.clone(), ty, held, false, ret)
            }
            TyKind::Adt(a) | TyKind::Shared(a) => {
                let adt = tys.adt(a);
                let shared = matches!(tys.kind(ty), TyKind::Shared(_));
                // A `shared` value's parts go with its last handle, and its
                // box after them: `__shared_free`.
                let end = if shared {
                    g.on_handle(SHARED_FREE, this.clone(), ty, ret)
                } else {
                    ret
                };
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
                            arms.push((v as u128, g.drops(parts, end)));
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
                                otherwise: end,
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
                    g.drops(parts, end)
                };
                let parts = match self.own_drop(ty) {
                    Some(inst) => g.call_on(inst, this.clone(), ty, rest),
                    None => rest,
                };
                if shared {
                    // Only the last handle drops the parts:
                    // `__shared_release` counts this one off and answers
                    // whether it was the last, leaving the box for them.
                    g.release_then(this.clone(), ty, parts, ret)
                } else {
                    parts
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
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) => g.vec_elems(this.clone(), ty, e, false, ret),
            // A map's, a set's or a deque's entries, moved out into a
            // `Vec` and dropped from there.
            TyKind::Intrinsic(
                IntrinsicTy::Set(e) | IntrinsicTy::SortedSet(e) | IntrinsicTy::VecDeque(e),
            ) => g.drain_then(this.clone(), ty, e, false, ret),
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => {
                let entry = tys.intern(TyKind::Tuple(vec![k, v]));
                g.drain_then(this.clone(), ty, entry, true, ret)
            }
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

    /// `r = &mut place; _ = native(move r) -> next`; the block.
    fn on_handle(&mut self, native: &str, place: Place, ty: Ty, next: BasicBlock) -> BasicBlock {
        let f = InstanceId {
            def: crate::ids::DefId(u32::MAX),
            args: Vec::new(),
            name: native.to_string(),
        };
        self.call_on(f, place, ty, next)
    }

    /// `r = &mut h; last = __shared_release(move r)`, then `parts` when
    /// it was the last handle and `next` when not; the first block.
    fn release_then(
        &mut self,
        h: Place,
        ty: Ty,
        parts: BasicBlock,
        next: BasicBlock,
    ) -> BasicBlock {
        let r = self.local(self.tys.intern(TyKind::MutRef(ty)));
        let last = self.local(self.tys.bool());
        let sw = self.block(
            vec![],
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::local(last)),
                targets: SwitchTargets::if_else(parts, next),
            },
        );
        self.block(
            vec![Statement {
                kind: StatementKind::Assign(Place::local(r), Rvalue::Ref(BorrowKind::Mut, h)),
                source_info: self.si,
            }],
            TerminatorKind::Call {
                func: fn_const(
                    self.tys,
                    InstanceId {
                        def: crate::ids::DefId(u32::MAX),
                        args: Vec::new(),
                        name: SHARED_RELEASE.to_string(),
                    },
                ),
                args: vec![Operand::Move(Place::local(r))],
                destination: Place::local(last),
                target: Some(sw),
                unwind: UnwindAction::Abort,
            },
        )
    }

    /// The entries of the map, set or deque at `c` (of type `ct`) moved out
    /// into a `Vec[E]` (`drain_entries`, in iteration order, a deque's
    /// front to back) and dropped in that order, a map entry's key before
    /// its value (core semantics §4); then the emptied `c` by a plain
    /// `Drop`. The first block.
    fn drain_then(&mut self, c: Place, ct: Ty, e: Ty, entry: bool, next: BasicBlock) -> BasicBlock {
        let tys = self.tys;
        let vt = tys.intern(TyKind::Intrinsic(IntrinsicTy::Vec(e)));
        let tmp = self.local(vt);
        let r = self.local(tys.intern(TyKind::MutRef(ct)));
        let done = self.block(
            vec![],
            TerminatorKind::Drop {
                place: c.clone(),
                target: next,
                unwind: UnwindAction::Abort,
            },
        );
        let elems = self.vec_elems(Place::local(tmp), vt, e, entry, done);
        self.block(
            vec![Statement {
                kind: StatementKind::Assign(Place::local(r), Rvalue::Ref(BorrowKind::Mut, c)),
                source_info: self.si,
            }],
            TerminatorKind::Call {
                func: Operand::Const(Const {
                    ty: tys.unit(),
                    kind: ConstKind::FnDef(InstanceId {
                        def: crate::ids::DefId(u32::MAX),
                        args: Vec::new(),
                        name: format!("{}.drain_entries", tys.display(ct)),
                    }),
                }),
                args: vec![Operand::Move(Place::local(r))],
                destination: Place::local(tmp),
                target: Some(elems),
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
    ///
    /// With `entry`, each element is a map's `(key, value)` entry, whose
    /// key drops first.
    fn vec_elems(&mut self, v: Place, vt: Ty, e: Ty, entry: bool, next: BasicBlock) -> BasicBlock {
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
        if let (true, TyKind::Tuple(kv)) = (entry, tys.kind(e)) {
            let at = Place::local(re).project(ProjElem::Deref);
            let value = self.block(
                vec![],
                TerminatorKind::Drop {
                    place: at.field(1, kv[1]),
                    target: step,
                    unwind: UnwindAction::Abort,
                },
            );
            self.blocks[drop_e.index()].terminator.kind = TerminatorKind::Drop {
                place: at.field(0, kv[0]),
                target: value,
                unwind: UnwindAction::Abort,
            };
        }
        start
    }
}

/// The type a library cell whose glue drains it holds, `T`, when `ty` is
/// a `OnceCell[T]`, `OnceLock[T]`, `Sender[T]`, `Receiver[T]` or
/// `BoundedChannel[T]` (B-2026-11-17-127).
fn drained_cell(tys: &TyInterner, ty: Ty) -> Option<Ty> {
    let TyKind::Adt(a) = tys.kind(ty) else {
        return None;
    };
    if !matches!(
        tys.adt(a).library_name(),
        Some("OnceCell" | "OnceLock" | "Sender" | "Receiver" | "BoundedChannel")
    ) {
        return None;
    }
    tys.tcx()
        .adt_of(ty)
        .and_then(|(_, args)| args.first().copied())
}

/// The type a library `Mutex[T]` holds in place, `T`, when `ty` is one.
fn mutex_held(tys: &TyInterner, ty: Ty) -> Option<Ty> {
    let TyKind::Adt(a) = tys.kind(ty) else {
        return None;
    };
    if tys.adt(a).library_name() != Some("Mutex") {
        return None;
    }
    tys.tcx()
        .adt_of(ty)
        .and_then(|(_, args)| args.first().copied())
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
