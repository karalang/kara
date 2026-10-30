//! `TaskGroup` tasks borrow the providers their closures need (core
//! semantics §14.9, B-2026-10-30-6): like a captured place, `mut ref` when
//! the task writes the resource and `ref` when it only reads it, from the
//! spawn until the group drops (§9.5). So while the group lives, a task
//! that writes `R` conflicts with any other task of it that uses `R`, with
//! a second run of its own spawn, and with the spawner's own uses of `R`;
//! and a spawner that writes `R` conflicts with a task that reads it.
//!
//! A task spawned into a group the body does not own (a borrowed group)
//! outlives the body's call, so its needs ride on the call: the caller
//! counts the call as a spawn into the group it passes.
//!
//! A provider-rooted resource's provider is always one a caller borrowed.
//! A program-rooted resource may be at its default, which tasks share
//! freely (two tasks may both print), so it is checked only in a body that
//! can run under an override of it: the closure a `with_provider[R]` call
//! is given, a body with a `providers` block binding `R`, and everything
//! those call or make (B-2026-10-30-10). A body that also runs at the
//! default is checked all the same, since its tasks race under the
//! override. The library's own bodies are not checked for an override.

use super::*;

/// A use of a resource's provider while a task of the same group holds it
/// in a way the use conflicts with.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskConflict {
    pub instance: String,
    /// The later use: a spawn of a second task (or a second run of the
    /// same spawn), or a use by the spawner.
    pub span: crate::token::Span,
    pub resource: String,
    /// The line of the spawn of the task holding the provider.
    pub task_line: usize,
    /// Whether the task holding the provider writes it; otherwise the
    /// later use does.
    pub task_writes: bool,
    pub kind: TaskConflictKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskConflictKind {
    /// A second task of the group.
    Task,
    /// The same spawn run again (in a loop) while its first task lives.
    Again,
    /// The spawner itself.
    Spawner,
}

impl TaskConflict {
    pub fn text(&self) -> String {
        let r = &self.resource;
        let line = self.task_line;
        let why = "core semantics §14.9: a task borrows the provider of each resource it \
                   uses, `mut ref` when it writes it, until its group drops";
        match (self.kind, self.task_writes) {
            (TaskConflictKind::Again, _) => format!(
                "this spawn starts a task that writes `{r}` each time it runs, and every task \
                 it starts may still run when the next one starts, so two tasks could write \
                 `{r}`'s provider at once ({why})"
            ),
            (TaskConflictKind::Task, true) => format!(
                "this task uses `{r}` while the task spawned at line {line}, which writes \
                 `{r}`, may still run ({why}); write `{r}` from one task only"
            ),
            (TaskConflictKind::Task, false) => format!(
                "this task writes `{r}` while the task spawned at line {line}, which uses \
                 `{r}`, may still run ({why}); write `{r}` from one task only"
            ),
            (TaskConflictKind::Spawner, true) => format!(
                "`{r}` is used here while the task spawned at line {line}, which writes it, \
                 may still run ({why}); use `{r}` before that task is spawned or after its \
                 group drops"
            ),
            (TaskConflictKind::Spawner, false) => format!(
                "`{r}` is written here while the task spawned at line {line}, which uses it, \
                 may still run ({why}); write `{r}` before that task is spawned or after its \
                 group drops"
            ),
        }
    }
}

/// One `TaskGroup.spawn` of the body whose task needs a provider.
struct Spawn {
    block: BasicBlock,
    /// The group's local, when the body owns the group; a borrowed group
    /// outlives the body.
    group: Option<u32>,
    /// The locals the task's closure passes through on its way to the
    /// spawn, whose making is the task's, not the spawner's.
    closure: Vec<u32>,
    needs: Needs,
}

/// The first resource both `a` and `b` use, one of them writing it: the
/// resource and whether `a` is the one that writes.
fn overlap(a: &Needs, b: &Needs) -> Option<(String, bool)> {
    a.iter().find_map(|(r, x)| {
        let y = b.get(r)?;
        (*x == Access::Writes || *y == Access::Writes).then(|| (r.clone(), *x == Access::Writes))
    })
}

/// Follow `p` back through single-assignment moves, copies and borrows.
fn chain(defs: &BTreeMap<u32, Place>, p: &Place) -> Vec<u32> {
    let mut out = vec![p.local.0];
    let mut at = p.local.0;
    while let Some(src) = defs.get(&at) {
        if out.contains(&src.local.0) {
            break;
        }
        at = src.local.0;
        out.push(at);
    }
    out
}

