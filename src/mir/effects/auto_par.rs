//! Auto-par verdicts (M4 item 13): whether a loop or a run of statements
//! the builder recorded as a candidate ([`Body::auto_par_candidates`]) may
//! run in parallel, for the builder to lower as a `par for` or a statement
//! `par {}` (item 14).
//!
//! A candidate has the shape of a [`ParRegion`], so it gets the check a
//! written `par` gets (core §11.2): no two branches, or two iterations of
//! a loop, with effects or uses of one place that must keep their order.
//! Then the D6 rule: every effect must be known, so a branch with an
//! unknown callee (a call through a value, an `extern` fn, a native the
//! table lacks) is never moved; drops are charged where they happen, as
//! everywhere in this pass. Four more rules are the candidate's own:
//!
//! - **A loop may carry reductions.** An accumulator a loop writes as
//!   `acc = acc op e`, with `op` one of `+`, `*`, `&`, `|`, `^` and `acc`
//!   used nowhere else in the body (or a chain of one such `op` with `acc`
//!   a leaf of it, `acc = a op acc op b`, which regroups as exactly as
//!   one does), conflicts between iterations only
//!   through that statement. The verdict names it, and the builder makes
//!   each task fold its run of iterations from `op`'s identity and folds
//!   the results in iteration order. That regrouping is exact for `&`,
//!   `|`, `^` and wrapping `+` and `*`, but not for an operation checked
//!   for overflow: a run's partial result can overflow where the in-order
//!   one never does (B-2026-10-24-20). A checked `*` is therefore never a
//!   reduction (an earlier 0 keeps the in-order product at 0 while a later
//!   run's overflows, unsigned or not); a checked `+` is, and the builder
//!   makes a signed one's join exact by checking each run's smallest and
//!   largest prefix against the type's range in order (an `i128` sum, with
//!   no wider type to keep the prefixes in, it keeps in order). An
//!   unsigned sum's prefixes only grow, so a run overflows only where the
//!   in-order sum does. An accumulator may be written at several sites
//!   (`acc = acc + a` in one arm, `acc += b` in another), all of one `op`
//!   and each its only other use; the verdict names every site. The join
//!   then knows a run would overflow but not at which site, so it runs
//!   that run again in order, where the overflow names its site; a body
//!   that can panic does nothing others see, so the second run is not
//!   observable.
//! - **A loop may collect.** A `Vec` local whose only use in the body is
//!   one `v.push(e)` (under an `if` too) is a push reduction: each task
//!   pushes onto a `Vec` of its own, and the join appends them in
//!   iteration order, so `v` ends as the sequential loop leaves it, with
//!   no `#[par_order_free]` needed.
//! - **A statement reads what an earlier one binds.** A `let` in one
//!   statement is visible to the next, where a written `par {}` refuses it
//!   at resolution; here the binding is a local the first statement has to
//!   itself, so the place check does not see the write, and the verdict
//!   checks the dependence directly.
//! - **Nothing may run that a stop would have prevented.** A loop body
//!   that can leave early (a `break`, a `return`, a `?`) stays in order.
//!   A loop body that can panic stays in order when it does something
//!   others see (writes, sends, receives), since in order the iterations
//!   after the panic never do it. A statement that can panic keeps its
//!   order against one that does something others see, and one that can
//!   return early against one that also writes what the caller sees
//!   (through a reference, or a parameter).
//! - **A statement group must pay for its spawn** (the review thread's
//!   ruling for item 13, with katas 165, 222 and 2 as pins). Legacy lost
//!   most where it spawned a group inside a hot function: 318x slower on
//!   kata 165 and 152x on kata 222. The measure is static: a group runs in
//!   parallel only when at least two of its statements do real work (a
//!   loop of their own, or a call that loops or recurses), and only when
//!   it runs rarely: not inside a loop of its function, and not in a
//!   function that is recursive or reached from a loop. A loop's cost
//!   gate is that second half only (its blocks always lie on its own
//!   cycle): in a function that runs many times, its tasks would be
//!   spawned again and again (kata 52's recursive search ran 160x slower).
//!   Otherwise the builder sizes it.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::token::Span;

/// What a candidate is, and for a run of statements which of them run
/// together.
#[derive(Debug, Clone, PartialEq)]
pub enum VerdictKind {
    /// A sequential `for`: its iterations may run as tasks.
    Loop,
    /// A run of statements. Each group is a run of consecutive statements
    /// (indices into the candidate's branches) that lowers as one
    /// statement `par {}`; the statements in no group stay in order.
    /// Empty when the verdict is a refusal.
    Statements { groups: Vec<Vec<usize>> },
}

/// `local = local op e` in a loop body, or `local.push(e)` on a `Vec`
/// local: the iterations may run apart and fold their `e` into `local` in
/// iteration order. A local written at several sites (`acc = acc + a` in
/// one arm, `acc = acc + b` in another) has a reduction for each, all of
/// one operator.
#[derive(Debug, Clone, PartialEq)]
pub struct Reduction {
    pub local: Local,
    pub op: ReduceOp,
    /// The statement that writes `local` (for a push, the call's block and
    /// the index past its statements): its block and index, and its source
    /// span.
    pub at: (BasicBlock, usize),
    pub span: Span,
}

/// How a reduction folds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReduceOp {
    /// `acc = acc op e`.
    Op(BinOp),
    /// `acc.push(e)`: each run's elements, appended in iteration order.
    Push,
}

/// Why a candidate stays sequential.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// A branch has an unknown effect (D6); the reason names it.
    Unknown(String),
    /// Two branches, or two iterations, must keep their order.
    Conflict(ParConflict),
    /// Statement `second` uses `local`, which statement `first` binds.
    Dependence {
        first: usize,
        second: usize,
        local: String,
    },
    /// Branch `stops` may panic or return, and branch `visible` does
    /// something others can see (`effect`): in order, a stop before it
    /// would keep it from happening at all. Both are 0 for a loop (two
    /// iterations).
    Stops {
        stops: usize,
        visible: usize,
        effect: String,
    },
    /// A loop body that can leave the loop early (a `break`, a `return`,
    /// a `?`): the iterations after it must not run.
    Exit,
    /// Safe, but not worth a spawn; the reason says which rule.
    Cost(String),
}

