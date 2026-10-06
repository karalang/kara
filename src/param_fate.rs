//! Per-parameter FATE — the first fact of ownership Slice 4's structural half
//! (`docs/spikes/ownership-drop-judgment.md`, § 8 "What remains").
//!
//! design.md § Drop ordering, rule 3: a by-value parameter's `Drop` body runs
//! in the CALLER at the end of the call, unless the value's ownership LEAVES
//! the callee — returned, carried out inside a returned aggregate, or stored
//! into a place that outlives the call. Whether it leaves is therefore the one
//! question every by-value-param decision on both backends asks, and today it
//! is asked through some fifty `crate::ast::fn_*` predicates
//! (`fn_always_returns_param`, `fn_conditionally_returns_param_bare`,
//! `fn_moves_param_into_outliving_place`, …), each answering a slightly
//! different spelling of it with its own traversal (B-2026-09-20-67's census).
//!
//! This module answers it ONCE, per exit path: [`param_fate`] walks the body
//! forking at every branch, follows the parameter through every local that
//! comes to hold it (`let q = p`, `let h = H { r: p }`, `v.push(p)` into a
//! local `v`, `if c { p } else { .. }`), and records, for each way the
//! function can exit, what became of it ([`ExitFate`]). Every predicate's
//! answer is then a COVERAGE question over that one list ([`Coverage`]).
//!
//! Step 1 consumes it only as an AUDIT: with `KARAC_DROP_SCHEDULE=audit` and
//! `KARAC_DROP_SCHEDULE_AUDIT=<path>`, the legacy predicates log every input
//! on which their answer and the fact's disagree (see [`audit`]). Nothing in
//! the default build reads it, so it changes no program's behaviour.
//!
//! Conservative by construction: a shape the walk cannot follow (a holder
//! captured by a closure, handed to a method it cannot resolve, a path
//! explosion) makes that exit [`ExitFate::Unknown`] rather than a guess, and
//! [`ParamFate::is_exact`] says whether any exit is unknown.

use crate::ast::*;
use std::collections::BTreeSet;

/// What became of the parameter on one exit path.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum ExitFate {
    /// Still owned by the callee's frame at exit (or died inside it): the
    /// caller runs its body at the end of the call (rule 3).
    Stays,
    /// The returned value carries the WHOLE parameter — bare, or inside an
    /// aggregate literal / variant constructor.
    Returned,
    /// The returned value carries a PART of it (a field, element or payload
    /// projected out), not the whole.
    ReturnedPart,
    /// Stored, whole or in part, into a place that outlives the call
    /// (`self`, a `ref`/`mut ref` parameter, or something reached through one).
    Stored,
    /// The walk could not follow it on this path.
    Unknown,
}

/// How many exits a property holds on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coverage {
    Never,
    Some,
    Always,
}

/// The fate of one by-value parameter, per exit path (in walk order;
/// duplicates removed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamFate {
    pub exits: Vec<ExitFate>,
    /// No path moved a PART of the parameter or consumed it inside the
    /// callee: every exit is a whole-value answer.
    pub whole_only: bool,
}

impl ParamFate {
    fn coverage(&self, p: impl Fn(ExitFate) -> bool) -> Coverage {
        let n = self.exits.iter().filter(|e| p(**e)).count();
        if n == 0 {
            Coverage::Never
        } else if n == self.exits.len() {
            Coverage::Always
        } else {
            Coverage::Some
        }
    }
    /// The whole parameter comes back in the result.
    pub fn returned_whole(&self) -> Coverage {
        self.coverage(|e| e == ExitFate::Returned)
    }
    /// The whole parameter or a part of it comes back.
    pub fn returned_any(&self) -> Coverage {
        self.coverage(|e| matches!(e, ExitFate::Returned | ExitFate::ReturnedPart))
    }
    pub fn stored(&self) -> Coverage {
        self.coverage(|e| e == ExitFate::Stored)
    }
    /// Ownership of (some of) the parameter leaves the callee.
    pub fn leaves(&self) -> Coverage {
        self.coverage(|e| {
            matches!(
                e,
                ExitFate::Returned | ExitFate::ReturnedPart | ExitFate::Stored
            )
        })
    }
    /// No exit is [`ExitFate::Unknown`].
    pub fn is_exact(&self) -> bool {
        !self.exits.contains(&ExitFate::Unknown)
    }
    /// Compact rendering for the audit log: `R|S|s|r|?` per exit.
    pub fn render(&self) -> String {
        self.exits
            .iter()
            .map(|e| match e {
                ExitFate::Stays => 's',
                ExitFate::Returned => 'R',
                ExitFate::ReturnedPart => 'r',
                ExitFate::Stored => 'S',
                ExitFate::Unknown => '?',
            })
            .collect()
    }
}

/// The fate of `f`'s positional parameter `idx`, or `None` when it is not a
/// by-value binding parameter (a `ref` / `mut ref` param, or a pattern param).
/// `program` resolves user callees so a hand-back through a call is followed.
pub fn param_fate(program: Option<&Program>, f: &Function, idx: usize) -> Option<ParamFate> {
    param_fate_depth(program, f, idx, 0)
}

/// `(Function address, span offset, span length, param index, call depth,
/// Program address or 0)`, with the function's name and parameter types as a
/// check against an address reused by a later clone (a monomorph shares its
/// template's span).
type FateMemoKey = (usize, usize, usize, usize, usize, usize);
type FateMemoMap = rustc_hash::FxHashMap<FateMemoKey, (u64, Option<ParamFate>)>;
thread_local! {
    static FATE_MEMO: std::cell::RefCell<Option<FateMemoMap>> =
        const { std::cell::RefCell::new(None) };
}

/// Answers [`param_fate`] once per (function, param) for one compile or one
/// interpreter run, until dropped. The walk follows callees, and with the fact
/// answering by default every ownership predicate asks it at every call site:
/// unmemoized, building the self-hosted parser went from 28 s to over ten
/// minutes. Off outside a guard, so a process holding several programs never
/// sees another's answer. Nested guards share the outermost's map.
pub struct FateMemo {
    outermost: bool,
}

impl FateMemo {
    pub fn enable() -> Self {
        let outermost = FATE_MEMO.with(|m| {
            let mut m = m.borrow_mut();
            if m.is_none() {
                *m = Some(FateMemoMap::default());
                true
            } else {
                false
            }
        });
        FateMemo { outermost }
    }
}

impl Drop for FateMemo {
    fn drop(&mut self) {
        if self.outermost {
            FATE_MEMO.with(|m| *m.borrow_mut() = None);
        }
    }
}

/// The check is a structural hash rather than the types' `Debug` text: it is
/// computed on every lookup, hits included, and rendering every param type per
/// call made the memo the hottest thing in a call-heavy `--interp` run (a third
/// of the instructions, measured by the kata thread 2026-10-03).
fn fate_memo_check(f: &Function) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    f.name.hash(&mut h);
    f.params.len().hash(&mut h);
    for p in &f.params {
        ty_fingerprint(&p.ty, &mut h);
    }
    h.finish()
}

/// Enough of a type's shape to tell one instantiation from another: the kind,
/// a path's segments and its type arguments, recursively.
fn ty_fingerprint(t: &TypeExpr, h: &mut rustc_hash::FxHasher) {
    use std::hash::Hash;
    std::mem::discriminant(&t.kind).hash(h);
    match &t.kind {
        TypeKind::Path(p) => {
            p.segments.hash(h);
            for a in p.generic_args.iter().flatten() {
                match a {
                    GenericArg::Type(t) => ty_fingerprint(t, h),
                    other => std::mem::discriminant(other).hash(h),
                }
            }
        }
        TypeKind::Tuple(ts) => ts.iter().for_each(|t| ty_fingerprint(t, h)),
        TypeKind::Ref(t)
        | TypeKind::MutRef(t)
        | TypeKind::Frozen(t)
        | TypeKind::MutSlice(t)
        | TypeKind::Weak(t) => ty_fingerprint(t, h),
        TypeKind::Array { element, .. } => ty_fingerprint(element, h),
        TypeKind::Pointer { inner, .. } => ty_fingerprint(inner, h),
        _ => (t.span.offset, t.span.length).hash(h),
    }
}

const MAX_CALL_DEPTH: usize = 4;
const MAX_PATHS: usize = 256;

fn param_fate_depth(
    program: Option<&Program>,
    f: &Function,
    idx: usize,
    depth: usize,
) -> Option<ParamFate> {
    // A borrowed or pattern param has no fate; answer before the memo, as the
    // uncached walk would.
    let p = f.params.get(idx)?;
    if is_borrow_ty(&p.ty) || !matches!(p.pattern.kind, PatternKind::Binding(_)) {
        return None;
    }
    if !FATE_MEMO.with(|m| m.borrow().is_some()) {
        return param_fate_depth_uncached(program, f, idx, depth);
    }
    let key = (
        f as *const Function as usize,
        f.span.offset,
        f.span.length,
        idx,
        depth,
        program.map_or(0, |p| p as *const Program as usize),
    );
    let check = fate_memo_check(f);
    let hit = FATE_MEMO.with(|m| {
        m.borrow()
            .as_ref()
            .and_then(|m| m.get(&key))
            .filter(|(c, _)| *c == check)
            .map(|(_, v)| v.clone())
    });
    if let Some(v) = hit {
        return v;
    }
    let v = param_fate_depth_uncached(program, f, idx, depth);
    FATE_MEMO.with(|m| {
        if let Some(m) = m.borrow_mut().as_mut() {
            m.insert(key, (check, v.clone()));
        }
    });
    v
}

fn param_fate_depth_uncached(
    program: Option<&Program>,
    f: &Function,
    idx: usize,
    depth: usize,
) -> Option<ParamFate> {
    let p = f.params.get(idx)?;
    if is_borrow_ty(&p.ty) {
        return None;
    }
    let PatternKind::Binding(name) = &p.pattern.kind else {
        return None;
    };
    let mut w = Walker {
        program,
        self_name: f.name.clone(),
        param_name: name.clone(),
        depth,
        borrows: f
            .params
            .iter()
            .filter(|q| is_borrow_ty(&q.ty))
            .filter_map(|q| match &q.pattern.kind {
                PatternKind::Binding(n) => Some(n.clone()),
                _ => None,
            })
            // A BORROWED receiver outlives the call; a by-value `self` is
            // this frame's own local, and a store into it is a hold.
            .chain(
                matches!(f.self_param, Some(SelfParam::Ref) | Some(SelfParam::MutRef))
                    .then(|| "self".to_string()),
            )
            .collect(),
        exits: Vec::new(),
        overflow: false,
        moved: false,
        param_ty: Some(p.ty.clone()),
        track: false,
        exit_states: Vec::new(),
        name_tys: Default::default(),
    };
    let mut s = PState::default();
    s.whole.insert(name.clone());
    let outs = w.block(&f.body, s);
    for (st, y) in outs {
        w.exit(&st, y);
    }
    if w.overflow {
        return Some(ParamFate {
            exits: vec![ExitFate::Unknown],
            whole_only: false,
        });
    }
    // B-2026-09-27-95 — a `let mut` rebind that is mutated in place keeps the
    // value in this frame, but the value that dies is the local's, with the
    // mutation in it: the callee owns it (the legacy local-container
    // handover), which no whole-value answer expresses.
    let whole_only = !w.moved && crate::ast::param_mutated_rebind_local(f, idx).is_none();
    let mut exits = w.exits;
    if exits.is_empty() {
        // Every path diverges (panic / infinite loop): nothing leaves.
        exits.push(ExitFate::Stays);
    }
    let mut seen = Vec::new();
    exits.retain(|e| {
        if seen.contains(e) {
            false
        } else {
            seen.push(*e);
            true
        }
    });
    Some(ParamFate { exits, whole_only })
}

