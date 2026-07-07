//! Block / statement / expression name resolution.
//!
//! Houses the four recursive walkers that drive name resolution for
//! function bodies:
//!
//! - `resolve_block` — pushes a `Block` scope, resolves each stmt
//!   + the optional final expr, pops on exit.
//! - `resolve_block_no_scope` — same body but caller-managed scope
//!   (used by loop bodies, where the loop-variable binding lives
//!   in the parent scope).
//! - `resolve_stmt` — per-statement dispatch on `StmtKind` (let /
//!   defer / errdefer / expression / assignment / return / break /
//!   continue).
//! - `resolve_expr` — the big `ExprKind` match: identifiers, paths,
//!   calls, method calls, struct / enum literals, control-flow
//!   (if / match / while / for / loop / unsafe / seq / par /
//!   labeled blocks), closures, and operator expressions.
//!
//! Lives in a sibling `impl<'a> super::Resolver<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::{ResolveError, ResolveErrorKind, ScopeKind, SymbolKind};

impl<'a> super::Resolver<'a> {
    /// Fuzzy-match a misspelled `break` / `continue` label against the
    /// loop labels currently in scope, returning the nearest one for the
    /// `undefined loop label` diagnostic's `did you mean` prose
    /// (B-2026-07-06-3). Prose only: `Break`/`Continue` carry no
    /// label-token span in the AST, so a machine-applicable `.replacement`
    /// is deferred to the follow-on that threads that span through.
    fn nearby_loop_label(&self, name: &str) -> Option<String> {
        let candidates: Vec<&str> = self
            .loop_labels
            .iter()
            .filter_map(|(l, _)| l.as_deref())
            .collect();
        crate::edit_distance::suggest_similar(name, &candidates)
    }

    // ── Block & Statement resolution ────────────────────────────

    pub(crate) fn resolve_block(&mut self, block: &Block) {
        self.table.push_scope(ScopeKind::Block);
        for stmt in &block.stmts {
            self.resolve_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.resolve_expr(expr);
        }
        self.table.pop_scope();
    }

