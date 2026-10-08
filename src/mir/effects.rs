//! Sound effects on resolved calls (redesign C10, REDESIGN_PROPOSAL §4.10),
//! with the value-keyed ordering of D6 (DESIGN_REVIEW_2026-10-07).
//!
//! The pass runs over elaborated MIR, where every call names its callee
//! instance and every drop is an explicit terminator, so the four defaults
//! the legacy checker gets wrong are decided here by construction:
//!
//! 1. **Unknown callees have every effect.** A `Call` whose function is not
//!    a `FnDef` constant, a `FnDef` with no body and no native entry (an
//!    `extern` fn, which MIR has no effect clause for yet), and a library
//!    native missing from [`native_effects`] all give [`EffectSet::unknown`].
//!    The exception is a call through a function-typed *parameter* (core
//!    §12): the body records [`EffectSet::calls`] for it, and each caller
//!    substitutes the effects of the function it passes, the way keys are
//!    substituted. A call through a field or a local holding a function
//!    value is unknown.
//! 2. **Callees are keyed by instance**, never by name suffix: a body's
//!    callees are the `FnDef` instances its terminators name.
//! 3. **Drops are charged where they happen.** A `Drop(place)` adds the
//!    drop glue of the place's type: the type's own `Drop` body, then the
//!    glue of every part (fields, elements, captures), transitively.
//! 4. **Recursion is a fixpoint** over the call graph's strongly connected
//!    components, starting from no effects.
//!
//! **Keys (D6).** An effect names a resource class (`Stdout`, `Channel`,
//! `Network`, ...) and a [`Key`]: the value it acts on, as a parameter or a
//! local of the body plus a field path, or [`Key::Any`] for "some value of
//! the class". A callee's parameter keys are rewritten to the caller's
//! argument places at each call; a key that does not survive (an element of
//! a collection, a temporary, a body-local value in a summary) widens to
//! `Any`, which conflicts with every value of its class: sound, just less
//! precise. [`conflicts`] is the D6 rule: `reads`/`writes` and `writes`/
//! `writes` conflict on overlapping keys, and so do two `sends` or two
//! `receives`, which keeps two sends on one connection in source order;
//! `sends` with `receives` never conflicts (full duplex), and neither do the
//! capability verbs (`allocates`, `panics`, `blocks`).
//!
//! **Which values a key can name.** Two keys on distinct roots overlap when
//! [`Origins`] finds a value both may hold: a clone of a `Sender` names the
//! clone's channel, and every parameter may be any value the caller has.
//! A key on a value made inside a call (a local of the callee, a connection
//! a client call opens) is [`Key::Fresh`] in that call's summary and
//! overlaps nothing, and so are `Network` and `FileSystem` effects that no
//! value roots: those are capabilities, not conflict keys (core §12 item 5).
//!
//! **`par` branches (core §11.2).** Each [`ParRegion`] is checked after
//! the fixpoint: every pair of branches of a `par` block, and a `par for`
//! body against itself (two iterations), must have no two effects that
//! keep their order. A key on a value the branch makes itself is fresh
//! there, so one iteration's channel does not conflict with the next's.
//! The conflicts are reported in [`EffectReport::par_conflicts`].

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::interp::Program;
use super::place_ty::place_ty;
use super::syntax::*;
use super::ty::{AdtId, Ty, TyCtxt, TyInterner};
use crate::ids::DefId;
use crate::ty::TyKind as SK;

/// The effect verbs MIR can produce. `suspends` returns with coroutines (M4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verb {
    Reads,
    Writes,
    Sends,
    Receives,
    Allocates,
    Panics,
    Blocks,
}

impl Verb {
    pub fn name(self) -> &'static str {
        match self {
            Verb::Reads => "reads",
            Verb::Writes => "writes",
            Verb::Sends => "sends",
            Verb::Receives => "receives",
            Verb::Allocates => "allocates",
            Verb::Panics => "panics",
            Verb::Blocks => "blocks",
        }
    }
}

/// The value an effect acts on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    /// Some value of the class: overlaps every key.
    Any,
    /// The body's parameter (its MIR local index, from 1), then a path of
    /// field indices into it.
    Param(u32, Vec<u32>),
    /// A local of the body, then a field path. Never appears in a summary.
    Local(u32, Vec<u32>),
    /// A value no other key can name: one made inside the call this summary
    /// describes, or inside the `par` branch being checked, or a capability
    /// with no value at all (a path on the file system). Overlaps nothing.
    Fresh,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Effect {
    pub verb: Verb,
    /// The resource class; empty for `panics` and `blocks`.
    pub class: String,
    pub key: Key,
}

impl Effect {
    fn new(verb: Verb, class: &str, key: Key) -> Effect {
        Effect {
            verb,
            class: class.to_string(),
            key,
        }
    }

    /// `sends(Channel @ p1.0)`, `writes(Stdout)`, `panics`.
    pub fn display(&self) -> String {
        if self.class.is_empty() {
            return self.verb.name().to_string();
        }
        let key = match &self.key {
            Key::Any => String::new(),
            Key::Fresh => " @ fresh".to_string(),
            Key::Param(p, path) | Key::Local(p, path) => {
                let tag = if matches!(self.key, Key::Param(..)) {
                    "p"
                } else {
                    "l"
                };
                let mut s = format!(" @ {tag}{p}");
                for f in path {
                    s.push_str(&format!(".{f}"));
                }
                s
            }
        };
        format!("{}({}{key})", self.verb.name(), self.class)
    }
}

/// The effects of a body, a call site, or a drop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectSet {
    /// Has every effect (an unknown callee somewhere below); `why` says
    /// where.
    pub unknown: bool,
    pub why: BTreeSet<String>,
    pub effects: BTreeSet<Effect>,
    /// The parameters (MIR local indices) whose function value is called:
    /// each caller substitutes the effects of the argument it passes
    /// (core §12, D6). Never escapes a body as anything but a parameter.
    pub calls: BTreeSet<u32>,
}

impl EffectSet {
    fn unknown(why: String) -> EffectSet {
        EffectSet {
            unknown: true,
            why: BTreeSet::from([why]),
            effects: BTreeSet::new(),
            calls: BTreeSet::new(),
        }
    }

    fn add(&mut self, e: Effect) {
        self.effects.insert(e);
    }

    fn union(&mut self, other: EffectSet) {
        self.unknown |= other.unknown;
        self.why.extend(other.why);
        self.effects.extend(other.effects);
        self.calls.extend(other.calls);
    }

    fn is_empty(&self) -> bool {
        !self.unknown && self.effects.is_empty() && self.calls.is_empty()
    }

    pub fn display(&self) -> Vec<String> {
        let mut v: Vec<String> = self.effects.iter().map(Effect::display).collect();
        v.extend(self.calls.iter().map(|p| format!("calls(p{p})")));
        if self.unknown {
            v.insert(0, "unknown".to_string());
        }
        v
    }
}

/// The effects of one terminator, with keys local to its body.
#[derive(Debug, Clone)]
pub struct Site {
    pub block: BasicBlock,
    pub effects: EffectSet,
}

#[derive(Debug, Default)]
pub struct EffectReport {
    /// Per instance name: its effects as callers see them (keys are
    /// parameters or `Any`).
    pub summaries: BTreeMap<String, EffectSet>,
    /// Per instance name: each effectful terminator's own effects.
    pub sites: BTreeMap<String, Vec<Site>>,
    /// Per instance name: the `par` regions whose branches conflict.
    pub par_conflicts: BTreeMap<String, Vec<ParConflict>>,
}

/// Two branches of a `par` block, or two iterations of a `par for` body
/// (`branches` is then `(0, 0)`), with an effect from each that must keep
/// its order (core §11.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParConflict {
    /// The index into the body's [`Body::par_regions`].
    pub region: usize,
    pub line: usize,
    pub branches: (usize, usize),
    /// The block and the effect on each side.
    pub first: (BasicBlock, String),
    pub second: (BasicBlock, String),
}