fn is_scalar_ty(t: &TypeExpr) -> bool {
    let TypeKind::Path(p) = &t.kind else {
        return false;
    };
    p.segments.len() == 1 && is_scalar_type_name(&p.segments[0])
}

/// The declared type of field `field` of the non-generic struct `t` names.
fn field_ty<'p>(program: &'p Program, t: &TypeExpr, field: &str) -> Option<&'p TypeExpr> {
    let TypeKind::Path(p) = &t.kind else {
        return None;
    };
    if p.segments.len() != 1 || p.generic_args.is_some() {
        return None;
    }
    program.items.iter().find_map(|item| match item {
        Item::StructDef(sd) if sd.name == p.segments[0] && sd.generic_params.is_none() => {
            sd.fields.iter().find(|f| f.name == field).map(|f| &f.ty)
        }
        _ => None,
    })
}

fn is_borrow_ty(t: &TypeExpr) -> bool {
    matches!(t.kind, TypeKind::Ref(_) | TypeKind::MutRef(_))
}

/// What a value carries of the parameter.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
enum Yield {
    None,
    Part,
    Whole,
}

fn join(a: Yield, b: Yield) -> Yield {
    a.max(b)
}

/// One path's state.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct PState {
    /// Locals holding the whole parameter (directly or inside an aggregate).
    whole: BTreeSet<String>,
    /// Locals holding a part of it.
    part: BTreeSet<String>,
    /// Some of it was stored into an outliving place on this path.
    stored: bool,
    /// The walk lost track of it on this path.
    unknown: bool,
    /// A part of it moved somewhere, or the whole was consumed inside the
    /// callee (discarded, or held by a local container). The exit still
    /// classifies, but whole-value consumers must not act on it alone.
    moved: bool,
    /// Step 4 (arm bindings) only: the value was handed somewhere by value —
    /// bound to a local, assigned, or passed to a callee's by-value
    /// parameter. Never set while following a parameter, so a param fate's
    /// path set is unchanged.
    consumed: bool,
    /// Step 4 only: names a pattern inside the followed arm bound. They die
    /// with the arm or the function, so holding the value there is not an
    /// enclosing local holding it.
    inner: BTreeSet<String>,
}

impl PState {
    fn yield_of(&self, n: &str) -> Yield {
        if self.whole.contains(n) {
            Yield::Whole
        } else if self.part.contains(n) {
            Yield::Part
        } else {
            Yield::None
        }
    }
    fn set(&mut self, n: &str, y: Yield) {
        self.whole.remove(n);
        self.part.remove(n);
        match y {
            Yield::Whole => {
                self.whole.insert(n.to_string());
            }
            Yield::Part => {
                self.part.insert(n.to_string());
            }
            Yield::None => {}
        }
    }
}

type Outs = Vec<(PState, Yield)>;

struct Walker<'p> {
    program: Option<&'p Program>,
    self_name: String,
    /// The parameter being followed.
    param_name: String,
    depth: usize,
    /// `ref` / `mut ref` parameter names: stores through these outlive the call.
    borrows: BTreeSet<String>,
    exits: Vec<ExitFate>,
    overflow: bool,
    /// Some exit's path set `PState::moved`.
    moved: bool,
    /// The parameter's declared type, for [`Walker::scalar_param_projection`].
    param_ty: Option<TypeExpr>,
    /// Step 4: following an ARM binding rather than a parameter — record
    /// [`PState::consumed`] and every exit's state.
    track: bool,
    /// With `track`, each `return`'s state and what it carries.
    exit_states: Vec<(PState, Yield)>,
    /// With `track`, the declared types of names a nested pattern bound out
    /// of a non-generic user enum, so a scalar field read off one is a read.
    name_tys: std::collections::HashMap<String, TypeExpr>,
}

fn dedup(mut v: Outs) -> Outs {
    v.sort();
    v.dedup();
    v
}