/// The verdict on one candidate, keyed by its instance and span.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoParVerdict {
    pub instance: String,
    pub span: Span,
    pub kind: VerdictKind,
    /// The reductions a loop carries (none for a run of statements), or
    /// why the candidate stays sequential.
    pub verdict: Result<Vec<Reduction>, Refusal>,
}

/// What the cost gate knows about the program's bodies.
pub(super) struct Cost {
    /// The blocks of each body that lie on a cycle of its control flow,
    /// one set per cycle (strongly connected component).
    cycles: BTreeMap<String, Vec<BTreeSet<BasicBlock>>>,
    /// The bodies each block of a body can run.
    calls: BTreeMap<String, Vec<Vec<String>>>,
    /// Bodies that loop or recurse, or call one that does.
    heavy: BTreeSet<String>,
    /// Bodies that can run many times: recursive, called from a loop, or
    /// called from a body that is.
    hot: BTreeSet<String>,
}

impl Cost {
    pub(super) fn of(a: &Analysis) -> Cost {
        let names: Vec<&String> = a.program.bodies.keys().collect();
        let index: BTreeMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        let mut cycles = BTreeMap::new();
        let mut calls = BTreeMap::new();
        for n in &names {
            let body = &a.program.bodies[*n];
            let succ: Vec<Vec<usize>> = body
                .blocks
                .iter()
                .map(|bb| {
                    bb.terminator
                        .kind
                        .successors()
                        .iter()
                        .map(|s| s.index())
                        .collect()
                })
                .collect();
            let cyc: Vec<BTreeSet<BasicBlock>> = tarjan(&succ)
                .into_iter()
                .filter(|c| c.len() > 1 || succ[c[0]].contains(&c[0]))
                .map(|c| c.into_iter().map(|b| BasicBlock(b as u32)).collect())
                .collect();
            cycles.insert((*n).clone(), cyc);
            let per: Vec<Vec<String>> = body
                .blocks
                .iter()
                .map(|bb| a.block_callees(body, bb))
                .collect();
            calls.insert((*n).clone(), per);
        }
        let edges: Vec<Vec<usize>> = names
            .iter()
            .map(|n| {
                let mut out: Vec<usize> = calls[*n]
                    .iter()
                    .flatten()
                    .filter_map(|c| index.get(c.as_str()).copied())
                    .collect();
                out.sort_unstable();
                out.dedup();
                out
            })
            .collect();
        let mut heavy = BTreeSet::new();
        let mut hot = BTreeSet::new();
        // Callee components come first, so a caller sees its callees' weight.
        for scc in tarjan(&edges) {
            let recursive = scc.len() > 1 || edges[scc[0]].contains(&scc[0]);
            let loops = scc.iter().any(|&i| !cycles[names[i]].is_empty());
            let calls_heavy = scc
                .iter()
                .any(|&i| edges[i].iter().any(|&j| heavy.contains(names[j])));
            if recursive || loops || calls_heavy {
                heavy.extend(scc.iter().map(|&i| names[i].clone()));
            }
            if recursive {
                hot.extend(scc.iter().map(|&i| names[i].clone()));
            }
        }
        for n in &names {
            let looping: BTreeSet<BasicBlock> = cycles[*n].iter().flatten().copied().collect();
            for (b, cs) in calls[*n].iter().enumerate() {
                if looping.contains(&BasicBlock(b as u32)) {
                    hot.extend(
                        cs.iter()
                            .filter(|c| index.contains_key(c.as_str()))
                            .cloned(),
                    );
                }
            }
        }
        let mut work: Vec<String> = hot.iter().cloned().collect();
        while let Some(n) = work.pop() {
            for &j in &edges[index[n.as_str()]] {
                if hot.insert(names[j].clone()) {
                    work.push(names[j].clone());
                }
            }
        }
        Cost {
            cycles,
            calls,
            heavy,
            hot,
        }
    }

    /// Does branch `blocks` of a candidate in `name` do real work: a loop
    /// of its own, or a call into a body that loops or recurses?
    fn heavy_branch(&self, name: &str, blocks: &[BasicBlock]) -> bool {
        let own: BTreeSet<BasicBlock> = blocks.iter().copied().collect();
        self.cycles[name].iter().any(|c| c.is_subset(&own))
            || blocks.iter().any(|b| {
                self.calls[name]
                    .get(b.index())
                    .is_some_and(|cs| cs.iter().any(|c| self.heavy.contains(c)))
            })
    }

    /// Why a candidate in `name` covering `blocks` may run many times: in
    /// a hot body, or inside a loop of its own body (a cycle through one of
    /// its blocks and a block outside it).
    fn hot_reason(&self, name: &str, blocks: &BTreeSet<BasicBlock>) -> Option<String> {
        if self.hot.contains(name) {
            return Some(format!("{name} is recursive or reached from a loop"));
        }
        self.cycles[name]
            .iter()
            .any(|c| !c.is_disjoint(blocks) && !c.is_subset(blocks))
            .then(|| "it is inside a loop".to_string())
    }
}

