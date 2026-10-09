//! The stackless coroutine transform (`docs/spikes/coroutines.md`, "The MIR
//! transform"): which bodies are coroutines, where they suspend, and which
//! locals the frame must hold across those points.
//!
//! A suspending call is an ordinary `Call` to every pass before this one, so
//! the analysis runs on elaborated, borrow-checked bodies and only reads them.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{DefId, NodeId};

use super::borrowck::liveness;
use super::syntax::*;
use super::ty::{AdtDef, IntTy, Ty, TyInterner, TyKind, VariantDef};

/// Natives that suspend. `__yield_now` is the executor's test native: it is
/// `Pending` on its first resume and `Ready(())` on the next.
pub const SUSPENDING_NATIVES: &[&str] = &["__yield_now"];

/// The callee a `Call` names directly, if it names one.
fn direct_callee(func: &Operand) -> Option<&str> {
    match func {
        Operand::Const(Const {
            kind: ConstKind::FnDef(inst),
            ..
        }) => Some(&inst.name),
        _ => None,
    }
}

fn calls(body: &Body) -> impl Iterator<Item = (BasicBlock, &str)> {
    body.blocks.iter().enumerate().filter_map(|(i, b)| {
        if let TerminatorKind::Call { func, .. } = &b.terminator.kind {
            direct_callee(func).map(|c| (BasicBlock(i as u32), c))
        } else {
            None
        }
    })
}

/// The bodies that are coroutines: those that declare `suspends`, and,
/// transitively, those that call a coroutine or a suspending native.
pub fn coroutines<'a>(bodies: impl IntoIterator<Item = &'a Body> + Clone) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = bodies
        .clone()
        .into_iter()
        .filter(|b| b.suspends)
        .map(|b| b.instance.name.clone())
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for b in bodies.clone() {
            if set.contains(&b.instance.name) {
                continue;
            }
            if calls(b).any(|(_, c)| set.contains(c) || SUSPENDING_NATIVES.contains(&c)) {
                set.insert(b.instance.name.clone());
                changed = true;
            }
        }
    }
    set
}

/// A call that may suspend.
#[derive(Debug, Clone, PartialEq)]
pub struct SuspensionPoint {
    /// The block whose terminator is the call.
    pub block: BasicBlock,
    pub callee: String,
    /// Where the call returns to.
    pub resume: BasicBlock,
    /// The locals live across the call, the destination excepted: the
    /// frame must hold them while the callee is pending.
    pub across: Vec<Local>,
}

/// Where a coroutine suspends, and the locals its frame holds.
#[derive(Debug, Clone, PartialEq)]
pub struct CoroutineLayout {
    /// In block order; point `k` resumes in state `k + 1`.
    pub points: Vec<SuspensionPoint>,
    /// Every local some point holds across, plus every borrowed local (a
    /// `ref` held across a point may be its only use), in local order.
    pub frame: Vec<Local>,
}

/// The suspension points of `body`, a member of `coroutines`, and its frame.
///
/// The frame over-approximates in one way: a local that is ever borrowed is
/// in it, because a reference to it may be live across a point when the
/// local itself is not used again. Narrowing that needs the borrow check's
/// loan liveness, and only costs frame size.
pub fn layout(body: &Body, coroutines: &BTreeSet<String>) -> CoroutineLayout {
    let live_out = liveness(body);
    let mut points = Vec::new();
    let mut frame: BTreeSet<Local> = BTreeSet::new();
    for (bb, callee) in calls(body) {
        if !coroutines.contains(callee) && !SUSPENDING_NATIVES.contains(&callee) {
            continue;
        }
        let TerminatorKind::Call {
            destination,
            target,
            ..
        } = &body.block(bb).terminator.kind
        else {
            unreachable!("calls yields Call blocks")
        };
        let Some(resume) = *target else {
            // A call that never returns never resumes either.
            continue;
        };
        let mut across: Vec<Local> = live_out[bb.index()]
            .iter()
            .copied()
            .filter(|&l| !(l == destination.local && destination.projection.is_empty()))
            .collect();
        across.sort();
        frame.extend(across.iter().copied());
        points.push(SuspensionPoint {
            block: bb,
            callee: callee.to_string(),
            resume,
            across,
        });
    }
    if !points.is_empty() {
        for b in &body.blocks {
            for s in &b.statements {
                if let StatementKind::Assign(_, Rvalue::Ref(_, p)) = &s.kind {
                    frame.insert(p.local);
                }
            }
        }
    }
    CoroutineLayout {
        points,
        frame: frame.into_iter().collect(),
    }
}

/// Coroutines that reach themselves through suspending calls. Their frame
/// would contain itself, so the transform refuses them until a measurement
/// says services need it (then the callee frame at the back edge is boxed).
/// Each cycle is named once, by its members in order.
pub fn recursive_cycles<'a>(
    bodies: impl IntoIterator<Item = &'a Body>,
    coroutines: &BTreeSet<String>,
) -> Vec<Vec<String>> {
    let edges: BTreeMap<&str, BTreeSet<&str>> = bodies
        .into_iter()
        .filter(|b| coroutines.contains(&b.instance.name))
        .map(|b| {
            let out = calls(b)
                .map(|(_, c)| c)
                .filter(|c| coroutines.contains(*c))
                .collect();
            (b.instance.name.as_str(), out)
        })
        .collect();
    // Strongly connected components, by mutual reachability: the graphs
    // are a handful of nodes.
    let reach = |from: &str| -> BTreeSet<&str> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<&str> = edges.get(from).into_iter().flatten().copied().collect();
        while let Some(n) = stack.pop() {
            if seen.insert(n) {
                stack.extend(edges.get(n).into_iter().flatten().copied());
            }
        }
        seen
    };
    let reaches: BTreeMap<&str, BTreeSet<&str>> = edges.keys().map(|&n| (n, reach(n))).collect();
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut cycles = Vec::new();
    for (&n, r) in &reaches {
        if done.contains(n) || !r.contains(n) {
            continue;
        }
        let scc: Vec<String> = reaches
            .iter()
            .filter(|(m, mr)| r.contains(*m) && mr.contains(n))
            .map(|(m, _)| m.to_string())
            .collect();
        done.extend(scc.iter().cloned());
        cycles.push(scc);
    }
    cycles
}

