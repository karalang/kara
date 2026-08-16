//! Call-site effect subtyping check (Phase D).
//!
//! Verifies that every Fn-typed argument's actual effect set is a
//! subset of the callee's declared `with [effects...]` constraint
//! on that parameter slot. Drives E0404 / E0410 diagnostics with
//! a structured subtype-trace.
//!
//! Houses `check_call_site_subtyping` (the driver) and the
//! three-way body walker (`check_subtyping_in_block`,
//! `check_subtyping_in_stmt`, `check_subtyping_in_expr`)
//! plus the per-call-args check (`check_call_args_subtyping`).
//!
//! Lives in a sibling `impl<'a> super::EffectChecker<'a>` block.

use std::collections::HashSet;

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::{
    format_monomorphized_signature, verb_name, EffectError, EffectErrorKind, EffectSubtypeTrace,
};

impl<'a> super::EffectChecker<'a> {
    pub(crate) fn check_call_site_subtyping(&mut self) {
        let bodies: Vec<std::rc::Rc<crate::ast::Function>> = self
            .function_bodies
            .values()
            .cloned()
            .chain(self.method_bodies.values().cloned())
            .collect();
        for f in bodies {
            self.check_subtyping_in_block(&f.body);
        }
    }

    fn check_subtyping_in_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.check_subtyping_in_stmt(stmt);
        }
        if let Some(expr) = &block.final_expr {
            self.check_subtyping_in_expr(expr);
        }
    }

    fn check_subtyping_in_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } => self.check_subtyping_in_expr(value),
            StmtKind::LetUninit { .. } => {}
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                self.check_subtyping_in_expr(value);
                self.check_subtyping_in_block(else_block);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.check_subtyping_in_block(body);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.check_subtyping_in_expr(target);
                self.check_subtyping_in_expr(value);
            }
            StmtKind::Expr(expr) => self.check_subtyping_in_expr(expr),
        }
    }

    /// Per-argument Fn-slot subtyping check, shared between `Call` and
    /// `MethodCall` arms of `check_subtyping_in_expr`. Resolves the
    /// callee's parameter list (via `function_bodies` or `method_bodies`)
    /// and emits `EffectSubtypeViolation` for any function-valued argument
    /// whose effect set exceeds its slot's declared effects.
    ///
    /// `args` indices align with `params` indices for both call shapes:
    /// method-call `args` exclude the receiver, and `method_bodies[k].params`
    /// excludes `self_param` (it is stored separately).
    ///
    /// `call_span` is the span of the call expression itself (not the args).
    /// Used to look up `call_type_subs` so the diagnostic can render a fully
    /// monomorphized callee signature when the call is generic.
    fn check_call_args_subtyping(&mut self, callee_name: &str, args: &[CallArg], call_span: &Span) {
        let params = self
            .function_bodies
            .get(callee_name)
            .map(|f| f.params.clone())
            .or_else(|| {
                self.method_bodies
                    .get(callee_name)
                    .map(|f| f.params.clone())
            });
        let Some(params) = params else {
            return;
        };
        let return_type = self
            .function_bodies
            .get(callee_name)
            .map(|f| f.return_type.clone())
            .or_else(|| {
                self.method_bodies
                    .get(callee_name)
                    .map(|f| f.return_type.clone())
            })
            .flatten();
        // Per-call bindings for `with E` slots: each named effect variable is
        // resolved to the union of effects supplied at every parameter
        // position that references it. A slot typed `Fn(...) with E` is then
        // checked against this concrete set rather than the empty set.
        // Round 9's unification check separately diagnoses disagreement
        // between positions.
        let var_bindings = self.compute_call_var_bindings(callee_name, args);
        // Phase 6 line 26 slice 8aa: persist the per-call-site
        // effect-variable substitutions into `call_effect_subs` so
        // slice 8ab can forward them to codegen (and slice 8y can
        // gate state-machine emission on whether the resolved
        // effects include any network-yield verb). Only record when
        // the callee has effect variables (`compute_call_var_bindings`
        // returns an empty map for non-polymorphic callees) — the
        // absence of an entry signals "no polymorphic-effect to
        // resolve" downstream, distinct from "resolved to ⊥".
        if !var_bindings.is_empty() {
            self.call_effect_subs
                .insert(SpanKey::from_span(call_span), var_bindings.clone());
        }
        // Look up type-parameter substitutions for this call (Round 10.3
        // step 7). Empty when the callee is non-generic or the typechecker
        // didn't run with `with_call_type_subs` wired in.
        let type_subs = self
            .call_type_subs
            .get(&SpanKey::from_span(call_span))
            .cloned()
            .unwrap_or_default();
        for (i, call_arg) in args.iter().enumerate() {
            let Some(param) = params.get(i) else {
                continue;
            };
            let slot_effects = match &param.ty.kind {
                TypeKind::FnType {
                    effect_spec: Some(EffectSpec::Polymorphic),
                    ..
                } => continue,
                TypeKind::FnType {
                    effect_spec: Some(EffectSpec::Specific(list)),
                    ..
                } => self.resolve_effect_list_to_set(list, Some(&var_bindings)),
                TypeKind::FnType {
                    effect_spec: None, ..
                } => HashSet::new(),
                _ => continue,
            };
            let arg_effects = self.get_arg_effects(&call_arg.value);
            let arg_span = call_arg.value.span;

            // Pre-compute trace fields shared across all E0404 errors for
            // this argument position (slot / argument / offending sets).
            let slot_str: Vec<String> = slot_effects
                .iter()
                .map(|e| format!("{}({})", verb_name(&e.verb), e.resource))
                .collect();
            let arg_str: Vec<String> = arg_effects
                .effects
                .iter()
                .filter(|te| !self.is_transparent_verb(&te.effect.verb))
                .map(|te| format!("{}({})", verb_name(&te.effect.verb), te.effect.resource))
                .collect();
            let offending_str: Vec<String> = arg_effects
                .effects
                .iter()
                .filter(|te| {
                    !self.is_transparent_verb(&te.effect.verb) && !slot_effects.contains(&te.effect)
                })
                .map(|te| format!("{}({})", verb_name(&te.effect.verb), te.effect.resource))
                .collect();

            // Render the monomorphized callee signature (Round 10.3 step 7).
            // Only emitted when the callee has at least one type parameter
            // for which a substitution is known — otherwise it would just be
            // a verbose echo of the source.
            let monomorphized = if type_subs.is_empty() && var_bindings.is_empty() {
                None
            } else {
                Some(format_monomorphized_signature(
                    callee_name,
                    &params,
                    return_type.as_ref(),
                    &type_subs,
                    &var_bindings,
                ))
            };

            for te in &arg_effects.effects {
                let is_transparent = self.is_transparent_verb(&te.effect.verb);
                if !slot_effects.contains(&te.effect) && !is_transparent {
                    let effect_str =
                        format!("{}({})", verb_name(&te.effect.verb), te.effect.resource);
                    let mut message = format!(
                        "argument {} has effect {} not declared in slot [{}]",
                        i + 1,
                        effect_str,
                        if slot_str.is_empty() {
                            "pure".to_string()
                        } else {
                            slot_str.join(", ")
                        },
                    );
                    if let Some(ref sig) = monomorphized {
                        message.push_str(&format!("; callee: {sig}"));
                    }
                    self.errors.push(EffectError {
                        message,
                        span: arg_span,
                        kind: EffectErrorKind::EffectSubtypeViolation,
                        subtype_trace: Some(EffectSubtypeTrace {
                            slot_effects: slot_str.clone(),
                            argument_effects: arg_str.clone(),
                            offending_effects: offending_str.clone(),
                            monomorphized_signature: monomorphized.clone(),
                        }),
                        replacement: None,
                    });
                }
            }
        }
    }

    fn check_subtyping_in_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(cname) = self.extract_callee_name(callee) {
                    self.check_call_args_subtyping(&cname, args, &expr.span);
                }
                // Recurse into callee and args
                self.check_subtyping_in_expr(callee);
                for arg in args {
                    self.check_subtyping_in_expr(&arg.value);
                }
            }
            ExprKind::Block(block) | ExprKind::Comptime(block) => {
                self.check_subtyping_in_block(block)
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.check_subtyping_in_expr(condition);
                self.check_subtyping_in_block(then_block);
                if let Some(e) = else_branch {
                    self.check_subtyping_in_expr(e);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.check_subtyping_in_expr(value);
                self.check_subtyping_in_block(then_block);
                if let Some(e) = else_branch {
                    self.check_subtyping_in_expr(e);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.check_subtyping_in_expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.check_subtyping_in_expr(g);
                    }
                    self.check_subtyping_in_expr(&arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.check_subtyping_in_expr(condition);
                self.check_subtyping_in_block(body);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.check_subtyping_in_expr(value);
                self.check_subtyping_in_block(body);
            }
            ExprKind::For { iterable, body, .. } => {
                self.check_subtyping_in_expr(iterable);
                self.check_subtyping_in_block(body);
            }
            ExprKind::Loop { body, .. }
            | ExprKind::Unsafe(body)
            | ExprKind::Try(body)
            | ExprKind::Seq(body)
            | ExprKind::Par(body) => {
                self.check_subtyping_in_block(body);
            }
            ExprKind::LabeledBlock { body, .. } => self.check_subtyping_in_block(body),
            ExprKind::Lock { body, .. } => self.check_subtyping_in_block(body),
            ExprKind::Closure { body, .. } => self.check_subtyping_in_expr(body),
            ExprKind::MethodCall { object, args, .. } => {
                // Mirror the `Call` branch: resolve to `Type.method` via the
                // typechecker side-table and run the same per-arg Fn-slot
                // subtyping check. Without this, an effectful closure could
                // satisfy a method's pure `Fn()` slot whenever the enclosing
                // caller declared the effects.
                if let Some(callee_key) = self.resolve_method_callee_key(&expr.span) {
                    self.check_call_args_subtyping(&callee_key, args, &expr.span);
                }
                self.check_subtyping_in_expr(object);
                for arg in args {
                    self.check_subtyping_in_expr(&arg.value);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.check_subtyping_in_expr(left);
                self.check_subtyping_in_expr(right);
            }
            ExprKind::Pipe { left, right } => {
                self.check_subtyping_in_expr(left);
                self.check_subtyping_in_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.check_subtyping_in_expr(operand),
            ExprKind::Return(Some(e)) | ExprKind::Question(e) => self.check_subtyping_in_expr(e),
            ExprKind::Break { value: Some(e), .. } => self.check_subtyping_in_expr(e),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.check_subtyping_in_expr(object)
            }
            ExprKind::Index { object, index } => {
                self.check_subtyping_in_expr(object);
                self.check_subtyping_in_expr(index);
            }
            ExprKind::Tuple(exprs) => {
                for e in exprs {
                    self.check_subtyping_in_expr(e);
                }
            }
            ExprKind::ArrayLiteral(elems) => {
                for e in elems {
                    self.check_subtyping_in_expr(e);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.check_subtyping_in_expr(value);
                self.check_subtyping_in_expr(count);
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.check_subtyping_in_expr(e);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.check_subtyping_in_expr(&f.value);
                }
                if let Some(s) = spread {
                    self.check_subtyping_in_expr(s);
                }
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.check_subtyping_in_expr(k);
                    self.check_subtyping_in_expr(v);
                }
            }
            ExprKind::Cast { expr: inner, .. } => self.check_subtyping_in_expr(inner),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.check_subtyping_in_expr(s);
                }
                if let Some(e) = end {
                    self.check_subtyping_in_expr(e);
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.check_subtyping_in_expr(left);
                self.check_subtyping_in_expr(right);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.check_subtyping_in_expr(object);
                if let Some(args) = args {
                    for a in args {
                        self.check_subtyping_in_expr(&a.value);
                    }
                }
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.check_subtyping_in_expr(&b.value);
                }
                self.check_subtyping_in_block(body);
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    if let ParsedInterpolationPart::Expr(e, _) = p {
                        self.check_subtyping_in_expr(e);
                    }
                }
            }
            // Leaf expressions — nothing to recurse into
            ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
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
