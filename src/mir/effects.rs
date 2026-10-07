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

use std::collections::{BTreeMap, BTreeSet};

use super::interp::Program;
use super::place_ty::place_ty;
use super::syntax::*;
use super::ty::{AdtId, Ty, TyInterner};
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
}

impl Key {
    fn widen_locals(self) -> Key {
        match self {
            Key::Local(..) => Key::Any,
            k => k,
        }
    }
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
                            x.key = x.key.widen_locals();
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
    report
}

impl EffectReport {
    /// `{"functions": [{"instance", "effects", "unknown_because", "sites":
    /// [{"block", "effects"}]}]}`, instances in name order: the output of
    /// `karac __mir-effects`.
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
                json!({
                    "instance": name,
                    "effects": s.display(),
                    "unknown_because": s.why.iter().collect::<Vec<_>>(),
                    "sites": sites,
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
/// with anything that has an effect, and so does a set that still calls a
/// parameter's function value, whose effects only the caller knows.
pub fn sets_conflict(a: &EffectSet, b: &EffectSet, may_alias: &dyn Fn(&Key, &Key) -> bool) -> bool {
    let top = |s: &EffectSet| s.unknown || !s.calls.is_empty();
    if (top(a) && !b.is_empty()) || (top(b) && !a.is_empty()) {
        return true;
    }
    a.effects
        .iter()
        .any(|x| b.effects.iter().any(|y| conflicts(x, y, may_alias)))
}

fn keys_overlap(a: &Key, b: &Key, may_alias: &dyn Fn(&Key, &Key) -> bool) -> bool {
    match (a, b) {
        (Key::Any, _) | (_, Key::Any) => true,
        (Key::Param(r1, p1) | Key::Local(r1, p1), Key::Param(r2, p2) | Key::Local(r2, p2)) => {
            let same_root = r1 == r2 && std::mem::discriminant(a) == std::mem::discriminant(b);
            if same_root {
                // One path is a prefix of the other: the same value or a
                // part of it.
                let n = p1.len().min(p2.len());
                p1[..n] == p2[..n]
            } else {
                may_alias(a, b)
            }
        }
    }
}

/// The default aliasing rule for one body: two distinct roots may name the
/// same value when either is a shared reference (`ref T`); owned values
/// and `mut ref`s are unique.
pub fn may_alias_in<'b>(body: &'b Body, tys: &'b TyInterner) -> impl Fn(&Key, &Key) -> bool + 'b {
    move |a: &Key, b: &Key| {
        let shared_ref = |k: &Key| match k {
            Key::Param(r, _) | Key::Local(r, _) => body
                .locals
                .get(*r as usize)
                .is_some_and(|d| matches!(tys.tcx().kind(d.ty), SK::Ref(_))),
            Key::Any => true,
        };
        shared_ref(a) || shared_ref(b)
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
        ("Sender", "send") => vec![(Sends, "Channel", Some(0)), (Blocks, "", None)],
        ("Receiver", "recv") => vec![(Receives, "Channel", Some(0)), (Blocks, "", None)],
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
        // Paths name files by string value, which a key cannot follow: the
        // whole file system is the resource.
        ("fs", "write" | "append" | "remove" | "create_dir") => {
            vec![(Writes, "FileSystem", None), (Blocks, "", None)]
        }
        ("fs", "read_to_string" | "read" | "exists") => {
            vec![(Reads, "FileSystem", None), (Blocks, "", None)]
        }
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
                    let key = key_arg.map_or(Key::Any, arg_key);
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
        let part_key = match &key {
            Key::Any => Key::Any,
            k => k.clone(),
        };
        for part in self.parts(ty) {
            self.drop_glue(part, part_key.clone(), summaries, seen, eff);
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
        // The summary cannot name `main`'s locals.
        assert_eq!(
            effects(&r, "main")[..2],
            [
                "sends(Channel)".to_string(),
                "receives(Channel)".to_string()
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
