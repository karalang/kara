//! v2 core §9.3 (`docs/core-semantics.md`): which closures escape, and which
//! non-escaping function parameters a body stores.
//!
//! A closure *escapes* when it, or a value containing it, is returned, stored
//! in a struct field or a collection, passed to an `escaping` parameter, or
//! sent to a task. An escaping closure captures every place by move, which
//! the use classifier reads off [`CoreEscape::escaping_closures`].
//!
//! A function-typed parameter is non-escaping unless declared
//! `f: escaping Fn(...)`: the callee may call it or pass it to another
//! non-escaping parameter, and nothing else. Storing, returning or binding it
//! is reported in [`CoreEscape::errors`].
//!
//! A plain-AST pass, run only by the strict commands (`karac check`).

use crate::ast::*;
use crate::index_disjoint::{for_each_child_public, Child};
use crate::resolver::SpanKey;
use crate::token::Span;
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Default, Debug)]
pub struct CoreEscape {
    /// Closure expressions that escape, keyed by the closure's span.
    pub escaping_closures: FxHashSet<SpanKey>,
    /// Non-escaping function parameters a body stores, returns or binds.
    pub errors: Vec<(Span, String)>,
    /// Why each closure in [`Self::escaping_closures`] escapes ("returned",
    /// "stored in a collection", ...), for a diagnostic that names it.
    pub reasons: FxHashMap<SpanKey, &'static str>,
    /// A named function used as a value in an escaping position, with the
    /// reason: `return tick` takes `tick` as a value (core §14.3, §14.5).
    pub escaping_fn_values: Vec<(Span, String, &'static str)>,
    /// The closures in [`Self::escaping_closures`] whose only way out is
    /// being returned from another closure: their result stays where the
    /// outer closure's caller keeps it, so the borrows they copy are checked
    /// there (B-2026-10-24-9).
    pub closure_result_only: FxHashSet<SpanKey>,
}

/// Per parameter: `Some(true)` for `escaping Fn(..)`, `Some(false)` for a
/// non-escaping function type, `None` for any other type.
type Sig = Vec<Option<bool>>;

/// Per parameter of a generic function: where a parameter whose type is
/// one of the function's own type parameters goes in its body
/// (B-2026-11-19-104). A closure handed to such a slot goes there too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GenSlot {
    /// Called, borrowed or dropped: the closure stays in the call.
    Stays,
    /// Stored (pushed, put in a field, sent): the closure escapes.
    Stored,
    /// Handed back: the closure is the call's value, and goes where it goes.
    Returned,
}

/// Why a closure handed to a storing generic slot escapes.
const GENERIC_STORE: &str = "passed to a generic parameter its callee stores";

#[derive(Clone, Copy)]
enum Bound {
    NonEscParam,
    /// The function's parameter at this index, typed by one of its own type
    /// parameters.
    GenParam(usize),
    /// A local holding a named function taken as a value (`let g = f;`),
    /// whose name [`Walker::fn_alias`] keeps (B-2026-11-19-105).
    FnValue,
    Closure(SpanKey),
    Other,
}