/// Compute every body's effects.
pub fn analyze(program: &Program, tys: &TyInterner) -> EffectReport {
    let a = Analysis { program, tys };
    let names: Vec<&String> = program.bodies.keys().collect();
    let index: BTreeMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let edges: Vec<Vec<usize>> = names
        .iter()
        .map(|n| {
            let mut out: Vec<usize> = a
                .callees(&program.bodies[*n])
                .iter()
                .filter_map(|c| index.get(c.as_str()).copied())
                .collect();
            out.sort_unstable();
            out.dedup();
            out
        })
        .collect();
    let origins: Vec<Origins> = names
        .iter()
        .map(|n| Origins::of(&program.bodies[*n], tys))
        .collect();
    let mut report = EffectReport::default();
    // Tarjan yields each component after every component it reaches, so
    // callees are summarised before their callers.
    for scc in tarjan(&edges) {
        loop {
            let mut changed = false;
            for &i in &scc {
                let body = &program.bodies[names[i]];
                let sites = a.body_sites(body, &report.summaries);
                let mut summary = EffectSet::default();
                for s in &sites {
                    let mut e = s.effects.clone();
                    e.effects = e
                        .effects
                        .into_iter()
                        .map(|mut x| {
                            x.key = origins[i].widen(x.key);
                            x
                        })
                        .collect();
                    summary.union(e);
                }
                if report.summaries.get(names[i]) != Some(&summary) {
                    report.summaries.insert(names[i].clone(), summary);
                    changed = true;
                }
                report.sites.insert(names[i].clone(), sites);
            }
            if !changed {
                break;
            }
        }
    }
    for (i, name) in names.iter().enumerate() {
        let body = &program.bodies[*name];
        if body.par_regions.is_empty() {
            continue;
        }
        let found = par_conflicts(body, &report.sites[*name], &origins[i]);
        if !found.is_empty() {
            report.par_conflicts.insert((*name).clone(), found);
        }
    }
    report
}

/// The conflicting branch pairs of `body`'s `par` regions, at most one per
/// pair, given its sites.
fn par_conflicts(body: &Body, sites: &[Site], origins: &Origins) -> Vec<ParConflict> {
    let at: BTreeMap<BasicBlock, &EffectSet> =
        sites.iter().map(|s| (s.block, &s.effects)).collect();
    let alias = |a: &Key, b: &Key| origins.may_alias(a, b);
    let mut out = Vec::new();
    for (ri, r) in body.par_regions.iter().enumerate() {
        // Each branch's sites, in block order, with keys on the values the
        // branch makes itself turned fresh.
        let sides: Vec<Vec<(BasicBlock, EffectSet)>> = r
            .branches
            .iter()
            .map(|br| {
                let blocks: BTreeSet<BasicBlock> = br.iter().copied().collect();
                let mut v: Vec<(BasicBlock, EffectSet)> = blocks
                    .iter()
                    .filter_map(|b| at.get(b).map(|s| (*b, origins.privatize(s, &blocks))))
                    .collect();
                v.sort_by_key(|(b, _)| *b);
                v
            })
            .collect();
        let pairs: Vec<(usize, usize)> = match r.kind {
            ParKind::Block => (0..sides.len())
                .flat_map(|i| (i + 1..sides.len()).map(move |j| (i, j)))
                .collect(),
            // Two iterations run the same body.
            ParKind::For { .. } => (0..sides.len()).map(|i| (i, i)).collect(),
        };
        for (i, j) in pairs {
            'pair: for (k, (bx, x)) in sides[i].iter().enumerate() {
                let from = if i == j { k } else { 0 };
                for (by, y) in &sides[j][from..] {
                    if let Some((ex, ey)) = conflict_witness(x, y, &alias) {
                        out.push(ParConflict {
                            region: ri,
                            line: r.span.line,
                            branches: (i, j),
                            first: (*bx, ex),
                            second: (*by, ey),
                        });
                        break 'pair;
                    }
                }
            }
        }
    }
    out
}

impl EffectReport {
    /// `{"functions": [{"instance", "effects", "unknown_because", "sites":
    /// [{"block", "effects"}], "par_conflicts": [{"region", "line",
    /// "branches", "first": {"block", "effect"}, "second"}]}]}`, instances
    /// in name order: the output of `karac __mir-effects`.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let fns: Vec<_> = self
            .summaries
            .iter()
            .map(|(name, s)| {
                let sites: Vec<_> = self.sites.get(name).map_or(Vec::new(), |v| {
                    v.iter()
                        .map(|site| json!({"block": site.block.0, "effects": site.effects.display()}))
                        .collect()
                });
                let par: Vec<_> = self.par_conflicts.get(name).map_or(Vec::new(), |v| {
                    v.iter()
                        .map(|c| {
                            json!({
                                "region": c.region,
                                "line": c.line,
                                "branches": [c.branches.0, c.branches.1],
                                "first": {"block": c.first.0 .0, "effect": c.first.1},
                                "second": {"block": c.second.0 .0, "effect": c.second.1},
                            })
                        })
                        .collect()
                });
                json!({
                    "instance": name,
                    "effects": s.display(),
                    "unknown_because": s.why.iter().collect::<Vec<_>>(),
                    "sites": sites,
                    "par_conflicts": par,
                })
            })
            .collect();
        json!({ "functions": fns })
    }
}

/// Do two effects need to keep their order (D6)? `may_alias` answers
/// whether two distinct roots (parameters or locals of one body) can name
/// the same value, e.g. two `ref` parameters.
pub fn conflicts(a: &Effect, b: &Effect, may_alias: &dyn Fn(&Key, &Key) -> bool) -> bool {
    use Verb::*;
    let ordered = matches!(
        (a.verb, b.verb),
        (Reads, Writes)
            | (Writes, Reads)
            | (Writes, Writes)
            | (Sends, Sends)
            | (Receives, Receives)
    );
    ordered && a.class == b.class && keys_overlap(&a.key, &b.key, may_alias)
}

/// Do two effect sets need to keep their order? An unknown set conflicts
/// with anything that has an effect that keeps an order, and so does a set
/// that still calls a parameter's function value, whose effects only the
/// caller knows.
pub fn sets_conflict(a: &EffectSet, b: &EffectSet, may_alias: &dyn Fn(&Key, &Key) -> bool) -> bool {
    conflict_witness(a, b, may_alias).is_some()
}

/// The first pair of effects, one from each set, that must keep their
/// order, displayed.
pub fn conflict_witness(
    a: &EffectSet,
    b: &EffectSet,
    may_alias: &dyn Fn(&Key, &Key) -> bool,
) -> Option<(String, String)> {
    if let (Some(x), Some(y)) = (top(a), ordered_effect(b)) {
        return Some((x, y));
    }
    if let (Some(x), Some(y)) = (ordered_effect(a), top(b)) {
        return Some((x, y));
    }
    a.effects.iter().find_map(|x| {
        b.effects
            .iter()
            .find(|y| conflicts(x, y, may_alias))
            .map(|y| (x.display(), y.display()))
    })
}

/// What makes a set conflict with everything: an unknown callee, or a call
/// through a parameter's function value.
fn top(s: &EffectSet) -> Option<String> {
    if s.unknown {
        return Some("unknown".to_string());
    }
    s.calls.iter().next().map(|p| format!("calls(p{p})"))
}

/// An effect of `s` that an unknown effect would conflict with: one that
/// keeps an order, on a value someone else can name.
fn ordered_effect(s: &EffectSet) -> Option<String> {
    use Verb::*;
    top(s).or_else(|| {
        s.effects
            .iter()
            .find(|e| matches!(e.verb, Reads | Writes | Sends | Receives) && e.key != Key::Fresh)
            .map(Effect::display)
    })
}

fn keys_overlap(a: &Key, b: &Key, may_alias: &dyn Fn(&Key, &Key) -> bool) -> bool {
    match (a, b) {
        (Key::Fresh, _) | (_, Key::Fresh) => false,
        (Key::Any, _) | (_, Key::Any) => true,
        (Key::Param(r1, p1) | Key::Local(r1, p1), Key::Param(r2, p2) | Key::Local(r2, p2)) => {
            let same_root = r1 == r2 && std::mem::discriminant(a) == std::mem::discriminant(b);
            // One path is a prefix of the other: the same value or a part of
            // it. Two different fields can still hold clones of one handle.
            let n = p1.len().min(p2.len());
            (same_root && p1[..n] == p2[..n]) || may_alias(a, b)
        }
    }
}

/// Where each local's value may come from, in one body: what decides
/// whether two keys can name the same value (D6, "when the compiler cannot
/// tell whether two such values are the same, they conflict"), and whether
/// a value is made inside a `par` branch.
///
/// An edge `a -> b` says `b` may hold a value that `a` held, or part of
/// one: an assignment, a projection, an aggregate, a call's result from its
/// arguments, and an argument stored into another that the callee can
/// write through (a reference, a `shared` handle, a closure). When the
/// receiving local is itself such a reference, the edge also runs back,
/// since writing through it changes what it points to. Every parameter
/// comes from one origin, the caller, which may pass one value (or clones
/// of one handle) twice. Scalars and strings hold no resource and are left
/// out. The relation ignores order, so a value is assumed to hold anything
/// that ever flows into its local.
pub struct Origins {
    /// Per local, the locals it receives values from directly; the extra
    /// last entry is the caller.
    preds: Vec<Vec<u32>>,
    /// Per local, the blocks that assign it.
    assigned: Vec<BTreeSet<BasicBlock>>,
    cache: RefCell<BTreeMap<u32, Rc<BTreeSet<u32>>>>,
}

