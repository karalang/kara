//! v2 core: a shared `ref` place is read-only (`docs/core-semantics.md`
//! §5.6). Writing through one — assigning a place reached through it,
//! calling a `mut ref self` method on it, or passing it as a `mut`
//! argument — is an error. A write through a `mut ref`, or through a handle
//! (`shared` / `par` value, `Rc`, `Arc`) whose `mut` fields carry their own
//! borrow flags (§6.2), is not.
//!
//! A pass over each function body, run by the strict commands only. The
//! place's type at each step of its projection chain comes from the typed
//! node table, so a binding that is a `ref` because of its declaration, its
//! parameter mode, a bare `for` (§4.6) or a `ref` pattern is all one case.

use super::*;
use crate::index_disjoint::{for_each_child_public, Child};

/// Builtin collection methods that write their receiver.
const MUTATING_BUILTINS: &[&str] = &[
    "push",
    "pop",
    "insert",
    "remove",
    "clear",
    "truncate",
    "sort",
    "sort_by",
    "sort_by_key",
    "reverse",
    "swap",
    "extend",
    "append",
    "retain",
    "drain",
    "dedup",
    "push_back",
    "push_front",
    "pop_back",
    "pop_front",
    "resize",
    "fill",
    "push_str",
    "set",
    "get_mut",
    "iter_mut",
    "entry",
];

struct Walk<'t, 'a> {
    tc: &'t TypeChecker<'a>,
    node_types: &'t FxHashMap<crate::ids::NodeId, (Type, u32)>,
    self_modes: &'t FxHashMap<String, SelfParam>,
    /// Bindings in scope, innermost last, with where each came from. A
    /// shadowing binding is pushed as `Owned`.
    scopes: Vec<Vec<(String, Origin)>>,
    /// The function's receiver is `ref self`.
    self_read_only: bool,
    errors: Vec<(Span, String)>,
}

/// Why a binding is a read-only borrow, for the error's fix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// Not a borrow its declaration makes read-only (its type may still be).
    Owned,
    /// A `ref T` parameter.
    Param,
    /// The item of a bare `for` over a collection (§4.6).
    ForElem,
    /// A `ref name` pattern binding.
    RefPattern,
}

/// What a step of a place's projection chain says about writing through it.
enum Step {
    /// A shared reference: the write goes through a read-only borrow.
    ReadOnly,
    /// A `mut ref`, or a handle with its own borrow flags: writable.
    Writable,
    /// Owned so far: look at the place this one projects from.
    Continue,
}

impl<'a> TypeChecker<'a> {
    pub(super) fn emit_core_ref_write_errors(
        &mut self,
        node_types: &FxHashMap<crate::ids::NodeId, (Type, u32)>,
    ) {
        let mut self_modes: FxHashMap<String, SelfParam> = FxHashMap::default();
        let mut fns: Vec<&Function> = Vec::new();
        for item in &self.program.items {
            match item {
                Item::Function(f) => fns.push(f),
                Item::ImplBlock(imp) => {
                    let head = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned(),
                        _ => None,
                    };
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            if let (Some(h), Some(sp)) = (&head, &m.self_param) {
                                self_modes.insert(format!("{h}.{}", m.name), sp.clone());
                            }
                            fns.push(m.as_ref());
                        }
                    }
                }
                _ => {}
            }
        }
        let mut w = Walk {
            tc: self,
            node_types,
            self_modes: &self_modes,
            scopes: Vec::new(),
            self_read_only: false,
            errors: Vec::new(),
        };
        for f in fns.into_iter().filter(|f| !f.stdlib_origin) {
            w.self_read_only = matches!(f.self_param, Some(SelfParam::Ref));
            w.scopes = vec![f
                .params
                .iter()
                .flat_map(|p| {
                    let origin = if matches!(p.ty.kind, TypeKind::Ref(_)) {
                        Origin::Param
                    } else {
                        Origin::Owned
                    };
                    p.pattern
                        .binding_names()
                        .into_iter()
                        .map(move |n| (n, origin))
                })
                .collect()];
            w.block(&f.body);
        }
        let mut errors = w.errors;
        errors.sort_by_key(|(s, _)| s.offset);
        errors.dedup_by(|a, b| a.0.offset == b.0.offset);
        for (span, message) in errors {
            self.type_error(message, span, TypeErrorKind::TypeMismatch);
        }
    }
}

impl Walk<'_, '_> {
    fn block(&mut self, b: &Block) {
        self.scopes.push(Vec::new());
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(e) = &b.final_expr {
            self.expr(e);
        }
        self.scopes.pop();
    }

    /// Bring `pattern`'s bindings into the innermost scope with `origin`;
    /// a `ref name` binding is read-only whatever `origin` says.
    fn bind(&mut self, pattern: &Pattern, origin: Origin) {
        let ref_spans = &self.tc.program.ref_binding_spans;
        let names: Vec<(String, Origin)> = pattern
            .binding_name_spans()
            .into_iter()
            .map(|(n, sp)| {
                if ref_spans.contains(&SpanKey::from_span(&sp)) {
                    (n, Origin::RefPattern)
                } else {
                    (n, origin)
                }
            })
            .collect();
        if let Some(scope) = self.scopes.last_mut() {
            scope.extend(names);
        }
    }