/// The verdict on candidate `r` of `body`, given the body's sites.
pub(super) fn verdict(
    name: &str,
    body: &Body,
    r: &ParRegion,
    sites: &[Site],
    origins: &Origins,
    cost: &Cost,
) -> AutoParVerdict {
    let at: BTreeMap<BasicBlock, &EffectSet> =
        sites.iter().map(|s| (s.block, &s.effects)).collect();
    let sets: Vec<BTreeSet<BasicBlock>> = r
        .branches
        .iter()
        .map(|br| br.iter().copied().collect())
        .collect();
    // D6: a branch with an unknown effect is never moved.
    let unknown: Vec<Option<String>> = sets
        .iter()
        .map(|set| {
            set.iter()
                .filter_map(|b| at.get(b))
                .find(|e| e.unknown || !e.calls.is_empty())
                .map(|e| match e.why.iter().next() {
                    Some(why) => why.clone(),
                    None => "a call through a parameter".to_string(),
                })
        })
        .collect();
    // What a branch does that a stop before it would have prevented, and
    // whether it can stop: panic, return (a `?`), or for a loop body leave
    // by any way but its one way back to the header (a `break`).
    let visible: Vec<Option<String>> = sets
        .iter()
        .map(|set| {
            set.iter()
                .filter_map(|b| at.get(b))
                .flat_map(|e| e.effects.iter())
                .find(|e| matches!(e.verb, Verb::Writes | Verb::Sends | Verb::Receives))
                .map(Effect::display)
        })
        .collect();
    let panic: Vec<Option<String>> = sets
        .iter()
        .map(|set| {
            set.iter()
                .filter_map(|b| at.get(b))
                .flat_map(|e| e.effects.iter())
                .find(|x| x.verb == Verb::Panics)
                .map(Effect::display)
        })
        .collect();
    let panics: Vec<bool> = panic.iter().map(Option::is_some).collect();
    // A branch on its way out of the function (a `?`, a `return`) writes
    // the return place, which nothing else in a branch does.
    let returns: Vec<bool> = sets
        .iter()
        .map(|set| {
            set.iter().any(|b| {
                let bb = &body.blocks[b.index()];
                let ret = |p: &Place| p.local == Local::RETURN_PLACE;
                bb.statements
                    .iter()
                    .any(|st| matches!(&st.kind, StatementKind::Assign(dst, _) if ret(dst)))
                    || match &bb.terminator.kind {
                        TerminatorKind::Call { destination, .. } => ret(destination),
                        TerminatorKind::Return => true,
                        _ => false,
                    }
            })
        })
        .collect();
    // A loop body that can go anywhere but back to its header, other than
    // into a panic: a `break`.
    let breaks = {
        let diverges = |b: &BasicBlock| {
            matches!(
                body.blocks[b.index()].terminator.kind,
                TerminatorKind::Abort { .. } | TerminatorKind::Unreachable
            )
        };
        let outside: BTreeSet<BasicBlock> = sets[0]
            .iter()
            .flat_map(|b| body.blocks[b.index()].terminator.kind.successors())
            .filter(|s| !sets[0].contains(s) && !diverges(s))
            .collect();
        outside.len() > 1
    };
    let sides: Vec<Vec<(BasicBlock, EffectSet)>> = sets
        .iter()
        .map(|set| {
            set.iter()
                .filter_map(|b| at.get(b).map(|s| (*b, origins.privatize(s, set))))
                .collect()
        })
        .collect();
    let touched: Vec<Vec<PlaceAccess>> = r
        .branches
        .iter()
        .map(|br| place_accesses(body, r, br, origins))
        .collect();
    // A write the caller sees: through a reference, or to a parameter.
    let escapes: Vec<Option<String>> = touched
        .iter()
        .map(|t| {
            t.iter()
                .find(|a| {
                    a.how != Use::Read
                        && (a.place.projection.contains(&ProjElem::Deref)
                            || (1..=body.arg_count as u32).contains(&a.place.local.0))
                })
                .map(|a| format!("{} of {}", a.how.noun(), place_name(body, &a.place)))
        })
        .collect();
    // Branch `i` stopping first would have kept something branch `j` does
    // from being seen: a panic ends the program, so only an effect counts;
    // a return ends the function, so a write the caller sees counts too,
    // and so does a panic, which would end a run the return lets go on
    // (B-2026-10-24-35).
    let stopped = |i: usize, j: usize| -> Option<Refusal> {
        let seen = if returns[i] {
            visible[j]
                .clone()
                .or_else(|| escapes[j].clone())
                .or_else(|| panic[j].clone())
        } else if panics[i] {
            visible[j].clone()
        } else {
            None
        };
        seen.map(|effect| Refusal::Stops {
            stops: i,
            visible: j,
            effect,
        })
    };
    let alias = |a: &Key, b: &Key| origins.may_alias(a, b);
    let conflict = |touched: &[Vec<PlaceAccess>], i: usize, j: usize| {
        effect_conflict(&sides, i, j, &alias)
            .or_else(|| place_conflict(body, touched, i, j, origins))
            .map(|(first, second)| ParConflict {
                region: 0,
                line: r.span.line,
                branches: (i, j),
                first,
                second,
            })
    };
    match r.kind {
        ParKind::For { .. } => {
            let verdict = if let Some(why) = &unknown[0] {
                Err(Refusal::Unknown(why.clone()))
            } else if breaks || returns[0] {
                Err(Refusal::Exit)
            } else {
                let reductions = reductions(body, &sets[0], &touched[0]);
                let carried: BTreeSet<u32> = reductions.iter().map(|x| x.local.0).collect();
                let rest: Vec<Vec<PlaceAccess>> = vec![touched[0]
                    .iter()
                    .filter(|a| !carried.contains(&a.place.local.0))
                    .cloned()
                    .collect()];
                match conflict(&rest, 0, 0) {
                    Some(c) => Err(Refusal::Conflict(c)),
                    None => match stopped(0, 0) {
                        Some(why) => Err(why),
                        // Its tasks would be spawned again and again.
                        None if cost.hot.contains(name) => Err(Refusal::Cost(format!(
                            "{name} is recursive or reached from a loop"
                        ))),
                        None => Ok(reductions),
                    },
                }
            };
            AutoParVerdict {
                instance: name.to_string(),
                span: r.span,
                kind: VerdictKind::Loop,
                verdict,
            }
        }
        ParKind::Block => {
            let own: Vec<BTreeSet<u32>> = r
                .branches
                .iter()
                .zip(&sets)
                .map(|(br, set)| own_locals(body, r, br, set, origins))
                .collect();
            let mentions: Vec<BTreeSet<u32>> = sets
                .iter()
                .map(|set| {
                    branch_uses(body, set)
                        .iter()
                        .flat_map(|(_, _, p)| {
                            std::iter::once(p.local.0).chain(p.projection.iter().filter_map(|e| {
                                match e {
                                    ProjElem::Index(l) => Some(l.0),
                                    _ => None,
                                }
                            }))
                        })
                        .collect()
                })
                .collect();
            // Why statements `i` and `j` cannot run together.
            let apart = |i: usize, j: usize| -> Option<Refusal> {
                if let Some(why) = unknown[i].as_ref().or(unknown[j].as_ref()) {
                    return Some(Refusal::Unknown(why.clone()));
                }
                let (first, second) = (i.min(j), i.max(j));
                if let Some(&l) = own[first].intersection(&mentions[second]).next() {
                    return Some(Refusal::Dependence {
                        first,
                        second,
                        local: place_name(body, &Place::local(Local(l))),
                    });
                }
                conflict(&touched, i, j)
                    .map(Refusal::Conflict)
                    .or_else(|| stopped(i, j))
                    .or_else(|| stopped(j, i))
            };
            // Runs of consecutive statements no two of which are apart.
            let mut safe: Vec<Vec<usize>> = Vec::new();
            let mut first_refusal = None;
            let mut cur: Vec<usize> = Vec::new();
            for (k, u) in unknown.iter().enumerate() {
                let why = cur.iter().find_map(|&m| apart(m, k));
                if let Some(why) = why {
                    first_refusal.get_or_insert(why);
                    if cur.len() > 1 {
                        safe.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                }
                if let Some(u) = u {
                    first_refusal.get_or_insert(Refusal::Unknown(u.clone()));
                } else {
                    cur.push(k);
                }
            }
            if cur.len() > 1 {
                safe.push(cur);
            }
            // The cost gate.
            let mut groups = Vec::new();
            for g in safe {
                let blocks: BTreeSet<BasicBlock> =
                    g.iter().flat_map(|&k| sets[k].iter().copied()).collect();
                let refused = match cost.hot_reason(name, &blocks) {
                    Some(why) => Some(why),
                    None => {
                        let heavy = g
                            .iter()
                            .filter(|&&k| cost.heavy_branch(name, &r.branches[k]))
                            .count();
                        (heavy < 2).then(|| {
                            format!("{heavy} of its {} statements loop or recurse", g.len())
                        })
                    }
                };
                match refused {
                    Some(why) => {
                        first_refusal = Some(Refusal::Cost(why));
                    }
                    None => groups.push(g),
                }
            }
            let verdict = if groups.is_empty() {
                Err(first_refusal.unwrap_or_else(|| Refusal::Cost("one statement".to_string())))
            } else {
                Ok(Vec::new())
            };
            AutoParVerdict {
                instance: name.to_string(),
                span: r.span,
                kind: VerdictKind::Statements { groups },
                verdict,
            }
        }
    }
}

/// The accumulators loop body `set` carries as `acc = acc op e`, given its
/// uses of shared places: each written there and used nowhere else. The
/// operation may be a chain of one operator with `acc` a leaf of it, as in
/// `acc = acc + a + b` or `acc = a + acc + b`, which regroups as exactly as
/// one `op` does.
fn reductions(body: &Body, set: &BTreeSet<BasicBlock>, touched: &[PlaceAccess]) -> Vec<Reduction> {
    // How often the body reads each local, leaving out a checked
    // operation's overflow flag (field 1), which only its assert reads.
    let mut reads: BTreeMap<Local, usize> = BTreeMap::new();
    for (_, how, p) in branch_uses(body, set) {
        let flag = matches!(p.projection.first(), Some(ProjElem::Field(f, _)) if f.0 == 1);
        if matches!(how, Use::Read | Use::Move) && !flag {
            *reads.entry(p.local).or_default() += 1;
        }
    }
    let chain = Chain {
        body,
        set,
        reads: &reads,
    };
    // Every `acc = acc op e` the body has, by accumulator.
    let mut sites: BTreeMap<Local, Vec<Reduction>> = BTreeMap::new();
    for b in set {
        for (k, st) in body.blocks[b.index()].statements.iter().enumerate() {
            let StatementKind::Assign(dst, rv) = &st.kind else {
                continue;
            };
            if !dst.projection.is_empty() || dst.local == Local::RETURN_PLACE {
                continue;
            }
            let acc = dst.local;
            let Some(op) = chain.op(rv, acc) else {
                continue;
            };
            sites.entry(acc).or_default().push(Reduction {
                local: acc,
                op: ReduceOp::Op(op),
                at: (*b, k),
                span: st.source_info.span,
            });
        }
        if let Some(r) = chain.push(*b) {
            sites.entry(r.local).or_default().push(r);
        }
    }
    let mut out = Vec::new();
    for (acc, rs) in sites {
        // Used nowhere else: for `acc op e`, one read (the one in the
        // operation) and one write at each site, all of one operator; for
        // a push, its one mutable borrow, at one site.
        let uses: Vec<&PlaceAccess> = touched.iter().filter(|a| a.place.local == acc).collect();
        let count = |how: Use| uses.iter().filter(|a| a.how == how).count();
        let plain = uses.iter().all(|a| a.place.projection.is_empty());
        let one_op = rs.iter().all(|r| r.op == rs[0].op);
        let n = rs.len();
        let alone = match rs[0].op {
            ReduceOp::Op(_) => {
                uses.len() == 2 * n && count(Use::Read) == n && count(Use::Write) == n
            }
            ReduceOp::Push => n == 1 && uses.len() == 1 && count(Use::MutBorrow) == 1,
        };
        if plain && one_op && alone {
            out.extend(rs);
        }
    }
    out
}

/// A loop body's operations, read back from their temporaries.
struct Chain<'a> {
    body: &'a Body,
    set: &'a BTreeSet<BasicBlock>,
    reads: &'a BTreeMap<Local, usize>,
}

