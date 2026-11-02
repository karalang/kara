//! v2 core §6.3 through function values (B-2026-10-26-13): a function value
//! is `CrossTask` only when everything its environment reaches is, through
//! any nesting of closures and erased `Fn` / `MutFn` / `OnceFn` values.
//!
//! The boundary check in `cross_task_check.rs` reads a spawned closure
//! literal's captures by their types, and a function type says nothing of
//! its environment, so `let bump: Fn() -> i64 = || s.clone().n;` captured by
//! a `TaskGroup.spawn` closure crossed with its `shared` handle. This pass
//! follows the environments instead:
//!
//! - each function-valued binding made from a closure literal carries what
//!   that closure's captures reach (a capture of another function value
//!   carries that value's environment);
//! - a function value that a spawned closure captures, or that is spawned
//!   itself, must reach nothing that is not `CrossTask`;
//! - an erased function type is `CrossTask` only when written `+ CrossTask`
//!   (B-2026-11-01-26; `cross_task_safe.rs` refuses the unmarked form at
//!   every boundary), so a value coerced into a marked type, a parameter,
//!   a `let`, a return, a field or an element, must reach nothing that is
//!   not `CrossTask`, and must not be an erased function value whose
//!   environment is out of sight: one from a call, a field, an element, or
//!   an unmarked parameter. The server entry points (`Server.serve*`) run
//!   their handlers as connection tasks, so their function parameters are
//!   checked the same way.
//!
//! A plain-AST pass over typed bodies, run after inference.

use super::types::{type_display, Type};
use super::TypeErrorKind;
use crate::ast::*;
use crate::cross_task_safe::{is_cross_task_safe_with, CrossTaskUnsafeFixIt, CrossTaskUnsafePath};
use crate::ids::NodeId;
use crate::index_disjoint::{for_each_child_public, Child};
use crate::resolver::SpanKey;
use crate::token::Span;
use rustc_hash::{FxHashMap, FxHashSet};

/// What a function value's environment reaches.
#[derive(Clone, Debug, Default)]
struct Taint {
    /// The first capture it reaches that is not `CrossTask`: the capture's
    /// name, its type and the path to the leaf.
    leak: Option<Leak>,
    /// It reaches an erased function value whose environment is out of
    /// sight: an unmarked function type from a call, a field, an element, or
    /// a parameter.
    opaque: bool,
    /// Where `+ CrossTask` goes in the type the value was declared with: a
    /// parameter's type, a `let`'s annotation, or the return type of the
    /// function a `let` was bound from.
    fix_at: Option<Span>,
}

#[derive(Clone, Debug)]
struct Leak {
    name: String,
    ty: Type,
    path: CrossTaskUnsafePath,
    /// Reached through a function value, not captured directly: a spawned
    /// closure literal's direct captures are `cross_task_check.rs`'s.
    via_fn: bool,
}

impl Taint {
    fn join(&mut self, other: &Taint) {
        if self.leak.is_none() {
            self.leak = other.leak.clone();
        }
        self.opaque |= other.opaque;
        if self.fix_at.is_none() {
            self.fix_at = other.fix_at;
        }
    }

    fn is_empty(&self) -> bool {
        self.leak.is_none() && !self.opaque
    }
}

/// A function value passed to a function-typed parameter.
struct Call {
    caller_is_user: bool,
    callee: String,
    index: usize,
    taint: Taint,
    span: Span,
}

/// The server entry points, whose function parameters run in connection
/// tasks (one per connection since 581bf59ec2).
const SERVER_ENTRIES: &[&str] = &[
    "Server.serve",
    "Server.serve_tls",
    "Server.serve_ws",
    "Server.serve_ws_tls",
];

fn is_fn_type_expr(t: &TypeExpr) -> bool {
    match &t.kind {
        TypeKind::FnType { .. } => true,
        TypeKind::Ref(inner) | TypeKind::MutRef(inner) => is_fn_type_expr(inner),
        _ => false,
    }
}

fn is_fn_type(t: &Type) -> bool {
    match t {
        Type::Function { .. } | Type::OnceFunction { .. } => true,
        Type::Ref(inner) | Type::MutRef(inner) => is_fn_type(inner),
        _ => false,
    }
}

