// src/default_args.rs
//! Call-site default-parameter fill — the pre-resolve pass that makes
//! `fn f(a: T, b: U = <default>)` callable as `f(x)` (B-2026-08-17-19).
//!
//! design.md § Default Parameter Values declares the call-site behaviour
//! unconditionally — omit trailing defaulted arguments, or skip over them
//! with labeled arguments (`create_server("0.0.0.0", max_connections: 100)`)
//! — but until this pass the stored defaults were INERT: the declaration
//! half (syntax, trailing-only rule, const validation) shipped with nothing
//! ever consuming a default at a call, so omitting one was an arity error
//! and label-skipping a label-mismatch error.
//!
//! ## Where the fill happens, and why
//!
//! This is an AST-rewriting pass run from [`crate::desugar::desugar_program`]
//! (pipeline: between parse and resolve), LAST — after trait default-method
//! bodies and `#[multiversion]` variants are synthesized, so calls inside
//! synthesized bodies are filled too. Each call to a defaulted free function
//! is rewritten into the complete argument list, with the omitted arguments
//! spliced in as clones of the declaration's default expressions. Every
//! later phase — resolver, typechecker, effect/ownership checkers, and all
//! three backends (interpreter, JIT, AOT) — then sees an ordinary full-arity
//! call, which is what makes the three-surface parity rule hold by
//! construction rather than by three hand-kept implementations. Cloning per
//! call site is also exactly the spec's evaluation order: "Defaults are
//! evaluated per call, not once at declaration time."
//!
//! The cloned default keeps its DECLARATION span (the established pattern
//! for synthesized bodies — trait default methods and `#[multiversion]`
//! variants clone spans the same way). Distinct defaults keep distinct
//! spans, and a given default has the same type at every call site, so
//! span-keyed side tables stay consistent.
//!
//! ## Scope of the rewrite
//!
//! A call is filled only when ALL of the following hold; otherwise it is
//! left untouched and the typechecker reports exactly what it reports today
//! (there is no shape this pass turns from an error into a worse error —
//! failure to fill always falls back to the existing diagnostics):
//!
//! * The callee is a BARE IDENTIFIER naming a top-level `fn` with at least
//!   one defaulted parameter, and no local binding (param, `let`, pattern,
//!   closure param, …) shadows that name at the call site — a shadowing
//!   closure value is called with the closure's own arity, and defaults
//!   never travel with function VALUES (`let g = f; g(x)` does not fill).
//!   Method / associated-function calls and module-qualified `Path` callees
//!   are out of scope for this slice.
//! * Every omitted parameter (skipped by a label or missing from the tail)
//!   actually has a default.
//! * The user's own argument list is well-formed enough to interpret:
//!   unlabeled-after-labeled stays untouched so the contiguity diagnostic
//!   fires on the author's shape, and an unknown label stays untouched so
//!   the label-mismatch diagnostic does.
//!
//! Spliced arguments carry their parameter's name as an argument LABEL, so
//! a fill interleaved with the author's labeled arguments still satisfies
//! the "labeled arguments are contiguous and in declaration order" rule.
//! When a defaulted parameter has no single name (a destructuring pattern),
//! its fill cannot be labeled; such a fill is only performed when the final
//! argument list carries no labels at all.

use std::collections::{HashMap, HashSet};

use crate::ast::*;

/// Per-function default info the filler needs, cloned out of the item so the
/// walk can mutate the program freely.
struct FnDefaultInfo {
    /// One entry per parameter: `Some(name)` for a simple binding (usable as
    /// an argument label), `None` for a destructuring pattern.
    names: Vec<Option<String>>,
    /// One entry per parameter: the default expression, if declared.
    defaults: Vec<Option<Expr>>,
}

pub(crate) fn fill_default_args_in_program(program: &mut Program) {
    let mut table: HashMap<String, FnDefaultInfo> = HashMap::new();
    for item in &program.items {
        if let Item::Function(f) = item {
            if f.params.iter().any(|p| p.default_value.is_some()) {
                table.insert(
                    f.name.clone(),
                    FnDefaultInfo {
                        names: f
                            .params
                            .iter()
                            .map(|p| p.name().map(|s| s.to_string()))
                            .collect(),
                        defaults: f.params.iter().map(|p| p.default_value.clone()).collect(),
                    },
                );
            }
        }
    }
    if table.is_empty() {
        return;
    }
    let mut filler = Filler {
        table,
        scopes: Vec::new(),
    };
    for item in &mut program.items {
        match item {
            Item::Function(f) => filler.walk_function(&f.params, &mut f.body),
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(m) = it {
                        filler.walk_function(&m.params, &mut m.body);
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &mut t.items {
                    if let TraitItem::Method(m) = it {
                        if let Some(body) = &mut m.body {
                            let params = m.params.clone();
                            filler.walk_function(&params, body);
                        }
                    }
                }
            }
            Item::TestCase(tc) => filler.walk_function(&[], &mut tc.body),
            _ => {}
        }
    }
}

