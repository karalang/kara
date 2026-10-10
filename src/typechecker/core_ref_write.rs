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
//!
//! The same walk checks that a place a `for` loop iterates is not written
//! in the loop's body through the same binding (§5.6; §6.2 for a `mut` field
//! of a shared value): pushing to `self.items` inside `for x in self.items`.
//! A write through another handle to the same object is §6.2's run-time
//! check, not this one.
//!
//! And it checks function kinds (§9.6): a closure literal that writes a
//! captured binding is a `MutFn`, so passing it to a user function or method whose
//! parameter is a plain `Fn` is an error, as is passing a `MutFn` parameter
//! on to one, or a local bound to such a closure. A closure that moves a
//! capture out is an `OnceFn`, which the type checker already reports where
//! an `Fn` is expected (E0235). The removed capture prefixes are reported
//! here too, with the edit that deletes them, as are calls of the removed
//! `collect_all` and `collect_all_vec` (§11.7) and of the free `spawn`
//! (§11.1).

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
    /// The places enclosing `for` loops iterate, innermost last.
    borrowed: Vec<Borrowed>,
    /// Locals bound by `let name = ref <place>`: the local, and the place it
    /// borrows (root, path, through a handle). A `for` over a place inside
    /// such a local borrows that place for the whole loop.
    ref_lets: Vec<(Root, Root, Vec<Option<String>>, bool)>,
    /// The `if` / `match` branches the walk is inside (see `exclusive`).
    branches: Vec<(usize, usize)>,
    /// Conflicting writes to a borrowed place (§5.6, §6.2).
    conflicts: Vec<(Span, String, Option<FixIt>)>,
    /// The program's own free functions by name, for the kind check at a
    /// call (§9.6). Library functions are not migrated to `MutFn` yet.
    fn_decls: &'t FxHashMap<String, &'a Function>,
    /// The program's own methods, keyed `Type.method`.
    method_decls: &'t FxHashMap<String, &'a Function>,
    /// Closure literals being walked, innermost last: the scope depth each
    /// starts at, and the first captured binding its body writes.
    closures: Vec<(usize, Option<String>)>,
    /// The captured binding each closure literal writes, by the closure's
    /// span offset.
    closure_writes: FxHashMap<usize, String>,
    /// The current function's `MutFn` parameters.
    mut_fn_params: FxHashSet<String>,
    /// Locals bound to a closure literal that writes a capture, with the
    /// capture's name.
    mut_closure_locals: Vec<((usize, usize), String)>,
    /// Function-kind errors (§9.6).
    kind_errors: Vec<(Span, String)>,
    /// Removed capture prefixes, with the edit that deletes each (§9.6).
    prefix_errors: Vec<(Span, String, FixIt)>,
    /// Calls of removed library functions (§11.7).
    removed_calls: Vec<(Span, String)>,
    /// The callee of each call of an `OnceFn` value, for the ownership
    /// pass's E0500 wording (§9.6).
    once_calls: FxHashSet<SpanKey>,
}

/// A place a `for` loop borrows for its whole body, or a `ref` pattern
/// binding borrows until its last use.
struct Borrowed {
    root: Root,
    /// Field and tuple steps from the root; `None` is an index.
    path: Vec<Option<String>>,
    /// The place as written, for the message.
    text: String,
    /// Reached through a `shared` handle (§6.2 rather than §5.6).
    handle: bool,
    /// For a `ref` pattern binding: what it borrows until its last use.
    binding: Option<RefHold>,
    /// The edit that removes a conflict with this borrow, when one is known.
    fix: Option<FixIt>,
}

/// A `ref` pattern binding's borrow (§5.6): a write to the place conflicts
/// while some use of the binding can still follow it.
#[derive(Clone)]
struct RefHold {
    name: String,
    /// The binding's offset, which identifies the hold.
    at: usize,
    /// Each use: its offset and the branches it sits in.
    uses: Vec<(usize, Vec<(usize, usize)>)>,
}

/// The branches (`if` / `match` node offset, branch index) on the path to a
/// point. Two points whose paths take different branches of one node are
/// never both reached.
fn exclusive(a: &[(usize, usize)], b: &[(usize, usize)]) -> bool {
    a.iter()
        .any(|(n, i)| b.iter().any(|(m, j)| n == m && i != j))
}

