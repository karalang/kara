//! Finds the inner arms of a loop's early-return guard, for codegen to
//! weigh as rare (B-2026-11-07-1).
//!
//! Kata 7's digit loop guards each step with `if result < min_div or
//! (result == min_div and digit < -8) { return 0 }`. The `and` lowers to a
//! branch on `result == min_div` whose arms meet in a `bool` the guard then
//! tests. LLVM's SimplifyCFG folds such a two-entry phi into a select when
//! the arm it would speculate is cheap enough, and at the phi-fold
//! threshold karac uses (6, `src/backend/mir_aot.rs`) it does: every digit
//! then computes `digit < -8` and the `and` (`add; cmp; ccmn; cset` on
//! arm64) for an arm that runs almost never, 1.12x the cycles on the M5.
//! SimplifyCFG leaves a branch alone when its weights call it predictable,
//! so the arm is weighed rare instead.
//!
//! The shape, on an elaborated body: an exit switch `X` in a loop, not the
//! loop's header, tests a `bool` local and one of its arms runs straight
//! (gotos only) out of the loop to `return`. A two-way switch `S` on a
//! `bool` local in the same loop is weighed when, following each arm with
//! the `bool` constants and copies it writes, one arm can reach `X`'s exit
//! and the other reaches `X` only with the local known to stay (or goes
//! back round the loop first). That arm, the one that can leave, is rare:
//! an early return is taken at most once per loop, and the other arm is
//! every remaining step. `S` itself never exits, so this never weighs a
//! guard's own test, only the short-circuit inside it, which is the branch
//! the phi fold removes. And `S` must test two computed integers of 32 bits
//! or more for equality (`==`, or `!=` with its equal arm the one that can
//! leave): an ordering test inside a guard is often a coin flip, where the
//! select is the better code, and so is a byte against a literal. Weighing
//! kata 729's `start < e and s < end { return false }` made its booking
//! scan 1.43x slower on x86, and kata 777's `s[i] == b'L' and i < j` 1.18x;
//! kata 7's `result == min_div` holds for one value in 2^32.

use super::counters::{dominators, natural_loops, predecessors};
use super::syntax::*;
use super::ty::{TyInterner, TyKind};

/// `(block, arm)`: the switch ending `block` takes `arm` rarely.
pub fn rare_exit_arms(body: &Body, tys: &TyInterner) -> Vec<(usize, BasicBlock)> {
    let preds = predecessors(body);
    let idom = dominators(body, &preds);
    let loops = natural_loops(body, &preds, &idom);
    let n = body.blocks.len();
    // The innermost loop of each block: the smallest that holds it.
    let mut inner: Vec<Option<usize>> = vec![None; n];
    let size: Vec<usize> = loops
        .iter()
        .map(|l| l.blocks.iter().filter(|&&x| x).count())
        .collect();
    for (li, lp) in loops.iter().enumerate() {
        for (slot, &held) in inner.iter_mut().zip(&lp.blocks) {
            if held && slot.is_none_or(|o| size[li] < size[o]) {
                *slot = Some(li);
            }
        }
    }
    let exits: Vec<Option<Exit>> = (0..n)
        .map(|b| exit_switch(body, &loops, &inner, b))
        .collect();
    let mut out = Vec::new();
    for s in 0..n {
        let Some(li) = inner[s] else { continue };
        if loops[li].header == s || exits[s].is_some() {
            continue;
        }
        let Some((d, t, f)) = bool_switch(&body.blocks[s].terminator.kind) else {
            continue;
        };
        // Only an equality is presumed rare: the arm where `a == b` holds.
        let Some(equal) = equality_arm(body, tys, &body.blocks[s], d, t, f) else {
            continue;
        };
        let walk = |arm: BasicBlock| {
            let mut w = Walk {
                body,
                exits: &exits,
                lp: &loops[li].blocks,
                header: loops[li].header,
                start: s,
                budget: 64,
                out: Outcome::default(),
            };
            w.go(arm.index(), &mut Vec::new(), &mut Vec::new());
            w.out
        };
        let (ot, of) = (walk(t), walk(f));
        let stays = |o: &Outcome| o.stay && !o.exit && !o.unknown;
        let leaves = |o: &Outcome| o.exit && !o.unknown;
        let (rare, common) = if equal == t { (ot, of) } else { (of, ot) };
        if stays(&common) && leaves(&rare) {
            out.push((s, equal));
        }
    }
    out
}

