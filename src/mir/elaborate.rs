//! Drop elaboration (`docs/spikes/mir-types.md` §5): turns the builder's
//! scope-end `Drop`s, which may name a place that is moved on some
//! paths, into exactly the drops that are due on each path.
//!
//! 1. Move paths: every place that is moved, assigned or dropped, and
//!    every prefix of one, plus every local that needs dropping.
//! 2. Maybe-initialized and maybe-uninitialized sets per move path, by a
//!    forward dataflow over the CFG.
//! 3. Each `Drop` is classified: dead (deleted), static (kept),
//!    conditional (guarded by a drop flag) or open (replaced by drops of
//!    the parts, each classified again).
//! 4. Drop flags start false in the entry block; parameters start true.
//!
//! The output is in the `DropsElaborated` phase: every remaining `Drop`
//! names a place that is fully initialized whenever it runs, which the
//! MIR interpreter checks.

use std::collections::{BTreeMap, HashMap};

use super::place_ty::place_ty;
use super::syntax::*;
use super::ty::{IntTy, Ty, TyInterner, TyKind};

type PathIdx = usize;

struct MovePath {
    children: Vec<PathIdx>,
}

struct MovePaths {
    paths: Vec<MovePath>,
    by_place: HashMap<Place, PathIdx>,
}

impl MovePaths {
    fn lookup(&self, p: &Place) -> Option<PathIdx> {
        self.by_place.get(p).copied()
    }

    /// The path for `place`, creating it and its prefixes as needed.
    fn intern(&mut self, place: &Place) -> PathIdx {
        if let Some(i) = self.lookup(place) {
            return i;
        }
        let parent = if place.projection.is_empty() {
            None
        } else {
            let mut prefix = place.clone();
            prefix.projection.pop();
            Some(self.intern(&prefix))
        };
        let i = self.paths.len();
        self.paths.push(MovePath {
            children: Vec::new(),
        });
        if let Some(p) = parent {
            self.paths[p].children.push(i);
        }
        self.by_place.insert(place.clone(), i);
        i
    }

    /// `p` and all its descendants.
    fn subtree(&self, p: PathIdx) -> Vec<PathIdx> {
        let mut out = vec![p];
        let mut i = 0;
        while i < out.len() {
            out.extend(self.paths[out[i]].children.iter().copied());
            i += 1;
        }
        out
    }
}

/// Maybe-initialized and maybe-uninitialized, one bit per move path.
#[derive(Clone, PartialEq)]
struct State {
    init: Vec<bool>,
    uninit: Vec<bool>,
}

impl State {
    fn bottom(n: usize) -> State {
        State {
            init: vec![false; n],
            uninit: vec![false; n],
        }
    }

    fn join(&mut self, other: &State) -> bool {
        let mut changed = false;
        for (a, b) in self
            .init
            .iter_mut()
            .chain(self.uninit.iter_mut())
            .zip(other.init.iter().chain(other.uninit.iter()))
        {
            if *b && !*a {
                *a = true;
                changed = true;
            }
        }
        changed
    }
}

/// What a statement or terminator does to initialization, in order.
#[derive(Clone, Copy)]
enum Effect {
    Init(PathIdx),
    Uninit(PathIdx),
}

/// How a drop of one node is lowered.
enum Style {
    Dead,
    Static,
    Conditional,
    Open,
}