impl Walker<'_> {
    fn exit(&mut self, s: &PState, y: Yield) {
        if self.track {
            self.exit_states.push((s.clone(), y));
        }
        self.moved |= s.moved || y == Yield::Part;
        let fate = if s.unknown {
            ExitFate::Unknown
        } else {
            match y {
                Yield::Whole => ExitFate::Returned,
                Yield::Part => ExitFate::ReturnedPart,
                Yield::None if s.stored => ExitFate::Stored,
                Yield::None => ExitFate::Stays,
            }
        };
        self.exits.push(fate);
    }

    /// Is `e` a field chain rooted at the parameter itself (`r.id`,
    /// `w.r.id`) whose type is scalar?
    fn scalar_param_projection(&self, e: &Expr, s: &PState) -> bool {
        let Some(program) = self.program else {
            return false;
        };
        // A field name, or (step 4 only) a tuple index.
        let mut fields: Vec<Result<&str, u64>> = Vec::new();
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, field } => {
                    fields.push(Ok(field.as_str()));
                    cur = object;
                }
                ExprKind::TupleIndex { object, index } if self.track => {
                    fields.push(Err(*index));
                    cur = object;
                }
                _ => break,
            }
        }
        if fields.is_empty() {
            return false;
        }
        let ty = match &cur.kind {
            ExprKind::Identifier(n) if *n == self.param_name && s.whole.contains(n) => {
                self.param_ty.as_ref()
            }
            // Step 4: a name a nested pattern bound a part into.
            ExprKind::Identifier(n) if self.track && s.part.contains(n) => self.name_tys.get(n),
            _ => None,
        };
        let Some(ty) = ty else {
            return false;
        };
        let mut t = ty;
        for f in fields.iter().rev() {
            let next = match f {
                Ok(f) => field_ty(program, t, f),
                Err(i) => match &t.kind {
                    TypeKind::Tuple(ts) => ts.get(*i as usize),
                    _ => None,
                },
            };
            match next {
                Some(next) => t = next,
                None => return false,
            }
        }
        is_scalar_ty(t)
    }

    fn cap(&mut self, v: Outs) -> Outs {
        let v = dedup(v);
        if v.len() > MAX_PATHS {
            self.overflow = true;
            return v.into_iter().take(1).collect();
        }
        v
    }

    // ── blocks and statements ─────────────────────────────────────────

    fn block(&mut self, b: &Block, s: PState) -> Outs {
        let entry = s.clone();
        let mut states = vec![s];
        for st in &b.stmts {
            let mut next = Vec::new();
            for s in states {
                next.extend(self.stmt(st, s));
            }
            states = self
                .cap(next.into_iter().map(|s| (s, Yield::None)).collect())
                .into_iter()
                .map(|(s, _)| s)
                .collect();
            if states.is_empty() {
                return Vec::new();
            }
        }
        let mut outs = Vec::new();
        for s in states {
            match &b.final_expr {
                Some(e) => outs.extend(self.expr(e, s)),
                None => outs.push((s, Yield::None)),
            }
        }
        // Names this block's own `let`s introduced go out of scope: restore
        // their membership from the entry state, so a block-local holder does
        // not shadow an outer binding of the same name after the block.
        let names = block_let_names(b);
        for (s, _) in outs.iter_mut() {
            for n in &names {
                let y = entry.yield_of(n);
                s.set(n, y);
            }
        }
        self.cap(outs)
    }

    fn stmt(&mut self, st: &Stmt, s: PState) -> Vec<PState> {
        match &st.kind {
            StmtKind::Let { pattern, value, .. } => {
                let param = self.param_name.clone();
                // B-2026-10-05-107 — `let (v, n) = (p, 1)`, directly or out of
                // a branch whose every leaf is such a literal: each name takes
                // ITS element's yield, so `v` holds the whole param rather
                // than the part the joined tuple's yield would give it.
                if let PatternKind::Tuple(ps) = &pattern.kind {
                    if tuple_literal_leaves(value, ps.len()) {
                        return self
                            .expr_elems(value, s, ps.len())
                            .into_iter()
                            .map(|(mut s, ys)| {
                                s.moved |= pattern_names(pattern)
                                    .iter()
                                    .any(|n| *n != param && s.yield_of(n) != Yield::None);
                                for (p, y) in ps.iter().zip(ys) {
                                    s.consumed |= self.track && y != Yield::None;
                                    bind(&mut s, p, y, self.track);
                                }
                                s
                            })
                            .collect();
                    }
                }
                self.expr(value, s)
                    .into_iter()
                    .map(|(mut s, y)| {
                        // Shadowing a LOCAL holder: the consumers key their
                        // slots by name and lose the shadowed one, so this
                        // fate is not a whole-value answer for them. The
                        // parameter's own name is the exception rule 3 settles.
                        s.moved |= pattern_names(pattern)
                            .iter()
                            .any(|n| *n != param && s.yield_of(n) != Yield::None);
                        s.consumed |= self.track && y != Yield::None;
                        bind(&mut s, pattern, y, self.track);
                        s
                    })
                    .collect()
            }
            StmtKind::LetUninit { name, .. } => {
                let mut s = s;
                s.set(name, Yield::None);
                vec![s]
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                let mut out = Vec::new();
                for (s, y) in self.expr(value, s) {
                    // The else block diverges; any path out of it is an exit
                    // the walk records itself (`return`), so its fall-through
                    // states are dropped.
                    let _ = self.block(else_block, s.clone());
                    let mut s = s;
                    s.consumed |= self.track && y != Yield::None;
                    bind(&mut s, pattern, y, self.track);
                    out.push(s);
                }
                out
            }
            StmtKind::Assign { target, value } => self
                .expr(value, s)
                .into_iter()
                .map(|(s, y)| self.assign(target, s, y))
                .collect(),
            StmtKind::MultiAssign { targets, values } => {
                let mut states = vec![s];
                for (t, v) in targets.iter().zip(values) {
                    let mut next = Vec::new();
                    for s in states {
                        for (s, y) in self.expr(v, s) {
                            next.push(self.assign(t, s, y));
                        }
                    }
                    states = next;
                }
                states
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                let mut out = Vec::new();
                for (s, _) in self.expr(value, s) {
                    out.extend(self.expr(target, s).into_iter().map(|(s, _)| s));
                }
                out
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                // A deferred block runs at exit; a hand-off inside it is not
                // modelled.
                let mut ns = Vec::new();
                collect_block_names(body, &mut ns);
                let mut s = s;
                if ns.iter().any(|n| s.yield_of(n) != Yield::None) {
                    s.unknown = true;
                }
                vec![s]
            }
            StmtKind::Expr(e) => self
                .expr(e, s)
                .into_iter()
                .map(|(mut s, y)| {
                    s.moved |= y != Yield::None;
                    s
                })
                .collect(),
        }
    }

    /// B-2026-10-05-107 — `e`, every value leaf of which is a tuple literal of
    /// `n` elements ([`tuple_literal_leaves`]), evaluated ELEMENT BY ELEMENT:
    /// the live paths, each with what every element carries. The `let (v, k)
    /// = ..` that consumes it binds each name to its own element's yield, so
    /// `let (v, n) = (p, 1); v` hands the WHOLE param back where binding the
    /// tuple's joined yield as a part said it only handed back a piece of it.
    fn expr_elems(&mut self, e: &Expr, s: PState, n: usize) -> Vec<(PState, Vec<Yield>)> {
        match &e.kind {
            ExprKind::Tuple(es) => {
                let mut states = vec![(s, Vec::new())];
                for x in es {
                    let mut next = Vec::new();
                    for (s, ys) in states {
                        for (s, y) in self.expr(x, s) {
                            let mut ys = ys.clone();
                            ys.push(y);
                            next.push((s, ys));
                        }
                    }
                    states = self.cap_elems(next);
                }
                states
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Seq(b) => {
                self.block_elems(b, s, n)
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch: Some(els),
                ..
            } => {
                let mut out = Vec::new();
                for (s, _) in self.expr(condition, s) {
                    out.extend(self.block_elems(then_block, s.clone(), n));
                    out.extend(self.expr_elems(els, s, n));
                }
                self.cap_elems(out)
            }
            ExprKind::Match { scrutinee, arms } => {
                let mut out = Vec::new();
                for (s, y) in self.expr(scrutinee, s) {
                    for arm in arms {
                        let mut a = s.clone();
                        bind_projected(&mut a, &arm.pattern, y, self.track);
                        let mut guarded = vec![(a, Yield::None)];
                        if let Some(g) = &arm.guard {
                            guarded = self.expr(g, guarded.pop().unwrap().0);
                        }
                        for (a, _) in guarded {
                            let names = pattern_names(&arm.pattern);
                            for (mut st, ys) in self.expr_elems(&arm.body, a, n) {
                                for nm in &names {
                                    st.set(nm, s.yield_of(nm));
                                }
                                out.push((st, ys));
                            }
                        }
                    }
                }
                self.cap_elems(out)
            }
            // A diverging leaf (`return ..`): its exit is recorded by the
            // walk and no path falls through.
            _ => {
                for _ in self.expr(e, s) {}
                Vec::new()
            }
        }
    }

    /// [`Self::block`] with an element-wise tail.
    fn block_elems(&mut self, b: &Block, s: PState, n: usize) -> Vec<(PState, Vec<Yield>)> {
        let entry = s.clone();
        let mut states = vec![s];
        for st in &b.stmts {
            let mut next = Vec::new();
            for s in states {
                next.extend(self.stmt(st, s));
            }
            states = self
                .cap(next.into_iter().map(|s| (s, Yield::None)).collect())
                .into_iter()
                .map(|(s, _)| s)
                .collect();
        }
        let Some(tail) = b.final_expr.as_deref() else {
            return Vec::new();
        };
        let mut outs = Vec::new();
        for s in states {
            outs.extend(self.expr_elems(tail, s, n));
        }
        let names = block_let_names(b);
        for (s, _) in outs.iter_mut() {
            for nm in &names {
                let y = entry.yield_of(nm);
                s.set(nm, y);
            }
        }
        self.cap_elems(outs)
    }

    fn cap_elems(&mut self, mut v: Vec<(PState, Vec<Yield>)>) -> Vec<(PState, Vec<Yield>)> {
        v.sort();
        v.dedup();
        if v.len() > MAX_PATHS {
            self.overflow = true;
            v.truncate(1);
        }
        v
    }

    /// `target = <value carrying y>`.
    fn assign(&mut self, target: &Expr, mut s: PState, y: Yield) -> PState {
        s.moved |= y == Yield::Part;
        s.consumed |= self.track && y != Yield::None;
        match &target.kind {
            ExprKind::Identifier(n) => {
                if self.borrows.contains(n) {
                    // `*r = v` spelled as a whole-write through a borrow param.
                    if y != Yield::None {
                        s.stored = true;
                    }
                } else {
                    // Overwriting a holder drops what it held, in this frame.
                    s.moved |= s.yield_of(n) != Yield::None;
                    s.set(n, y);
                }
            }
            _ => {
                let root = place_root(target);
                match root.as_deref() {
                    Some(r) if self.borrows.contains(r) => {
                        if y != Yield::None {
                            s.stored = true;
                        }
                    }
                    Some(r) => {
                        // A field / element of a local now holds it.
                        if y != Yield::None {
                            s.moved = true;
                            let cur = s.yield_of(r);
                            s.set(r, join(cur, y));
                        }
                    }
                    None => {
                        if y != Yield::None {
                            s.unknown = true;
                        }
                    }
                }
            }
        }
        s
    }

    // ── expressions ───────────────────────────────────────────────────

    /// Evaluate `e` on path `s`: the resulting live paths and what each
    /// path's value carries of the parameter. Paths that exit (`return`) are
    /// recorded and not returned.
    fn expr(&mut self, e: &Expr, s: PState) -> Outs {
        match &e.kind {
            ExprKind::Identifier(n) => {
                let y = s.yield_of(n);
                vec![(s, y)]
            }
            ExprKind::SelfValue => {
                let y = s.yield_of("self");
                vec![(s, y)]
            }
            // B-2026-10-05-7 — a SCALAR field read straight off the parameter
            // (`r.id`, `w.r.id`) copies a word and carries nothing of it, so
            // `fn eat(r: R) -> i64 { r.id }` keeps `r` on every exit.
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
                if self.scalar_param_projection(e, &s) =>
            {
                vec![(s, Yield::None)]
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => self
                .expr(object, s)
                .into_iter()
                .map(|(s, y)| (s, if y == Yield::None { y } else { Yield::Part }))
                .collect(),
            ExprKind::Index { object, index } => {
                let mut out = Vec::new();
                for (s, y) in self.expr(object, s) {
                    for (s, _) in self.expr(index, s) {
                        out.push((s, if y == Yield::None { y } else { Yield::Part }));
                    }
                }
                out
            }
            // Aggregates carry whatever their elements carry.
            ExprKind::Tuple(es)
            | ExprKind::ArrayLiteral(es)
            | ExprKind::PrefixCollectionLiteral { items: es, .. } => self.seq(es.iter(), s),
            ExprKind::StructLiteral { fields, spread, .. } => {
                let mut outs = self.seq(fields.iter().map(|f| &f.value), s);
                if let Some(sp) = spread {
                    let mut next = Vec::new();
                    for (s, y) in outs {
                        for (s, y2) in self.expr(sp, s) {
                            next.push((
                                s,
                                join(y, if y2 == Yield::None { y2 } else { Yield::Part }),
                            ));
                        }
                    }
                    outs = next;
                }
                outs
            }
            ExprKind::MapLiteral { entries, .. } => {
                self.seq(entries.iter().flat_map(|(k, v)| [k, v]), s)
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.seq([&**value, &**count].into_iter(), s)
            }
            ExprKind::Call { callee, args } => self.call(callee, args, s),
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => self.method_call(object, method, args, s),
            ExprKind::Binary { left, right, .. } => {
                let mut out = Vec::new();
                for (s, _) in self.expr(left, s) {
                    out.extend(
                        self.expr(right, s)
                            .into_iter()
                            .map(|(s, _)| (s, Yield::None)),
                    );
                }
                out
            }
            ExprKind::Unary { operand, .. } => self
                .expr(operand, s)
                .into_iter()
                .map(|(s, _)| (s, Yield::None))
                .collect(),
            ExprKind::Cast { expr, .. } => self.expr(expr, s),
            ExprKind::Question(inner) => {
                // `e?` exits early on the error path: nothing of the param
                // rides out on it unless the operand itself carries it.
                let mut out = Vec::new();
                for (s, y) in self.expr(inner, s) {
                    let mut es = s.clone();
                    if y != Yield::None {
                        es.unknown = true;
                    }
                    self.exit(&es, Yield::None);
                    out.push((s, if y == Yield::None { y } else { Yield::Part }));
                }
                out
            }
            ExprKind::NilCoalesce { left, right } => {
                let mut out = Vec::new();
                for (s, y) in self.expr(left, s) {
                    out.push((s.clone(), if y == Yield::None { y } else { Yield::Part }));
                    out.extend(self.expr(right, s));
                }
                out
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Comptime(b) => self.block(b, s),
            ExprKind::LabeledBlock { body, .. } => self.block(body, s),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                let mut out = Vec::new();
                for (s, _) in self.expr(condition, s) {
                    out.extend(self.block(then_block, s.clone()));
                    match else_branch {
                        Some(e) => out.extend(self.expr(e, s)),
                        None => out.push((s, Yield::None)),
                    }
                }
                self.cap(out)
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                let mut out = Vec::new();
                for (s, y) in self.expr(value, s) {
                    let mut hit = s.clone();
                    bind_projected(&mut hit, pattern, y, self.track);
                    out.extend(self.block(then_block, hit));
                    match else_branch {
                        Some(e) => out.extend(self.expr(e, s)),
                        None => out.push((s, Yield::None)),
                    }
                }
                self.cap(out)
            }
            ExprKind::Match { scrutinee, arms } => {
                let mut out = Vec::new();
                for (s, y) in self.expr(scrutinee, s) {
                    for arm in arms {
                        let mut a = s.clone();
                        if self.track {
                            if let Some(p) = self.program {
                                for n in pattern_names(&arm.pattern) {
                                    match payload_binding_ty(p, &arm.pattern, &n) {
                                        Some(t) => {
                                            self.name_tys.insert(n, t.clone());
                                        }
                                        None => {
                                            self.name_tys.remove(&n);
                                        }
                                    }
                                }
                            }
                        }
                        bind_projected(&mut a, &arm.pattern, y, self.track);
                        let mut guarded = vec![(a, Yield::None)];
                        if let Some(g) = &arm.guard {
                            guarded = self.expr(g, guarded.pop().unwrap().0);
                        }
                        for (a, _) in guarded {
                            let names = pattern_names(&arm.pattern);
                            for (mut st, yy) in self.expr(&arm.body, a) {
                                for n in &names {
                                    st.set(n, s.yield_of(n));
                                }
                                out.push((st, yy));
                            }
                        }
                    }
                }
                self.cap(out)
            }
            ExprKind::While {
                condition, body, ..
            } => {
                let mut out = Vec::new();
                for (s, _) in self.expr(condition, s) {
                    out.push((s.clone(), Yield::None));
                    out.extend(
                        self.block(body, s)
                            .into_iter()
                            .map(|(s, _)| (s, Yield::None)),
                    );
                }
                self.cap(out)
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                let mut out = Vec::new();
                for (s, y) in self.expr(value, s) {
                    out.push((s.clone(), Yield::None));
                    let mut hit = s;
                    bind_projected(&mut hit, pattern, y, self.track);
                    out.extend(
                        self.block(body, hit)
                            .into_iter()
                            .map(|(s, _)| (s, Yield::None)),
                    );
                }
                self.cap(out)
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                let mut out = Vec::new();
                for (s, y) in self.expr(iterable, s) {
                    out.push((s.clone(), Yield::None));
                    let mut hit = s;
                    bind_projected(&mut hit, pattern, y, self.track);
                    out.extend(
                        self.block(body, hit)
                            .into_iter()
                            .map(|(s, _)| (s, Yield::None)),
                    );
                }
                self.cap(out)
            }
            ExprKind::Loop { body, .. } => {
                // A `loop` leaves through `break` (modelled as falling out of
                // the body) or `return`.
                let outs = self.block(body, s);
                outs.into_iter().map(|(s, _)| (s, Yield::None)).collect()
            }
            ExprKind::Return(v) => {
                match v {
                    Some(v) => {
                        for (s, y) in self.expr(v, s) {
                            self.exit(&s, y);
                        }
                    }
                    None => self.exit(&s, Yield::None),
                }
                Vec::new()
            }
            ExprKind::Break { value: Some(v), .. } => self.expr(v, s),
            ExprKind::Closure { body, .. } => {
                let mut names = Vec::new();
                collect_expr_names(body, &mut names);
                let mut s = s;
                if names.iter().any(|n| s.yield_of(n) != Yield::None) {
                    s.unknown = true;
                }
                vec![(s, Yield::None)]
            }
            ExprKind::Pipe { left, right } => {
                let mut out = Vec::new();
                for (s, y) in self.expr(left, s) {
                    let mut s = s;
                    if y != Yield::None {
                        s.unknown = true;
                    }
                    out.extend(
                        self.expr(right, s)
                            .into_iter()
                            .map(|(s, _)| (s, Yield::None)),
                    );
                }
                out
            }
            ExprKind::OptionalChain { object, args, .. } => {
                let mut out = Vec::new();
                for (s, _) in self.expr(object, s) {
                    let es: Vec<&Expr> = args.iter().flatten().map(|a| &a.value).collect();
                    for (mut s, y) in self.seq(es.into_iter(), s) {
                        if y != Yield::None {
                            s.unknown = true;
                        }
                        out.push((s, Yield::None));
                    }
                }
                out
            }
            ExprKind::Range { start, end, .. } => {
                let es: Vec<&Expr> = start.iter().chain(end.iter()).map(|b| &**b).collect();
                self.seq(es.into_iter(), s)
                    .into_iter()
                    .map(|(s, _)| (s, Yield::None))
                    .collect()
            }
            ExprKind::Par(b) => {
                let mut names = Vec::new();
                collect_block_names(b, &mut names);
                let mut s = s;
                if names.iter().any(|n| s.yield_of(n) != Yield::None) {
                    s.unknown = true;
                }
                vec![(s, Yield::None)]
            }
            ExprKind::Lock { body, .. } | ExprKind::Providers { body, .. } => self
                .block(body, s)
                .into_iter()
                .map(|(s, _)| (s, Yield::None))
                .collect(),
            ExprKind::InterpolatedStringLit(parts) => {
                // Each hole is evaluated (a consuming call inside one is a
                // real use); the formatted string owns nothing of a holder.
                let mut states = vec![s];
                for p in parts {
                    if let ParsedInterpolationPart::Expr(x, _) = p {
                        let mut next = Vec::new();
                        for s in states {
                            next.extend(self.expr(x, s).into_iter().map(|(s, _)| s));
                        }
                        states = next;
                    }
                }
                self.cap(states.into_iter().map(|s| (s, Yield::None)).collect())
            }
            _ => {
                // A shape the walk does not model: if it mentions a holder,
                // whole-value consumers must not act on this fate.
                let mut names = Vec::new();
                collect_expr_names(e, &mut names);
                let mut s = s;
                if names.iter().any(|n| s.yield_of(n) != Yield::None) {
                    s.moved = true;
                }
                vec![(s, Yield::None)]
            }
        }
    }

    /// Evaluate a sequence left to right; the result carries the join of what
    /// every element carries (an aggregate holds all of them).
    fn seq<'e>(&mut self, es: impl Iterator<Item = &'e Expr>, s: PState) -> Outs {
        let mut states = vec![(s, Yield::None)];
        for e in es {
            let mut next = Vec::new();
            for (s, acc) in states {
                for (s, y) in self.expr(e, s) {
                    next.push((s, join(acc, y)));
                }
            }
            states = self.cap(next);
        }
        states
    }

    fn call(&mut self, callee: &Expr, args: &[CallArg], s: PState) -> Outs {
        let key = match &callee.kind {
            ExprKind::Identifier(n) => Some(n.clone()),
            ExprKind::Path { segments, .. } => Some(segments.join(".")),
            _ => None,
        };
        let target = match (self.program, key.as_deref()) {
            (Some(p), Some(k)) if k != self.self_name => resolve_fn(p, k),
            _ => None,
        };
        let ctor = target.is_none() && key.as_deref().is_some_and(is_ctor_name);
        // A desugared operator on a primitive (`c.id + 10` reaches here as
        // `i64.add(c.id, 10)`): its operands are scalars, copied rather than
        // moved, and its result owns nothing of any holder.
        let scalar_op = target.is_none()
            && matches!(&callee.kind, ExprKind::Path { segments, .. }
                if segments.len() == 2 && is_scalar_type_name(&segments[0]));
        let mut outs = Vec::new();
        for (s, ys) in self.args(args, s) {
            if scalar_op {
                outs.push((s, Yield::None));
                continue;
            }
            if ctor {
                let y = ys.iter().copied().fold(Yield::None, join);
                outs.push((s, y));
                continue;
            }
            let mut s = s;
            let mut result = Yield::None;
            for (i, y) in ys.iter().enumerate() {
                if *y == Yield::None {
                    continue;
                }
                // Step 4: a print-family builtin or a desugared comparison
                // only reads its operands.
                if self.track && target.is_none() && key.as_deref().is_some_and(is_lending_builtin)
                {
                    continue;
                }
                match target {
                    Some(g) if self.depth < MAX_CALL_DEPTH => {
                        if g.params.get(i).is_some_and(|p| is_borrow_ty(&p.ty)) {
                            continue; // lent, not handed over
                        }
                        s.moved |= *y == Yield::Part;
                        s.consumed |= self.track;
                        match param_fate_depth(self.program, g, i, self.depth + 1) {
                            Some(fate) if fate.is_exact() => {
                                s.moved |= !fate.whole_only;
                                match (fate.returned_any(), fate.stored()) {
                                    (Coverage::Never, Coverage::Never) => {}
                                    (Coverage::Always, Coverage::Never) => {
                                        let r = if fate.returned_whole() == Coverage::Always {
                                            *y
                                        } else {
                                            Yield::Part
                                        };
                                        result = join(result, r);
                                    }
                                    _ => s.unknown = true,
                                }
                            }
                            _ => s.unknown = true,
                        }
                    }
                    _ => s.unknown = true,
                }
            }
            outs.push((s, result));
        }
        outs
    }

    fn args(&mut self, args: &[CallArg], s: PState) -> Vec<(PState, Vec<Yield>)> {
        let mut states: Vec<(PState, Vec<Yield>)> = vec![(s, Vec::new())];
        for a in args {
            let mut next = Vec::new();
            for (s, ys) in states {
                for (s, y) in self.expr(&a.value, s) {
                    let mut ys = ys.clone();
                    ys.push(y);
                    next.push((s, ys));
                }
            }
            states = next;
            if states.len() > MAX_PATHS {
                self.overflow = true;
                states.truncate(1);
            }
        }
        states
    }

    fn method_call(&mut self, object: &Expr, method: &str, args: &[CallArg], s: PState) -> Outs {
        let mut outs = Vec::new();
        for (s, oy) in self.expr(object, s) {
            for (mut s, ys) in self.args(args, s) {
                let carried = ys.iter().copied().fold(Yield::None, join);
                if carried != Yield::None {
                    s.moved |= carried == Yield::Part;
                    if is_container_store(method) {
                        match place_root(object).as_deref() {
                            Some(r) if self.borrows.contains(r) => s.stored = true,
                            Some(r) => {
                                s.moved = true;
                                let cur = s.yield_of(r);
                                s.set(r, join(cur, carried));
                            }
                            None => s.unknown = true,
                        }
                    } else {
                        s.unknown = true;
                    }
                }
                // A method on a holder that is not a store: reads (`len`,
                // `clone`, field getters) return no ownership of it; a
                // consuming method we cannot resolve is unknown.
                if oy != Yield::None && is_consuming_method(method) {
                    s.unknown = true;
                }
                // A user method taking `self` by value consumes the holder in
                // ITS frame: the parameter's body is no longer this caller's.
                if oy != Yield::None
                    && !self.track
                    && self
                        .program
                        .is_some_and(|p| user_method_takes_owned_self(p, method))
                {
                    s.moved = true;
                }
                outs.push((s, Yield::None));
            }
        }
        outs
    }
}