    /// Resolve a block without pushing a new scope (used when the caller
    /// already pushed a scope, e.g. for-loop body where the binding is
    /// in the same scope as the body).
    pub(crate) fn resolve_block_no_scope(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.resolve_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.resolve_expr(expr);
        }
    }

    /// The bindings a `par`-branch statement introduces into the block:
    /// `(name, span, is_mut)` per binding. Non-binding statements
    /// (expression statements, assignments, defer, ...) introduce none.
    fn par_branch_bindings(stmt: &Stmt) -> Vec<(String, Span, bool)> {
        match &stmt.kind {
            StmtKind::Let {
                is_mut, pattern, ..
            } => pattern
                .binding_name_spans()
                .into_iter()
                .map(|(n, sp)| (n, sp, *is_mut))
                .collect(),
            StmtKind::LetUninit {
                is_mut,
                name,
                name_span,
                ..
            } => vec![(name.clone(), name_span.clone(), *is_mut)],
            StmtKind::LetElse { pattern, .. } => pattern
                .binding_name_spans()
                .into_iter()
                .map(|(n, sp)| (n, sp, false))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Resolve an explicit `par { }` block (B-2026-07-02-5). Statement
    /// semantics differ from a sequential block in exactly one way: each
    /// top-level statement is a concurrent BRANCH resolved in its own
    /// child scope, with sibling branches' bindings invisible — reads of
    /// them get the tailored cross-branch diagnostic via
    /// `par_sibling_bindings` (consulted in `error_undefined_name`).
    /// The block's TAIL expression is the join point: every branch's
    /// bindings are (re-)defined into the block scope before it resolves,
    /// so `par { let a = f(); let b = g(); (a, b) }` keeps working.
    pub(crate) fn resolve_par_block(&mut self, block: &Block) {
        let branch_bindings: Vec<Vec<(String, Span, bool)>> =
            block.stmts.iter().map(Self::par_branch_bindings).collect();

        self.table.push_scope(ScopeKind::Block);
        for (i, stmt) in block.stmts.iter().enumerate() {
            // Merge the outer frame (nested `par`s: an outer par's sibling
            // set stays active inside an inner branch) with this par's
            // OTHER branches. This branch's own bindings are absent —
            // they define normally into the branch scope below, so reads
            // of them resolve and never reach the diagnostic.
            let saved = self.par_sibling_bindings.clone();
            for (j, binds) in branch_bindings.iter().enumerate() {
                if j != i {
                    for (name, span, _) in binds {
                        self.par_sibling_bindings.insert(name.clone(), span.clone());
                    }
                }
            }
            self.table.push_scope(ScopeKind::Block);
            self.resolve_stmt(stmt);
            self.table.pop_scope();
            self.par_sibling_bindings = saved;
        }
        // Join point: all branch bindings become visible for the tail.
        for binds in &branch_bindings {
            for (name, span, is_mut) in binds {
                if let Err(e) = self.table.define_shadowable(
                    name.clone(),
                    SymbolKind::Variable { is_mut: *is_mut },
                    span.clone(),
                    false,
                ) {
                    self.errors.push(e);
                }
            }
        }
        if let Some(ref expr) = block.final_expr {
            self.resolve_expr(expr);
        }
        self.table.pop_scope();
    }

    pub(crate) fn resolve_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let {
                is_mut,
                pattern,
                ty,
                value,
            } => {
                // Resolve the value first (before introducing bindings)
                self.resolve_expr(value);
                if let Some(ref ty) = ty {
                    self.resolve_type_expr(ty);
                }
                // Now define the pattern bindings (shadowing-permitted: a `let`
                // may re-bind a name already present in the current scope).
                self.define_let_bindings(pattern, *is_mut);
            }
            StmtKind::LetUninit {
                is_mut,
                name,
                name_span,
                ty,
            } => {
                self.resolve_type_expr(ty);
                if let Err(e) = self.table.define_shadowable(
                    name.clone(),
                    SymbolKind::Variable { is_mut: *is_mut },
                    name_span.clone(),
                    false,
                ) {
                    self.errors.push(e);
                }
            }
            StmtKind::LetElse {
                pattern,
                ty,
                value,
                else_block,
            } => {
                self.resolve_expr(value);
                if let Some(ref ty) = ty {
                    self.resolve_type_expr(ty);
                }
                self.resolve_block(else_block);
                self.define_let_bindings(pattern, false);
            }
            StmtKind::Defer { body } => {
                self.resolve_block(body);
            }
            StmtKind::ErrDefer { binding, body } => {
                self.table.push_scope(ScopeKind::Block);
                if let Some(name) = binding {
                    if let Err(e) = self.table.define(
                        name.clone(),
                        SymbolKind::Variable { is_mut: false },
                        stmt.span.clone(),
                        false,
                    ) {
                        self.errors.push(e);
                    }
                }
                self.resolve_block(body);
                self.table.pop_scope();
            }
            StmtKind::Assign { target, value } => {
                self.resolve_expr(target);
                self.resolve_expr(value);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.resolve_expr(target);
                self.resolve_expr(value);
            }
            StmtKind::Expr(expr) => {
                self.resolve_expr(expr);
            }
        }
    }

    // ── Expression resolution ───────────────────────────────────

    pub(crate) fn resolve_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Return(None)
            | ExprKind::Error => {}

            ExprKind::Continue { label } => {
                if let Some(name) = label {
                    let entry = self
                        .loop_labels
                        .iter()
                        .find(|(l, _)| l.as_deref() == Some(name.as_str()));
                    match entry {
                        Some((_, LabelKind::Loop)) => {
                            // accepted — `continue` to a labeled loop
                        }
                        Some((_, LabelKind::Block)) => {
                            // LB2 — `continue` to a labeled block is rejected.
                            self.errors.push(ResolveError {
                                message: format!(
                                    "error[E_CONTINUE_LABEL_BLOCK]: continue label `{}` refers to a labeled block; continue is only valid for loops",
                                    name
                                ),
                                span: expr.span.clone(),
                                kind: ResolveErrorKind::ContinueOnBlockLabel,
                                suggestion: Some(format!(
                                    "rename the label or restructure `{}` as a loop if iteration is intended",
                                    name
                                )),
                                replacement: None,
                stub_hint: None,
                            });
                        }
                        None => {
                            let suggestion = self.nearby_loop_label(name);
                            let mut message = format!("undefined loop label `{}`", name);
                            if let Some(ref s) = suggestion {
                                message.push_str(&format!(", did you mean `{}`?", s));
                            }
                            self.errors.push(ResolveError {
                                message,
                                span: expr.span.clone(),
                                kind: ResolveErrorKind::UndefinedLabel,
                                suggestion,
                                replacement: None,
                                stub_hint: None,
                            });
                        }
                    }
                }
            }

            ExprKind::Break { label, value: None } => {
                if let Some(name) = label {
                    if !self
                        .loop_labels
                        .iter()
                        .any(|(l, _)| l.as_deref() == Some(name.as_str()))
                    {
                        let suggestion = self.nearby_loop_label(name);
                        let mut message = format!("undefined loop label `{}`", name);
                        if let Some(ref s) = suggestion {
                            message.push_str(&format!(", did you mean `{}`?", s));
                        }
                        self.errors.push(ResolveError {
                            message,
                            span: expr.span.clone(),
                            kind: ResolveErrorKind::UndefinedLabel,
                            suggestion,
                            replacement: None,
                            stub_hint: None,
                        });
                    }
                }
            }

            ExprKind::Identifier(name) => {
                if let Some(sym) = self.table.lookup(name) {
                    let id = sym.id;
                    self.record_resolution(&expr.span, id);
                } else {
                    self.error_undefined_name(name, expr.span.clone());
                }
            }

            ExprKind::Path { segments, .. } => {
                // Resolve the first segment, then qualified access
                if let Some(first) = segments.first() {
                    if let Some(sym) = self.table.lookup(first) {
                        let id = sym.id;
                        self.record_resolution(&expr.span, id);
                    } else {
                        self.error_undefined_name(first, expr.span.clone());
                    }
                }
            }

            ExprKind::SelfValue => {
                if let Some(sym) = self.table.lookup("self") {
                    let id = sym.id;
                    self.record_resolution(&expr.span, id);
                } else {
                    self.errors.push(ResolveError {
                        message: "'self' used outside of impl method".to_string(),
                        span: expr.span.clone(),
                        kind: ResolveErrorKind::UndefinedName,
                        suggestion: None,
                        replacement: None,
                        stub_hint: None,
                    });
                }
            }

            ExprKind::SelfType => {
                if let Some(sym) = self.table.lookup("Self") {
                    let id = sym.id;
                    self.record_resolution(&expr.span, id);
                } else {
                    self.errors.push(ResolveError {
                        message: "'Self' used outside of impl block".to_string(),
                        span: expr.span.clone(),
                        kind: ResolveErrorKind::UndefinedName,
                        suggestion: None,
                        replacement: None,
                        stub_hint: None,
                    });
                }
            }

            ExprKind::PipePlaceholder => {
                // Validated in type checker; nothing to resolve
            }

            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
                self.resolve_expr(left);
                self.resolve_expr(right);
            }

            ExprKind::Unary { operand, .. } => {
                self.resolve_expr(operand);
            }

            ExprKind::Question(inner) | ExprKind::OptionalChain { object: inner, .. } => {
                self.resolve_expr(inner);
            }

            ExprKind::NilCoalesce { left, right } => {
                self.resolve_expr(left);
                self.resolve_expr(right);
            }

            ExprKind::Call { callee, args } => {
                // Bare-identifier callee that is undefined but matches a
                // trait-declared associated function name: defer resolution
                // to the typechecker, which dispatches via the expected type
                // (`let x: T = default()` → `T.default()`). The resolver does
                // not know the expected type so it cannot pick a target; it
                // suppresses the undefined-name error and lets the typechecker
                // produce a more targeted diagnostic if no expected type is
                // available.
                //
                // Test-file unresolved-call: when the file is `_test.kara`
                // and the callee is a bare unresolved identifier that is
                // not a trait-assoc-fn name, emit the augmented
                // `UndefinedName` carrying a `StubHint` (phase-5-diagnostics
                // line 633). Production-file calls keep the plain
                // diagnostic.
                let mut deferred = false;
                let mut handled = false;
                if let ExprKind::Identifier(name) = &callee.kind {
                    if self.table.lookup(name).is_none() {
                        if self.is_trait_assoc_fn_name(name) {
                            deferred = true;
                        } else if self.is_test_file {
                            self.error_undefined_call_with_stub(name, callee.span.clone(), args);
                            handled = true;
                        }
                    }
                }
                if !deferred && !handled {
                    self.resolve_expr(callee);
                }
                for arg in args {
                    self.resolve_expr(&arg.value);
                }
            }

            ExprKind::MethodCall {
                object,
                args,
                turbofish,
                ..
            } => {
                self.resolve_expr(object);
                for arg in args {
                    self.resolve_expr(&arg.value);
                }
                if let Some(ref tf) = turbofish {
                    for ty in tf {
                        self.resolve_type_expr(ty);
                    }
                }
            }

            ExprKind::FieldAccess { object, .. } => {
                self.resolve_expr(object);
            }

            ExprKind::TupleIndex { object, .. } => {
                self.resolve_expr(object);
            }

            ExprKind::Index { object, index } => {
                self.resolve_expr(object);
                self.resolve_expr(index);
            }

            ExprKind::Block(block) | ExprKind::Comptime(block) => {
                self.resolve_block(block);
            }

            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.resolve_expr(condition);
                self.resolve_block(then_block);
                if let Some(ref else_expr) = else_branch {
                    self.resolve_expr(else_expr);
                }
            }

            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.resolve_expr(value);
                self.table.push_scope(ScopeKind::Block);
                self.resolve_pattern(pattern);
                self.resolve_block(then_block);
                self.table.pop_scope();
                if let Some(ref else_expr) = else_branch {
                    self.resolve_expr(else_expr);
                }
            }

            ExprKind::Match { scrutinee, arms } => {
                self.resolve_expr(scrutinee);
                for arm in arms {
                    self.table.push_scope(ScopeKind::MatchArm);
                    self.resolve_pattern(&arm.pattern);
                    self.resolve_expr(&arm.body);
                    self.table.pop_scope();
                }
            }

            ExprKind::While {
                condition,
                body,
                label,
                ..
            } => {
                self.resolve_expr(condition);
                self.loop_labels.push((label.clone(), LabelKind::Loop));
                self.table.push_scope(ScopeKind::Loop);
                self.resolve_block_no_scope(body);
                self.table.pop_scope();
                self.loop_labels.pop();
            }

            ExprKind::WhileLet {
                pattern,
                value,
                body,
                label,
                ..
            } => {
                self.resolve_expr(value);
                self.loop_labels.push((label.clone(), LabelKind::Loop));
                self.table.push_scope(ScopeKind::Loop);
                self.resolve_pattern(pattern);
                self.resolve_block_no_scope(body);
                self.table.pop_scope();
                self.loop_labels.pop();
            }

            ExprKind::For {
                pattern,
                iterable,
                body,
                label,
                ..
            } => {
                self.resolve_expr(iterable);
                self.loop_labels.push((label.clone(), LabelKind::Loop));
                self.table.push_scope(ScopeKind::Loop);
                self.define_pattern_bindings(pattern, false);
                self.resolve_block_no_scope(body);
                self.table.pop_scope();
                self.loop_labels.pop();
            }

            ExprKind::Loop { body, label, .. } => {
                self.loop_labels.push((label.clone(), LabelKind::Loop));
                self.table.push_scope(ScopeKind::Loop);
                self.resolve_block_no_scope(body);
                self.table.pop_scope();
                self.loop_labels.pop();
            }

            ExprKind::LabeledBlock { label, body, .. } => {
                // LB1 — labeled block: register label with `Block` kind so
                // the resolver can reject `continue label` referring here.
                self.loop_labels
                    .push((Some(label.clone()), LabelKind::Block));
                self.table.push_scope(ScopeKind::Block);
                self.resolve_block_no_scope(body);
                self.table.pop_scope();
                self.loop_labels.pop();
            }

            ExprKind::Closure {
                params,
                capture_mode: _,
                prefix_span: _,
                body,
            } => {
                // LB4 — closure-boundary rule. Save the current label stack
                // and replace it with an empty stack while resolving the
                // closure body, so a `break label` / `continue label` inside
                // the body cannot target an enclosing loop / block label.
                // Restored on exit. Also fixes the missing closure-boundary
                // rule for labeled loops as a side-effect (audit finding
                // 2026-05-08).
                let saved_labels = std::mem::take(&mut self.loop_labels);
                self.table.push_scope(ScopeKind::Closure);
                for param in params {
                    self.define_pattern_bindings(&param.pattern, false);
                    if let Some(ref ty) = param.ty {
                        self.resolve_type_expr(ty);
                    }
                }
                self.resolve_expr(body);
                self.table.pop_scope();
                self.loop_labels = saved_labels;
            }

            ExprKind::Return(inner) => {
                if let Some(ref expr) = inner {
                    self.resolve_expr(expr);
                }
            }

            ExprKind::Break {
                label,
                value: Some(ref expr),
            } => {
                if let Some(name) = label {
                    if !self
                        .loop_labels
                        .iter()
                        .any(|(l, _)| l.as_deref() == Some(name.as_str()))
                    {
                        let suggestion = self.nearby_loop_label(name);
                        let mut message = format!("undefined loop label `{}`", name);
                        if let Some(ref s) = suggestion {
                            message.push_str(&format!(", did you mean `{}`?", s));
                        }
                        self.errors.push(ResolveError {
                            message,
                            span: expr.span.clone(),
                            kind: ResolveErrorKind::UndefinedLabel,
                            suggestion,
                            replacement: None,
                            stub_hint: None,
                        });
                    }
                }
                self.resolve_expr(expr);
            }

            ExprKind::Tuple(exprs) => {
                for e in exprs {
                    self.resolve_expr(e);
                }
            }

            ExprKind::StructLiteral {
                path,
                fields,
                spread,
            } => {
                // Resolve the struct type name
                if let Some(first) = path.first() {
                    if let Some(sym) = self.table.lookup(first) {
                        let id = sym.id;
                        self.record_resolution(&expr.span, id);
                    } else {
                        self.error_undefined_name(first, expr.span.clone());
                    }
                }
                for field in fields {
                    self.resolve_expr(&field.value);
                }
                if let Some(ref spread_expr) = spread {
                    self.resolve_expr(spread_expr);
                }
            }

            ExprKind::Cast { expr: inner, ty } => {
                self.resolve_expr(inner);
                self.resolve_type_expr(ty);
            }
            ExprKind::OffsetOf { ty, field_path: _ } => {
                // Resolve the type expression so the typechecker sees a
                // canonical Type. The field path is identifier-only and
                // resolves against `ty`'s declared fields at typecheck
                // time, not at name resolution.
                self.resolve_type_expr(ty);
            }

            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.resolve_expr(s);
                }
                if let Some(e) = end {
                    self.resolve_expr(e);
                }
            }

            ExprKind::Unsafe(block) => {
                self.resolve_block(block);
            }

            ExprKind::Try(block) => {
                self.resolve_block(block);
            }

            ExprKind::ArrayLiteral(elements) => {
                for elem in elements {
                    self.resolve_expr(elem);
                }
            }

            ExprKind::RepeatLiteral { value, count, .. } => {
                self.resolve_expr(value);
                self.resolve_expr(count);
            }

            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for elem in items {
                    self.resolve_expr(elem);
                }
            }

            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e) = part {
                        self.resolve_expr(e);
                    }
                }
            }

            ExprKind::MapLiteral(entries) => {
                for (key, val) in entries {
                    self.resolve_expr(key);
                    self.resolve_expr(val);
                }
            }

            ExprKind::Seq(block) => {
                self.resolve_block(block);
            }

            // B-2026-07-02-5: each top-level statement of an explicit
            // `par { }` block is a concurrent branch with its own scope —
            // sibling bindings are NOT visible to each other (design.md
            // § Explicit Concurrency: branches "bind [values] to `let`
            // bindings and combine them at the end of the block"). The
            // previous shared arm resolved `par` like a sequential block,
            // so a branch reading a sibling binding sailed through
            // resolution and panicked the interpreter (`unreachable:
            // variable 'x' not found ... should be caught by resolver`) /
            // errored ungracefully in codegen.
            ExprKind::Par(block) => {
                self.resolve_par_block(block);
            }

            ExprKind::Lock {
                mutex, alias, body, ..
            } => {
                // Resolve the place expression naming the mutex (`m`, `self.state`).
                self.resolve_expr(mutex);
                // Resolve body with optional alias binding. The alias (or, for an
                // `Identifier` place, the mutex name itself shadowed) names the
                // inner value for the body.
                self.table.push_scope(ScopeKind::Block);
                let bind_name = alias.clone().or_else(|| match &mutex.kind {
                    ExprKind::Identifier(n) => Some(n.clone()),
                    _ => None,
                });
                if let Some(name) = bind_name {
                    let _ = self.table.define(
                        name,
                        SymbolKind::Variable { is_mut: true },
                        expr.span.clone(),
                        false,
                    );
                }
                self.resolve_block_no_scope(body);
                self.table.pop_scope();
            }

            ExprKind::Providers { bindings, body } => {
                // Each binding key names an effect resource. Resolve against
                // the symbol table so undefined resources surface early (same
                // policy as `effect resource` references in effect verbs).
                // Values are plain expressions; body is a child scope.
                for b in bindings {
                    match self.table.lookup(&b.resource) {
                        Some(sym) if matches!(sym.kind, SymbolKind::EffectResource) => {
                            let id = sym.id;
                            self.record_resolution(&b.resource_span, id);
                        }
                        Some(_) => {
                            self.errors.push(ResolveError {
                                message: format!("'{}' is not an effect resource", b.resource),
                                span: b.resource_span.clone(),
                                kind: ResolveErrorKind::UndefinedName,
                                suggestion: None,
                                replacement: None,
                                stub_hint: None,
                            });
                        }
                        None => {
                            self.errors.push(ResolveError {
                                message: format!("undefined effect resource '{}'", b.resource),
                                span: b.resource_span.clone(),
                                kind: ResolveErrorKind::UndefinedName,
                                suggestion: None,
                                replacement: None,
                                stub_hint: None,
                            });
                        }
                    }
                    self.resolve_expr(&b.value);
                }
                self.resolve_block(body);
            }
        }
    }
}