/// How far [`Chain`] follows temporaries, so a long chain is refused
/// rather than walked without end.
const CHAIN_HOPS: usize = 64;

impl Chain<'_> {
    /// The one statement in the body assigning local `t`.
    fn assignment(&self, t: Local) -> Option<&Rvalue> {
        let mut found = None;
        for b in self.set {
            for st in &self.body.blocks[b.index()].statements {
                if let StatementKind::Assign(dst, rv) = &st.kind {
                    if dst.local == t {
                        if found.is_some() || !dst.projection.is_empty() {
                            return None;
                        }
                        found = Some(rv);
                    }
                }
            }
        }
        found
    }

    /// The operation `rv` stands for: itself, or through copies of
    /// temporaries the body assigns once and reads once, the operation
    /// assigned to the first, with whether its value is field 0 of a
    /// checked operation's result.
    fn resolve<'r>(&'r self, mut rv: &'r Rvalue, hops: &mut usize) -> Option<(&'r Rvalue, bool)> {
        loop {
            *hops += 1;
            if *hops > CHAIN_HOPS {
                return None;
            }
            let Rvalue::Use(Operand::Copy(p) | Operand::Move(p)) = rv else {
                return Some((rv, false));
            };
            if self.reads.get(&p.local) != Some(&1) {
                return None;
            }
            match p.projection.as_slice() {
                [] => rv = self.assignment(p.local)?,
                [ProjElem::Field(f, _)] if f.0 == 0 => {
                    return Some((self.assignment(p.local)?, true));
                }
                _ => return None,
            }
        }
    }

    /// `v.push(e)` at the end of block `b`, with `v` a local `Vec` the
    /// call borrows mutably through a temporary of its own.
    fn push(&self, b: BasicBlock) -> Option<Reduction> {
        let t = &self.body.blocks[b.index()].terminator;
        let TerminatorKind::Call { func, args, .. } = &t.kind else {
            return None;
        };
        let Operand::Const(Const {
            kind: ConstKind::FnDef(f),
            ..
        }) = func
        else {
            return None;
        };
        if !(f.name.starts_with("Vec[") && f.name.ends_with("].push")) {
            return None;
        }
        let [Operand::Move(r), _] = args.as_slice() else {
            return None;
        };
        if !r.projection.is_empty() || self.reads.get(&r.local) != Some(&1) {
            return None;
        }
        let Rvalue::Ref(BorrowKind::Mut, v) = self.assignment(r.local)? else {
            return None;
        };
        if !v.projection.is_empty() || v.local == Local::RETURN_PLACE {
            return None;
        }
        Some(Reduction {
            local: v.local,
            op: ReduceOp::Push,
            at: (b, self.body.blocks[b.index()].statements.len()),
            span: t.source_info.span,
        })
    }

    /// `op` when `rv`, the value assigned to `acc`, is a chain of `op`
    /// with `acc` one of its leaves: one reducible operator throughout,
    /// all checked for overflow or none, and not a checked `*`, whose
    /// partial products can overflow where the in-order product never
    /// does (B-2026-10-24-20).
    fn op(&self, rv: &Rvalue, acc: Local) -> Option<BinOp> {
        let mut hops = 0;
        let (rv, field) = self.resolve(rv, &mut hops)?;
        let (op, checked) = match rv {
            Rvalue::BinaryOp(op, ..) if !field => (*op, false),
            Rvalue::CheckedBinaryOp(op, ..) if field && *op != BinOp::Mul => (*op, true),
            _ => return None,
        };
        let reducible = matches!(
            op,
            BinOp::Add | BinOp::Mul | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
        );
        (reducible && self.holds(rv, acc, op, checked, &mut hops)).then_some(op)
    }

    /// Whether `acc` is a leaf of link `rv` of an `op` chain: an operand
    /// itself, or below exactly one operand through more links.
    fn holds(&self, rv: &Rvalue, acc: Local, op: BinOp, checked: bool, hops: &mut usize) -> bool {
        let (x, y) = match rv {
            Rvalue::BinaryOp(o, x, y) if *o == op && !checked => (x, y),
            Rvalue::CheckedBinaryOp(o, x, y) if *o == op && checked => (x, y),
            _ => return false,
        };
        let below = |o: &Operand, hops: &mut usize| -> bool {
            match o {
                Operand::Copy(p) if *p == Place::local(acc) => true,
                Operand::Copy(p) | Operand::Move(p) if p.local != acc => {
                    let link = Rvalue::Use(o.clone());
                    match self.resolve(&link, hops) {
                        Some((next, field)) if field == checked => {
                            self.holds(next, acc, op, checked, hops)
                        }
                        _ => false,
                    }
                }
                _ => false,
            }
        };
        below(x, hops) != below(y, hops)
    }
}