/// Step 4: builtins whose arguments are only read — the print family and the
/// desugared comparison operators (`String.eq(a, b)`).
fn is_lending_builtin(key: &str) -> bool {
    let last = key.rsplit('.').next().unwrap_or(key);
    matches!(
        key,
        "println"
            | "print"
            | "eprintln"
            | "eprint"
            | "assert"
            | "assert_eq"
            | "assert_ne"
            | "debug_assert"
    ) || (key.contains('.') && matches!(last, "eq" | "ne" | "lt" | "le" | "gt" | "ge" | "cmp"))
}

fn is_scalar_type_name(n: &str) -> bool {
    matches!(
        n,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f16"
            | "bf16"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}

/// B-2026-10-05-107 — is every value leaf of `e` a tuple LITERAL of `n`
/// elements, or a `return` that leaves no value behind? Through a block's
/// tail, both arms of an `if` with an `else`, and every arm of a `match`.
fn tuple_literal_leaves(e: &Expr, n: usize) -> bool {
    match &e.kind {
        ExprKind::Tuple(es) => es.len() == n,
        ExprKind::Return(_) => true,
        ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Seq(b) => b
            .final_expr
            .as_deref()
            .is_some_and(|t| tuple_literal_leaves(t, n)),
        ExprKind::If {
            then_block,
            else_branch: Some(els),
            ..
        } => {
            then_block
                .final_expr
                .as_deref()
                .is_some_and(|t| tuple_literal_leaves(t, n))
                && tuple_literal_leaves(els, n)
        }
        ExprKind::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|a| tuple_literal_leaves(&a.body, n))
        }
        _ => false,
    }
}