/// Elaborates the drops of a `Built` (or `Checked`) body in place and
/// moves it to `DropsElaborated`.
pub fn elaborate_drops(body: &mut Body, tys: &mut TyInterner) -> Result<(), String> {
    if body.phase >= MirPhase::DropsElaborated {
        return Err("drops are already elaborated".into());
    }
    let paths = gather_move_paths(body, tys)?;
    let n_orig = body.blocks.len();

    // Per-block effects of the statements (each statement's list) and of
    // the terminator.
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

    let entry = entry_state(body, &paths);
    let states = dataflow(body, &paths, &entry, &stmt_effects, &term_effects);

    let mut e = Elaborator {
        body,
        tys,
        paths,
        flags: BTreeMap::new(),
    };

    // Pass A: rewrite every original `Drop`, using the state just before it.
    let mut drop_clears: Vec<Option<(PathIdx, BasicBlock)>> = vec![None; n_orig];
    for bi in 0..n_orig {
        let TerminatorKind::Drop { place, target } = e.body.blocks[bi].terminator.kind.clone()
        else {
            continue;
        };
        let mut st = states[bi].clone();
        for effs in &stmt_effects[bi] {
            apply(&e.paths, &mut st, effs);
        }
        let Some(p) = e.paths.lookup(&place) else {
            // A place whose type needs no drop: nothing to do.
            e.body.blocks[bi].terminator.kind = TerminatorKind::Goto { target };
            continue;
        };
        // Every way out of the drop passes `clear`, which resets the flags
        // of the dropped paths once all flags are known.
        let clear = e.new_block(Vec::new(), TerminatorKind::Goto { target });
        let entry_bb = e.drop_node(&st, &place, Some(p), p, clear)?;
        e.body.blocks[bi].terminator.kind = TerminatorKind::Goto { target: entry_bb };
        drop_clears[bi] = Some((p, clear));
    }
    for (p, clear) in drop_clears.iter().flatten().copied().collect::<Vec<_>>() {
        let info = SourceInfo::dummy();
        let ups = e.flag_updates(Effect::Uninit(p), info);
        e.body.blocks[clear.index()].statements = ups;
    }

    // Pass B: keep the flags in step with every move and initialization.
    if !e.flags.is_empty() {
        for bi in 0..n_orig {
            let mut new_stmts = Vec::new();
            let old = std::mem::take(&mut e.body.blocks[bi].statements);
            for (si, s) in old.into_iter().enumerate() {
                let info = s.source_info;
                new_stmts.push(s);
                for eff in &stmt_effects[bi][si] {
                    new_stmts.extend(e.flag_updates(*eff, info));
                }
            }
            e.body.blocks[bi].statements = new_stmts;
            let info = e.body.blocks[bi].terminator.source_info;
            // A terminator's moves happen when it runs, so their flags are
            // cleared just before it; an initialization (a call's
            // destination) is recorded on the edge it returns along.
            let (before, after): (Vec<Effect>, Vec<Effect>) = term_effects[bi]
                .iter()
                .partition(|eff| matches!(eff, Effect::Uninit(_)));
            if drop_clears[bi].is_none() {
                for eff in before {
                    let ups = e.flag_updates(eff, info);
                    e.body.blocks[bi].statements.extend(ups);
                }
            }
            let ups: Vec<Statement> = after
                .iter()
                .flat_map(|eff| e.flag_updates(*eff, info))
                .collect();
            if !ups.is_empty() {
                if let TerminatorKind::Call {
                    target: Some(t), ..
                } = e.body.blocks[bi].terminator.kind
                {
                    let edge = e.new_block(ups, TerminatorKind::Goto { target: t });
                    if let TerminatorKind::Call { target, .. } =
                        &mut e.body.blocks[bi].terminator.kind
                    {
                        *target = Some(edge);
                    }
                }
            }
        }
        e.init_flags_at_entry();
    }

    e.body.phase = MirPhase::DropsElaborated;
    Ok(())
}

fn needs_drop_place(body: &Body, tys: &TyInterner, p: &Place) -> bool {
    match place_ty(body, tys, p) {
        Ok(pt) => tys.needs_drop(pt.ty),
        Err(_) => false,
    }
}

fn gather_move_paths(body: &Body, tys: &TyInterner) -> Result<MovePaths, String> {
    let mut mp = MovePaths {
        paths: Vec::new(),
        by_place: HashMap::new(),
    };
    for (i, decl) in body.locals.iter().enumerate().skip(1) {
        if tys.needs_drop(decl.ty) {
            mp.intern(&Place::local(Local(i as u32)));
        }
    }
    let mut add = |p: &Place| {
        if p.local != Local::RETURN_PLACE
            && !p.is_move_forbidden()
            && needs_drop_place(body, tys, p)
        {
            mp.intern(p);
        }
    };
    for block in &body.blocks {
        for s in &block.statements {
            if let StatementKind::Assign(dest, rv) = &s.kind {
                add(dest);
                for o in rvalue_operands(rv) {
                    if let Operand::Move(p) = o {
                        add(p);
                    }
                }
            }
        }
        match &block.terminator.kind {
            TerminatorKind::Drop { place, .. } => add(place),
            TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } => {
                add(destination);
                for o in std::iter::once(func).chain(args) {
                    if let Operand::Move(p) = o {
                        add(p);
                    }
                }
            }
            TerminatorKind::SwitchInt {
                discr: Operand::Move(p),
                ..
            } => add(p),
            _ => {}
        }
    }
    Ok(mp)
}

fn rvalue_operands(rv: &Rvalue) -> Vec<&Operand> {
    match rv {
        Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => vec![o],
        Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => vec![a, b],
        Rvalue::Aggregate(_, ops) => ops.iter().collect(),
        Rvalue::Ref(..) | Rvalue::Retain(_) | Rvalue::Discriminant(_) | Rvalue::Len(_) => {
            Vec::new()
        }
    }
}

fn moves<'o>(paths: &MovePaths, ops: impl IntoIterator<Item = &'o Operand>) -> Vec<Effect> {
    ops.into_iter()
        .filter_map(|o| match o {
            Operand::Move(p) => paths.lookup(p).map(Effect::Uninit),
            _ => None,
        })
        .collect()
}

fn statement_effects(paths: &MovePaths, s: &StatementKind) -> Vec<Effect> {
    match s {
        StatementKind::Assign(dest, rv) => {
            let mut v = moves(paths, rvalue_operands(rv));
            if let Some(p) = paths.lookup(dest) {
                v.push(Effect::Init(p));
            }
            v
        }
        StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => paths
            .lookup(&Place::local(*l))
            .map(Effect::Uninit)
            .into_iter()
            .collect(),
        StatementKind::SetDiscriminant(..) | StatementKind::Nop => Vec::new(),
    }
}

