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
}

/// Per parameter: `Some(true)` for `escaping Fn(..)`, `Some(false)` for a
/// non-escaping function type, `None` for any other type.
type Sig = Vec<Option<bool>>;

#[derive(Clone, Copy)]
enum Bound {
    NonEscParam,
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

const STORE_METHODS: &[&str] = &["push", "push_back", "push_front", "insert", "set", "append"];

pub fn analyze(program: &Program) -> CoreEscape {
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
    for item in &program.items {
        match item {
            Item::Function(f) => {
                sigs.insert(f.name.clone(), sig_of(f));
                fns.push(f);
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
    // The standard library is checked by its own rules, not the user's.
    for f in fns.into_iter().filter(|f| !f.stdlib_origin) {
        let mut w = Walker {
            sigs: &sigs,
            esc,
            scopes: Vec::new(),
            out: &mut out,
        };
        w.walk_fn(f);
    }
    out
}

struct Walker<'a> {
    sigs: &'a FxHashMap<String, Sig>,
    esc: &'a FxHashSet<SpanKey>,
    scopes: Vec<FxHashMap<String, Bound>>,
    out: &'a mut CoreEscape,
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
                            Bound::Closure(SpanKey::from_span(&value.span))
                        }
                        ExprKind::Identifier(n) => match self.lookup(n) {
                            Some(Bound::Closure(k)) => Bound::Closure(k),
                            _ => {
                                self.walk_expr(value, Pos::Bind);
                                Bound::Other
                            }
                        },
                        _ => {
                            self.walk_expr(value, Pos::Other);
                            Bound::Other
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

    fn walk_expr(&mut self, e: &Expr, pos: Pos) {
        match &e.kind {
            ExprKind::Identifier(n) => match (self.lookup(n), pos) {
                (Some(Bound::Closure(k)), Pos::Escape(_)) => {
                    self.out.escaping_closures.insert(k);
                }
                (Some(Bound::NonEscParam), Pos::Escape(why)) => self.non_escaping(e, n, why),
                (Some(Bound::NonEscParam), Pos::Bind) => {
                    self.non_escaping(e, n, "bound to a variable")
                }
                _ => {}
            },
            ExprKind::Closure { params, body, .. } => {
                if matches!(pos, Pos::Escape(_)) {
                    self.out
                        .escaping_closures
                        .insert(SpanKey::from_span(&e.span));
                }
                let mut scope = FxHashMap::default();
                for p in params {
                    for n in p.pattern.binding_names() {
                        scope.insert(n, Bound::Other);
                    }
                }
                self.scopes.push(scope);
                self.walk_expr(body, Pos::Other);
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
                        } else {
                            let sig = if local { None } else { self.sigs.get(name) };
                            self.walk_args(args, sig, None);
                        }
                    }
                    ExprKind::Path { segments, .. } => {
                        let key = segments.join(".");
                        match self.sigs.get(&key) {
                            Some(sig) => self.walk_args(args, Some(sig), None),
                            // An enum variant constructor: the payload goes
                            // where the value goes.
                            None => self.walk_args(args, None, value_of(pos)),
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
            ExprKind::ArrayLiteral(es) => {
                for x in es {
                    self.walk_expr(x, Pos::Escape("stored in a collection"));
                }
            }
            ExprKind::Tuple(es) => {
                for x in es {
                    self.walk_expr(x, tail_of(pos));
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

    fn non_escaping(&mut self, e: &Expr, name: &str, why: &str) {
        self.out.errors.push((
            e.span,
            format!(
                "parameter `{name}` is a non-escaping function value and cannot be {why}: \
                 a function-typed parameter may only be called or passed to another \
                 non-escaping parameter. Declare it `{name}: escaping Fn(...)` to store or \
                 return it (core-semantics.md §9.3)"
            ),
        ));
    }
}

/// The position of a value that flows on into `pos` unchanged (a tail, a
/// tuple element). Binding a tuple is not binding its parts.
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
