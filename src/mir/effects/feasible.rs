//! The blocks of a body no execution reaches, because every way in passes a
//! `switchInt` on an enum's discriminant that an earlier switch on the same,
//! unchanged enum has already decided the other way (B-2026-11-05-5).
//!
//! Drop elaboration opens a partly moved enum with a switch on its
//! discriminant. After `x?` takes the `Err` payload out of a
//! `Result[D, E]`, the residual drop of the temporary switches again, and
//! its `Ok` arm drops the `D` that path never holds. The arm is dead, but
//! counted, it charged `D`'s `Drop` effects to the branch of a `par` that
//! drops nothing.
//!
//! The analysis is a forward dataflow over facts "discriminant of local `l`
//! is `v`" or "is none of `vs`", established on the edges of a switch whose
//! operand is a temporary read as `discriminant(l)`. A fact dies at any
//! write to `l` (through any projection), at a `SetDiscriminant` or a `Drop`
//! of it, and when it is a call's destination. A local whose address is
//! taken anywhere in the body (`Ref`) is never tracked, so nothing writes it
//! behind the analysis's back. A block is infeasible when no path reaches
//! it, given the facts, though an edge does; dropping its effects only drops
//! effects no run has.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::super::syntax::*;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Fact {
    Is(u128),
    IsNot(BTreeSet<u128>),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct State {
    /// The discriminant fact of each tracked enum local.
    facts: BTreeMap<u32, Fact>,
    /// A temporary that holds `discriminant(l)` of the enum local `l`, read
    /// while `l` was as it is now.
    temps: BTreeMap<u32, u32>,
}

impl State {
    fn kill(&mut self, l: u32) {
        self.facts.remove(&l);
        self.temps.remove(&l);
        self.temps.retain(|_, e| *e != l);
    }

    /// What holds on both of two paths.
    fn join(&self, other: &State) -> State {
        let mut facts = BTreeMap::new();
        for (l, a) in &self.facts {
            let Some(b) = other.facts.get(l) else {
                continue;
            };
            let met = match (a, b) {
                (Fact::Is(x), Fact::Is(y)) if x == y => Some(Fact::Is(*x)),
                (Fact::IsNot(s), Fact::IsNot(t)) => {
                    let both: BTreeSet<u128> = s.intersection(t).copied().collect();
                    (!both.is_empty()).then_some(Fact::IsNot(both))
                }
                (Fact::Is(x), Fact::IsNot(s)) | (Fact::IsNot(s), Fact::Is(x)) => {
                    (!s.contains(x)).then(|| Fact::IsNot(s.clone()))
                }
                _ => None,
            };
            if let Some(f) = met {
                facts.insert(*l, f);
            }
        }
        let temps = self
            .temps
            .iter()
            .filter(|(t, e)| other.temps.get(t) == Some(e))
            .map(|(t, e)| (*t, *e))
            .collect();
        State { facts, temps }
    }
}

/// The blocks of `body` that no execution reaches.
pub(crate) fn infeasible_blocks(body: &Body) -> BTreeSet<BasicBlock> {
    let n = body.blocks.len();
    if n == 0 {
        return BTreeSet::new();
    }
    let mut borrowed: BTreeSet<u32> = BTreeSet::new();
    for bb in &body.blocks {
        for s in &bb.statements {
            if let StatementKind::Assign(_, Rvalue::Ref(_, p)) = &s.kind {
                borrowed.insert(p.local.0);
            }
        }
    }
    let mut entry: Vec<Option<State>> = vec![None; n];
    entry[0] = Some(State::default());
    let mut work: VecDeque<usize> = VecDeque::from([0]);
    let mut queued = vec![false; n];
    queued[0] = true;
    while let Some(i) = work.pop_front() {
        queued[i] = false;
        let Some(mut st) = entry[i].clone() else {
            continue;
        };
        let data = &body.blocks[i];
        for s in &data.statements {
            match &s.kind {
                StatementKind::Assign(place, rv) => {
                    st.kill(place.local.0);
                    if let Rvalue::Discriminant(p) = rv {
                        if place.projection.is_empty()
                            && p.projection.is_empty()
                            && !borrowed.contains(&p.local.0)
                        {
                            st.temps.insert(place.local.0, p.local.0);
                        }
                    }
                }
                StatementKind::SetDiscriminant(place, _) | StatementKind::FreeBox(place) => {
                    st.kill(place.local.0)
                }
                StatementKind::StorageDead(l) => st.kill(l.0),
                _ => {}
            }
        }
        let mut out: Vec<(BasicBlock, State)> = Vec::new();
        match &data.terminator.kind {
            TerminatorKind::SwitchInt { discr, targets } => {
                let enum_local = match discr {
                    Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => {
                        st.temps.get(&p.local.0).copied()
                    }
                    _ => None,
                };
                match enum_local {
                    Some(e) => {
                        let known = st.facts.get(&e).cloned();
                        for (v, bb) in &targets.values {
                            let feasible = match &known {
                                Some(Fact::Is(k)) => k == v,
                                Some(Fact::IsNot(s)) => !s.contains(v),
                                None => true,
                            };
                            if feasible {
                                let mut s2 = st.clone();
                                s2.facts.insert(e, Fact::Is(*v));
                                out.push((*bb, s2));
                            }
                        }
                        let values: BTreeSet<u128> =
                            targets.values.iter().map(|(v, _)| *v).collect();
                        let otherwise = match &known {
                            Some(Fact::Is(k)) if values.contains(k) => None,
                            Some(Fact::Is(k)) => Some(Fact::Is(*k)),
                            Some(Fact::IsNot(s)) => {
                                Some(Fact::IsNot(s.union(&values).copied().collect()))
                            }
                            None => Some(Fact::IsNot(values)),
                        };
                        if let Some(f) = otherwise {
                            let mut s2 = st.clone();
                            s2.facts.insert(e, f);
                            out.push((targets.otherwise, s2));
                        }
                    }
                    None => {
                        for bb in targets.all_targets() {
                            out.push((bb, st.clone()));
                        }
                    }
                }
            }
            TerminatorKind::Call { destination, .. } => {
                st.kill(destination.local.0);
                for bb in data.terminator.kind.successors() {
                    out.push((bb, st.clone()));
                }
            }
            TerminatorKind::Drop { place, .. } => {
                st.kill(place.local.0);
                for bb in data.terminator.kind.successors() {
                    out.push((bb, st.clone()));
                }
            }
            kind => {
                for bb in kind.successors() {
                    out.push((bb, st.clone()));
                }
            }
        }
        for (bb, s) in out {
            let j = bb.index();
            let next = match &entry[j] {
                None => s,
                Some(old) => old.join(&s),
            };
            if entry[j].as_ref() != Some(&next) {
                entry[j] = Some(next);
                if !queued[j] {
                    queued[j] = true;
                    work.push_back(j);
                }
            }
        }
    }
    // Only what the facts rule out: a block no edge reaches at all is left
    // to whatever reaches it otherwise.
    let mut reached = vec![false; n];
    let mut stack = vec![0usize];
    reached[0] = true;
    while let Some(i) = stack.pop() {
        for bb in body.blocks[i].terminator.kind.successors() {
            if !reached[bb.index()] {
                reached[bb.index()] = true;
                stack.push(bb.index());
            }
        }
    }
    (0..n)
        .filter(|i| reached[*i] && entry[*i].is_none())
        .map(|i| BasicBlock(i as u32))
        .collect()
}
