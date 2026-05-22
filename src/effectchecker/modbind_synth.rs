//! Synthetic per-binding resource attribution for module-level
//! `let mut` bindings (design.md §1322 + §1330).
//!
//! Every module-level `let mut BINDING` implicitly declares a
//! project-internal `effect resource BINDING_resource`. Reads of the
//! binding contribute `reads(BINDING_resource)` to the enclosing
//! function's inferred effect set; assignments contribute
//! `writes(BINDING_resource)`. Immutable `let` declares no synthetic
//! resource — reads of immutable bindings are free.
//!
//! Under `#[thread_local]`, the synthetic resource is wrapped as
//! `ThreadLocal[BINDING_resource]` (design.md §1330) so it never
//! conflicts with itself across tasks — each task holds a disjoint
//! instance, semantically.
//!
//! Wiring: a per-binding pair of synthetic "callee" keys
//! (`__modbind_read.<NAME>` / `__modbind_write.<NAME>`) is seeded
//! into `inferred_effects` at start-of-check with the appropriate
//! effect, and a shadow-aware walker pushes those keys as
//! pseudo-call entries when it encounters a read/write of the
//! binding in a function body. The existing call-graph propagation
//! then carries the synthetic effect through callers (a function
//! that calls another function which mutates the binding inherits
//! the `writes(BINDING_resource)`), keeping conflict analysis
//! unchanged.

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

use super::{Effect, EffectOrigin, EffectSet};

/// Metadata for a single module-level `let mut` binding.
#[derive(Debug, Clone)]
pub(crate) struct ModBindingInfo {
    /// `<NAME>_resource` for the bare case, or
    /// `ThreadLocal[<NAME>_resource]` when `#[thread_local]` is
    /// present (design.md §1330).
    pub(crate) resource_name: String,
}

/// Synthetic callee-key prefix for the read side. The suffix is the
/// binding's source name (e.g. `__modbind_read.COUNTER`). The full
/// key is seeded once in `inferred_effects` with the synthetic
/// `reads(<resource>)` effect.
pub(crate) const MODBIND_READ_PREFIX: &str = "__modbind_read.";

/// Synthetic callee-key prefix for the write side.
pub(crate) const MODBIND_WRITE_PREFIX: &str = "__modbind_write.";

impl<'a> super::EffectChecker<'a> {
    /// Walk `program.items` and populate `modbind_let_mut` with one
    /// entry per `let mut` module binding. Immutable `let` bindings
    /// are skipped — they declare no synthetic resource.
    pub(crate) fn collect_module_let_mut_bindings(&mut self) {
        for item in &self.program.items {
            let b = match item {
                Item::ModuleBinding(b) => b,
                _ => continue,
            };
            if !b.is_mut {
                continue;
            }
            let is_thread_local = b.attributes.iter().any(|a| a.is_bare("thread_local"));
            let base = format!("{}_resource", b.name);
            let resource_name = if is_thread_local {
                format!("ThreadLocal[{}]", base)
            } else {
                base
            };
            self.modbind_let_mut
                .insert(b.name.clone(), ModBindingInfo { resource_name });
        }
    }

    /// For every module-level `let mut BINDING`, seed `inferred_effects`
    /// with two synthetic callee keys carrying the read/write effects
    /// on the binding's synthetic resource. The walker emits these
    /// keys at call-collection time so existing call-graph propagation
    /// carries the synthetic effect through callers without any
    /// change to the propagation logic.
    pub(crate) fn seed_modbind_synth_effects(&mut self, builtin_span: &Span) {
        let entries: Vec<(String, String)> = self
            .modbind_let_mut
            .iter()
            .map(|(name, info)| (name.clone(), info.resource_name.clone()))
            .collect();
        for (name, resource) in entries {
            let read_key = format!("{}{}", MODBIND_READ_PREFIX, name);
            let write_key = format!("{}{}", MODBIND_WRITE_PREFIX, name);
            let mut read_set = EffectSet::new();
            read_set.add(
                Effect {
                    verb: EffectVerbKind::Reads,
                    resource: resource.clone(),
                },
                EffectOrigin::Direct(builtin_span.clone()),
            );
            let mut write_set = EffectSet::new();
            write_set.add(
                Effect {
                    verb: EffectVerbKind::Writes,
                    resource,
                },
                EffectOrigin::Direct(builtin_span.clone()),
            );
            self.inferred_effects.insert(read_key, read_set);
            self.inferred_effects.insert(write_key, write_set);
        }
    }

