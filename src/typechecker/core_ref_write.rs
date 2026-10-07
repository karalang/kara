//! v2 core: a shared `ref` place is read-only (`docs/core-semantics.md`
//! §5.9). Writing through one — assigning a place reached through it,
//! calling a `mut ref self` method on it, or passing it as a `mut`
//! argument — is an error. A write through a `mut ref`, or through a handle
//! (`shared` / `par` value, `Rc`, `Arc`) whose `mut` fields carry their own
//! borrow flags (§6.2), is not.
//!
//! A pass over each function body, run by the strict commands only. The
//! place's type at each step of its projection chain comes from the typed
//! node table, so a binding that is a `ref` because of its declaration, its
//! parameter mode, a bare `for` (§4.6) or a `ref` pattern is all one case.
//!
//! Where the borrow's declaration says how to make it writable, the error
//! carries that edit for `karac fix`: a `ref` parameter becomes `mut ref`
//! (its call sites then get their own `mut` marker fix), a `ref self`
//! receiver becomes `mut ref self`, and a bare `for` over a `Vec` or array
//! iterates `.iter_mut()`.

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
    scopes: Vec<Vec<Binding>>,
    /// The function's receiver is `ref self`.
    self_read_only: bool,
    /// The edit that makes a `ref self` receiver `mut ref self`.
    self_fix: Option<FixIt>,
    errors: Vec<(Span, String, Option<FixIt>)>,
}