    fn origin_of(&self, name: &str) -> Origin {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.iter().rev().find(|(n, _)| n == name).map(|(_, o)| *o))
            .unwrap_or(Origin::Owned)
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let { pattern, value, .. } => {
                self.expr(value);
                // A local bound to a read-only borrow is one too (§5.9).
                let origin = match &value.kind {
                    ExprKind::Identifier(n) => self.origin_of(n),
                    _ => Origin::Owned,
                };
                self.bind(pattern, origin);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.expr(value);
                self.block(else_block);
                self.bind(pattern, Origin::Owned);
            }
            StmtKind::LetUninit { name, .. } => {
                if let Some(scope) = self.scopes.last_mut() {
                    scope.push((name.clone(), Origin::Owned));
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.block(body),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.expr(value);
                self.expr(target);
                self.check_assign(target);
            }
            StmtKind::MultiAssign { targets, values } => {
                for e in values.iter().chain(targets) {
                    self.expr(e);
                }
                for t in targets {
                    self.check_assign(t);
                }
            }
            StmtKind::Expr(e) => self.expr(e),
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::MethodCall { object, method, .. }
                if self.method_writes_receiver(e, object, method) =>
            {
                let action = match (&object.kind, place_root(object)) {
                    (ExprKind::Identifier(_) | ExprKind::SelfValue, Some(r)) => {
                        format!("`{r}.{method}` (a method that writes its receiver)")
                    }
                    (_, Some(r)) => format!(
                        "`.{method}` (a method that writes its receiver) on a place inside `{r}`"
                    ),
                    _ => format!("`.{method}` (a method that writes its receiver)"),
                };
                self.check_through(object, e.span, &action);
            }
            ExprKind::Call { args, .. } => {
                for a in args.iter().filter(|a| a.mut_marker) {
                    self.check_through(&a.value, a.value.span, "a `mut` argument");
                }
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.expr(iterable);
                let origin = if self.for_binds_read_only(iterable) {
                    Origin::ForElem
                } else {
                    Origin::Owned
                };
                self.scopes.push(Vec::new());
                self.bind(pattern, origin);
                self.block(body);
                self.scopes.pop();
                return;
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.scopes.push(Vec::new());
                    self.bind(&arm.pattern, Origin::Owned);
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                    self.scopes.pop();
                }
                return;
            }
            ExprKind::Closure { params, body, .. } => {
                self.scopes.push(Vec::new());
                for p in params {
                    self.bind(&p.pattern, Origin::Owned);
                }
                self.expr(body);
                self.scopes.pop();
                return;
            }
            _ => {}
        }
        let mut kids: Vec<Child<'_>> = Vec::new();
        for_each_child_public(e, &mut |c| kids.push(c));
        for c in kids {
            match c {
                Child::Expr(x) => self.expr(x),
                Child::Block(b) => self.block(b),
            }
        }
    }

    /// §4.6: a bare `for` over a standard collection, or over an iterator
    /// that yields shared references, binds its elements as `ref`.
    fn for_binds_read_only(&self, iterable: &Expr) -> bool {
        let mut e = iterable;
        while let ExprKind::MethodCall { object, method, .. } = &e.kind {
            match method.as_str() {
                "iter_mut" | "values_mut" => return false,
                "iter" | "keys" | "values" => return true,
                "enumerate" | "rev" | "skip" | "take" | "step_by" | "filter" | "peekable"
                | "skip_while" | "take_while" => e = object,
                _ => return false,
            }
        }
        self.node_types
            .get(&iterable.id)
            .is_some_and(|(t, _)| TypeChecker::for_iterable_is_core_collection(t))
    }

    fn method_writes_receiver(&self, call: &Expr, object: &Expr, method: &str) -> bool {
        let key = self
            .tc
            .method_callee_types
            .get(&SpanKey::from_span(&call.span));
        // A stdlib type's own method (`Arena.push` takes `ref self`) wins over
        // the builtin-name guess; find it by the receiver's type name.
        let by_type = || {
            let mut t = &self.node_types.get(&object.id)?.0;
            while let Type::Ref(inner) | Type::MutRef(inner) = t {
                t = inner;
            }
            let name = match t {
                Type::Named { name, .. } | Type::Shared(name) => name,
                _ => return None,
            };
            if let Some(m) = self.self_modes.get(&format!("{name}.{method}")) {
                return Some(matches!(m, SelfParam::MutRef));
            }
            // A baked stdlib impl is in the env only: its receiver is the
            // method signature's first parameter.
            self.tc.env.impls.iter().find_map(|imp| {
                let sig = imp
                    .methods
                    .get(method)
                    .filter(|_| &imp.target_type == name)?;
                sig.params.first().map(|p| matches!(p, Type::MutRef(_)))
            })
        };
        match key.and_then(|k| self.self_modes.get(k)) {
            Some(m) => matches!(m, SelfParam::MutRef),
            None => by_type().unwrap_or_else(|| MUTATING_BUILTINS.contains(&method)),
        }
    }

    /// An assignment writes the slot `target` names; the places it projects
    /// from are what it writes through. A bare name is a rebinding.
    fn check_assign(&mut self, target: &Expr) {
        match &target.kind {
            ExprKind::FieldAccess { object, .. }
            | ExprKind::TupleIndex { object, .. }
            | ExprKind::Index { object, .. } => {
                self.check_through(object, target.span, "an assignment through it")
            }
            ExprKind::Unary {
                op: UnaryOp::Deref,
                operand,
            } => self.check_through(operand, target.span, "an assignment through it"),
            _ => {}
        }
    }

    /// Walk `place`'s projection chain from the outside in; the first step
    /// that is a reference or a handle decides.
    fn check_through(&mut self, place: &Expr, at: Span, action: &str) {
        let mut cur = place;
        loop {
            match self.step(cur) {
                Step::ReadOnly => {
                    let message = self.read_only_message(cur, action);
                    self.errors.push((at, message));
                    return;
                }
                Step::Writable => return,
                Step::Continue => {}
            }
            cur = match &cur.kind {
                ExprKind::FieldAccess { object, .. }
                | ExprKind::TupleIndex { object, .. }
                | ExprKind::Index { object, .. } => object,
                _ => return,
            };
        }
    }

    /// The error for a write through `at`, the read-only step, worded by
    /// where the borrow came from so the fix can be named.
    fn read_only_message(&self, at: &Expr, action: &str) -> String {
        let (name, origin) = match &at.kind {
            ExprKind::SelfValue => {
                let action = action.replace("through it", "through `self`");
                return format!(
                    "the receiver is `ref self`, so {action} cannot modify it \
                     (core-semantics.md §5.9). Declare the method `mut ref self`"
                );
            }
            ExprKind::Identifier(n) if n == "self" => {
                let action = action.replace("through it", "through `self`");
                return format!(
                    "the receiver is `ref self`, so {action} cannot modify it \
                     (core-semantics.md §5.9). Declare the method `mut ref self`"
                );
            }
            ExprKind::Identifier(n) => (n.as_str(), self.origin_of(n)),
            _ => {
                return format!(
                    "this place is a shared `ref`, so {action} cannot modify it \
                     (core-semantics.md §5.9). Borrow it as `mut ref` to write through it"
                )
            }
        };
        let action = action.replace("through it", &format!("through `{name}`"));
        match origin {
            Origin::Param => format!(
                "`{name}` is a `ref` parameter, so {action} cannot modify it \
                 (core-semantics.md §5.9). Declare it `{name}: mut ref ...` and mark its \
                 call sites `mut`"
            ),
            Origin::ForElem => format!(
                "`{name}` is a `ref` to an element of the collection the `for` iterates \
                 (§4.6), so {action} cannot modify it (core-semantics.md §5.9). Iterate \
                 with `.iter_mut()`"
            ),
            Origin::RefPattern => format!(
                "`{name}` is bound by `ref`, so {action} cannot modify it \
                 (core-semantics.md §5.9). Bind it as `mut ref {name}` from a mutable \
                 owned scrutinee"
            ),
            Origin::Owned => format!(
                "`{name}` is a shared `ref`, so {action} cannot modify it \
                 (core-semantics.md §5.9). Borrow it as `mut ref` to write through it"
            ),
        }
    }

    fn step(&self, e: &Expr) -> Step {
        let named_read_only = match &e.kind {
            ExprKind::SelfValue => self.self_read_only,
            ExprKind::Identifier(n) if n == "self" => self.self_read_only,
            ExprKind::Identifier(n) => self.origin_of(n) != Origin::Owned,
            _ => false,
        };
        let Some((ty, _)) = self.node_types.get(&e.id) else {
            return if named_read_only {
                Step::ReadOnly
            } else {
                Step::Writable
            };
        };
        // §5.9's exception: a handle's `mut` fields are writable through any
        // handle, however it was reached (§6.2 checks them at run time).
        if self.is_handle(ty) {
            return Step::Writable;
        }
        match ty {
            Type::Ref(inner) if self.is_handle(inner) => Step::Writable,
            Type::Ref(_) => Step::ReadOnly,
            Type::MutRef(_) => Step::Writable,
            _ if named_read_only => Step::ReadOnly,
            _ => Step::Continue,
        }
    }

    fn is_handle(&self, ty: &Type) -> bool {
        match ty {
            Type::Shared(_) | Type::Rc(_) | Type::Arc(_) => true,
            Type::Named { name, .. } => {
                self.tc
                    .env
                    .structs
                    .get(name)
                    .is_some_and(|s| s.is_shared || s.is_par)
                    || self
                        .tc
                        .env
                        .enums
                        .get(name)
                        .is_some_and(|s| s.is_shared || s.is_par)
            }
            _ => false,
        }
    }
}

/// The binding a place expression is rooted at.
fn place_root(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n),
        ExprKind::SelfValue => Some("self"),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root(object),
        _ => None,
    }
}
