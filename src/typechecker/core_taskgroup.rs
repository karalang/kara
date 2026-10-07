//! v2 core §9.5 (`docs/core-semantics.md`): `TaskGroup` tasks borrow.
//!
//! The closure passed to `TaskGroup.spawn` is not escaping, so it captures
//! by `ref` / `mut ref` as §9.1 infers, and the group then borrows the
//! closure's origins until it drops (dropping a group joins its tasks). So,
//! while the group lives:
//!
//! - a captured place may not be written or moved;
//! - a place one task captures by `mut ref` may not be captured by another;
//! - every origin must outlive the group: a place declared after it in the
//!   same scope drops first, which is an error.
//!
//! A source-order pass over each function body, run by the strict commands
//! only. The group lives from its `let` to the end of the enclosing block.

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

/// One binding in scope: its name and where it was declared, which tells
/// shadowed bindings apart.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Decl {
    offset: usize,
}

struct Group {
    name: String,
    decl: Decl,
    /// The group's block ends here; the group drops there.
    end: usize,
}

/// A spawn's capture of an outer place.
struct Capture {
    group: usize,
    name: String,
    decl: Decl,
    spawn: Span,
    mutable: bool,
    in_loop: bool,
}

/// A write or move of a place, for checking against the live borrows.
struct Write {
    name: String,
    decl: Decl,
    span: Span,
    what: &'static str,
}

struct Walk<'t, 'a> {
    tc: &'t TypeChecker<'a>,
    node_types: &'t FxHashMap<crate::ids::NodeId, (Type, u32)>,
    self_modes: &'t FxHashMap<String, SelfParam>,
    scopes: Vec<Vec<(String, Decl)>>,
    /// Each declaration's span, by its offset.
    decl_spans: FxHashMap<usize, Span>,
    /// Each open group's index, with the scope depth it was declared at.
    open_groups: Vec<(usize, usize)>,
    groups: Vec<Group>,
    captures: Vec<Capture>,
    writes: Vec<Write>,
    errors: Vec<(Span, String)>,
    loop_depth: usize,
}

impl<'a> TypeChecker<'a> {
    pub(super) fn emit_core_taskgroup_errors(
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
        let mut errors = Vec::new();
        for f in fns.into_iter().filter(|f| !f.stdlib_origin) {
            let mut w = Walk {
                tc: self,
                node_types,
                self_modes: &self_modes,
                scopes: vec![f
                    .params
                    .iter()
                    .flat_map(|p| p.pattern.binding_names())
                    .map(|n| (n, Decl { offset: 0 }))
                    .collect()],
                decl_spans: FxHashMap::default(),
                open_groups: Vec::new(),
                groups: Vec::new(),
                captures: Vec::new(),
                writes: Vec::new(),
                errors: Vec::new(),
                loop_depth: 0,
            };
            w.block(&f.body);
            w.finish();
            errors.extend(w.errors);
        }
        for (span, message) in errors {
            self.type_error(message, span, TypeErrorKind::TypeMismatch);
        }
    }
}