// ---- the transform ----

/// The local a resume body reaches its frame through.
const FRAME: Local = Local(1);

/// A coroutine's frame type and where each of its locals lives in it.
#[derive(Debug, Clone)]
pub struct FrameInfo {
    /// `F.Frame`, a struct whose field 0 is the `u32` state.
    pub ty: Ty,
    /// The field holding each frame local of the original body.
    pub fields: BTreeMap<Local, u32>,
    /// The field holding the callee's frame at each suspension point that
    /// calls a body (a native keeps no frame).
    pub subs: BTreeMap<usize, u32>,
    /// `Poll[T]` for the body's return type `T`: `Ready(T) | Pending`.
    pub poll: Ty,
    /// The state of a frame whose body has returned.
    pub returned: u32,
}

/// The `resume` body of coroutine `name`.
pub fn resume_name(name: &str) -> String {
    format!("{name}.resume")
}

/// The `drop_frame` body of coroutine `name`.
pub fn drop_frame_name(name: &str) -> String {
    format!("{name}.drop_frame")
}

/// A DefId no type uses yet, for the frame and `Poll` types the transform
/// makes.
fn fresh_def(tys: &TyInterner) -> DefId {
    let mut n = u32::MAX / 2;
    while tys.tcx().adt_def(DefId(n)).is_some() {
        n += 1;
    }
    DefId(n)
}

fn poll_of(tys: &TyInterner, polls: &mut BTreeMap<Ty, Ty>, t: Ty) -> Ty {
    if let Some(&p) = polls.get(&t) {
        return p;
    }
    let def = fresh_def(tys);
    let id = tys.add_adt(AdtDef {
        def,
        name: format!("Poll[{}]", tys.display(t)),
        is_enum: true,
        variants: vec![
            VariantDef {
                name: "Ready".into(),
                fields: vec![("0".into(), t)],
            },
            VariantDef {
                name: "Pending".into(),
                fields: vec![],
            },
        ],
        has_drop_impl: false,
        is_copy: false,
    });
    let p = tys.intern(TyKind::Adt(id));
    polls.insert(t, p);
    p
}

/// Rewrites every coroutine among `bodies` into its `resume` body
/// (`F.resume(frame: mut ref F.Frame) -> Poll[T]`), registering the frame
/// and `Poll` types in `tys`. The other bodies are returned unchanged; the
/// original coroutine bodies are not returned, since nothing may call one
/// except through its `resume`.
///
/// Refuses a recursive cycle of coroutines, whose frame would contain
/// itself.
pub fn transform(
    bodies: &[Body],
    tys: &TyInterner,
) -> Result<(Vec<Body>, BTreeMap<String, FrameInfo>), String> {
    let set = coroutines(bodies);
    if let Some(cycle) = recursive_cycles(bodies, &set).first() {
        return Err(format!(
            "recursive coroutines are not supported yet: {}",
            cycle.join(", ")
        ));
    }
    let by_name: BTreeMap<&str, &Body> = bodies
        .iter()
        .filter(|b| set.contains(&b.instance.name))
        .map(|b| (b.instance.name.as_str(), b))
        .collect();
    let layouts: BTreeMap<&str, CoroutineLayout> =
        by_name.iter().map(|(&n, b)| (n, layout(b, &set))).collect();
    // A frame holds its callees' frames, so make the callees' types first.
    let mut order: Vec<&str> = Vec::new();
    fn visit<'a>(
        n: &'a str,
        layouts: &BTreeMap<&'a str, CoroutineLayout>,
        order: &mut Vec<&'a str>,
    ) {
        if order.contains(&n) {
            return;
        }
        for p in &layouts[n].points {
            if let Some((&c, _)) = layouts.get_key_value(p.callee.as_str()) {
                visit(c, layouts, order);
            }
        }
        order.push(n);
    }
    for &n in layouts.keys() {
        visit(n, &layouts, &mut order);
    }
    let u32t = tys.int(IntTy::U32);
    let mut polls = BTreeMap::new();
    let mut frames: BTreeMap<String, FrameInfo> = BTreeMap::new();
    for &n in &order {
        let body = by_name[n];
        let lay = &layouts[n];
        let mut locals: BTreeSet<Local> = lay.frame.iter().copied().collect();
        locals.extend(body.args());
        let mut fields_def = vec![("state".to_string(), u32t)];
        let mut fields = BTreeMap::new();
        for l in locals {
            fields.insert(l, fields_def.len() as u32);
            fields_def.push((format!("l{}", l.0), body.local(l).ty));
        }
        let mut subs = BTreeMap::new();
        for (k, p) in lay.points.iter().enumerate() {
            if let Some(f) = frames.get(&p.callee) {
                subs.insert(k, fields_def.len() as u32);
                fields_def.push((format!("sub{k}"), f.ty));
            }
        }
        let id = tys.add_adt(AdtDef {
            def: fresh_def(tys),
            name: format!("{n}.Frame"),
            is_enum: false,
            variants: vec![VariantDef {
                name: format!("{n}.Frame"),
                fields: fields_def,
            }],
            has_drop_impl: false,
            is_copy: false,
        });
        frames.insert(
            n.to_string(),
            FrameInfo {
                ty: tys.intern(TyKind::Adt(id)),
                fields,
                subs,
                poll: poll_of(tys, &mut polls, body.return_ty()),
                returned: lay.points.len() as u32 + 1,
            },
        );
    }
    let mut out: Vec<Body> = bodies
        .iter()
        .filter(|b| !set.contains(&b.instance.name))
        .cloned()
        .collect();
    for &n in &order {
        out.push(Resume::new(by_name[n], &layouts[n], &frames, tys).build()?);
        out.push(drop_frame(by_name[n], &layouts[n], &frames, tys)?);
    }
    Ok((out, frames))
}