fn terminator_effects(paths: &MovePaths, t: &TerminatorKind) -> Vec<Effect> {
    match t {
        TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } => {
            let mut v = moves(paths, std::iter::once(func).chain(args));
            if let Some(p) = paths.lookup(destination) {
                v.push(Effect::Init(p));
            }
            v
        }
        TerminatorKind::SwitchInt { discr, .. } => moves(paths, [discr]),
        TerminatorKind::Drop { place, .. } => paths
            .lookup(place)
            .map(Effect::Uninit)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn apply(paths: &MovePaths, st: &mut State, effs: &[Effect]) {
    for eff in effs {
        let (p, init) = match *eff {
            Effect::Init(p) => (p, true),
            Effect::Uninit(p) => (p, false),
        };
        for q in paths.subtree(p) {
            st.init[q] = init;
            st.uninit[q] = !init;
        }
    }
}

/// Parameters are initialized at entry; every other path is not.
fn entry_state(body: &Body, paths: &MovePaths) -> State {
    let n = paths.paths.len();
    let mut st = State {
        init: vec![false; n],
        uninit: vec![true; n],
    };
    for a in body.args() {
        if let Some(p) = paths.lookup(&Place::local(a)) {
            apply(paths, &mut st, &[Effect::Init(p)]);
        }
    }
    st
}

/// The state at the entry of every block.
fn dataflow(
    body: &Body,
    paths: &MovePaths,
    entry: &State,
    stmt_effects: &[Vec<Vec<Effect>>],
    term_effects: &[Vec<Effect>],
) -> Vec<State> {
    let n = paths.paths.len();
    let mut states = vec![State::bottom(n); body.blocks.len()];
    states[0] = entry.clone();
    let mut work: Vec<usize> = (0..body.blocks.len()).rev().collect();
    let mut queued = vec![true; body.blocks.len()];
    while let Some(bi) = work.pop() {
        queued[bi] = false;
        let mut st = states[bi].clone();
        for effs in &stmt_effects[bi] {
            apply(paths, &mut st, effs);
        }
        apply(paths, &mut st, &term_effects[bi]);
        for s in body.blocks[bi].terminator.kind.successors() {
            if states[s.index()].join(&st) && !queued[s.index()] {
                queued[s.index()] = true;
                work.push(s.index());
            }
        }
    }
    states
}

struct Elaborator<'a> {
    body: &'a mut Body,
    tys: &'a mut TyInterner,
    paths: MovePaths,
    flags: BTreeMap<PathIdx, Local>,
}

