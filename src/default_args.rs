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
pub(crate) struct FnDefaultInfo {
    /// One entry per parameter: `Some(name)` for a simple binding (usable as
    /// an argument label), `None` for a destructuring pattern.
    pub(crate) names: Vec<Option<String>>,
    /// One entry per parameter: the default expression, if declared.
    pub(crate) defaults: Vec<Option<Expr>>,
    /// The index of the first named parameter (after the `;`), or the
    /// parameter count when there are none.
    pub(crate) named_from: usize,
}

impl FnDefaultInfo {
    fn of(params: &[Param]) -> Self {
        FnDefaultInfo {
            named_from: params
                .iter()
                .position(|p| p.is_named)
                .unwrap_or(params.len()),
            names: params
                .iter()
                .map(|p| p.name().map(|s| s.to_string()))
                .collect(),
            defaults: params.iter().map(|p| p.default_value.clone()).collect(),
        }
    }
}

/// B-2026-09-01-20 — INSTANCE-METHOD defaults, keyed `"Type.method"`.
///
/// Unlike the free-function and associated-fn tables built inside
/// [`fill_default_args_in_program`], this one is NOT consumed here. A method
/// call names its callee only through the RECEIVER, and this pass runs before
/// resolve, so there is no type to dispatch on — the module doc's "Scope of
/// the rewrite" records that as the reason methods were left out.
///
/// So the table is handed to the TYPECHECKER, which is the first phase that
/// resolves a method call to a concrete impl. It plans the fill with the same
/// [`try_fill`] every other spelling uses (so labels, contiguity and the
/// no-name-destructuring rule behave identically), records the completed
/// argument list, and `lowering` splices it into the AST — before
/// effectcheck, ownership, the interpreter and codegen, so all three surfaces
/// still see one ordinary full-arity call.
///
/// Ambiguity is dropped exactly as the associated table drops it: a
/// `Type.method` claimed by both an inherent and a trait impl is removed.
///
/// That drop is LOAD-BEARING here, not belt-and-braces, and it is worth being
/// precise about why — the tempting reading is that the typechecker consults
/// this table only after it has already picked the impl, so a wrong-signature
/// fill is impossible. It is not: the key is `Type.method`, which does not
/// distinguish an inherent `impl S` from an `impl T1 for S`, and the resolved
/// `FunctionSig` carries no default EXPRESSIONS to key on instead. So with the
/// drop removed, `s.m()` would be filled from whichever of the two impls
/// happened to be inserted last.
///
/// The cost is stated rather than hidden: a type carrying both an inherent and
/// a trait method of one name gets no default fill on EITHER, and falls back
/// to today's arity error. Declining is the safe direction — filling the wrong
/// signature's defaults changes the program silently.
pub(crate) fn method_default_table(program: &Program) -> HashMap<String, FnDefaultInfo> {
    let mut out: HashMap<String, FnDefaultInfo> = HashMap::new();
    let mut ambiguous: Vec<String> = Vec::new();
    for item in &program.items {
        let Item::ImplBlock(imp) = item else { continue };
        let Some(head) = impl_target_head(&imp.target_type) else {
            continue;
        };
        for it in &imp.items {
            let ImplItem::Method(m) = it else { continue };
            // METHODS only — the receiver-less half is the `assoc` table's.
            if m.self_param.is_none() {
                continue;
            }
            if !m
                .params
                .iter()
                .any(|p| p.default_value.is_some() || p.is_named)
            {
                continue;
            }
            let key = format!("{head}.{}", m.name);
            if out
                .insert(key.clone(), FnDefaultInfo::of(&m.params))
                .is_some()
            {
                ambiguous.push(key);
            }
        }
    }
    for key in ambiguous {
        out.remove(&key);
    }
    out
}

/// The head name of an impl's target type — `H` for `impl H`, `Vec` for
/// `impl Vec[T]`. `None` for a target this pass cannot name (a tuple, a
/// pointer, a function type), which simply means its associated fns are not
/// registered and calls to them keep today's behaviour.
fn impl_target_head(target: &TypeExpr) -> Option<String> {
    match &target.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        _ => None,
    }
}

