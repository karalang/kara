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
    /// Checking an assignment's target: a read-only `Slice[T]` on the way
    /// stops it. A method that writes a slice reports its own error.
    assigning: bool,
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
    /// Reads of a place a `mut ref` local borrows (§5.6). A write at the
    /// same place reports instead.
    read_conflicts: Vec<(Span, String)>,
    /// The place expressions already asked about a read, by address: each
    /// projection chain is asked once, as a whole.
    place_seen: FxHashSet<usize>,
    /// Offsets the current point cannot reach code past: the end of each
    /// `return` it is inside or about to run into. A use of a `ref` local
    /// there is on no path from here, so it does not keep the borrow live
    /// (§5.6; B-2026-10-15-31).
    dead_after: Vec<usize>,
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
    /// Its `MutFn` parameters held by a shared borrow (a bare or `ref`
    /// one), with the edit that makes each `own` where there is one.
    shared_mut_fn_params: FxHashMap<String, Option<FixIt>>,
    /// Calls of a `MutFn` through a shared borrow (§9.6).
    shared_mut_fn_calls: Vec<(Span, String, Option<FixIt>)>,
    /// The scopes that hold a `for` pattern's bindings.
    for_item_scopes: Vec<usize>,
    /// Locals bound to a closure literal that writes a capture, with the
    /// capture's name.
    mut_closure_locals: Vec<((usize, usize), String)>,
    /// MutFn locals a `let` has moved out of (§1.1: a `MutFn` is never
    /// `Copy`), with the binding that took each one (B-2026-10-19-36).
    moved_mut_fns: Vec<((usize, usize), String)>,
    /// Function-kind errors (§9.6).
    kind_errors: Vec<(Span, String)>,
    /// Removed capture prefixes, with the edit that deletes each (§9.6).
    prefix_errors: Vec<(Span, String, FixIt)>,
    /// Calls into a library surface placed in M4a (B-2026-10-20-8).
    m4a_calls: Vec<(Span, String)>,
    /// `Server.serve*` calls whose handler's `Response` is misshaped: a type
    /// mismatch, not an M4a deferral.
    handler_shapes: Vec<(Span, String)>,
    /// Calls of removed library functions (§11.7).
    removed_calls: Vec<(Span, String)>,
    /// Offsets of plain pattern bindings some write goes through.
    written_pattern_bindings: FxHashSet<usize>,
    /// A plain pattern binding of a `match`, `if let` or `while let` over a
    /// place reached through a shared `ref`, by the binding's offset: where
    /// the place is read-only, and the scrutinee as written. A non-`Copy`
    /// binding is a `ref` into that place (§4.6), so a write through it is
    /// a write through the `ref` (§5.9; B-2026-10-24-21).
    pattern_read_only: FxHashMap<usize, (ReadOnlyAt, String)>,
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
    /// A `let name = mut ref <place>` borrow: while it is live the place is
    /// not read either (§5.6).
    unique: bool,
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
    /// A plain pattern binding at this offset: owned, as `Owned` is. A write
    /// through it means the `ref` its move fix inserts must be `mut ref`
    /// (B-2026-10-18-28).
    OwnedPattern(usize),
    /// A `ref T` parameter.
    Param,
    /// The item of a bare `for` over a collection (§4.6).
    ForElem,
    /// A `ref name` pattern binding.
    RefPattern,
    /// A `mut ref name` pattern binding: writable, and the write through
    /// its scrutinee is checked where it is bound.
    MutRefPattern,
    /// `let b = self;` over a borrowed receiver: `b` is a `ref Self` (§4.6).
    SelfCopy,
}