/// An exit switch: the `bool` it tests, and the value that leaves.
#[derive(Clone, Copy)]
struct Exit {
    local: Local,
    leaves_on: bool,
}

/// The arm of `data`'s switch on `d` (`t` when true, `f` when false)
/// taken when two values are equal, if the block computes `d` as `Eq` or
/// `Ne` of them.
fn equality_arm(
    body: &Body,
    tys: &TyInterner,
    data: &BasicBlockData,
    d: Local,
    t: BasicBlock,
    f: BasicBlock,
) -> Option<BasicBlock> {
    let s = data
        .statements
        .iter()
        .rev()
        .find(|s| matches!(&s.kind, StatementKind::Assign(p, _) if p.local == d))?;
    let StatementKind::Assign(p, Rvalue::BinaryOp(op, a, b)) = &s.kind else {
        return None;
    };
    // Two computed values of a wide integer type: a byte or a char against
    // a literal (`s[i] == b'L'`, kata 777) is a classification, often true.
    let wide = |o: &Operand| match o {
        Operand::Copy(q) | Operand::Move(q) if q.projection.is_empty() => {
            matches!(tys.kind(body.local(q.local).ty), TyKind::Int(it) if it.bits() >= 32)
        }
        _ => false,
    };
    if !p.projection.is_empty() || !wide(a) || !wide(b) {
        return None;
    }
    match op {
        BinOp::Eq => Some(t),
        BinOp::Ne => Some(f),
        _ => None,
    }
}

/// `(local, arm when true, arm when false)` of a two-way switch on a
/// plain `bool` local.
fn bool_switch(t: &TerminatorKind) -> Option<(Local, BasicBlock, BasicBlock)> {
    let TerminatorKind::SwitchInt {
        discr: Operand::Copy(d) | Operand::Move(d),
        targets,
    } = t
    else {
        return None;
    };
    if !d.projection.is_empty() {
        return None;
    }
    match targets.values.as_slice() {
        [(0, f)] => Some((d.local, targets.otherwise, *f)),
        [(1, t)] => Some((d.local, *t, targets.otherwise)),
        _ => None,
    }
}

fn exit_switch(
    body: &Body,
    loops: &[super::counters::Loop],
    inner: &[Option<usize>],
    b: usize,
) -> Option<Exit> {
    let li = inner[b]?;
    if loops[li].header == b {
        return None;
    }
    let (local, t, f) = bool_switch(&body.blocks[b].terminator.kind)?;
    let returns = |arm: BasicBlock| {
        let mut x = arm.index();
        for _ in 0..8 {
            if loops[li].blocks[x] {
                return false;
            }
            match &body.blocks[x].terminator.kind {
                TerminatorKind::Return => return true,
                TerminatorKind::Goto { target } => x = target.index(),
                _ => return false,
            }
        }
        false
    };
    let inside = |arm: BasicBlock| loops[li].blocks[arm.index()];
    match (returns(t), returns(f)) {
        (true, false) if inside(f) => Some(Exit {
            local,
            leaves_on: true,
        }),
        (false, true) if inside(t) => Some(Exit {
            local,
            leaves_on: false,
        }),
        _ => None,
    }
}

/// What following an arm can come to: leave through an exit switch, stay
/// in the loop for another step, or something this does not follow.
#[derive(Default)]
struct Outcome {
    exit: bool,
    stay: bool,
    unknown: bool,
}

struct Walk<'a> {
    body: &'a Body,
    exits: &'a [Option<Exit>],
    lp: &'a [bool],
    header: usize,
    start: usize,
    budget: usize,
    out: Outcome,
}