struct Resume<'a> {
    body: &'a Body,
    layout: &'a CoroutineLayout,
    frames: &'a BTreeMap<String, FrameInfo>,
    me: &'a FrameInfo,
    tys: &'a TyInterner,
    locals: Vec<LocalDecl>,
    blocks: Vec<BasicBlockData>,
}

impl<'a> Resume<'a> {
    fn new(
        body: &'a Body,
        layout: &'a CoroutineLayout,
        frames: &'a BTreeMap<String, FrameInfo>,
        tys: &'a TyInterner,
    ) -> Self {
        Resume {
            body,
            layout,
            frames,
            me: &frames[&body.instance.name],
            tys,
            locals: Vec::new(),
            blocks: Vec::new(),
        }
    }

    fn info(&self) -> SourceInfo {
        SourceInfo {
            span: self.body.span,
            scope: SourceScope(0),
        }
    }

    fn temp(&mut self, ty: Ty) -> Local {
        self.locals.push(LocalDecl {
            ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Temp,
            source_info: self.info(),
        });
        Local(self.locals.len() as u32 - 1)
    }

    fn stmt(&self, kind: StatementKind) -> Statement {
        Statement {
            kind,
            source_info: self.info(),
        }
    }

    fn assign(&self, p: Place, rv: Rvalue) -> Statement {
        self.stmt(StatementKind::Assign(p, rv))
    }

    /// `(*frame).f`, of type `ty`.
    fn field(&self, f: u32, ty: Ty) -> Place {
        Place {
            local: FRAME,
            projection: vec![ProjElem::Deref, ProjElem::Field(FieldIdx(f), ty)],
        }
    }

    fn state(&self) -> Place {
        self.field(0, self.tys.int(IntTy::U32))
    }

    fn u32_const(&self, v: u32) -> Operand {
        Operand::Const(Const {
            ty: self.tys.int(IntTy::U32),
            kind: ConstKind::Scalar(v as u128),
        })
    }

    /// `state = s; _0 = Poll::Pending; return`, after `stmts`.
    fn pending(&self, mut stmts: Vec<Statement>, s: u32) -> BasicBlockData {
        stmts.push(self.assign(self.state(), Rvalue::Use(self.u32_const(s))));
        stmts.push(self.assign(
            Place::local(Local::RETURN_PLACE),
            Rvalue::Aggregate(
                AggregateKind::Adt {
                    ty: self.me.poll,
                    variant: VariantIdx(1),
                },
                vec![],
            ),
        ));
        BasicBlockData {
            statements: stmts,
            terminator: Terminator {
                kind: TerminatorKind::Return,
                source_info: self.info(),
            },
        }
    }

    fn local(&self, l: Local) -> Result<Place, String> {
        Ok(match self.me.fields.get(&l) {
            Some(&f) => self.field(f, self.body.local(l).ty),
            None => Place::local(Local(l.0 + 2)),
        })
    }

    fn place(&self, p: &Place) -> Result<Place, String> {
        let mut out = self.local(p.local)?;
        for e in &p.projection {
            out.projection.push(match e {
                ProjElem::Index(i) => match self.local(*i)? {
                    Place { local, projection } if projection.is_empty() => ProjElem::Index(local),
                    _ => {
                        return Err(format!(
                            "{}: index {i} is held across a suspension point",
                            self.body.instance.name
                        ))
                    }
                },
                e => *e,
            });
        }
        Ok(out)
    }

    fn operand(&self, o: &Operand) -> Result<Operand, String> {
        Ok(match o {
            Operand::Copy(p) => Operand::Copy(self.place(p)?),
            Operand::Move(p) => Operand::Move(self.place(p)?),
            Operand::Const(c) => Operand::Const(c.clone()),
        })
    }

    fn rvalue(&self, rv: &Rvalue) -> Result<Rvalue, String> {
        Ok(match rv {
            Rvalue::Use(o) => Rvalue::Use(self.operand(o)?),
            Rvalue::UnaryOp(op, o) => Rvalue::UnaryOp(*op, self.operand(o)?),
            Rvalue::Cast(k, o, t) => Rvalue::Cast(*k, self.operand(o)?, *t),
            Rvalue::BinaryOp(op, a, b) => Rvalue::BinaryOp(*op, self.operand(a)?, self.operand(b)?),
            Rvalue::CheckedBinaryOp(op, a, b) => {
                Rvalue::CheckedBinaryOp(*op, self.operand(a)?, self.operand(b)?)
            }
            Rvalue::Aggregate(k, ops) => Rvalue::Aggregate(
                k.clone(),
                ops.iter()
                    .map(|o| self.operand(o))
                    .collect::<Result<_, _>>()?,
            ),
            Rvalue::Ref(k, p) => Rvalue::Ref(*k, self.place(p)?),
            Rvalue::Retain(p) => Rvalue::Retain(self.place(p)?),
            Rvalue::Discriminant(p) => Rvalue::Discriminant(self.place(p)?),
            Rvalue::Len(p) => Rvalue::Len(self.place(p)?),
            Rvalue::NullaryOp(op, t) => Rvalue::NullaryOp(*op, *t),
        })
    }