impl Elaborator<'_> {
    fn new_block(&mut self, statements: Vec<Statement>, kind: TerminatorKind) -> BasicBlock {
        let b = BasicBlock(self.body.blocks.len() as u32);
        self.body.blocks.push(BasicBlockData {
            statements,
            terminator: Terminator {
                kind,
                source_info: SourceInfo::dummy(),
            },
        });
        b
    }

    fn new_local(&mut self, ty: Ty, kind: LocalKind) -> Local {
        let l = Local(self.body.locals.len() as u32);
        self.body.locals.push(LocalDecl {
            ty,
            mutability: Mutability::Mut,
            kind,
            source_info: SourceInfo::dummy(),
        });
        l
    }

    fn flag(&mut self, p: PathIdx) -> Local {
        if let Some(l) = self.flags.get(&p) {
            return *l;
        }
        let b = self.tys.bool();
        let l = self.new_local(b, LocalKind::DropFlag);
        self.flags.insert(p, l);
        l
    }

    fn set_flag(&mut self, l: Local, v: bool, info: SourceInfo) -> Statement {
        let b = self.tys.bool();
        Statement {
            kind: StatementKind::Assign(
                Place::local(l),
                Rvalue::Use(Operand::Const(Const {
                    ty: b,
                    kind: ConstKind::Scalar(v as u128),
                })),
            ),
            source_info: info,
        }
    }

    /// The flag assignments that mirror `eff`.
    fn flag_updates(&mut self, eff: Effect, info: SourceInfo) -> Vec<Statement> {
        let (p, v) = match eff {
            Effect::Init(p) => (p, true),
            Effect::Uninit(p) => (p, false),
        };
        let flagged: Vec<Local> = self
            .paths
            .subtree(p)
            .into_iter()
            .filter_map(|q| self.flags.get(&q).copied())
            .collect();
        flagged
            .into_iter()
            .map(|l| self.set_flag(l, v, info))
            .collect()
    }

    fn style(&self, st: &State, own: Option<PathIdx>, owner: PathIdx) -> Style {
        let (live, dead, count) = match own {
            Some(p) => {
                let sub = self.paths.subtree(p);
                let live = sub.iter().any(|&q| st.init[q]);
                let dead = sub.iter().any(|&q| st.uninit[q]);
                (live, dead, sub.len())
            }
            None => (st.init[owner], st.uninit[owner], 1),
        };
        match (live, dead, count) {
            (false, _, _) => Style::Dead,
            (true, false, _) => Style::Static,
            (true, true, 1) => Style::Conditional,
            (true, true, _) => Style::Open,
        }
    }

    /// Emits the code that drops `place` and then continues at `succ`, and
    /// returns its entry block. `own` is the place's move path, if it has
    /// one; otherwise its state is that of `owner`, its nearest tracked
    /// ancestor.
    fn drop_node(
        &mut self,
        st: &State,
        place: &Place,
        own: Option<PathIdx>,
        owner: PathIdx,
        succ: BasicBlock,
    ) -> Result<BasicBlock, String> {
        let flag_path = own.unwrap_or(owner);
        match self.style(st, own, owner) {
            Style::Dead => Ok(succ),
            Style::Static => Ok(self.new_block(
                Vec::new(),
                TerminatorKind::Drop {
                    place: place.clone(),
                    target: succ,
                },
            )),
            Style::Conditional => {
                let f = self.flag(flag_path);
                let d = self.new_block(
                    Vec::new(),
                    TerminatorKind::Drop {
                        place: place.clone(),
                        target: succ,
                    },
                );
                Ok(self.new_block(
                    Vec::new(),
                    TerminatorKind::SwitchInt {
                        discr: Operand::Copy(Place::local(f)),
                        targets: SwitchTargets::if_else(d, succ),
                    },
                ))
            }
            Style::Open => {
                let p = own.expect("only a tracked place can be opened");
                let entry = self.open(st, place, p, succ)?;
                // If the place itself may have been moved as a whole, its
                // parts are all dead on that path: skip them by its flag.
                if st.uninit[p] {
                    let f = self.flag(p);
                    Ok(self.new_block(
                        Vec::new(),
                        TerminatorKind::SwitchInt {
                            discr: Operand::Copy(Place::local(f)),
                            targets: SwitchTargets::if_else(entry, succ),
                        },
                    ))
                } else {
                    Ok(entry)
                }
            }
        }
    }

    /// Drops the parts of `place` (move path `p`) that are still owned:
    /// last to first (core semantics §6).
    fn open(
        &mut self,
        st: &State,
        place: &Place,
        p: PathIdx,
        succ: BasicBlock,
    ) -> Result<BasicBlock, String> {
        let ty = place_ty(self.body, self.tys, place)?.ty;
        if self.tys.has_drop_impl(ty) {
            return Err(format!(
                "{} is partly moved but has a Drop body (rejected by C4)",
                self.tys.display(ty)
            ));
        }
        match self.tys.kind(ty).clone() {
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => {
                let fields: Vec<(Place, Ty)> = ts
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (place.field(i as u32, *t), *t))
                    .collect();
                self.drop_fields(st, &fields, p, succ)
            }
            TyKind::Adt(a) if !self.tys.adt(a).is_enum => {
                let fields: Vec<(Place, Ty)> = self.tys.adt(a).variants[0]
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, (_, t))| (place.field(i as u32, *t), *t))
                    .collect();
                self.drop_fields(st, &fields, p, succ)
            }
            TyKind::Adt(a) => {
                let variants = self.tys.adt(a).variants.clone();
                let mut arms = Vec::new();
                for (v, var) in variants.iter().enumerate() {
                    let down = place.project(ProjElem::Downcast(VariantIdx(v as u32)));
                    let owner = self.paths.lookup(&down).unwrap_or(p);
                    let fields: Vec<(Place, Ty)> = var
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(i, (_, t))| (down.field(i as u32, *t), *t))
                        .collect();
                    let arm = self.drop_fields(st, &fields, owner, succ)?;
                    arms.push((v as u128, arm));
                }
                let unreachable = self.new_block(Vec::new(), TerminatorKind::Unreachable);
                let usize_t = self.tys.int(IntTy::Usize);
                let d = self.new_local(usize_t, LocalKind::Temp);
                let info = SourceInfo::dummy();
                Ok(self.new_block(
                    vec![Statement {
                        kind: StatementKind::Assign(
                            Place::local(d),
                            Rvalue::Discriminant(place.clone()),
                        ),
                        source_info: info,
                    }],
                    TerminatorKind::SwitchInt {
                        discr: Operand::Copy(Place::local(d)),
                        targets: SwitchTargets {
                            values: arms,
                            otherwise: unreachable,
                        },
                    },
                ))
            }
            _ => Err(format!(
                "cannot open a drop of {}: its parts are not separately movable",
                self.tys.display(ty)
            )),
        }
    }

    fn drop_fields(
        &mut self,
        st: &State,
        fields: &[(Place, Ty)],
        owner: PathIdx,
        succ: BasicBlock,
    ) -> Result<BasicBlock, String> {
        // Build back to front so the last field runs first.
        let mut next = succ;
        for (fp, fty) in fields {
            if !self.tys.needs_drop(*fty) {
                continue;
            }
            let own = self.paths.lookup(fp);
            next = self.drop_node(st, fp, own, owner, next)?;
        }
        Ok(next)
    }

    /// Makes `bb0` set every flag (false, or true for a parameter's paths)
    /// and jump to the old entry, which moves to a new block.
    fn init_flags_at_entry(&mut self) {
        let old_entry = std::mem::replace(
            &mut self.body.blocks[0],
            BasicBlockData {
                statements: Vec::new(),
                terminator: Terminator {
                    kind: TerminatorKind::Unreachable,
                    source_info: SourceInfo::dummy(),
                },
            },
        );
        let moved = BasicBlock(self.body.blocks.len() as u32);
        self.body.blocks.push(old_entry);
        for b in self.body.blocks.iter_mut() {
            retarget(&mut b.terminator.kind, |t| {
                if t == BasicBlock(0) {
                    moved
                } else {
                    t
                }
            });
        }
        let info = SourceInfo::dummy();
        let flags: Vec<(PathIdx, Local)> = self.flags.iter().map(|(p, l)| (*p, *l)).collect();
        let mut stmts: Vec<Statement> = flags
            .iter()
            .map(|(_, l)| self.set_flag(*l, false, info))
            .collect();
        for a in self.body.args().collect::<Vec<_>>() {
            if let Some(p) = self.paths.lookup(&Place::local(a)) {
                stmts.extend(self.flag_updates(Effect::Init(p), info));
            }
        }
        self.body.blocks[0] = BasicBlockData {
            statements: stmts,
            terminator: Terminator {
                kind: TerminatorKind::Goto { target: moved },
                source_info: info,
            },
        };
    }
}