pub(crate) fn fill_default_args_in_program(program: &mut Program) {
    let mut table: HashMap<String, FnDefaultInfo> = HashMap::new();
    // B-2026-09-01-20 — ASSOCIATED fns, keyed `"Type.method"`. Kept in a
    // separate map from the free-function one on purpose: the key spaces do
    // not overlap (one holds bare names, the other dotted ones), and merging
    // them would let a free `fn take` be reached by a `H.take(..)` call
    // through the shared key space, which is the false positive
    // B-2026-08-22-11 records one phase later.
    let mut assoc: HashMap<String, FnDefaultInfo> = HashMap::new();
    // Keys claimed by more than one impl for the same type — an inherent and
    // a trait method of the same name, say. A call spelled `H.f(..)` names no
    // trait, so this pass cannot tell which one it means, and filling the
    // wrong signature's defaults is worse than not filling: it changes the
    // program silently. Recorded and then REMOVED below, so the ambiguous case
    // falls back to today's arity error.
    let mut assoc_ambiguous: Vec<String> = Vec::new();
    let mut values: HashMap<String, Vec<(String, TypeExpr)>> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                if f.params
                    .iter()
                    .any(|p| p.default_value.is_some() || p.is_named)
                {
                    table.insert(f.name.clone(), FnDefaultInfo::of(&f.params));
                    if let Some(ps) = value_params(f) {
                        values.insert(f.name.clone(), ps);
                    }
                }
            }
            Item::ImplBlock(imp) => {
                let Some(head) = impl_target_head(&imp.target_type) else {
                    continue;
                };
                for it in &imp.items {
                    let ImplItem::Method(m) = it else { continue };
                    // ASSOCIATED fns only. A method with a receiver is called
                    // `h.g(..)`, an `ExprKind::MethodCall` whose impl this
                    // pre-resolve pass cannot pick without the receiver's
                    // TYPE — see the module doc's "Scope of the rewrite".
                    if m.self_param.is_some() {
                        continue;
                    }
                    if !m
                        .params
                        .iter()
                        .any(|p| p.default_value.is_some() || p.is_named)
                    {
                        continue;
                    }
                    let key = format!("{head}.{}", m.name);
                    if assoc
                        .insert(key.clone(), FnDefaultInfo::of(&m.params))
                        .is_some()
                    {
                        assoc_ambiguous.push(key);
                    }
                }
            }
            _ => {}
        }
    }
    for key in assoc_ambiguous {
        assoc.remove(&key);
    }
    let mut methods: HashMap<String, Vec<FnDefaultInfo>> = HashMap::new();
    for (key, info) in method_default_table(program) {
        if info.named_from < info.names.len() {
            let name = key.rsplit('.').next().unwrap_or(&key).to_string();
            methods.entry(name).or_default().push(info);
        }
    }
    if table.is_empty() && assoc.is_empty() && methods.is_empty() {
        return;
    }
    let mut filler = Filler {
        table,
        assoc,
        methods,
        values,
        scopes: Vec::new(),
        hoisted: 0,
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
    /// Associated fns keyed `"Type.method"` — see the table build above.
    assoc: HashMap<String, FnDefaultInfo>,
    /// Lexical scope stack of locally-bound names. A callee identifier that
    /// appears here refers to a local value, not the top-level fn — no fill.
    scopes: Vec<HashSet<String>>,
    /// Instance methods with named parameters, by method name, for
    /// [`reorders`].
    methods: HashMap<String, Vec<FnDefaultInfo>>,
    /// Free functions with named parameters that can be used as values,
    /// with their positional parameters ([`value_params`]).
    values: HashMap<String, Vec<(String, TypeExpr)>>,
    /// Fresh-name counter for [`hoist_impure_args`].
    hoisted: usize,
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
        let mut hoisted = Vec::new();
        // A function with named parameters used as a value: the closure
        // that takes its positional parameters and calls it, so each call
        // through it uses the defaults (design.md § Function values).
        if let ExprKind::Identifier(name) = &expr.kind {
            if !self.is_shadowed(name) {
                if let Some(ps) = self.values.get(name) {
                    *expr = value_closure(name, ps, expr.span);
                } else {
                    return;
                }
            } else {
                return;
            }
        }
        match &mut expr.kind {
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(..)
            | ExprKind::ByteLit(..)
            | ExprKind::ByteStringLit(..)
            | ExprKind::StringLit(..)
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
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.walk_expr(object);
                for a in args.iter_mut() {
                    self.walk_expr(&mut a.value);
                }
                // The receiver's type is not known yet, so this asks every
                // method of this name with named parameters; hoisting when
                // only some of them would reorder costs nothing but a `let`.
                // The receiver is bound first when it is hoisted too, since
                // it is evaluated before the arguments.
                let reordered = self
                    .methods
                    .get(method.as_str())
                    .is_some_and(|infos| infos.iter().any(|i| reorders(args, i)));
                if reordered && args.iter().any(|a| !is_pure(&a.value)) {
                    if !is_pure(object) {
                        hoisted.push(bind_fresh(object, &mut self.hoisted));
                    }
                    hoisted.extend(hoist_impure_args(args, &mut self.hoisted));
                }
            }
            ExprKind::Call { callee, args } => {
                // A callee named directly is a call, not a function value.
                if !matches!(callee.kind, ExprKind::Identifier(_)) {
                    self.walk_expr(callee);
                }
                for a in args.iter_mut() {
                    self.walk_expr(&mut a.value);
                }
                let info = match &callee.kind {
                    ExprKind::Identifier(name) if !self.is_shadowed(name) => self.table.get(name),
                    // B-2026-09-01-20 — `Type.assoc_fn(args)`. The path NAMES
                    // the type, so this needs no inference and no heuristic:
                    // the key is exact and a miss simply leaves the call
                    // alone. That is what separates it from the sibling
                    // `h.g(args)` spelling, which would have to guess the
                    // receiver's type at a point in the pipeline where no type
                    // exists yet.
                    ExprKind::Path { segments, .. } if segments.len() == 2 => {
                        self.assoc.get(&format!("{}.{}", segments[0], segments[1]))
                    }
                    _ => None,
                };
                if let Some(info) = info {
                    if info.named_from < info.defaults.len()
                        && reorders(args, info)
                        && try_fill(args, info).is_some()
                    {
                        hoisted = hoist_impure_args(args, &mut self.hoisted);
                    }
                    if let Some(filled) = try_fill(args, info) {
                        *args = filled;
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
            ExprKind::MapLiteral { entries: pairs, .. } => {
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
        if !hoisted.is_empty() {
            wrap_in_block(expr, hoisted);
        }
    }
}

/// Compute the completed argument list for a call to a defaulted fn, or
/// `None` to leave the call untouched (nothing to fill, or a shape whose
/// existing diagnostics should fire unchanged — see the module doc).
pub(crate) fn try_fill(args: &[CallArg], info: &FnDefaultInfo) -> Option<Vec<CallArg>> {
    let n = info.defaults.len();
    if info.named_from < n {
        return fill_named(args, info);
    }
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

/// The argument list of a call to a function with named parameters
/// (design.md § Named and default parameters), in declaration order: the
/// positional arguments, then each named parameter's labeled argument or,
/// when omitted, its default. `None` leaves the call to the typechecker's
/// diagnostics: a wrong positional count, a label that names no named
/// parameter or names one twice, an unlabeled argument after a labeled one,
/// or an omitted named parameter with no default.
///
/// The arguments' written order is their evaluation order; a caller that
/// reorders them hoists any with side effects first ([`hoist_impure_args`]).
fn fill_named(args: &[CallArg], info: &FnDefaultInfo) -> Option<Vec<CallArg>> {
    let n = info.defaults.len();
    let positional = args.iter().take_while(|a| a.label.is_none()).count();
    if positional != info.named_from {
        return None;
    }
    let mut given: Vec<Option<CallArg>> = vec![None; n - info.named_from];
    for arg in &args[positional..] {
        let label = arg.label.as_deref()?;
        let k = (info.named_from..n).find(|&k| info.names[k].as_deref() == Some(label))?;
        let slot = &mut given[k - info.named_from];
        if slot.is_some() {
            return None;
        }
        *slot = Some(arg.clone());
    }
    let mut out: Vec<CallArg> = args[..positional].to_vec();
    for (i, g) in given.into_iter().enumerate() {
        let k = info.named_from + i;
        out.push(match g {
            Some(a) => a,
            None => synthesize_arg(info.defaults[k].as_ref()?, &info.names[k]),
        });
    }
    Some(out)
}

/// The positional parameters of a free function with named parameters that
/// can be a value: one that is not generic, binds each positional parameter
/// to a name, and gives every named parameter a default (design.md
/// § Function values). `None` for any other function.
fn value_params(f: &Function) -> Option<Vec<(String, TypeExpr)>> {
    if f.generic_params.is_some() || !f.params.iter().any(|p| p.is_named) {
        return None;
    }
    let mut out = Vec::new();
    for p in &f.params {
        if p.is_named {
            p.default_value.as_ref()?;
        } else {
            out.push((p.name()?.to_string(), p.ty.clone()));
        }
    }
    Some(out)
}

/// `|p0: T0, ...| name(p0, ...)`: the call inside is filled like any other.
fn value_closure(name: &str, params: &[(String, TypeExpr)], span: crate::token::Span) -> Expr {
    let dummy = crate::ids::NodeId::DUMMY;
    let ident = |n: &str| Expr {
        kind: ExprKind::Identifier(n.to_string()),
        span,
        id: dummy,
    };
    let args = params
        .iter()
        .map(|(n, _)| CallArg {
            label: None,
            mut_marker: false,
            mut_marker_span: None,
            span,
            value: ident(n),
        })
        .collect();
    Expr {
        kind: ExprKind::Closure {
            params: params
                .iter()
                .map(|(n, t)| ClosureParam {
                    pattern: Pattern {
                        id: dummy,
                        kind: PatternKind::Binding(n.clone()),
                        span,
                    },
                    ty: Some(t.clone()),
                    span,
                })
                .collect(),
            capture_mode: None,
            prefix_span: None,
            body: Box::new(Expr {
                kind: ExprKind::Call {
                    callee: Box::new(ident(name)),
                    args,
                },
                span,
                id: dummy,
            }),
        },
        span,
        id: dummy,
    }
}

/// Why [`fill_named`] declined a call, for the typechecker to report:
/// the message and the argument to point at (`None`: the whole call).
/// `None` when the call fills.
pub(crate) fn diagnose_named(
    args: &[CallArg],
    info: &FnDefaultInfo,
) -> Option<(String, Option<crate::token::Span>)> {
    let n = info.defaults.len();
    let first_named = |k: usize| info.names[k].clone().unwrap_or_else(|| "_".to_string());
    let positional = args.iter().take_while(|a| a.label.is_none()).count();
    if positional < info.named_from {
        let arg = &args[positional..].first();
        return Some(match arg {
            Some(a)
                if (0..info.named_from).any(|k| info.names[k].as_deref() == a.label.as_deref()) =>
            {
                (
                    format!(
                        "`{}` is a positional parameter; pass it without a label",
                        a.label.as_deref().unwrap_or("_")
                    ),
                    Some(a.span),
                )
            }
            _ => (
                format!(
                    "expected {} positional argument(s) before the named ones, found {}",
                    info.named_from, positional
                ),
                arg.map(|a| a.span),
            ),
        });
    }
    if positional > info.named_from {
        let a = &args[info.named_from];
        let msg = if info.named_from < n {
            format!(
                "too many positional arguments: `{}` and the parameters after it are named; \
                 write `{}: ...`",
                first_named(info.named_from),
                first_named(info.named_from)
            )
        } else {
            format!("expected {} argument(s), found {}", n, args.len())
        };
        return Some((msg, Some(a.span)));
    }
    let mut seen: Vec<&str> = Vec::new();
    for a in &args[positional..] {
        let Some(label) = a.label.as_deref() else {
            return Some((
                "an argument without a label cannot follow a labeled one".to_string(),
                Some(a.span),
            ));
        };
        if !(info.named_from..n).any(|k| info.names[k].as_deref() == Some(label)) {
            let msg = if (0..info.named_from).any(|k| info.names[k].as_deref() == Some(label)) {
                format!("`{label}` is a positional parameter; pass it without a label")
            } else {
                format!("no named parameter `{label}`")
            };
            return Some((msg, Some(a.span)));
        }
        if seen.contains(&label) {
            return Some((format!("`{label}` is passed twice"), Some(a.span)));
        }
        seen.push(label);
    }
    let missing: Vec<String> = (info.named_from..n)
        .filter(|&k| info.defaults[k].is_none())
        .map(first_named)
        .filter(|name| !seen.contains(&name.as_str()))
        .collect();
    if !missing.is_empty() {
        let list = missing
            .iter()
            .map(|m| format!("`{m}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Some((format!("missing named argument(s) {list}"), None));
    }
    None
}

/// The machine-applicable repair for a call [`diagnose_named`] declines, when
/// there is one: label the positional arguments that now pass named
/// parameters (`f(x, 9090)` to `f(x, port: 9090)` once `port` is named), or
/// drop a label from an argument that is the positional parameter it names,
/// in its own position. Empty when the call needs a human.
pub(crate) fn named_call_fix(
    args: &[CallArg],
    info: &FnDefaultInfo,
) -> Vec<crate::resolver::TextEdit> {
    let n = info.defaults.len();
    let positional = args.iter().take_while(|a| a.label.is_none()).count();
    if positional > info.named_from {
        // Each extra positional argument takes the name of the parameter in
        // its position. Only when every one of them has a parameter, and no
        // later argument already passes that name by label.
        if positional > n {
            return vec![];
        }
        let names: Option<Vec<&str>> = (info.named_from..positional)
            .map(|k| info.names[k].as_deref())
            .collect();
        let Some(names) = names else { return vec![] };
        if args[positional..]
            .iter()
            .any(|a| a.label.as_deref().is_some_and(|l| names.contains(&l)))
        {
            return vec![];
        }
        return args[info.named_from..positional]
            .iter()
            .zip(names)
            .map(|(a, name)| crate::resolver::TextEdit {
                offset: a.span.offset,
                length: 0,
                replacement: format!("{name}: "),
            })
            .collect();
    }
    if positional < info.named_from {
        let a = &args[positional];
        if a.label.is_some() && a.label.as_deref() == info.names[positional].as_deref() {
            let end = a
                .mut_marker_span
                .map(|m| m.offset)
                .unwrap_or(a.value.span.offset);
            if end > a.span.offset {
                return vec![crate::resolver::TextEdit {
                    offset: a.span.offset,
                    length: end - a.span.offset,
                    replacement: String::new(),
                }];
            }
        }
    }
    vec![]
}

/// Free and associated functions with named parameters, keyed `"name"` and
/// `"Type.name"`, for the typechecker's call diagnostics ([`diagnose_named`]).
pub(crate) fn named_param_table(program: &Program) -> HashMap<String, FnDefaultInfo> {
    let mut out = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) if f.params.iter().any(|p| p.is_named) => {
                out.insert(f.name.clone(), FnDefaultInfo::of(&f.params));
            }
            Item::ImplBlock(imp) => {
                let Some(head) = impl_target_head(&imp.target_type) else {
                    continue;
                };
                for it in &imp.items {
                    let ImplItem::Method(m) = it else { continue };
                    if m.self_param.is_none() && m.params.iter().any(|p| p.is_named) {
                        out.insert(format!("{head}.{}", m.name), FnDefaultInfo::of(&m.params));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether evaluating `e` can have an effect or depend on one: anything
/// but a literal or a place read through names and fields.
fn is_pure(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::Bool(_)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        // Making a closure runs nothing, and hoisting one would cut it off
        // from the parameter type it infers its own parameters from.
        | ExprKind::Closure { .. } => true,
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            is_pure(object)
        }
        // Kept in place so a literal operand still takes the parameter's
        // type (`f(n: -1)` for a `u8` parameter).
        ExprKind::Unary { operand, .. } => is_pure(operand),
        ExprKind::Binary { left, right, .. } => is_pure(left) && is_pure(right),
        _ => false,
    }
}

/// Binds each argument with side effects to a fresh local, in the order
/// written, and returns the `let`s for [`wrap_in_block`] to put ahead of the
/// call. Used on a call whose labeled arguments the fill puts back in
/// declaration order ([`reorders`]): the hoisting keeps that from reordering
/// their evaluation (design.md § Named and default parameters: arguments are
/// evaluated in the order written). Pure arguments stay in place.
fn hoist_impure_args(args: &mut [CallArg], counter: &mut usize) -> Vec<Stmt> {
    args.iter_mut()
        .filter(|a| !is_pure(&a.value))
        .map(|a| bind_fresh(&mut a.value, counter))
        .collect()
}

/// Moves `e` into `let __karac_argN = e` and leaves the name in its place.
fn bind_fresh(e: &mut Expr, counter: &mut usize) -> Stmt {
    let name = format!("__karac_arg{}", *counter);
    *counter += 1;
    let span = e.span;
    let value = std::mem::replace(
        e,
        Expr {
            kind: ExprKind::Identifier(name.clone()),
            span,
            id: crate::ids::NodeId::DUMMY,
        },
    );
    Stmt {
        kind: StmtKind::Let {
            is_mut: false,
            pattern: Pattern {
                id: crate::ids::NodeId::DUMMY,
                kind: PatternKind::Binding(name),
                span,
            },
            ty: None,
            value,
        },
        span,
        id: crate::ids::NodeId::DUMMY,
    }
}

/// Whether filling `args` against `info` changes their order: a labeled
/// argument written before one whose parameter comes earlier.
fn reorders(args: &[CallArg], info: &FnDefaultInfo) -> bool {
    let mut last = 0usize;
    for a in args {
        let Some(label) = &a.label else { continue };
        let Some(k) = info.names.iter().position(|n| n.as_deref() == Some(label)) else {
            continue;
        };
        if k < last {
            return true;
        }
        last = k;
    }
    false
}

/// Replaces `expr` with `{ stmts; expr }`.
fn wrap_in_block(expr: &mut Expr, stmts: Vec<Stmt>) {
    let span = expr.span;
    let inner = std::mem::replace(
        expr,
        Expr {
            kind: ExprKind::Tuple(Vec::new()),
            span,
            id: crate::ids::NodeId::DUMMY,
        },
    );
    *expr = Expr {
        kind: ExprKind::Block(Block {
            stmts,
            final_expr: Some(Box::new(inner)),
            span,
        }),
        span,
        id: crate::ids::NodeId::DUMMY,
    };
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