struct Filler {
    table: HashMap<String, FnDefaultInfo>,
    /// Lexical scope stack of locally-bound names. A callee identifier that
    /// appears here refers to a local value, not the top-level fn — no fill.
    scopes: Vec<HashSet<String>>,
}

impl Filler {
    fn walk_function(&mut self, params: &[Param], body: &mut Block) {
        let mut scope = HashSet::new();
        for p in params {
            scope.extend(p.pattern.binding_names());
        }
        self.scopes.push(scope);
        self.walk_block(body);
        self.scopes.pop();
    }

    fn is_shadowed(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    fn bind(&mut self, names: Vec<String>) {
        if let Some(top) = self.scopes.last_mut() {
            top.extend(names);
        }
    }

    fn walk_block(&mut self, block: &mut Block) {
        self.scopes.push(HashSet::new());
        for stmt in &mut block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(e) = &mut block.final_expr {
            self.walk_expr(e);
        }
        self.scopes.pop();
    }

    fn walk_stmt(&mut self, stmt: &mut Stmt) {
        match &mut stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                self.walk_expr(value);
                // The binding is visible only AFTER its own initializer, so a
                // `let f = …; f(x)` pair shadows the second statement but a
                // self-referential `let f = f(…)` initializer still sees the
                // top-level fn.
                self.bind(pattern.binding_names());
            }
            StmtKind::LetUninit { name, .. } => self.bind(vec![name.clone()]),
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block(else_block);
                self.bind(pattern.binding_names());
            }
            StmtKind::Defer { body } => self.walk_block(body),
            StmtKind::ErrDefer { body, .. } => self.walk_block(body),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::MultiAssign { targets, values } => {
                // Removed by the multi-assign desugar before this pass runs;
                // walked anyway so pass ordering is not load-bearing here.
                for t in targets {
                    self.walk_expr(t);
                }
                for v in values {
                    self.walk_expr(v);
                }
            }
            StmtKind::Expr(e) => self.walk_expr(e),
        }
    }

    fn walk_expr(&mut self, expr: &mut Expr) {
        // Recurse into children first (an inner call inside an argument is
        // filled before the outer call is considered), then attempt the fill
        // on this node.
        match &mut expr.kind {
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(..)
            | ExprKind::ByteLit(..)
            | ExprKind::ByteStringLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::MultiStringLit(..)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(..)
            | ExprKind::Identifier(..)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::Continue { .. }
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    if let ParsedInterpolationPart::Expr(inner, _) = p {
                        self.walk_expr(inner);
                    }
                }
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Par(b)
            | ExprKind::Seq(b)
            | ExprKind::Try(b)
            | ExprKind::Unsafe(b)
            | ExprKind::LabeledBlock { body: b, .. }
            | ExprKind::Loop { body: b, .. }
            | ExprKind::Lock { body: b, .. } => self.walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_block(then_block);
                if let Some(e) = else_branch {
                    self.walk_expr(e);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.walk_expr(value);
                self.scopes
                    .push(pattern.binding_names().into_iter().collect());
                self.walk_block(then_block);
                self.scopes.pop();
                if let Some(e) = else_branch {
                    self.walk_expr(e);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition);
                self.walk_block(body);
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                self.walk_expr(value);
                self.scopes
                    .push(pattern.binding_names().into_iter().collect());
                self.walk_block(body);
                self.scopes.pop();
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.walk_expr(iterable);
                self.scopes
                    .push(pattern.binding_names().into_iter().collect());
                self.walk_block(body);
                self.scopes.pop();
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    self.scopes
                        .push(arm.pattern.binding_names().into_iter().collect());
                    if let Some(g) = &mut arm.guard {
                        self.walk_expr(g);
                    }
                    self.walk_expr(&mut arm.body);
                    self.scopes.pop();
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.walk_expr(object);
                for a in args {
                    self.walk_expr(&mut a.value);
                }
            }
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee);
                for a in args.iter_mut() {
                    self.walk_expr(&mut a.value);
                }
                if let ExprKind::Identifier(name) = &callee.kind {
                    if !self.is_shadowed(name) {
                        if let Some(info) = self.table.get(name) {
                            if let Some(filled) = try_fill(args, info) {
                                *args = filled;
                            }
                        }
                    }
                }
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object);
                if let Some(args) = args {
                    for a in args {
                        self.walk_expr(&mut a.value);
                    }
                }
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Binary { left, right, .. }
            | ExprKind::NilCoalesce { left, right }
            | ExprKind::Pipe { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Question(inner) => self.walk_expr(inner),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object)
            }
            ExprKind::Cast { expr: inner, .. } => self.walk_expr(inner),
            ExprKind::Closure { params, body, .. } => {
                let mut scope = HashSet::new();
                for p in params.iter() {
                    scope.extend(p.pattern.binding_names());
                }
                self.scopes.push(scope);
                self.walk_expr(body);
                self.scopes.pop();
            }
            ExprKind::Return(opt) => {
                if let Some(inner) = opt {
                    self.walk_expr(inner);
                }
            }
            ExprKind::Break { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr(v);
                }
            }
            ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
                for x in exprs {
                    self.walk_expr(x);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for x in items {
                    self.walk_expr(x);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.walk_expr(k);
                    self.walk_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_expr(&mut f.value);
                }
                if let Some(sp) = spread {
                    self.walk_expr(sp);
                }
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(e) = end {
                    self.walk_expr(e);
                }
            }
            ExprKind::Providers { bindings, body } => {
                for pb in bindings.iter_mut() {
                    self.walk_expr(&mut pb.value);
                }
                // Provider resource names are not value bindings, but treat
                // them as shadowing anyway — over-shadowing only disables a
                // fill, never corrupts one.
                let names: HashSet<String> =
                    bindings.iter().map(|pb| pb.resource.clone()).collect();
                self.scopes.push(names);
                self.walk_block(body);
                self.scopes.pop();
            }
        }
    }
}

