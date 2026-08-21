//! Effect collection from expressions into per-statement metadata.
//!
//! Extracted verbatim from `concurrency.rs`'s `ConcurrencyChecker` impl
//! (structural-debt extraction, 2026-08-16). Lives in a sibling
//! `impl super::ConcurrencyChecker` block; methods are `pub(super)`.

use super::*;

impl<'a> super::ConcurrencyChecker<'a> {
    pub(super) fn collect_expr_effects(&self, expr: &Expr, info: &mut StmtInfo) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                // Look up callee effects
                if let Some(name) = self.extract_callee_name(callee) {
                    info.called_fn_names.insert(name.clone());
                    let from = info.effects.len();
                    self.add_function_effects(&name, info);
                    // Slice 3: substitute the callee's parameterized-resource
                    // keys (`writes(Db[id])`) with these arguments, for the
                    // effects just added.
                    self.apply_parameterized_keys(&name, args, from, info);
                }
                self.collect_expr_effects(callee, info);
                for arg in args {
                    self.collect_expr_effects(&arg.value, info);
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                info.called_fn_names.insert(method.clone());
                // Walk every effect key ending in `.<method>`. Builtin methods
                // (`Vec.push`, `Map.insert`, ...) live only in
                // `effects.inferred_effects`; user-defined impl methods live
                // in both `method_bodies` and `effects.inferred_effects`, so
                // iterating the latter covers both. Matches the renderer in
                // `concurrency_report::render_stmt_effects`.
                let from = info.effects.len();
                let suffix = format!(".{}", method);
                for key in self.effects.inferred_effects.keys() {
                    if key.ends_with(&suffix) {
                        self.add_function_effects(key, info);
                    }
                }
                for key in self.effects.declared_effects.keys() {
                    if key.ends_with(&suffix) {
                        self.add_function_effects(key, info);
                    }
                }
                // Also try bare method name (matches free-function shape).
                self.add_function_effects(method, info);
                // Slice 3: parameterized-resource keys for a method call. Resolve
                // the EXACT receiver-type method via `method_callee_types` (keyed
                // by the method-call span, which equals the receiver span) so the
                // callee's declared `Db[id]` param substitutes with THESE args
                // (method params exclude the receiver, so arg positions align).
                if let Some(types) = self.types {
                    if let Some(mkey) = types
                        .method_callee_types
                        .get(&SpanKey::from_span(&expr.span))
                    {
                        self.apply_parameterized_keys(mkey, args, from, info);
                    }
                }
                self.collect_expr_effects(object, info);
                for arg in args {
                    self.collect_expr_effects(&arg.value, info);
                }
            }
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
                self.collect_expr_effects(left, info);
                self.collect_expr_effects(right, info);
            }
            ExprKind::NilCoalesce { left, right } => {
                self.collect_expr_effects(left, info);
                self.collect_expr_effects(right, info);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.collect_expr_effects(operand, info);
            }
            ExprKind::Block(block)
            | ExprKind::Comptime(block)
            | ExprKind::Unsafe(block)
            | ExprKind::Try(block)
            | ExprKind::Seq(block)
            | ExprKind::Par(block) => {
                self.collect_block_effects(block, info);
            }
            ExprKind::Lock { body, .. } => {
                self.collect_block_effects(body, info);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.collect_expr_effects(condition, info);
                self.collect_block_effects(then_block, info);
                if let Some(e) = else_branch {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.collect_expr_effects(value, info);
                self.collect_block_effects(then_block, info);
                if let Some(e) = else_branch {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.collect_expr_effects(scrutinee, info);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.collect_expr_effects(guard, info);
                    }
                    self.collect_expr_effects(&arm.body, info);
                }
            }
            ExprKind::While {
                condition, body, ..
            }
            | ExprKind::For {
                iterable: condition,
                body,
                ..
            } => {
                self.collect_expr_effects(condition, info);
                self.collect_block_effects(body, info);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.collect_expr_effects(value, info);
                self.collect_block_effects(body, info);
            }
            ExprKind::Loop { body, .. } => {
                self.collect_block_effects(body, info);
            }
            ExprKind::LabeledBlock { body, .. } => {
                self.collect_block_effects(body, info);
            }
            ExprKind::Closure { body, .. } => {
                self.collect_expr_effects(body, info);
            }
            ExprKind::Return(Some(inner)) => {
                self.collect_expr_effects(inner, info);
            }
            ExprKind::Break {
                value: Some(inner), ..
            } => {
                self.collect_expr_effects(inner, info);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.collect_expr_effects(object, info);
            }
            ExprKind::Index { object, index } => {
                self.collect_expr_effects(object, info);
                self.collect_expr_effects(index, info);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.collect_expr_effects(object, info);
                if let Some(args) = args {
                    for arg in args {
                        self.collect_expr_effects(&arg.value, info);
                    }
                }
            }
            ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
                for e in exprs {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.collect_expr_effects(value, info);
                self.collect_expr_effects(count, info);
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.collect_expr_effects(k, info);
                    self.collect_expr_effects(v, info);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.collect_expr_effects(&f.value, info);
                }
                if let Some(s) = spread {
                    self.collect_expr_effects(s, info);
                }
            }
            ExprKind::Cast { expr: inner, .. } => {
                self.collect_expr_effects(inner, info);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.collect_expr_effects(s, info);
                }
                if let Some(e) = end {
                    self.collect_expr_effects(e, info);
                }
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.collect_expr_effects(&b.value, info);
                }
                self.collect_block_effects(body, info);
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let ParsedInterpolationPart::Expr(inner, _) = part {
                        self.collect_expr_effects(inner, info);
                    }
                }
            }
            // Leaf expressions — no effects
            ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::ByteStringLit(_)
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

    pub(super) fn collect_block_effects(&self, block: &Block, info: &mut StmtInfo) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::MultiAssign { .. } => unreachable!(
                    "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
                ),
                StmtKind::Let { value, .. } => self.collect_expr_effects(value, info),
                StmtKind::LetUninit { .. } => {}
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    self.collect_expr_effects(value, info);
                    self.collect_block_effects(else_block, info);
                }
                StmtKind::Assign { target, value } => {
                    self.collect_expr_effects(target, info);
                    self.collect_expr_effects(value, info);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.collect_expr_effects(target, info);
                    self.collect_expr_effects(value, info);
                }
                StmtKind::Expr(e) => self.collect_expr_effects(e, info),
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.collect_block_effects(body, info);
                }
            }
        }
        if let Some(e) = &block.final_expr {
            self.collect_expr_effects(e, info);
        }
    }

    /// Look up a function's inferred effects and add them to the effect list.
    /// Also sets `info.calls_polymorphic` if the callee's declared effects
    /// include `with _` — in which case the inferred set alone doesn't describe
    /// what the callee may actually do at runtime.
    pub(super) fn add_function_effects(&self, name: &str, info: &mut StmtInfo) {
        if let Some(effect_set) = self.effects.inferred_effects.get(name) {
            for te in &effect_set.effects {
                info.effects.push(StmtEffect {
                    verb: te.effect.verb.clone(),
                    resource: te.effect.resource.clone(),
                    source_callee: Some(name.to_string()),
                    key: None,
                });
            }
        }
        if matches!(
            self.effects.declared_effects.get(name),
            Some(DeclaredEffects::Polymorphic | DeclaredEffects::PolymorphicWithFixed(_))
        ) {
            info.calls_polymorphic = true;
        }
    }

    /// A2b-2 Phase 2 Slice 3: fill in `StmtEffect::key` for the effects a call
    /// contributed (the tail of `info.effects` starting at `from`), from the
    /// callee's DECLARED parameterized resources (`with writes(Db[id])`)
    /// substituted with the actual arguments. `callee` names the resolved
    /// function/method (`fn` name or `Type.method`); `args` are the call args.
    /// Only compile-time-literal partition keys are recorded (a variable arg
    /// stays `None` = unproven = conservatively conflicting). Additive: a
    /// callee with no `[param]` resource leaves every key `None`.
    pub(super) fn apply_parameterized_keys(
        &self,
        callee: &str,
        args: &[CallArg],
        from: usize,
        info: &mut StmtInfo,
    ) {
        let Some(func) = self
            .function_bodies
            .get(callee)
            .or_else(|| self.method_bodies.get(callee))
        else {
            return;
        };
        let Some(list) = &func.effects else {
            return;
        };
        for item in &list.items {
            let EffectItem::Verb(ev) = item else {
                continue;
            };
            for res in &ev.resources {
                let Some(param) = &res.param else {
                    continue;
                };
                let Some(key) = resolve_param_key(param, &func.params, args) else {
                    continue;
                };
                let res_name = res.path.join(".");
                for e in info.effects[from..].iter_mut() {
                    if e.verb == ev.kind && e.resource == res_name {
                        e.key = Some(key.clone());
                    }
                }
            }
        }
    }

    /// Extract a callee name from a call expression.
    pub(super) fn extract_callee_name(&self, callee: &Expr) -> Option<String> {
        match &callee.kind {
            ExprKind::Identifier(name) => Some(name.clone()),
            ExprKind::Path { segments, .. } => {
                if segments.len() == 2 {
                    Some(format!("{}.{}", segments[0], segments[1]))
                } else {
                    segments.last().cloned()
                }
            }
            _ => None,
        }
    }
}