/// `let <pattern> = <value carrying y>`. `track`: following an arm binding.
fn bind(s: &mut PState, pattern: &Pattern, y: Yield, track: bool) {
    match &pattern.kind {
        PatternKind::Binding(n) => {
            s.moved |= y == Yield::Part;
            if track {
                s.inner.insert(n.clone());
            }
            s.set(n, y)
        }
        PatternKind::Wildcard => s.moved |= y != Yield::None,
        _ => bind_projected(s, pattern, y, track),
    }
}

/// Names bound inside a destructuring pattern each hold a PART. Following an
/// arm binding (`track`), destructuring it is not by itself a move: what the
/// parts then do is followed through their own names.
fn bind_projected(s: &mut PState, pattern: &Pattern, y: Yield, track: bool) {
    let part = if y == Yield::None { y } else { Yield::Part };
    let names = pattern_names(pattern);
    if !track {
        s.moved |= part != Yield::None && !names.is_empty();
    }
    for n in names {
        if track {
            s.inner.insert(n.clone());
        }
        s.set(&n, part);
    }
}

fn pattern_names(p: &Pattern) -> Vec<String> {
    let mut out = Vec::new();
    fn go(p: &Pattern, out: &mut Vec<String>) {
        match &p.kind {
            PatternKind::Binding(n) => out.push(n.clone()),
            PatternKind::AtBinding { name, pattern, .. } => {
                out.push(name.clone());
                go(pattern, out);
            }
            PatternKind::Struct { fields, .. } => {
                for f in fields {
                    match &f.pattern {
                        Some(sub) => go(sub, out),
                        None => out.push(f.name.clone()),
                    }
                }
            }
            PatternKind::TupleVariant { patterns, .. } => {
                for f in patterns {
                    go(f, out);
                }
            }
            PatternKind::Tuple(es) | PatternKind::Or(es) => {
                for e in es {
                    go(e, out);
                }
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                for e in prefix.iter().chain(suffix) {
                    go(e, out);
                }
                if let Some(RestPattern::Bound(n)) = rest {
                    out.push(n.clone());
                }
            }
            _ => {}
        }
    }
    go(p, &mut out);
    out
}

fn block_let_names(b: &Block) -> Vec<String> {
    let mut out = Vec::new();
    for st in &b.stmts {
        match &st.kind {
            StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                out.extend(pattern_names(pattern))
            }
            StmtKind::LetUninit { name, .. } => out.push(name.clone()),
            _ => {}
        }
    }
    out
}

/// The root binding of a place expression (`a.b[i].c` → `a`).
fn place_root(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        ExprKind::SelfValue => Some("self".into()),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root(object),
        _ => None,
    }
}

fn is_container_store(m: &str) -> bool {
    matches!(
        m,
        "push" | "push_back" | "push_front" | "insert" | "set" | "append" | "add" | "enqueue"
    )
}

fn is_consuming_method(m: &str) -> bool {
    matches!(
        m,
        "into_iter"
            | "unwrap"
            | "expect"
            // B-2026-09-29-16 — the `Err` side moves its payload out the same way.
            | "unwrap_err"
            | "expect_err"
            | "unwrap_or"
            | "take"
            | "into"
            | "into_inner"
    )
}

fn user_method_takes_owned_self(program: &Program, name: &str) -> bool {
    program.items.iter().any(|item| match item {
        Item::ImplBlock(b) => b.items.iter().any(|ii| {
            matches!(ii, ImplItem::Method(g)
                if g.name == name && matches!(g.self_param, Some(SelfParam::Owned)))
        }),
        _ => false,
    })
}

fn is_ctor_name(k: &str) -> bool {
    let last = k.rsplit('.').next().unwrap_or(k);
    matches!(last, "Some" | "Ok" | "Err")
        || (k.contains('.') && last.chars().next().is_some_and(|c| c.is_uppercase()))
}

fn resolve_fn<'p>(program: &'p Program, key: &str) -> Option<&'p Function> {
    match key.split_once('.') {
        None => program.items.iter().find_map(|item| match item {
            Item::Function(g) if g.name == key => Some(g),
            _ => None,
        }),
        Some((ty, m)) => program.items.iter().find_map(|item| match item {
            Item::ImplBlock(b) => {
                let TypeKind::Path(pth) = &b.target_type.kind else {
                    return None;
                };
                if pth.segments.last().map(String::as_str) != Some(ty) {
                    return None;
                }
                b.items.iter().find_map(|ii| match ii {
                    ImplItem::Method(g) if g.name == m && g.self_param.is_none() => Some(&**g),
                    _ => None,
                })
            }
            _ => None,
        }),
    }
}

fn collect_block_names(b: &Block, out: &mut Vec<String>) {
    for st in &b.stmts {
        match &st.kind {
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                collect_expr_names(value, out)
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                collect_expr_names(target, out);
                collect_expr_names(value, out);
            }
            StmtKind::Expr(e) => collect_expr_names(e, out),
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                collect_block_names(body, out)
            }
            StmtKind::MultiAssign { targets, values } => {
                for e in targets.iter().chain(values) {
                    collect_expr_names(e, out);
                }
            }
            StmtKind::LetUninit { .. } => {}
        }
    }
    if let Some(e) = &b.final_expr {
        collect_expr_names(e, out);
    }
}