impl Origins {
    pub fn of(body: &Body, tys: &TyInterner) -> Origins {
        let tcx = tys.tcx();
        let n = body.locals.len();
        let carries: Vec<bool> = body
            .locals
            .iter()
            .map(|d| carries_value(tcx, d.ty))
            .collect();
        let by_ref: Vec<bool> = body.locals.iter().map(|d| refers(tcx, d.ty, 0)).collect();
        let mut preds: Vec<Vec<u32>> = vec![Vec::new(); n + 1];
        let mut assigned = vec![BTreeSet::new(); n];
        let flow = |preds: &mut Vec<Vec<u32>>, src: usize, dst: usize, back: bool| {
            if src == dst || !carries[src] || !carries[dst] {
                return;
            }
            preds[dst].push(src as u32);
            if back && by_ref[dst] {
                preds[src].push(dst as u32);
            }
        };
        for a in body.args() {
            if carries[a.index()] {
                preds[a.index()].push(n as u32);
            }
        }
        let root = |o: &Operand| o.place().map(|p| p.local.index());
        for (bi, bb) in body.blocks.iter().enumerate() {
            let block = BasicBlock(bi as u32);
            for st in &bb.statements {
                let StatementKind::Assign(dst, rv) = &st.kind else {
                    continue;
                };
                let d = dst.local.index();
                assigned[d].insert(block);
                let srcs: Vec<usize> = match rv {
                    Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => {
                        root(o).into_iter().collect()
                    }
                    Rvalue::Ref(_, p)
                    | Rvalue::Retain(p)
                    | Rvalue::Discriminant(p)
                    | Rvalue::Len(p) => vec![p.local.index()],
                    Rvalue::BinaryOp(_, x, y) | Rvalue::CheckedBinaryOp(_, x, y) => {
                        [x, y].into_iter().filter_map(root).collect()
                    }
                    Rvalue::Aggregate(_, ops) => ops.iter().filter_map(root).collect(),
                };
                for src in srcs {
                    flow(&mut preds, src, d, true);
                }
            }
            if let TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } = &bb.terminator.kind
            {
                let d = destination.local.index();
                assigned[d].insert(block);
                let ins: Vec<usize> = std::iter::once(func).chain(args).filter_map(root).collect();
                for &a in &ins {
                    flow(&mut preds, a, d, true);
                    // The callee may store `a` through any argument it can
                    // write through.
                    for &b in &ins {
                        if by_ref[b] {
                            flow(&mut preds, a, b, false);
                        }
                    }
                }
            }
        }
        for p in &mut preds {
            p.sort_unstable();
            p.dedup();
        }
        Origins {
            preds,
            assigned,
            cache: RefCell::new(BTreeMap::new()),
        }
    }

    /// The caller, as an origin.
    fn caller(&self) -> u32 {
        self.assigned.len() as u32
    }

    /// Every local (and possibly the caller) whose value `l` may hold,
    /// `l` included.
    fn origins(&self, l: u32) -> Rc<BTreeSet<u32>> {
        if let Some(s) = self.cache.borrow().get(&l) {
            return s.clone();
        }
        let mut seen = BTreeSet::from([l]);
        let mut work = vec![l];
        while let Some(x) = work.pop() {
            for &p in self.preds.get(x as usize).map_or(&[][..], |v| v.as_slice()) {
                if seen.insert(p) {
                    work.push(p);
                }
            }
        }
        let s = Rc::new(seen);
        self.cache.borrow_mut().insert(l, s.clone());
        s
    }

    /// Can two keys of this body name the same value? Two distinct roots
    /// can when they share an origin; one root always can, since two of its
    /// fields may hold clones of one handle.
    pub fn may_alias(&self, a: &Key, b: &Key) -> bool {
        let root = |k: &Key| match k {
            Key::Param(r, _) | Key::Local(r, _) => Some(*r),
            _ => None,
        };
        match (a, b) {
            (Key::Fresh, _) | (_, Key::Fresh) => false,
            _ => match (root(a), root(b)) {
                (Some(x), Some(y)) if x == y => true,
                (Some(x), Some(y)) => {
                    let ox = self.origins(x);
                    self.origins(y).iter().any(|o| ox.contains(o))
                }
                _ => true,
            },
        }
    }

    /// A key as the body's callers see it: a local that may hold a value
    /// the caller passed is some value of its class; one that cannot was
    /// made inside the call.
    fn widen(&self, k: Key) -> Key {
        match k {
            Key::Local(r, _) if self.origins(r).contains(&self.caller()) => Key::Any,
            Key::Local(..) => Key::Fresh,
            k => k,
        }
    }

    /// Is every value `l` holds made inside `blocks`? It is when `l` and
    /// all its origins are assigned there and nowhere else.
    fn made_in(&self, l: u32, blocks: &BTreeSet<BasicBlock>) -> bool {
        let here = |x: u32| {
            self.assigned
                .get(x as usize)
                .is_some_and(|a| !a.is_empty() && a.is_subset(blocks))
        };
        self.origins(l).iter().all(|&x| here(x))
    }

    /// `s` with the keys on values made inside `blocks` made fresh.
    fn privatize(&self, s: &EffectSet, blocks: &BTreeSet<BasicBlock>) -> EffectSet {
        let mut out = s.clone();
        out.effects = s
            .effects
            .iter()
            .map(|e| {
                let mut e = e.clone();
                if let Key::Local(r, _) = e.key {
                    if self.made_in(r, blocks) {
                        e.key = Key::Fresh;
                    }
                }
                e
            })
            .collect();
        out
    }
}

/// Can a value of type `ty` hold a resource? Scalars and strings cannot,
/// nor can a reference to one.
fn carries_value(tcx: &TyCtxt, ty: Ty) -> bool {
    let mut t = ty;
    while let SK::Ref(i) | SK::MutRef(i) | SK::RawPtr { pointee: i, .. } = tcx.kind(t) {
        t = i;
    }
    !matches!(
        tcx.kind(t),
        SK::Int(_)
            | SK::UInt(_)
            | SK::Float(_)
            | SK::Bool
            | SK::Char
            | SK::Str
            | SK::StaticStr
            | SK::Unit
            | SK::Never
            | SK::FnDef { .. }
    )
}

/// Can writing through a value of type `ty` change another value: is it,
/// or does it contain, a reference, a `shared` handle or a closure?
fn refers(tcx: &TyCtxt, ty: Ty, depth: u32) -> bool {
    if depth > 4 {
        return true;
    }
    match tcx.kind(ty) {
        SK::Ref(_)
        | SK::MutRef(_)
        | SK::RawPtr { .. }
        | SK::Shared { .. }
        | SK::Weak(_)
        | SK::Closure { .. }
        | SK::Fn { .. } => true,
        SK::Tuple(l) | SK::Adt { args: l, .. } | SK::Intrinsic { args: l, .. } => {
            tcx.list(l).into_iter().any(|t| refers(tcx, t, depth + 1))
        }
        SK::Array { elem, .. } | SK::Slice { elem, .. } => refers(tcx, elem, depth + 1),
        _ => false,
    }
}