    /// Walk a function body and emit synthetic `__modbind_*` call
    /// entries for every read / write of a module-level `let mut`
    /// binding. The shadow stack tracks local-let / parameter /
    /// pattern-introduced names so a body that shadows a module
    /// binding with a local of the same name does not contribute
    /// the synthetic effect (mirrors the typechecker's slice-5
    /// behaviour where local shadowing takes precedence over the
    /// module-binding LHS-mutability check).
    pub(crate) fn collect_modbind_synth_calls_in_block(
        &self,
        block: &Block,
        param_names: &[String],
    ) -> Vec<(String, Span)> {
        if self.modbind_let_mut.is_empty() {
            return Vec::new();
        }
        let mut walker = ModBindingSynthWalker::new(&self.modbind_let_mut);
        for p in param_names {
            walker.push_shadow(p.clone());
        }
        walker.walk_block(block);
        walker.calls
    }
}

/// Walker state for the synthetic-resource pass. Each entry on
/// `shadow` is a name introduced into the current local scope (let
/// binding, parameter, closure binder, for-loop pattern, match arm
/// pattern, if-let / while-let / let-else pattern). `push_shadow` /
/// `pop_shadow` form a stack so block exits restore the prior view.
struct ModBindingSynthWalker<'a> {
    bindings: &'a HashMap<String, ModBindingInfo>,
    shadow: Vec<String>,
    calls: Vec<(String, Span)>,
}

impl<'a> ModBindingSynthWalker<'a> {
    fn new(bindings: &'a HashMap<String, ModBindingInfo>) -> Self {
        ModBindingSynthWalker {
            bindings,
            shadow: Vec::new(),
            calls: Vec::new(),
        }
    }

    fn push_shadow(&mut self, name: String) {
        self.shadow.push(name);
    }

    fn is_shadowed(&self, name: &str) -> bool {
        self.shadow.iter().any(|n| n == name)
    }

    fn is_let_mut_binding(&self, name: &str) -> bool {
        self.bindings.contains_key(name)
    }

    fn record_read(&mut self, name: &str, span: &Span) {
        if self.is_let_mut_binding(name) && !self.is_shadowed(name) {
            self.calls
                .push((format!("{}{}", MODBIND_READ_PREFIX, name), span.clone()));
        }
    }

    fn record_write(&mut self, name: &str, span: &Span) {
        if self.is_let_mut_binding(name) && !self.is_shadowed(name) {
            self.calls
                .push((format!("{}{}", MODBIND_WRITE_PREFIX, name), span.clone()));
        }
    }