/// An erased function type not written `+ CrossTask`.
fn is_unmarked_fn_type(t: &Type) -> bool {
    match t {
        Type::Function { cross_task, .. } | Type::OnceFunction { cross_task, .. } => !cross_task.0,
        // B-2026-11-02-14: an element or payload too, so a `Vec[Fn()]` a
        // call hands back is as opaque as the `Fn()` itself.
        Type::Named { args, .. } => args.iter().any(is_unmarked_fn_type),
        Type::Tuple(ts) => ts.iter().any(is_unmarked_fn_type),
        Type::Array { element, .. } | Type::Slice { element, .. } => is_unmarked_fn_type(element),
        Type::Ref(i) | Type::MutRef(i) | Type::Arc(i) | Type::Rc(i) | Type::Weak(i) => {
            is_unmarked_fn_type(i)
        }
        _ => false,
    }
}

/// Whether `t` holds a function type written `+ CrossTask`, so a value
/// coerced into it is checked.
fn has_marked_fn_type(t: &Type) -> bool {
    match t {
        Type::Function { cross_task, .. } | Type::OnceFunction { cross_task, .. } => cross_task.0,
        Type::Named { args, .. } => args.iter().any(has_marked_fn_type),
        Type::Tuple(ts) => ts.iter().any(has_marked_fn_type),
        Type::Array { element, .. } | Type::Slice { element, .. } => has_marked_fn_type(element),
        Type::Ref(i) | Type::MutRef(i) | Type::Arc(i) | Type::Rc(i) | Type::Weak(i) => {
            has_marked_fn_type(i)
        }
        _ => false,
    }
}

impl<'a> super::TypeChecker<'a> {
    pub(super) fn fn_env_cross_task_pass(
        &mut self,
        node_types: &FxHashMap<NodeId, (Type, u32)>,
        call_subs: &FxHashMap<NodeId, FxHashMap<String, Type>>,
    ) {
        let mut fns: Vec<(String, &Function)> = Vec::new();
        for item in &self.program.items {
            match item {
                Item::Function(f) => fns.push((f.name.clone(), f)),
                Item::ImplBlock(imp) => {
                    let head = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned(),
                        _ => None,
                    };
                    for it in &imp.items {
                        if let (ImplItem::Method(m), Some(h)) = (it, &head) {
                            fns.push((format!("{h}.{}", m.name), m.as_ref()));
                        }
                    }
                }
                _ => {}
            }
        }
        // The parameters an argument is coerced into a marked function type
        // through, and the server entries' handlers.
        let mut required: FxHashSet<(String, usize)> = FxHashSet::default();
        for (key, f) in &fns {
            let server = SERVER_ENTRIES.contains(&key.as_str());
            for (i, p) in f.params.iter().enumerate() {
                if (server && is_fn_type_expr(&p.ty)) || self.type_expr_marks_fn(&p.ty, true) {
                    required.insert((key.clone(), i));
                }
            }
        }
        let mut calls: Vec<Call> = Vec::new();
        let mut errors: Vec<(Span, String, Option<Span>)> = Vec::new();
        let fn_items: FxHashMap<String, &Function> =
            fns.iter().map(|(k, f)| (k.clone(), *f)).collect();
        for (_, f) in &fns {
            let mut scope: Vec<(String, Taint)> = Vec::new();
            if f.self_param.is_some() {
                scope.push(("self".to_string(), Taint::default()));
            }
            for p in &f.params {
                // An unmarked function type hides the environment of the
                // argument behind it.
                let opaque = self.type_expr_marks_fn(&p.ty, false);
                let taint = Taint {
                    leak: None,
                    opaque,
                    fix_at: if opaque {
                        self.fn_marker_point(&p.ty)
                    } else {
                        None
                    },
                };
                for n in p.pattern.binding_names() {
                    scope.push((n, taint.clone()));
                }
            }
            let ret_marked = f
                .return_type
                .as_ref()
                .is_some_and(|t| self.type_expr_marks_fn(t, true));
            let mut w = Walk {
                tc: self,
                node_types,
                user: !f.stdlib_origin,
                scopes: vec![scope],
                collectors: Vec::new(),
                calls: &mut calls,
                errors: &mut errors,
                ret_marked,
                fn_items: &fn_items,
                call_subs,
                closure_results: FxHashMap::default(),
            };
            let tail = w.block_value(&f.body);
            if ret_marked {
                let at = f.body.final_expr.as_ref().map_or(f.body.span, |e| e.span);
                w.coerced(&tail, at, "the function's `+ CrossTask` return type");
            }
        }
        for c in &calls {
            if !c.caller_is_user || !required.contains(&(c.callee.clone(), c.index)) {
                continue;
            }
            if let Some(leak) = &c.taint.leak {
                let lead = format!(
                    "the function passed to `{}` runs in another task, and its environment \
                     captures `{}` (type `{}`), which cannot cross a task boundary \
                     (core semantics §6.3)",
                    c.callee,
                    leak.name,
                    type_display(&leak.ty)
                );
                errors.push((c.span, leak_message(&lead, leak), None));
            } else if c.taint.opaque {
                errors.push((
                    c.span,
                    opaque_message(&format!("the function passed to `{}`", c.callee)),
                    c.taint.fix_at,
                ));
            }
        }
        errors.sort_by_key(|(s, _, _)| s.offset);
        errors.dedup_by(|a, b| a.0.offset == b.0.offset && a.1 == b.1);
        for (span, message, fix_at) in errors {
            match fix_at {
                // B-2026-11-02-13: the same insertion E0200 offers for a `dyn`.
                Some(at) => self.type_error_with_fix_it(
                    message,
                    span,
                    TypeErrorKind::CrossTaskUnsafeCapture,
                    super::FixIt {
                        span: at,
                        replacement: " + CrossTask".to_string(),
                    },
                ),
                None => self.type_error(message, span, TypeErrorKind::CrossTaskUnsafeCapture),
            }
        }
    }
}