#[cfg(test)]
// Each branch is a range of source lines, and a one-branch loop body is
// written as a one-range slice on purpose.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use std::ops::RangeInclusive;

    /// The verdicts on `src` with every written `par` region also a
    /// candidate, which is the shape the builder records for a sequential
    /// `for` and a run of statements. A written `par for` runs its
    /// iterations as tasks (M4 item 14), so it stands for the plain loop
    /// in its place, whose candidate the builder records.
    fn written(src: &str) -> Vec<AutoParVerdict> {
        let plain = src.replace("par for ", "    for ");
        let loops: BTreeSet<usize> = src.match_indices("par for ").map(|(i, _)| i + 4).collect();
        let mut l =
            crate::mir::lower::build_source_recording(&plain).unwrap_or_else(|e| panic!("{e}"));
        for b in l.program.bodies.values_mut() {
            b.auto_par_candidates
                .retain(|c| loops.contains(&c.span.offset));
            let blocks = b.par_regions.clone();
            b.auto_par_candidates.extend(blocks);
        }
        analyze(&l.program, &l.tys).auto_par
    }

    /// The verdicts on `src` with one candidate in `name` made by source
    /// line: each branch the blocks with a statement or terminator on its
    /// lines, a block on two branches' lines going to the first.
    fn by_lines(
        src: &str,
        name: &str,
        kind: ParKind,
        branches: &[RangeInclusive<usize>],
    ) -> AutoParVerdict {
        let mut l = crate::mir::lower::build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let body = l.program.bodies.get_mut(name).expect("no such body");
        let mut taken = BTreeSet::new();
        let mut out = Vec::new();
        for lines in branches {
            let mut br = Vec::new();
            for (i, bb) in body.blocks.iter().enumerate() {
                let on = bb
                    .statements
                    .iter()
                    .map(|s| s.source_info.span.line)
                    .chain([bb.terminator.source_info.span.line])
                    .any(|n| lines.contains(&n));
                if on && taken.insert(i) {
                    br.push(BasicBlock(i as u32));
                }
            }
            out.push(br);
        }
        body.auto_par_candidates = vec![ParRegion {
            kind,
            span: body.span,
            branches: out,
        }];
        let mut v = analyze(&l.program, &l.tys).auto_par;
        assert_eq!(v.len(), 1);
        v.remove(0)
    }

    fn only<'a>(v: &'a [AutoParVerdict], name: &str) -> &'a AutoParVerdict {
        let mine: Vec<&AutoParVerdict> = v.iter().filter(|x| x.instance == name).collect();
        assert_eq!(mine.len(), 1, "{name}: {v:#?}");
        mine[0]
    }

    fn groups(v: &AutoParVerdict) -> Vec<Vec<usize>> {
        match &v.kind {
            VerdictKind::Statements { groups } => groups.clone(),
            VerdictKind::Loop => panic!("a loop: {v:#?}"),
        }
    }

    fn cost_reason(v: &AutoParVerdict) -> String {
        match &v.verdict {
            Err(Refusal::Cost(why)) => why.clone(),
            other => panic!("not a cost refusal: {other:?}"),
        }
    }

    const WORK: &str = "fn work(v: ref Vec[i64]) -> i64 {
    let mut s = 0;
    for x in v.iter() {
        s = s + *x;
    }
    s
}
";

    /// A loop that sums into one accumulator, by `+=` or spelled out, is a
    /// reduction; reading the accumulator anywhere else in the body, or
    /// printing, orders the iterations.
    #[test]
    fn auto_par_loop_reductions_and_conflicts() {
        let v = written(
            "fn squares() -> i64 {
    let mut sum = 0;
    par for i in 0..10 {
        sum += i * i;
    }
    sum
}

fn bits() -> i64 {
    let mut acc = 0;
    par for i in 0..10 {
        acc = acc ^ (i * 7);
    }
    acc
}

fn peeks() -> i64 {
    let mut sum = 0;
    let mut seen = 0;
    par for i in 0..10 {
        sum += i;
        seen = sum;
    }
    seen
}

fn prints() {
    par for i in 0..3 {
        println(i);
    }
}

fn main() {
    println(squares());
    println(bits());
    println(peeks());
    prints();
}
",
        );
        let sq = only(&v, "squares");
        assert_eq!(sq.kind, VerdictKind::Loop);
        let r = sq.verdict.as_ref().unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].op, ReduceOp::Op(BinOp::Add));
        assert_eq!(r[0].span.line, 4);
        let bits = only(&v, "bits")
            .verdict
            .as_ref()
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            bits.iter().map(|x| x.op).collect::<Vec<_>>(),
            [ReduceOp::Op(BinOp::BitXor)]
        );
        match &only(&v, "peeks").verdict {
            Err(Refusal::Conflict(c)) => assert_eq!(c.branches, (0, 0)),
            other => panic!("{other:?}"),
        }
        match &only(&v, "prints").verdict {
            Err(Refusal::Conflict(c)) => assert_eq!(c.first.1, "writes(Stdout)"),
            other => panic!("{other:?}"),
        }
    }

    /// B-2026-10-24-20: a product checked for overflow is not a reduction,
    /// since a run's partial product can overflow where the in-order one
    /// never does (after a 0, say), so its loop keeps its order; a checked
    /// sum still is.
    #[test]
    fn auto_par_checked_product_is_not_a_reduction() {
        let v = written(
            "fn factorial() -> u64 {
    let mut p: u64 = 1;
    par for i in 0..15 {
        p *= i as u64;
    }
    p
}

fn triangle() -> u64 {
    let mut t: u64 = 0;
    par for i in 0..15 {
        t += i as u64;
    }
    t
}

fn main() {
    println(factorial());
    println(triangle());
}
",
        );
        match &only(&v, "factorial").verdict {
            Err(Refusal::Conflict(c)) => assert_eq!(c.branches, (0, 0)),
            other => panic!("{other:?}"),
        }
        let t = only(&v, "triangle")
            .verdict
            .as_ref()
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            t.iter().map(|x| x.op).collect::<Vec<_>>(),
            [ReduceOp::Op(BinOp::Add)]
        );
    }

    /// A chain of one operator with the accumulator a leaf of it is a
    /// reduction wherever the leaf sits; a chain mixing operators is not,
    /// nor one whose partial result the body also reads.
    #[test]
    fn auto_par_operator_chain_is_a_reduction() {
        let v = written(
            "fn front() -> i64 {
    let mut s = 0;
    par for i in 0..10 {
        s = s + i + 1;
    }
    s
}

fn middle() -> i64 {
    let mut s = 0;
    par for i in 0..10 {
        s = i + s + 1 + i * 2;
    }
    s
}

fn masks() -> i64 {
    let mut m = 0;
    par for i in 0..10 {
        m = m | i | (i * 4);
    }
    m
}

fn mixed() -> i64 {
    let mut s = 0;
    par for i in 0..10 {
        s = (s ^ i) + 1;
    }
    s
}

fn seen() -> i64 {
    let mut s = 0;
    let mut last = 0;
    par for i in 0..10 {
        let t = s + i;
        s = t + 1;
        last ^= t;
    }
    s + last
}

fn main() {
    println(front());
    println(middle());
    println(masks());
    println(mixed());
    println(seen());
}
",
        );
        for (name, op) in [
            ("front", BinOp::Add),
            ("middle", BinOp::Add),
            ("masks", BinOp::BitOr),
        ] {
            let r = only(&v, name)
                .verdict
                .as_ref()
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(
                r.iter().map(|x| x.op).collect::<Vec<_>>(),
                [ReduceOp::Op(op)],
                "{name}"
            );
        }
        for name in ["mixed", "seen"] {
            match &only(&v, name).verdict {
                Err(Refusal::Conflict(c)) => assert_eq!(c.branches, (0, 0), "{name}"),
                other => panic!("{name}: {other:?}"),
            }
        }
    }

    /// D6: a call into C has unknown effects, so the loop stays in order
    /// even though nothing else conflicts.
    #[test]
    fn auto_par_unknown_callee_refuses() {
        let v = written(
            "unsafe extern \"C\" {
    fn labs(x: i64) -> i64;
}

fn main() {
    let mut sum = 0;
    par for i in 0..4 {
        sum += labs(i - 2);
    }
    println(sum);
}
",
        );
        assert!(
            matches!(only(&v, "main").verdict, Err(Refusal::Unknown(_))),
            "{v:#?}"
        );
    }

    /// A loop that can `break` stays in order, reduction or not: the
    /// iterations after the break must not run. One that can panic stays
    /// in order when an iteration does something others see, here a send
    /// on a channel of its own, which no other iteration conflicts with.
    #[test]
    fn auto_par_loops_that_stop_early_stay_in_order() {
        let brk = by_lines(
            "fn main() {
    let mut sum = 0;
    for i in 0..10 {
        if i == 5 {
            break;
        }
        sum += i;
    }
    println(sum);
}
",
            "main",
            ParKind::For { limit: None },
            &[4..=7],
        );
        assert_eq!(brk.verdict, Err(Refusal::Exit), "{brk:#?}");
        let v = written(
            "fn sends() {
    par for i in 0..10 {
        let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
        tx.send(i * i);
    }
}