/// Every identifier an expression mentions (over-approximate: shadowing is
/// ignored, which only makes a capture check more conservative).
fn collect_expr_names(e: &Expr, out: &mut Vec<String>) {
    let go = |x: &Expr, out: &mut Vec<String>| collect_expr_names(x, out);
    match &e.kind {
        ExprKind::Identifier(n) => out.push(n.clone()),
        ExprKind::SelfValue => out.push("self".into()),
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            go(left, out);
            go(right, out);
        }
        ExprKind::Unary { operand: x, .. }
        | ExprKind::Question(x)
        | ExprKind::Cast { expr: x, .. }
        | ExprKind::FieldAccess { object: x, .. }
        | ExprKind::TupleIndex { object: x, .. }
        | ExprKind::Closure { body: x, .. } => go(x, out),
        ExprKind::OptionalChain { object, args, .. } => {
            go(object, out);
            for a in args.iter().flatten() {
                go(&a.value, out);
            }
        }
        ExprKind::Call { callee, args } => {
            go(callee, out);
            for a in args {
                go(&a.value, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            go(object, out);
            for a in args {
                go(&a.value, out);
            }
        }
        ExprKind::Index { object, index } => {
            go(object, out);
            go(index, out);
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Providers { body: b, .. } => collect_block_names(b, out),
        ExprKind::Lock { mutex, body, .. } => {
            go(mutex, out);
            collect_block_names(body, out);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            go(condition, out);
            collect_block_names(then_block, out);
            if let Some(x) = else_branch {
                go(x, out);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            go(value, out);
            collect_block_names(then_block, out);
            if let Some(x) = else_branch {
                go(x, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            go(scrutinee, out);
            for a in arms {
                if let Some(g) = &a.guard {
                    go(g, out);
                }
                go(&a.body, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            go(condition, out);
            collect_block_names(body, out);
        }
        ExprKind::WhileLet { value, body, .. }
        | ExprKind::For {
            iterable: value,
            body,
            ..
        } => {
            go(value, out);
            collect_block_names(body, out);
        }
        ExprKind::Return(Some(x)) => go(x, out),
        ExprKind::Break { value: Some(x), .. } => go(x, out),
        ExprKind::Tuple(es)
        | ExprKind::ArrayLiteral(es)
        | ExprKind::PrefixCollectionLiteral { items: es, .. } => {
            for x in es {
                go(x, out);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            go(value, out);
            go(count, out);
        }
        ExprKind::MapLiteral { entries, .. } => {
            for (k, v) in entries {
                go(k, out);
                go(v, out);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                go(&f.value, out);
            }
            if let Some(x) = spread {
                go(x, out);
            }
        }
        ExprKind::Range { start, end, .. } => {
            for x in start.iter().chain(end.iter()) {
                go(x, out);
            }
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(x, _) = p {
                    go(x, out);
                }
            }
        }
        _ => {}
    }
}

// ───────────────────────────── consumers ──────────────────────────────

/// Whether the per-param fate is the ANSWER, not only an audit. On by default
/// since Slice 4 step 3 (2026-10-02); `KARAC_DROP_SCHEDULE=0` restores the
/// legacy predicates and `KARAC_DROP_SCHEDULE=audit` runs them with the audit
/// log. Read once per process.
pub fn schedule_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("KARAC_DROP_SCHEDULE").ok().as_deref(),
            Some("0") | Some("audit")
        )
    })
}

/// Step 2's question, asked by the CALLER of a by-value argument: does the
/// whole value leave the callee on every exit (`Some(true)` — the new owner
/// runs its body, the caller must not), or stay on every exit (`Some(false)` —
/// the caller runs it at the end of the call, design.md rule 3)?
///
/// `None` — the flag is off, the param is not a by-value binding, the walk
/// lost track on some exit, a PART leaves while the rest stays, or the
/// answer differs by path. Each of those keeps the legacy machinery, which
/// handles parts and per-path flags; the fact only settles the two
/// unconditional, whole-value answers.
/// Shapes whose consumers are not yet ready for the fact's answer, measured
/// with the flag on: a lowered `self` receiver and a generic callee (the
/// per-path conditional machinery mishandles both when the fact says
/// "returned on some exits" where the legacy predicate said "never").
///
/// B-2026-10-05-107 — except a generic callee's param declared as a BARE type
/// parameter (`a: T`). Its legacy answer missed a hand-back through a local
/// (`let v = if c { a } else { b }; v`), so the caller ran the returned
/// value's body as well. Measured 2026-10-06 with every generic param flipped:
/// the interpreter, codegen and memory_sanitizer suites lost one cell, a
/// generic ENUM param (`h: Ho[T]`, `let m = h; if k { return m }`) whose
/// compiled monomorph lost the body on the leg that keeps it. A bare `T`
/// reaches none of those enum-payload legs.
fn flip_reaches(f: &Function, idx: usize) -> bool {
    f.generic_params.as_ref().is_none_or(|gp| {
        f.params.get(idx).is_some_and(|p| {
            matches!(&p.ty.kind, TypeKind::Path(tp)
                if tp.generic_args.is_none()
                    && tp.segments.len() == 1
                    && gp.params.iter().any(|g| !g.is_const && g.name == tp.segments[0]))
        })
    }) && !f
        .params
        .get(idx)
        .is_some_and(|p| matches!(&p.pattern.kind, PatternKind::Binding(n) if n == "self"))
}

pub fn whole_param_leaves(program: Option<&Program>, f: &Function, idx: usize) -> Option<bool> {
    if !schedule_enabled() || !flip_reaches(f, idx) {
        return None;
    }
    let fate = param_fate(program, f, idx).filter(|x| x.whole_only)?;
    if fate
        .exits
        .iter()
        .all(|e| matches!(e, ExitFate::Returned | ExitFate::Stored))
    {
        Some(true)
    } else if fate.exits.iter().all(|e| *e == ExitFate::Stays) {
        Some(false)
    } else {
        None
    }
}

/// B-2026-10-06-6 — does an owned-`self` method hand its receiver back WHOLE
/// on every exit (`fn me(self) -> X { self }`, `let m = self; m`)?
///
/// The receiver twin of `fn_always_returns_param`, which cannot ask it: `self`
/// is not in `f.params`. Exact answers only, so `false` covers both "not on
/// every exit" and "could not tell", the direction that keeps the caller's
/// cleanup.
pub fn whole_self_always_returned(program: Option<&Program>, f: &Function) -> bool {
    if !matches!(f.self_param, Some(SelfParam::Owned)) {
        return false;
    }
    let mut w = Walker {
        program,
        self_name: f.name.clone(),
        param_name: "self".to_string(),
        depth: 0,
        borrows: f
            .params
            .iter()
            .filter(|q| is_borrow_ty(&q.ty))
            .filter_map(|q| match &q.pattern.kind {
                PatternKind::Binding(n) => Some(n.clone()),
                _ => None,
            })
            .collect(),
        exits: Vec::new(),
        overflow: false,
        moved: false,
        param_ty: None,
        track: false,
        exit_states: Vec::new(),
        name_tys: Default::default(),
    };
    let mut s = PState::default();
    s.whole.insert("self".to_string());
    let outs = w.block(&f.body, s);
    for (st, y) in outs {
        w.exit(&st, y);
    }
    !w.overflow
        && !w.moved
        && !w.exits.is_empty()
        && w.exits.iter().all(|e| *e == ExitFate::Returned)
}

/// Slice 4 step 4 — design.md § Drop ordering rule 3 for a binding the block
/// does NOT declare: a `match` arm's or `if let`'s payload binding, or an
/// enclosing block's local used inside a nested block.
///
/// Returns `(name, i)` for each such binding whose LAST mention in `b` is
/// `b.stmts[i]`, where it is passed bare, by value, to a callee whose fate
/// keeps it on every exit (`whole_param_leaves` is `Some(false)`). The value
/// then dies at the end of that statement, as a `let` local passed the same
/// way does through the block's NLL map; without this both backends ran its
/// body at the end of the enclosing arm or statement instead
/// (B-2026-10-05-7).
///
/// Declined when the statement mentions the name anywhere else, when a `let`
/// in the block or inside the statement shadows it, when a `defer` mentions
/// it, or when the call sits in a closure.
pub fn bindings_dying_in_callee(program: Option<&Program>, b: &Block) -> Vec<(String, usize)> {
    dying_in_callee(program, b, false)
}

/// B-2026-10-04-47 — the twin of [`bindings_dying_in_callee`] for a binding
/// the block DOES declare. Such a binding already dies at that statement
/// through the block's NLL map, but only for the actions NLL admits: a
/// holder's release of a `shared struct` it carries (a struct field, an
/// `Option` or enum payload, a tuple or `Vec` element) stays at scope exit
/// when the last use is a READ, and rule 3 moves it to the call when the last
/// use hands the holder by value to a callee that keeps it. Same declines as
/// the twin, plus a statement that rebinds the name itself (`let o = f(o)`),
/// where the binding left alive at the statement's end is the new one.
pub fn declared_bindings_dying_in_callee(
    program: Option<&Program>,
    b: &Block,
) -> Vec<(String, usize)> {
    dying_in_callee(program, b, true)
}

fn dying_in_callee(
    program: Option<&Program>,
    b: &Block,
    declared_only: bool,
) -> Vec<(String, usize)> {
    let Some(program) = program else {
        return Vec::new();
    };
    if !schedule_enabled() {
        return Vec::new();
    }
    let declared = block_let_names(b);
    let mut out: Vec<(String, usize)> = Vec::new();
    for (i, st) in b.stmts.iter().enumerate() {
        let mut found: Vec<String> = Vec::new();
        let mut shadow: Vec<String> = Vec::new();
        stays_args_stmt(program, st, &mut found, &mut shadow);
        for n in found {
            if declared.contains(&n) != declared_only
                || shadow.contains(&n)
                || out.iter().any(|(m, _)| *m == n)
            {
                continue;
            }
            if declared_only && stmt_binds(st, &n) {
                continue;
            }
            let mut here = Vec::new();
            collect_stmt_names(st, &mut here);
            if here.iter().filter(|m| **m == n).count() != 1 {
                continue;
            }
            let mut later = Vec::new();
            for s in &b.stmts[i + 1..] {
                collect_stmt_names(s, &mut later);
            }
            if let Some(e) = &b.final_expr {
                collect_expr_names(e, &mut later);
            }
            if later.contains(&n) {
                continue;
            }
            let deferred = b.stmts[..i].iter().any(|s| match &s.kind {
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    let mut ns = Vec::new();
                    collect_block_names(body, &mut ns);
                    ns.contains(&n)
                }
                _ => false,
            });
            if !deferred {
                out.push((n, i));
            }
        }
    }
    out
}

/// Does `st` itself bind `n` (a `let` whose pattern names it)?
fn stmt_binds(st: &Stmt, n: &str) -> bool {
    match &st.kind {
        StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
            pattern_names(pattern).iter().any(|m| m == n)
        }
        StmtKind::LetUninit { name, .. } => name == n,
        _ => false,
    }
}

fn stays_args_stmt(
    program: &Program,
    st: &Stmt,
    found: &mut Vec<String>,
    shadow: &mut Vec<String>,
) {
    match &st.kind {
        StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
            stays_args_expr(program, value, found, shadow)
        }
        StmtKind::Assign { value, .. } => stays_args_expr(program, value, found, shadow),
        StmtKind::Expr(e) => stays_args_expr(program, e, found, shadow),
        _ => {}
    }
}

fn stays_args_block(
    program: &Program,
    b: &Block,
    found: &mut Vec<String>,
    shadow: &mut Vec<String>,
) {
    shadow.extend(block_let_names(b));
    for st in &b.stmts {
        stays_args_stmt(program, st, found, shadow);
    }
    if let Some(e) = &b.final_expr {
        stays_args_expr(program, e, found, shadow);
    }
}

/// Bare identifiers handed by value to a resolved callee whose fate keeps
/// them on every exit. Closures are not entered; any shape not named here
/// contributes nothing, so its mentions fail the caller's count check.
fn stays_args_expr(program: &Program, e: &Expr, found: &mut Vec<String>, shadow: &mut Vec<String>) {
    let go = |x: &Expr, found: &mut Vec<String>, shadow: &mut Vec<String>| {
        stays_args_expr(program, x, found, shadow)
    };
    match &e.kind {
        ExprKind::Call { callee, args } => {
            let key = match &callee.kind {
                ExprKind::Identifier(n) => Some(n.clone()),
                ExprKind::Path { segments, .. } => Some(segments.join(".")),
                _ => None,
            };
            let target = key.as_deref().and_then(|k| resolve_fn(program, k));
            for (j, a) in args.iter().enumerate() {
                match (&a.value.kind, target) {
                    (ExprKind::Identifier(n), Some(g))
                        if whole_param_leaves(Some(program), g, j) == Some(false) =>
                    {
                        found.push(n.clone())
                    }
                    _ => go(&a.value, found, shadow),
                }
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            go(object, found, shadow);
            for a in args {
                go(&a.value, found, shadow);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            go(left, found, shadow);
            go(right, found, shadow);
        }
        ExprKind::Unary { operand, .. } => go(operand, found, shadow),
        ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
            for x in es {
                go(x, found, shadow);
            }
        }
        ExprKind::StructLiteral { fields, .. } => {
            for f in fields {
                go(&f.value, found, shadow);
            }
        }
        ExprKind::Block(b) => stays_args_block(program, b, found, shadow),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            go(condition, found, shadow);
            stays_args_block(program, then_block, found, shadow);
            if let Some(x) = else_branch {
                go(x, found, shadow);
            }
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(x, _) = p {
                    go(x, found, shadow);
                }
            }
        }
        _ => {}
    }
}

fn collect_stmt_names(st: &Stmt, out: &mut Vec<String>) {
    match &st.kind {
        StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
            collect_expr_names(value, out)
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            collect_expr_names(target, out);
            collect_expr_names(value, out);
        }
        StmtKind::Expr(e) => collect_expr_names(e, out),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_block_names(body, out)
        }
        StmtKind::MultiAssign { targets, values } => {
            for e in targets.iter().chain(values) {
                collect_expr_names(e, out);
            }
        }
        StmtKind::LetUninit { .. } => {}
    }
}

/// Slice 4 step 2 — under the flag, an EXACT whole-value fate that never hands
/// the parameter back answers every "returns the param" question no, however
/// it is phrased (bare, through a call, or beside `None`). Those predicates
/// match the parameter by NAME, so a body that shadows it (`fn k8(x: R) -> R {
/// let x = mk(28); x }`, B-2026-09-29-61) read as returning it.
pub fn whole_param_never_returned(program: Option<&Program>, f: &Function, idx: usize) -> bool {
    schedule_enabled()
        && !f.params.get(idx).is_some_and(|p| is_scalar_ty(&p.ty))
        && flip_reaches(f, idx)
        && param_fate(program, f, idx)
            .is_some_and(|x| x.is_exact() && x.whole_only && x.returned_any() == Coverage::Never)
}

// ─────────────────────────────── audit ────────────────────────────────

/// Step 1's only consumer: compare a legacy predicate's answer with the fact.
///
/// Armed by `KARAC_DROP_SCHEDULE=audit` plus `KARAC_DROP_SCHEDULE_AUDIT=<path>`
/// (an absolute or relative path containing a `/`, so `=1` cannot silently
/// write a file called `1`). Each distinct disagreement is appended once per
/// process as one tab-separated line:
/// `predicate  fn  param-index  legacy  fact  exits  exact  fn-source`.
/// Slice 4 step 4 — what became of a `match` / `if let` arm's payload binding
/// on one way out of the arm.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArmExit {
    /// Still owned by the binding and only read: nothing left it.
    Stays,
    /// Handed somewhere by value inside the arm — a local, a callee's
    /// by-value parameter, a discard — so the arm, not the scrutinee, owns it.
    Consumed,
    /// The arm's value carries it, whole or in part.
    Value,
    /// An enclosing local now holds it.
    Held,
    /// Stored into an outliving place.
    Stored,
    /// A `return` inside the arm carries it out of the function.
    Returned,
    /// The walk could not follow it on this path.
    Unknown,
}