    fn walk_block(&mut self, block: &Block) {
        let saved = self.shadow.len();
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref e) = block.final_expr {
            self.walk_expr(e);
        }
        self.shadow.truncate(saved);
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                self.walk_expr(value);
                for name in pattern.binding_names() {
                    self.push_shadow(name);
                }
            }
            StmtKind::LetUninit { name, .. } => {
                self.push_shadow(name.clone());
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.walk_expr(value);
                // Else block runs without the binding in scope.
                self.walk_block(else_block);
                for name in pattern.binding_names() {
                    self.push_shadow(name);
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body);
            }
            StmtKind::Assign { target, value } => {
                self.walk_assign_target(target);
                self.walk_expr(value);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_compound_target(target);
                self.walk_expr(value);
            }
            StmtKind::Expr(e) => self.walk_expr(e),
        }
    }

    /// Pure-write target. A bare identifier `X = …` writes the
    /// binding (no read); compound targets (`x.y = …`, `x[i] = …`)
    /// recurse normally so any nested identifier reads still count.
    fn walk_assign_target(&mut self, target: &Expr) {
        if let ExprKind::Identifier(name) = &target.kind {
            self.record_write(name, &target.span);
        } else {
            self.walk_expr(target);
        }
    }

    /// Read-and-write target. `X += …` reads the binding before
    /// adding (load+store at codegen), so the spec contributes both
    /// `reads(X_resource)` and `writes(X_resource)`.
    fn walk_compound_target(&mut self, target: &Expr) {
        if let ExprKind::Identifier(name) = &target.kind {
            self.record_read(name, &target.span);
            self.record_write(name, &target.span);
        } else {
            self.walk_expr(target);
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Identifier(name) => {
                self.record_read(name, &expr.span);
            }
            ExprKind::Path { segments, .. } => {
                // A bare single-segment path can also reference a
                // module binding (less common — usually parses as
                // Identifier). Multi-segment paths address something
                // qualified and are not a bare module-binding read.
                if segments.len() == 1 {
                    self.record_read(&segments[0], &expr.span);
                }
            }
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::NilCoalesce { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Question(inner) => self.walk_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object);
                if let Some(args) = args {
                    for a in args {
                        self.walk_expr(&a.value);
                    }
                }
            }
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee);
                for a in args {
                    self.walk_expr(&a.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.walk_expr(object);
                for a in args {
                    self.walk_expr(&a.value);
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object);
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Block(b) => self.walk_block(b),
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
            } => {
                self.walk_expr(value);
                let saved = self.shadow.len();
                for n in pattern.binding_names() {
                    self.push_shadow(n);
                }
                self.walk_block(then_block);
                self.shadow.truncate(saved);
                if let Some(e) = else_branch {
                    self.walk_expr(e);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    let saved = self.shadow.len();
                    for n in arm.pattern.binding_names() {
                        self.push_shadow(n);
                    }
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g);
                    }
                    self.walk_expr(&arm.body);
                    self.shadow.truncate(saved);
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
                let saved = self.shadow.len();
                for n in pattern.binding_names() {
                    self.push_shadow(n);
                }
                self.walk_block(body);
                self.shadow.truncate(saved);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.walk_expr(iterable);
                let saved = self.shadow.len();
                for n in pattern.binding_names() {
                    self.push_shadow(n);
                }
                self.walk_block(body);
                self.shadow.truncate(saved);
            }
            ExprKind::Loop { body, .. }
            | ExprKind::Unsafe(body)
            | ExprKind::Try(body)
            | ExprKind::Seq(body)
            | ExprKind::Par(body)
            | ExprKind::LabeledBlock { body, .. } => {
                self.walk_block(body);
            }
            ExprKind::Lock { body, .. } => {
                self.walk_block(body);
            }
            ExprKind::Closure { params, body, .. } => {
                let saved = self.shadow.len();
                for p in params {
                    for n in p.pattern.binding_names() {
                        self.push_shadow(n);
                    }
                }
                self.walk_expr(body);
                self.shadow.truncate(saved);
            }
            ExprKind::Return(Some(inner)) => self.walk_expr(inner),
            ExprKind::Break {
                value: Some(inner), ..
            } => self.walk_expr(inner),
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for e in items {
                    self.walk_expr(e);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.walk_expr(e);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.walk_expr(k);
                    self.walk_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_expr(&f.value);
                }
                if let Some(s) = spread {
                    self.walk_expr(s);
                }
            }
            ExprKind::Cast { expr: inner, .. } => self.walk_expr(inner),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(e) = end {
                    self.walk_expr(e);
                }
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.walk_expr(&b.value);
                }
                self.walk_block(body);
            }
            // Leaves with no nested expressions.
            ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::InterpolatedStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Continue { .. }
            | ExprKind::Return(None)
            | ExprKind::Break { value: None, .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
        }
    }
}