/// Compute the completed argument list for a call to a defaulted fn, or
/// `None` to leave the call untouched (nothing to fill, or a shape whose
/// existing diagnostics should fire unchanged — see the module doc).
fn try_fill(args: &[CallArg], info: &FnDefaultInfo) -> Option<Vec<CallArg>> {
    let n = info.defaults.len();
    if args.len() >= n {
        return None;
    }
    let mut out: Vec<CallArg> = Vec::with_capacity(n);
    // Indices (into `out`) of spliced arguments, so the label policy below
    // can be applied after the full list is known.
    let mut spliced: Vec<usize> = Vec::new();
    let mut cursor = 0usize;
    let mut seen_label = false;
    for arg in args {
        match &arg.label {
            None => {
                // Unlabeled after labeled is the author's contiguity error —
                // leave it for the typechecker to report on the original shape.
                if seen_label || cursor >= n {
                    return None;
                }
                out.push(arg.clone());
                cursor += 1;
            }
            Some(label) => {
                seen_label = true;
                // The labeled argument names a parameter at or after the
                // cursor; every parameter stepped over must have a default.
                // An unknown / out-of-order label falls back to the existing
                // label-mismatch diagnostic.
                let j = (cursor..n).find(|&k| info.names[k].as_deref() == Some(label.as_str()))?;
                for k in cursor..j {
                    spliced.push(out.len());
                    out.push(synthesize_arg(info.defaults[k].as_ref()?, &info.names[k]));
                }
                out.push(arg.clone());
                cursor = j + 1;
            }
        }
    }
    for k in cursor..n {
        spliced.push(out.len());
        out.push(synthesize_arg(info.defaults[k].as_ref()?, &info.names[k]));
    }
    if spliced.is_empty() {
        return None;
    }
    // Label policy: spliced args carry their parameter name as a label so the
    // final list satisfies the contiguous/declaration-order label rules. A
    // defaulted parameter with no single name (destructuring pattern) cannot
    // be labeled; that is only representable when the final list has no
    // labels at all (fills are then trailing-only and an unlabeled suffix is
    // legal) — otherwise leave the call untouched.
    let any_labeled = out.iter().any(|a| a.label.is_some());
    if any_labeled && spliced.iter().any(|&i| out[i].label.is_none()) {
        return None;
    }
    Some(out)
}

/// Build the spliced argument for an omitted parameter: a clone of the
/// declaration's default expression, labeled with the parameter's name when
/// it has one. Declaration spans are kept (see the module doc).
fn synthesize_arg(default: &Expr, name: &Option<String>) -> CallArg {
    CallArg {
        label: name.clone(),
        mut_marker: false,
        mut_marker_span: None,
        span: default.span,
        value: default.clone(),
    }
}