    fn statement(&self, s: &Statement) -> Result<Statement, String> {
        let frame_local = |l: &Local| self.me.fields.contains_key(l);
        let kind = match &s.kind {
            StatementKind::Assign(p, rv) => StatementKind::Assign(self.place(p)?, self.rvalue(rv)?),
            StatementKind::StorageLive(l) | StatementKind::StorageDead(l) if frame_local(l) => {
                StatementKind::Nop
            }
            StatementKind::StorageLive(l) => StatementKind::StorageLive(Local(l.0 + 2)),
            StatementKind::StorageDead(l) => StatementKind::StorageDead(Local(l.0 + 2)),
            StatementKind::SetDiscriminant(p, v) => {
                StatementKind::SetDiscriminant(self.place(p)?, *v)
            }
            StatementKind::BorrowFlag(op) => StatementKind::BorrowFlag(match op {
                FlagOp::Acquire { place, kind, loan } => FlagOp::Acquire {
                    place: self.place(place)?,
                    kind: *kind,
                    loan: *loan,
                },
                FlagOp::Release { loan } => FlagOp::Release { loan: *loan },
                FlagOp::Check { place, kind } => FlagOp::Check {
                    place: self.place(place)?,
                    kind: *kind,
                },
            }),
            StatementKind::Nop => StatementKind::Nop,
        };
        Ok(Statement {
            kind,
            source_info: s.source_info,
        })
    }

    fn build(mut self) -> Result<Body, String> {
        let name = &self.body.instance.name;
        let b = |bb: BasicBlock| BasicBlock(bb.0 + 1);
        let frame_ref = self.tys.intern(TyKind::MutRef(self.me.ty));
        // `_0: Poll[T]`, `_1: mut ref F.Frame`, then the original locals.
        self.locals.push(LocalDecl {
            ty: self.me.poll,
            mutability: Mutability::Mut,
            kind: LocalKind::ReturnPlace,
            source_info: self.info(),
        });
        self.locals.push(LocalDecl {
            ty: frame_ref,
            mutability: Mutability::Not,
            kind: LocalKind::Arg {
                name: "frame".into(),
                node: NodeId(0),
            },
            source_info: self.info(),
        });
        for d in &self.body.locals {
            let mut d = d.clone();
            if matches!(d.kind, LocalKind::ReturnPlace | LocalKind::Arg { .. }) {
                d.kind = LocalKind::Temp;
            }
            self.locals.push(d);
        }
        // bb0 dispatches on the state; the original blocks follow.
        self.blocks.push(BasicBlockData {
            statements: vec![],
            terminator: Terminator {
                kind: TerminatorKind::Unreachable,
                source_info: self.info(),
            },
        });
        let points: BTreeMap<BasicBlock, usize> = self
            .layout
            .points
            .iter()
            .enumerate()
            .map(|(k, p)| (p.block, k))
            .collect();
        let mut resumes: Vec<(u128, BasicBlock)> = Vec::new();
        let mut extra: Vec<BasicBlockData> = Vec::new();
        let first_extra = 1 + self.body.blocks.len() as u32;
        for (i, data) in self.body.blocks.iter().enumerate() {
            let mut statements = data
                .statements
                .iter()
                .map(|s| self.statement(s))
                .collect::<Result<Vec<_>, _>>()?;
            let t = &data.terminator;
            let kind = match &t.kind {
                TerminatorKind::Call {
                    args,
                    destination,
                    target: Some(next),
                    ..
                } if points.contains_key(&BasicBlock(i as u32)) => {
                    let k = points[&BasicBlock(i as u32)];
                    let state = k as u32 + 1;
                    let callee = &self.layout.points[k].callee;
                    let dest = self.place(destination)?;
                    let at =
                        |extra: &Vec<BasicBlockData>| BasicBlock(first_extra + extra.len() as u32);
                    match self.frames.get(callee) {
                        None => {
                            // A suspending native: `__yield_now` is pending
                            // once, then `()`.
                            let unit = self.tys.unit();
                            let back = at(&extra);
                            extra.push(BasicBlockData {
                                statements: vec![self.assign(
                                    dest,
                                    Rvalue::Use(Operand::Const(Const {
                                        ty: unit,
                                        kind: ConstKind::Unit,
                                    })),
                                )],
                                terminator: Terminator {
                                    kind: TerminatorKind::Goto { target: b(*next) },
                                    source_info: t.source_info,
                                },
                            });
                            resumes.push((state as u128, back));
                            self.blocks.push(self.pending(statements, state));
                            continue;
                        }
                        Some(callee_frame) => {
                            let callee_body_ret = match self.tys.kind(callee_frame.poll) {
                                TyKind::Adt(a) => self.tys.adt(a).variants[0].fields[0].1,
                                _ => unreachable!("Poll is an ADT"),
                            };
                            let sub_f = self.me.subs[&k];
                            let sub = self.field(sub_f, callee_frame.ty);
                            // Start the callee: its state and its arguments.
                            let mut start = sub.clone();
                            start
                                .projection
                                .push(ProjElem::Field(FieldIdx(0), self.tys.int(IntTy::U32)));
                            statements.push(self.assign(start, Rvalue::Use(self.u32_const(0))));
                            for (j, a) in args.iter().enumerate() {
                                let l = Local(j as u32 + 1);
                                let f = callee_frame.fields[&l];
                                let ty = match self.tys.kind(callee_frame.ty) {
                                    TyKind::Adt(id) => {
                                        self.tys.adt(id).variants[0].fields[f as usize].1
                                    }
                                    _ => unreachable!(),
                                };
                                let mut p = sub.clone();
                                p.projection.push(ProjElem::Field(FieldIdx(f), ty));
                                statements.push(self.assign(p, Rvalue::Use(self.operand(a)?)));
                            }
                            let r = self.temp(callee_frame.poll);
                            let tref = self.temp(self.tys.intern(TyKind::MutRef(callee_frame.ty)));
                            let d = self.temp(self.tys.int(IntTy::I64));
                            let poll = at(&extra);
                            let check = BasicBlock(poll.0 + 1);
                            let ready = BasicBlock(poll.0 + 2);
                            let pend = BasicBlock(poll.0 + 3);
                            let unit = self.tys.unit();
                            extra.push(BasicBlockData {
                                statements: vec![self.assign(
                                    Place::local(tref),
                                    Rvalue::Ref(BorrowKind::Mut, sub.clone()),
                                )],
                                terminator: Terminator {
                                    kind: TerminatorKind::Call {
                                        func: Operand::Const(Const {
                                            ty: unit,
                                            kind: ConstKind::FnDef(InstanceId {
                                                def: DefId(0),
                                                args: vec![],
                                                name: resume_name(callee),
                                            }),
                                        }),
                                        args: vec![Operand::Move(Place::local(tref))],
                                        destination: Place::local(r),
                                        target: Some(check),
                                        unwind: UnwindAction::Abort,
                                    },
                                    source_info: t.source_info,
                                },
                            });
                            extra.push(BasicBlockData {
                                statements: vec![self.assign(
                                    Place::local(d),
                                    Rvalue::Discriminant(Place::local(r)),
                                )],
                                terminator: Terminator {
                                    kind: TerminatorKind::SwitchInt {
                                        discr: Operand::Copy(Place::local(d)),
                                        targets: SwitchTargets {
                                            values: vec![(0, ready)],
                                            otherwise: pend,
                                        },
                                    },
                                    source_info: t.source_info,
                                },
                            });
                            extra.push(BasicBlockData {
                                statements: vec![self.assign(
                                    dest,
                                    Rvalue::Use(Operand::Move(Place {
                                        local: r,
                                        projection: vec![
                                            ProjElem::Downcast(VariantIdx(0)),
                                            ProjElem::Field(FieldIdx(0), callee_body_ret),
                                        ],
                                    })),
                                )],
                                terminator: Terminator {
                                    kind: TerminatorKind::Goto { target: b(*next) },
                                    source_info: t.source_info,
                                },
                            });
                            extra.push(self.pending(vec![], state));
                            resumes.push((state as u128, poll));
                            TerminatorKind::Goto { target: poll }
                        }
                    }
                }
                TerminatorKind::Return => {
                    let ret = self.local(Local::RETURN_PLACE)?;
                    statements.push(self.assign(
                        Place::local(Local::RETURN_PLACE),
                        Rvalue::Aggregate(
                            AggregateKind::Adt {
                                ty: self.me.poll,
                                variant: VariantIdx(0),
                            },
                            vec![Operand::Move(ret)],
                        ),
                    ));
                    statements.push(
                        self.assign(self.state(), Rvalue::Use(self.u32_const(self.me.returned))),
                    );
                    TerminatorKind::Return
                }
                TerminatorKind::Goto { target } => TerminatorKind::Goto { target: b(*target) },
                TerminatorKind::SwitchInt { discr, targets } => TerminatorKind::SwitchInt {
                    discr: self.operand(discr)?,
                    targets: SwitchTargets {
                        values: targets.values.iter().map(|&(v, t)| (v, b(t))).collect(),
                        otherwise: b(targets.otherwise),
                    },
                },
                TerminatorKind::Call {
                    func,
                    args,
                    destination,
                    target,
                    unwind,
                } => TerminatorKind::Call {
                    func: self.operand(func)?,
                    args: args
                        .iter()
                        .map(|a| self.operand(a))
                        .collect::<Result<_, _>>()?,
                    destination: self.place(destination)?,
                    target: target.map(b),
                    unwind: *unwind,
                },
                TerminatorKind::Drop {
                    place,
                    target,
                    unwind,
                } => TerminatorKind::Drop {
                    place: self.place(place)?,
                    target: b(*target),
                    unwind: *unwind,
                },
                k @ (TerminatorKind::Abort { .. } | TerminatorKind::Unreachable) => k.clone(),
            };
            self.blocks.push(BasicBlockData {
                statements,
                terminator: Terminator {
                    kind,
                    source_info: t.source_info,
                },
            });
        }
        self.blocks.extend(extra);
        let resumed_after_return = BasicBlock(self.blocks.len() as u32);
        self.blocks.push(BasicBlockData {
            statements: vec![],
            terminator: Terminator {
                kind: TerminatorKind::Abort {
                    reason: AbortReason::Panic,
                },
                source_info: self.info(),
            },
        });
        let mut values = vec![(0, BasicBlock(1))];
        values.extend(resumes);
        self.blocks[0].terminator.kind = TerminatorKind::SwitchInt {
            discr: Operand::Copy(self.state()),
            targets: SwitchTargets {
                values,
                otherwise: resumed_after_return,
            },
        };
        let mut instance = self.body.instance.clone();
        instance.name = resume_name(name);
        Ok(Body {
            instance,
            locals: self.locals,
            arg_count: 1,
            blocks: self.blocks,
            scopes: self.body.scopes.clone(),
            phase: self.body.phase,
            span: self.body.span,
            par_regions: self
                .body
                .par_regions
                .iter()
                .map(|r| ParRegion {
                    kind: r.kind.clone(),
                    span: r.span,
                    branches: r
                        .branches
                        .iter()
                        .map(|br| br.iter().map(|&x| b(x)).collect())
                        .collect(),
                })
                .collect(),
            suspends: false,
            drop_flags: Vec::new(),
        })
    }
}

