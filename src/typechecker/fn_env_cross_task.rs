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
        Type::Ref(inner) | Type::MutRef(inner) => is_unmarked_fn_type(inner),
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
    pub(super) fn fn_env_cross_task_pass(&mut self, node_types: &FxHashMap<NodeId, (Type, u32)>) {
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
        let mut errors: Vec<(Span, String)> = Vec::new();
        for (_, f) in &fns {
            let mut scope: Vec<(String, Taint)> = Vec::new();
            if f.self_param.is_some() {
                scope.push(("self".to_string(), Taint::default()));
            }
            for p in &f.params {
                // An unmarked function type hides the environment of the
                // argument behind it.
                let taint = Taint {
                    leak: None,
                    opaque: self.type_expr_marks_fn(&p.ty, false),
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
                errors.push((c.span, leak_message(&lead, leak)));
            } else if c.taint.opaque {
                errors.push((
                    c.span,
                    opaque_message(&format!("the function passed to `{}`", c.callee)),
                ));
            }
        }
        errors.sort_by_key(|(s, _)| s.offset);
        errors.dedup_by(|a, b| a.0.offset == b.0.offset && a.1 == b.1);
        for (span, message) in errors {
            self.type_error(message, span, TypeErrorKind::CrossTaskUnsafeCapture);
        }
    }
}

/// A value coerced into a marked function type, or a server handler, that
/// reaches an erased function value whose environment is out of sight.
fn opaque_message(what: &str) -> String {
    format!(
        "error[E_NOT_CROSS_TASK]: {what} must be `CrossTask`, but it holds a function value \
         whose type is not written `+ CrossTask` and whose environment is out of sight (a \
         call's result, a field, an element, or a parameter), so nothing shows it reaches \
         only `CrossTask` values (core semantics §6.3)\n\
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
    errors: &'w mut Vec<(Span, String)>,
    /// The function's return type holds a `+ CrossTask` function type, so
    /// each `return` value is coerced into it.
    ret_marked: bool,
}

impl Walk<'_, '_, '_> {
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
                },
                Err(path) => Taint {
                    leak: Some(Leak {
                        name: name.to_string(),
                        ty: ty.clone(),
                        path,
                        via_fn: false,
                    }),
                    opaque: false,
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
                pattern, value, ty, ..
            } => {
                let t = self.value(value);
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
            StmtKind::LetUninit { name, .. } => self.declare(name.clone(), Taint::default()),
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
                self.value(body);
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
                if marked_receiver {
                    for (t, a) in ts.iter().zip(args) {
                        self.coerced(t, a.value.span, "an element whose type is `+ CrossTask`");
                    }
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
            self.errors.push((span, leak_message(&lead, leak)));
        } else if t.opaque {
            self.errors.push((
                span,
                opaque_message(&format!("a value coerced into {slot}")),
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
        self.errors.push((arg.span, leak_message(&lead, leak)));
    }
}