impl Walk<'_, '_> {
    fn lookup(&self, name: &str) -> Option<Decl> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.iter().rev().find(|(n, _)| n == name).map(|(_, d)| *d))
    }

    fn declare(&mut self, name: String, span: Span) {
        self.decl_spans.insert(span.offset, span);
        if let Some(s) = self.scopes.last_mut() {
            s.push((
                name,
                Decl {
                    offset: span.offset,
                },
            ));
        }
    }

    fn block(&mut self, b: &Block) {
        self.scopes.push(Vec::new());
        let depth = self.scopes.len();
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(e) = &b.final_expr {
            self.expr(e);
        }
        // Groups declared in this block drop at its end.
        let end = b.span.offset + b.span.length;
        while let Some(&(g, d)) = self.open_groups.last() {
            if d < depth {
                break;
            }
            self.groups[g].end = end;
            self.open_groups.pop();
        }
        self.scopes.pop();
    }

    fn is_taskgroup_new(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::MethodCall { object, method, .. } => {
                method == "new"
                    && matches!(&object.kind, ExprKind::Identifier(n) if n == "TaskGroup")
            }
            ExprKind::Call { callee, .. } => matches!(
                &callee.kind,
                ExprKind::Path { segments, .. } if segments.len() == 2 && segments[0] == "TaskGroup" && segments[1] == "new"
            ),
            _ => false,
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let { pattern, value, .. } => {
                self.expr(value);
                if let ExprKind::Identifier(n) = &value.kind {
                    self.moved(n, value);
                }
                let names = pattern.binding_names();
                for n in names {
                    self.declare(n, pattern.span);
                }
                if let PatternKind::Binding(name) = &pattern.kind {
                    if Self::is_taskgroup_new(value) {
                        let g = self.groups.len();
                        self.groups.push(Group {
                            name: name.clone(),
                            decl: Decl {
                                offset: pattern.span.offset,
                            },
                            end: usize::MAX,
                        });
                        self.open_groups.push((g, self.scopes.len()));
                    }
                }
            }
            StmtKind::LetUninit {
                name, name_span, ..
            } => self.declare(name.clone(), *name_span),
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.expr(value);
                self.block(else_block);
                for n in pattern.binding_names() {
                    self.declare(n, pattern.span);
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.block(body),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.expr(value);
                self.expr(target);
                if let Some(root) = place_root(target) {
                    self.write(root, target.span, "written");
                }
            }
            StmtKind::MultiAssign { targets, values } => {
                for e in values.iter().chain(targets) {
                    self.expr(e);
                }
            }
            StmtKind::Expr(e) => self.expr(e),
        }
    }

    fn write(&mut self, name: &str, span: Span, what: &'static str) {
        if let Some(decl) = self.lookup(name) {
            self.writes.push(Write {
                name: name.to_string(),
                decl,
                span,
                what,
            });
        }
    }

    /// `let x = name` moves `name` unless it is `Copy` or a handle.
    fn moved(&mut self, name: &str, e: &Expr) {
        let Some((ty, _)) = self.node_types.get(&e.id) else {
            return;
        };
        if self.tc.is_copy_type_during_check(ty) || self.tc.copy_is_only_an_rc_retain(ty) {
            return;
        }
        self.write(name, e.span, "moved");
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.expr(object);
                let mut spawned = false;
                if method == "spawn" {
                    if let ExprKind::Identifier(g) = &object.kind {
                        let decl = self.lookup(g);
                        let group = self.open_groups.iter().map(|(i, _)| *i).find(|i| {
                            Some(self.groups[*i].decl) == decl && &self.groups[*i].name == g
                        });
                        if let (Some(group), Some(arg)) = (group, args.first()) {
                            if let ExprKind::Closure { params, body, .. } = &arg.value.kind {
                                // The body's writes are the capture itself,
                                // not writes while the group borrows.
                                self.spawn(group, e.span, params, body);
                                spawned = true;
                            }
                        }
                    }
                }
                for a in args.iter().skip(usize::from(spawned)) {
                    self.expr(&a.value);
                }
                if let Some(root) = place_root(object) {
                    if self.method_writes_receiver(e, method) {
                        self.write(root, object.span, "written");
                    }
                }
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                for a in args {
                    self.expr(&a.value);
                    if a.mut_marker {
                        if let Some(root) = place_root(&a.value) {
                            self.write(root, a.value.span, "written");
                        }
                    }
                }
            }
            ExprKind::Closure { params, body, .. } => {
                self.scopes.push(
                    params
                        .iter()
                        .flat_map(|p| p.pattern.binding_names())
                        .map(|n| (n, Decl { offset: usize::MAX }))
                        .collect(),
                );
                self.expr(body);
                self.scopes.pop();
            }
            ExprKind::Block(b) => self.block(b),
            ExprKind::While {
                condition, body, ..
            } => {
                self.expr(condition);
                self.loop_depth += 1;
                self.block(body);
                self.loop_depth -= 1;
            }
            _ => {
                let looping = matches!(
                    e.kind,
                    ExprKind::For { .. } | ExprKind::Loop { .. } | ExprKind::WhileLet { .. }
                );
                self.loop_depth += usize::from(looping);
                let mut kids: Vec<Child<'_>> = Vec::new();
                for_each_child_public(e, &mut |c| kids.push(c));
                for c in kids {
                    match c {
                        Child::Expr(x) => self.expr(x),
                        Child::Block(b) => self.block(b),
                    }
                }
                self.loop_depth -= usize::from(looping);
            }
        }
    }

    fn method_writes_receiver(&self, call: &Expr, method: &str) -> bool {
        let key = self
            .tc
            .method_callee_types
            .get(&SpanKey::from_span(&call.span));
        match key.and_then(|k| self.self_modes.get(k)) {
            Some(SelfParam::MutRef) => true,
            Some(_) => false,
            None => MUTATING_BUILTINS.contains(&method),
        }
    }

    /// Record what `g.spawn(|| body)` captures: each outer binding the body
    /// names, `mut` when the body writes it.
    fn spawn(&mut self, group: usize, span: Span, params: &[ClosureParam], body: &Expr) {
        let mut inner = Walk {
            tc: self.tc,
            node_types: self.node_types,
            self_modes: self.self_modes,
            scopes: {
                let mut sc = self.scopes.clone();
                sc.push(
                    params
                        .iter()
                        .flat_map(|p| p.pattern.binding_names())
                        .map(|n| (n, Decl { offset: usize::MAX }))
                        .collect(),
                );
                sc
            },
            decl_spans: FxHashMap::default(),
            open_groups: Vec::new(),
            groups: Vec::new(),
            captures: Vec::new(),
            writes: Vec::new(),
            errors: Vec::new(),
            loop_depth: 0,
        };
        inner.expr(body);
        let written: FxHashSet<String> = inner
            .writes
            .iter()
            .filter(|w| w.decl.offset != usize::MAX)
            .map(|w| w.name.clone())
            .collect();
        let mut names: Vec<(String, Expr)> = Vec::new();
        free_identifiers(body, &mut names);
        // A capture the body moves is captured by move (§9.1), not borrowed.
        let mut moved: FxHashSet<String> = FxHashSet::default();
        self.moved_in(body, true, &mut moved);
        let mut seen = FxHashSet::default();
        for (name, ident) in names {
            if !seen.insert(name.clone()) || moved.contains(&name) {
                continue;
            }
            let Some(decl) = self.lookup(&name) else {
                continue;
            };
            if let Some((ty, _)) = self.node_types.get(&ident.id) {
                if self.tc.is_copy_type_during_check(ty) && !written.contains(&name) {
                    continue;
                }
            }
            self.captures.push(Capture {
                group,
                mutable: written.contains(&name),
                name,
                decl,
                spawn: span,
                in_loop: self.loop_depth > 0,
            });
        }
    }

    /// Names `e` moves by value: passed to an owned parameter of a known
    /// function, stored into a collection, bound by `let`, or (`tail`) left
    /// as the closure's result.
    fn moved_in(&self, e: &Expr, tail: bool, out: &mut FxHashSet<String>) {
        let ident = |x: &Expr| match &x.kind {
            ExprKind::Identifier(n) => Some(n.clone()),
            _ => None,
        };
        match &e.kind {
            ExprKind::Identifier(n) if tail => {
                out.insert(n.clone());
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Identifier(f) = &callee.kind {
                    if let Some(sig) = self.tc.env.functions.get(f) {
                        for (a, p) in args.iter().zip(&sig.params) {
                            if !matches!(p, Type::Ref(_) | Type::MutRef(_)) {
                                if let Some(n) = ident(&a.value) {
                                    out.insert(n);
                                }
                            }
                        }
                    }
                }
            }
            ExprKind::MethodCall { method, args, .. }
                if matches!(
                    method.as_str(),
                    "push" | "push_back" | "push_front" | "insert"
                ) =>
            {
                for a in args {
                    if let Some(n) = ident(&a.value) {
                        out.insert(n);
                    }
                }
            }
            ExprKind::Block(b) => {
                for s in &b.stmts {
                    match &s.kind {
                        StmtKind::Let { value, .. } => {
                            if let Some(n) = ident(value) {
                                out.insert(n);
                            }
                            self.moved_in(value, false, out);
                        }
                        StmtKind::Expr(x) => self.moved_in(x, false, out),
                        _ => {}
                    }
                }
                if let Some(t) = &b.final_expr {
                    self.moved_in(t, tail, out);
                }
                return;
            }
            _ => {}
        }
        let mut kids: Vec<Child<'_>> = Vec::new();
        for_each_child_public(e, &mut |c| kids.push(c));
        for c in kids {
            match c {
                Child::Expr(x) => self.moved_in(x, false, out),
                Child::Block(b) => {
                    for s in &b.stmts {
                        match &s.kind {
                            StmtKind::Let { value, .. } | StmtKind::Expr(value) => {
                                self.moved_in(value, false, out)
                            }
                            _ => {}
                        }
                    }
                    if let Some(t) = &b.final_expr {
                        self.moved_in(t, false, out);
                    }
                }
            }
        }
    }

    fn finish(&mut self) {
        let mut seen: FxHashSet<(usize, usize)> = FxHashSet::default();
        for c in &self.captures {
            let g = &self.groups[c.group];
            if c.decl.offset > g.decl.offset
                && c.decl.offset != usize::MAX
                && seen.insert((c.decl.offset, c.group))
            {
                let at = self
                    .decl_spans
                    .get(&c.decl.offset)
                    .copied()
                    .unwrap_or(c.spawn);
                self.errors.push((
                    at,
                    format!(
                        "`{}` drops before `{}`, but a task of `{}` borrows it (spawned at \
                         line {}). Declare `{}` before `{}`, or put the group in an inner \
                         block (core-semantics.md §9.5)",
                        c.name, g.name, g.name, c.spawn.line, c.name, g.name
                    ),
                ));
            }
            for w in &self.writes {
                if w.decl == c.decl && w.span.offset > c.spawn.offset && w.span.offset < g.end {
                    self.errors.push((
                        w.span,
                        format!(
                            "`{}` is {} while a task of `{}` borrows it (spawned at line {}; \
                             the borrow lasts until `{}` drops, core-semantics.md §9.5)",
                            w.name, w.what, g.name, c.spawn.line, g.name
                        ),
                    ));
                }
            }
        }
        // A `mut ref` capture is exclusive: no other task of the group may
        // capture the same place, and a spawn in a loop makes many tasks.
        for (i, c) in self.captures.iter().enumerate() {
            if !c.mutable {
                continue;
            }
            let other = self
                .captures
                .iter()
                .enumerate()
                .any(|(j, d)| j != i && d.group == c.group && d.decl == c.decl);
            if other || c.in_loop {
                self.errors.push((
                    c.spawn,
                    format!(
                        "a task of `{}` captures `{}` by `mut ref`, which no other task of the \
                         group may capture{} (core-semantics.md §9.5)",
                        self.groups[c.group].name,
                        c.name,
                        if c.in_loop && !other {
                            ": this spawn runs in a loop"
                        } else {
                            ""
                        }
                    ),
                ));
            }
        }
        self.errors.sort_by_key(|(s, _)| s.offset);
        self.errors
            .dedup_by(|a, b| a.0.offset == b.0.offset && a.1 == b.1);
    }
}

/// The binding a place expression is rooted at.
fn place_root(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_root(object),
        _ => None,
    }
}

/// Every identifier the expression names, with its node, in source order.
fn free_identifiers(e: &Expr, out: &mut Vec<(String, Expr)>) {
    if let ExprKind::Identifier(n) = &e.kind {
        out.push((n.clone(), e.clone()));
        return;
    }
    let mut kids: Vec<Child<'_>> = Vec::new();
    for_each_child_public(e, &mut |c| kids.push(c));
    for c in kids {
        match c {
            Child::Expr(x) => free_identifiers(x, out),
            Child::Block(b) => {
                for s in &b.stmts {
                    match &s.kind {
                        StmtKind::Let { value, .. } | StmtKind::Expr(value) => {
                            free_identifiers(value, out)
                        }
                        StmtKind::Assign { target, value }
                        | StmtKind::CompoundAssign { target, value, .. } => {
                            free_identifiers(target, out);
                            free_identifiers(value, out);
                        }
                        _ => {}
                    }
                }
                if let Some(t) = &b.final_expr {
                    free_identifiers(t, out);
                }
            }
        }
    }
}