/// What `drop_frame` does to one place of a suspended frame.
enum Release {
    Drop(Place),
    /// Drop the place when the flag field is set.
    Guarded(Place, Place),
    /// Drop the callee's frame through `G.drop_frame`.
    Callee(String, Place, Ty),
}

/// `F.drop_frame(frame: mut ref F.Frame)`: drops what a frame suspended at
/// each point still owns, innermost first (the callee's frame, then the
/// locals in reverse order), and marks the frame returned. A frame that has
/// not started or has returned owns nothing.
///
/// Which places are initialized at a point comes from the same dataflow
/// drop elaboration uses: a place that is initialized on every path is
/// dropped, one that is on some paths is dropped under the flag
/// elaboration made for it, and one with no flag of its own is opened into
/// its parts, as elaboration opens it.
fn drop_frame(
    body: &Body,
    layout: &CoroutineLayout,
    frames: &BTreeMap<String, FrameInfo>,
    tys: &TyInterner,
) -> Result<Body, String> {
    use super::elaborate::{
        apply, dataflow, entry_state, gather_move_paths, statement_effects, terminator_effects,
        Effect, MovePaths, PathIdx, State,
    };
    let name = &body.instance.name;
    let r = Resume::new(body, layout, frames, tys);
    let me = r.me;
    let paths = gather_move_paths(body, tys, &|t| tys.needs_drop(t))?;
    let stmt_effects: Vec<Vec<Vec<Effect>>> = body
        .blocks
        .iter()
        .map(|b| {
            b.statements
                .iter()
                .map(|s| statement_effects(&paths, &s.kind))
                .collect()
        })
        .collect();
    let term_effects: Vec<Vec<Effect>> = body
        .blocks
        .iter()
        .map(|b| terminator_effects(&paths, &b.terminator.kind))
        .collect();
    let states = dataflow(
        body,
        &paths,
        &entry_state(body, &paths),
        &stmt_effects,
        &term_effects,
    );
    fn release(
        r: &Resume,
        paths: &MovePaths,
        st: &State,
        p: PathIdx,
        out: &mut Vec<Release>,
    ) -> Result<(), String> {
        let place = paths.place(p);
        if st.init[p] && !st.uninit[p] {
            out.push(Release::Drop(r.place(place)?));
        } else if st.init[p] {
            match r.body.drop_flags.iter().find(|(pl, _)| pl == place) {
                Some((_, flag)) => {
                    if !r.me.fields.contains_key(flag) {
                        return Err(format!(
                            "{}: the drop flag of {place:?} is not in the frame",
                            r.body.instance.name
                        ));
                    }
                    out.push(Release::Guarded(r.local(*flag)?, r.place(place)?));
                }
                None => {
                    for &c in paths.children(p).iter().rev() {
                        release(r, paths, st, c, out)?;
                    }
                }
            }
        }
        Ok(())
    }
    let mut per_state: Vec<Vec<Release>> = Vec::new();
    for (k, pt) in layout.points.iter().enumerate() {
        let bi = pt.block.index();
        let mut st = states[bi].clone();
        for effs in &stmt_effects[bi] {
            apply(&paths, &mut st, effs);
        }
        // The call's moves have happened; its destination is not written yet.
        let moved: Vec<Effect> = term_effects[bi]
            .iter()
            .copied()
            .filter(|e| matches!(e, Effect::Uninit(_)))
            .collect();
        apply(&paths, &mut st, &moved);
        let mut out = Vec::new();
        if let Some(callee) = frames.get(&pt.callee) {
            out.push(Release::Callee(
                pt.callee.clone(),
                r.field(me.subs[&k], callee.ty),
                callee.ty,
            ));
        }
        for &l in pt.across.iter().rev() {
            if let Some(p) = paths.lookup(&Place::local(l)) {
                release(&r, &paths, &st, p, &mut out)?;
            }
        }
        per_state.push(out);
    }

    let mut r = r;
    let unit = tys.unit();
    r.locals.push(LocalDecl {
        ty: unit,
        mutability: Mutability::Mut,
        kind: LocalKind::ReturnPlace,
        source_info: r.info(),
    });
    r.locals.push(LocalDecl {
        ty: tys.intern(TyKind::MutRef(me.ty)),
        mutability: Mutability::Not,
        kind: LocalKind::Arg {
            name: "frame".into(),
            node: NodeId(0),
        },
        source_info: r.info(),
    });
    let block = |r: &Resume, statements, kind| BasicBlockData {
        statements,
        terminator: Terminator {
            kind,
            source_info: r.info(),
        },
    };
    // bb0 dispatches, bb1 marks the frame returned.
    r.blocks
        .push(block(&r, vec![], TerminatorKind::Unreachable));
    let done = vec![
        r.assign(r.state(), Rvalue::Use(r.u32_const(me.returned))),
        r.assign(
            Place::local(Local::RETURN_PLACE),
            Rvalue::Use(Operand::Const(Const {
                ty: unit,
                kind: ConstKind::Unit,
            })),
        ),
    ];
    r.blocks.push(block(&r, done, TerminatorKind::Return));
    let mut values = Vec::new();
    for (k, rel) in per_state.into_iter().enumerate() {
        // Built back to front: each release continues to the next one.
        let mut next = BasicBlock(1);
        for a in rel.into_iter().rev() {
            let at = BasicBlock(r.blocks.len() as u32);
            match a {
                Release::Drop(place) => {
                    let kind = TerminatorKind::Drop {
                        place,
                        target: next,
                        unwind: UnwindAction::Abort,
                    };
                    r.blocks.push(block(&r, vec![], kind));
                }
                Release::Guarded(flag, place) => {
                    let kind = TerminatorKind::Drop {
                        place,
                        target: next,
                        unwind: UnwindAction::Abort,
                    };
                    r.blocks.push(block(&r, vec![], kind));
                    let kind = TerminatorKind::SwitchInt {
                        discr: Operand::Copy(flag),
                        targets: SwitchTargets::if_else(at, next),
                    };
                    r.blocks.push(block(&r, vec![], kind));
                }
                Release::Callee(callee, sub, ty) => {
                    let t = r.temp(tys.intern(TyKind::MutRef(ty)));
                    let u = r.temp(unit);
                    let kind = TerminatorKind::Call {
                        func: Operand::Const(Const {
                            ty: unit,
                            kind: ConstKind::FnDef(InstanceId {
                                def: DefId(0),
                                args: vec![],
                                name: drop_frame_name(&callee),
                            }),
                        }),
                        args: vec![Operand::Move(Place::local(t))],
                        destination: Place::local(u),
                        target: Some(next),
                        unwind: UnwindAction::Abort,
                    };
                    let stmts = vec![r.assign(Place::local(t), Rvalue::Ref(BorrowKind::Mut, sub))];
                    r.blocks.push(block(&r, stmts, kind));
                }
            }
            next = BasicBlock(r.blocks.len() as u32 - 1);
        }
        values.push((k as u128 + 1, next));
    }
    r.blocks[0].terminator.kind = TerminatorKind::SwitchInt {
        discr: Operand::Copy(r.state()),
        targets: SwitchTargets {
            values,
            otherwise: BasicBlock(1),
        },
    };
    let mut instance = body.instance.clone();
    instance.name = drop_frame_name(name);
    Ok(Body {
        instance,
        locals: r.locals,
        arg_count: 1,
        blocks: r.blocks,
        scopes: body.scopes.clone(),
        phase: body.phase,
        span: body.span,
        par_regions: Vec::new(),
        suspends: false,
        drop_flags: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::parse_module;

    const SRC: &str = "
fn leaf() -> () suspends {
    let mut _0: ();
    let _1: ();
    bb0: {
        _1 = __yield_now() -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn mid(_1: i64) -> i64 {
    let mut _0: i64;
    let _2: i64;
    let _3: ();
    let _4: ref i64;
    let _5: i64;
    bb0: {
        _2 = Add(copy _1, const 1_i64);
        _4 = &_2;
        _3 = leaf() -> bb1;
    }
    bb1: {
        _5 = copy (*_4);
        _0 = Add(copy _5, copy _1);
        return;
    }
}

fn plain(_1: i64) -> i64 {
    let mut _0: i64;
    bb0: {
        _0 = copy _1;
        return;
    }
}

fn top() -> i64 {
    let mut _0: i64;
    let _1: i64;
    bb0: {
        _1 = plain(const 2_i64) -> bb1;
    }
    bb1: {
        _0 = mid(copy _1) -> bb2;
    }
    bb2: {
        return;
    }
}
";

    #[test]
    fn coroutine_set_is_transitive_over_direct_calls() {
        let m = parse_module(SRC).unwrap();
        let set = coroutines(&m.bodies);
        let names: Vec<&str> = set.iter().map(|s| s.as_str()).collect();
        assert_eq!(names, ["leaf", "mid", "top"]);
        assert!(recursive_cycles(&m.bodies, &set).is_empty());
    }

    #[test]
    fn frame_holds_what_is_live_across_and_what_is_borrowed() {
        let m = parse_module(SRC).unwrap();
        let set = coroutines(&m.bodies);
        let mid = layout(m.body("mid").unwrap(), &set);
        assert_eq!(mid.points.len(), 1);
        let p = &mid.points[0];
        assert_eq!(
            (p.block, p.callee.as_str(), p.resume),
            (BasicBlock(0), "leaf", BasicBlock(1))
        );
        // `_1` and the reference `_4` are used after; `_3`, the destination,
        // is written by the resume.
        assert_eq!(p.across, [Local(1), Local(4)]);
        // `_2` is not used again, but `_4` points at it.
        assert_eq!(mid.frame, [Local(1), Local(2), Local(4)]);

        let top = layout(m.body("top").unwrap(), &set);
        // `plain` is not a coroutine, so only the call to `mid` suspends;
        // its argument is moved into the call and nothing else is live.
        assert_eq!(top.points.len(), 1);
        assert_eq!(top.points[0].callee, "mid");
        assert!(top.frame.is_empty(), "{:?}", top.frame);

        let leaf = layout(m.body("leaf").unwrap(), &set);
        assert_eq!(leaf.points[0].callee, "__yield_now");
        assert!(leaf.frame.is_empty());
        assert!(layout(m.body("plain").unwrap(), &set).points.is_empty());
    }

    #[test]
    fn recursive_coroutines_are_found() {
        let src = "
fn a() -> () suspends {
    let mut _0: ();
    bb0: {
        _0 = b() -> bb1;
    }
    bb1: {
        return;
    }
}

fn b() -> () {
    let mut _0: ();
    bb0: {
        _0 = a() -> bb1;
    }
    bb1: {
        return;
    }
}

fn c() -> () {
    let mut _0: ();
    bb0: {
        _0 = c() -> bb1;
    }
    bb1: {
        return;
    }
}
";
        let m = parse_module(src).unwrap();
        let set = coroutines(&m.bodies);
        // `c` recurses but never suspends, so it is not a coroutine.
        assert_eq!(
            set.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            recursive_cycles(&m.bodies, &set),
            [vec!["a".to_string(), "b".to_string()]]
        );
    }

    const LOOP: &str = "
fn leaf(_1: i64) -> i64 suspends {
    let mut _0: i64;
    let _2: ();
    let _3: ref i64;
    let _4: i64;
    bb0: {
        _3 = &_1;
        _2 = __yield_now() -> bb1;
    }
    bb1: {
        _4 = copy (*_3);
        _0 = Add(copy _4, const 10_i64);
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let _1: i64;
    let _2: bool;
    let _3: i64;
    let _4: ();
    bb0: {
        _1 = const 0_i64;
        goto -> bb1;
    }
    bb1: {
        _2 = Lt(copy _1, const 3_i64);
        switchInt(copy _2) -> [0: bb5, otherwise: bb2];
    }
    bb2: {
        _3 = leaf(copy _1) -> bb3;
    }
    bb3: {
        _4 = println(copy _3) -> bb4;
    }
    bb4: {
        _1 = Add(copy _1, const 1_i64);
        goto -> bb1;
    }
    bb5: {
        _0 = const ();
        return;
    }
}
";

    /// The executor runs the state machine to the output the synchronous
    /// run prints, resuming once per yield: a loop variable and a borrowed
    /// argument live in the frames across each one.
    #[test]
    fn executor_matches_the_synchronous_run() {
        use crate::mir::interp::{run, run_coroutines, Program};
        let m = parse_module(LOOP).unwrap();
        let prog = Program::from_module(&m);
        let sync = run(&prog, &m.tys, "main", vec![]);
        assert_eq!(sync.output, "10\n11\n12\n", "{:?}", sync.outcome);
        let tasks = run_coroutines(&prog, &m.tys, "main", vec![]);
        assert_eq!(tasks.output, sync.output, "{:?}", tasks.outcome);
        assert_eq!(tasks.exit_code(), Some(0), "{:?}", tasks.outcome);
        assert_eq!(tasks.resumes, 3);

        let (bodies, frames) = transform(&m.bodies, &m.tys).unwrap();
        let names: BTreeSet<&str> = bodies.iter().map(|b| b.instance.name.as_str()).collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "leaf.drop_frame",
                "leaf.resume",
                "main.drop_frame",
                "main.resume"
            ])
        );
        // `main` keeps `i` and `leaf`'s frame; `leaf` keeps its argument
        // and the reference to it.
        assert_eq!(
            frames["main"].fields.keys().copied().collect::<Vec<_>>(),
            [Local(1)]
        );
        assert_eq!(frames["main"].subs.len(), 1);
        assert_eq!(
            frames["leaf"].fields.keys().copied().collect::<Vec<_>>(),
            [Local(1), Local(3)]
        );
    }

    #[test]
    fn transform_refuses_recursive_coroutines() {
        let src = "
fn a() -> () suspends {
    let mut _0: ();
    bb0: {
        _0 = a() -> bb1;
    }
    bb1: {
        return;
    }
}
";
        let m = parse_module(src).unwrap();
        let e = transform(&m.bodies, &m.tys).unwrap_err();
        assert!(e.contains("recursive coroutines"), "{e}");
    }

    /// A value with a Drop body held across a yield is dropped once, after
    /// the resume, in both the caller's and the callee's frame.
    #[test]
    fn drops_across_a_yield_run_once() {
        use crate::mir::interp::{run, run_coroutines, Program};
        let src = "
struct R: Drop { id: i64 }

fn R.drop(_1: mut ref R) -> () {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = println(const \"drop\", copy (*_1).0) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn leaf(_1: R) -> () suspends {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = __yield_now() -> bb1;
    }
    bb1: {
        _2 = println(const \"leaf\", copy _1.0) -> bb2;
    }
    bb2: {
        drop(_1) -> bb3;
    }
    bb3: {
        _0 = const ();
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let _1: R;
    let _2: R;
    let _3: ();
    bb0: {
        _1 = R { const 8_i64 };
        _2 = R { const 7_i64 };
        _0 = leaf(move _2) -> bb1;
    }
    bb1: {
        _3 = println(const \"main\", copy _1.0) -> bb2;
    }
    bb2: {
        drop(_1) -> bb3;
    }
    bb3: {
        return;
    }
}
";
        let m = parse_module(src).unwrap();
        let prog = Program::from_module(&m);
        let sync = run(&prog, &m.tys, "main", vec![]);
        assert_eq!(
            sync.output, "leaf7\ndrop7\nmain8\ndrop8\n",
            "{:?}",
            sync.outcome
        );
        let tasks = run_coroutines(&prog, &m.tys, "main", vec![]);
        assert_eq!(tasks.output, sync.output, "{:?}", tasks.outcome);
        assert_eq!(tasks.exit_code(), Some(0), "{:?}", tasks.outcome);
        assert_eq!(tasks.resumes, 1);
    }

    const FLAGGED: &str = "
struct R: Drop { id: i64 }

fn R.drop(_1: mut ref R) -> () {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = println(const \"drop\", copy (*_1).0) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn consume(_1: R) -> () {
    let mut _0: ();
    bb0: {
        drop(_1) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn leaf(_1: R, _2: bool) -> () suspends {
    let mut _0: ();
    let _3: ();
    bb0: {
        switchInt(copy _2) -> [0: bb2, otherwise: bb1];
    }
    bb1: {
        _3 = consume(move _1) -> bb2;
    }
    bb2: {
        _3 = __yield_now() -> bb3;
    }
    bb3: {
        _3 = println(const \"resumed\") -> bb4;
    }
    bb4: {
        drop(_1) -> bb5;
    }
    bb5: {
        _0 = const ();
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let _1: R;
    let _2: R;
    bb0: {
        _1 = R { const 8_i64 };
        _2 = R { const 7_i64 };
        _0 = leaf(move _2, const MOVED) -> bb1;
    }
    bb1: {
        drop(_1) -> bb2;
    }
    bb2: {
        return;
    }
}
";

    /// A frame dropped while suspended drops what it owns at that point:
    /// the callee's frame first, then its own locals; a conditionally
    /// moved argument under the flag elaboration made for it.
    #[test]
    fn drop_frame_drops_a_suspended_frame() {
        use crate::mir::elaborate_drops;
        use crate::mir::interp::{run, run_coroutines, run_coroutines_cancelled, Program};
        for (moved, sync_out, cancelled_out) in [
            ("false", "resumed\ndrop7\ndrop8\n", "drop7\ndrop8\n"),
            ("true", "drop7\nresumed\ndrop8\n", "drop7\ndrop8\n"),
        ] {
            let mut m = parse_module(&FLAGGED.replace("MOVED", moved)).unwrap();
            for b in &mut m.bodies {
                elaborate_drops(b, &mut m.tys).unwrap();
            }
            let leaf = m.body("leaf").unwrap();
            assert_eq!(leaf.drop_flags.len(), 1, "{moved}: the flag for _1");
            let prog = Program::from_module(&m);
            let sync = run(&prog, &m.tys, "main", vec![]);
            assert_eq!(sync.output, sync_out, "{moved}: {:?}", sync.outcome);
            let tasks = run_coroutines(&prog, &m.tys, "main", vec![]);
            assert_eq!(tasks.output, sync_out, "{moved}: {:?}", tasks.outcome);
            assert_eq!(tasks.resumes, 1);
            let c = run_coroutines_cancelled(&prog, &m.tys, "main", vec![], 1);
            assert_eq!(c.output, cancelled_out, "{moved}: {:?}", c.outcome);
            assert_eq!(c.exit_code(), Some(0), "{moved}: {:?}", c.outcome);
        }
    }
}