/// A binding in scope: its name, why it is read-only, and the edit to its
/// declaration that would make it writable, when there is one.
type Binding = (String, Origin, Option<FixIt>);

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
    /// A `mut ref name` pattern binding: writable, and the write through
    /// its scrutinee is checked where it is bound.
    MutRefPattern,
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
            self_fix: None,
            errors: Vec::new(),
        };
        for f in fns.into_iter().filter(|f| !f.stdlib_origin) {
            w.self_read_only = matches!(f.self_param, Some(SelfParam::Ref));
            w.self_fix = match &f.self_span {
                Some(sp) if w.self_read_only && !f.self_is_frozen => Some(FixIt {
                    span: *sp,
                    replacement: "mut ref self".to_string(),
                }),
                _ => None,
            };
            w.scopes = vec![f
                .params
                .iter()
                .flat_map(|p| {
                    let (origin, fix) = if matches!(p.ty.kind, TypeKind::Ref(_)) {
                        // `ref T` → `mut ref T`: insert before the `ref`.
                        let span = Span {
                            length: 0,
                            ..p.ty.span
                        };
                        let fix = (!p.is_frozen).then(|| FixIt {
                            span,
                            replacement: "mut ".to_string(),
                        });
                        (Origin::Param, fix)
                    } else {
                        (Origin::Owned, None)
                    };
                    p.pattern
                        .binding_names()
                        .into_iter()
                        .map(move |n| (n, origin, fix.clone()))
                })
                .collect()];
            w.block(&f.body);
        }
        let mut errors = w.errors;
        errors.sort_by_key(|(s, _, _)| s.offset);
        errors.dedup_by(|a, b| a.0.offset == b.0.offset);
        // Several writes through one borrow share its declaration's edit;
        // attach it once, or `karac fix` would apply it once per write.
        let mut fixed: FxHashSet<usize> = FxHashSet::default();
        for (span, message, fix) in errors {
            match fix.filter(|f| fixed.insert(f.span.offset)) {
                Some(fix) => {
                    self.type_error_with_fix_it(message, span, TypeErrorKind::TypeMismatch, fix)
                }
                None => self.type_error(message, span, TypeErrorKind::TypeMismatch),
            }
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
    fn bind(&mut self, pattern: &Pattern, origin: Origin, fix: Option<FixIt>) {
        let program = self.tc.program;
        let names: Vec<Binding> = pattern
            .binding_name_spans()
            .into_iter()
            .map(|(n, sp)| {
                let key = SpanKey::from_span(&sp);
                if program.mut_ref_binding_spans.contains(&key) {
                    (n, Origin::MutRefPattern, None)
                } else if program.ref_binding_spans.contains(&key) {
                    // `ref name` → `mut ref name`: insert before the `ref`.
                    let fix = program.ref_binding_keywords.get(&key).map(|&offset| FixIt {
                        span: Span {
                            offset,
                            length: 0,
                            ..sp
                        },
                        replacement: "mut ".to_string(),
                    });
                    (n, Origin::RefPattern, fix)
                } else {
                    (n, origin, fix.clone())
                }
            })
            .collect();
        if let Some(scope) = self.scopes.last_mut() {
            scope.extend(names);
        }
    }

    /// A `mut ref name` binding writes through the scrutinee it borrows
    /// from, so the scrutinee must not be reached through a shared `ref`.
    fn check_mut_ref_bindings(&mut self, pattern: &Pattern, scrutinee: &Expr) {
        for (name, sp) in pattern.binding_name_spans() {
            if self
                .tc
                .program
                .mut_ref_binding_spans
                .contains(&SpanKey::from_span(&sp))
            {
                let action = format!("the `mut ref {name}` binding");
                self.check_through(scrutinee, sp, &action);
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.iter().rev().find(|(n, _, _)| n == name))
    }

    fn origin_of(&self, name: &str) -> Origin {
        self.lookup(name).map_or(Origin::Owned, |b| b.1)
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let { pattern, value, .. } => {
                self.expr(value);
                self.check_mut_ref_bindings(pattern, value);
                // A local bound to a read-only borrow is one too (§5.9),
                // and the same edit to the borrow's declaration fixes it.
                let (origin, fix) = match &value.kind {
                    ExprKind::Identifier(n) => self
                        .lookup(n)
                        .map_or((Origin::Owned, None), |b| (b.1, b.2.clone())),
                    _ => (Origin::Owned, None),
                };
                self.bind(pattern, origin, fix);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.expr(value);
                self.check_mut_ref_bindings(pattern, value);
                self.block(else_block);
                self.bind(pattern, Origin::Owned, None);
            }
            StmtKind::LetUninit { name, .. } => {
                if let Some(scope) = self.scopes.last_mut() {
                    scope.push((name.clone(), Origin::Owned, None));
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
                let (origin, fix) = if self.for_binds_read_only(iterable) {
                    (Origin::ForElem, self.iter_mut_fix(iterable))
                } else {
                    (Origin::Owned, None)
                };
                self.scopes.push(Vec::new());
                self.bind(pattern, origin, fix);
                self.block(body);
                self.scopes.pop();
                return;
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.check_mut_ref_bindings(&arm.pattern, scrutinee);
                    self.scopes.push(Vec::new());
                    self.bind(&arm.pattern, Origin::Owned, None);
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                    self.scopes.pop();
                }
                return;
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.expr(value);
                self.check_mut_ref_bindings(pattern, value);
                self.scopes.push(Vec::new());
                self.bind(pattern, Origin::Owned, None);
                self.block(then_block);
                self.scopes.pop();
                if let Some(e) = else_branch {
                    self.expr(e);
                }
                return;
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                self.expr(value);
                self.check_mut_ref_bindings(pattern, value);
                self.scopes.push(Vec::new());
                self.bind(pattern, Origin::Owned, None);
                self.block(body);
                self.scopes.pop();
                return;
            }
            ExprKind::Closure { params, body, .. } => {
                self.scopes.push(Vec::new());
                for p in params {
                    self.bind(&p.pattern, Origin::Owned, None);
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

    /// The edit that makes a bare `for` over a `Vec` or array bind its
    /// elements as `mut ref`: append `.iter_mut()` to the iterated place.
    /// Other iterables (a map, a set, an iterator chain) have no one edit.
    fn iter_mut_fix(&self, iterable: &Expr) -> Option<FixIt> {
        place_root(iterable)?;
        let mut t = &self.node_types.get(&iterable.id)?.0;
        while let Type::Ref(inner) | Type::MutRef(inner) = t {
            t = inner;
        }
        let vec_like = match t {
            Type::Array { .. } => true,
            Type::Named { name, .. } => name == "Vec",
            _ => false,
        };
        vec_like.then(|| FixIt {
            span: Span {
                offset: iterable.span.offset + iterable.span.length,
                length: 0,
                ..iterable.span
            },
            replacement: ".iter_mut()".to_string(),
        })
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
                    let fix = match &cur.kind {
                        ExprKind::SelfValue => self.self_fix.clone(),
                        ExprKind::Identifier(n) if n == "self" => self.self_fix.clone(),
                        ExprKind::Identifier(n) => self.lookup(n).and_then(|b| b.2.clone()),
                        _ => None,
                    };
                    self.errors.push((at, message, fix));
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
            Origin::Owned | Origin::MutRefPattern => format!(
                "`{name}` is a shared `ref`, so {action} cannot modify it \
                 (core-semantics.md §5.9). Borrow it as `mut ref` to write through it"
            ),
        }
    }

    fn step(&self, e: &Expr) -> Step {
        let named_read_only = match &e.kind {
            ExprKind::SelfValue => self.self_read_only,
            ExprKind::Identifier(n) if n == "self" => self.self_read_only,
            ExprKind::Identifier(n) => match self.origin_of(n) {
                Origin::Owned => false,
                Origin::MutRefPattern => return Step::Writable,
                _ => true,
            },
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