fn retarget(t: &mut TerminatorKind, f: impl Fn(BasicBlock) -> BasicBlock) {
    match t {
        TerminatorKind::Goto { target } | TerminatorKind::Drop { target, .. } => {
            *target = f(*target)
        }
        TerminatorKind::SwitchInt { targets, .. } => {
            for (_, b) in targets.values.iter_mut() {
                *b = f(*b);
            }
            targets.otherwise = f(targets.otherwise);
        }
        TerminatorKind::Call { target, .. } => {
            if let Some(b) = target {
                *b = f(*b);
            }
        }
        TerminatorKind::Return | TerminatorKind::Abort { .. } | TerminatorKind::Unreachable => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::DefId;
    use crate::mir::build::BodyBuilder;
    use crate::mir::interp::{run, Event, Outcome, Program, Value};
    use crate::mir::ty::{AdtDef, AdtId, VariantDef};
    use crate::mir::validate::validate;

    fn inst(name: &str) -> InstanceId {
        InstanceId {
            def: DefId(0),
            args: Vec::new(),
            name: name.to_string(),
        }
    }

    struct World {
        tys: TyInterner,
        bodies: Vec<Body>,
        drop_impls: BTreeMap<AdtId, String>,
        i64t: Ty,
        unit: Ty,
        boolt: Ty,
        /// `struct R { id: i64 }`; its Drop body prints `drop <id>`.
        rt: Ty,
    }

    impl World {
        fn new() -> World {
            let mut tys = TyInterner::new();
            let i64t = tys.int(IntTy::I64);
            let unit = tys.unit();
            let boolt = tys.bool();
            let strt = tys.intern(TyKind::Str);
            let r = tys.add_adt(AdtDef {
                def: DefId(1),
                name: "R".into(),
                is_enum: false,
                variants: vec![VariantDef {
                    name: "R".into(),
                    fields: vec![("id".into(), i64t)],
                }],
                has_drop_impl: true,
                is_copy: false,
            });
            let rt = tys.intern(TyKind::Adt(r));
            let rref = tys.intern(TyKind::MutRef(rt));
            let mut w = World {
                tys,
                bodies: Vec::new(),
                drop_impls: BTreeMap::new(),
                i64t,
                unit,
                boolt,
                rt,
            };

            let mut b = BodyBuilder::new(inst("drop_R"), unit);
            let s = b.arg("self", rref);
            let t = b.temp(unit);
            let (bb0, bb1, bb2) = (b.new_block(), b.new_block(), b.new_block());
            let print = w.fn_op("print");
            let println = w.fn_op("println");
            b.terminate(
                bb0,
                TerminatorKind::Call {
                    func: print,
                    args: vec![Operand::Const(Const {
                        ty: strt,
                        kind: ConstKind::Str("drop ".into()),
                    })],
                    destination: t.into(),
                    target: Some(bb1),
                },
            );
            b.terminate(
                bb1,
                TerminatorKind::Call {
                    func: println,
                    args: vec![Operand::Copy(
                        Place::from(s).project(ProjElem::Deref).field(0, i64t),
                    )],
                    destination: t.into(),
                    target: Some(bb2),
                },
            );
            w.ret_unit(&mut b, bb2);
            w.bodies.push(b.finish().unwrap());
            w.drop_impls.insert(r, "drop_R".into());

            let mut b = BodyBuilder::new(inst("consume"), unit);
            let a = b.arg("r", rt);
            let (bb0, bb1) = (b.new_block(), b.new_block());
            b.terminate(
                bb0,
                TerminatorKind::Drop {
                    place: a.into(),
                    target: bb1,
                },
            );
            w.ret_unit(&mut b, bb1);
            w.bodies.push(b.finish().unwrap());
            w
        }

        fn fn_op(&mut self, name: &str) -> Operand {
            Operand::Const(Const {
                ty: self.unit,
                kind: ConstKind::FnDef(inst(name)),
            })
        }

        fn int(&self, v: i64) -> Operand {
            Operand::Const(Const {
                ty: self.i64t,
                kind: ConstKind::Scalar(v as u128),
            })
        }

        fn ret_unit(&self, b: &mut BodyBuilder, bb: BasicBlock) {
            b.assign(
                bb,
                Local::RETURN_PLACE,
                Rvalue::Use(Operand::Const(Const {
                    ty: self.unit,
                    kind: ConstKind::Unit,
                })),
            );
            b.terminate(bb, TerminatorKind::Return);
        }

        fn make_r(&self, b: &mut BodyBuilder, bb: BasicBlock, dest: impl Into<Place>, id: i64) {
            b.assign(
                bb,
                dest,
                Rvalue::Aggregate(
                    AggregateKind::Adt {
                        ty: self.rt,
                        variant: VariantIdx(0),
                    },
                    vec![self.int(id)],
                ),
            );
        }

        fn consume(&mut self, b: &mut BodyBuilder, bb: BasicBlock, arg: Place, next: BasicBlock) {
            let t = b.temp(self.unit);
            let f = self.fn_op("consume");
            b.terminate(
                bb,
                TerminatorKind::Call {
                    func: f,
                    args: vec![Operand::Move(arg)],
                    destination: t.into(),
                    target: Some(next),
                },
            );
        }

        fn program(&self, main: &Body, others: &[Body]) -> Program {
            let mut p = Program::default();
            for b in others {
                p.add(b.clone());
            }
            p.add(main.clone());
            p.drop_impls = self.drop_impls.clone();
            p
        }

        /// Runs `main` as built (the reference) and elaborated (every body),
        /// on each input, and requires the same output and the same user
        /// Drop bodies; returns the elaborated `main`.
        fn check(&mut self, main: Body, runs: &[(Vec<Value>, &str)]) -> Body {
            let built = self.program(&main, &self.bodies);
            let mut elaborated = Vec::new();
            for b in self.bodies.iter().chain(std::iter::once(&main)) {
                let mut b = b.clone();
                elaborate_drops(&mut b, &mut self.tys).unwrap();
                let errs = validate(&b, &self.tys);
                assert!(errs.is_empty(), "{}: {errs:?}", b.instance.name);
                elaborated.push(b);
            }
            let emain = elaborated.pop().unwrap();
            let after = self.program(&emain, &elaborated);
            let drop_bodies = |evs: &[Event]| {
                evs.iter()
                    .filter(|e| matches!(e, Event::DropBody(_)))
                    .count()
            };
            for (args, want) in runs {
                let a = run(&built, &self.tys, "main", args.clone());
                assert_eq!(a.outcome, Outcome::Returned(Value::Unit), "built {args:?}");
                assert_eq!(a.output, *want, "built {args:?}");
                let b = run(&after, &self.tys, "main", args.clone());
                assert_eq!(
                    b.outcome,
                    Outcome::Returned(Value::Unit),
                    "elaborated {args:?}:\n{}",
                    crate::mir::pretty_body(&emain, &self.tys)
                );
                assert_eq!(b.output, *want, "elaborated {args:?}");
                assert_eq!(drop_bodies(&a.events), drop_bodies(&b.events));
            }
            emain
        }
    }

    fn has_flag(b: &Body) -> bool {
        b.locals.iter().any(|l| l.kind == LocalKind::DropFlag)
    }

    fn drop_count(b: &Body) -> usize {
        b.blocks
            .iter()
            .filter(|bb| matches!(bb.terminator.kind, TerminatorKind::Drop { .. }))
            .count()
    }

    fn t(v: bool) -> Vec<Value> {
        vec![Value::Bool(v)]
    }

    #[test]
    fn mir_elab_conditional_move_gets_a_flag() {
        let mut w = World::new();
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let c = b.arg("c", w.boolt);
        let r = b.user_local("r", w.rt, Mutability::Not);
        let (bb0, bb1, bb2, bb3) = (b.new_block(), b.new_block(), b.new_block(), b.new_block());
        w.make_r(&mut b, bb0, r, 7);
        b.terminate(
            bb0,
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(c.into()),
                targets: SwitchTargets::if_else(bb1, bb2),
            },
        );
        w.consume(&mut b, bb1, r.into(), bb2);
        b.terminate(
            bb2,
            TerminatorKind::Drop {
                place: r.into(),
                target: bb3,
            },
        );
        w.ret_unit(&mut b, bb3);
        let body = b.finish().unwrap();
        let e = w.check(body, &[(t(true), "drop 7\n"), (t(false), "drop 7\n")]);
        assert!(has_flag(&e));
    }

    #[test]
    fn mir_elab_drop_after_unconditional_move_is_deleted() {
        let mut w = World::new();
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let r = b.user_local("r", w.rt, Mutability::Not);
        let (bb0, bb1, bb2) = (b.new_block(), b.new_block(), b.new_block());
        w.make_r(&mut b, bb0, r, 7);
        w.consume(&mut b, bb0, r.into(), bb1);
        b.terminate(
            bb1,
            TerminatorKind::Drop {
                place: r.into(),
                target: bb2,
            },
        );
        w.ret_unit(&mut b, bb2);
        let e = w.check(b.finish().unwrap(), &[(vec![], "drop 7\n")]);
        assert_eq!(drop_count(&e), 0);
        assert!(!has_flag(&e));
    }

    #[test]
    fn mir_elab_live_drop_stays_static() {
        let mut w = World::new();
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let r = b.user_local("r", w.rt, Mutability::Not);
        let (bb0, bb1) = (b.new_block(), b.new_block());
        w.make_r(&mut b, bb0, r, 7);
        b.terminate(
            bb0,
            TerminatorKind::Drop {
                place: r.into(),
                target: bb1,
            },
        );
        w.ret_unit(&mut b, bb1);
        let e = w.check(b.finish().unwrap(), &[(vec![], "drop 7\n")]);
        assert_eq!(drop_count(&e), 1);
        assert!(!has_flag(&e));
    }

    /// `t = (R(1), R(2))`, then parts of it moved, then `drop(t)`.
    fn tuple_main(
        w: &mut World,
        body: impl FnOnce(&mut World, &mut BodyBuilder, Local, Local, BasicBlock, BasicBlock),
        with_arg: bool,
    ) -> Body {
        let tup = w.tys.intern(TyKind::Tuple(vec![w.rt, w.rt]));
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let c = if with_arg {
            b.arg("c", w.boolt)
        } else {
            Local(0)
        };
        let t = b.user_local("t", tup, Mutability::Not);
        let (r1, r2) = (b.temp(w.rt), b.temp(w.rt));
        let (bb0, mid, end, ret) = (b.new_block(), b.new_block(), b.new_block(), b.new_block());
        w.make_r(&mut b, bb0, r1, 1);
        w.make_r(&mut b, bb0, r2, 2);
        b.assign(
            bb0,
            t,
            Rvalue::Aggregate(
                AggregateKind::Tuple,
                vec![Operand::Move(r1.into()), Operand::Move(r2.into())],
            ),
        );
        b.terminate(bb0, TerminatorKind::Goto { target: mid });
        body(w, &mut b, c, t, mid, end);
        b.terminate(
            end,
            TerminatorKind::Drop {
                place: t.into(),
                target: ret,
            },
        );
        w.ret_unit(&mut b, ret);
        b.finish().unwrap()
    }

    #[test]
    fn mir_elab_partial_move_opens_the_drop() {
        let mut w = World::new();
        let rt = w.rt;
        let main = tuple_main(
            &mut w,
            |w, b, _, t, mid, end| w.consume(b, mid, Place::from(t).field(0, rt), end),
            false,
        );
        let e = w.check(main, &[(vec![], "drop 1\ndrop 2\n")]);
        assert!(!has_flag(&e));
    }

    #[test]
    fn mir_elab_conditional_partial_move() {
        let mut w = World::new();
        let rt = w.rt;
        let main = tuple_main(
            &mut w,
            |w, b, c, t, mid, end| {
                let take = b.new_block();
                b.terminate(
                    mid,
                    TerminatorKind::SwitchInt {
                        discr: Operand::Copy(c.into()),
                        targets: SwitchTargets::if_else(take, end),
                    },
                );
                w.consume(b, take, Place::from(t).field(1, rt), end);
            },
            true,
        );
        let e = w.check(
            main,
            &[
                (t(true), "drop 2\ndrop 1\n"),
                (t(false), "drop 2\ndrop 1\n"),
            ],
        );
        assert!(has_flag(&e));
    }

    /// One branch moves the whole tuple, the other one part of it.
    #[test]
    fn mir_elab_whole_or_partial_move() {
        let mut w = World::new();
        let rt = w.rt;
        let main = tuple_main(
            &mut w,
            |w, b, c, t, mid, end| {
                let tup = b_local_ty(b, t);
                let u = b.user_local("u", tup, Mutability::Not);
                let (whole, part) = (b.new_block(), b.new_block());
                b.terminate(
                    mid,
                    TerminatorKind::SwitchInt {
                        discr: Operand::Copy(c.into()),
                        targets: SwitchTargets::if_else(whole, part),
                    },
                );
                b.assign(whole, u, Rvalue::Use(Operand::Move(t.into())));
                b.terminate(
                    whole,
                    TerminatorKind::Drop {
                        place: u.into(),
                        target: end,
                    },
                );
                w.consume(b, part, Place::from(t).field(1, rt), end);
            },
            true,
        );
        w.check(
            main,
            &[
                (t(true), "drop 2\ndrop 1\n"),
                (t(false), "drop 2\ndrop 1\n"),
            ],
        );
    }

    fn b_local_ty(b: &BodyBuilder, l: Local) -> Ty {
        b.local_ty(l)
    }

    /// `enum E { A(R, R), B(R) }`: a match arm moves `(e as A).0`, then
    /// the scope end drops `e`.
    #[test]
    fn mir_elab_enum_payload_move_opens_by_variant() {
        let mut w = World::new();
        let rt = w.rt;
        let ea = w.tys.add_adt(AdtDef {
            def: DefId(2),
            name: "E".into(),
            is_enum: true,
            variants: vec![
                VariantDef {
                    name: "A".into(),
                    fields: vec![("0".into(), rt), ("1".into(), rt)],
                },
                VariantDef {
                    name: "B".into(),
                    fields: vec![("0".into(), rt)],
                },
            ],
            has_drop_impl: false,
            is_copy: false,
        });
        let et = w.tys.intern(TyKind::Adt(ea));
        let usize_t = w.tys.int(IntTy::Usize);
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let c = b.arg("c", w.boolt);
        let e = b.user_local("e", et, Mutability::Not);
        let x = b.user_local("x", rt, Mutability::Not);
        let d = b.temp(usize_t);
        let (r1, r2, r3) = (b.temp(rt), b.temp(rt), b.temp(rt));
        let [bb0, mk_a, mk_b, matched, take, end, ret] = [(); 7].map(|_| b.new_block());
        b.terminate(
            bb0,
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(c.into()),
                targets: SwitchTargets::if_else(mk_a, mk_b),
            },
        );
        w.make_r(&mut b, mk_a, r1, 1);
        w.make_r(&mut b, mk_a, r2, 2);
        b.assign(
            mk_a,
            e,
            Rvalue::Aggregate(
                AggregateKind::Adt {
                    ty: et,
                    variant: VariantIdx(0),
                },
                vec![Operand::Move(r1.into()), Operand::Move(r2.into())],
            ),
        );
        b.terminate(mk_a, TerminatorKind::Goto { target: matched });
        w.make_r(&mut b, mk_b, r3, 3);
        b.assign(
            mk_b,
            e,
            Rvalue::Aggregate(
                AggregateKind::Adt {
                    ty: et,
                    variant: VariantIdx(1),
                },
                vec![Operand::Move(r3.into())],
            ),
        );
        b.terminate(mk_b, TerminatorKind::Goto { target: matched });
        b.assign(matched, d, Rvalue::Discriminant(e.into()));
        b.terminate(
            matched,
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(d.into()),
                targets: SwitchTargets {
                    values: vec![(0, take)],
                    otherwise: end,
                },
            },
        );
        let payload = Place::from(e)
            .project(ProjElem::Downcast(VariantIdx(0)))
            .field(0, rt);
        b.assign(take, x, Rvalue::Use(Operand::Move(payload)));
        w.consume(&mut b, take, x.into(), end);
        b.terminate(
            end,
            TerminatorKind::Drop {
                place: e.into(),
                target: ret,
            },
        );
        w.ret_unit(&mut b, ret);
        w.check(
            b.finish().unwrap(),
            &[(t(true), "drop 1\ndrop 2\n"), (t(false), "drop 3\n")],
        );
    }

    /// A loop that makes a fresh `R(i)` each time and moves it only on
    /// the first iteration; the flag must be re-set on every iteration.
    #[test]
    fn mir_elab_loop_reinitializes_the_flag() {
        let mut w = World::new();
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let i = b.user_local("i", w.i64t, Mutability::Mut);
        let r = b.user_local("r", w.rt, Mutability::Not);
        let (go, first) = (b.temp(w.boolt), b.temp(w.boolt));
        let [bb0, head, body_bb, take, latch, ret] = [(); 6].map(|_| b.new_block());
        b.assign(bb0, i, Rvalue::Use(w.int(0)));
        b.terminate(bb0, TerminatorKind::Goto { target: head });
        b.assign(
            head,
            go,
            Rvalue::BinaryOp(BinOp::Lt, Operand::Copy(i.into()), w.int(2)),
        );
        b.terminate(
            head,
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(go.into()),
                targets: SwitchTargets::if_else(body_bb, ret),
            },
        );
        b.assign(
            body_bb,
            r,
            Rvalue::Aggregate(
                AggregateKind::Adt {
                    ty: w.rt,
                    variant: VariantIdx(0),
                },
                vec![Operand::Copy(i.into())],
            ),
        );
        b.assign(
            body_bb,
            first,
            Rvalue::BinaryOp(BinOp::Eq, Operand::Copy(i.into()), w.int(0)),
        );
        b.terminate(
            body_bb,
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(first.into()),
                targets: SwitchTargets::if_else(take, latch),
            },
        );
        w.consume(&mut b, take, r.into(), latch);
        let step = b.new_block();
        b.terminate(
            latch,
            TerminatorKind::Drop {
                place: r.into(),
                target: step,
            },
        );
        b.assign(
            step,
            i,
            Rvalue::BinaryOp(BinOp::Add, Operand::Copy(i.into()), w.int(1)),
        );
        b.terminate(step, TerminatorKind::Goto { target: head });
        w.ret_unit(&mut b, ret);
        let e = w.check(b.finish().unwrap(), &[(vec![], "drop 0\ndrop 1\n")]);
        assert!(has_flag(&e));
    }

    /// A by-value parameter moved on one path: its flag starts true.
    #[test]
    fn mir_elab_parameter_flag_starts_true() {
        let mut w = World::new();
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let c = b.arg("c", w.boolt);
        let r = b.arg("r", w.rt);
        let (bb0, bb1, bb2, bb3) = (b.new_block(), b.new_block(), b.new_block(), b.new_block());
        b.terminate(
            bb0,
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(c.into()),
                targets: SwitchTargets::if_else(bb1, bb2),
            },
        );
        w.consume(&mut b, bb1, r.into(), bb2);
        b.terminate(
            bb2,
            TerminatorKind::Drop {
                place: r.into(),
                target: bb3,
            },
        );
        w.ret_unit(&mut b, bb3);
        let r7 = Value::Agg(vec![Value::Int(7)]);
        let e = w.check(
            b.finish().unwrap(),
            &[
                (vec![Value::Bool(true), r7.clone()], "drop 7\n"),
                (vec![Value::Bool(false), r7], "drop 7\n"),
            ],
        );
        assert!(has_flag(&e));
    }

    /// Elaborating the same body twice is refused rather than repeated.
    #[test]
    fn mir_elab_refuses_an_elaborated_body() {
        let mut w = World::new();
        let mut b = w.bodies[1].clone();
        elaborate_drops(&mut b, &mut w.tys).unwrap();
        assert!(elaborate_drops(&mut b, &mut w.tys).is_err());
    }
}