fn main() {
    sends();
}
",
        );
        match &only(&v, "sends").verdict {
            Err(Refusal::Stops {
                stops: 0,
                visible: 0,
                effect,
            }) => assert!(effect.starts_with("sends(Channel"), "{effect}"),
            other => panic!("{other:?}"),
        }
    }

    /// A statement that can return early (a `?`) keeps the next one from
    /// writing what the caller sees; one that can panic keeps it from
    /// printing. (A written `par {}` runs every branch before it returns
    /// a branch's error, core §11.7, so the `?` case is a sequential run.)
    #[test]
    fn auto_par_statements_that_stop_early_keep_their_order() {
        let fill = by_lines(
            &format!(
                "{WORK}
fn first(v: ref Vec[i64]) -> Result[i64, String] {{
    if v.len() == 0 {{
        return Err(\"empty\");
    }}
    Ok(work(v))
}}

fn fill(out: mut ref Vec[i64], v: ref Vec[i64]) -> Result[i64, String] {{
    let a = first(v)?;
    out.push(work(v));
    Ok(a)
}}

fn main() {{
    let v: Vec[i64] = vec![1, 2, 3];
    let mut out: Vec[i64] = Vec.new();
    match fill(mut out, v) {{
        Ok(a) => println(a),
        Err(e) => println(e),
    }}
}}
"
            ),
            "fill",
            ParKind::Block,
            &[17..=17, 18..=18],
        );
        match &fill.verdict {
            Err(Refusal::Stops {
                stops: 0,
                visible: 1,
                effect,
            }) => assert_eq!(effect, "mutable borrow of out"),
            other => panic!("{other:?}"),
        }
        let v = written(&format!(
            "{WORK}
fn shout(v: ref Vec[i64]) -> i64 {{
    let mut a = 0;
    par {{
        a = work(v);
        println(\"done\");
    }}
    a
}}

fn main() {{
    let v: Vec[i64] = vec![1, 2, 3];
    println(shout(v));
}}
"
        ));
        match &only(&v, "shout").verdict {
            Err(Refusal::Stops {
                stops: 0,
                visible: 1,
                effect,
            }) => assert_eq!(effect, "writes(Stdout)"),
            other => panic!("{other:?}"),
        }
    }

    /// The statement-run verdicts on `src` for the candidates the builder
    /// records itself, one per function named.
    fn recorded_runs(src: &str) -> Vec<AutoParVerdict> {
        let l = crate::mir::lower::build_source_recording(src).unwrap_or_else(|e| panic!("{e}"));
        analyze(&l.program, &l.tys)
            .auto_par
            .into_iter()
            .filter(|v| matches!(v.kind, VerdictKind::Statements { .. }))
            .collect()
    }

    /// A statement whose whole arm ends in `return` still does its work
    /// before leaving: here it reads `v`, which the statement before it
    /// fills, and a binding the statement before it makes, so neither
    /// pair may run together (B-2026-10-24-34).
    #[test]
    fn auto_par_statement_that_returns_still_uses_what_it_reads() {
        let v = recorded_runs(
            "fn fill(v: mut ref Vec[i64], n: i64) {
    for i in 0..n {
        v.push(i);
    }
}

fn sum(v: ref Vec[i64]) -> i64 {
    let mut t = 0;
    for x in v.iter() {
        t = t + *x;
    }
    t
}

fn go(c: bool) -> i64 {
    let mut v: Vec[i64] = Vec.new();
    fill(mut v, 100);
    if c { let mut t = 0; for x in v.iter() { t = t + *x; }; return t; }
    0
}

fn near(c: bool) -> i64 {
    let w: Vec[i64] = vec![1, 2, 3];
    let p = sum(w);
    if c { let mut t = 0; for x in w.iter() { t = t + *x * p; }; return t; }
    0
}

fn main() {
    println(go(true));
    println(near(true));
}
",
        );
        for name in ["go", "near"] {
            let grouped: Vec<&AutoParVerdict> = v
                .iter()
                .filter(|r| r.instance == name && !groups(r).is_empty())
                .collect();
            assert!(grouped.is_empty(), "{grouped:#?}");
        }
    }

    /// A statement that may panic stays after one that returns: run in
    /// order, the return would have kept the panic from ending the run
    /// (B-2026-10-24-35).
    #[test]
    fn auto_par_statement_that_may_panic_stays_after_a_return() {
        let v = recorded_runs(
            "fn grow(n: i64) -> i64 {
    let mut s = 1;
    for i in 0..n {
        s = s * 3 + i;
    }
    s
}

fn second(lines: ref Vec[i64]) -> i64 {
    if grow(lines.len()) < 10 {
        return -1;
    }
    let g = grow(lines[1]);
    g
}

fn main() {
    let lines: Vec[i64] = vec![5];
    println(second(lines));
}
",
        );
        let mine: Vec<&AutoParVerdict> = v.iter().filter(|r| r.instance == "second").collect();
        assert!(mine.iter().all(|r| groups(r).is_empty()), "{mine:#?}");
        assert!(
            mine.iter().any(|r| matches!(
                &r.verdict,
                Err(Refusal::Stops { visible: 1, effect, .. }) if effect == "panics"
            )),
            "{mine:#?}"
        );
    }

    /// A sequential `for`, recorded by its body's lines as the builder
    /// records it, gets the verdict its `par for` twin gets.
    #[test]
    fn auto_par_sequential_for_is_a_reduction() {
        let v = by_lines(
            "fn main() {
    let mut sum = 0;
    for i in 0..10 {
        sum = sum + i * 3;
    }
    println(sum);
}
",
            "main",
            ParKind::For { limit: None },
            &[4..=4],
        );
        let r = v.verdict.unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].op, ReduceOp::Op(BinOp::Add));
    }

    /// Two statements that each loop run together; two that only do
    /// arithmetic stay in order, safe or not, because a spawn would cost
    /// more than they do.
    #[test]
    fn auto_par_statements_need_work_on_two_sides() {
        let v = written(&format!(
            "{WORK}
fn heavy(v: ref Vec[i64]) -> i64 {{
    let mut a = 0;
    let mut b = 0;
    par {{
        a = work(v);
        b = work(v);
    }}
    a + b
}}

fn light() -> i64 {{
    let mut a = 0;
    let mut b = 0;
    par {{
        a = 1 + 2;
        b = 3 * 4;
    }}
    a + b
}}

fn one_side(v: ref Vec[i64]) -> i64 {{
    let mut a = 0;
    let mut b = 0;
    par {{
        a = work(v);
        b = 3 * 4;
    }}
    a + b
}}

fn main() {{
    let v: Vec[i64] = vec![1, 2, 3];
    println(heavy(v));
    println(light());
    println(one_side(v));
}}
"
        ));
        let h = only(&v, "heavy");
        assert_eq!(h.verdict, Ok(Vec::new()), "{h:#?}");
        assert_eq!(groups(h), [vec![0, 1]]);
        let l = only(&v, "light");
        assert_eq!(groups(l), Vec::<Vec<usize>>::new());
        assert_eq!(cost_reason(l), "0 of its 2 statements loop or recurse");
        assert_eq!(
            cost_reason(only(&v, "one_side")),
            "1 of its 2 statements loop or recurse"
        );
    }

    /// The pins of the review thread's ruling: a group in a function
    /// called from a loop (kata 165's shape), in a recursive function
    /// (kata 222's), or inside a loop of its own function stays in order,
    /// however much work its statements do.
    #[test]
    fn auto_par_statements_in_hot_code_stay_in_order() {
        let v = written(&format!(
            "{WORK}
fn compare(v: ref Vec[i64]) -> i64 {{
    let mut a = 0;
    let mut b = 0;
    par {{
        a = work(v);
        b = work(v);
    }}
    a - b
}}

fn count(v: ref Vec[i64], n: i64) -> i64 {{
    if n == 0 {{
        return 0;
    }}
    let mut a = 0;
    let mut b = 0;
    par {{
        a = work(v);
        b = work(v);
    }}
    a + b + count(v, n - 1)
}}

fn looped(v: ref Vec[i64]) -> i64 {{
    let mut t = 0;
    for i in 0..3 {{
        let mut a = 0;
        let mut b = 0;
        par {{
            a = work(v);
            b = work(v);
        }}
        t = t + a + b + i;
    }}
    t
}}

fn main() {{
    let v: Vec[i64] = vec![1, 2, 3];
    let mut s = 0;
    for i in 0..1000 {{
        s = s + compare(v);
    }}
    println(s);
    println(count(v, 5));
    println(looped(v));
}}
"
        ));
        assert_eq!(
            cost_reason(only(&v, "compare")),
            "compare is recursive or reached from a loop"
        );
        assert_eq!(
            cost_reason(only(&v, "count")),
            "count is recursive or reached from a loop"
        );
        assert_eq!(cost_reason(only(&v, "looped")), "it is inside a loop");
    }

    /// A statement that uses what an earlier statement binds keeps its
    /// order, which a written `par {}` cannot even express (resolution
    /// refuses the read); the statements before it can still pair up.
    #[test]
    fn auto_par_statement_reading_an_earlier_binding_stays_after_it() {
        let src = format!(
            "{WORK}
fn main() {{
    let v: Vec[i64] = vec![1, 2, 3];
    let w: Vec[i64] = vec![4, 5];
    let a = work(v);
    let b = work(w);
    let c = a + work(v);
    println(f\"{{a}} {{b}} {{c}}\");
}}
"
        );
        let v = by_lines(&src, "main", ParKind::Block, &[12..=12, 13..=13, 14..=14]);
        assert_eq!(groups(&v), [vec![0, 1]], "{v:#?}");
        assert_eq!(v.verdict, Ok(Vec::new()));
        let v = by_lines(&src, "main", ParKind::Block, &[12..=12, 14..=14]);
        match &v.verdict {
            Err(Refusal::Dependence {
                first: 0,
                second: 1,
                local,
            }) => assert_eq!(local, "a"),
            other => panic!("{other:?}"),
        }
    }

    /// A push onto a `Vec` local the body uses for nothing else is a push
    /// reduction; reading it too (`parts.len()`) orders the iterations. An
    /// accumulator written at several sites of one operator is a reduction
    /// at each; mixing `*` and `+` on it is not.
    #[test]
    fn auto_par_push_and_multi_site_reductions() {
        let v = written(
            "fn collect() -> i64 {
    let mut parts: Vec[i64] = Vec.new();
    par for i in 0..10 {
        if i % 2 == 0 {
            parts.push(i * i);
        }
    }
    parts.len()
}

fn peeks() -> i64 {
    let mut parts: Vec[i64] = Vec.new();
    par for i in 0..10 {
        parts.push(i + parts.len());
    }
    parts.len()
}

fn arms() -> i64 {
    let mut sink = 0;
    par for i in 0..10 {
        if i % 3 == 0 {
            sink = sink + i;
        } else {
            sink += 1;
        }
    }
    sink
}

fn mixed() -> i64 {
    let mut m = 1;
    par for i in 0..10 {
        if i % 2 == 0 {
            m = m * 2;
        } else {
            m = m + 1;
        }
    }
    m
}

fn main() {
    println(collect() + peeks() + arms() + mixed());
}
",
        );
        let r = only(&v, "collect")
            .verdict
            .as_ref()
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(r.iter().map(|x| x.op).collect::<Vec<_>>(), [ReduceOp::Push]);
        assert_eq!(r[0].span.line, 5);
        let r = only(&v, "arms")
            .verdict
            .as_ref()
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            r.iter().map(|x| (x.op, x.span.line)).collect::<Vec<_>>(),
            [
                (ReduceOp::Op(BinOp::Add), 23),
                (ReduceOp::Op(BinOp::Add), 25)
            ]
        );
        for name in ["peeks", "mixed"] {
            match &only(&v, name).verdict {
                Err(Refusal::Conflict(c)) => assert_eq!(c.branches, (0, 0), "{name}"),
                other => panic!("{name}: {other:?}"),
            }
        }
    }
}