/// What a library native does, by `(type, method)` with the type's
/// arguments stripped; `None` for a native this table does not know, which
/// the caller turns into an unknown effect. The `usize` is the argument
/// whose value keys the effect.
fn native_effects(base: &str, method: &str) -> Option<Vec<(Verb, &'static str, Option<usize>)>> {
    use Verb::*;
    const HEAP: (Verb, &str, Option<usize>) = (Allocates, "Heap", None);
    const PANICS: (Verb, &str, Option<usize>) = (Panics, "", None);
    Some(match (base, method) {
        ("println" | "print", "") => vec![(Writes, "Stdout", None), HEAP],
        ("eprintln" | "eprint", "") => vec![(Writes, "Stderr", None), HEAP],
        ("Stdout", "println" | "print" | "flush") => vec![(Writes, "Stdout", None), HEAP],
        ("Stderr", "println" | "print" | "flush") => vec![(Writes, "Stderr", None), HEAP],
        ("format", "") | (_, "to_string") => vec![HEAP],
        ("Env", "args" | "var") => vec![(Reads, "Env", None), HEAP],
        ("Env", "set" | "remove") => vec![(Writes, "Env", None), HEAP],
        ("Clock", "now") => vec![(Reads, "Clock", None)],
        // Drawing a number advances the source, so two draws on one source
        // keep their order (legacy called this a read).
        ("RandomSource", "next_u64" | "next_i64" | "next_f64") => {
            vec![(Writes, "RandomSource", Some(0))]
        }
        ("Atomic" | "Mutex", "new") => vec![HEAP],
        ("sleep_ms", "") => vec![(Blocks, "", None)],
        // The collections the MIR interpreter implements natively: they
        // allocate, and an out-of-range index or a missing key panics.
        // `Option`/`Result` combinators are here too; a closure they take is
        // charged below, with every native's closure arguments.
        (
            "String" | "Vec" | "VecDeque" | "Map" | "Set" | "SortedMap" | "SortedSet" | "Entry"
            | "Slice" | "Array" | "Option" | "Result",
            _,
        ) => vec![HEAP, PANICS],
        (_, "as_slice" | "as_mut_slice" | "slice" | "slice_mut") => vec![PANICS],
        (_, "clone") => vec![HEAP],
        // Arithmetic, conversions and predicates on scalars touch no
        // resource; an overflow or a checked conversion can panic.
        (
            "bool" | "char" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16"
            | "u32" | "u64" | "u128" | "usize" | "f16" | "bf16" | "f32" | "f64",
            _,
        ) => vec![PANICS],
        ("Ordering", _) => vec![],
        // A comparison native on a user type is a derived one: it reads its
        // operands and nothing else.
        (_, "cmp" | "partial_cmp" | "eq" | "ne") => vec![PANICS],
        // An atomic is a resource of its own, keyed by the atomic value.
        ("Atomic", "load") => vec![(Reads, "Atomic", Some(0))],
        ("Atomic", "store" | "swap" | "compare_exchange" | "fetch_add" | "fetch_sub") => {
            vec![(Writes, "Atomic", Some(0))]
        }
        // D6: channels and connections key their effects by the endpoint
        // (argument 0, the receiver); files by the open file. These have no
        // MIR natives yet (M2 brings the I/O intrinsics); the entries fix
        // the keying the conflict rule relies on.
        ("Channel", "new") => vec![HEAP],
        ("Sender", "send") => vec![(Sends, "Channel", Some(0)), (Blocks, "", None)],
        ("Sender", "try_send") => vec![(Sends, "Channel", Some(0))],
        ("Receiver", "recv") => vec![(Receives, "Channel", Some(0)), (Blocks, "", None)],
        ("Receiver", "try_recv") => vec![(Receives, "Channel", Some(0))],
        ("TcpStream", "write" | "write_all" | "flush") => {
            vec![(Sends, "Network", Some(0)), (Blocks, "", None)]
        }
        ("TcpStream", "read" | "read_line" | "read_to_string") => {
            vec![(Receives, "Network", Some(0)), (Blocks, "", None)]
        }
        ("File", "write" | "write_all" | "flush") => {
            vec![(Writes, "File", Some(0)), (Blocks, "", None)]
        }
        ("File", "read" | "read_line" | "read_to_string") => {
            vec![(Reads, "File", Some(0)), (Blocks, "", None)]
        }
        // Starting, waiting on or killing a child sends to the process
        // table (`sends(ProcessTable)` in the library); a call on one child
        // is keyed by it, so two children's waits do not conflict. Its pipes
        // are keyed by the pipe value, like a connection.
        ("Command", "spawn" | "output" | "status") => {
            vec![(Sends, "ProcessTable", None), (Blocks, "", None), HEAP]
        }
        ("Command", _) => vec![HEAP],
        ("Child", "wait" | "try_wait" | "kill" | "wait_with_output") => {
            vec![(Sends, "ProcessTable", Some(0)), (Blocks, "", None), HEAP]
        }
        ("ChildStdin", "write" | "write_all" | "flush" | "close") => {
            vec![(Sends, "Pipe", Some(0)), (Blocks, "", None)]
        }
        ("ChildStdout" | "ChildStderr", "read" | "read_line" | "read_to_string") => {
            vec![(Receives, "Pipe", Some(0)), (Blocks, "", None), HEAP]
        }
        // Taking a child's pipe hands out a handle; the I/O is on the pipe.
        ("Child", "stdin" | "stdout" | "stderr" | "id") => vec![HEAP],
        // An HTTP request or response is a plain value once received: its
        // accessors and builders only allocate.
        ("Request" | "Response" | "HttpError", _) => vec![HEAP],
        ("Client", "new" | "request") | ("RequestBuilder", "header" | "body" | "timeout") => {
            vec![HEAP]
        }
        // A client call opens its own connection, so there is no value to
        // key it by: it sends and receives on the network as a whole.
        ("Client", "get" | "post") | ("RequestBuilder", "send") => vec![
            (Sends, "Network", None),
            (Receives, "Network", None),
            (Blocks, "", None),
            HEAP,
        ],
        // Paths name files by string value, which a key cannot follow: the
        // whole file system is the resource.
        ("fs" | "FileSystem", "write" | "append" | "remove" | "create_dir") => {
            vec![(Writes, "FileSystem", None), (Blocks, "", None), HEAP]
        }
        ("fs" | "FileSystem", "read_to_string" | "read" | "read_lines" | "exists") => {
            vec![(Reads, "FileSystem", None), (Blocks, "", None), HEAP]
        }
        ("File", "open") => vec![(Reads, "FileSystem", None), (Blocks, "", None), HEAP],
        ("File", "create") => vec![(Writes, "FileSystem", None), (Blocks, "", None), HEAP],
        // Seeking moves the open file's position, which every later read
        // or write on it observes.
        ("File", "seek") => vec![(Writes, "File", Some(0)), (Blocks, "", None)],
        ("Stdin", "read_line" | "read_to_string" | "lines") => {
            vec![(Reads, "Stdin", None), (Blocks, "", None), HEAP]
        }
        // Pure libraries over values: they allocate, and a bad index or
        // argument panics. Parsing command-line arguments reads them.
        ("Regex" | "Match" | "Json" | "Arg" | "Stats", _) => vec![HEAP, PANICS],
        ("Parser", "parse") => vec![(Reads, "Env", None), HEAP, PANICS],
        ("Parser", _) => vec![HEAP, PANICS],
        ("format_spec", "") => vec![HEAP],
        // A tracing value is built without emitting it; the stdout exporter
        // prints. `Log.*` is left out: it calls whichever exporter is
        // registered, which can be the program's own.
        ("Span" | "LogEvent", _) => vec![HEAP],
        ("StdoutExporter", "export_event" | "export_span") => {
            vec![(Writes, "Stdout", None), HEAP]
        }
        ("NoOpExporter", "export_event" | "export_span") => vec![],
        ("usleep", "") => vec![(Blocks, "", None)],
        // A fence orders the atomics around it, so it conflicts with every
        // atomic; the scheduler queries read the scheduler's state.
        ("fence" | "compiler_fence", "") => vec![(Writes, "Atomic", None)],
        ("list_par_blocks" | "list_tasks", "") => vec![(Reads, "Scheduler", None), HEAP],
        ("has_debug_metadata", "") => vec![],
        _ => return None,
    })
}

/// `Vec[i64].push` -> (`Vec`, `push`); `println` -> (`println`, ``). The
/// split is at the last `.` outside brackets, as the interpreter does.
fn split_native(name: &str) -> (&str, &str) {
    let mut depth = 0i32;
    let mut dot = None;
    for (i, c) in name.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            '.' if depth == 0 => dot = Some(i),
            _ => {}
        }
    }
    let (ty, method) = match dot {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (name, ""),
    };
    (ty.split('[').next().unwrap_or(ty), method)
}

struct Analysis<'p> {
    program: &'p Program,
    tys: &'p TyInterner,
}

