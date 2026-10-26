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
//! - a function-typed parameter that reaches a spawn makes that parameter
//!   one that must be `CrossTask`, and each call checks its argument there,
//!   through any chain of functions forwarding it. The server entry points
//!   (`Server.serve*`) run their handlers as connection tasks, so their
//!   function parameters are such parameters too.
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
    /// The enclosing function's function-typed parameters it reaches.
    params: Vec<usize>,
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
        for p in &other.params {
            if !self.params.contains(p) {
                self.params.push(*p);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.leak.is_none() && self.params.is_empty()
    }
}

/// A function value passed to a function-typed parameter.
struct Call {
    caller: String,
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
        let mut required: FxHashSet<(String, usize)> = FxHashSet::default();
        for (key, f) in &fns {
            if SERVER_ENTRIES.contains(&key.as_str()) {
                for (i, p) in f.params.iter().enumerate() {
                    if is_fn_type_expr(&p.ty) {
                        required.insert((key.clone(), i));
                    }
                }
            }
        }
        let mut calls: Vec<Call> = Vec::new();
        let mut errors: Vec<(Span, String)> = Vec::new();
        for (key, f) in &fns {
            let mut scope: Vec<(String, Taint)> = Vec::new();
            if f.self_param.is_some() {
                scope.push(("self".to_string(), Taint::default()));
            }
            for (i, p) in f.params.iter().enumerate() {
                let taint = Taint {
                    leak: None,
                    params: if is_fn_type_expr(&p.ty) {
                        vec![i]
                    } else {
                        Vec::new()
                    },
                };
                for n in p.pattern.binding_names() {
                    scope.push((n, taint.clone()));
                }
            }
            let mut w = Walk {
                tc: self,
                node_types,
                key,
                user: !f.stdlib_origin,
                scopes: vec![scope],
                collectors: Vec::new(),
                calls: &mut calls,
                required: &mut required,
                errors: &mut errors,
            };
            w.block_value(&f.body);
        }
        // A parameter forwarded to a parameter that must be `CrossTask` must
        // be too.
        loop {
            let mut grew = false;
            for c in &calls {
                if required.contains(&(c.callee.clone(), c.index)) {
                    for &p in &c.taint.params {
                        grew |= required.insert((c.caller.clone(), p));
                    }
                }
            }
            if !grew {
                break;
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
            }
        }
        errors.sort_by_key(|(s, _)| s.offset);
        errors.dedup_by(|a, b| a.0.offset == b.0.offset && a.1 == b.1);
        for (span, message) in errors {
            self.type_error(message, span, TypeErrorKind::CrossTaskUnsafeCapture);
        }
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
    key: &'w str,
    user: bool,
    scopes: Vec<Vec<(String, Taint)>>,
    /// Each closure literal being walked: the scope depth it starts at, and
    /// what its captures reach so far.
    collectors: Vec<(usize, Taint)>,
    calls: &'w mut Vec<Call>,
    required: &'w mut FxHashSet<(String, usize)>,
    errors: &'w mut Vec<(Span, String)>,
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
                Err(path) => Taint {
                    leak: Some(Leak {
                        name: name.to_string(),
                        ty: ty.clone(),
                        path,
                        via_fn: false,
                    }),
                    params: Vec::new(),
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
            StmtKind::Let { pattern, value, .. } => {
                let t = self.value(value);
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
                    matches!(t, Type::Named { name, .. } if name == "TaskGroup")
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
                self.args(callee, args);
                Taint::default()
            }
            ExprKind::Call { callee, args } => {
                let key = match &callee.kind {
                    ExprKind::Identifier(n) if self.lookup(n).is_none() => Some(n.clone()),
                    ExprKind::Path { segments, .. } => Some(segments.join(".")),
                    _ => None,
                };
                self.value(callee);
                self.args(key, args);
                Taint::default()
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

    fn args(&mut self, callee: Option<String>, args: &[CallArg]) {
        for (i, a) in args.iter().enumerate() {
            let t = self.value(&a.value);
            if let (Some(callee), false) = (&callee, t.is_empty()) {
                self.calls.push(Call {
                    caller: self.key.to_string(),
                    caller_is_user: self.user,
                    callee: callee.clone(),
                    index: i,
                    taint: t,
                    span: a.value.span,
                });
            }
        }
    }

    /// The function value `arg` is sent to a task: what it reaches must be
    /// `CrossTask`.
    fn spawned(&mut self, t: &Taint, arg: &Expr) {
        for &p in &t.params {
            self.required.insert((self.key.to_string(), p));
        }
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