/// Which binding a place is rooted at: `self`, or a position in `scopes`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Root {
    SelfValue,
    Local(usize, usize),
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
    ) -> FxHashSet<SpanKey> {
        let mut self_modes: FxHashMap<String, SelfParam> = FxHashMap::default();
        let mut fns: Vec<&Function> = Vec::new();
        let mut fn_decls: FxHashMap<String, &Function> = FxHashMap::default();
        let mut method_decls: FxHashMap<String, &Function> = FxHashMap::default();
        for item in &self.program.items {
            match item {
                Item::Function(f) => {
                    if !f.stdlib_origin {
                        fn_decls.insert(f.name.clone(), f);
                    }
                    fns.push(f)
                }
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
                            if let (Some(h), false) = (&head, m.stdlib_origin) {
                                method_decls.insert(format!("{h}.{}", m.name), m.as_ref());
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
            borrowed: Vec::new(),
            ref_lets: Vec::new(),
            branches: Vec::new(),
            conflicts: Vec::new(),
            fn_decls: &fn_decls,
            method_decls: &method_decls,
            closures: Vec::new(),
            closure_writes: FxHashMap::default(),
            mut_fn_params: FxHashSet::default(),
            mut_closure_locals: Vec::new(),
            kind_errors: Vec::new(),
            prefix_errors: Vec::new(),
            once_calls: FxHashSet::default(),
            removed_calls: Vec::new(),
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
            w.borrowed.clear();
            w.ref_lets.clear();
            w.branches.clear();
            w.mut_fn_params.clear();
            w.mut_closure_locals.clear();
            let mut_fns: Vec<String> = f
                .params
                .iter()
                .filter(|p| w.is_mut_fn(&p.ty))
                .flat_map(|p| p.pattern.binding_names())
                .collect();
            w.mut_fn_params.extend(mut_fns);
            w.block(&f.body);
        }
        let mut conflicts = w.conflicts;
        let mut errors = w.errors;
        let kind_errors = w.kind_errors;
        let prefix_errors = w.prefix_errors;
        let once_calls = w.once_calls;
        for (span, message) in w.removed_calls {
            self.type_error(message, span, TypeErrorKind::RemovedInCore);
        }
        for (span, message) in kind_errors {
            self.type_error(message, span, TypeErrorKind::FnKindMismatch);
        }
        for (span, message, fix) in prefix_errors {
            self.type_error_with_fix_it(message, span, TypeErrorKind::CapturePrefixRemoved, fix);
        }
        conflicts.sort_by_key(|(s, _, _)| s.offset);
        conflicts.dedup_by(|a, b| a.0.offset == b.0.offset);
        let mut fixed_loops: FxHashSet<usize> = FxHashSet::default();
        for (span, message, fix) in conflicts {
            match fix.filter(|f| fixed_loops.insert(f.span.offset)) {
                Some(fix) => {
                    self.type_error_with_fix_it(message, span, TypeErrorKind::BorrowConflict, fix)
                }
                None => self.type_error(message, span, TypeErrorKind::BorrowConflict),
            }
        }
        errors.sort_by_key(|(s, _, _)| s.offset);
        errors.dedup_by(|a, b| a.0.offset == b.0.offset);
        // Several writes through one borrow share its declaration's edit;
        // attach it once, or `karac fix` would apply it once per write.
        let mut fixed: FxHashSet<usize> = FxHashSet::default();
        for (span, message, fix) in errors {
            match fix.filter(|f| fixed.insert(f.span.offset)) {
                Some(fix) => self.type_error_with_fix_it(
                    message,
                    span,
                    TypeErrorKind::WriteThroughSharedRef,
                    fix,
                ),
                None => self.type_error(message, span, TypeErrorKind::WriteThroughSharedRef),
            }
        }
        once_calls
    }
}

impl Walk<'_, '_> {
    fn block(&mut self, b: &Block) {
        self.scopes.push(Vec::new());
        let lets = self.ref_lets.len();
        let mut held = Vec::new();
        for (i, s) in b.stmts.iter().enumerate() {
            self.stmt(s);
            held.extend(self.hold_ref_let(s, &b.stmts[i + 1..], b.final_expr.as_deref()));
        }
        if let Some(e) = &b.final_expr {
            self.expr(e);
        }
        self.release(&held);
        self.ref_lets.truncate(lets);
        self.scopes.pop();
        let depth = self.scopes.len();
        self.mut_closure_locals.retain(|((i, _), _)| *i < depth);
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

    /// The binding `name` resolves to.
    fn root_of(&self, name: &str) -> Root {
        if name == "self" {
            return Root::SelfValue;
        }
        for (i, scope) in self.scopes.iter().enumerate().rev() {
            if let Some(j) = scope.iter().rposition(|(n, _, _)| n == name) {
                return Root::Local(i, j);
            }
        }
        Root::SelfValue
    }

    /// A place's root binding and its projection steps, and whether a step
    /// goes through a `shared` handle.
    fn place_path(&self, e: &Expr) -> Option<(Root, Vec<Option<String>>, bool)> {
        let mut path = Vec::new();
        let mut handle = false;
        let mut cur = e;
        loop {
            let (object, step) = match &cur.kind {
                ExprKind::FieldAccess { object, field } => (object, Some(field.clone())),
                ExprKind::TupleIndex { object, index } => (object, Some(index.to_string())),
                ExprKind::Index { object, .. } => (object, None),
                ExprKind::Identifier(n) => {
                    path.reverse();
                    return Some((self.root_of(n), path, handle));
                }
                ExprKind::SelfValue => {
                    path.reverse();
                    return Some((Root::SelfValue, path, handle));
                }
                _ => return None,
            };
            path.push(step);
            handle |= self
                .node_types
                .get(&object.id)
                .is_some_and(|(t, _)| match t {
                    Type::Ref(inner) | Type::MutRef(inner) => self.is_handle(inner),
                    t => self.is_handle(t),
                });
            cur = object;
        }
    }

    /// The place a `for` over `iterable` borrows for its whole body: the
    /// collection place under any iterator adaptors (`v.iter()`,
    /// `v.iter_mut().enumerate()`). A temporary, or a consuming
    /// `.into_iter()`, borrows nothing.
    fn iterated_place(&self, iterable: &Expr) -> Option<Borrowed> {
        let mut e = iterable;
        while let ExprKind::MethodCall { object, method, .. } = &e.kind {
            match method.as_str() {
                "iter" | "iter_mut" | "keys" | "values" | "values_mut" | "enumerate" | "rev"
                | "skip" | "take" | "step_by" | "filter" | "peekable" | "skip_while"
                | "take_while" | "chars" | "bytes" | "lines" => e = object,
                _ => return None,
            }
        }
        let (root, path, handle) = self.place_path(e)?;
        Some(Borrowed {
            root,
            path,
            text: place_text(e)?,
            handle,
            binding: None,
            fix: None,
        })
    }

    /// Push `held`, returning what identifies each for `release`.
    fn hold(&mut self, held: Vec<Borrowed>) -> Vec<usize> {
        let keys = held
            .iter()
            .filter_map(|b| b.binding.as_ref().map(|h| h.at))
            .collect();
        self.borrowed.extend(held);
        keys
    }

    /// Drop the borrows `hold` pushed. An assignment in between may have
    /// removed some already.
    fn release(&mut self, keys: &[usize]) {
        self.borrowed
            .retain(|b| b.binding.as_ref().is_none_or(|h| !keys.contains(&h.at)));
    }

    /// §5.6: `let name = ref <place>` borrows the place from here to the
    /// last use of `name` in `rest` / `tail`, as a `ref` pattern binding
    /// does. Returns what identifies the hold for `release`.
    fn hold_ref_let(&mut self, s: &Stmt, rest: &[Stmt], tail: Option<&Expr>) -> Vec<usize> {
        let StmtKind::Let { pattern, value, .. } = &s.kind else {
            return Vec::new();
        };
        let (
            PatternKind::Binding(name),
            ExprKind::Unary {
                op: UnaryOp::Ref,
                operand,
            },
        ) = (&pattern.kind, &value.kind)
        else {
            return Vec::new();
        };
        let (Some((root, path, handle)), Some(text)) =
            (self.place_path(operand), place_text(operand))
        else {
            return Vec::new();
        };
        self.ref_lets
            .push((self.root_of(name), root, path.clone(), handle));
        let rest = Block {
            stmts: rest.to_vec(),
            final_expr: tail.map(|e| Box::new(e.clone())),
            span: s.span,
        };
        let mut uses = Vec::new();
        uses_in_block(&rest, name, &mut self.branches.clone(), &mut uses);
        if uses.is_empty() {
            return Vec::new();
        }
        self.hold(vec![Borrowed {
            root,
            path,
            text,
            handle,
            binding: Some(RefHold {
                name: name.clone(),
                at: pattern.span.offset,
                uses,
            }),
            fix: None,
        }])
    }

    /// The place a `for` over `iterable` borrows through a `let … = ref`
    /// local it is rooted at: `rel.deps` after `let rel = ref self.rels[i]`
    /// borrows `self.rels[..].deps` for the whole loop.
    fn iterated_through_ref_let(&self, iterable: &Expr) -> Option<Borrowed> {
        let inner = self.iterated_place(iterable)?;
        let (_, base, base_path, handle) = self
            .ref_lets
            .iter()
            .rev()
            .find(|(local, ..)| *local == inner.root)?;
        let mut path = base_path.clone();
        path.extend(inner.path);
        Some(Borrowed {
            root: *base,
            path,
            text: inner.text,
            handle: *handle || inner.handle,
            binding: None,
            fix: None,
        })
    }

    /// The scrutinee place each `ref` / `mut ref` binding of `pattern`
    /// borrows while the binding is live: from here to its last use in
    /// `body` (§5.6). A binding never used after the pattern borrows
    /// nothing a write could conflict with.
    fn ref_binding_borrows(
        &self,
        pattern: &Pattern,
        scrutinee: &Expr,
        body: &[&Expr],
        blocks: &[&Block],
    ) -> Vec<Borrowed> {
        let program = self.tc.program;
        let Some((root, path, handle)) = self.place_path(scrutinee) else {
            return Vec::new();
        };
        let Some(text) = place_text(scrutinee) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (name, sp) in pattern.binding_name_spans() {
            let key = SpanKey::from_span(&sp);
            if !program.ref_binding_spans.contains(&key)
                && !program.mut_ref_binding_spans.contains(&key)
            {
                continue;
            }
            let mut uses = Vec::new();
            let mut branch = self.branches.clone();
            for e in body {
                uses_in_expr(e, &name, &mut branch, &mut uses);
            }
            for b in blocks {
                uses_in_block(b, &name, &mut branch, &mut uses);
            }
            if !uses.is_empty() {
                out.push(Borrowed {
                    root,
                    path: path.clone(),
                    text: text.clone(),
                    handle,
                    binding: Some(RefHold {
                        name,
                        at: sp.offset,
                        uses,
                    }),
                    fix: None,
                });
            }
        }
        out
    }

    /// §5.6 / §6.2: a write to `place` while an enclosing `for` loop iterates
    /// it, a place inside it, or a place it is inside, through the same
    /// binding.
    fn check_borrowed_write(&mut self, place: &Expr, at: Span, verb: &str) {
        if self.borrowed.is_empty() {
            return;
        }
        let Some((root, path, handle)) = self.place_path(place) else {
            return;
        };
        let branches = &self.branches;
        let overlaps = |b: &Borrowed| {
            b.binding.as_ref().is_none_or(|h| {
                h.uses
                    .iter()
                    .any(|(u, p)| *u > at.offset && !exclusive(p, branches))
            }) && b.root == root
                && b.path
                    .iter()
                    .zip(&path)
                    .all(|(x, y)| x.is_none() || y.is_none() || x == y)
        };
        let Some(b) = self.borrowed.iter().rev().find(|b| overlaps(b)) else {
            return;
        };
        let text = place_text(place).unwrap_or_else(|| b.text.clone());
        let message = if let Some(RefHold { name, .. }) = &b.binding {
            format!(
                "cannot {verb} `{text}` while `ref {name}` borrows `{}`: `{name}` is used \
                 after this (core-semantics.md §5.6). Use `{name}` before the write, or \
                 clone what you need from it first",
                b.text
            )
        } else if b.handle || handle {
            format!(
                "cannot {verb} `{text}` while the `for` loop over `{}` borrows it: a `mut` \
                 field of a shared value is borrowed for the whole loop, and this write \
                 goes through the same handle (core-semantics.md §6.2). Collect the \
                 changes and apply them after the loop",
                b.text
            )
        } else {
            format!(
                "cannot {verb} `{text}` while the `for` loop over `{}` borrows it \
                 (core-semantics.md §5.6). Collect the changes and apply them after the \
                 loop, or iterate over a copy",
                b.text
            )
        };
        let fix = b.fix.clone();
        self.conflicts.push((at, message, fix));
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
                let wrote = self.closure_writes.get(&value.span.offset).cloned();
                if let (Some(n), PatternKind::Binding(name)) = (wrote, &pattern.kind) {
                    if matches!(value.kind, ExprKind::Closure { .. }) {
                        if let Root::Local(i, j) = self.root_of(name) {
                            self.mut_closure_locals.push(((i, j), n));
                        }
                    }
                }
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
                self.check_borrowed_write(target, target.span, "assign");
                self.note_capture_write(target);
                // A rebound root names another value from here on.
                if let ExprKind::Identifier(n) = &target.kind {
                    let root = self.root_of(n);
                    self.borrowed.retain(|b| b.root != root);
                }
            }
            StmtKind::MultiAssign { targets, values } => {
                for e in values.iter().chain(targets) {
                    self.expr(e);
                }
                for t in targets {
                    self.check_assign(t);
                    self.check_borrowed_write(t, t.span, "assign");
                    self.note_capture_write(t);
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
                let verb = format!("call `.{method}` (a method that writes its receiver) on");
                self.check_borrowed_write(object, e.span, &verb);
                self.note_capture_write(object);
            }
            ExprKind::Call { callee, args } => {
                for a in args.iter().filter(|a| a.mut_marker) {
                    self.check_through(&a.value, a.value.span, "a `mut` argument");
                    self.check_borrowed_write(&a.value, a.value.span, "pass `mut`");
                    self.note_capture_write(&a.value);
                }
                self.children(e);
                self.check_fn_kinds(callee, args);
                return;
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
                let before = self.borrowed.len();
                let borrowed = self.iterated_place(iterable);
                self.borrowed.extend(borrowed);
                if let Some(mut b) = self.iterated_through_ref_let(iterable) {
                    // A conflict here is fixed by iterating a copy.
                    b.fix = Some(FixIt {
                        span: Span {
                            offset: iterable.span.offset + iterable.span.length,
                            length: 0,
                            ..iterable.span
                        },
                        replacement: ".clone()".to_string(),
                    });
                    self.borrowed.push(b);
                }
                self.scopes.push(Vec::new());
                self.bind(pattern, origin, fix);
                self.block(body);
                self.scopes.pop();
                self.borrowed.truncate(before);
                return;
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for (i, arm) in arms.iter().enumerate() {
                    self.check_mut_ref_bindings(&arm.pattern, scrutinee);
                    let held = self.ref_binding_borrows(
                        &arm.pattern,
                        scrutinee,
                        &[arm.guard.as_ref(), Some(&arm.body)]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>(),
                        &[],
                    );
                    let keys = self.hold(held);
                    self.scopes.push(Vec::new());
                    self.bind(&arm.pattern, Origin::Owned, None);
                    self.branches.push((e.span.offset, i + 1));
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                    self.branches.pop();
                    self.scopes.pop();
                    self.release(&keys);
                }
                return;
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.expr(condition);
                self.branches.push((e.span.offset, 1));
                self.block(then_block);
                self.branches.pop();
                if let Some(x) = else_branch {
                    self.branches.push((e.span.offset, 2));
                    self.expr(x);
                    self.branches.pop();
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
                let held = self.ref_binding_borrows(pattern, value, &[], &[then_block]);
                let keys = self.hold(held);
                self.scopes.push(Vec::new());
                self.bind(pattern, Origin::Owned, None);
                self.branches.push((e.span.offset, 1));
                self.block(then_block);
                self.branches.pop();
                self.scopes.pop();
                self.release(&keys);
                if let Some(x) = else_branch {
                    self.branches.push((e.span.offset, 2));
                    self.expr(x);
                    self.branches.pop();
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
                let held = self.ref_binding_borrows(pattern, value, &[], &[body]);
                let keys = self.hold(held);
                self.scopes.push(Vec::new());
                self.bind(pattern, Origin::Owned, None);
                self.block(body);
                self.scopes.pop();
                self.release(&keys);
                return;
            }
            ExprKind::Closure {
                params,
                body,
                prefix_span,
                ..
            } => {
                if let Some(sp) = prefix_span {
                    // Delete the prefix and the space after it; `karac fix`
                    // drops the edit if what is left does not parse.
                    let fix = FixIt {
                        span: Span {
                            length: sp.length + 1,
                            ..*sp
                        },
                        replacement: String::new(),
                    };
                    self.prefix_errors.push((
                        *sp,
                        "closure capture prefixes are removed; a closure's captures are \
                         inferred from its body (§9.6). Clone a value before the closure \
                         to give the closure its own copy"
                            .to_string(),
                        fix,
                    ));
                }
                self.closures.push((self.scopes.len(), None));
                self.scopes.push(Vec::new());
                for p in params {
                    self.bind(&p.pattern, Origin::Owned, None);
                }
                self.expr(body);
                self.scopes.pop();
                if let Some((_, Some(name))) = self.closures.pop() {
                    self.closure_writes.insert(e.span.offset, name);
                }
                return;
            }
            _ => {}
        }
        self.children(e);
        if let ExprKind::MethodCall { args, .. } = &e.kind {
            self.check_method_kinds(e, args);
        }
    }

    fn children(&mut self, e: &Expr) {
        let mut kids: Vec<Child<'_>> = Vec::new();
        for_each_child_public(e, &mut |c| kids.push(c));
        for c in kids {
            match c {
                Child::Expr(x) => self.expr(x),
                Child::Block(b) => self.block(b),
            }
        }
    }

    /// A `MutFn(...)` type, under any `ref`s.
    fn is_mut_fn(&self, ty: &TypeExpr) -> bool {
        matches!(ty.kind, TypeKind::FnType { is_once: false, .. })
            && self
                .tc
                .program
                .mut_fn_types
                .contains(&SpanKey::from_span(&ty.span))
    }

    /// A write to `place` from inside a closure that captures its root makes
    /// that closure (and each enclosing one that captures it) a `MutFn`.
    fn note_capture_write(&mut self, place: &Expr) {
        if self.closures.is_empty() {
            return;
        }
        let mut p = place;
        while let ExprKind::Unary {
            op: UnaryOp::Deref,
            operand,
        } = &p.kind
        {
            p = operand;
        }
        let Some(root) = place_root(p) else {
            return;
        };
        let depth = if root == "self" {
            0
        } else {
            match self
                .scopes
                .iter()
                .rposition(|s| s.iter().any(|(n, _, _)| n == root))
            {
                Some(i) => i,
                None => return,
            }
        };
        for frame in self.closures.iter_mut().filter(|f| f.0 > depth) {
            frame.1.get_or_insert_with(|| root.to_string());
        }
    }

    /// §9.6 at a call: a user function's plain `Fn` parameter takes neither
    /// a closure that writes a capture nor a `MutFn` parameter.
    fn check_fn_kinds(&mut self, callee: &Expr, args: &[CallArg]) {
        let ExprKind::Identifier(name) = &callee.kind else {
            return;
        };
        if let Some((Type::OnceFunction { .. }, _)) = self.node_types.get(&callee.id) {
            self.once_calls.insert(SpanKey::from_span(&callee.span));
        }
        if self.lookup(name).is_some() {
            return;
        }
        if matches!(name.as_str(), "collect_all" | "collect_all_vec")
            && !self.fn_decls.contains_key(name.as_str())
        {
            self.removed_calls.push((
                callee.span,
                format!(
                    "`{name}` is removed (§11.7); a `par {{ … }}` block whose branches \
                     produce `Result` values without `?` already returns every result, \
                     in source order"
                ),
            ));
            return;
        }
        if name == "spawn" && !self.fn_decls.contains_key("spawn") {
            self.removed_calls.push((
                callee.span,
                "free `spawn` is removed (§11.1); a task starts only in `par { … }`, \
                 `par for` or `TaskGroup.spawn`, so it cannot outlive its scope"
                    .to_string(),
            ));
            return;
        }
        let Some(f) = self.fn_decls.get(name.as_str()).copied() else {
            return;
        };
        self.check_arg_kinds(name, f, args);
    }

    /// §9.6 at a method call: the same check against a program's own
    /// method.
    fn check_method_kinds(&mut self, call: &Expr, args: &[CallArg]) {
        let Some(key) = self
            .tc
            .method_callee_types
            .get(&SpanKey::from_span(&call.span))
        else {
            return;
        };
        let Some(f) = self.method_decls.get(key.as_str()).copied() else {
            return;
        };
        let name = key.clone();
        self.check_arg_kinds(&name, f, args);
    }

    /// A plain `Fn` parameter of `f` takes neither a closure that writes a
    /// capture nor a `MutFn` (§9.6).
    fn check_arg_kinds(&mut self, name: &str, f: &Function, args: &[CallArg]) {
        let mut position = 0;
        for a in args {
            let param = match &a.label {
                Some(l) => f
                    .params
                    .iter()
                    .find(|p| p.pattern.binding_names().iter().any(|n| n == l)),
                None => {
                    position += 1;
                    f.params.get(position - 1)
                }
            };
            let Some(param) = param else {
                continue;
            };
            let mut ty = &param.ty;
            while let TypeKind::Ref(inner) | TypeKind::MutRef(inner) = &ty.kind {
                ty = inner;
            }
            if !matches!(ty.kind, TypeKind::FnType { is_once: false, .. }) || self.is_mut_fn(ty) {
                continue;
            }
            let message = match &a.value.kind {
                ExprKind::Closure { .. } => match self.closure_writes.get(&a.value.span.offset) {
                    Some(n) => format!(
                        "the closure mutates {n}, so it is a MutFn, but {name} expects Fn \
                         (§9.6)"
                    ),
                    None => continue,
                },
                ExprKind::Identifier(x) => {
                    match self.root_of(x) {
                        Root::Local(0, _) if self.mut_fn_params.contains(x) => {
                            format!("`{x}` is a MutFn, but {name} expects Fn (§9.6)")
                        }
                        Root::Local(i, j) => match self.mut_closure_locals.iter().find(|(r, n)| {
                            *r == (i, j) && self.scopes[i][j].0 == *x && !n.is_empty()
                        }) {
                            Some((_, n)) => format!(
                                "`{x}` mutates {n}, so it is a MutFn, but {name} expects Fn (§9.6)"
                            ),
                            None => continue,
                        },
                        Root::SelfValue => continue,
                    }
                }
                _ => continue,
            };
            self.kind_errors.push((a.value.span, message));
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

/// Every read of the identifier `name` in `e`, with the branches it sits in.
fn uses_in_expr(
    e: &Expr,
    name: &str,
    branch: &mut Vec<(usize, usize)>,
    out: &mut Vec<(usize, Vec<(usize, usize)>)>,
) {
    let at = e.span.offset;
    match &e.kind {
        ExprKind::Identifier(n) if n == name => out.push((at, branch.clone())),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            uses_in_expr(condition, name, branch, out);
            branch.push((at, 1));
            uses_in_block(then_block, name, branch, out);
            branch.pop();
            if let Some(x) = else_branch {
                branch.push((at, 2));
                uses_in_expr(x, name, branch, out);
                branch.pop();
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            uses_in_expr(value, name, branch, out);
            branch.push((at, 1));
            uses_in_block(then_block, name, branch, out);
            branch.pop();
            if let Some(x) = else_branch {
                branch.push((at, 2));
                uses_in_expr(x, name, branch, out);
                branch.pop();
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            uses_in_expr(scrutinee, name, branch, out);
            for (i, arm) in arms.iter().enumerate() {
                branch.push((at, i + 1));
                if let Some(g) = &arm.guard {
                    uses_in_expr(g, name, branch, out);
                }
                uses_in_expr(&arm.body, name, branch, out);
                branch.pop();
            }
        }
        _ => for_each_child_public(e, &mut |c| match c {
            Child::Expr(x) => uses_in_expr(x, name, branch, out),
            Child::Block(b) => uses_in_block(b, name, branch, out),
        }),
    }
}

fn uses_in_block(
    b: &Block,
    name: &str,
    branch: &mut Vec<(usize, usize)>,
    out: &mut Vec<(usize, Vec<(usize, usize)>)>,
) {
    crate::index_disjoint::for_each_block_child(b, &mut |c| match c {
        Child::Expr(x) => uses_in_expr(x, name, branch, out),
        Child::Block(b) => uses_in_block(b, name, branch, out),
    });
}

/// A place expression as written: `self.items`, `b.items[]`.
fn place_text(e: &Expr) -> Option<String> {
    Some(match &e.kind {
        ExprKind::Identifier(n) => n.clone(),
        ExprKind::SelfValue => "self".to_string(),
        ExprKind::FieldAccess { object, field } => format!("{}.{field}", place_text(object)?),
        ExprKind::TupleIndex { object, index } => format!("{}.{index}", place_text(object)?),
        ExprKind::Index { object, .. } => format!("{}[..]", place_text(object)?),
        _ => return None,
    })
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