impl Analysis<'_> {
    /// Per instance: the program-rooted resources it can run under an
    /// override of.
    pub(super) fn overridden(&self) -> BTreeMap<String, BTreeSet<String>> {
        let rooted = |r: &str| {
            self.program
                .resources
                .get(r)
                .is_some_and(|d| d.default.is_some())
        };
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut work: Vec<(String, String)> = Vec::new();
        let mut add = |b: String, r: &str, work: &mut Vec<(String, String)>| {
            if out.entry(b.clone()).or_default().insert(r.to_string()) {
                work.push((b, r.to_string()));
            }
        };
        for (name, body) in &self.program.bodies {
            for bb in &body.blocks {
                if let TerminatorKind::Call {
                    func:
                        Operand::Const(Const {
                            kind: ConstKind::FnDef(inst),
                            ..
                        }),
                    args,
                    ..
                } = &bb.terminator.kind
                {
                    if let (Some(r), Some(f)) = (self.with_provider(&inst.name), args.get(1)) {
                        if rooted(r) {
                            for b in self.fn_value_bodies(body, f) {
                                add(b, r, &mut work);
                            }
                        }
                    }
                }
            }
            for p in body.provided.iter().filter(|p| rooted(&p.resource)) {
                add(name.clone(), &p.resource, &mut work);
            }
        }
        while let Some((b, r)) = work.pop() {
            let Some(body) = self.program.bodies.get(&b) else {
                continue;
            };
            for c in self.callees(body) {
                add(c, &r, &mut work);
            }
        }
        out
    }

    /// Per instance: the needs of the tasks its calls leave running in a
    /// group it was lent, given every body's provider needs.
    pub(super) fn escaping_tasks(
        &self,
        providers: &BTreeMap<String, Needs>,
        overridden: &BTreeMap<String, BTreeSet<String>>,
    ) -> BTreeMap<String, Needs> {
        let mut escapes: BTreeMap<String, Needs> = BTreeMap::new();
        loop {
            let mut changed = false;
            for (name, body) in &self.program.bodies {
                let mut e = Needs::new();
                for s in self.spawns(body, providers, &escapes, overridden) {
                    if s.group.is_none() {
                        need_all(&mut e, &s.needs);
                    }
                }
                if !e.is_empty() && escapes.get(name) != Some(&e) {
                    escapes.insert(name.clone(), e);
                    changed = true;
                }
            }
            if !changed {
                return escapes;
            }
        }
    }

    /// The spawns of `body` whose tasks need a provider-rooted resource's
    /// provider: its `TaskGroup.spawn` calls outside `par` branches, and
    /// its calls of bodies that leave such tasks in a group they are lent.
    fn spawns(
        &self,
        body: &Body,
        providers: &BTreeMap<String, Needs>,
        escapes: &BTreeMap<String, Needs>,
        overridden: &BTreeMap<String, BTreeSet<String>>,
    ) -> Vec<Spawn> {
        // `par` branches are tasks too, checked as branches (§11.2).
        let in_par: BTreeSet<BasicBlock> = body
            .par_regions
            .iter()
            .flat_map(|r| r.branches.iter().flatten().copied())
            .collect();
        let library = super::super::providers::is_library(body, self.program.user_source_len);
        let here = overridden.get(&body.instance.name).filter(|_| !library);
        let checked = |r: &String| {
            self.program
                .resources
                .get(r)
                .is_some_and(|d| d.default.is_none())
                || here.is_some_and(|o| o.contains(r))
        };
        let defs = single_defs(body);
        let owned = |p: &Place| {
            chain(&defs, p)
                .last()
                .copied()
                .filter(|l| matches!(body.locals[*l as usize].kind, LocalKind::User { .. }))
        };
        let tcx = self.tys.tcx();
        let is_group = |p: &Place| {
            let Ok(pt) = place_ty(body, self.tys, p) else {
                return false;
            };
            let mut t = pt.ty;
            while let SK::Ref(i) | SK::MutRef(i) = tcx.kind(t) {
                t = i;
            }
            tcx.adt_of(t)
                .is_some_and(|(adt, _)| matches!(adt.name.as_str(), "TaskGroup" | "IsolatingGroup"))
        };
        let mut out = Vec::new();
        for (i, bb) in body.blocks.iter().enumerate() {
            let block = BasicBlock(i as u32);
            let TerminatorKind::Call {
                func:
                    Operand::Const(Const {
                        kind: ConstKind::FnDef(inst),
                        ..
                    }),
                args,
                ..
            } = &bb.terminator.kind
            else {
                continue;
            };
            if in_par.contains(&block) {
                continue;
            }
            let mut needs = Needs::new();
            let (group, closure) = if inst.name == "TaskGroup.spawn" {
                let Some(f) = args.get(1) else {
                    continue;
                };
                for b in self.fn_value_bodies(body, f) {
                    for (r, a) in providers.get(&b).into_iter().flatten() {
                        if checked(r) {
                            need(&mut needs, r, *a);
                        }
                    }
                }
                let group = args.first().and_then(Operand::place).and_then(owned);
                let closure = f.place().map(|p| chain(&defs, p)).unwrap_or_default();
                (group, closure)
            } else if let Some(e) = escapes.get(&inst.name) {
                needs = e.clone();
                let group = args
                    .iter()
                    .filter_map(Operand::place)
                    .find(|p| is_group(p))
                    .and_then(owned);
                (group, Vec::new())
            } else {
                continue;
            };
            if !needs.is_empty() {
                out.push(Spawn {
                    block,
                    group,
                    closure,
                    needs,
                });
            }
        }
        out
    }

    /// The uses of provider-rooted resources in `body` that conflict with
    /// a task of a group the body owns or spawns into.
    pub(super) fn task_conflicts(
        &self,
        body: &Body,
        providers: &BTreeMap<String, Needs>,
        escapes: &BTreeMap<String, Needs>,
        overridden: &BTreeMap<String, BTreeSet<String>>,
    ) -> Vec<TaskConflict> {
        let spawns = self.spawns(body, providers, escapes, overridden);
        if spawns.is_empty() {
            return Vec::new();
        }
        let live = live_tasks(body, &spawns);
        let span_of = |b: BasicBlock| body.blocks[b.index()].terminator.source_info.span;
        let mut out: Vec<TaskConflict> = Vec::new();
        let mut push = |c: TaskConflict| {
            if !out
                .iter()
                .any(|o| o.span == c.span && o.resource == c.resource)
            {
                out.push(c);
            }
        };
        // A task started while another of the group lives.
        for (si, s) in spawns.iter().enumerate() {
            for &ti in &live[s.block.index()] {
                let t = &spawns[ti];
                let Some((resource, task_writes)) = overlap(&t.needs, &s.needs) else {
                    continue;
                };
                push(TaskConflict {
                    instance: body.instance.name.clone(),
                    span: span_of(s.block),
                    resource,
                    task_line: span_of(t.block).line,
                    task_writes,
                    kind: if ti == si {
                        TaskConflictKind::Again
                    } else {
                        TaskConflictKind::Task
                    },
                });
            }
        }
        // The spawner's own uses while a task lives: not the making of a
        // task's closure, nor a spawning call's own needs, which are its
        // tasks'.
        let made: BTreeSet<u32> = spawns.iter().flat_map(|s| s.closure.clone()).collect();
        let spawn_blocks: BTreeSet<BasicBlock> = spawns.iter().map(|s| s.block).collect();
        for (block, span, n, from) in self.needs_sites_made(body, providers) {
            match from {
                Some(l) if made.contains(&l) => continue,
                None if spawn_blocks.contains(&block) => continue,
                _ => {}
            }
            for &ti in &live[block.index()] {
                let t = &spawns[ti];
                let Some((resource, task_writes)) = overlap(&t.needs, &n) else {
                    continue;
                };
                push(TaskConflict {
                    instance: body.instance.name.clone(),
                    span,
                    resource,
                    task_line: span_of(t.block).line,
                    task_writes,
                    kind: TaskConflictKind::Spawner,
                });
            }
        }
        out
    }
}

/// For each block, the spawns (indices into `spawns`) whose task may be
/// running when the block starts: a task runs from its spawn until the
/// `Drop` of its group, or to the end of the body when the group is
/// borrowed.
fn live_tasks(body: &Body, spawns: &[Spawn]) -> Vec<BTreeSet<usize>> {
    let n = body.blocks.len();
    let mut live_in: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    let at: BTreeMap<BasicBlock, usize> = spawns
        .iter()
        .enumerate()
        .map(|(i, s)| (s.block, i))
        .collect();
    let mut work: Vec<usize> = (0..n).collect();
    while let Some(i) = work.pop() {
        let bb = &body.blocks[i];
        let mut out = live_in[i].clone();
        if let TerminatorKind::Drop { place, .. } = &bb.terminator.kind {
            if place.projection.is_empty() {
                out.retain(|&s| spawns[s].group != Some(place.local.0));
            }
        }
        if let Some(&s) = at.get(&BasicBlock(i as u32)) {
            out.insert(s);
        }
        for succ in bb.terminator.kind.successors() {
            let j = succ.index();
            let before = live_in[j].len();
            live_in[j].extend(out.iter().copied());
            if live_in[j].len() != before {
                work.push(j);
            }
        }
    }
    live_in
}