/// The fate of one arm binding: one entry per way out of the arm (its end and
/// every `return` inside it), duplicates removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmFate {
    pub exits: Vec<ArmExit>,
    /// The binding's declared type was known to the walk. Without it a
    /// scalar field read (`t.id`) cannot be told from a partial move, so an
    /// arm VALUE or `return` that carries a projection of it is not evidence.
    pub typed: bool,
}

impl ArmFate {
    pub fn is_exact(&self) -> bool {
        !self.exits.contains(&ArmExit::Unknown)
    }
    /// Is the binding only READ on every way out — the question both
    /// backends' legacy reads-only predicates ask? `None` when inexact.
    pub fn reads_only(&self) -> Option<bool> {
        if !self.typed
            && self
                .exits
                .iter()
                .any(|e| matches!(e, ArmExit::Value | ArmExit::Returned))
        {
            return None;
        }
        self.is_exact()
            .then(|| self.exits.iter().all(|e| *e == ArmExit::Stays))
    }
    /// `s` stays, `c` consumed, `V` value, `H` held, `S` stored, `R` returned,
    /// `?` unknown.
    pub fn render(&self) -> String {
        self.exits
            .iter()
            .map(|e| match e {
                ArmExit::Stays => 's',
                ArmExit::Consumed => 'c',
                ArmExit::Value => 'V',
                ArmExit::Held => 'H',
                ArmExit::Stored => 'S',
                ArmExit::Returned => 'R',
                ArmExit::Unknown => '?',
            })
            .collect()
    }
}

/// An arm body: a `match` arm's expression, or an `if let` / `while let`
/// block.
#[derive(Clone, Copy)]
pub enum ArmBody<'a> {
    Expr(&'a Expr),
    Block(&'a Block),
}

/// The fate of arm binding `name`, walked through the arm's `guard` and
/// `body` with the same [`Walker`] the parameter fate uses. `binding_ty`, when
/// known, lets a scalar field read (`x.id`) count as a read rather than a
/// partial move; [`payload_binding_ty`] supplies it for a non-generic user
/// enum's payload.
pub fn arm_binding_fate(
    program: Option<&Program>,
    binding_ty: Option<&TypeExpr>,
    guard: Option<&Expr>,
    body: ArmBody<'_>,
    name: &str,
) -> ArmFate {
    let mut w = Walker {
        program,
        self_name: String::new(),
        param_name: name.to_string(),
        depth: 0,
        borrows: BTreeSet::new(),
        exits: Vec::new(),
        overflow: false,
        moved: false,
        param_ty: binding_ty.cloned(),
        track: true,
        exit_states: Vec::new(),
        name_tys: Default::default(),
    };
    let mut s = PState::default();
    s.whole.insert(name.to_string());
    let mut starts = vec![s];
    if let Some(g) = guard {
        starts = w
            .expr(g, starts.pop().unwrap())
            .into_iter()
            .map(|(s, _)| s)
            .collect();
    }
    let mut ends: Outs = Vec::new();
    for s in starts {
        ends.extend(match body {
            ArmBody::Expr(e) => w.expr(e, s),
            ArmBody::Block(b) => w.block(b, s),
        });
    }
    let classify = |s: &PState, y: Yield, at_end: bool| -> ArmExit {
        if s.unknown {
            ArmExit::Unknown
        } else if y != Yield::None {
            if at_end {
                ArmExit::Value
            } else {
                ArmExit::Returned
            }
        } else if s.stored {
            ArmExit::Stored
        } else if s
            .whole
            .iter()
            .chain(&s.part)
            .any(|n| n != name && !s.inner.contains(n))
        {
            ArmExit::Held
        } else if s.moved || s.consumed || s.yield_of(name) != Yield::Whole {
            ArmExit::Consumed
        } else {
            ArmExit::Stays
        }
    };
    let mut exits: Vec<ArmExit> = if w.overflow {
        vec![ArmExit::Unknown]
    } else {
        ends.iter()
            .map(|(s, y)| classify(s, *y, true))
            .chain(w.exit_states.iter().map(|(s, y)| classify(s, *y, false)))
            .collect()
    };
    if exits.is_empty() {
        exits.push(ArmExit::Stays);
    }
    let mut seen = Vec::new();
    exits.retain(|e| {
        if seen.contains(e) {
            false
        } else {
            seen.push(*e);
            true
        }
    });
    ArmFate {
        exits,
        typed: binding_ty.is_some(),
    }
}

/// Step 4: does `pattern` destructure a variant of a `shared` / `par` enum?
/// Handing such a payload on by value COPIES it — the husk keeps its own —
/// so the arm fate's "consumed" says nothing about who runs the husk's
/// drops, and the flip declines.
pub fn pattern_enum_is_rc_backed(program: &Program, pattern: &Pattern) -> bool {
    let path = match &pattern.kind {
        PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. } => path,
        _ => return false,
    };
    let Some(en) = path.first() else {
        return false;
    };
    program
        .items
        .iter()
        .any(|it| matches!(it, Item::EnumDef(e) if &e.name == en && (e.is_shared || e.is_par)))
}

/// The declared type of payload binding `name` in `pattern`, when the pattern
/// destructures a variant of a NON-GENERIC user enum directly (`E.V(a, b)`,
/// `E.V { f }`). `None` otherwise — a generic or built-in enum's payload type
/// needs the scrutinee's instantiation, which this module does not see.
pub fn payload_binding_ty<'p>(
    program: &'p Program,
    pattern: &Pattern,
    name: &str,
) -> Option<&'p TypeExpr> {
    let (path, sub): (&Vec<String>, Vec<(Option<&str>, &Pattern)>) = match &pattern.kind {
        PatternKind::TupleVariant { path, patterns } => {
            (path, patterns.iter().map(|p| (None, p)).collect())
        }
        PatternKind::Struct { path, fields, .. } => (
            path,
            fields
                .iter()
                .filter_map(|f| f.pattern.as_ref().map(|p| (Some(f.name.as_str()), p)))
                .collect(),
        ),
        _ => return None,
    };
    let [en, vn] = path.as_slice() else {
        return None;
    };
    let ed = program.items.iter().find_map(|it| match it {
        Item::EnumDef(e) if &e.name == en && e.generic_params.is_none() => Some(e),
        _ => None,
    })?;
    let v = ed.variants.iter().find(|v| &v.name == vn)?;
    match (&v.kind, &pattern.kind) {
        (VariantKind::Tuple(tys), PatternKind::TupleVariant { .. }) => sub
            .iter()
            .position(|(_, p)| matches!(&p.kind, PatternKind::Binding(n) if n == name))
            .and_then(|i| tys.get(i)),
        (VariantKind::Struct(fs), PatternKind::Struct { fields, .. }) => {
            let fname = fields.iter().find_map(|f| match &f.pattern {
                None if f.name == name => Some(f.name.as_str()),
                Some(p) if matches!(&p.kind, PatternKind::Binding(n) if n == name) => {
                    Some(f.name.as_str())
                }
                _ => None,
            })?;
            fs.iter().find(|f| f.name == fname).map(|f| &f.ty)
        }
        _ => None,
    }
}

/// Slice 4 step 4: `KARAC_DROP_SCHEDULE_ARMS=1` lets the arm fate answer the
/// reads-only question at every match-arm / `if let` site (default OFF while
/// it is measured; it also requires the schedule itself to be on).
pub fn arms_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        schedule_enabled() && std::env::var("KARAC_DROP_SCHEDULE_ARMS").ok().as_deref() == Some("1")
    })
}