/// A value coerced into a marked function type, or a server handler, that
/// reaches an erased function value whose environment is out of sight.
fn opaque_message(what: &str) -> String {
    format!(
        "error[E_NOT_CROSS_TASK]: {what} must be `CrossTask`, but it holds a function value \
         whose type is not written `+ CrossTask` and whose environment is out of sight (a \
         call's result, a field, an element, a parameter, or a `let mut` that takes function \
         values), so nothing shows it reaches only `CrossTask` values (core semantics §6.3)\n\
         help: write `+ CrossTask` on the function type the value is declared with, so that \
         each value coerced into it is checked"
    )
}

impl super::TypeChecker<'_> {
    /// Whether `t` names a function type written `+ CrossTask` (`marked`),
    /// or one not written so (`!marked`).
    fn type_expr_marks_fn(&self, t: &TypeExpr, marked: bool) -> bool {
        let mut found = false;
        crate::span_visitor::for_each_type_expr(t, &mut |x| {
            if matches!(x.kind, TypeKind::FnType { .. }) {
                let m =
                    crate::prelude::marks_cross_task_fn(self.program, &SpanKey::from_span(&x.span));
                found |= m == marked;
            }
        });
        found
    }
}

/// Whether `t`, with `subs` for its type parameters, holds a `+ CrossTask`
/// function type where a value sits: not inside a function type's
/// parameters or return, whose value is the function.
fn solved_value_marked(t: &TypeExpr, subs: &FxHashMap<String, Type>) -> bool {
    match &t.kind {
        TypeKind::Path(p) => {
            let own = match p.segments.as_slice() {
                [name] => subs.get(name).is_some_and(has_marked_fn_type),
                _ => false,
            };
            own || p.generic_args.iter().flatten().any(|a| match a {
                crate::ast::GenericArg::Type(t) => solved_value_marked(t, subs),
                _ => false,
            })
        }
        TypeKind::Tuple(ts) => ts.iter().any(|t| solved_value_marked(t, subs)),
        TypeKind::Array { element: t, .. }
        | TypeKind::Ref(t)
        | TypeKind::MutRef(t)
        | TypeKind::Frozen(t)
        | TypeKind::MutSlice(t)
        | TypeKind::Weak(t) => solved_value_marked(t, subs),
        _ => false,
    }
}

/// The return type of the function type `t` (under a `ref`), if it is one.
fn fn_return(t: &TypeExpr) -> Option<&TypeExpr> {
    match &t.kind {
        TypeKind::FnType { return_type, .. } => return_type.as_deref(),
        TypeKind::Ref(i) | TypeKind::MutRef(i) => fn_return(i),
        _ => None,
    }
}

fn leak_message(lead: &str, leak: &Leak) -> String {
    let path_suffix = if leak.path.path.is_empty() {
        String::new()
    } else {
        format!(" at {}", leak.path.path.join(" -> "))
    };
    let mut help = leak.path.fix_it.help_text(&leak.path.unsafe_leaf);
    if leak.path.fix_it == CrossTaskUnsafeFixIt::SharedToPar {
        help.push_str(", with `Mutex[T]` or `Atomic[T]` for its `mut` fields");
    }
    format!(
        "error[E_NOT_CROSS_TASK]: {lead}\n\
         note: type `{ty}` reaches `{leaf}`{path_suffix}\n\
         help: {help}",
        ty = type_display(&leak.ty),
        leaf = leak.path.unsafe_leaf,
    )
}