impl Walk<'_> {
    /// Follows block `b` with the `bool` locals `known`, having passed
    /// `path` on the way.
    fn go(&mut self, b: usize, known: &mut Vec<(Local, bool)>, path: &mut Vec<usize>) {
        if b == self.header || b == self.start || path.contains(&b) {
            self.out.stay = true;
            return;
        }
        if !self.lp[b] || self.budget == 0 {
            self.out.unknown = true;
            return;
        }
        self.budget -= 1;
        let saved = known.clone();
        let data = &self.body.blocks[b];
        for s in &data.statements {
            let (StatementKind::Assign(p, _) | StatementKind::SetDiscriminant(p, _)) = &s.kind
            else {
                continue;
            };
            let value = match &s.kind {
                StatementKind::Assign(p, Rvalue::Use(o)) if p.projection.is_empty() => match o {
                    Operand::Const(Const {
                        kind: ConstKind::Scalar(v @ (0 | 1)),
                        ty,
                    }) if self.body.local(p.local).ty == *ty => Some(*v == 1),
                    Operand::Copy(q) | Operand::Move(q) if q.projection.is_empty() => known
                        .iter()
                        .rev()
                        .find(|(l, _)| *l == q.local)
                        .map(|(_, v)| *v),
                    _ => None,
                },
                _ => None,
            };
            known.retain(|(l, _)| *l != p.local);
            if let Some(v) = value {
                known.push((p.local, v));
            }
        }
        let get = |l: Local, known: &Vec<(Local, bool)>| {
            known.iter().rev().find(|(k, _)| *k == l).map(|(_, v)| *v)
        };
        path.push(b);
        if let Some(x) = self.exits[b] {
            match get(x.local, known) {
                Some(v) if v == x.leaves_on => self.out.exit = true,
                Some(_) => self.out.stay = true,
                None => {
                    self.out.exit = true;
                    self.out.stay = true;
                }
            }
        } else {
            match &data.terminator.kind {
                TerminatorKind::Goto { target } => self.go(target.index(), known, path),
                TerminatorKind::Abort { .. } | TerminatorKind::Unreachable => {}
                t => match bool_switch(t) {
                    Some((l, tt, ff)) => match get(l, known) {
                        Some(true) => self.go(tt.index(), known, path),
                        Some(false) => self.go(ff.index(), known, path),
                        None => {
                            self.go(tt.index(), known, path);
                            self.go(ff.index(), known, path);
                        }
                    },
                    None => {
                        // A switch on anything else: every arm, knowing
                        // nothing new.
                        if matches!(t, TerminatorKind::SwitchInt { .. }) {
                            for s in t.successors() {
                                self.go(s.index(), known, path);
                            }
                        } else {
                            self.out.unknown = true;
                        }
                    }
                },
            }
        }
        path.pop();
        *known = saved;
    }
}

#[cfg(test)]
mod tests {
    use crate::mir::lower::build_source;

    /// How many arms of `f` in `src` are weighed rare.
    fn rare(src: &str) -> usize {
        let l = build_source(src).unwrap_or_else(|e| panic!("{e}"));
        let body = l
            .program
            .bodies
            .values()
            .find(|b| b.instance.name == "f")
            .expect("f");
        super::rare_exit_arms(body, &l.tys).len()
    }

    fn program(body: &str) -> String {
        format!(
            concat!(
                "fn f(x: i32) -> i32 {{\n",
                "    let mut x = x;\n",
                "    let mut r: i32 = 0i32;\n",
                "    let hi: i32 = 214748364i32;\n",
                "    while x != 0i32 {{\n",
                "        let d: i32 = x % 10i32;\n",
                "{body}",
                "        r = r * 10i32 + d;\n",
                "        x = x / 10i32;\n",
                "    }}\n",
                "    r\n",
                "}}\n",
                "fn main() {{\n",
                "    println(f(123));\n",
                "}}\n",
            ),
            body = body,
        )
    }

    /// Kata 7's two guards: the `and` inside each is the rare arm; the
    /// guards' own tests and the loop's are not weighed.
    #[test]
    fn the_and_inside_an_early_return_guard_is_rare() {
        let src = program(concat!(
            "        if r > hi or (r == hi and d > 7i32) { return 0i32; }\n",
            "        if r < 0i32 - hi or (r == 0i32 - hi and d < -8i32) { return 0i32; }\n",
        ));
        assert_eq!(rare(&src), 2);
    }

    /// No early return, a guard with no short-circuit inside it, or one
    /// whose inner test is an ordering has nothing to weigh.
    #[test]
    fn a_plain_guard_or_a_branch_that_stays_is_not_weighed() {
        assert_eq!(rare(&program("        if r > hi { return 0i32; }\n")), 0);
        assert_eq!(
            rare(&program(
                "        if r > hi or (r > 7i32 and d > 7i32) { return 0i32; }\n"
            )),
            0
        );
        assert_eq!(
            rare(&program(
                "        if r > hi or (r == hi and d > 7i32) { r = 0i32; }\n"
            )),
            0
        );
    }
}