pub mod audit {
    use super::*;
    use std::collections::HashSet;
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};

    /// The open log and the disagreements already written to it.
    type Sink = Mutex<(std::fs::File, HashSet<String>)>;

    fn sink() -> Option<&'static Sink> {
        static SINK: OnceLock<Option<Sink>> = OnceLock::new();
        SINK.get_or_init(|| {
            if std::env::var("KARAC_DROP_SCHEDULE").ok().as_deref() != Some("audit") {
                return None;
            }
            let path = std::env::var("KARAC_DROP_SCHEDULE_AUDIT").ok()?;
            if !path.contains('/') {
                eprintln!(
                    "KARAC_DROP_SCHEDULE_AUDIT must be a path containing '/'; got {path:?} -- audit off"
                );
                return None;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()?;
            Some(Mutex::new((file, HashSet::new())))
        })
        .as_ref()
    }

    /// Whether auditing is armed (cheap after the first call).
    pub fn armed() -> bool {
        sink().is_some()
    }

    /// Step 4: record a legacy arm reads-only answer against the arm fate.
    /// `site` names the consumer; `binds` are the bindings the legacy
    /// predicate considered. Returns `legacy` unchanged.
    pub fn check_arm(
        site: &str,
        program: Option<&Program>,
        pattern: &Pattern,
        binds: &[(String, Option<TypeExpr>)],
        guard: Option<&Expr>,
        body: ArmBody<'_>,
        legacy: bool,
    ) -> bool {
        if binds.is_empty() {
            return legacy;
        }
        // Step 4, behind its own switch while it is measured: an EXACT arm
        // fate answers in place of the legacy predicate at every site.
        if super::arms_enabled() && !program.is_some_and(|p| pattern_enum_is_rc_backed(p, pattern))
        {
            let fact = binds
                .iter()
                .map(|(n, t)| arm_binding_fate(program, t.as_ref(), guard, body, n).reads_only())
                .try_fold(true, |acc, r| r.map(|b| acc && b));
            return fact.unwrap_or(legacy);
        }
        let Some(sink) = sink() else {
            return legacy;
        };
        let fates: Vec<(String, ArmFate)> = binds
            .iter()
            .map(|(n, t)| {
                (
                    n.clone(),
                    arm_binding_fate(program, t.as_ref(), guard, body, n),
                )
            })
            .collect();
        let fact = fates
            .iter()
            .map(|(_, f)| f.reads_only())
            .try_fold(true, |acc, r| r.map(|b| acc && b));
        if fact == Some(legacy) {
            return legacy;
        }
        let text = match body {
            ArmBody::Expr(e) => crate::formatter::render_expr(e),
            ArmBody::Block(b) => crate::formatter::render_expr(&Expr {
                kind: ExprKind::Block(b.clone()),
                span: b.span,
            }),
        }
        .replace('\n', " ");
        let rendered: Vec<String> = fates
            .iter()
            .map(|(n, f)| {
                format!(
                    "{n}:{}{}",
                    f.render(),
                    if binds.iter().any(|(m, t)| m == n && t.is_some()) {
                        ""
                    } else {
                        "~"
                    }
                )
            })
            .collect();
        let line = format!(
            "{site}\t{legacy}\t{}\t{}\t{text}\n",
            match fact {
                Some(b) => b.to_string(),
                None => "inexact".to_string(),
            },
            rendered.join(",")
        );
        if let Ok(mut g) = sink.lock() {
            let key = format!("{site}/{text}");
            if g.1.insert(key) {
                let _ = g.0.write_all(line.as_bytes());
            }
        }
        legacy
    }

    /// Record `predicate`'s `legacy` answer for `f`'s param `idx` against
    /// `fact`'s derivation of the same question. Returns `legacy` unchanged.
    pub fn check(
        predicate: &str,
        program: Option<&Program>,
        f: &Function,
        idx: usize,
        legacy: bool,
        derive: impl Fn(&ParamFate) -> bool,
    ) -> bool {
        // Slice 4 step 2: under `KARAC_DROP_SCHEDULE=1` an EXACT fate answers
        // in place of the legacy predicate, for every consumer at once, so the
        // two backends cannot be flipped one at a time. An inexact fate
        // (an exit the walker could not follow) keeps the legacy answer.
        if super::schedule_enabled()
            && !f.params.get(idx).is_some_and(|p| is_scalar_ty(&p.ty))
            && super::flip_reaches(f, idx)
        {
            if let Some(fate) = param_fate(program, f, idx).filter(|x| x.is_exact() && x.whole_only)
            {
                return derive(&fate);
            }
            return legacy;
        }
        let Some(sink) = sink() else {
            return legacy;
        };
        // A scalar parameter runs no body and frees nothing, so the legacy
        // predicates are free to answer anything for it; comparing them is
        // noise.
        if f.params.get(idx).is_some_and(|p| is_scalar_ty(&p.ty)) {
            return legacy;
        }
        let Some(fate) = param_fate(program, f, idx) else {
            return legacy;
        };
        let fact = derive(&fate);
        if fact != legacy {
            let text = crate::formatter::format_program(&Program {
                items: vec![Item::Function(f.clone())],
                ..Default::default()
            })
            .replace('\n', " ");
            let line = format!(
                "{predicate}\t{}\t{idx}\t{legacy}\t{fact}\t{}\t{}\t{text}\n",
                f.name,
                fate.render(),
                fate.is_exact()
            );
            if let Ok(mut g) = sink.lock() {
                let key = format!("{predicate}/{idx}/{text}");
                if g.1.insert(key) {
                    let _ = g.0.write_all(line.as_bytes());
                }
            }
        }
        legacy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fate_of(src: &str, fname: &str, idx: usize) -> ParamFate {
        let prog = crate::parse(src).program;
        let f = prog
            .items
            .iter()
            .find_map(|i| match i {
                Item::Function(f) if f.name == fname => Some(f.clone()),
                _ => None,
            })
            .expect("fn");
        param_fate(Some(&prog), &f, idx).expect("by-value param")
    }

    const PRE: &str = "struct R { id: i64 }\nstruct H { r: R }\n";

    /// The fate of binding `b` in the first arm of the first `match` that is
    /// `main`'s final expression.
    fn arm_fate_of(src: &str, b: &str) -> ArmFate {
        let prog = crate::parse(src).program;
        let main = prog
            .items
            .iter()
            .find_map(|i| match i {
                Item::Function(f) if f.name == "main" => Some(f.clone()),
                _ => None,
            })
            .expect("main");
        let Some(ExprKind::Match { arms, .. }) = main.body.final_expr.as_deref().map(|e| &e.kind)
        else {
            panic!("main's tail is not a match")
        };
        let ty = payload_binding_ty(&prog, &arms[0].pattern, b).cloned();
        arm_binding_fate(
            Some(&prog),
            ty.as_ref(),
            arms[0].guard.as_ref(),
            ArmBody::Expr(&arms[0].body),
            b,
        )
    }

    const ARM_PRE: &str = "struct R { id: i64, s: String }\nenum E { A(R), B }\nfn eat(r: R) { }\nfn look(r: ref R) -> i64 { r.id }\n";

    #[test]
    fn arm_binding_read_through_print_and_scalar_field_stays() {
        for body in [
            "println(f\"{x.id}\")",
            "{ println(x.s); println(x.id) }",
            "look(x)",
            "{ match o2 { E.A(y) => println(y.id), E.B => {} }; println(x.id) }",
        ] {
            let src = format!("{ARM_PRE}fn main() {{ let o = E.A(R {{ id: 1, s: \"a\" }}); let o2 = E.B; match o {{ E.A(x) => {body}, E.B => {{}} }} }}");
            let f = arm_fate_of(&src, "x");
            assert_eq!(f.reads_only(), Some(true), "{body}: {}", f.render());
        }
    }

    #[test]
    fn arm_binding_handed_on_is_not_read_only() {
        for (body, want) in [
            ("eat(x)", "c"),
            ("{ let y = x; println(y.id) }", "c"),
            ("Some(x)", "V"),
            ("{ return Some(x) }", "R"),
        ] {
            let src = format!("{ARM_PRE}fn main() -> Option[R] {{ let o = E.A(R {{ id: 1, s: \"a\" }}); match o {{ E.A(x) => {body}, E.B => None }} }}");
            let f = arm_fate_of(&src, "x");
            assert_eq!(f.reads_only(), Some(false), "{body}: {}", f.render());
            assert!(f.render().contains(want), "{body}: {}", f.render());
        }
    }

    #[test]
    fn arm_binding_destructured_by_a_nested_match_and_read_stays() {
        let src = format!("{ARM_PRE}enum W {{ P(E), Q }}\nfn main() -> i64 {{ let w = W.P(E.B); match w {{ W.P(e) => {{ match e {{ E.A(r) => {{ return r.id; }}, E.B => {{ return 0; }} }} }}, W.Q => 1 }} }}");
        let f = arm_fate_of(&src, "e");
        assert_eq!(f.reads_only(), Some(true), "{}", f.render());
    }

    #[test]
    fn desugared_scalar_operator_on_a_field_moves_nothing() {
        let f = fate_of(
            &format!("{PRE}fn f(a: R) -> R {{ let mut c = a; c.id = i64.add(c.id, 10); c }}"),
            "f",
            0,
        );
        assert_eq!(f.exits, vec![ExitFate::Returned], "{}", f.render());
    }

    #[test]
    fn bare_return_is_returned_always() {
        let f = fate_of(&format!("{PRE}fn f(r: R) -> R {{ r }}"), "f", 0);
        assert_eq!(f.returned_whole(), Coverage::Always);
    }

    #[test]
    fn local_rebind_then_dies_stays() {
        let f = fate_of(
            &format!("{PRE}fn f(r: R) {{ let x = r; println(\"mid\") }}"),
            "f",
            0,
        );
        assert_eq!(f.exits, vec![ExitFate::Stays]);
    }

    #[test]
    fn rebind_then_return_follows_the_alias() {
        let f = fate_of(
            &format!("{PRE}fn f(a: R) -> R {{ let mut c = a; c.id = c.id + 10; c }}"),
            "f",
            0,
        );
        assert_eq!(f.returned_whole(), Coverage::Always);
    }

    #[test]
    fn wrapped_in_returned_literal_is_returned() {
        let f = fate_of(
            &format!("{PRE}fn f(r: R) -> H {{ return H {{ r: r }} }}"),
            "f",
            0,
        );
        assert_eq!(f.returned_whole(), Coverage::Always);
    }

    #[test]
    fn conditional_return_is_some() {
        let f = fate_of(
            &format!(
                "{PRE}fn f(r: R, c: bool) -> R {{ if c {{ return r }} return R {{ id: 1 }} }}"
            ),
            "f",
            0,
        );
        assert_eq!(f.returned_whole(), Coverage::Some);
        assert!(f.is_exact());
    }

    #[test]
    fn discarded_literal_stays() {
        let f = fate_of(
            &format!("{PRE}fn f(w: R) {{ let _ = (w, 1); println(\"in\") }}"),
            "f",
            0,
        );
        assert_eq!(f.exits, vec![ExitFate::Stays]);
    }

    #[test]
    fn push_into_borrowed_container_is_stored() {
        let f = fate_of(
            &format!(
                "{PRE}fn w4(a: R, xs: mut ref Vec[H]) {{ let x = H {{ r: a }}; xs.push(x); }}"
            ),
            "w4",
            0,
        );
        assert_eq!(f.stored(), Coverage::Always);
    }

    #[test]
    fn push_into_returned_local_vec_is_returned() {
        let f = fate_of(
            &format!(
                "{PRE}fn cm(x: R, c: bool) -> Vec[R] {{ let mut v = Vec[x]; if c {{ return v }} return Vec[R {{ id: 9 }}] }}"
            ),
            "cm",
            0,
        );
        assert_eq!(f.returned_whole(), Coverage::Some);
    }

    #[test]
    fn handed_to_a_wrapper_whose_result_is_discarded_stays() {
        // B-2026-09-25-33's shape: `wrapC(x);` as a statement.
        let f = fate_of(
            &format!(
                "{PRE}fn wrapC(v: R) -> H {{ return H {{ r: v }} }}\nfn outerC(x: R) {{ wrapC(x); println(\"o\") }}"
            ),
            "outerC",
            0,
        );
        assert_eq!(f.exits, vec![ExitFate::Stays]);
    }

    #[test]
    fn returned_through_a_call_is_returned() {
        let f = fate_of(
            &format!("{PRE}fn id(v: R) -> R {{ v }}\nfn g(x: R) -> R {{ return id(x) }}"),
            "g",
            0,
        );
        assert_eq!(f.returned_whole(), Coverage::Always);
    }

    #[test]
    fn match_arm_payload_returned_is_part() {
        let f = fate_of(
            "struct R { id: i64 }\nfn f(x: Option[R]) -> R { match x { Some(t) => { return t; } None => R { id: 0 } } }",
            "f",
            0,
        );
        assert_eq!(f.returned_any(), Coverage::Some);
        assert!(f.exits.contains(&ExitFate::ReturnedPart));
    }

    #[test]
    fn shadowing_let_drops_the_holder() {
        // B-2026-09-29-61: `let s = 5` shadows the param; it still stays.
        let f = fate_of(
            "struct S { id: i64 }\nfn f(s: S) -> i64 { let s = 5; s }",
            "f",
            0,
        );
        assert_eq!(f.exits, vec![ExitFate::Stays]);
    }

    #[test]
    fn ref_param_has_no_fate() {
        let prog = crate::parse("struct R { id: i64 }\nfn f(r: ref R) -> i64 { r.id }").program;
        let Item::Function(f) = &prog.items[1] else {
            panic!()
        };
        assert!(param_fate(Some(&prog), f, 0).is_none());
    }
}