impl<'p> Analysis<'p> {
    /// The bodies `body` may run: direct callees, closures handed to a
    /// native, and the `Drop` bodies its drops reach.
    fn callees(&self, body: &Body) -> Vec<String> {
        let mut out = Vec::new();
        for bb in &body.blocks {
            match &bb.terminator.kind {
                TerminatorKind::Call {
                    func:
                        Operand::Const(Const {
                            kind: ConstKind::FnDef(inst),
                            ..
                        }),
                    args,
                    ..
                } => {
                    if self.program.bodies.contains_key(&inst.name) {
                        out.push(inst.name.clone());
                    }
                    // A function value passed along may run in the callee.
                    for a in args {
                        out.extend(self.fn_value_bodies(body, a));
                    }
                }
                TerminatorKind::Drop { place, .. } => {
                    if let Ok(pt) = place_ty(body, self.tys, place) {
                        let mut seen = Vec::new();
                        self.drop_bodies(pt.ty, &mut seen, &mut out);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The bodies of a closure or function item passed as `arg` (behind
    /// any references).
    fn fn_value_bodies(&self, body: &Body, arg: &Operand) -> Vec<String> {
        let ty = match arg {
            Operand::Copy(p) | Operand::Move(p) => match place_ty(body, self.tys, p) {
                Ok(pt) => pt.ty,
                Err(_) => return Vec::new(),
            },
            Operand::Const(c) => c.ty,
        };
        let tcx = self.tys.tcx();
        let mut t = ty;
        while let SK::Ref(i) | SK::MutRef(i) = tcx.kind(t) {
            t = i;
        }
        let def: DefId = match tcx.kind(t) {
            SK::Closure { def, .. } | SK::FnDef { def, .. } => def,
            _ => return Vec::new(),
        };
        self.program
            .bodies
            .values()
            .filter(|b| b.instance.def == def)
            .map(|b| b.instance.name.clone())
            .collect()
    }

    /// The `Drop` bodies dropping a value of type `ty` can run.
    fn drop_bodies(&self, ty: Ty, seen: &mut Vec<Ty>, out: &mut Vec<String>) {
        if seen.contains(&ty) {
            return;
        }
        seen.push(ty);
        let tcx = self.tys.tcx();
        let adt = match tcx.kind(ty) {
            SK::Adt { def, .. } | SK::Shared { def, .. } => Some(def),
            _ => None,
        };
        if let Some(def) = adt {
            if let Some(name) = self.program.drop_impls.get(&AdtId(def.0)) {
                out.push(name.clone());
            }
        }
        for part in self.parts(ty) {
            self.drop_bodies(part, seen, out);
        }
    }

    /// The types a value of type `ty` owns and drops with it.
    fn parts(&self, ty: Ty) -> Vec<Ty> {
        let tcx = self.tys.tcx();
        match tcx.kind(ty) {
            SK::Tuple(l) | SK::Closure { captures: l, .. } | SK::Intrinsic { args: l, .. } => {
                tcx.list(l)
            }
            SK::Array { elem, .. } | SK::Slice { elem, .. } => vec![elem],
            SK::Adt { .. } | SK::Shared { .. } => {
                let Some((adt, args)) = tcx.adt_of(ty) else {
                    return Vec::new();
                };
                adt.variants
                    .iter()
                    .flat_map(|v| v.fields.iter().map(|(_, t)| tcx.subst(*t, &args, &[])))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The effects of each terminator of `body`, given the summaries of the
    /// bodies computed so far (a callee not summarised yet, in the same
    /// component, contributes nothing this round).
    fn body_sites(&self, body: &Body, summaries: &BTreeMap<String, EffectSet>) -> Vec<Site> {
        let defs = single_defs(body);
        let mut sites = Vec::new();
        for (i, bb) in body.blocks.iter().enumerate() {
            let block = BasicBlock(i as u32);
            let mut eff = EffectSet::default();
            match &bb.terminator.kind {
                TerminatorKind::Call { func, args, .. } => {
                    self.call_effects(body, &defs, func, args, summaries, &mut eff);
                }
                TerminatorKind::Drop { place, .. } => {
                    let key = place_key(body, &defs, place);
                    match place_ty(body, self.tys, place) {
                        Ok(pt) => {
                            let mut seen = Vec::new();
                            self.drop_glue(pt.ty, key, summaries, &mut seen, &mut eff);
                        }
                        Err(e) => eff.union(EffectSet::unknown(format!("untyped drop: {e}"))),
                    }
                }
                TerminatorKind::Abort { .. } => eff.add(Effect::new(Verb::Panics, "", Key::Any)),
                _ => {}
            }
            if !eff.is_empty() {
                sites.push(Site {
                    block,
                    effects: eff,
                });
            }
        }
        sites
    }

    fn call_effects(
        &self,
        body: &Body,
        defs: &BTreeMap<u32, Place>,
        func: &Operand,
        args: &[Operand],
        summaries: &BTreeMap<String, EffectSet>,
        eff: &mut EffectSet,
    ) {
        let Operand::Const(Const {
            kind: ConstKind::FnDef(inst),
            ..
        }) = func
        else {
            // A call through a parameter's function value has the effects
            // of whatever each caller passes (core §12); a call through any
            // other value (a field, a local holding an escaping function)
            // has every effect.
            match func.place().map(|p| place_key(body, defs, p)) {
                Some(Key::Param(p, path)) if path.is_empty() => {
                    eff.calls.insert(p);
                }
                _ => eff.union(EffectSet::unknown(format!(
                    "{}: a call through a value",
                    body.instance.name
                ))),
            }
            return;
        };
        let arg_key = |i: usize| -> Key {
            match args.get(i).and_then(Operand::place) {
                Some(p) => place_key(body, defs, p),
                None => Key::Any,
            }
        };
        let arg_fn = |i: usize| self.fn_value_effects(body, defs, args.get(i), summaries);
        if self.program.bodies.contains_key(&inst.name) {
            if let Some(callee) = summaries.get(&inst.name) {
                eff.union(substitute(callee, &arg_key, &arg_fn));
            }
            return;
        }
        let native = inst.def == DefId(u32::MAX);
        let (base, method) = split_native(&inst.name);
        match native_effects(base, method).filter(|_| native) {
            Some(table) => {
                for (verb, class, key_arg) in table {
                    // `Network` and `FileSystem` with no value to root them
                    // are capabilities, not conflict keys (core §12 item 5).
                    let unrooted = if matches!(class, "Network" | "FileSystem") {
                        Key::Fresh
                    } else {
                        Key::Any
                    };
                    let key = key_arg.map_or(unrooted, arg_key);
                    eff.add(Effect::new(verb, class, key));
                }
                // A closure or function handed to a native runs inside it.
                for a in args {
                    for b in self.fn_value_bodies(body, a) {
                        if let Some(s) = summaries.get(&b) {
                            eff.union(substitute(s, &|_| Key::Any, &|_| opaque()));
                        }
                    }
                }
            }
            None if native => eff.union(EffectSet::unknown(format!("native {}", inst.name))),
            None => eff.union(EffectSet::unknown(format!("no body for {}", inst.name))),
        }
    }

    /// The effects of calling the function value passed as `arg`: the
    /// bodies it can name, or the caller's own parameter, forwarded; every
    /// effect when it is neither.
    fn fn_value_effects(
        &self,
        body: &Body,
        defs: &BTreeMap<u32, Place>,
        arg: Option<&Operand>,
        summaries: &BTreeMap<String, EffectSet>,
    ) -> EffectSet {
        let Some(arg) = arg else {
            return opaque();
        };
        let bodies = self.fn_value_bodies(body, arg);
        if !bodies.is_empty() {
            let mut out = EffectSet::default();
            for b in bodies {
                if let Some(s) = summaries.get(&b) {
                    out.union(substitute(s, &|_| Key::Any, &|_| opaque()));
                }
            }
            return out;
        }
        match arg.place().map(|p| place_key(body, defs, p)) {
            Some(Key::Param(p, path)) if path.is_empty() => EffectSet {
                calls: BTreeSet::from([p]),
                ..EffectSet::default()
            },
            _ => EffectSet::unknown(format!(
                "{}: a function value that cannot be resolved",
                body.instance.name
            )),
        }
    }

    /// The effects of dropping a value of type `ty` whose key is `key`.
    fn drop_glue(
        &self,
        ty: Ty,
        key: Key,
        summaries: &BTreeMap<String, EffectSet>,
        seen: &mut Vec<Ty>,
        eff: &mut EffectSet,
    ) {
        if seen.contains(&ty) {
            return;
        }
        seen.push(ty);
        let tcx = self.tys.tcx();
        let def = match tcx.kind(ty) {
            SK::Adt { def, .. } | SK::Shared { def, .. } => Some(def),
            _ => None,
        };
        if let Some(def) = def {
            if let Some(name) = self.program.drop_impls.get(&AdtId(def.0)) {
                if let Some(s) = summaries.get(name) {
                    // The Drop body's single parameter is the value itself.
                    let k = key.clone();
                    eff.union(substitute(
                        s,
                        &move |i| if i == 0 { k.clone() } else { Key::Any },
                        &|_| opaque(),
                    ));
                }
            }
        }
        // A part keeps the key only as "part of this value"; its own field
        // path is not tracked.
        for part in self.parts(ty) {
            self.drop_glue(part, key.clone(), summaries, seen, eff);
        }
        seen.pop();
    }
}

/// Field paths are cut to this length, so a recursion that walks deeper
/// into its parameter (`f(p.next)`) still reaches a fixpoint. A shorter path
/// names an enclosing value, which overlaps more: sound.
const MAX_PATH: usize = 4;

/// A function value whose effects are not tracked: a closure's capture
/// environment, or the receiver of a Drop body.
fn opaque() -> EffectSet {
    EffectSet::unknown("a call through a value".to_string())
}

/// A callee's summary seen from a call site: parameter keys become the
/// keys of the matching arguments, and a call through a parameter becomes
/// the effects of calling the matching argument (`arg_fn`, by argument
/// index).
fn substitute(
    callee: &EffectSet,
    arg_key: &dyn Fn(usize) -> Key,
    arg_fn: &dyn Fn(usize) -> EffectSet,
) -> EffectSet {
    let mut out = EffectSet {
        unknown: callee.unknown,
        why: callee.why.clone(),
        ..EffectSet::default()
    };
    for p in &callee.calls {
        out.union(arg_fn(*p as usize - 1));
    }
    for e in &callee.effects {
        let key = match &e.key {
            Key::Param(p, path) => match arg_key(*p as usize - 1) {
                Key::Any => Key::Any,
                Key::Fresh => Key::Fresh,
                Key::Param(r, mut base) => {
                    base.extend(path);
                    base.truncate(MAX_PATH);
                    Key::Param(r, base)
                }
                Key::Local(r, mut base) => {
                    base.extend(path);
                    base.truncate(MAX_PATH);
                    Key::Local(r, base)
                }
            },
            k => k.clone(),
        };
        out.add(Effect {
            verb: e.verb,
            class: e.class.clone(),
            key,
        });
    }
    out
}

/// Temporaries assigned exactly once from a borrow or a use of a place:
/// the place they stand for.
fn single_defs(body: &Body) -> BTreeMap<u32, Place> {
    let mut count: BTreeMap<u32, u32> = BTreeMap::new();
    let mut src: BTreeMap<u32, Place> = BTreeMap::new();
    for bb in &body.blocks {
        for s in &bb.statements {
            if let StatementKind::Assign(dst, rv) = &s.kind {
                if !dst.projection.is_empty() {
                    continue;
                }
                *count.entry(dst.local.0).or_default() += 1;
                let from = match rv {
                    Rvalue::Ref(_, p) => Some((p.clone(), true)),
                    Rvalue::Use(Operand::Copy(p) | Operand::Move(p)) => Some((p.clone(), false)),
                    _ => None,
                };
                if let Some((p, is_ref)) = from {
                    // A borrow stands for the borrowed place behind one
                    // `Deref`; a use stands for the place itself.
                    let mut p = p;
                    if is_ref {
                        p.projection.push(ProjElem::Deref);
                    }
                    src.insert(dst.local.0, p);
                }
            }
        }
        if let TerminatorKind::Call { destination, .. } = &bb.terminator.kind {
            *count.entry(destination.local.0).or_default() += 2;
        }
    }
    src.retain(|l, _| {
        count.get(l) == Some(&1) && matches!(body.locals[*l as usize].kind, LocalKind::Temp)
    });
    src
}

/// The key of the value at `place`: its root (through single-assignment
/// temporaries) and the field path. An element of a collection or array
/// is `Any`, and so is a value reached only through a reference this body
/// cannot trace.
fn place_key(body: &Body, defs: &BTreeMap<u32, Place>, place: &Place) -> Key {
    let mut cur = place.clone();
    for _ in 0..32 {
        let Some(src) = defs.get(&cur.local.0) else {
            break;
        };
        // `t = &p` and the use goes through `*t`: drop that `Deref`.
        let mut proj = src.projection.clone();
        let mut rest = cur.projection.as_slice();
        if proj.last() == Some(&ProjElem::Deref) && rest.first() == Some(&ProjElem::Deref) {
            proj.pop();
            rest = &rest[1..];
        }
        proj.extend_from_slice(rest);
        cur = Place {
            local: src.local,
            projection: proj,
        };
    }
    let mut path = Vec::new();
    for e in &cur.projection {
        match e {
            ProjElem::Field(f, _) if path.len() < MAX_PATH => path.push(f.0),
            ProjElem::Field(..) => {}
            ProjElem::Deref | ProjElem::Downcast(_) => {}
            ProjElem::Index(_) | ProjElem::ConstIndex(_) => return Key::Any,
        }
    }
    let root = cur.local.0;
    if root >= 1 && (root as usize) <= body.arg_count {
        return Key::Param(root, path);
    }
    match body.locals.get(root as usize).map(|d| &d.kind) {
        Some(LocalKind::User { .. }) => Key::Local(root, path),
        // A temporary we could not trace, or the return place.
        _ => Key::Any,
    }
}

/// Strongly connected components of `edges`, each after every component
/// it reaches (Tarjan).
fn tarjan(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct St<'e> {
        edges: &'e [Vec<usize>],
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        on: Vec<bool>,
        stack: Vec<usize>,
        next: usize,
        out: Vec<Vec<usize>>,
    }
    fn visit(s: &mut St, v: usize) {
        s.index[v] = Some(s.next);
        s.low[v] = s.next;
        s.next += 1;
        s.stack.push(v);
        s.on[v] = true;
        for i in 0..s.edges[v].len() {
            let w = s.edges[v][i];
            match s.index[w] {
                None => {
                    visit(s, w);
                    s.low[v] = s.low[v].min(s.low[w]);
                }
                Some(iw) if s.on[w] => s.low[v] = s.low[v].min(iw),
                _ => {}
            }
        }
        if Some(s.low[v]) == s.index[v] {
            let mut comp = Vec::new();
            while let Some(w) = s.stack.pop() {
                s.on[w] = false;
                comp.push(w);
                if w == v {
                    break;
                }
            }
            comp.sort_unstable();
            s.out.push(comp);
        }
    }
    let n = edges.len();
    let mut s = St {
        edges,
        index: vec![None; n],
        low: vec![0; n],
        on: vec![false; n],
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };
    for v in 0..n {
        if s.index[v].is_none() {
            visit(&mut s, v);
        }
    }
    s.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::parse_module;

    fn from_source(src: &str) -> EffectReport {
        let l = crate::mir::lower::build_source(src).unwrap_or_else(|e| panic!("{e}"));
        analyze(&l.program, &l.tys)
    }

    /// A parsed module, with every body-less callee except `externs`
    /// marked as a library native, as the lowering marks them.
    fn from_mir(src: &str, externs: &[&str]) -> (Program, TyInterner) {
        let m = parse_module(src).unwrap_or_else(|e| panic!("{e}"));
        let mut p = Program::from_module(&m);
        let names: BTreeSet<String> = p.bodies.keys().cloned().collect();
        for body in p.bodies.values_mut() {
            for bb in &mut body.blocks {
                if let TerminatorKind::Call {
                    func:
                        Operand::Const(Const {
                            kind: ConstKind::FnDef(inst),
                            ..
                        }),
                    ..
                } = &mut bb.terminator.kind
                {
                    if !names.contains(&inst.name) && !externs.contains(&inst.name.as_str()) {
                        inst.def = DefId(u32::MAX);
                    }
                }
            }
        }
        (p, m.tys)
    }

    fn effects(r: &EffectReport, name: &str) -> Vec<String> {
        r.summaries
            .get(name)
            .unwrap_or_else(|| panic!("no body {name}: {:?}", r.summaries.keys()))
            .display()
    }

    #[test]
    fn effects_println_writes_stdout() {
        let r = from_source("fn main() {\n    println(\"hi\");\n}\n");
        assert!(
            effects(&r, "main").contains(&"writes(Stdout)".to_string()),
            "{:?}",
            effects(&r, "main")
        );
        assert!(!r.summaries["main"].unknown);
    }

    /// C10: a value's Drop body is charged to the function whose scope
    /// drops it, and not to one that only moves it on.
    #[test]
    fn effects_drop_is_charged_where_the_drop_happens() {
        let r = from_source(
            "struct Logger { id: i64 }

impl Drop for Logger {
    fn drop(mut ref self) {
        println(\"drop\");
    }
}

fn make() {
    let l = Logger { id: 1 };
}

fn pass(l: Logger) -> Logger {
    l
}

fn main() {
    make();
    let l = pass(Logger { id: 2 });
}
",
        );
        assert!(
            effects(&r, "make").contains(&"writes(Stdout)".to_string()),
            "{:?}",
            effects(&r, "make")
        );
        assert!(
            !effects(&r, "pass").contains(&"writes(Stdout)".to_string()),
            "{:?}",
            effects(&r, "pass")
        );
        assert!(effects(&r, "main").contains(&"writes(Stdout)".to_string()));
    }

    /// C10: mutually recursive functions share one fixpoint, so a function
    /// that prints only through its partner still prints.
    #[test]
    fn effects_recursion_reaches_a_fixpoint() {
        let r = from_source(
            "fn even(n: i64) -> bool {
    if n == 0 { return true; }
    odd(n - 1)
}

fn odd(n: i64) -> bool {
    if n == 0 { return false; }
    println(\"odd\");
    even(n - 1)
}

fn main() {
    let b = even(4);
}
",
        );
        for f in ["even", "odd", "main"] {
            assert!(
                effects(&r, f).contains(&"writes(Stdout)".to_string()),
                "{f}: {:?}",
                effects(&r, f)
            );
        }
    }

    /// A closure handed to a higher-order function is monomorphised into
    /// the callee's instance, so its effects reach the caller.
    #[test]
    fn effects_closure_effects_reach_through_a_higher_order_call() {
        let r = from_source(
            "fn apply(f: Fn(i64) -> i64, x: i64) -> i64 {
    f(x)
}

fn main() {
    let a = apply(|v: i64| v + 1, 1);
    let b = apply(|v: i64| { println(\"hi\"); v }, 2);
}
",
        );
        assert!(
            effects(&r, "main").contains(&"writes(Stdout)".to_string()),
            "{:?}",
            effects(&r, "main")
        );
        let quiet: Vec<_> = r
            .summaries
            .iter()
            .filter(|(n, s)| {
                n.starts_with("apply") && !s.display().contains(&"writes(Stdout)".to_string())
            })
            .collect();
        assert_eq!(
            quiet.len(),
            1,
            "one apply instance prints and one does not: {:?}",
            r.summaries
        );
    }

    /// C10: a callee the pass cannot see into has every effect: a call
    /// through a value, a function with no body (an `extern`), and a
    /// library native this pass has no entry for.
    #[test]
    fn effects_unresolved_callees_have_every_effect() {
        let (p, tys) = from_mir(
            "
fn through_value(_1: i64) -> () {
    let mut _0: ();
    let _2: i64;
    bb0: {
        _2 = copy _1;
        _2 = copy _1;
        _0 = copy _2() -> bb1;
    }
    bb1: {
        return;
    }
}

fn foreign() -> () {
    let mut _0: ();
    bb0: {
        _0 = ext() -> bb1;
    }
    bb1: {
        return;
    }
}

fn odd_native() -> () {
    let mut _0: ();
    bb0: {
        _0 = Gadget.frob() -> bb1;
    }
    bb1: {
        return;
    }
}

fn caller() -> () {
    let mut _0: ();
    bb0: {
        _0 = foreign() -> bb1;
    }
    bb1: {
        return;
    }
}
",
            &["ext"],
        );
        let r = analyze(&p, &tys);
        for (f, why) in [
            ("through_value", "through_value: a call through a value"),
            ("foreign", "no body for ext"),
            ("odd_native", "native Gadget.frob"),
            ("caller", "no body for ext"),
        ] {
            let s = &r.summaries[f];
            assert!(s.unknown, "{f}: {s:?}");
            assert!(s.why.contains(why), "{f}: {:?}", s.why);
        }
        // Unknown conflicts with anything that has an effect, and not with
        // a function that has none.
        let none = |_: &Key, _: &Key| false;
        let mut print = EffectSet::default();
        print.add(Effect::new(Verb::Writes, "Stdout", Key::Any));
        assert!(sets_conflict(&r.summaries["foreign"], &print, &none));
        assert!(!sets_conflict(
            &r.summaries["foreign"],
            &EffectSet::default(),
            &none
        ));
    }

    /// Core §12: a call through a function-typed parameter has the effects
    /// of the argument each caller passes, forwarded through any number of
    /// parameters; an argument that names no function has every effect.
    #[test]
    fn effects_calls_through_a_parameter_take_the_arguments_effects() {
        let (mut p, tys) = from_mir(
            "
fn apply(_1: i64, _2: i64) -> i64 {
    let mut _0: i64;
    bb0: {
        _0 = copy _1(copy _2) -> bb1;
    }
    bb1: {
        return;
    }
}

fn forward(_1: i64, _2: i64) -> i64 {
    let mut _0: i64;
    bb0: {
        _0 = apply(copy _1, copy _2) -> bb1;
    }
    bb1: {
        return;
    }
}

fn noisy(_1: i64) -> i64 {
    let mut _0: i64;
    let _2: ();
    bb0: {
        _2 = println(copy _1) -> bb1;
    }
    bb1: {
        _0 = copy _1;
        return;
    }
}

fn quiet(_1: i64) -> i64 {
    let mut _0: i64;
    bb0: {
        _0 = copy _1;
        return;
    }
}

fn loud_caller() -> () {
    let mut _0: ();
    let _1: i64;
    bb0: {
        _1 = forward(const 0_i64, const 5_i64) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn quiet_caller() -> () {
    let mut _0: ();
    let _1: i64;
    bb0: {
        _1 = forward(const 0_i64, const 5_i64) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}

fn blind_caller() -> () {
    let mut _0: ();
    let _1: i64;
    bb0: {
        _1 = forward(const 0_i64, const 5_i64) -> bb1;
    }
    bb1: {
        _0 = const ();
        return;
    }
}
",
            &[],
        );
        // The text form has no function constants: pass `noisy` and
        // `quiet` by patching the first argument.
        let mut fn_const = |caller: &str, callee: &str| {
            let def = p.bodies[callee].instance.def;
            let op = Operand::Const(Const {
                ty: tys.intern(crate::mir::TyKind::FnDef(def)),
                kind: ConstKind::FnDef(p.bodies[callee].instance.clone()),
            });
            let body = p.bodies.get_mut(caller).unwrap();
            let TerminatorKind::Call { args, .. } = &mut body.blocks[0].terminator.kind else {
                unreachable!()
            };
            args[0] = op;
        };
        fn_const("loud_caller", "noisy");
        fn_const("quiet_caller", "quiet");
        let r = analyze(&p, &tys);
        assert_eq!(effects(&r, "apply"), ["calls(p1)"]);
        assert_eq!(effects(&r, "forward"), ["calls(p1)"]);
        assert!(!r.summaries["apply"].unknown);
        let loud = &r.summaries["loud_caller"];
        assert!(!loud.unknown && loud.calls.is_empty(), "{loud:?}");
        assert!(effects(&r, "loud_caller").contains(&"writes(Stdout)".to_string()));
        assert!(
            r.summaries["quiet_caller"].is_empty(),
            "{:?}",
            r.summaries["quiet_caller"]
        );
        assert!(r.summaries["blind_caller"].unknown);
        // A set that still calls a parameter orders against any effect.
        let none = |_: &Key, _: &Key| false;
        let mut print = EffectSet::default();
        print.add(Effect::new(Verb::Writes, "Stdout", Key::Any));
        assert!(sets_conflict(&r.summaries["apply"], &print, &none));
    }

    /// D6: a channel effect is keyed by the endpoint value. Inside a body
    /// the key is the parameter; at a call it becomes the argument's place.
    #[test]
    fn effects_channel_effects_are_keyed_by_the_endpoint() {
        let (p, tys) = from_mir(
            "
fn pipe(_1: ref Vec[i64], _2: ref Vec[i64]) -> () {
    let mut _0: ();
    let _3: ();
    bb0: {
        _3 = Sender[i64].send(copy _1, const 1_i64) -> bb1;
    }
    bb1: {
        _0 = Receiver[i64].recv(copy _2) -> bb2;
    }
    bb2: {
        return;
    }
}

fn main() -> () {
    let mut _0: ();
    let _1: Vec[i64]; // a
    let _2: Vec[i64]; // b
    let _3: ref Vec[i64];
    let _4: ref Vec[i64];
    bb0: {
        _1 = Vec[i64].new() -> bb1;
    }
    bb1: {
        _2 = Vec[i64].new() -> bb2;
    }
    bb2: {
        _3 = &_1;
        _4 = &_2;
        _0 = pipe(move _3, move _4) -> bb3;
    }
    bb3: {
        return;
    }
}
",
            &[],
        );
        let r = analyze(&p, &tys);
        assert_eq!(
            effects(&r, "pipe"),
            ["sends(Channel @ p1)", "receives(Channel @ p2)", "blocks"],
        );
        let site = r.sites["main"].iter().find(|s| s.block.0 == 2).unwrap();
        assert_eq!(
            site.effects.display(),
            ["sends(Channel @ l1)", "receives(Channel @ l2)", "blocks"],
        );
        // The summary cannot name `main`'s locals, which no caller can
        // reach: they are fresh.
        assert_eq!(
            effects(&r, "main")[..2],
            [
                "sends(Channel @ fresh)".to_string(),
                "receives(Channel @ fresh)".to_string()
            ],
        );
    }

    /// Child processes, the HTTP client and files have entries, so a
    /// program that uses them is not left with every effect.
    #[test]
    fn effects_process_http_and_files_are_known() {
        let (p, tys) = from_mir(
            "
fn io(_1: ref Vec[i64], _2: ref Vec[i64], _3: ref Vec[i64]) -> () {
    let mut _0: ();
    let _4: ();
    let _5: ();
    bb0: {
        _4 = Child.wait(copy _1) -> bb1;
    }
    bb1: {
        _5 = Client.get(copy _2, const 1_i64) -> bb2;
    }
    bb2: {
        _0 = File.seek(copy _3, const 0_i64) -> bb3;
    }
    bb3: {
        return;
    }
}
",
            &[],
        );
        let r = analyze(&p, &tys);
        assert!(!r.summaries["io"].unknown, "{:?}", r.summaries["io"].why);
        assert_eq!(
            effects(&r, "io"),
            [
                "writes(File @ p3)",
                "sends(Network @ fresh)",
                "sends(ProcessTable @ p1)",
                "receives(Network @ fresh)",
                "allocates(Heap)",
                "blocks",
            ],
        );
    }

    /// D6, the conflict rule itself.
    #[test]
    fn effects_d6_conflicts_follow_verb_class_and_key() {
        let e = |verb, class, key| Effect::new(verb, class, key);
        let a = Key::Local(1, vec![]);
        let b = Key::Local(2, vec![]);
        let distinct = |_: &Key, _: &Key| false;
        let aliased = |_: &Key, _: &Key| true;
        use Verb::*;
        // The same verb on the same connection keeps its order.
        assert!(conflicts(
            &e(Sends, "Network", a.clone()),
            &e(Sends, "Network", a.clone()),
            &distinct
        ));
        assert!(conflicts(
            &e(Receives, "Channel", a.clone()),
            &e(Receives, "Channel", a.clone()),
            &distinct
        ));
        // Two connections that cannot alias run in either order.
        assert!(!conflicts(
            &e(Sends, "Network", a.clone()),
            &e(Sends, "Network", b.clone()),
            &distinct
        ));
        assert!(conflicts(
            &e(Sends, "Network", a.clone()),
            &e(Sends, "Network", b.clone()),
            &aliased
        ));
        // Sends and receives never conflict, even on one connection.
        assert!(!conflicts(
            &e(Sends, "Network", a.clone()),
            &e(Receives, "Network", a.clone()),
            &aliased
        ));
        // An unkeyed effect overlaps every value of its class, not other classes.
        assert!(conflicts(
            &e(Sends, "Network", Key::Any),
            &e(Sends, "Network", b.clone()),
            &distinct
        ));
        assert!(!conflicts(
            &e(Sends, "Network", Key::Any),
            &e(Sends, "Channel", Key::Any),
            &distinct
        ));
        // Reads and writes: a write conflicts with a read or a write, two reads do not.
        assert!(conflicts(
            &e(Reads, "File", a.clone()),
            &e(Writes, "File", a.clone()),
            &distinct
        ));
        assert!(conflicts(
            &e(Writes, "Stdout", Key::Any),
            &e(Writes, "Stdout", Key::Any),
            &distinct
        ));
        assert!(!conflicts(
            &e(Reads, "File", a.clone()),
            &e(Reads, "File", a.clone()),
            &distinct
        ));
        // A field is part of its value; two different fields are disjoint.
        let f0 = Key::Local(1, vec![0]);
        let f1 = Key::Local(1, vec![1]);
        assert!(conflicts(
            &e(Sends, "Channel", a.clone()),
            &e(Sends, "Channel", f0.clone()),
            &distinct
        ));
        assert!(!conflicts(
            &e(Sends, "Channel", f0),
            &e(Sends, "Channel", f1),
            &distinct
        ));
        // A fresh value, or a capability no value roots, overlaps nothing.
        assert!(!conflicts(
            &e(Sends, "Network", Key::Fresh),
            &e(Sends, "Network", Key::Any),
            &aliased
        ));
        assert!(!conflicts(
            &e(Writes, "FileSystem", Key::Fresh),
            &e(Writes, "FileSystem", Key::Fresh),
            &aliased
        ));
        // An unknown set conflicts with an ordered effect, but not with a
        // fresh one or a capability verb.
        let set = |es: Vec<Effect>| EffectSet {
            effects: es.into_iter().collect(),
            ..EffectSet::default()
        };
        let unknown = EffectSet::unknown("test".to_string());
        assert!(sets_conflict(
            &unknown,
            &set(vec![e(Reads, "File", b.clone())]),
            &distinct
        ));
        assert!(!sets_conflict(
            &unknown,
            &set(vec![
                e(Sends, "Channel", Key::Fresh),
                e(Allocates, "Heap", Key::Any)
            ]),
            &distinct
        ));
        // Capability verbs never order anything.
        assert!(!conflicts(
            &e(Allocates, "Heap", Key::Any),
            &e(Allocates, "Heap", Key::Any),
            &aliased
        ));
        assert!(!conflicts(
            &e(Blocks, "", Key::Any),
            &e(Blocks, "", Key::Any),
            &aliased
        ));
    }

    fn par_conflicts_of(r: &EffectReport, name: &str) -> Vec<(usize, usize, String, String)> {
        r.par_conflicts.get(name).map_or(Vec::new(), |v| {
            v.iter()
                .map(|c| {
                    (
                        c.branches.0,
                        c.branches.1,
                        c.first.1.clone(),
                        c.second.1.clone(),
                    )
                })
                .collect()
        })
    }

    /// Core §11.2 with D6 keys: `par` branches that send on one channel
    /// (through a clone) conflict, two channels do not; a `par for` body
    /// conflicts with itself on a channel from outside, not on one each
    /// iteration makes; printing conflicts, and so does a `Drop` body that
    /// prints, which only the blocks drop elaboration adds reach.
    #[test]
    fn effects_par_branches_conflict_on_shared_resources() {
        let r = from_source(
            "struct Loud {
    n: i64,
}

impl Drop for Loud {
    fn drop(mut ref self) {
        println(self.n);
    }
}

fn produce(tx: Sender[i64], from: i64, to: i64) -> i64 {
    for i in from..to { tx.send(i); }
    to - from
}

fn two_channels() {
    let (tx1, rx1): (Sender[i64], Receiver[i64]) = Channel.new();
    let (tx2, rx2): (Sender[i64], Receiver[i64]) = Channel.new();
    let (r1, r2) = par { produce(tx1, 0, 4), produce(tx2, 4, 8) };
}

fn one_channel() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let tx2 = tx.clone();
    let (r1, r2) = par { produce(tx, 0, 4), produce(tx2, 4, 8) };
}

fn outer_tx() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let v = par for i in 0..3 { tx.send(i); i };
}

fn local_tx() {
    let v = par for i in 0..3 {
        let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
        tx.send(i);
        i
    };
}

fn prints() {
    let (a, b) = par { println(\"a\"), println(\"b\") };
}

fn drops() {
    let (a, b) = par { { let x = Loud { n: 1 }; 1 }, { let y = Loud { n: 2 }; 2 } };
}

fn quiet() {
    let (a, b) = par { 1 + 2, 3 * 4 };
}

fn main() {
    two_channels();
    one_channel();
    outer_tx();
    local_tx();
    prints();
    drops();
    quiet();
}
",
        );
        for name in ["two_channels", "local_tx", "quiet"] {
            assert_eq!(par_conflicts_of(&r, name), [], "{name}");
        }
        let one = par_conflicts_of(&r, "one_channel");
        assert_eq!(one.len(), 1, "{one:?}");
        assert_eq!((one[0].0, one[0].1), (0, 1));
        assert!(one[0].2.starts_with("sends(Channel @ l"), "{one:?}");
        let outer = par_conflicts_of(&r, "outer_tx");
        assert_eq!(outer.len(), 1, "{outer:?}");
        assert_eq!((outer[0].0, outer[0].1), (0, 0));
        for name in ["prints", "drops"] {
            assert_eq!(
                par_conflicts_of(&r, name),
                [(0, 1, "writes(Stdout)".into(), "writes(Stdout)".into())],
                "{name}"
            );
        }
    }

    /// Two client calls open two connections: the `Network` capability
    /// does not order them. Two writes on files a caller passed do conflict,
    /// since the caller can pass one file twice.
    #[test]
    fn effects_par_capabilities_do_not_conflict_but_parameters_may_alias() {
        let (p, tys) = from_mir(
            "
fn fetch(_1: ref Vec[i64], _2: ref Vec[i64]) -> () {
    let mut _0: ();
    let _3: ();
    let _4: ();
    par#0 block [bb1] [bb2]
    bb0: {
        goto -> bb1;
    }
    bb1: {
        _3 = Client.get(copy _1, const 1_i64) -> bb2;
    }
    bb2: {
        _4 = Client.get(copy _2, const 2_i64) -> bb3;
    }
    bb3: {
        return;
    }
}

fn store(_1: ref Vec[i64], _2: ref Vec[i64]) -> () {
    let mut _0: ();
    let _3: ();
    let _4: ();
    par#0 block [bb1] [bb2]
    bb0: {
        goto -> bb1;
    }
    bb1: {
        _3 = File.write(copy _1, const 1_i64) -> bb2;
    }
    bb2: {
        _4 = File.write(copy _2, const 2_i64) -> bb3;
    }
    bb3: {
        return;
    }
}
",
            &[],
        );
        let r = analyze(&p, &tys);
        assert_eq!(par_conflicts_of(&r, "fetch"), []);
        assert_eq!(
            par_conflicts_of(&r, "store"),
            [(0, 1, "writes(File @ p1)".into(), "writes(File @ p2)".into())]
        );
    }

    /// A recursion that walks deeper into its parameter each round still
    /// stops: keys are cut to a fixed path length.
    #[test]
    fn effects_key_paths_are_bounded() {
        let mut s = EffectSet::default();
        s.add(Effect::new(
            Verb::Sends,
            "Channel",
            Key::Param(1, vec![0; MAX_PATH]),
        ));
        let out = substitute(&s, &|_| Key::Param(1, vec![0]), &|_| opaque());
        assert_eq!(
            out.effects.iter().next().unwrap().key,
            Key::Param(1, vec![0; MAX_PATH])
        );
    }
}