/// Where a value goes. `Escape` carries the reason, for the diagnostic.
#[derive(Clone, Copy)]
enum Pos {
    Escape(&'static str),
    Bind,
    Other,
}

/// A closure's result goes to its caller, so a closure literal there
/// escapes (core semantics §9.3: "it, or a value containing it, is
/// returned", B-2026-10-26-9). A parameter or function value handed back
/// stays inside the function that holds the closure, so this reason
/// escapes closures only.
const CLOSURE_RESULT: &str = "returned from a closure";

const STORE_METHODS: &[&str] = &["push", "push_back", "push_front", "insert", "set", "append"];

/// The node types a typed caller has, so a method named like a lazy
/// adaptor on an `Option` or `Result` is seen to run its closure at once.
pub type NodeTypes = FxHashMap<crate::ids::NodeId, (crate::typechecker::types::Type, u32)>;

/// Whether a receiver of this type runs the closure a method named like a
/// lazy adaptor is handed at once and keeps nothing: `Option.map`,
/// `Result.map`, `Option.filter`, `Option.take` (docs/library/core-types.md,
/// B-2026-11-18-78).
pub(crate) fn eager_receiver_ty(ty: &crate::typechecker::types::Type) -> bool {
    use crate::typechecker::types::Type;
    match ty {
        Type::Ref(inner) | Type::MutRef(inner) => eager_receiver_ty(inner),
        Type::Named { name, .. } => name == "Option" || name == "Result",
        _ => false,
    }
}

pub fn analyze(program: &Program, types: Option<&NodeTypes>) -> CoreEscape {
    let esc = &program.escaping_fn_types;
    let sig_of = |f: &Function| -> Sig {
        f.params
            .iter()
            .map(|p| match p.ty.kind {
                TypeKind::FnType { .. } => Some(esc.contains(&SpanKey::from_span(&p.ty.span))),
                _ => None,
            })
            .collect()
    };
    let mut sigs: FxHashMap<String, Sig> = FxHashMap::default();
    let mut fns: Vec<&Function> = Vec::new();
    // Free functions, the generic ones of which [`generic_slots`] reads.
    let mut free: Vec<&Function> = Vec::new();
    let mut enums: FxHashSet<String> = ["Option", "Result"].iter().map(|s| s.to_string()).collect();
    for item in &program.items {
        match item {
            Item::EnumDef(e) => {
                enums.insert(e.name.clone());
            }
            Item::Function(f) => {
                sigs.insert(f.name.clone(), sig_of(f));
                fns.push(f);
                free.push(f);
            }
            Item::ImplBlock(imp) => {
                let head = match &imp.target_type.kind {
                    TypeKind::Path(p) => p.segments.last().cloned(),
                    _ => None,
                };
                for it in &imp.items {
                    if let ImplItem::Method(m) = it {
                        if let Some(h) = &head {
                            sigs.insert(format!("{h}.{}", m.name), sig_of(m.as_ref()));
                        }
                        fns.push(m.as_ref());
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = CoreEscape::default();
    let gen = generic_slots(&free, &sigs, &enums, esc, types);
    // The standard library is checked by its own rules, not the user's.
    for f in fns.into_iter().filter(|f| !f.stdlib_origin) {
        let mut w = Walker {
            sigs: &sigs,
            enums: &enums,
            esc,
            scopes: Vec::new(),
            out: &mut out,
            rebound: FxHashMap::default(),
            closure_holds: FxHashMap::default(),
            types,
            gen: &gen,
            gen_hits: None,
            fn_alias: FxHashMap::default(),
            other_params: FxHashSet::default(),
            held_other: FxHashMap::default(),
        };
        w.walk_fn(f);
    }
    out
}

/// Per generic function, where each parameter typed by one of its own type
/// parameters goes in the body: `fn keep[F](v: mut ref Vec[F], f: own F) {
/// v.push(f); }` stores `f`, `fn id[T](x: own T) -> T { x }` returns it. A
/// callee that hands the parameter to another generic slot inherits that
/// slot's fate, so the walk repeats until nothing changes.
fn generic_slots(
    fns: &[&Function],
    sigs: &FxHashMap<String, Sig>,
    enums: &FxHashSet<String>,
    esc: &FxHashSet<SpanKey>,
    types: Option<&NodeTypes>,
) -> FxHashMap<String, Vec<GenSlot>> {
    let mut gen: FxHashMap<String, Vec<GenSlot>> = FxHashMap::default();
    // (key, function, the indices of its generic-typed parameters)
    let mut cands: Vec<(String, &Function, Vec<usize>)> = Vec::new();
    for f in fns {
        let key = f.name.clone();
        let Some(gp) = &f.generic_params else {
            continue;
        };
        let names: FxHashSet<&str> = gp.params.iter().map(|p| p.name.as_str()).collect();
        let idx: Vec<usize> = f
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| match &p.ty.kind {
                TypeKind::Path(path) => {
                    path.segments.len() == 1 && names.contains(path.segments[0].as_str())
                }
                _ => false,
            })
            .map(|(i, _)| i)
            .collect();
        if !idx.is_empty() {
            gen.insert(key.clone(), vec![GenSlot::Stays; f.params.len()]);
            cands.push((key, f, idx));
        }
    }
    for _ in 0..8 {
        let mut changed = false;
        for (key, f, idx) in &cands {
            let mut scratch = CoreEscape::default();
            let mut w = Walker {
                sigs,
                enums,
                esc,
                scopes: Vec::new(),
                out: &mut scratch,
                rebound: FxHashMap::default(),
                closure_holds: FxHashMap::default(),
                types,
                gen: &gen,
                gen_hits: Some(Vec::new()),
                fn_alias: FxHashMap::default(),
                other_params: FxHashSet::default(),
                held_other: FxHashMap::default(),
            };
            w.walk_fn_generic(f, idx);
            let hits = w.gen_hits.take().unwrap_or_default();
            let mut slots = gen[key].clone();
            for (i, why) in hits {
                let new = if why == "returned" {
                    GenSlot::Returned
                } else {
                    GenSlot::Stored
                };
                // A parameter both returned and stored is stored.
                if slots[i] == GenSlot::Stays || new == GenSlot::Stored {
                    slots[i] = new;
                }
            }
            if slots != gen[key] {
                gen.insert(key.clone(), slots);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    gen
}

struct Walker<'a> {
    sigs: &'a FxHashMap<String, Sig>,
    /// The enums' names, whose `E.V(x)` constructs a value.
    enums: &'a FxHashSet<String>,
    esc: &'a FxHashSet<SpanKey>,
    scopes: Vec<FxHashMap<String, Bound>>,
    out: &'a mut CoreEscape,
    /// A local bound from a non-escaping parameter (`let mut f = g;`), to
    /// the parameter it holds, for the diagnostic (B-2026-11-04-62).
    rebound: FxHashMap<String, String>,
    /// A closure bound by `let` that captures non-escaping parameters, to
    /// those parameters: where the binding escapes, they would be stored
    /// with it (B-2026-11-19-103).
    closure_holds: FxHashMap<SpanKey, Vec<String>>,
    types: Option<&'a NodeTypes>,
    /// [`generic_slots`]'s answer so far.
    gen: &'a FxHashMap<String, Vec<GenSlot>>,
    /// While [`generic_slots`] walks a generic function: where each of its
    /// generic-typed parameters goes, by index and reason.
    gen_hits: Option<Vec<(usize, &'static str)>>,
    /// The function a [`Bound::FnValue`] local holds.
    fn_alias: FxHashMap<String, String>,
    /// In a method with a `self` or `mut ref self` receiver, whose result
    /// borrows `self` only (§5.4): its other parameters' names. A closure
    /// handed back that names one is no view result (B-2026-11-26-88).
    other_params: FxHashSet<String>,
    /// In such a method, a closure bound by `let` that reaches one of
    /// [`Self::other_params`], directly or through another such closure it
    /// names, to those closures: handed back, every one of them escapes
    /// (B-2026-11-26-89).
    held_other: FxHashMap<SpanKey, Vec<SpanKey>>,
}

impl Walker<'_> {
    fn lookup(&self, name: &str) -> Option<Bound> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn bind(&mut self, name: String, b: Bound) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name, b);
        }
    }

    fn bind_pattern(&mut self, p: &Pattern) {
        for n in p.binding_names() {
            self.bind(n, Bound::Other);
        }
    }

    fn walk_fn(&mut self, f: &Function) {
        let mut scope = FxHashMap::default();
        for p in &f.params {
            let non_esc = matches!(p.ty.kind, TypeKind::FnType { .. })
                && !self.esc.contains(&SpanKey::from_span(&p.ty.span));
            for n in p.pattern.binding_names() {
                scope.insert(
                    n,
                    if non_esc {
                        Bound::NonEscParam
                    } else {
                        Bound::Other
                    },
                );
            }
        }
        self.other_params = if matches!(f.self_param, Some(SelfParam::Ref | SelfParam::MutRef)) {
            scope.keys().cloned().collect()
        } else {
            FxHashSet::default()
        };
        self.scopes.push(scope);
        self.walk_block(&f.body, Pos::Escape("returned"));
        self.scopes.pop();
        self.other_params.clear();
        self.held_other.clear();
    }

    /// [`Self::walk_fn`] for [`generic_slots`]: the parameters at `idx` are
    /// followed, and every other one is plain.
    fn walk_fn_generic(&mut self, f: &Function, idx: &[usize]) {
        let mut scope = FxHashMap::default();
        for (i, p) in f.params.iter().enumerate() {
            let b = if idx.contains(&i) {
                Bound::GenParam(i)
            } else {
                Bound::Other
            };
            for n in p.pattern.binding_names() {
                scope.insert(n, b);
            }
        }
        self.scopes.push(scope);
        self.walk_block(&f.body, Pos::Escape("returned"));
        self.scopes.pop();
    }

    fn walk_block(&mut self, b: &Block, tail: Pos) {
        self.scopes.push(FxHashMap::default());
        for s in &b.stmts {
            self.walk_stmt(s);
        }
        if let Some(e) = &b.final_expr {
            self.walk_expr(e, tail);
        }
        self.scopes.pop();
    }

    fn walk_stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let { pattern, value, .. } => {
                if let PatternKind::Binding(name) = &pattern.kind {
                    let b = match &value.kind {
                        ExprKind::Closure { .. } => {
                            self.walk_expr(value, Pos::Other);
                            let k = SpanKey::from_span(&value.span);
                            let held = self.captured_params(value);
                            if !held.is_empty() {
                                self.closure_holds.insert(k, held);
                            }
                            if let ExprKind::Closure { params, body, .. } = &value.kind {
                                if let Some(subs) = self.other_param_reach(params, body) {
                                    self.held_other.insert(k, subs);
                                }
                            }
                            Bound::Closure(k)
                        }
                        ExprKind::Identifier(n) => match self.lookup(n) {
                            Some(Bound::Closure(k)) => Bound::Closure(k),
                            // `let mut f = f;` is how design.md §7 calls an
                            // `own MutFn` parameter. A local rebind stores
                            // nothing; the new name is held to the same rule
                            // (B-2026-11-04-62).
                            Some(Bound::NonEscParam) => {
                                let origin = self.rebound.get(n).cloned().unwrap_or(n.clone());
                                self.rebound.insert(name.clone(), origin);
                                Bound::NonEscParam
                            }
                            Some(Bound::GenParam(i)) => Bound::GenParam(i),
                            // `let g = read_clk;` takes the function as a
                            // value; where `g` goes, it goes (§14.3).
                            None if self.sigs.contains_key(n) => {
                                self.fn_alias.insert(name.clone(), n.clone());
                                Bound::FnValue
                            }
                            Some(Bound::FnValue) => {
                                let f = self.fn_alias.get(n).cloned().unwrap_or(n.clone());
                                self.fn_alias.insert(name.clone(), f);
                                Bound::FnValue
                            }
                            _ => {
                                self.walk_expr(value, Pos::Bind);
                                Bound::Other
                            }
                        },
                        _ => {
                            self.walk_expr(value, Pos::Other);
                            // `let it = v.iter().filter(f)`: the adaptor
                            // holds `f`, so `it` is held to `f`'s rule
                            // (B-2026-11-19-103).
                            match self.adaptor_holds(value) {
                                Some(p) => {
                                    self.rebound.insert(name.clone(), p);
                                    Bound::NonEscParam
                                }
                                None => Bound::Other,
                            }
                        }
                    };
                    self.bind(name.clone(), b);
                } else {
                    self.walk_expr(value, Pos::Other);
                    self.bind_pattern(pattern);
                }
            }
            StmtKind::LetUninit { name, .. } => self.bind(name.clone(), Bound::Other),
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.walk_expr(value, Pos::Other);
                self.walk_block(else_block, Pos::Other);
                self.bind_pattern(pattern);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body, Pos::Other)
            }
            StmtKind::Assign { target, value } => {
                self.walk_expr(target, Pos::Other);
                let pos = match &target.kind {
                    ExprKind::Identifier(_) => Pos::Bind,
                    ExprKind::Index { .. } => Pos::Escape("stored in a collection"),
                    _ => Pos::Escape("stored in a field"),
                };
                self.walk_expr(value, pos);
            }
            StmtKind::MultiAssign { targets, values } => {
                for e in targets.iter().chain(values) {
                    self.walk_expr(e, Pos::Other);
                }
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target, Pos::Other);
                self.walk_expr(value, Pos::Other);
            }
            StmtKind::Expr(e) => self.walk_expr(e, Pos::Other),
        }
    }

    fn escapes(&mut self, k: SpanKey, why: &'static str) {
        // Returned from the function is a result too: a closure that
        // captures a borrowed parameter is then a view result borrowing the
        // argument, which MIR checks where the caller keeps it (core
        // semantics §5.7, §9.3, B-2026-11-26-74).
        if why != CLOSURE_RESULT && why != "returned" {
            self.out.closure_result_only.remove(&k);
        } else if !self.out.escaping_closures.contains(&k) {
            self.out.closure_result_only.insert(k);
        }
        self.out.escaping_closures.insert(k);
        self.out.reasons.insert(k, why);
    }

    fn walk_args(&mut self, args: &[CallArg], sig: Option<&Sig>, all: Option<&'static str>) {
        for (i, a) in args.iter().enumerate() {
            let pos = match (all, sig.and_then(|s| s.get(i).copied().flatten())) {
                (Some(r), _) => Pos::Escape(r),
                (None, Some(true)) => Pos::Escape("passed to an `escaping` parameter"),
                _ => Pos::Other,
            };
            self.walk_expr(&a.value, pos);
        }
    }

    /// A call of a generic function: an argument in a slot its callee stores
    /// escapes, one in a slot it returns goes where the call's value goes
    /// (B-2026-11-19-104), and the rest are as [`Self::walk_args`] has them.
    fn walk_generic_args(
        &mut self,
        args: &[CallArg],
        sig: Option<&Sig>,
        slots: &[GenSlot],
        pos: Pos,
    ) {
        for (i, a) in args.iter().enumerate() {
            let p = match slots.get(i) {
                Some(GenSlot::Stored) => Pos::Escape(GENERIC_STORE),
                Some(GenSlot::Returned) => match pos {
                    Pos::Escape(why) => Pos::Escape(why),
                    _ => Pos::Other,
                },
                _ => match sig.and_then(|s| s.get(i).copied().flatten()) {
                    Some(true) => Pos::Escape("passed to an `escaping` parameter"),
                    _ => Pos::Other,
                },
            };
            self.walk_expr(&a.value, p);
        }
    }

    fn walk_expr(&mut self, e: &Expr, pos: Pos) {
        match &e.kind {
            ExprKind::Identifier(n) => match (self.lookup(n), pos) {
                (Some(Bound::Closure(k)), Pos::Escape(why)) => {
                    if (why == "returned" || why == CLOSURE_RESULT)
                        && self.held_other.contains_key(&k)
                    {
                        self.escapes_unviewed(k, why);
                    }
                    self.escapes(k, why);
                    // `let g = |x| f(x); g`: the closure stores the
                    // parameters it captures (B-2026-11-19-103).
                    if why != CLOSURE_RESULT {
                        for p in self.closure_holds.get(&k).cloned().unwrap_or_default() {
                            self.out.errors.push((
                                e.span,
                                format!(
                                    "`{n}` captures parameter `{p}`, which is a non-escaping \
                                     function value, so `{n}` cannot be {why}: the closure \
                                     stores it. A function-typed parameter may only be called \
                                     or passed to another non-escaping parameter. Declare it \
                                     `{p}: escaping Fn(...)` to store or return it \
                                     (core-semantics.md §9.3)"
                                ),
                            ));
                        }
                    }
                }
                (None, Pos::Escape(why)) if self.sigs.contains_key(n) => {
                    // A closure's result is a value it hands out, a function
                    // taken as a value too (`|| Some(read_clk)`, B-2026-11-19-138).
                    self.out.escaping_fn_values.push((e.span, n.clone(), why));
                }
                (Some(Bound::NonEscParam), Pos::Escape(why)) if why != CLOSURE_RESULT => {
                    self.non_escaping(e, n, why)
                }
                (Some(Bound::NonEscParam), Pos::Bind) => {
                    self.non_escaping(e, n, "bound to a variable")
                }
                (Some(Bound::FnValue), Pos::Escape(why)) => {
                    let f = self.fn_alias.get(n).cloned().unwrap_or(n.clone());
                    self.out.escaping_fn_values.push((e.span, f, why));
                }
                (Some(Bound::GenParam(i)), Pos::Escape(why)) if why != CLOSURE_RESULT => {
                    if let Some(h) = &mut self.gen_hits {
                        h.push((i, why));
                    }
                }
                _ => {}
            },
            ExprKind::Closure { params, body, .. } => {
                if let Pos::Escape(why) = pos {
                    // Handed back from a method whose result borrows `self`
                    // only, a closure over another parameter cannot be the
                    // view result that parameter's argument would back: it
                    // escapes, and captures that parameter by move (§5.4,
                    // §9.3). A caller that kept it past the argument read
                    // freed memory (B-2026-11-26-88).
                    let k = SpanKey::from_span(&e.span);
                    let reach = if why == "returned" || why == CLOSURE_RESULT {
                        self.other_param_reach(params, body)
                    } else {
                        None
                    };
                    if let Some(subs) = reach {
                        // A closure it reaches the parameter through goes
                        // with it, so that one captures it by move too
                        // (B-2026-11-26-89).
                        self.escapes(k, why);
                        self.out.closure_result_only.remove(&k);
                        for sub in subs {
                            self.escapes_unviewed(sub, why);
                        }
                    } else {
                        self.escapes(k, why);
                    }
                    // An escaping closure captures by move, so a
                    // non-escaping function parameter it names is stored
                    // with it (§9.3; B-2026-11-02-31).
                    if why != CLOSURE_RESULT {
                        let mut bound: FxHashSet<String> = params
                            .iter()
                            .flat_map(|p| p.pattern.binding_names())
                            .collect();
                        let mut names = Vec::new();
                        names_in(body, &mut names, &mut bound);
                        let mut seen = FxHashSet::default();
                        for (n, span) in names {
                            if bound.contains(&n) || !seen.insert(n.clone()) {
                                continue;
                            }
                            if let Some(Bound::NonEscParam) = self.lookup(&n) {
                                let (subject, param) = self.non_escaping_subject(&n);
                                self.out.errors.push((
                                    span,
                                    format!(
                                        "{subject} is a non-escaping function value and \
                                         cannot be captured by a closure that is {why}: the \
                                         closure stores it. A function-typed parameter may only \
                                         be called or passed to another non-escaping parameter. \
                                         Declare it `{param}: escaping Fn(...)` to store or \
                                         return it (core-semantics.md §9.3)"
                                    ),
                                ));
                            }
                        }
                    }
                }
                let mut scope = FxHashMap::default();
                for p in params {
                    for n in p.pattern.binding_names() {
                        scope.insert(n, Bound::Other);
                    }
                }
                self.scopes.push(scope);
                self.walk_expr(body, Pos::Escape(CLOSURE_RESULT));
                self.scopes.pop();
            }
            ExprKind::Call { callee, args } => {
                match &callee.kind {
                    ExprKind::Identifier(name) => {
                        let local = self.lookup(name).is_some();
                        if name == "spawn" && !local {
                            self.walk_args(args, None, Some("sent to a task"));
                        } else if matches!(name.as_str(), "Some" | "Ok" | "Err") && !local {
                            self.walk_args(args, None, value_of(pos));
                        } else if let (false, Some(slots)) = (local, self.gen.get(name)) {
                            let slots = slots.clone();
                            let sig = self.sigs.get(name);
                            self.walk_generic_args(args, sig, &slots, pos);
                        } else {
                            let sig = if local { None } else { self.sigs.get(name) };
                            self.walk_args(args, sig, None);
                        }
                    }
                    ExprKind::Path { segments, .. } => {
                        let key = segments.join(".");
                        let variant = segments.len() >= 2
                            && self.enums.contains(&segments[segments.len() - 2]);
                        match self.sigs.get(&key) {
                            Some(sig) => self.walk_args(args, Some(sig), None),
                            // An enum variant constructor: a payload is a
                            // positional field, so a closure in one escapes
                            // as in a struct field or a tuple, even in a
                            // local value (§9.3, B-2026-11-07-5).
                            None if variant => self.walk_args(
                                args,
                                None,
                                Some(value_of(pos).unwrap_or("stored in an enum payload")),
                            ),
                            // A built-in associated function (`Vec.from_fn`):
                            // its function-typed parameters do not escape
                            // (§9.3, B-2026-10-25-20).
                            None => self.walk_args(args, None, None),
                        }
                    }
                    _ => {
                        self.walk_expr(callee, Pos::Other);
                        self.walk_args(args, None, None);
                    }
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                // B-2026-11-15-32: a lazy adaptor's result holds the closure it
                // is handed (and the iterator it was called on), so where the
                // result escapes, both do: a closure returning
                // `(0..3).map(|c| m + clears[c])` returns that closure.
                // `Option.map` and `Result.map` share the name but run the
                // closure at once (B-2026-11-18-78).
                let eager = self
                    .types
                    .and_then(|t| t.get(&object.id))
                    .is_some_and(|(t, _)| eager_receiver_ty(t));
                if let (Pos::Escape(why), true) =
                    (pos, LAZY_ADAPTORS.contains(&method.as_str()) && !eager)
                {
                    self.walk_expr(object, Pos::Escape(why));
                    for a in args {
                        self.walk_expr(&a.value, Pos::Escape(why));
                    }
                    return;
                }
                self.walk_expr(object, Pos::Other);
                // `TaskGroup.spawn` is not here: a task that joins inside the
                // scope of every origin may hold views (§5.7), so its closure
                // borrows. Only an unstructured `spawn` escapes.
                if STORE_METHODS.contains(&method.as_str()) {
                    self.walk_args(args, None, Some("stored in a collection"));
                } else {
                    let suffix = format!(".{method}");
                    let escaping: Vec<bool> = (0..args.len())
                        .map(|i| {
                            self.sigs.iter().any(|(k, s)| {
                                k.ends_with(&suffix) && s.get(i).copied().flatten() == Some(true)
                            })
                        })
                        .collect();
                    for (a, esc) in args.iter().zip(escaping) {
                        let pos = if esc {
                            Pos::Escape("passed to an `escaping` parameter")
                        } else {
                            Pos::Other
                        };
                        self.walk_expr(&a.value, pos);
                    }
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_expr(&f.value, Pos::Escape("stored in a struct field"));
                }
                if let Some(s) = spread {
                    self.walk_expr(s, Pos::Other);
                }
            }
            // `Vec[f, g]` and `[f; 3]` store their elements as `[f, g]`
            // does (B-2026-11-17-24).
            ExprKind::ArrayLiteral(es) | ExprKind::PrefixCollectionLiteral { items: es, .. } => {
                for x in es {
                    self.walk_expr(x, Pos::Escape("stored in a collection"));
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value, Pos::Escape("stored in a collection"));
                self.walk_expr(count, Pos::Other);
            }
            // A tuple element is a positional field, so a closure in one
            // escapes as in a struct field (§9.3, B-2026-11-02-39).
            ExprKind::Tuple(es) => {
                let pos = match pos {
                    Pos::Escape(why) => Pos::Escape(why),
                    _ => Pos::Escape("stored in a tuple"),
                };
                for x in es {
                    self.walk_expr(x, pos);
                }
            }
            ExprKind::Return(Some(v)) => self.walk_expr(v, Pos::Escape("returned")),
            ExprKind::Block(b) => self.walk_block(b, tail_of(pos)),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
                ..
            } => {
                self.walk_expr(condition, Pos::Other);
                self.walk_block(then_block, tail_of(pos));
                if let Some(e) = else_branch {
                    self.walk_expr(e, tail_of(pos));
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee, Pos::Other);
                for arm in arms {
                    self.scopes.push(FxHashMap::default());
                    self.bind_pattern(&arm.pattern);
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g, Pos::Other);
                    }
                    self.walk_expr(&arm.body, tail_of(pos));
                    self.scopes.pop();
                }
            }
            _ => {
                let mut kids: Vec<Child<'_>> = Vec::new();
                for_each_child_public(e, &mut |c| kids.push(c));
                for c in kids {
                    match c {
                        Child::Expr(x) => self.walk_expr(x, Pos::Other),
                        Child::Block(b) => self.walk_block(b, Pos::Other),
                    }
                }
            }
        }
    }

    /// A closure of [`Self::held_other`] escaping from such a method: it,
    /// and every closure it reaches the parameter through, is no view
    /// result.
    fn escapes_unviewed(&mut self, k: SpanKey, why: &'static str) {
        let mut todo = vec![k];
        let mut seen = FxHashSet::default();
        while let Some(k) = todo.pop() {
            if !seen.insert(k) {
                continue;
            }
            self.escapes(k, why);
            self.out.closure_result_only.remove(&k);
            todo.extend(self.held_other.get(&k).cloned().unwrap_or_default());
        }
    }

    /// Whether a closure with these parameters and body reaches one of
    /// [`Self::other_params`]: names it as the function's parameter, not a
    /// local that shadows it, or names a closure of [`Self::held_other`].
    /// The answer lists the closures of the second kind.
    fn other_param_reach(&self, params: &[ClosureParam], body: &Expr) -> Option<Vec<SpanKey>> {
        if self.other_params.is_empty() {
            return None;
        }
        let mut bound: FxHashSet<String> = params
            .iter()
            .flat_map(|p| p.pattern.binding_names())
            .collect();
        let mut names = Vec::new();
        names_in(body, &mut names, &mut bound);
        let mut direct = false;
        let mut subs = Vec::new();
        for (n, _) in &names {
            if bound.contains(n) {
                continue;
            }
            match self.lookup(n) {
                Some(Bound::Closure(k)) if self.held_other.contains_key(&k) => subs.push(k),
                _ => {
                    direct |= self.other_params.contains(n)
                        && self.scopes.iter().rposition(|s| s.contains_key(n)) == Some(0)
                }
            }
        }
        (direct || !subs.is_empty()).then_some(subs)
    }

    /// The non-escaping parameters the closure `e` names from outside
    /// itself, each as the parameter it holds.
    fn captured_params(&self, e: &Expr) -> Vec<String> {
        let mut bound: FxHashSet<String> = match &e.kind {
            ExprKind::Closure { params, .. } => params
                .iter()
                .flat_map(|p| p.pattern.binding_names())
                .collect(),
            _ => FxHashSet::default(),
        };
        let mut names = Vec::new();
        match &e.kind {
            ExprKind::Closure { body, .. } => names_in(body, &mut names, &mut bound),
            _ => return Vec::new(),
        }
        let mut out: Vec<String> = Vec::new();
        for (n, _) in names {
            if bound.contains(&n) {
                continue;
            }
            if let Some(Bound::NonEscParam) = self.lookup(&n) {
                let p = self.non_escaping_subject(&n).1;
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// The non-escaping parameter a lazy adaptor chain holds
    /// (`v.iter().filter(f)`, `xs.map(|x| f(x))`): one handed to an adaptor
    /// directly or captured by a closure handed to one, or held by the
    /// value the chain starts from.
    fn adaptor_holds(&self, e: &Expr) -> Option<String> {
        match &e.kind {
            ExprKind::Identifier(n) => match self.lookup(n) {
                Some(Bound::NonEscParam) => Some(self.non_escaping_subject(n).1),
                Some(Bound::Closure(k)) => {
                    self.closure_holds.get(&k).and_then(|v| v.first().cloned())
                }
                _ => None,
            },
            ExprKind::Closure { .. } => self.captured_params(e).into_iter().next(),
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if LAZY_ADAPTORS.contains(&method.as_str()) => {
                let eager = self
                    .types
                    .and_then(|t| t.get(&object.id))
                    .is_some_and(|(t, _)| eager_receiver_ty(t));
                if eager {
                    return None;
                }
                args.iter()
                    .find_map(|a| self.adaptor_holds(&a.value))
                    .or_else(|| self.adaptor_holds(object))
            }
            _ => None,
        }
    }

    /// How a diagnostic names a non-escaping function value: the parameter
    /// itself, or a local that holds one (`let mut g = f;`).
    fn non_escaping_subject(&self, name: &str) -> (String, String) {
        match self.rebound.get(name) {
            Some(p) if p != name => (format!("`{name}` holds parameter `{p}`, which"), p.clone()),
            _ => (format!("parameter `{name}`"), name.to_string()),
        }
    }

    fn non_escaping(&mut self, e: &Expr, name: &str, why: &str) {
        let (subject, param) = self.non_escaping_subject(name);
        self.out.errors.push((
            e.span,
            format!(
                "{subject} is a non-escaping function value and cannot be {why}: \
                 a function-typed parameter may only be called or passed to another \
                 non-escaping parameter. Declare it `{param}: escaping Fn(...)` to store or \
                 return it (core-semantics.md §9.3)"
            ),
        ));
    }
}

/// Every identifier `e` names, with its span, in source order, and in
/// `bound` every name a `let` or pattern inside it binds.
fn names_in(e: &Expr, out: &mut Vec<(String, Span)>, bound: &mut FxHashSet<String>) {
    if let ExprKind::Identifier(n) = &e.kind {
        out.push((n.clone(), e.span));
        return;
    }
    if let ExprKind::Closure { params, .. } = &e.kind {
        for p in params {
            bound.extend(p.pattern.binding_names());
        }
    }
    if let ExprKind::Match { arms, .. } = &e.kind {
        for arm in arms {
            bound.extend(arm.pattern.binding_names());
        }
    }
    let mut kids: Vec<Child<'_>> = Vec::new();
    for_each_child_public(e, &mut |c| kids.push(c));
    for c in kids {
        match c {
            Child::Expr(x) => names_in(x, out, bound),
            Child::Block(b) => {
                for st in &b.stmts {
                    match &st.kind {
                        StmtKind::Let { pattern, value, .. } => {
                            bound.extend(pattern.binding_names());
                            names_in(value, out, bound);
                        }
                        StmtKind::Expr(x) => names_in(x, out, bound),
                        StmtKind::Assign { target, value }
                        | StmtKind::CompoundAssign { target, value, .. } => {
                            names_in(target, out, bound);
                            names_in(value, out, bound);
                        }
                        _ => {}
                    }
                }
                if let Some(t) = &b.final_expr {
                    names_in(t, out, bound);
                }
            }
        }
    }
}

/// The position of a value that flows on into `pos` unchanged (a tail).
fn tail_of(pos: Pos) -> Pos {
    match pos {
        Pos::Bind => Pos::Other,
        p => p,
    }
}

/// The reason an enum payload escapes: when the enum value does.
fn value_of(pos: Pos) -> Option<&'static str> {
    match pos {
        Pos::Escape(r) => Some(r),
        _ => None,
    }
}

/// Iterator methods whose result holds the closure argument and the
/// receiver, to be run later (docs/library/iterators.md).
pub(crate) const LAZY_ADAPTORS: &[&str] = &[
    "map",
    "filter",
    "filter_map",
    "flat_map",
    "take_while",
    "skip_while",
    "map_while",
    "inspect",
    "scan",
    "step_by",
    "take",
    "skip",
    "enumerate",
    "peekable",
    "rev",
    "chain",
    "zip",
];