struct Walk<'w, 't, 'a> {
    tc: &'t super::TypeChecker<'a>,
    node_types: &'w FxHashMap<NodeId, (Type, u32)>,
    user: bool,
    scopes: Vec<Vec<(String, Taint)>>,
    /// Each closure literal being walked: the scope depth it starts at, and
    /// what its captures reach so far.
    collectors: Vec<(usize, Taint)>,
    calls: &'w mut Vec<Call>,
    errors: &'w mut Vec<(Span, String, Option<Span>)>,
    /// The function's return type holds a `+ CrossTask` function type, so
    /// each `return` value is coerced into it.
    ret_marked: bool,
    /// The program's functions and methods by name (`Head.method`).
    fn_items: &'w FxHashMap<String, &'w Function>,
    /// Each generic call's solved type arguments, by the call's node.
    call_subs: &'w FxHashMap<NodeId, FxHashMap<String, Type>>,
    /// What each closure literal's result reaches, and where its result is.
    closure_results: FxHashMap<NodeId, (Taint, Span)>,
}

impl<'w> Walk<'w, '_, '_> {
    fn lookup(&self, name: &str) -> Option<(usize, &Taint)> {
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(d, s)| s.iter().rev().find(|(n, _)| n == name).map(|(_, t)| (d, t)))
    }

    fn declare(&mut self, name: String, taint: Taint) {
        if let Some(s) = self.scopes.last_mut() {
            s.push((name, taint));
        }
    }

    fn declare_pattern(&mut self, p: &Pattern) {
        for n in p.binding_names() {
            self.declare(n, Taint::default());
        }
    }

    /// A use of `name`: a capture of each closure literal it was declared
    /// outside of.
    fn use_binding(&mut self, name: &str, e: &Expr) {
        let Some((depth, taint)) = self.lookup(name) else {
            return;
        };
        let taint = taint.clone();
        let Some((ty, _)) = self.node_types.get(&e.id) else {
            return;
        };
        let reach = if is_fn_type(ty) {
            let mut t = taint;
            if let Some(l) = t.leak.as_mut() {
                l.via_fn = true;
            }
            t
        } else {
            match is_cross_task_safe_with(ty, &self.tc.env.structs, &self.tc.env.enums) {
                Ok(()) => return,
                // An erased function type the capture holds hides the
                // environment behind it.
                Err(path) if path.fix_it == CrossTaskUnsafeFixIt::FnCrossTask => Taint {
                    leak: None,
                    opaque: true,
                    fix_at: None,
                },
                Err(path) => Taint {
                    leak: Some(Leak {
                        name: name.to_string(),
                        ty: ty.clone(),
                        path,
                        via_fn: false,
                    }),
                    opaque: false,
                    fix_at: None,
                },
            }
        };
        for (base, t) in self.collectors.iter_mut() {
            if *base > depth {
                t.join(&reach);
            }
        }
    }

    fn block_value(&mut self, b: &Block) -> Taint {
        self.scopes.push(Vec::new());
        for s in &b.stmts {
            self.stmt(s);
        }
        let t = match &b.final_expr {
            Some(e) => self.value(e),
            None => Taint::default(),
        };
        self.scopes.pop();
        t
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let {
                is_mut,
                pattern,
                value,
                ty,
            } => {
                let mut t = self.value(value);
                // B-2026-11-02-14: a `let mut` collection, tuple or payload
                // of unmarked function values takes elements nothing here
                // follows (`hs.push(f)`, `hs[0] = f`), so what it holds is
                // out of sight, however it started.
                if *is_mut
                    && self
                        .node_types
                        .get(&value.id)
                        .is_some_and(|(vt, _)| !is_fn_type(vt) && is_unmarked_fn_type(vt))
                {
                    t.opaque = true;
                }
                if let Some(at) = self.declared_marker_point(ty.as_ref(), value) {
                    t.fix_at = Some(at);
                }
                if ty
                    .as_ref()
                    .is_some_and(|ty| self.tc.type_expr_marks_fn(ty, true))
                {
                    self.coerced(&t, value.span, "a `let` whose type is `+ CrossTask`");
                }
                if let PatternKind::Binding(name) = &pattern.kind {
                    self.declare(name.clone(), t);
                } else {
                    self.declare_pattern(pattern);
                }
            }
            StmtKind::LetUninit {
                is_mut, name, ty, ..
            } => {
                let opaque = *is_mut
                    && !matches!(ty.kind, TypeKind::FnType { .. })
                    && self.tc.type_expr_marks_fn(ty, false);
                let t = Taint {
                    leak: None,
                    opaque,
                    fix_at: self.tc.fn_marker_point(ty),
                };
                self.declare(name.clone(), t);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.value(value);
                self.block_value(else_block);
                self.declare_pattern(pattern);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.block_value(body);
            }
            StmtKind::Assign { target, value } => {
                let t = self.value(value);
                self.value(target);
                if self
                    .node_types
                    .get(&target.id)
                    .is_some_and(|(ty, _)| has_marked_fn_type(ty))
                {
                    self.coerced(&t, value.span, "a place whose type is `+ CrossTask`");
                }
                // A rebound function value reaches what either value does.
                if let ExprKind::Identifier(n) = &target.kind {
                    for s in self.scopes.iter_mut().rev() {
                        if let Some((_, old)) = s.iter_mut().rev().find(|(m, _)| m == n) {
                            old.join(&t);
                            break;
                        }
                    }
                }
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.value(value);
                self.value(target);
            }
            StmtKind::MultiAssign { targets, values } => {
                for e in values.iter().chain(targets) {
                    self.value(e);
                }
            }
            StmtKind::Expr(e) => {
                self.value(e);
            }
        }
    }

    /// Walk `e`, returning what the function value it evaluates to reaches.
    fn value(&mut self, e: &Expr) -> Taint {
        match &e.kind {
            ExprKind::Identifier(n) => {
                self.use_binding(n, e);
                self.lookup(n).map(|(_, t)| t.clone()).unwrap_or_default()
            }
            ExprKind::SelfValue => {
                self.use_binding("self", e);
                Taint::default()
            }
            ExprKind::Closure { params, body, .. } => {
                self.collectors.push((self.scopes.len(), Taint::default()));
                self.scopes.push(Vec::new());
                for p in params {
                    self.declare_pattern(&p.pattern);
                }
                // B-2026-11-02-18: a closure whose return type holds a `+
                // CrossTask` function type coerces its result into it, as a
                // function's return does.
                let ret_marked = self.node_types.get(&e.id).is_some_and(|(t, _)| match t {
                    Type::Function { return_type, .. } | Type::OnceFunction { return_type, .. } => {
                        has_marked_fn_type(return_type)
                    }
                    _ => false,
                });
                let outer = std::mem::replace(&mut self.ret_marked, ret_marked);
                let result = self.value(body);
                self.ret_marked = outer;
                let at = match &body.kind {
                    ExprKind::Block(b) => b.final_expr.as_ref().map_or(body.span, |x| x.span),
                    _ => body.span,
                };
                if ret_marked {
                    self.coerced(&result, at, "a closure's `+ CrossTask` return type");
                }
                self.closure_results.insert(e.id, (result, at));
                self.scopes.pop();
                self.collectors.pop().map(|(_, t)| t).unwrap_or_default()
            }
            ExprKind::Block(b) => self.block_value(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.value(condition);
                let mut t = self.block_value(then_block);
                if let Some(eb) = else_branch {
                    t.join(&self.value(eb));
                }
                t
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.value(value);
                self.scopes.push(Vec::new());
                self.declare_pattern(pattern);
                let mut t = self.block_value(then_block);
                self.scopes.pop();
                if let Some(eb) = else_branch {
                    t.join(&self.value(eb));
                }
                t
            }
            ExprKind::Match { scrutinee, arms } => {
                self.value(scrutinee);
                let mut t = Taint::default();
                for arm in arms {
                    self.scopes.push(Vec::new());
                    self.declare_pattern(&arm.pattern);
                    if let Some(g) = &arm.guard {
                        self.value(g);
                    }
                    t.join(&self.value(&arm.body));
                    self.scopes.pop();
                }
                t
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.value(object);
                let group = self.node_types.get(&object.id).is_some_and(|(t, _)| {
                    let mut t = t;
                    while let Type::Ref(i) | Type::MutRef(i) = t {
                        t = i;
                    }
                    matches!(t, Type::Named { name, .. } if name == "TaskGroup" || name == "IsolatingGroup")
                });
                if method == "spawn" && group {
                    if let Some(a) = args.first() {
                        let t = self.value(&a.value);
                        self.spawned(&t, &a.value);
                    }
                    for a in args.iter().skip(1) {
                        self.value(&a.value);
                    }
                    return Taint::default();
                }
                let callee = self
                    .tc
                    .method_callee_types
                    .get(&SpanKey::from_span(&e.span))
                    .cloned();
                // A collection or channel of `+ CrossTask` function values
                // takes each one through a generic parameter: `v.push(f)`,
                // `tx.send(f)`.
                let marked_receiver = self
                    .node_types
                    .get(&object.id)
                    .is_some_and(|(t, _)| has_marked_fn_type(t));
                let ts = self.args(callee, args);
                // B-2026-11-02-18: each argument into its parameter's type as
                // solved (`o.unwrap_or_else(|| ..)`'s result into `T`).
                if let Some(f) = self
                    .tc
                    .node_method_callees
                    .get(&e.id)
                    .and_then(|k| self.fn_items.get(k).copied())
                {
                    self.coerce_into_params(e, f, args, &ts);
                }
                if marked_receiver {
                    for (t, a) in ts.iter().zip(args) {
                        self.coerced(t, a.value.span, "an element whose type is `+ CrossTask`");
                    }
                }
                // B-2026-11-02-15: `collect()` into a collection of `+
                // CrossTask` function values takes each item as a push would,
                // so an item whose type is not marked is out of sight.
                if method == "collect"
                    && args.is_empty()
                    && self
                        .node_types
                        .get(&e.id)
                        .is_some_and(|(t, _)| has_marked_fn_type(t))
                    && self
                        .tc
                        .library_iter_item(object)
                        .is_some_and(|item| self.unmarked_deep(&item))
                {
                    return Taint {
                        leak: None,
                        opaque: true,
                        fix_at: None,
                    };
                }
                self.opaque_if_fn(e)
            }
            ExprKind::Call { callee, args } => {
                let key = match &callee.kind {
                    ExprKind::Identifier(n) if self.lookup(n).is_none() => Some(n.clone()),
                    ExprKind::Path { segments, .. } => Some(segments.join(".")),
                    _ => None,
                };
                self.value(callee);
                let ts = self.args(key.clone(), args);
                // A variant's payload is the value: `Some(f)`, `Ok(f)`.
                let constructor = key.as_deref().is_some_and(|k| {
                    k.rsplit('.')
                        .next()
                        .is_some_and(|last| last.starts_with(|c: char| c.is_ascii_uppercase()))
                });
                if constructor {
                    let mut t = Taint::default();
                    for x in &ts {
                        t.join(x);
                    }
                    return t;
                }
                // B-2026-11-02-15 / -18: a type parameter solved as a `+
                // CrossTask` type from where the result goes does not make the
                // argument one. Each argument is coerced into its parameter's
                // type as solved; what the arguments reach is what a result
                // of a solved type parameter reaches; and what the callee
                // writes through a `mut` argument of a marked type is coerced
                // into it.
                let callee_fn = key.as_deref().and_then(|k| self.callee_fn(k));
                if let Some(f) = callee_fn {
                    self.coerce_into_params(e, f, args, &ts);
                }
                let generic = callee_fn.filter(|f| {
                    f.generic_params
                        .as_ref()
                        .is_some_and(|g| !g.params.is_empty())
                        || self.call_subs.contains_key(&e.id)
                });
                // A callee with no body here (`mem.replace`, a builtin) may
                // write any argument through a `mut ref`, and return any.
                let unseen =
                    callee_fn.is_none() && key.as_deref().is_some_and(|k| self.lookup(k).is_none());
                if generic.is_some() || unseen {
                    self.coerce_into_marked_places(args, &ts);
                }
                let result_marked = self
                    .node_types
                    .get(&e.id)
                    .is_some_and(|(ty, _)| has_marked_fn_type(ty));
                if result_marked && (unseen || generic.is_some_and(|f| self.returns_solved(e, f))) {
                    let mut t = self.opaque_if_fn(e);
                    for x in &ts {
                        t.join(x);
                    }
                    return t;
                }
                self.opaque_if_fn(e)
            }
            ExprKind::FieldAccess { .. } | ExprKind::Index { .. } | ExprKind::TupleIndex { .. } => {
                self.children(e);
                self.opaque_if_fn(e)
            }
            ExprKind::Return(Some(v)) => {
                let t = self.value(v);
                if self.ret_marked {
                    self.coerced(&t, v.span, "the function's `+ CrossTask` return type");
                }
                Taint::default()
            }
            ExprKind::Tuple(xs)
            | ExprKind::ArrayLiteral(xs)
            | ExprKind::PrefixCollectionLiteral { items: xs, .. } => {
                let mut t = Taint::default();
                for x in xs {
                    t.join(&self.value(x));
                }
                t
            }
            ExprKind::StructLiteral {
                path,
                fields,
                spread,
                ..
            } => {
                let info = path.last().and_then(|n| self.tc.env.structs.get(n));
                let marked: Vec<bool> = fields
                    .iter()
                    .map(|fi| {
                        info.and_then(|i| i.fields.iter().find(|(n, _, _)| *n == fi.name))
                            .is_some_and(|(_, ty, _)| has_marked_fn_type(ty))
                    })
                    .collect();
                let mut t = Taint::default();
                for (fi, m) in fields.iter().zip(marked) {
                    let ft = self.value(&fi.value);
                    if m {
                        self.coerced(&ft, fi.value.span, "a field whose type is `+ CrossTask`");
                    }
                    t.join(&ft);
                }
                if let Some(sp) = spread {
                    t.join(&self.value(sp));
                }
                t
            }
            ExprKind::For { pattern, .. } | ExprKind::WhileLet { pattern, .. } => {
                // The loop's own binding shadows inside its body.
                self.scopes.push(Vec::new());
                self.declare_pattern(pattern);
                self.children(e);
                self.scopes.pop();
                Taint::default()
            }
            _ => {
                self.children(e);
                Taint::default()
            }
        }
    }

    /// The function a call names: `id`, `Bx.new`, or a library function by
    /// its last segment.
    fn callee_fn(&self, key: &str) -> Option<&'w Function> {
        self.fn_items
            .get(key)
            .or_else(|| {
                key.rsplit_once('.')
                    .and_then(|(_, last)| self.fn_items.get(last))
                    .filter(|f| {
                        f.stdlib_origin
                            || f.span.offset >= self.tc.program.library_start.unwrap_or(usize::MAX)
                    })
            })
            .copied()
    }

    /// Whether `f`'s return type names a type parameter the call `e`
    /// solved, so the result's type is what inference solved it as.
    fn returns_solved(&self, e: &Expr, f: &Function) -> bool {
        let Some(ret) = &f.return_type else {
            return false;
        };
        let own = f.generic_params.as_ref();
        let subs = self.call_subs.get(&e.id);
        let mut found = false;
        crate::span_visitor::for_each_type_expr(ret, &mut |x| {
            if let TypeKind::Path(p) = &x.kind {
                if let [name] = p.segments.as_slice() {
                    found |= own.is_some_and(|g| g.params.iter().any(|gp| &gp.name == name))
                        || subs.is_some_and(|s| s.contains_key(name));
                }
            }
        });
        found
    }

    /// B-2026-11-02-18: each argument of the call `e` to `f` coerced into
    /// its parameter's type as the call solved it. A parameter whose solved
    /// type holds a `+ CrossTask` function type where a value sits (`x: T`
    /// with `T = Fn() + CrossTask`, `v: Vec[T]`) takes the argument as a
    /// coercion; a function parameter whose solved return type does
    /// (`f: OnceFn() -> T`) takes a closure literal's result as one.
    fn coerce_into_params(&mut self, e: &Expr, f: &Function, args: &[CallArg], ts: &[Taint]) {
        let Some(subs) = self.call_subs.get(&e.id) else {
            return;
        };
        for (i, a) in args.iter().enumerate() {
            let p = match &a.label {
                Some(l) => f
                    .params
                    .iter()
                    .find(|p| matches!(&p.pattern.kind, PatternKind::Binding(n) if n == l)),
                None => f.params.get(i),
            };
            let Some(p) = p else {
                continue;
            };
            if solved_value_marked(&p.ty, subs) {
                self.coerced(
                    &ts[i],
                    a.value.span,
                    "a parameter whose type is `+ CrossTask` as solved",
                );
            }
            if let (Some(ret), ExprKind::Closure { .. }) = (fn_return(&p.ty), &a.value.kind) {
                if solved_value_marked(ret, subs) {
                    if let Some((t, at)) = self.closure_results.get(&a.value.id).cloned() {
                        self.coerced(&t, at, "a closure's `+ CrossTask` return type");
                    }
                }
            }
        }
    }

    /// An argument written `mut` whose type holds a `+ CrossTask` function
    /// type is a place the generic callee may write the other arguments
    /// into (`mem.replace(mut f, g)`, `mem.swap(mut f, mut g)`), so each is
    /// coerced into it.
    fn coerce_into_marked_places(&mut self, args: &[CallArg], ts: &[Taint]) {
        let places: Vec<usize> = (0..args.len())
            .filter(|&i| {
                args[i].mut_marker
                    && self
                        .node_types
                        .get(&args[i].value.id)
                        .is_some_and(|(t, _)| has_marked_fn_type(t))
            })
            .collect();
        for (j, (a, t)) in args.iter().zip(ts).enumerate() {
            // `mem.swap(mut f, mut g)` writes each into the other.
            if places.iter().any(|&i| i != j) {
                self.coerced(t, a.value.span, "a place whose type is `+ CrossTask`");
            }
        }
    }

    /// [`is_unmarked_fn_type`] through the type variables inference solved.
    fn unmarked_deep(&self, t: &Type) -> bool {
        let t = super::inference::resolve_type_var_top(t, &self.tc.env.substitutions);
        match &t {
            Type::Function { cross_task, .. } | Type::OnceFunction { cross_task, .. } => {
                !cross_task.0
            }
            Type::Named { args, .. } => args.iter().any(|a| self.unmarked_deep(a)),
            Type::Tuple(ts) => ts.iter().any(|a| self.unmarked_deep(a)),
            Type::Array { element, .. } | Type::Slice { element, .. } => {
                self.unmarked_deep(element)
            }
            Type::Ref(i) | Type::MutRef(i) | Type::Arc(i) | Type::Rc(i) | Type::Weak(i) => {
                self.unmarked_deep(i)
            }
            _ => false,
        }
    }

    /// Where `+ CrossTask` goes in the type a `let` binds: its annotation,
    /// or the return type of the function its value is a call of.
    fn declared_marker_point(&self, ty: Option<&TypeExpr>, value: &Expr) -> Option<Span> {
        if let Some(t) = ty {
            return self.tc.fn_marker_point(t);
        }
        let ExprKind::Call { callee, .. } = &value.kind else {
            return None;
        };
        let ExprKind::Identifier(n) = &callee.kind else {
            return None;
        };
        self.tc.program.items.iter().find_map(|it| match it {
            Item::Function(g) if &g.name == n => g
                .return_type
                .as_ref()
                .and_then(|r| self.tc.fn_marker_point(r)),
            _ => None,
        })
    }

    fn children(&mut self, e: &Expr) {
        let mut kids: Vec<Child<'_>> = Vec::new();
        for_each_child_public(e, &mut |c| kids.push(c));
        for c in kids {
            match c {
                Child::Expr(x) => {
                    self.value(x);
                }
                Child::Block(b) => {
                    self.block_value(b);
                }
            }
        }
    }

    fn args(&mut self, callee: Option<String>, args: &[CallArg]) -> Vec<Taint> {
        let mut out = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let t = self.value(&a.value);
            if let (Some(callee), false) = (&callee, t.is_empty()) {
                self.calls.push(Call {
                    caller_is_user: self.user,
                    callee: callee.clone(),
                    index: i,
                    taint: t.clone(),
                    span: a.value.span,
                });
            }
            out.push(t);
        }
        out
    }

    /// An erased function value whose type is not marked hides its
    /// environment: a call's result, a field, an element.
    fn opaque_if_fn(&self, e: &Expr) -> Taint {
        Taint {
            leak: None,
            opaque: self
                .node_types
                .get(&e.id)
                .is_some_and(|(t, _)| is_unmarked_fn_type(t)),
            fix_at: None,
        }
    }

    /// The value `t` describes is coerced into a type holding a `+
    /// CrossTask` function type at `span`: what it reaches must be
    /// `CrossTask`, and an environment out of sight is not known to be.
    fn coerced(&mut self, t: &Taint, span: Span, slot: &str) {
        if !self.user {
            return;
        }
        if let Some(leak) = &t.leak {
            let lead = format!(
                "a function value coerced into {slot} must be `CrossTask`, and its \
                 environment captures `{}` (type `{}`), which is not (core semantics §6.3)",
                leak.name,
                type_display(&leak.ty)
            );
            self.errors.push((span, leak_message(&lead, leak), None));
        } else if t.opaque {
            self.errors.push((
                span,
                opaque_message(&format!("a value coerced into {slot}")),
                t.fix_at,
            ));
        }
    }

    /// The function value `arg` is sent to a task: what it reaches must be
    /// `CrossTask`.
    fn spawned(&mut self, t: &Taint, arg: &Expr) {
        let Some(leak) = &t.leak else {
            return;
        };
        let literal = matches!(arg.kind, ExprKind::Closure { .. });
        if !self.user || (literal && !leak.via_fn) {
            return;
        }
        let what = match &arg.kind {
            ExprKind::Identifier(n) => format!("`{n}`"),
            _ => "the spawned closure".to_string(),
        };
        let lead = format!(
            "{what} crosses a TaskGroup.spawn task boundary, and the environment of a \
             function value it holds captures `{}` (type `{}`), which cannot cross a task \
             boundary (core semantics §6.3)",
            leak.name,
            type_display(&leak.ty)
        );
        self.errors
            .push((arg.span, leak_message(&lead, leak), None));
    }
}
