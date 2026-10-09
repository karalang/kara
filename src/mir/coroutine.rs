//! The stackless coroutine transform (`docs/spikes/coroutines.md`, "The MIR
//! transform"): which bodies are coroutines, where they suspend, and which
//! locals the frame must hold across those points.
//!
//! A suspending call is an ordinary `Call` to every pass before this one, so
//! the analysis runs on elaborated, borrow-checked bodies and only reads them.

use std::collections::{BTreeMap, BTreeSet};

use super::borrowck::liveness;
use super::syntax::*;

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
}