/// The read-only step of a place: the expression, where its binding came
/// from, and the edit that makes it writable.
#[derive(Clone)]
struct ReadOnlyAt {
    at: Expr,
    origin: Origin,
    fix: Option<FixIt>,
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
            assigning: false,
            borrowed: Vec::new(),
            ref_lets: Vec::new(),
            branches: Vec::new(),
            conflicts: Vec::new(),
            read_conflicts: Vec::new(),
            place_seen: FxHashSet::default(),
            dead_after: Vec::new(),
            fn_decls: &fn_decls,
            method_decls: &method_decls,
            closures: Vec::new(),
            closure_writes: FxHashMap::default(),
            mut_fn_params: FxHashSet::default(),
            shared_mut_fn_params: FxHashMap::default(),
            shared_mut_fn_calls: Vec::new(),
            for_item_scopes: Vec::new(),
            mut_closure_locals: Vec::new(),
            moved_mut_fns: Vec::new(),
            kind_errors: Vec::new(),
            prefix_errors: Vec::new(),
            once_calls: FxHashSet::default(),
            removed_calls: Vec::new(),
            m4a_calls: Vec::new(),
            handler_shapes: Vec::new(),
            written_pattern_bindings: FxHashSet::default(),
            pattern_read_only: FxHashMap::default(),
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
            w.place_seen.clear();
            w.dead_after.clear();
            w.ref_lets.clear();
            w.branches.clear();
            w.mut_fn_params.clear();
            w.mut_closure_locals.clear();
            w.moved_mut_fns.clear();
            let mut_fns: Vec<String> = f
                .params
                .iter()
                .filter(|p| w.is_mut_fn(&p.ty))
                .flat_map(|p| p.pattern.binding_names())
                .collect();
            w.mut_fn_params.extend(mut_fns);
            w.shared_mut_fn_params = f
                .params
                .iter()
                .filter(|p| !p.is_own)
                .filter_map(|p| {
                    let fix = match &p.ty.kind {
                        TypeKind::Ref(inner) if w.is_mut_fn(inner) => None,
                        _ if crate::d5_params::on() && w.is_mut_fn(&p.ty) => {
                            let escaping =
                                w.tc.program
                                    .escaping_fn_types
                                    .contains(&SpanKey::from_span(&p.ty.span));
                            (!escaping).then(|| FixIt {
                                span: Span {
                                    length: 0,
                                    ..p.ty.span
                                },
                                replacement: "own ".to_string(),
                            })
                        }
                        _ => return None,
                    };
                    Some((p, fix))
                })
                .flat_map(|(p, fix)| {
                    p.pattern
                        .binding_names()
                        .into_iter()
                        .map(move |n| (n, fix.clone()))
                })
                .collect();
            w.block(&f.body);
        }
        let mut conflicts = w.conflicts;
        let mut errors = w.errors;
        let kind_errors = w.kind_errors;
        let shared_mut_fn_calls = w.shared_mut_fn_calls;
        let prefix_errors = w.prefix_errors;
        let once_calls = w.once_calls;
        let read_conflicts = w.read_conflicts;
        let removed_calls = w.removed_calls;
        let m4a_calls = w.m4a_calls;
        let handler_shapes = w.handler_shapes;
        // A binding the fix makes `ref` that its arm writes through must be
        // `mut ref` instead (B-2026-10-18-28).
        let written = w.written_pattern_bindings;
        for edits in self.core_moving_scrutinees.values_mut() {
            for e in edits.iter_mut().filter(|e| written.contains(&e.offset)) {
                e.replacement = e.replacement.replace("ref ", "mut ref ");
            }
        }
        for (span, message) in removed_calls {
            self.type_error(message, span, TypeErrorKind::RemovedInCore);
        }
        for (span, message) in m4a_calls {
            self.type_error(message, span, TypeErrorKind::DeferredToM4a);
        }
        for (span, message) in handler_shapes {
            self.type_error(message, span, TypeErrorKind::TypeMismatch);
        }
        for (span, message) in kind_errors {
            self.type_error(message, span, TypeErrorKind::FnKindMismatch);
        }
        for (span, message, fix) in shared_mut_fn_calls {
            match fix {
                Some(fix) => {
                    self.type_error_with_fix_it(message, span, TypeErrorKind::FnKindMismatch, fix)
                }
                None => self.type_error(message, span, TypeErrorKind::FnKindMismatch),
            }
        }
        for (span, message, fix) in prefix_errors {
            self.type_error_with_fix_it(message, span, TypeErrorKind::CapturePrefixRemoved, fix);
        }
        let reads: Vec<_> = read_conflicts
            .into_iter()
            .filter(|(r, _)| {
                !conflicts
                    .iter()
                    .any(|(c, _, _)| r.offset >= c.offset && r.offset <= c.offset + c.length)
            })
            .map(|(r, m)| (r, m, None))
            .collect();
        conflicts.extend(reads);
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
        // A block that ends in `return` and holds no `break` or `continue`
        // leaves the function from every point in it.
        let ends = self
            .closures
            .is_empty()
            .then(|| block_return_end(b))
            .flatten();
        if let Some(end) = ends {
            self.dead_after.push(end);
        }
        self.block_inner(b);
        if ends.is_some() {
            self.dead_after.pop();
        }
    }

    fn block_inner(&mut self, b: &Block) {
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
        self.moved_mut_fns.retain(|((i, _), _)| *i < depth);
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
                } else if origin == Origin::Owned {
                    (n, Origin::OwnedPattern(sp.offset), fix.clone())
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
            // A call that hands back an owned collection (a user `keys`
            // returning a cloned `Vec[i64]`) borrows nothing, whatever its
            // name (B-2026-10-21-2).
            if self
                .node_types
                .get(&e.id)
                .is_some_and(|(t, _)| owned_collection(t))
            {
                return None;
            }
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
            unique: false,
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
                op: op @ (UnaryOp::Ref | UnaryOp::MutRef),
                operand,
            },
        ) = (&pattern.kind, &value.kind)
        else {
            return Vec::new();
        };
        let unique = matches!(op, UnaryOp::MutRef);
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
            unique,
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
            unique: false,
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
                    unique: false,
                });
            }
        }
        out
    }

    /// §5.6 / §6.2: a write to `place` while an enclosing `for` loop iterates
    /// it, a place inside it, or a place it is inside, through the same
    /// binding.
    fn check_borrowed_write(&mut self, place: &Expr, at: Span, verb: &str) {
        self.check_borrowed_write_after(place, at, at.offset, verb);
    }

    /// [`Self::check_borrowed_write`] for a write that happens once the code
    /// up to `after` has run: an assignment's right side and a call's
    /// arguments are evaluated before the place is written, so a `ref`
    /// binding last used there no longer borrows it (two-phase, as for every
    /// `mut ref` argument; B-2026-10-15-109).
    fn check_borrowed_write_after(&mut self, place: &Expr, at: Span, after: usize, verb: &str) {
        if self.borrowed.is_empty() {
            return;
        }
        let Some((root, path, handle)) = self.place_path(place) else {
            return;
        };
        let branches = &self.branches;
        let dead = &self.dead_after;
        let overlaps = |b: &Borrowed| {
            b.binding.as_ref().is_none_or(|h| {
                h.uses.iter().any(|(u, p)| {
                    *u >= after && !exclusive(p, branches) && !dead.iter().any(|d| u >= d)
                })
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
            let kw = if b.unique { "mut ref" } else { "ref" };
            format!(
                "cannot {verb} `{text}` while `{kw} {name}` borrows `{}`: `{name}` is used \
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

    /// §5.6: a read of a place while a `mut ref` local that borrows it, or a
    /// place inside it, is still used later. Asked once per projection
    /// chain, for the whole chain.
    fn check_borrowed_read(&mut self, place: &Expr) {
        let key = place as *const Expr as usize;
        if !self.place_seen.insert(key) {
            return;
        }
        let mut cur = place;
        while let ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } = &cur.kind
        {
            self.place_seen
                .insert(object.as_ref() as *const Expr as usize);
            cur = object;
        }
        if !self.borrowed.iter().any(|b| b.unique) {
            return;
        }
        let Some((root, path, _)) = self.place_path(place) else {
            return;
        };
        let after = place.span.offset;
        let branches = &self.branches;
        let dead = &self.dead_after;
        let Some(b) = self.borrowed.iter().rev().find(|b| {
            b.unique
                && b.binding.as_ref().is_some_and(|h| {
                    h.uses.iter().any(|(u, p)| {
                        *u > after && !exclusive(p, branches) && !dead.iter().any(|d| u >= d)
                    })
                })
                && b.root == root
                && b.path
                    .iter()
                    .zip(&path)
                    .all(|(x, y)| x.is_none() || y.is_none() || x == y)
        }) else {
            return;
        };
        let name = b.binding.as_ref().map_or("", |h| h.name.as_str());
        let text = place_text(place).unwrap_or_else(|| b.text.clone());
        let message = format!(
            "cannot read `{text}` while `mut ref {name}` borrows `{}`: `{name}` is used \
             after this (core-semantics.md §5.6). Read it through `{name}`, or use \
             `{name}` for the last time first",
            b.text
        );
        self.read_conflicts.push((place.span, message));
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.iter().rev().find(|(n, _, _)| n == name))
    }

    /// Whether `e` is a `Copy` value, or a reference to one.
    fn holds_a_copy(&self, e: &Expr) -> bool {
        let Some((t, _)) = self.node_types.get(&e.id) else {
            return false;
        };
        let mut t = t;
        while let Type::Ref(inner) | Type::MutRef(inner) = t {
            t = inner;
        }
        self.tc.is_copy_type_during_check(t)
    }

    fn origin_of(&self, name: &str) -> Origin {
        self.lookup(name).map_or(Origin::Owned, |b| b.1)
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let {
                is_mut,
                pattern,
                value,
                ..
            } => {
                self.expr(value);
                self.check_mut_ref_bindings(pattern, value);
                // A local bound to a read-only borrow is one too (§5.9),
                // and the same edit to the borrow's declaration fixes it.
                // A `let mut` of a `Copy` value read through a reference
                // copies it, so that local is its own (§3; B-2026-10-23-11).
                let (origin, fix) = match &value.kind {
                    ExprKind::Identifier(_) if *is_mut && self.holds_a_copy(value) => {
                        (Origin::Owned, None)
                    }
                    ExprKind::Identifier(n) => self
                        .lookup(n)
                        .map_or((Origin::Owned, None), |b| (b.1, b.2.clone())),
                    // `let mut b = self;` over a borrowed receiver copies the
                    // reference (§4.6; B-2026-10-22-33).
                    ExprKind::SelfValue if self.self_read_only => {
                        (Origin::SelfCopy, self.self_fix.clone())
                    }
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
                self.note_mut_fn_move(pattern, value);
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
                let after = value.span.offset + value.span.length;
                self.check_borrowed_write_after(target, target.span, after, "assign");
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

    /// `let g = f` with `f` a local bound to a closure that writes a capture:
    /// a `MutFn` is never `Copy` (§1.1), so `f` moves into `g`, and `g` is a
    /// `MutFn` too. Only a `let` in `f`'s own scope is counted, outside any
    /// closure, so a move on one branch never reports a use on another.
    fn note_mut_fn_move(&mut self, pattern: &Pattern, value: &Expr) {
        let (ExprKind::Identifier(x), PatternKind::Binding(name)) = (&value.kind, &pattern.kind)
        else {
            return;
        };
        if !self.closures.is_empty() || name == x {
            return;
        }
        let Root::Local(i, j) = self.root_of(x) else {
            return;
        };
        let Some(n) = self
            .mut_closure_locals
            .iter()
            .find(|(r, _)| *r == (i, j))
            .map(|(_, n)| n.clone())
        else {
            return;
        };
        // The new binding is the last one in the innermost scope; `f` must
        // live in that scope too.
        let depth = self.scopes.len() - 1;
        if i != depth {
            return;
        }
        self.moved_mut_fns.push(((i, j), name.clone()));
        if let Root::Local(gi, gj) = self.root_of(name) {
            self.mut_closure_locals.push(((gi, gj), n));
        }
    }

    /// A use of a `MutFn` local after a `let` moved it (B-2026-10-19-36).
    fn check_moved_mut_fn_use(&mut self, e: &Expr) {
        let ExprKind::Identifier(x) = &e.kind else {
            return;
        };
        let Root::Local(i, j) = self.root_of(x) else {
            return;
        };
        if self.scopes[i][j].0 != *x {
            return;
        }
        if let Some(k) = self.moved_mut_fns.iter().position(|(r, _)| *r == (i, j)) {
            let (_, into) = self.moved_mut_fns.remove(k);
            self.kind_errors.push((
                e.span,
                format!(
                    "use of moved value `{x}`: it writes a captured binding, so it is a \
                     MutFn, and a MutFn is never Copy (core-semantics §1.1), so binding it \
                     to `{into}` moved it. Call it through `{into}` from here on"
                ),
            ));
        }
    }

    fn expr(&mut self, e: &Expr) {
        self.check_moved_mut_fn_use(e);
        if matches!(
            e.kind,
            ExprKind::Identifier(_)
                | ExprKind::SelfValue
                | ExprKind::FieldAccess { .. }
                | ExprKind::TupleIndex { .. }
                | ExprKind::Index { .. }
        ) {
            self.check_borrowed_read(e);
        }
        if let ExprKind::Pipe { .. } = &e.kind {
            self.removed_calls.push((
                e.span,
                "there is no pipe operator `|>` (design.md § Function values): write \
                 `f(x)` for `x |> f`, or a method chain"
                    .to_string(),
            ));
        }
        match &e.kind {
            // Nothing after a `return` runs once its value is computed.
            ExprKind::Return(Some(v)) if self.closures.is_empty() => {
                self.dead_after.push(e.span.offset + e.span.length);
                self.expr(v);
                self.dead_after.pop();
                return;
            }
            ExprKind::MethodCall { object, method, .. }
                if matches!(&object.kind, ExprKind::Identifier(ty)
                    if self.check_m4a_call(e, ty, method)) => {}
            ExprKind::Call { callee, .. }
                if matches!(&callee.kind, ExprKind::Path { segments, .. }
                    if segments.len() == 2 && self.check_m4a_call(e, &segments[0], &segments[1])) =>
                {}
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
                let after = match &e.kind {
                    ExprKind::MethodCall {
                        args_close_span, ..
                    } if args_close_span.offset > e.span.offset => args_close_span.offset,
                    _ => e.span.offset + 1,
                };
                self.check_borrowed_write_after(object, e.span, after, &verb);
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
                self.check_shared_mut_fn_call(callee);
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
                self.for_item_scopes.push(self.scopes.len() - 1);
                self.bind(pattern, origin, fix);
                self.block(body);
                self.for_item_scopes.pop();
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
                    self.note_pattern_read_only(&arm.pattern, scrutinee);
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
                self.note_pattern_read_only(pattern, value);
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
                self.note_pattern_read_only(pattern, value);
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
            // `mut ref place` writes through the place (§5.9) and conflicts
            // with any borrow of it still in use (§5.6).
            ExprKind::Unary {
                op: UnaryOp::MutRef,
                operand,
            } => {
                self.check_through(operand, e.span, "a `mut ref` borrow through it");
                self.check_borrowed_write(operand, e.span, "borrow `mut ref`");
                self.note_capture_write(operand);
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

    /// B-2026-10-20-8: the TLS server (`Server.serve_tls`, `TlsListener`)
    /// and WebSocket (`Server.serve_ws`, `Server.serve_ws_tls`, `WebSocket`)
    /// keep their services-track (M4a) placement (docs/library/io.md), and
    /// the v2 backends do not run them, so the check refuses each entry
    /// point rather than let a program reach an internal MIR error. The TLS
    /// client (`TlsStream.connect`) is not one. Records the call and returns
    /// false, so the walk goes on as for any other call.
    fn check_m4a_call(&mut self, call: &Expr, ty: &str, method: &str) -> bool {
        if ty == "Server" && method.starts_with("serve") && method != "serve_static" {
            self.check_handler_response(call, method);
        }
        let what = match (ty, method) {
            ("Server", "serve_tls") => "the TLS server",
            ("Server", "serve_ws" | "serve_ws_tls") => "WebSocket",
            ("TlsListener", _) => "the TLS server",
            ("WebSocket", _) => "WebSocket",
            ("Base64" | "Hex" | "Url" | "Stats", _) => "",
            _ => return false,
        };
        // A local of that name, or a program's own type, is not the library's.
        if self.lookup(ty).is_some()
            || self.tc.program.items.iter().any(|it| match it {
                Item::StructDef(s) => !s.stdlib_origin && s.name == *ty,
                Item::EnumDef(e) => !e.stdlib_origin && e.name == *ty,
                _ => false,
            })
        {
            return false;
        }
        if what.is_empty() {
            self.check_removed_namespace(call, ty, method);
            return false;
        }
        self.m4a_calls.push((
            call.span,
            format!(
                "`{ty}.{method}` is not available yet: {what} comes with the services \
                 track (M4a, docs/library/io.md), and until then `karac check` refuses \
                 it. Plain TCP (`std.net`) and the HTTPS client are available now"
            ),
        ));
        false
    }

    /// B-2026-10-22-33: the legacy library's `Base64` / `Hex` / `Url`
    /// namespaces are `std.encoding`'s free functions on v2
    /// (docs/library/encoding.md), and `Stats` is the `kara-data` package's
    /// (docs/deferred.md § Numerical types), so a call of either is refused
    /// with the spelling that replaces it rather than reach the backend.
    fn check_removed_namespace(&mut self, call: &Expr, ty: &str, method: &str) {
        let instead = match (ty, method) {
            ("Base64", "encode") => "`base64_encode(bytes)`",
            ("Base64", "encode_url_safe") => "`base64url_encode(bytes)` (no padding)",
            ("Base64", "decode") => "`base64_decode(text)`",
            ("Hex", "encode") => "`hex_encode(bytes)`",
            ("Hex", "encode_upper") => "`hex_encode(bytes).to_uppercase()`",
            ("Hex", "decode") => "`hex_decode(text)`",
            ("Url", "encode") => "`percent_encode(text.bytes())`",
            ("Url", "decode") => "`percent_decode(text)` and then `String.from_utf8`",
            ("Stats", _) => {
                self.removed_calls.push((
                    call.span,
                    format!(
                        "`Stats.{method}` is not in the standard library: statistics come \
                         with the `kara-data` package after v1 (docs/deferred.md § \
                         Numerical types)"
                    ),
                ));
                return;
            }
            _ => "the free functions of `std.encoding`",
        };
        self.removed_calls.push((
            call.span,
            format!(
                "`{ty}.{method}` is removed: encodings are the free functions of \
                 `std.encoding` (docs/library/encoding.md), so write {instead} after \
                 `import std.encoding.{{...}}`"
            ),
        ));
    }

    /// A `Server.serve` handler returns a `Response`, and a program may declare
    /// its own (examples/shortener/README.md): `status: i64` and
    /// `body: String`, then optionally `headers: Vec[(String, String)]`. A
    /// struct of any other shape is refused here, where the legacy pipeline
    /// refused it in codegen (the misshaped-handler test in
    /// tests/http_client_codegen.rs).
    fn check_handler_response(&mut self, call: &Expr, method: &str) {
        let Some(own) = self.tc.program.items.iter().find_map(|it| match it {
            Item::StructDef(s) if !s.stdlib_origin && s.name == "Response" => Some(s),
            _ => None,
        }) else {
            return;
        };
        let fields: Vec<(String, String)> = own
            .fields
            .iter()
            .map(|f| (f.name.clone(), crate::formatter::render_type_expr(&f.ty)))
            .collect();
        let want = [
            ("status", "i64"),
            ("body", "String"),
            ("headers", "Vec[(String, String)]"),
        ];
        let fits = (2..=3).contains(&fields.len())
            && fields
                .iter()
                .zip(want.iter())
                .all(|((n, t), (wn, wt))| n == wn && t == wt);
        if fits {
            return;
        }
        let shape = fields
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.handler_shapes.push((
            call.span,
            format!(
                "a `Server.{method}` handler must return a `Response` with `status: i64` and \
                 `body: String` (then, optionally, `headers: Vec[(String, String)]`), but this \
                 program's `struct Response` is {{ {shape} }}"
            ),
        ));
    }

    /// Whether `name` is bound by the pattern of an enclosing bare `for`
    /// over a collection.
    fn is_for_item(&self, name: &str) -> bool {
        matches!(self.origin_of(name), Origin::ForElem)
            && self
                .scopes
                .iter()
                .rposition(|s| s.iter().any(|(n, _, _)| n == name))
                .is_some_and(|i| self.for_item_scopes.contains(&i))
    }

    /// §9.6: a `MutFn` is called only through a unique access: an owned
    /// binding, an `own` parameter or a `mut ref`. A bare parameter is a
    /// shared borrow (§4.1), as a `ref` one is, so calling either is an
    /// error. The fix declares a bare one `own` (B-2026-10-19-49).
    fn check_shared_mut_fn_call(&mut self, callee: &Expr) {
        let ExprKind::Identifier(name) = &callee.kind else {
            return;
        };
        let Some(fix) = self.shared_mut_fn_params.get(name).cloned() else {
            return;
        };
        // A local of the same name shadows the parameter.
        let innermost = self
            .scopes
            .iter()
            .rposition(|s| s.iter().any(|(n, _, _)| n == name));
        if innermost != Some(0) {
            return;
        }
        self.shared_mut_fn_calls.push((
            callee.span,
            format!(
                "`{name}` is a borrowed `MutFn`, so it cannot be called \
                 (core-semantics.md §9.6): a `MutFn` is called only through an owned \
                 binding, an `own` parameter or a `mut ref`. Declare the parameter \
                 `{name}: own MutFn(...)`"
            ),
            fix,
        ));
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
            // Every `Atomic` method takes a borrowed `self`: an atomic is
            // written through a shared reference (concurrency.md § Atomics),
            // so `swap` is not the collection method of that name
            // (B-2026-10-21-75).
            if name == "Atomic" {
                return Some(false);
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
                self.assigning = true;
                self.check_through(object, target.span, "an assignment through it");
                self.assigning = false;
            }
            ExprKind::Unary {
                op: UnaryOp::Deref,
                operand,
            } => self.check_through(operand, target.span, "an assignment through it"),
            // §5.9: the item of a bare `for` is a `ref` into the collection,
            // so assigning it is a write through that `ref`, also when it is
            // a `Copy` value that reads as one (§5.10). Accepted, it changed
            // nothing in the collection (B-2026-10-19-45). A local bound from
            // the item is its own binding, which an assignment rebinds.
            ExprKind::Identifier(n) if self.is_for_item(n) => {
                let message = self.read_only_message(target, "an assignment through it");
                let fix = self.lookup(n).and_then(|b| b.2.clone());
                self.errors.push((target.span, message, fix));
            }
            _ => {}
        }
    }

    /// Walk `place`'s projection chain from the outside in; the first step
    /// that is a reference or a handle decides.
    fn check_through(&mut self, place: &Expr, at: Span, action: &str) {
        let mut cur = place;
        loop {
            if let ExprKind::Identifier(n) = &cur.kind {
                if let Origin::OwnedPattern(at) = self.origin_of(n) {
                    self.written_pattern_bindings.insert(at);
                }
            }
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
                ExprKind::Identifier(n) => {
                    if let Some((ro, text)) = self.bound_from_read_only(cur) {
                        let through = format!("through `{n}` (bound from `{text}`)");
                        let action = if action.contains(&format!("`{n}.")) {
                            format!("{action}, with `{n}` bound from `{text}`,")
                        } else if action.contains("through it") {
                            action.replace("through it", &through)
                        } else {
                            format!("{action} {through}")
                        };
                        let message = self.read_only_message_as(&ro.at, &action, ro.origin);
                        self.errors.push((at, message, ro.fix));
                    }
                    return;
                }
                _ => return,
            };
        }
    }

    /// Where `place` is read-only, if it is: `check_through`'s walk without
    /// its effects.
    fn read_only_step(&self, place: &Expr) -> Option<ReadOnlyAt> {
        let mut cur = place;
        loop {
            match self.step(cur) {
                Step::ReadOnly => {
                    let (origin, fix) = match &cur.kind {
                        ExprKind::SelfValue => (Origin::Owned, self.self_fix.clone()),
                        ExprKind::Identifier(n) if n == "self" => {
                            (Origin::Owned, self.self_fix.clone())
                        }
                        ExprKind::Identifier(n) => {
                            (self.origin_of(n), self.lookup(n).and_then(|b| b.2.clone()))
                        }
                        _ => (Origin::Owned, None),
                    };
                    return Some(ReadOnlyAt {
                        at: cur.clone(),
                        origin,
                        fix,
                    });
                }
                Step::Writable => return None,
                Step::Continue => {}
            }
            cur = match &cur.kind {
                ExprKind::FieldAccess { object, .. }
                | ExprKind::TupleIndex { object, .. }
                | ExprKind::Index { object, .. } => object,
                ExprKind::Identifier(_) => return self.bound_from_read_only(cur).map(|r| r.0),
                _ => return None,
            };
        }
    }

    /// The read-only place a non-`Copy` plain pattern binding `e` was bound
    /// from, and that place as written.
    fn bound_from_read_only(&self, e: &Expr) -> Option<(ReadOnlyAt, String)> {
        let ExprKind::Identifier(n) = &e.kind else {
            return None;
        };
        let Origin::OwnedPattern(at) = self.origin_of(n) else {
            return None;
        };
        if self.holds_a_copy(e) {
            return None;
        }
        self.pattern_read_only.get(&at).cloned()
    }

    /// Record, for each plain binding of `pattern`, the read-only place
    /// `scrutinee` is reached through, before the bindings are in scope.
    fn note_pattern_read_only(&mut self, pattern: &Pattern, scrutinee: &Expr) {
        let Some(text) = place_text(scrutinee) else {
            return;
        };
        let Some(ro) = self.read_only_step(scrutinee) else {
            return;
        };
        for (_, sp) in pattern.binding_name_spans() {
            self.pattern_read_only
                .insert(sp.offset, (ro.clone(), text.clone()));
        }
    }

    /// The error for a write through `at`, the read-only step, worded by
    /// where the borrow came from so the fix can be named.
    fn read_only_message(&self, at: &Expr, action: &str) -> String {
        let origin = match &at.kind {
            ExprKind::Identifier(n) => self.origin_of(n),
            _ => Origin::Owned,
        };
        self.read_only_message_as(at, action, origin)
    }

    /// `read_only_message` for a binding whose origin is `origin`.
    fn read_only_message_as(&self, at: &Expr, action: &str, origin: Origin) -> String {
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
            ExprKind::Identifier(n)
                if matches!(
                    self.node_types.get(&at.id),
                    Some((Type::Slice { mutable: false, .. }, _))
                ) =>
            {
                let ty = type_display(&self.node_types[&at.id].0);
                let action = action.replace("through it", &format!("through `{n}`"));
                return format!(
                    "`{n}` is a read-only `{ty}`, so {action} cannot modify it \
                     (library/collections.md: changing a slice in place needs a \
                     `mut Slice[T]`). Write through the sequence itself, pass \
                     `mut v[a..b]` to a `mut Slice[T]` parameter, or copy the \
                     elements with `.to_vec()`"
                );
            }
            ExprKind::Identifier(n) => (n.as_str(), origin),
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
            Origin::SelfCopy => format!(
                "`{name}` copies the receiver `ref self` (core-semantics.md §4.6), so \
                 {action} cannot modify it (§5.9). Declare the method `mut ref self`"
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
            Origin::Owned | Origin::OwnedPattern(_) | Origin::MutRefPattern => format!(
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
                Origin::Owned | Origin::OwnedPattern(_) => false,
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
        // A `Slice[T]` is a read-only view; only a `mut Slice[T]` writes
        // (library/collections.md, "Changing in place"; B-2026-10-15-61).
        if let (true, Type::Slice { mutable, .. }) = (self.assigning, ty) {
            return if *mutable {
                Step::Writable
            } else {
                Step::ReadOnly
            };
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

/// The end of the `return` that ends `b`, when `b` holds no `break` or
/// `continue` that could leave it another way.
fn block_return_end(b: &Block) -> Option<usize> {
    let last = match (&b.final_expr, b.stmts.last()) {
        (Some(e), _) => e.as_ref(),
        (None, Some(s)) => match &s.kind {
            StmtKind::Expr(e) => e,
            _ => return None,
        },
        (None, None) => return None,
    };
    if !matches!(last.kind, ExprKind::Return(_)) {
        return None;
    }
    let mut jumps = false;
    crate::index_disjoint::for_each_block_child(b, &mut |c| jumps |= child_jumps(c));
    (!jumps).then_some(last.span.offset + last.span.length)
}

/// Whether a `break` or `continue` is anywhere inside, closures aside.
fn child_jumps(c: Child<'_>) -> bool {
    match c {
        Child::Expr(e) => match &e.kind {
            ExprKind::Break { .. } | ExprKind::Continue { .. } => true,
            ExprKind::Closure { .. } => false,
            _ => {
                let mut j = false;
                for_each_child_public(e, &mut |c| j |= child_jumps(c));
                j
            }
        },
        Child::Block(b) => {
            let mut j = false;
            crate::index_disjoint::for_each_block_child(b, &mut |c| j |= child_jumps(c));
            j
        }
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

/// An owned `Vec` / `VecDeque` / `Set` / `Map` / `String` whose type mentions
/// no reference, so no part of it can borrow from anything (B-2026-10-21-2).
fn owned_collection(t: &Type) -> bool {
    fn ref_free(t: &Type) -> bool {
        match t {
            Type::Ref(_) | Type::MutRef(_) => false,
            Type::Named { name, args } => name != "StringSlice" && args.iter().all(ref_free),
            Type::Tuple(ts) => ts.iter().all(ref_free),
            Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char | Type::Str => {
                true
            }
            _ => false,
        }
    }
    match t {
        Type::Str => true,
        Type::Named { name, args } => {
            matches!(
                name.as_str(),
                "Vec" | "VecDeque" | "Set" | "SortedSet" | "Map" | "SortedMap"
            ) && args.iter().all(ref_free)
        }
        _ => false,
    }
}
