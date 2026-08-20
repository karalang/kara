//! JSON + JSONL diagnostic emission: the machine surface behind
//! `--output=json` / `--output=jsonl` (span/diagnostic serialization, the
//! codegen-hint side tables, and the streaming pipeline events).
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 2).

use super::*;
use rustc_hash::FxHashMap;

// ── JSON Output ─────────────────────────────────────────────────

pub(super) fn span_to_json(span: &Span, filename: &str) -> String {
    format!(
        "\"file\":{},\"line\":{},\"column\":{}",
        json_string(filename),
        span.line,
        span.column
    )
}

pub(super) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => {
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub(super) fn json_string_list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| json_string(s)).collect();
    format!("[{}]", parts.join(","))
}

/// Build the per-callee "is effectful" side-table from an `EffectCheckResult`.
///
/// A callee is "effectful" iff its inferred or declared effect set contains
/// any `reads` / `writes` / `sends` / `receives` verb (the four verbs that
/// drive cooperative-cancellation observability). `allocates`, `panics`,
/// `blocks`, `suspends`, `UserDefined` are excluded — they don't motivate
/// the per-call cancel check.
pub(super) fn build_callee_effectful_table(
    effects: &EffectCheckResult,
) -> std::collections::HashMap<String, bool> {
    fn set_is_effectful(set: &crate::effectchecker::EffectSet) -> bool {
        set.effects.iter().any(|t| {
            matches!(
                t.effect.verb,
                EffectVerbKind::Reads
                    | EffectVerbKind::Writes
                    | EffectVerbKind::Sends
                    | EffectVerbKind::Receives
            )
        })
    }
    let mut table = std::collections::HashMap::new();
    for (name, set) in &effects.inferred_effects {
        table.insert(name.clone(), set_is_effectful(set));
    }
    for (name, decl) in &effects.declared_effects {
        // Polymorphic / PolymorphicWithFixed callees may carry effects per
        // monomorphization — treat them as effectful (conservative).
        let effectful = match decl {
            DeclaredEffects::Explicit(set) => set_is_effectful(set),
            // The polymorphic portion may pick up any effect at a
            // monomorphization site, so treat as effectful even if the fixed
            // set is empty.
            DeclaredEffects::PolymorphicWithFixed(_) | DeclaredEffects::Polymorphic => true,
            DeclaredEffects::None => false,
        };
        table
            .entry(name.clone())
            .and_modify(|v| *v = *v || effectful)
            .or_insert(effectful);
    }
    table
}

/// Phase 6 line 26 slice 8ab: convert the effect-checker's
/// `call_effect_subs` (keyed by `SpanKey` with internal `Effect`
/// values) into the AST-level `CallEffectSubsTable` (keyed by
/// `(offset, length)` with plain-data `EffectKey` values) so codegen
/// can read it without taking a dependency on the effectchecker's
/// `Effect` struct. Each entry's verb is rendered via a local
/// `verb_to_name` mirror of the effectchecker's diagnostic rendering;
/// resource names round-trip unchanged.
pub fn build_call_effect_subs_table(
    effects: &EffectCheckResult,
) -> crate::ast::CallEffectSubsTable {
    fn verb_to_name(verb: &EffectVerbKind) -> String {
        match verb {
            EffectVerbKind::Reads => "reads".to_string(),
            EffectVerbKind::Writes => "writes".to_string(),
            EffectVerbKind::Sends => "sends".to_string(),
            EffectVerbKind::Receives => "receives".to_string(),
            EffectVerbKind::Allocates => "allocates".to_string(),
            EffectVerbKind::Panics => "panics".to_string(),
            EffectVerbKind::Blocks => "blocks".to_string(),
            EffectVerbKind::Suspends => "suspends".to_string(),
            EffectVerbKind::UserDefined(name) => name.clone(),
        }
    }
    let mut table = crate::ast::CallEffectSubsTable::new();
    for (span_key, bindings) in &effects.call_effect_subs {
        let mut inner = std::collections::HashMap::new();
        for (var_name, effect_set) in bindings {
            let keys: Vec<crate::ast::EffectKey> = effect_set
                .iter()
                .map(|e| crate::ast::EffectKey {
                    verb: verb_to_name(&e.verb),
                    resource: e.resource.clone(),
                })
                .collect();
            inner.insert(var_name.clone(), keys);
        }
        table.insert((span_key.0, span_key.1), inner);
    }
    table
}

/// Phase 6 line 26 slice 8y: build the set of callee names whose
/// declared effects are `DeclaredEffects::Polymorphic` only — purely
/// `with E` (or `with _`) with no static fixed portion. Codegen uses
/// this set to identify callees for which `call_effect_subs` is the
/// sole authoritative source of "does this call resolve to a
/// network-yield effect", as opposed to `PolymorphicWithFixed` or
/// `Explicit` callees whose static portion may already carry
/// `sends(Network)` / `receives(Network)` and therefore must always
/// flow through the state-machine transform regardless of `E`
/// resolution.
///
/// Mirrors `build_callee_network_yield_effect_table`'s sourcing of
/// `declared_effects`; inferred effects on private fns are never
/// `Polymorphic` (`DeclaredEffects::Polymorphic` is set only via an
/// explicit `with E` / `with _` annotation), so they are excluded by
/// construction.
pub fn build_callee_purely_polymorphic_effects_set(
    effects: &EffectCheckResult,
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for (name, decl) in &effects.declared_effects {
        if matches!(decl, DeclaredEffects::Polymorphic) {
            set.insert(name.clone());
        }
    }
    set
}

/// Build the per-callee "is network-boundary" side-table from an
/// `EffectCheckResult`.
///
/// A callee is "network-boundary" iff its inferred or declared effect set
/// contains a `sends(Network)` or `receives(Network)` verb-resource pair.
/// These are the only effects that route through the network event loop's
/// non-blocking park-and-yield path at v1 (design.md § Network Event Loop
/// and State-Machine Transform > State-Machine Transform — Network-Boundary
/// Functions). Functions whose suspension is rooted in other verbs
/// (`Receiver.recv` via `suspends`, custom user `suspends`, future channel
/// waits) continue to thread-block at v1 and are NOT marked.
///
/// Consumed by:
///   - the state-machine transform codegen (phase 6 line 26) — only callees
///     marked `true` are candidates for the transform;
///   - codegen lowering at network-effect call sites (phase 6 line 17
///     sub-item 6) — a call to a `true` callee lowers to "register fd +
///     park + yield" instead of a synchronous call.
pub fn build_callee_network_yield_effect_table(
    effects: &EffectCheckResult,
) -> std::collections::HashMap<String, bool> {
    fn set_has_network_yield(set: &crate::effectchecker::EffectSet) -> bool {
        set.effects.iter().any(|t| {
            matches!(
                t.effect.verb,
                EffectVerbKind::Sends | EffectVerbKind::Receives
            ) && t.effect.resource == "Network"
        })
    }
    let mut table = std::collections::HashMap::new();
    for (name, set) in &effects.inferred_effects {
        table.insert(name.clone(), set_has_network_yield(set));
    }
    for (name, decl) in &effects.declared_effects {
        // Polymorphic effect parameters may bind to a `sends(Network)` /
        // `receives(Network)` at a monomorphization site, so conservatively
        // mark as network-boundary candidate. The state-machine transform
        // itself reads the resolved monomorphized effect set when deciding
        // to apply, so over-counting here is harmless — it just keeps the
        // function in the candidate pool that the transform pass filters.
        let network_yield = match decl {
            DeclaredEffects::Explicit(set) => set_has_network_yield(set),
            DeclaredEffects::PolymorphicWithFixed(_) | DeclaredEffects::Polymorphic => true,
            DeclaredEffects::None => false,
        };
        table
            .entry(name.clone())
            .and_modify(|v| *v = *v || network_yield)
            .or_insert(network_yield);
    }
    table
}

/// Walk every function/method body in `program` and, for each
/// network-boundary function (one marked `true` in `network_yield`),
/// produce its ordered list of yield points — call sites whose callee is
/// itself in `network_yield` with value `true`.
///
/// Callee resolution rules at a call site:
///   - `Call { callee: Identifier(name) }` → callee key is `name`;
///   - `Call { callee: Path { segments, .. } }` → callee key is the joined
///     segments separated by `.` (matches `Type.method` shape from
///     `EffectCheckResult` keys);
///   - `MethodCall { .. }` → callee key looked up in `method_callee_types`
///     via the call expression's span;
///   - All other callee shapes (indirect through closure value, function
///     pointer, etc.) → skipped — the codegen lowering pass can't park
///     through an unresolved callee without a stable effect signature.
///
/// Functions without any yield-point calls are omitted from the table
/// (they may still be network-boundary if classified via Polymorphic
/// effect declaration, but they have no concrete suspension points within
/// their bodies for the state-machine transform to lower against).
pub fn build_yield_points_table(
    program: &Program,
    network_yield: &std::collections::HashMap<String, bool>,
    method_callee_types: &FxHashMap<crate::resolver::SpanKey, String>,
) -> std::collections::HashMap<String, Vec<crate::ast::YieldPoint>> {
    let mut table = std::collections::HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(func) => {
                let key = func.name.clone();
                if network_yield.get(&key).copied().unwrap_or(false) {
                    let yps = walk_fn_for_yield_points(func, network_yield, method_callee_types);
                    if !yps.is_empty() {
                        table.insert(key, yps);
                    }
                }
            }
            Item::ImplBlock(imp) => {
                let type_name = match &imp.target_type.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                    _ => continue,
                };
                for ii in &imp.items {
                    let method = match ii {
                        crate::ast::ImplItem::Method(m) => m,
                        crate::ast::ImplItem::AssocType(_) => continue,
                    };
                    let key = format!("{}.{}", type_name, method.name);
                    if network_yield.get(&key).copied().unwrap_or(false) {
                        let yps =
                            walk_fn_for_yield_points(method, network_yield, method_callee_types);
                        if !yps.is_empty() {
                            table.insert(key, yps);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    table
}

/// Walker state for one function body. Threads the network-boundary
/// classification + method-callee resolution maps (read-only), tracks a
/// running scope stack of in-scope binding names (push on let / pattern
/// binding, pop on block exit), and accumulates yield-point records.
/// Centralizes the recursive-walk state cleaner than threading every
/// argument through each helper.
struct YieldPointWalker<'a> {
    network_yield: &'a std::collections::HashMap<String, bool>,
    method_callee_types: &'a FxHashMap<crate::resolver::SpanKey, String>,
    /// Flat stack of in-scope local-binding names in source-introduction
    /// order. Function parameters occupy the bottom of the stack; later
    /// pushes come from `let` / `let-else` / `if let` / `while let` /
    /// `for` / match-arm pattern bindings as the walker crosses them.
    /// On every block exit, the walker truncates back to a recorded
    /// length (lexical scope discipline).
    scope: Vec<String>,
    out: Vec<crate::ast::YieldPoint>,
}

pub(super) fn walk_fn_for_yield_points(
    func: &crate::ast::Function,
    network_yield: &std::collections::HashMap<String, bool>,
    method_callee_types: &FxHashMap<crate::resolver::SpanKey, String>,
) -> Vec<crate::ast::YieldPoint> {
    let mut walker = YieldPointWalker {
        network_yield,
        method_callee_types,
        scope: Vec::new(),
        out: Vec::new(),
    };
    // Function parameters are in scope throughout the body. `self` is
    // bound automatically when `self_param` is present (method bodies).
    // Each non-self param has a `Pattern` that may bind one (simple
    // `name: T`) or multiple (destructuring `let (a, b): (i64, i64)`)
    // names; collect them all.
    if func.self_param.is_some() {
        walker.scope.push("self".to_string());
    }
    for p in &func.params {
        for name in p.pattern.binding_names() {
            walker.scope.push(name);
        }
    }
    walker.walk_block(&func.body);
    walker.out
}

pub(super) fn callee_key_from_call(callee: &crate::ast::Expr) -> Option<String> {
    use crate::ast::ExprKind;
    match &callee.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::Path { segments, .. } => Some(segments.join(".")),
        _ => None,
    }
}

impl YieldPointWalker<'_> {
    fn snapshot_scope(&self) -> Vec<String> {
        self.scope.clone()
    }

    fn walk_block(&mut self, block: &crate::ast::Block) {
        let scope_mark = self.scope.len();
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    /// Walk a block where the pattern's bindings are pre-pushed onto the
    /// scope (used for `if let` / `while let` / `for` bodies and the
    /// match-arm `Block` form). Pattern bindings live through the entire
    /// block and pop when it exits.
    fn walk_block_with_pattern(&mut self, pat: &crate::ast::Pattern, block: &crate::ast::Block) {
        let scope_mark = self.scope.len();
        for name in pat.binding_names() {
            self.scope.push(name);
        }
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    /// Same idea for a match-arm body expression (which may be a Block
    /// or any other Expr — non-block arms still need pattern scope).
    fn walk_expr_with_pattern(&mut self, pat: &crate::ast::Pattern, expr: &crate::ast::Expr) {
        let scope_mark = self.scope.len();
        for name in pat.binding_names() {
            self.scope.push(name);
        }
        self.walk_expr(expr);
        self.scope.truncate(scope_mark);
    }

    fn walk_stmt(&mut self, stmt: &crate::ast::Stmt) {
        use crate::ast::StmtKind;
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, pattern, .. } => {
                // Walk the value FIRST — yield points in the RHS see the
                // pre-binding scope. Then introduce the pattern's bindings
                // into the parent scope.
                self.walk_expr(value);
                for name in pattern.binding_names() {
                    self.scope.push(name);
                }
            }
            StmtKind::LetUninit { name, .. } => {
                self.scope.push(name.clone());
            }
            StmtKind::LetElse {
                value,
                pattern,
                else_block,
                ..
            } => {
                // Value walks against the pre-binding scope.
                self.walk_expr(value);
                // Else block walks in its own nested scope — it must
                // diverge, so its bindings never propagate to the parent.
                self.walk_block(else_block);
                // Success-branch pattern bindings flow into the parent
                // scope after the let-else statement.
                for name in pattern.binding_names() {
                    self.scope.push(name);
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Expr(expr) => self.walk_expr(expr),
        }
    }

    fn walk_expr(&mut self, expr: &crate::ast::Expr) {
        use crate::ast::ExprKind;
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(key) = callee_key_from_call(callee) {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        let captured = self.snapshot_scope();
                        self.out.push(crate::ast::YieldPoint {
                            callee: key,
                            span: expr.span,
                            captured_locals: captured,
                        });
                    }
                }
                self.walk_expr(callee);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                if let Some(key) = self
                    .method_callee_types
                    .get(&crate::resolver::SpanKey::from_span(&expr.span))
                    .cloned()
                {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        let captured = self.snapshot_scope();
                        self.out.push(crate::ast::YieldPoint {
                            callee: key,
                            span: expr.span,
                            captured_locals: captured,
                        });
                    }
                }
                self.walk_expr(object);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Question(inner) => self.walk_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object);
                if let Some(arglist) = args {
                    for arg in arglist {
                        self.walk_expr(&arg.value);
                    }
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object)
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_block(then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                pattern,
                then_block,
                else_branch,
            } => {
                self.walk_expr(value);
                self.walk_block_with_pattern(pattern, then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    if let Some(ref g) = arm.guard {
                        // Guards execute under the arm's pattern bindings.
                        let scope_mark = self.scope.len();
                        for name in arm.pattern.binding_names() {
                            self.scope.push(name);
                        }
                        self.walk_expr(g);
                        self.scope.truncate(scope_mark);
                    }
                    self.walk_expr_with_pattern(&arm.pattern, &arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition);
                self.walk_block(body);
            }
            ExprKind::WhileLet {
                value,
                pattern,
                body,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block_with_pattern(pattern, body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.walk_expr(iterable);
                self.walk_block_with_pattern(pattern, body);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                self.walk_block(body)
            }
            // Closures form their own state machine — a yield point inside
            // a closure body is the closure's yield, not the enclosing
            // function's. Do NOT walk into the closure body for the outer
            // function's yield-point enumeration.
            ExprKind::Closure { .. } => {}
            ExprKind::Return(Some(e)) => self.walk_expr(e),
            ExprKind::Return(None) => {}
            ExprKind::Break { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr(v);
                }
            }
            ExprKind::Continue { .. } => {}
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
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
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
            ExprKind::Pipe { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Cast { expr, .. } => self.walk_expr(expr),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(e) = end {
                    self.walk_expr(e);
                }
            }
            ExprKind::Lock { body, .. } => self.walk_block(body),
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.walk_expr(&b.value);
                }
                self.walk_block(body);
            }
            // Leaves / no-call shapes.
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e, _) = part {
                        self.walk_expr(e);
                    }
                }
            }
        }
    }
}

/// Build the per-function state-struct layout table from a fully-typed
/// `Program` whose `yield_points` table is populated. For each
/// network-boundary function with at least one concrete yield point,
/// produces a `StateStructLayout` whose `fields` list is the union of
/// every yield point's captured-locals set in source-introduction order
/// (parameters first left-to-right, then per-block let-binding sequence;
/// first occurrence across yield points fixes position).
///
/// Each field's `type_name` is looked up in `pattern_binding_types`
/// against the introducing pattern's span — primitives and other shapes
/// the typechecker doesn't record there yield `None`, and codegen falls
/// through to its primitive-sizing path on absent entries.
///
/// `self` is recorded with `type_name` set to the impl block's target
/// type name (not via `pattern_binding_types` — there is no pattern
/// span for `self`; the impl target supplies the canonical name
/// directly).
///
/// Shadowed bindings get separate field slots — collision is keyed on
/// the introducing pattern's span, not the binding name, so the v1
/// layout faithfully reflects the source-level binding identity.
///
/// Functions network-boundary by Polymorphic declared-effect candidacy
/// without any concrete sub-call yield points are omitted from the
/// table (mirrors `YieldPointsTable`'s presence rule).
/// Scalar primitives, whose names the state-struct layout deliberately drops —
/// see the `filter` in `record_entry`.
pub(super) fn is_scalar_primitive_type_name(n: &str) -> bool {
    matches!(
        n,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}

pub fn build_state_struct_layouts(
    program: &Program,
    network_yield: &std::collections::HashMap<String, bool>,
    method_callee_types: &FxHashMap<crate::resolver::SpanKey, String>,
    pattern_binding_types: &FxHashMap<crate::resolver::SpanKey, String>,
) -> std::collections::HashMap<String, crate::ast::StateStructLayout> {
    let mut table = std::collections::HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(func) => {
                let key = func.name.clone();
                if network_yield.get(&key).copied().unwrap_or(false) {
                    if let Some(layout) = walk_fn_for_state_struct_layout(
                        func,
                        None,
                        network_yield,
                        method_callee_types,
                        pattern_binding_types,
                    ) {
                        table.insert(key, layout);
                    }
                }
            }
            Item::ImplBlock(imp) => {
                let type_name = match &imp.target_type.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                    _ => continue,
                };
                for ii in &imp.items {
                    let method = match ii {
                        crate::ast::ImplItem::Method(m) => m,
                        crate::ast::ImplItem::AssocType(_) => continue,
                    };
                    let key = format!("{}.{}", type_name, method.name);
                    if network_yield.get(&key).copied().unwrap_or(false) {
                        if let Some(layout) = walk_fn_for_state_struct_layout(
                            method,
                            Some(type_name.as_str()),
                            network_yield,
                            method_callee_types,
                            pattern_binding_types,
                        ) {
                            table.insert(key, layout);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    table
}

/// Walker state for one function body's state-struct layout synthesis.
/// Mirrors `YieldPointWalker`'s scope-tracking discipline (push on binding
/// introduction; truncate on block exit) but enriches each scope slot with
/// the `SpanKey` of the pattern that introduced the binding so the
/// typechecker's `pattern_binding_types` lookup resolves at yield-point
/// snapshots. The walker accumulates a per-function field union directly
/// — duplicate (name, span) pairs across yield points are coalesced via
/// `seen`. Same-name bindings introduced at different spans (shadowing)
/// get distinct slots.
struct StateStructLayoutWalker<'a> {
    network_yield: &'a std::collections::HashMap<String, bool>,
    method_callee_types: &'a FxHashMap<crate::resolver::SpanKey, String>,
    pattern_binding_types: &'a FxHashMap<crate::resolver::SpanKey, String>,
    /// Flat stack of in-scope binding (name, introducing-pattern-span)
    /// pairs in source-introduction order. `self` carries a fixed sentinel
    /// span-key — its type comes from the impl target, not from the
    /// pattern_binding_types map.
    scope: Vec<ScopeEntry>,
    fields: Vec<crate::ast::StateStructField>,
    seen: std::collections::HashSet<ScopeEntryKey>,
    /// Flips `true` the first time the walker recognises a network-effect
    /// call site (yield point). Drives the presence rule: a network-boundary
    /// function without any concrete yield-point call in its body — even
    /// one classified by Polymorphic candidacy at the FFI primitive layer
    /// — produces no table entry, mirroring `YieldPointsTable`.
    had_yield_point: bool,
}

#[derive(Clone)]
struct ScopeEntry {
    name: String,
    /// `Some(key)` for ordinary bindings (param, let, pattern); `None`
    /// for `self` and any future synthetic binding without a recorded
    /// pattern span. When `None`, `type_override` carries the surface
    /// type directly.
    span_key: Option<crate::resolver::SpanKey>,
    /// Source `Span` of the binding's introducing pattern, threaded
    /// into `StateStructField.binding_span` so `raii_check` can anchor
    /// a "binding declared here" secondary highlight. `SpanKey` is
    /// lossy (offset+length only), so the full `Span` is carried in
    /// parallel rather than reconstructed. `None` mirrors `span_key:
    /// None` (synthetic bindings like `self`).
    binding_span: Option<crate::token::Span>,
    type_override: Option<String>,
}

#[derive(Clone, Eq, PartialEq, Hash)]
enum ScopeEntryKey {
    Span(crate::resolver::SpanKey),
    Synthetic(String),
}

pub(super) fn walk_fn_for_state_struct_layout(
    func: &crate::ast::Function,
    impl_target_type: Option<&str>,
    network_yield: &std::collections::HashMap<String, bool>,
    method_callee_types: &FxHashMap<crate::resolver::SpanKey, String>,
    pattern_binding_types: &FxHashMap<crate::resolver::SpanKey, String>,
) -> Option<crate::ast::StateStructLayout> {
    let mut walker = StateStructLayoutWalker {
        network_yield,
        method_callee_types,
        pattern_binding_types,
        scope: Vec::new(),
        fields: Vec::new(),
        seen: std::collections::HashSet::new(),
        had_yield_point: false,
    };
    if func.self_param.is_some() {
        walker.scope.push(ScopeEntry {
            name: "self".to_string(),
            span_key: None,
            binding_span: None,
            type_override: impl_target_type.map(|s| s.to_string()),
        });
    }
    for p in &func.params {
        for (name, span) in p.pattern.binding_name_spans() {
            walker.scope.push(ScopeEntry {
                name,
                span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                binding_span: Some(span),
                type_override: None,
            });
        }
    }
    walker.walk_block(&func.body);
    if walker.had_yield_point {
        Some(crate::ast::StateStructLayout {
            fields: walker.fields,
        })
    } else {
        None
    }
}

impl StateStructLayoutWalker<'_> {
    fn record_yield_point_capture(&mut self) {
        self.had_yield_point = true;
        for entry in &self.scope {
            let key = match entry.span_key {
                Some(k) => ScopeEntryKey::Span(k),
                None => ScopeEntryKey::Synthetic(entry.name.clone()),
            };
            if self.seen.insert(key) {
                let type_name = entry.type_override.clone().or_else(|| {
                    entry
                        .span_key
                        .and_then(|k| self.pattern_binding_types.get(&k).cloned())
                        // A primitive-typed field stays `None` here, which is
                        // this layout's contract: codegen falls through to its
                        // primitive-sizing path on an absent entry, and a
                        // present `"i64"` would send it down the named-type
                        // path instead.
                        //
                        // The filter became necessary when B-2026-08-11-21 made
                        // the typechecker record scalar binding types — it had
                        // to, because codegen's `%llu`/`%lld` choice reads that
                        // map and an un-annotated `let x = <u64>` printed
                        // signed without it. Every OTHER consumer wants the
                        // scalar name; this one wants its absence, so the
                        // narrowing belongs here rather than at the recording
                        // site.
                        .filter(|n| !is_scalar_primitive_type_name(n))
                });
                self.fields.push(crate::ast::StateStructField {
                    name: entry.name.clone(),
                    type_name,
                    binding_span: entry.binding_span,
                });
            }
        }
    }

    fn walk_block(&mut self, block: &crate::ast::Block) {
        let scope_mark = self.scope.len();
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    fn walk_block_with_pattern(&mut self, pat: &crate::ast::Pattern, block: &crate::ast::Block) {
        let scope_mark = self.scope.len();
        for (name, span) in pat.binding_name_spans() {
            self.scope.push(ScopeEntry {
                name,
                span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                binding_span: Some(span),
                type_override: None,
            });
        }
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    fn walk_expr_with_pattern(&mut self, pat: &crate::ast::Pattern, expr: &crate::ast::Expr) {
        let scope_mark = self.scope.len();
        for (name, span) in pat.binding_name_spans() {
            self.scope.push(ScopeEntry {
                name,
                span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                binding_span: Some(span),
                type_override: None,
            });
        }
        self.walk_expr(expr);
        self.scope.truncate(scope_mark);
    }

    fn walk_stmt(&mut self, stmt: &crate::ast::Stmt) {
        use crate::ast::StmtKind;
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, pattern, .. } => {
                self.walk_expr(value);
                for (name, span) in pattern.binding_name_spans() {
                    self.scope.push(ScopeEntry {
                        name,
                        span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                        binding_span: Some(span),
                        type_override: None,
                    });
                }
            }
            StmtKind::LetUninit {
                name, name_span, ..
            } => {
                self.scope.push(ScopeEntry {
                    name: name.clone(),
                    span_key: Some(crate::resolver::SpanKey::from_span(name_span)),
                    binding_span: Some(*name_span),
                    type_override: None,
                });
            }
            StmtKind::LetElse {
                value,
                pattern,
                else_block,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block(else_block);
                for (name, span) in pattern.binding_name_spans() {
                    self.scope.push(ScopeEntry {
                        name,
                        span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                        binding_span: Some(span),
                        type_override: None,
                    });
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Expr(expr) => self.walk_expr(expr),
        }
    }

    fn walk_expr(&mut self, expr: &crate::ast::Expr) {
        use crate::ast::ExprKind;
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(key) = callee_key_from_call(callee) {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        self.record_yield_point_capture();
                    }
                }
                self.walk_expr(callee);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                if let Some(key) = self
                    .method_callee_types
                    .get(&crate::resolver::SpanKey::from_span(&expr.span))
                    .cloned()
                {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        self.record_yield_point_capture();
                    }
                }
                self.walk_expr(object);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Question(inner) => self.walk_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object);
                if let Some(arglist) = args {
                    for arg in arglist {
                        self.walk_expr(&arg.value);
                    }
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object)
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_block(then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                pattern,
                then_block,
                else_branch,
            } => {
                self.walk_expr(value);
                self.walk_block_with_pattern(pattern, then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    if let Some(ref g) = arm.guard {
                        let scope_mark = self.scope.len();
                        for (name, span) in arm.pattern.binding_name_spans() {
                            self.scope.push(ScopeEntry {
                                name,
                                span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                                binding_span: Some(span),
                                type_override: None,
                            });
                        }
                        self.walk_expr(g);
                        self.scope.truncate(scope_mark);
                    }
                    self.walk_expr_with_pattern(&arm.pattern, &arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition);
                self.walk_block(body);
            }
            ExprKind::WhileLet {
                value,
                pattern,
                body,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block_with_pattern(pattern, body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.walk_expr(iterable);
                self.walk_block_with_pattern(pattern, body);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                self.walk_block(body)
            }
            // Closures form their own state machine — same as YieldPointWalker.
            ExprKind::Closure { .. } => {}
            ExprKind::Return(Some(e)) => self.walk_expr(e),
            ExprKind::Return(None) => {}
            ExprKind::Break { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr(v);
                }
            }
            ExprKind::Continue { .. } => {}
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
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
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
            ExprKind::Pipe { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Cast { expr, .. } => self.walk_expr(expr),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(e) = end {
                    self.walk_expr(e);
                }
            }
            ExprKind::Lock { body, .. } => self.walk_block(body),
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.walk_expr(&b.value);
                }
                self.walk_block(body);
            }
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e, _) = part {
                        self.walk_expr(e);
                    }
                }
            }
        }
    }
}

pub(super) fn effect_verb_str(v: &EffectVerbKind) -> &str {
    match v {
        EffectVerbKind::Reads => "reads",
        EffectVerbKind::Writes => "writes",
        EffectVerbKind::Sends => "sends",
        EffectVerbKind::Receives => "receives",
        EffectVerbKind::Allocates => "allocates",
        EffectVerbKind::Panics => "panics",
        EffectVerbKind::Blocks => "blocks",
        EffectVerbKind::Suspends => "suspends",
        EffectVerbKind::UserDefined(s) => s.as_str(),
    }
}

pub(super) fn ownership_mode_str(m: &OwnershipMode) -> &str {
    match m {
        OwnershipMode::Own => "own",
        OwnershipMode::Ref => "ref",
        OwnershipMode::MutRef => "mut_ref",
    }
}

struct DiagEntry<'a> {
    id: &'a str,
    severity: &'a str,
    phase: &'a str,
    code: &'a str,
    category: &'a str,
    message: &'a str,
    filename: &'a str,
    span: &'a Span,
    suggestion: Option<&'a str>,
    /// Optional pre-formatted JSON fields appended verbatim to the entry object.
    extra_json: Option<String>,
    /// Registered lint name when this entry is a warning routed through
    /// a lint (slice 7 of the lint-level entry — see
    /// `phase-5-diagnostics.md`). Surfaced as `"lint_name":"..."` in the
    /// JSON output so `karac --output=json` consumers can route, group,
    /// and filter by lint. `None` on hard errors and on warnings that
    /// haven't migrated to a registered lint yet.
    lint_name: Option<&'a str>,
    /// Machine-applicable fix-it edit when the diagnostic supplies one
    /// (`#[non_exhaustive]` slice 7 introduces the producers for the
    /// cross-package pattern and match diagnostics). Surfaced as a
    /// nested `"fix_it":{"span":{...},"replacement":"..."}` object so
    /// IDE / formatter consumers can apply it without re-parsing the
    /// message text. `None` for every other diagnostic; widens as
    /// more producers land.
    fix_it: Option<&'a crate::typechecker::FixIt>,
    /// Broad-category class label for the structured-diagnostic
    /// JSON envelope (`karac --output=json` consumers — LLM agents,
    /// IDE tooling). Auto-derived from the typechecker error's
    /// `kind` at `TypeError` construction time; the wire form is
    /// the UPPER_SNAKE `DiagnosticClass::as_str()`. Line 619 slice
    /// 4 surfaces it on every type/effect/lint diagnostic.
    class: Option<&'static str>,
    /// Display form of the *expected* type / shape at the diagnostic
    /// site, when populated by the typechecker via the typed-fields
    /// helper. Surfaces as `"expected":"i32"` in the JSON record.
    /// Line 619 slice 4.
    expected: Option<&'a str>,
    /// Display form of the *got* / actual type at the diagnostic
    /// site. Mirror of `expected`. Line 619 slice 4.
    got: Option<&'a str>,
    /// Pre-rendered JSON object for a `hints[]` entry carrying a
    /// signature-from-call-site stub diff (phase-5-diagnostics line
    /// 633). Set on unresolved-call diagnostics emitted inside a
    /// `_test.kara` file; left `None` everywhere else. The string is
    /// the inner JSON object `{"description":"…","diff":{"file":…,
    /// "line":…,"old":"","new":"…"}}` — spliced into the unified
    /// `hints` array alongside the existing `did you mean`
    /// description entry when both are present.
    stub_hint_json: Option<String>,
}

pub(super) struct DiagnosticJson {
    pub(super) entries: Vec<String>,
}

impl DiagnosticJson {
    fn new() -> Self {
        DiagnosticJson {
            entries: Vec::new(),
        }
    }

    fn add(&mut self, d: DiagEntry<'_>) {
        let mut entry = format!(
            "{{\"id\":{},\"severity\":{},\"primary\":true,\"phase\":{},\"code\":{},\"category\":{},{},\"message\":{}",
            json_string(d.id),
            json_string(d.severity),
            json_string(d.phase),
            json_string(d.code),
            json_string(d.category),
            span_to_json(d.span, d.filename),
            json_string(d.message),
        );
        // Unified `hints` array: combines the (existing) `suggestion`
        // description entry and any signature-from-call-site stub-diff
        // entry (line 633). At least one of the two must be set for
        // the field to appear; both can coexist on the same
        // diagnostic (e.g. an unresolved-call in a test file that
        // also has a `did you mean` neighbour).
        let mut hints: Vec<String> = Vec::new();
        if let Some(s) = d.suggestion {
            hints.push(format!("{{\"description\":{}}}", json_string(s)));
        }
        if let Some(ref sj) = d.stub_hint_json {
            hints.push(sj.clone());
        }
        if !hints.is_empty() {
            write!(entry, ",\"hints\":[{}]", hints.join(",")).unwrap();
        }
        if let Some(name) = d.lint_name {
            write!(entry, ",\"lint_name\":{}", json_string(name)).unwrap();
        }
        if let Some(class) = d.class {
            write!(entry, ",\"class\":{}", json_string(class)).unwrap();
        }
        if let Some(expected) = d.expected {
            write!(entry, ",\"expected\":{}", json_string(expected)).unwrap();
        }
        if let Some(got) = d.got {
            write!(entry, ",\"got\":{}", json_string(got)).unwrap();
        }
        if let Some(fix) = d.fix_it {
            // `#[non_exhaustive]` slice 7 — surface the
            // machine-applicable edit as a nested object. `length` is
            // included so consumers can distinguish insertion
            // (length=0) from replacement without re-deriving from
            // start/end markers.
            write!(
                entry,
                ",\"fix_it\":{{\"span\":{{{},\"offset\":{},\"length\":{}}},\"replacement\":{}}}",
                span_to_json(&fix.span, d.filename),
                fix.span.offset,
                fix.span.length,
                json_string(&fix.replacement),
            )
            .unwrap();
            // Line 619 slice 5 — also emit the multi-edit `fixes`
            // array form per the structured-diagnostic spec. The
            // single-edit `fix_it` field stays for back-compat with
            // existing consumers; the array form is what new LLM /
            // IDE consumers should consume going forward. Each fix
            // carries a `description` (derived from the lint name
            // when available, else a generic "apply suggested
            // edit") and an `edits` array of `{span, replacement}`
            // entries. v1 ships one entry per fix; the array shape
            // is forward-compatible with multi-edit fixes when they
            // land.
            let description = d.lint_name.unwrap_or("apply suggested edit");
            write!(
                entry,
                ",\"fixes\":[{{\"description\":{},\"edits\":[{{\"span\":{{{},\"offset\":{},\"length\":{}}},\"replacement\":{}}}]}}]",
                json_string(description),
                span_to_json(&fix.span, d.filename),
                fix.span.offset,
                fix.span.length,
                json_string(&fix.replacement),
            )
            .unwrap();
        }
        if let Some(ref extra) = d.extra_json {
            write!(entry, ",{}", extra).unwrap();
        }
        entry.push('}');
        self.entries.push(entry);
    }

    pub(super) fn to_json_array(&self) -> String {
        if self.entries.is_empty() {
            return "[]".to_string();
        }
        format!("[{}]", self.entries.join(","))
    }
}

/// Munge the path of a `_test.kara` file to its sibling production
/// file: `src/math_test.kara` → `src/math.kara`. Returns the input
/// unchanged when the basename does not match the `_test.kara`
/// convention — defensive fallback so a future test-file convention
/// change does not silently mis-route the stub diff.
pub(super) fn sibling_production_file(test_path: &str) -> String {
    // Split the last path component so the `_test.kara` suffix swap
    // does not touch directory names containing `_test` substrings.
    if let Some(stripped) = test_path.strip_suffix("_test.kara") {
        format!("{stripped}.kara")
    } else {
        test_path.to_string()
    }
}

/// Best-effort line count for the sibling production file. Used as the
/// `line` field of the stub-hint diff so the consumer (LLM agent / IDE)
/// knows where in the file the insertion lands. When the file does not
/// exist yet (pure-TDD opener: test file written first, production
/// file not yet created), returns `1` so the diff describes "create
/// the file with this body."
pub(super) fn target_append_line(target_file: &str) -> u32 {
    match std::fs::read_to_string(target_file) {
        Ok(contents) => {
            // Append after the last existing line. Line count + 1 even
            // when the file ends with a trailing newline — the new
            // content lands on the line *after* the trailing newline.
            let line_count = contents.lines().count();
            (line_count as u32) + 1
        }
        Err(_) => 1,
    }
}

/// Render a single `hints[]` entry carrying a signature-from-call-site
/// stub diff (phase-5-diagnostics line 633). The output is the inner
/// JSON object — the surrounding `[ ]` is added by `DiagnosticJson::add`
/// when assembling the unified hints array.
pub(super) fn render_stub_hint_json(filename: &str, hint: &crate::resolver::StubHint) -> String {
    let target_file = sibling_production_file(filename);
    let line = target_append_line(&target_file);
    let new_source = hint.render_source();
    let description = format!(
        "stub `{}` in {} with inferred signature",
        hint.callee_name, target_file
    );
    format!(
        "{{\"description\":{},\"diff\":{{\"file\":{},\"line\":{},\"old\":{},\"new\":{}}}}}",
        json_string(&description),
        json_string(&target_file),
        line,
        json_string(""),
        json_string(&new_source),
    )
}

pub(super) fn collect_diagnostics(pipeline: &Pipeline) -> DiagnosticJson {
    let mut diags = DiagnosticJson::new();
    let filename = &pipeline.filename;
    let mut id_counter = 0u32;

    for err in &pipeline.parsed.errors {
        id_counter += 1;
        // Parse-phase machine-applicable fix (e.g. delete a stray comma in a
        // comma-separated `with` clause), matched to this diagnostic by span.
        // Same `"replacement":{offset,length,text}` shape the resolver emits,
        // so `karac fix` and IDE quick-fix consumers read it uniformly.
        let replacement_json = pipeline
            .parsed
            .fix_edits
            .get(&crate::resolver::SpanKey::from_span(&err.span))
            .map(|e| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    e.offset,
                    e.length,
                    json_string(&e.replacement),
                )
            });
        // Multi-edit parse envelope, rendered exactly like the resolver's and
        // ownership's `"fix_diff":[{...},{...}]`. Mutually exclusive with
        // `replacement` per diagnostic — a repair is one-sided or it is not.
        let fix_diff_json = pipeline
            .parsed
            .fix_diffs
            .get(&crate::resolver::SpanKey::from_span(&err.span))
            .filter(|v| !v.is_empty())
            .map(|edits| {
                let items: Vec<String> = edits
                    .iter()
                    .map(|e| {
                        format!(
                            "{{\"offset\":{},\"length\":{},\"text\":{}}}",
                            e.offset,
                            e.length,
                            json_string(&e.replacement),
                        )
                    })
                    .collect();
                format!("\"fix_diff\":[{}]", items.join(","))
            });
        let parse_extra_json = match (replacement_json, fix_diff_json) {
            (Some(rep), Some(f)) => Some(format!("{rep},{f}")),
            (Some(rep), None) => Some(rep),
            (None, Some(f)) => Some(f),
            (None, None) => None,
        };
        diags.add(DiagEntry {
            id: &format!("d{id_counter}"),
            severity: "error",
            phase: "parse",
            code: err.kind.code(),
            category: "parse",
            message: &err.message,
            filename,
            span: &err.span,
            suggestion: None,
            extra_json: parse_extra_json,
            lint_name: None,
            fix_it: None,
            class: None,
            expected: None,
            got: None,
            stub_hint_json: None,
        });
    }

    if let Some(ref r) = pipeline.resolved {
        for err in &r.errors {
            id_counter += 1;
            let code = match err.kind {
                crate::resolver::ResolveErrorKind::UndefinedName => "E0100",
                crate::resolver::ResolveErrorKind::DuplicateDefinition => "E0101",
                crate::resolver::ResolveErrorKind::ReservedIdentifier => "E0102",
                crate::resolver::ResolveErrorKind::PrivateAccess => "E0103",
                crate::resolver::ResolveErrorKind::UndefinedType => "E0104",
                crate::resolver::ResolveErrorKind::UndefinedVariant => "E0105",
                crate::resolver::ResolveErrorKind::UndefinedField => "E0106",
                crate::resolver::ResolveErrorKind::UndefinedLabel => "E0107",
                crate::resolver::ResolveErrorKind::OperatorTraitImplRestricted => "E0108",
                crate::resolver::ResolveErrorKind::IntoTraitImplNotAllowed => "E0109",
                crate::resolver::ResolveErrorKind::ImplLevelEffectVarNotAllowed => "E0110",
                crate::resolver::ResolveErrorKind::UnknownModule => "E0112",
                crate::resolver::ResolveErrorKind::UnknownItemInModule => "E0113",
                crate::resolver::ResolveErrorKind::PrivateItemAccess => "E0111",
                crate::resolver::ResolveErrorKind::AmbiguousWildcardImport => "E0124",
                crate::resolver::ResolveErrorKind::ReservedEffectResource => "E0114",
                crate::resolver::ResolveErrorKind::CompilerBuiltinReserved => "E0115",
                crate::resolver::ResolveErrorKind::ContinueOnBlockLabel => "E0116",
                crate::resolver::ResolveErrorKind::NonExhaustiveInvalidTarget => "E0117",
                crate::resolver::ResolveErrorKind::TrackCallerInvalidTarget => "E0118",
                crate::resolver::ResolveErrorKind::GpuInvalidTarget => "E0800",
                crate::resolver::ResolveErrorKind::CodegenHintInvalidTarget => {
                    "E_CODEGEN_HINT_INVALID_POSITION"
                }
                crate::resolver::ResolveErrorKind::CodegenHintOnExternDecl => {
                    "E_CODEGEN_HINT_ON_EXTERN_DECL"
                }
                crate::resolver::ResolveErrorKind::DeprecatedOnImpl => "E0119",
                crate::resolver::ResolveErrorKind::DeprecatedOnField => "E0120",
                crate::resolver::ResolveErrorKind::UnknownAttribute => "E0121",
                crate::resolver::ResolveErrorKind::ProfileInvalidTarget => "E0122",
                crate::resolver::ResolveErrorKind::UnknownProfile => "E0123",
                crate::resolver::ResolveErrorKind::QueryResolutionConflict => {
                    "E_QUERY_RESOLUTION_CONFLICT"
                }
                crate::resolver::ResolveErrorKind::UnionNonExhaustiveForbidden => {
                    "E_UNION_NON_EXHAUSTIVE_FORBIDDEN"
                }
                crate::resolver::ResolveErrorKind::DefaultAttributeInvalidPosition => {
                    "E_DEFAULT_ATTRIBUTE_INVALID_POSITION"
                }
                crate::resolver::ResolveErrorKind::DefaultAttributeWithoutDerive => {
                    "E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE"
                }
                crate::resolver::ResolveErrorKind::MalformedAttributeArgs => {
                    "E_MALFORMED_ATTRIBUTE_ARGS"
                }
            };
            // Surface the machine-applicable replacement (when present)
            // alongside the human-readable suggestion. Consumers like
            // `karac fix` and IDE quick-fix UIs read this directly to
            // produce one-click rewrites.
            let replacement_json = err.replacement.as_ref().map(|r| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    r.offset,
                    r.length,
                    json_string(&r.replacement),
                )
            });
            // Multi-edit fix envelope (B-2026-07-31-33), rendered exactly
            // like the ownership channel's: `"fix_diff":[{...},{...}]`.
            // `E_MODULE_BINDING_NAMING` uses it for a rename that spans the
            // declaration plus every use site. Mutually exclusive with
            // `replacement` per diagnostic — see `ResolveResult`.
            let fix_diff_json = r
                .error_fix_diffs
                .get(&crate::resolver::SpanKey::from_span(&err.span))
                .filter(|v| !v.is_empty())
                .map(|edits| {
                    let items: Vec<String> = edits
                        .iter()
                        .map(|e| {
                            format!(
                                "{{\"offset\":{},\"length\":{},\"text\":{}}}",
                                e.offset,
                                e.length,
                                json_string(&e.replacement),
                            )
                        })
                        .collect();
                    format!("\"fix_diff\":[{}]", items.join(","))
                });
            let extra_json = match (replacement_json, fix_diff_json) {
                (Some(rep), Some(f)) => Some(format!("{rep},{f}")),
                (Some(rep), None) => Some(rep),
                (None, Some(f)) => Some(f),
                (None, None) => None,
            };
            let stub_hint_json = err
                .stub_hint
                .as_ref()
                .map(|s| render_stub_hint_json(filename, s));
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "resolve",
                code,
                category: "resolve",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: err.suggestion.as_deref(),
                extra_json,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json,
            });
        }
    }

    if let Some(ref t) = pipeline.typed {
        for err in &t.errors {
            id_counter += 1;
            let code = match err.kind {
                crate::typechecker::TypeErrorKind::TypeMismatch => "E0200",
                crate::typechecker::TypeErrorKind::UndefinedField => "E0201",
                crate::typechecker::TypeErrorKind::WrongNumberOfArgs => "E0202",
                crate::typechecker::TypeErrorKind::MissingField => "E0203",
                crate::typechecker::TypeErrorKind::ExtraField => "E0204",
                crate::typechecker::TypeErrorKind::NonExhaustiveMatch => "E0205",
                crate::typechecker::TypeErrorKind::NotCallable => "E0206",
                crate::typechecker::TypeErrorKind::NotAStruct => "E0207",
                crate::typechecker::TypeErrorKind::InvalidBinaryOp => "E0208",
                crate::typechecker::TypeErrorKind::InvalidUnaryOp => "E0209",
                crate::typechecker::TypeErrorKind::InvalidCast => "E0210",
                crate::typechecker::TypeErrorKind::ConditionNotBool => "E0211",
                crate::typechecker::TypeErrorKind::BranchTypeMismatch => "E0212",
                crate::typechecker::TypeErrorKind::ReturnTypeMismatch => "E0213",
                crate::typechecker::TypeErrorKind::InvalidTupleIndex => "E0214",
                crate::typechecker::TypeErrorKind::LabelMismatch => "E0215",
                crate::typechecker::TypeErrorKind::NonContiguousLabels => "E0216",
                crate::typechecker::TypeErrorKind::InvalidPipePlaceholder => "E0217",
                crate::typechecker::TypeErrorKind::MissingMutMarker => "E0218",
                crate::typechecker::TypeErrorKind::InvalidMutMarker => "E0219",
                crate::typechecker::TypeErrorKind::UnsupportedNumericSuffix => "E0220",
                crate::typechecker::TypeErrorKind::PrivateTypeInPublicSignature => "E0221",
                crate::typechecker::TypeErrorKind::RefutablePattern => "E0222",
                crate::typechecker::TypeErrorKind::MissingSupertrait => "E0229",
                crate::typechecker::TypeErrorKind::TraitBoundNotSatisfied => "E0232",
                crate::typechecker::TypeErrorKind::AmbiguousAssocFn => "E0233",
                crate::typechecker::TypeErrorKind::CannotInferAssocFn => "E0234",
                crate::typechecker::TypeErrorKind::OnceFnIntoFnSlot => "E0235",
                crate::typechecker::TypeErrorKind::NoMethodFound => "E0236",
                crate::typechecker::TypeErrorKind::UnreachableArm => "W0237",
                crate::typechecker::TypeErrorKind::RefinementDomainTooWide => "W0238",
                crate::typechecker::TypeErrorKind::CannotInferTypeParam => "E0238",
                crate::typechecker::TypeErrorKind::AmbiguousMethod => "E0239",
                crate::typechecker::TypeErrorKind::AmbiguousBareVariant => "E0279",
                crate::typechecker::TypeErrorKind::ConflictingImpl => "E0240",
                crate::typechecker::TypeErrorKind::NonExhaustiveCrossPackageLiteral => "E0241",
                crate::typechecker::TypeErrorKind::NonExhaustiveCrossPackageMatch => "E0242",
                crate::typechecker::TypeErrorKind::NonExhaustiveCrossPackagePattern => "E0243",
                crate::typechecker::TypeErrorKind::UnknownLint => "W0244",
                // `Deprecated` only appears as a warning under default
                // settings; if `#[deny(deprecated)]` promotes it to an
                // error the same code is reused as `E0245`.
                crate::typechecker::TypeErrorKind::Deprecated => "E0245",
                // `MissingNonExhaustive` is `Deny`-by-default per
                // `STARTER_LINTS`, so it normally surfaces as an error
                // (W-prefixed because the underlying carrier is a lint).
                crate::typechecker::TypeErrorKind::MissingNonExhaustive => "W0246",
                // Lint-level slice 4b polish — emitted only when the
                // CLI sets `-F NAME` and an inner `#[allow(NAME)]`
                // is rejected; never appears as a warning (the
                // diagnostic is a hard error by construction).
                crate::typechecker::TypeErrorKind::ForbiddenLintAllow => "E0247",
                // Lint-level slice 5 — `#[expect(unfulfilled_lint_expectation)]`
                // rejection (would be circular).
                crate::typechecker::TypeErrorKind::ExpectOnUnfulfilled => "E0248",
                // Lint-level slice 5 — appears on the errors path only
                // when promoted via `#[deny(unfulfilled_lint_expectation)]`.
                crate::typechecker::TypeErrorKind::UnfulfilledLintExpectation => "E0249",
                // Module-level `let` / `let mut` slice 4 — see
                // `docs/implementation_checklist/phase-8-stdlib-floor.md`
                // mod-let entry. The const-init structural rule and the
                // §1297 heap-String rejection both surface here.
                crate::typechecker::TypeErrorKind::ModuleBindingEffectfulInit => "E0250",
                crate::typechecker::TypeErrorKind::ModuleBindingHeapType => "E0251",
                // Slice 5 — assignment to a module-level immutable `let`.
                crate::typechecker::TypeErrorKind::ReassignToImmutableModuleBinding => "E0252",
                // Phase 6 line 218 slice 2 — ScopeLocal escape
                // diagnostic (design.md § ScopeLocal). Fires when a
                // `ScopeLocal` marker-trait type appears in a
                // function return, struct/enum field, or
                // `Sender.send` argument.
                crate::typechecker::TypeErrorKind::ScopeLocalEscape => "E0253",
                // Phase 6 line 170 slice 3a — cross-task-safe boundary
                // check at `spawn(closure)` / `TaskGroup.spawn(closure)`
                // call sites. Fires when a captured binding's type
                // reaches a cross-task-unsafe leaf (`Rc[T]`, `shared`,
                // `OnceCell[T]`, raw pointer) per
                // `src/cross_task_safe.rs`'s closed structural list.
                crate::typechecker::TypeErrorKind::CrossTaskUnsafeCapture => "E0254",
                // Phase-8 line 49 — `#[unstable]` use-site lint
                // promoted to error via `#[deny(unstable_api)]`.
                // Reuses the same numeric slot as the warning
                // (`W0255`) by convention with `Deprecated`.
                crate::typechecker::TypeErrorKind::UnstableApi => "E0255",
                // Phase 9 line 25 step 1 — a refinement type's `where`
                // predicate uses a construct outside the allowed
                // constraint language (design.md § Refinement Types).
                crate::typechecker::TypeErrorKind::InvalidRefinementPredicate => "E0256",
                // Phase 6 `par struct` slice A — a `mut` field of a
                // `par struct` / `par enum` is not `Atomic[T]` / `Mutex[T]`
                // (design.md § Part 5b > Field constraints).
                crate::typechecker::TypeErrorKind::ParFieldNotConcurrent => "E0257",
                // Phase 6 `par struct` slice A — a `par struct` / `par enum`
                // method declares a `mut self` receiver; only `ref self` (and
                // consuming `self`) are permitted because `par` values are
                // always Arc with potential multiple holders (design.md
                // § Part 5b > `ref self` receivers only).
                crate::typechecker::TypeErrorKind::ParMutSelfReceiver => "E0258",
                // E0259 retired: a `lock` block body MAY now contain early exits
                // (`return` / `break` / `continue`) — codegen seeds the release
                // as a cleanup-frame action so it fires on every exit path.
                // Phase 6 `Mutex` / `lock` — the `lock` target is not a
                // `Mutex[T]` binding.
                crate::typechecker::TypeErrorKind::LockTargetNotMutex => "E0260",
                // `#[repr(transparent)]` carrier-shape violation (design.md §
                // `#[repr(transparent)]` for distinct-type FFI). One numeric
                // code for the family; the specific `E_REPR_TRANSPARENT_*`
                // symbolic code is in the message.
                crate::typechecker::TypeErrorKind::ReprTransparentInvalid => "E0803",
                crate::typechecker::TypeErrorKind::ExternSignatureInvalid => "E0805",
                crate::typechecker::TypeErrorKind::DiscriminantInvalid => "E0804",
                // Phase 8 `@` bindings slice 4 — owned scrutinee, outer
                // `@` alias and an inner sub-pattern binding both claim
                // non-Copy ownership of overlapping content (design.md
                // § @ Bindings > Owned scrutinee).
                crate::typechecker::TypeErrorKind::AtBindingDoubleConsume => "E0261",
                // Type Aliases (v60 item 50) — a generic alias use-site arg
                // fails a trait bound declared on the alias parameter.
                crate::typechecker::TypeErrorKind::TypeAliasBoundNotSatisfied => "E0262",
                // Range Patterns (v60 item 51) — a const-named range bound
                // does not resolve to a module-level int/char const.
                crate::typechecker::TypeErrorKind::RangePatternBoundNotConst => "E0263",
                // Fallible Allocation (v60 item 46) — a panicking heap-allocating
                // operation appears under `panic_on_alloc_failure = false`.
                crate::typechecker::TypeErrorKind::PanickingAllocRejected => "E0264",
                // Fallible Allocation (v60 item 46) — `#[derive(Clone)]` whose
                // synthesized clone may panic on allocation failure under
                // `panic_on_alloc_failure = false`.
                crate::typechecker::TypeErrorKind::DeriveCloneAllocates => "E0265",
                // Phase-8 entry-point contract Slice C — `main()` declares a
                // return type outside `()` / `Result[(), E: Display]` /
                // `ExitCode` (design.md § Entry Point).
                crate::typechecker::TypeErrorKind::MainReturnType => "E0266",
                // Slice C — `main() -> Result[(), E]` where `E` lacks `Display`.
                crate::typechecker::TypeErrorKind::MainErrNotDisplay => "E0267",
                // `s[i]` (scalar index) on a `String` — UTF-8 is
                // variable-width, so `[]` is rejected in favour of
                // `s.char_at(i)` / `s.bytes()[i]` (design.md § Character type).
                crate::typechecker::TypeErrorKind::StringNotIndexable => "E0268",
                crate::typechecker::TypeErrorKind::IteratorNotIndexable => "E0274",
                crate::typechecker::TypeErrorKind::TypeNotIndexable => "E0275",
                crate::typechecker::TypeErrorKind::NilCoalesceNotWrapped => "E0276",
                crate::typechecker::TypeErrorKind::OptionalChainNotOption => "E0277",
                // B-2026-06-30-3 — reassignment of a non-`mut` field on a
                // `shared struct` / `par struct` (design.md § Shared Types).
                crate::typechecker::TypeErrorKind::SharedFieldNotMut => "E0269",
                // An `Atomic[T]` op (`load`/`store`/`fetch_*`/`swap`) called
                // without its required explicit `MemoryOrdering` argument
                // (deferred.md § Atomic Operations — no implicit-ordering form).
                crate::typechecker::TypeErrorKind::AtomicMissingOrdering => "E0270",
                // A return-position `impl Trait` yielding 2+ distinct concrete
                // witnesses (design.md § `impl Trait`: one witness per
                // monomorphization). Run-fatal (B-2026-07-08-1).
                crate::typechecker::TypeErrorKind::ImplTraitMultipleWitnesses => "E0271",
                // `Atomic[T]` with a non-atomic inner type (`T` must be an
                // integer, `bool`, or raw pointer). Run-fatal: codegen crashes
                // the LLVM verifier while the interpreter silently accepts it.
                crate::typechecker::TypeErrorKind::AtomicInvalidInnerType => "E0272",
                // FE-2 — a `#[gpu]` function uses a non-GPU-safe type.
                crate::typechecker::TypeErrorKind::GpuNotSafe => "E0801",
                // B-2026-07-17-6 — an enum-variant pattern (`Some(v)`, `Ok(e)`,
                // `Color.Red`, or a bare unit-variant name like `None`) matched
                // against a scrutinee whose type cannot own that variant.
                crate::typechecker::TypeErrorKind::PatternScrutineeMismatch => "E0273",
            };
            // Also surface a typecheck fix-it as the top-level
            // `"replacement":{offset,length,text}` shape every other phase
            // (resolver/parse/effect/ownership) uses, so `karac fix` and the
            // Mend loop detect typecheck fixes uniformly. The nested
            // `"fix_it"`/`"fixes"` forms below stay for IDE consumers.
            let replacement_json = err.fix_it.as_ref().map(|f| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    f.span.offset,
                    f.span.length,
                    json_string(&f.replacement),
                )
            });
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "typecheck",
                code,
                category: "typecheck",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: None,
                extra_json: replacement_json,
                lint_name: err.lint_name.as_deref(),
                fix_it: err.fix_it.as_ref(),
                class: Some(err.class.map(|c| c.as_str()).unwrap_or("OTHER")),
                expected: err.expected.as_deref(),
                got: err.got.as_deref(),
                stub_hint_json: None,
            });
        }
        for warn in &t.warnings {
            id_counter += 1;
            let code = match warn.kind {
                crate::typechecker::TypeErrorKind::UnreachableArm => "W0237",
                crate::typechecker::TypeErrorKind::RefinementDomainTooWide => "W0238",
                crate::typechecker::TypeErrorKind::UnknownLint => "W0244",
                crate::typechecker::TypeErrorKind::Deprecated => "W0245",
                crate::typechecker::TypeErrorKind::MissingNonExhaustive => "W0246",
                crate::typechecker::TypeErrorKind::UnfulfilledLintExpectation => "W0249",
                crate::typechecker::TypeErrorKind::UnstableApi => "W0255",
                // Other kinds aren't expected to appear as warnings today.
                _ => "W0299",
            };
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "warning",
                phase: "typecheck",
                code,
                category: "typecheck",
                message: &warn.message,
                filename,
                span: &warn.span,
                suggestion: None,
                extra_json: None,
                lint_name: warn.lint_name.as_deref(),
                fix_it: warn.fix_it.as_ref(),
                class: Some(warn.class.map(|c| c.as_str()).unwrap_or("OTHER")),
                expected: warn.expected.as_deref(),
                got: warn.got.as_deref(),
                stub_hint_json: None,
            });
        }
    }

    // B-2026-08-17-37 — the `must_use` lint reaches the JSON feed. It used to
    // run ONLY from `cmd_run`, so `karac check --output=json` emitted
    // `"diagnostics":[]` for a program `karac run` warned about — and since
    // CLAUDE.md's Mend loop is built on exactly that feed, the whole category
    // was invisible to it: an author never saw the diagnostic, `karac fix`
    // never got to offer the `let _ =` repair, and the gap could not show up
    // in the machine-fix statistics, because a diagnostic that is never
    // emitted also never counts as unfixed. design.md § must_use calls this a
    // COMPILE warning, so the compile path is where it belongs.
    //
    // Runs off the same inputs as the `cmd_run` call site (`typed` for the
    // `expr_types` lookups that recognise `Option`/`Result`), and honours
    // `-A must_use` through the shared overrides, so text and JSON agree.
    for diag in crate::must_use_lint::check_implicit_must_use(
        &pipeline.parsed.program,
        pipeline.typed.as_ref(),
        &pipeline.lint_overrides,
    ) {
        id_counter += 1;
        let is_error = diag.level == crate::must_use_lint::LintLevel::Error;
        diags.add(DiagEntry {
            id: &format!("d{id_counter}"),
            severity: if is_error { "error" } else { "warning" },
            phase: "lint",
            // W0278/E0278, NOT the W0250/E0250 this lint shipped with:
            // `E0250` is ALREADY the typecheck `ModuleBindingEffectfulInit`,
            // registered under that meaning in `explain.rs`'s code table, so a
            // `must_use` escalated by `-D must_use` emitted a code `karac
            // explain` answers for an unrelated module-binding error. The pair
            // moves together — a lint whose warn and error codes differ in
            // number would be its own wart, and `W0250` is not referenced by
            // the explain table or any consumer in-tree. B-2026-08-18-17.
            code: if is_error { "E0278" } else { "W0278" },
            category: "lint",
            message: &diag.message,
            filename,
            span: &diag.span,
            suggestion: diag.help.as_deref(),
            extra_json: None,
            lint_name: Some(&diag.lint_name),
            // No machine-applicable edit: the repair depends on intent —
            // `let _ = m.get(1);` to discard deliberately, or using the value.
            // Offering one would be a guess at which the author meant.
            fix_it: None,
            class: Some("LINT_WARNING"),
            expected: None,
            got: None,
            stub_hint_json: None,
        });
    }

    // B-2026-08-18-2 — the JSON twin of the three sibling lints wired onto the
    // compile path in `cli.rs`. Same lints, same overrides, same order, so the
    // two renderings cannot disagree; see that call site for why these three
    // and not the other two.
    //
    // W0259/E0259 rather than reusing must_use's pair — a separate number per
    // lint, which is the convention `deprecated` (W0245/E0245) follows. The
    // collision this comment used to warn about (must_use shipping on
    // `E0250`, already the typecheck `ModuleBindingEffectfulInit`) is fixed:
    // must_use moved to W0278/E0278. `lint_name` remains the addressable key
    // for `-A` / `-D` either way. B-2026-08-18-17.
    for (lint_name, is_error, message, span) in lint_entries_for_compile_path(pipeline) {
        id_counter += 1;
        diags.add(DiagEntry {
            id: &format!("d{id_counter}"),
            severity: if is_error { "error" } else { "warning" },
            phase: "lint",
            code: if is_error { "E0259" } else { "W0259" },
            category: "lint",
            message: &message,
            filename,
            span: &span,
            suggestion: None,
            extra_json: None,
            lint_name: Some(&lint_name),
            // No machine-applicable edit for any of the three. A `// Safety:`
            // justification is prose only the author can write; an
            // `unsafe { }` wrap and an FFI float comparison both depend on
            // intent. `karac fix` offering a guess here would be worse than
            // offering nothing.
            fix_it: None,
            class: Some("LINT_WARNING"),
            expected: None,
            got: None,
            stub_hint_json: None,
        });
    }

    if let Some(ref e) = pipeline.effects {
        for err in &e.errors {
            id_counter += 1;
            let (code, severity) = match err.kind {
                crate::effectchecker::EffectErrorKind::MissingEffectDeclaration => {
                    ("E0400", "error")
                }
                crate::effectchecker::EffectErrorKind::OverDeclaredEffect => ("E0401", "error"),
                crate::effectchecker::EffectErrorKind::CircularEffectGroup => ("E0402", "error"),
                crate::effectchecker::EffectErrorKind::UndefinedEffectGroup => ("E0403", "error"),
                crate::effectchecker::EffectErrorKind::EffectSubtypeViolation => ("E0404", "error"),
                crate::effectchecker::EffectErrorKind::ProfileViolation => ("E0405", "error"),
                crate::effectchecker::EffectErrorKind::ImplExceedsTraitCeiling => {
                    ("E0230", "error")
                }
                crate::effectchecker::EffectErrorKind::TraitDefaultExceedsCeiling => {
                    ("E0231", "error")
                }
                crate::effectchecker::EffectErrorKind::FfiLintHint => ("L0001", "note"),
                crate::effectchecker::EffectErrorKind::EffectVariableConflict => ("E0406", "error"),
                crate::effectchecker::EffectErrorKind::ProfileIncompatibleEffect => {
                    ("E0407", "error")
                }
                // Sibling of E0407 — same guarantee, declared per function
                // with `#[no_effect(...)]` instead of via a profile.
                crate::effectchecker::EffectErrorKind::NoEffectViolated => ("E0416", "error"),
                crate::effectchecker::EffectErrorKind::ModuleBindingWriteInPar => {
                    ("E0408", "error")
                }
                crate::effectchecker::EffectErrorKind::PubFnSyntheticResource => ("E0409", "error"),
                crate::effectchecker::EffectErrorKind::ForbiddenEffectInContract => {
                    ("E0410", "error")
                }
                crate::effectchecker::EffectErrorKind::TargetGateViolation => ("E0411", "error"),
                crate::effectchecker::EffectErrorKind::ResourceReceiverContradiction => {
                    ("E0412", "error")
                }
                crate::effectchecker::EffectErrorKind::ExternCUnwindRequiresPanics => {
                    ("E0413", "error")
                }
                crate::effectchecker::EffectErrorKind::ExternExportSuspendsUnsupported => {
                    ("E0414", "error")
                }
                crate::effectchecker::EffectErrorKind::ExternCUnwindRequiresUnwindProfile => {
                    ("E0415", "error")
                }
                crate::effectchecker::EffectErrorKind::GpuEffectViolation => ("E0802", "error"),
            };
            let subtype_json = err.subtype_trace.as_ref().map(|t| {
                let slot = json_string_list(&t.slot_effects);
                let arg = json_string_list(&t.argument_effects);
                let offending = json_string_list(&t.offending_effects);
                let signature_json = match &t.monomorphized_signature {
                    Some(sig) => format!(",\"signature\":{}", json_string(sig)),
                    None => String::new(),
                };
                format!(
                    "\"effect-subset-fail\":{{\"slot\":{slot},\"argument\":{arg},\"offending\":{offending}{signature_json}}}"
                )
            });
            // Surface the machine-applicable replacement (when present)
            // alongside the structured subtype trace — same payload shape
            // as the resolver/ownership `replacement` field, so `karac
            // fix` and IDE quick-fix consumers handle all three phases
            // uniformly. The two never co-occur today (trace ⇒ E0404,
            // replacement ⇒ E0412) but the merge is future-proof.
            let replacement_json = err.replacement.as_deref().map(|r| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    r.offset,
                    r.length,
                    json_string(&r.replacement),
                )
            });
            let extra_json = match (subtype_json, replacement_json) {
                (Some(a), Some(b)) => Some(format!("{a},{b}")),
                (a, b) => a.or(b),
            };
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity,
                phase: "effect",
                code,
                category: "effects",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: None,
                extra_json,
                lint_name: None,
                fix_it: None,
                class: crate::effectchecker::class_for_effect_error_kind(&err.kind)
                    .map(|c| c.as_str()),
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref o) = pipeline.ownership {
        for err in &o.errors {
            id_counter += 1;
            let code = match err.kind {
                crate::ownership::OwnershipErrorKind::UseAfterMove => "E0500",
                crate::ownership::OwnershipErrorKind::OwnershipCycle => "E0501",
                crate::ownership::OwnershipErrorKind::NoRcViolation => "E0502",
                crate::ownership::OwnershipErrorKind::RcFallbackNote => "N0503",
                crate::ownership::OwnershipErrorKind::CaptureModeViolation => "E0504",
                crate::ownership::OwnershipErrorKind::UseOfUninitialized => "E0505",
                crate::ownership::OwnershipErrorKind::ReassignToImmutable => "E0506",
                crate::ownership::OwnershipErrorKind::MutateImmutableBinding => "E0510",
                crate::ownership::OwnershipErrorKind::FrozenParamEscapes => "E0511",
                crate::ownership::OwnershipErrorKind::FrozenTypeNotFreezable => "E0512",
                crate::ownership::OwnershipErrorKind::UnusedMutCaptureNote => "N0507",
                crate::ownership::OwnershipErrorKind::RefCaptureEscapesScope => "E0508",
                crate::ownership::OwnershipErrorKind::SliceFromTemporaryEscapes => {
                    "E_SLICE_FROM_TEMPORARY_ESCAPES"
                }
                crate::ownership::OwnershipErrorKind::SliceBorrowConflict { .. } => {
                    "E_SLICE_BORROW_CONFLICT"
                }
                crate::ownership::OwnershipErrorKind::CrossBorrowConflict => {
                    "E_CROSS_BORROW_CONFLICT"
                }
                crate::ownership::OwnershipErrorKind::ClosureCaptureBorrowConflict => {
                    "E_CLOSURE_CAPTURE_BORROW_CONFLICT"
                }
                crate::ownership::OwnershipErrorKind::RcBudgetExceeded { .. } => {
                    "E_RC_BUDGET_EXCEEDED"
                }
                crate::ownership::OwnershipErrorKind::ConcurrentSharedStruct { .. } => {
                    "E_CONCURRENT_SHARED_STRUCT"
                }
                crate::ownership::OwnershipErrorKind::ConcurrentPlainStruct { .. } => {
                    "E_CONCURRENT_PLAIN_STRUCT"
                }
                crate::ownership::OwnershipErrorKind::BorrowReturnNotSourcePinned { .. } => "E0509",
                crate::ownership::OwnershipErrorKind::RcFallbackAllocatesUnderFallibleProfile => {
                    "E_RC_FALLBACK_ALLOCATES_UNDER_FALLIBLE_PROFILE"
                }
                crate::ownership::OwnershipErrorKind::ExclusiveBorrowAliasedArgs => {
                    "E_EXCLUSIVE_BORROW_ALIASED_ARGS"
                }
            };
            let replacement_json = err.replacement.as_ref().map(|r| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    r.offset,
                    r.length,
                    json_string(&r.replacement),
                )
            });
            // Phase-7 line 197 follow-up: multi-edit fix_diff envelope.
            // `ConcurrentSharedStruct` / `ConcurrentPlainStruct` carry
            // their per-mut-field `Mutex[T]` wrap edits in the sibling
            // `error_fix_diffs` map keyed by the diagnostic's primary
            // span. Render as a JSON array `"fix_diff":[{...},{...}]`
            // and splice into the diagnostic's extra_json slot. The
            // single-edit `replacement` and multi-edit `fix_diff` are
            // mutually exclusive in v1 (the new kinds emit
            // `replacement: None`), so either-or is sufficient — when
            // a future kind needs both, this site combines them.
            let fix_diff_json = o
                .error_fix_diffs
                .get(&crate::resolver::SpanKey::from_span(&err.span))
                .filter(|v| !v.is_empty())
                .map(|edits| {
                    let items: Vec<String> = edits
                        .iter()
                        .map(|e| {
                            format!(
                                "{{\"offset\":{},\"length\":{},\"text\":{}}}",
                                e.offset,
                                e.length,
                                json_string(&e.replacement),
                            )
                        })
                        .collect();
                    format!("\"fix_diff\":[{}]", items.join(","))
                });
            let extra_json = match (replacement_json, fix_diff_json) {
                (Some(r), Some(f)) => Some(format!("{r},{f}")),
                (Some(r), None) => Some(r),
                (None, Some(f)) => Some(f),
                (None, None) => None,
            };
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "ownership",
                code,
                category: "ownership",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: err.suggestion.as_deref(),
                extra_json,
                lint_name: None,
                fix_it: None,
                class: crate::ownership::class_for_ownership_error_kind(&err.kind)
                    .map(|c| c.as_str()),
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
        for note in &o.notes {
            id_counter += 1;
            let code = match note.kind {
                crate::ownership::OwnershipErrorKind::UnusedMutCaptureNote => "N0507",
                _ => "N0503",
            };
            let replacement_json = note.replacement.as_ref().map(|r| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    r.offset,
                    r.length,
                    json_string(&r.replacement),
                )
            });
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "note",
                phase: "ownership",
                code,
                category: "ownership",
                message: &note.message,
                filename,
                span: &note.span,
                suggestion: note.suggestion.as_deref(),
                extra_json: replacement_json,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref esc) = pipeline.provider_escape {
        for err in esc {
            id_counter += 1;
            let message = err.message();
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "provider_escape",
                code: "E0600",
                category: "provider_escape",
                message: &message,
                filename,
                span: &err.closure_span,
                suggestion: None,
                extra_json: None,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref raii) = pipeline.raii_errors {
        for err in raii {
            id_counter += 1;
            let message = err.message();
            let mut extra_parts: Vec<String> = Vec::new();
            if let Some(ref bs) = err.binding_span {
                extra_parts.push(format!(
                    "\"binding_span\":{{{}}}",
                    span_to_json(bs, filename)
                ));
            }
            if let Some(ref sv) = err.state_violation {
                extra_parts.push(format!(
                    "\"state_violation\":{{\"soiling_method\":{},\"clear_method_name\":{},\"soil_span\":{{{}}}}}",
                    json_string(&sv.soiling_method),
                    json_string(&sv.clear_method_name),
                    span_to_json(&sv.soil_span, filename),
                ));
            }
            let extra_json = if extra_parts.is_empty() {
                None
            } else {
                Some(extra_parts.join(","))
            };
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "raii_check",
                code: "E_RAII_ACROSS_YIELD",
                category: "raii_across_yield",
                message: &message,
                filename,
                span: &err.yield_span,
                suggestion: None,
                extra_json,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref simd) = pipeline.simd_errors {
        for err in simd {
            id_counter += 1;
            let message = err.message();
            let help = err.help();
            let func = json_string(&err.func_name);
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "simd_check",
                code: "E_REQUIRE_SIMD",
                category: "require_simd",
                message: &message,
                filename,
                span: &err.span,
                suggestion: Some(&help),
                extra_json: Some(format!("\"function\":{func}")),
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref comptime) = pipeline.comptime_errors {
        for err in comptime {
            id_counter += 1;
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "comptime",
                code: "E_COMPTIME",
                category: "comptime",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: None,
                extra_json: None,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    diags
}

pub(super) fn program_effects_json(pipeline: &Pipeline) -> String {
    match &pipeline.effects {
        Some(effects) => {
            // Collect all effects from main() or program-level
            let mut all_effects: Vec<String> = Vec::new();
            if let Some(main_effects) = effects.inferred_effects.get("main") {
                for te in &main_effects.effects {
                    all_effects.push(format!(
                        "{}({})",
                        effect_verb_str(&te.effect.verb),
                        te.effect.resource
                    ));
                }
            }
            if all_effects.is_empty() {
                "[]".to_string()
            } else {
                json_string_list(&all_effects)
            }
        }
        None => "null".to_string(),
    }
}

pub(super) fn public_function_effects_json(pipeline: &Pipeline) -> String {
    let Some(effects) = &pipeline.effects else {
        return "{}".to_string();
    };
    let mut names: Vec<&String> = effects
        .function_visibility
        .iter()
        .filter_map(|(n, is_pub)| {
            if *is_pub && n != "main" {
                Some(n)
            } else {
                None
            }
        })
        .collect();
    names.sort();
    if names.is_empty() {
        return "{}".to_string();
    }
    let entries: Vec<String> = names
        .iter()
        .map(|name| {
            let list: Vec<String> = effects
                .inferred_effects
                .get(*name)
                .map(|set| {
                    set.effects
                        .iter()
                        .map(|te| {
                            format!(
                                "{}({})",
                                effect_verb_str(&te.effect.verb),
                                te.effect.resource
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("{}:{}", json_string(name), json_string_list(&list))
        })
        .collect();
    format!("{{{}}}", entries.join(","))
}

pub(super) fn mutual_recursion_groups_json(pipeline: &Pipeline) -> String {
    match &pipeline.effects {
        Some(effects) => {
            if effects.mutual_recursion_groups.is_empty() {
                return "[]".to_string();
            }
            let groups: Vec<String> = effects
                .mutual_recursion_groups
                .iter()
                .map(|g| {
                    let funcs = json_string_list(&g.functions);
                    let traces: Vec<String> = g
                        .resolution_trace
                        .iter()
                        .map(|r| {
                            format!(
                                "{{\"call_site\":\"{}:{}\",\"resolved_via\":{},\"effect\":{}}}",
                                r.call_site_function,
                                r.call_site_line,
                                json_string(&r.resolved_via),
                                json_string(&r.effect),
                            )
                        })
                        .collect();
                    format!(
                        "{{\"functions\":{},\"resolution_trace\":[{}]}}",
                        funcs,
                        traces.join(","),
                    )
                })
                .collect();
            format!("[{}]", groups.join(","))
        }
        None => "[]".to_string(),
    }
}

/// Render a `crate::unsafe_lint::LintDiagnostic` in rustc-style format:
/// the primary line plus optional `= note:` and `= help:` continuation
/// lines. The `note:` carries the conceptual explanation (e.g. the two
/// distinct roles of `unsafe`); the `help:` carries the actionable
/// suggestion (wrap in `unsafe { ... }` and add a `// Safety:` comment).
pub(super) fn render_unsafe_lint_diag(diag: &crate::unsafe_lint::LintDiagnostic, filename: &str) {
    eprintln!(
        "{}[{}]: {}:{}:{}: {}",
        if diag.level == crate::unsafe_lint::LintLevel::Error {
            "error"
        } else {
            "warning"
        },
        diag.lint_name,
        filename,
        diag.span.line,
        diag.span.column,
        diag.message
    );
    if let Some(note) = &diag.note {
        eprintln!("   = note: {note}");
    }
    if let Some(help) = &diag.help {
        eprintln!("   = help: {help}");
    }
}

/// Render a `crate::must_use_lint::LintDiagnostic` in the same
/// rustc-style three-piece shape (primary / `= note:` / `= help:`) as
/// `render_unsafe_lint_diag`. Kept parallel rather than unified because
/// each lint module currently owns its own `LintDiagnostic` struct (the
/// pre-existing pattern across `unsafe_lint`, `logical_lint`,
/// `ffi_lint`); a future lint-registry refactor (`docs/implementation_
/// checklist/phase-5-diagnostics.md` § "Lint level attributes") would
/// unify these.
pub(super) fn render_must_use_lint_diag(
    diag: &crate::must_use_lint::LintDiagnostic,
    filename: &str,
) {
    eprintln!(
        "{}[{}]: {}:{}:{}: {}",
        if diag.level == crate::must_use_lint::LintLevel::Error {
            "error"
        } else {
            "warning"
        },
        diag.lint_name,
        filename,
        diag.span.line,
        diag.span.column,
        diag.message
    );
    if let Some(note) = &diag.note {
        eprintln!("   = note: {note}");
    }
    if let Some(help) = &diag.help {
        eprintln!("   = help: {help}");
    }
}

/// Render the `#[target(...)]`-stripped items of a SINGLE-target check as a
/// note (B-2026-08-05-29).
///
/// A stripped item is removed before any pass runs, so its body is not
/// checked leniently — it is not checked at all, and `check` then prints "All
/// checks passed" over source it never examined. That is how a fixture with
/// two plain type errors lived in `main` (B-2026-08-05-24): nothing in the
/// default single-target output distinguished "checked and clean" from "not
/// looked at".
///
/// Single-target only. The `--targets=` / `[build].targets` matrix already
/// checks each declared target under its own filtering, so the same note
/// there would fire once per pass for items the OTHER pass just checked.
///
/// Deliberately a note, not a warning: gating an item away is correct
/// behaviour, and the author of a native-only build has no reason to be
/// nagged. What was missing is the fact, plus the flag that acts on it.
pub(super) fn target_skip_note(pipeline: &Pipeline) -> Option<String> {
    render_target_skip_note(&pipeline.target_skipped, crate::target::active_target())
}

/// The note itself, over a name → rendered-spec map. Split out so the
/// multi-target driver can reuse it for the items NO requested target
/// checked — see `unchecked_across_targets`.
pub(super) fn render_target_skip_note(
    skipped: &std::collections::HashMap<String, String>,
    scope: &str,
) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let mut items: Vec<_> = skipped
        .iter()
        .map(|(name, spec)| format!("{name} ({spec})"))
        .collect();
    items.sort();
    let n = items.len();
    let plural = if n == 1 { "" } else { "s" };
    Some(format!(
        "note: {n} item{plural} NOT checked — gated away from {scope}: {}\n      \
         Their bodies are stripped before any pass runs, so nothing above covers them. \
         Check them with `--targets=all` (or declare `[build] targets` in kara.toml).",
        items.join(", "),
    ))
}

/// Items every requested target stripped — the matrix's blind spot.
///
/// The single-target note deliberately does not fire per-pass in a matrix
/// run: an item one pass gates away is usually an item another pass is
/// busy checking, and nagging about it would be noise. But that reasoning
/// only holds for items SOME pass admits. `--targets=native,wasm_wasi`
/// over a `#[target(gpu)]` item checks it nowhere and, before this,
/// said nothing — the same silence the note exists to break, one level
/// up. Intersecting the per-pass maps keeps the original intent
/// (never mention an item a sibling pass checked) and closes that hole.
pub(super) fn unchecked_across_targets(
    per_target: &[std::collections::HashMap<String, String>],
) -> std::collections::HashMap<String, String> {
    let Some((first, rest)) = per_target.split_first() else {
        return std::collections::HashMap::new();
    };
    first
        .iter()
        .filter(|(name, _)| rest.iter().all(|m| m.contains_key(*name)))
        .map(|(name, spec)| (name.clone(), spec.clone()))
        .collect()
}

/// `target_skipped` as a JSON array of `{name, gated_for}` (B-2026-08-05-29).
///
/// This is the field the Mend loop actually needs. `--output=json` is the
/// documented front door of that loop, and without this an empty
/// `"diagnostics":[]` is indistinguishable between "the file is clean" and
/// "some of the file was never looked at". Sorted by name so the output is
/// deterministic across runs.
pub(super) fn target_skipped_json(pipeline: &Pipeline) -> String {
    render_target_skipped_json(&pipeline.target_skipped)
}

/// Map form of [`target_skipped_json`], so the multi-target driver can
/// emit the same field for the items no requested target checked.
pub(super) fn render_target_skipped_json(
    skipped: &std::collections::HashMap<String, String>,
) -> String {
    let mut rows: Vec<(&String, &String)> = skipped.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let parts: Vec<String> = rows
        .iter()
        .map(|(name, spec)| {
            format!(
                "{{\"name\":{},\"gated_for\":{}}}",
                json_string(name),
                json_string(spec)
            )
        })
        .collect();
    format!("[{}]", parts.join(","))
}

pub(super) fn emit_json_output(pipeline: &Pipeline) {
    let diags = collect_diagnostics(pipeline);
    let effects = program_effects_json(pipeline);
    let pub_effects = public_function_effects_json(pipeline);
    let mrg = mutual_recursion_groups_json(pipeline);
    println!(
        "{{\"program_effects\":{},\"public_function_effects\":{},\"mutual_recursion_groups\":{},\"target_skipped\":{},\"diagnostics\":{}}}",
        effects,
        pub_effects,
        mrg,
        target_skipped_json(pipeline),
        diags.to_json_array()
    );
}

// ── JSONL Streaming Output ──────────────────────────────────────

pub(super) fn emit_jsonl_event(event_type: &str, fields: &str) {
    println!("{{\"type\":{},{}}}", json_string(event_type), fields);
}

pub(super) fn run_pipeline_jsonl(pipeline: &mut Pipeline) {
    let filename = &pipeline.filename.clone();

    // build_start
    emit_jsonl_event(
        "build_start",
        &format!("\"timestamp\":\"\",\"files\":[{}]", json_string(filename)),
    );

    // lex phase (already done during parse)
    emit_jsonl_event(
        "phase_start",
        &format!(
            "\"phase\":\"lex\",\"scope\":{{\"files\":[{}]}}",
            json_string(filename)
        ),
    );
    emit_jsonl_event(
        "phase_complete",
        "\"phase\":\"lex\",\"errors\":0,\"warnings\":0,\"notes\":0",
    );

    // parse phase
    emit_jsonl_event("phase_start", "\"phase\":\"parse\"");
    let parse_errors = pipeline.parsed.errors.len();
    if parse_errors > 0 {
        let diags = collect_diagnostics(pipeline);
        for entry in &diags.entries {
            // Re-emit parse diagnostics as streaming events
            println!("{entry}");
        }
    }
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"parse\",\"errors\":{},\"warnings\":0,\"notes\":0",
            parse_errors
        ),
    );

    if pipeline.has_parse_errors() {
        // Skip remaining phases
        for phase in &["resolve", "typecheck", "effect", "ownership"] {
            emit_jsonl_event(
                "phase_skipped",
                &format!(
                    "\"phase\":{},\"reason\":\"parse errors in input\",\"blocking\":[\"d1\"]",
                    json_string(phase)
                ),
            );
        }
        emit_jsonl_event(
            "build_complete",
            &format!(
                "\"success\":false,\"total_errors\":{},\"total_warnings\":0,\"program_effects\":null",
                parse_errors
            ),
        );
        return;
    }

    // resolve phase
    emit_jsonl_event("phase_start", "\"phase\":\"resolve\"");
    pipeline.resolve();
    let resolve_errors = pipeline.resolved.as_ref().map_or(0, |r| r.errors.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"resolve\",\"errors\":{},\"warnings\":0,\"notes\":0",
            resolve_errors
        ),
    );

    if pipeline.has_resolve_errors() {
        for phase in &["typecheck", "effect", "ownership"] {
            emit_jsonl_event(
                "phase_skipped",
                &format!(
                    "\"phase\":{},\"reason\":\"resolve errors in input\",\"blocking\":[]",
                    json_string(phase)
                ),
            );
        }
        let total = parse_errors + resolve_errors;
        emit_jsonl_event(
            "build_complete",
            &format!(
                "\"success\":false,\"total_errors\":{},\"total_warnings\":0,\"program_effects\":null",
                total
            ),
        );
        return;
    }

    // typecheck phase
    emit_jsonl_event("phase_start", "\"phase\":\"typecheck\"");
    pipeline.typecheck();
    pipeline.lower();
    let type_errors = pipeline.typed.as_ref().map_or(0, |t| t.errors.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"typecheck\",\"errors\":{},\"warnings\":0,\"notes\":0",
            type_errors
        ),
    );

    // effect phase
    emit_jsonl_event("phase_start", "\"phase\":\"effect\"");
    pipeline.effectcheck();
    let (effect_errors, effect_notes) = pipeline.effects.as_ref().map_or((0, 0), |e| {
        let errors = e
            .errors
            .iter()
            .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
            .count();
        let notes = e
            .errors
            .iter()
            .filter(|e| e.kind == EffectErrorKind::FfiLintHint)
            .count();
        (errors, notes)
    });
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"effect\",\"errors\":{},\"warnings\":0,\"notes\":{}",
            effect_errors, effect_notes
        ),
    );

    // ownership phase
    emit_jsonl_event("phase_start", "\"phase\":\"ownership\"");
    pipeline.ownershipcheck();
    let ownership_errors = pipeline.ownership.as_ref().map_or(0, |o| o.errors.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"ownership\",\"errors\":{},\"warnings\":0,\"notes\":0",
            ownership_errors
        ),
    );

    // provider escape phase
    emit_jsonl_event("phase_start", "\"phase\":\"provider_escape\"");
    pipeline.provider_escape_check();
    let escape_errors = pipeline.provider_escape.as_ref().map_or(0, |e| e.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"provider_escape\",\"errors\":{},\"warnings\":0,\"notes\":0",
            escape_errors
        ),
    );

    // RAII-across-yield phase (phase 6 line 31 slice 1)
    emit_jsonl_event("phase_start", "\"phase\":\"raii_check\"");
    pipeline.raii_check();
    let raii_errors = pipeline.raii_errors.as_ref().map_or(0, |r| r.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"raii_check\",\"errors\":{},\"warnings\":0,\"notes\":0",
            raii_errors
        ),
    );

    // `#[require_simd]` guarantee phase (phase-7-codegen.md line 308 slice 5a)
    emit_jsonl_event("phase_start", "\"phase\":\"simd_check\"");
    pipeline.simd_check();
    let simd_errors = pipeline.simd_errors.as_ref().map_or(0, |s| s.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"simd_check\",\"errors\":{},\"warnings\":0,\"notes\":0",
            simd_errors
        ),
    );

    let comptime_errors = pipeline.comptime_errors.as_ref().map_or(0, |c| c.len());
    let total = parse_errors
        + resolve_errors
        + type_errors
        + effect_errors
        + ownership_errors
        + escape_errors
        + raii_errors
        + simd_errors
        + comptime_errors;
    let effects = program_effects_json(pipeline);
    emit_jsonl_event(
        "build_complete",
        &format!(
            "\"success\":{},\"total_errors\":{},\"total_warnings\":0,\"program_effects\":{}",
            total == 0,
            total,
            effects,
        ),
    );
}
/// The three lints B-2026-08-18-2 moved onto the compile path, as
/// `(lint_name, is_error, message, span)`.
///
/// One list, two renderers: `cli.rs`'s text path and `collect_diagnostics`
/// above both iterate it, so a lint added here reaches both feeds or neither.
/// That is the property the row was really about — `must_use` was invisible to
/// `karac check --output=json` for exactly as long as its call site lived in
/// one command instead of one shared place.
pub(super) fn lint_entries_for_compile_path(
    pipeline: &Pipeline,
) -> Vec<(String, bool, String, crate::token::Span)> {
    let source = pipeline.source.as_deref().unwrap_or("");
    let mut out: Vec<(String, bool, String, crate::token::Span)> = Vec::new();
    for d in crate::unsafe_lint::check_undocumented_unsafe(
        &pipeline.parsed.program,
        source,
        &pipeline.lint_overrides,
    ) {
        let is_err = d.level == crate::unsafe_lint::LintLevel::Error;
        out.push((d.lint_name, is_err, d.message, d.span));
    }
    for d in crate::unsafe_lint::check_unsafe_op_in_unsafe_fn(
        &pipeline.parsed.program,
        pipeline.typed.as_ref(),
    ) {
        let is_err = d.level == crate::unsafe_lint::LintLevel::Error;
        out.push((d.lint_name, is_err, d.message, d.span));
    }
    for d in crate::ffi_lint::check_ffi_float_eq(&pipeline.parsed.program, &pipeline.lint_overrides)
    {
        let is_err = d.level == crate::ffi_lint::LintLevel::Error;
        out.push(("ffi_float_eq".to_string(), is_err, d.message, d.span));
    }
    out
}

#[cfg(test)]
mod diagnostic_json_tests {
    //! Direct-construction tests for the `DiagnosticJson` JSON
    //! emitter. The CLI integration tests in `tests/cli.rs`
    //! exercise the same emitter via real fixtures; these unit tests
    //! pin the *shape* against a synthetic `DiagEntry` so the
    //! field-by-field wire format is testable without standing up a
    //! full pipeline.
    use super::{DiagEntry, DiagnosticJson};
    use crate::token::Span;
    use crate::typechecker::FixIt;

    fn synth_span() -> Span {
        Span {
            line: 1,
            column: 5,
            offset: 4,
            length: 0,
        }
    }

    #[test]
    fn fix_it_emits_both_legacy_field_and_fixes_array() {
        // Line 619 slice 5 pin — a DiagEntry carrying a FixIt
        // produces both the legacy `fix_it` object (single-edit
        // form, kept for backward compat) and the new `fixes` array
        // (the spec's preferred shape per `docs/deferred.md` §
        // Structured Diagnostics). Both wire from the same FixIt
        // data; the legacy form has no `description` field, the
        // array form does.
        let mut diags = DiagnosticJson::new();
        let span = synth_span();
        let fix = FixIt {
            span,
            replacement: ", ..".to_string(),
        };
        diags.add(DiagEntry {
            id: "d1",
            severity: "error",
            phase: "typecheck",
            code: "E_NON_EXHAUSTIVE_CROSS_PACKAGE_PATTERN",
            category: "typecheck",
            message: "test message",
            filename: "test.kara",
            span: &span,
            suggestion: None,
            extra_json: None,
            lint_name: None,
            fix_it: Some(&fix),
            class: Some("OTHER"),
            expected: None,
            got: None,
            stub_hint_json: None,
        });
        let json = diags.to_json_array();
        // Legacy field still present.
        assert!(
            json.contains("\"fix_it\":"),
            "expected legacy fix_it field; got: {json}"
        );
        // New array form.
        assert!(
            json.contains("\"fixes\":["),
            "expected fixes array; got: {json}"
        );
        // Array entry carries description + edits.
        assert!(json.contains("\"description\":"));
        assert!(json.contains("\"edits\":[{"));
        // Edits entry carries span + replacement.
        assert!(json.contains("\"replacement\":\", ..\""));
        // No fix-it on plain diagnostics — confirm the field is
        // omitted when fix_it: None.
    }

    #[test]
    fn no_fix_it_omits_both_fix_fields() {
        // When `fix_it: None`, neither the legacy `fix_it` field nor
        // the new `fixes` array should appear in the JSON — keeps
        // the lean shape that consumers expect for diagnostics
        // without machine-applicable patches.
        let mut diags = DiagnosticJson::new();
        let span = synth_span();
        diags.add(DiagEntry {
            id: "d1",
            severity: "error",
            phase: "typecheck",
            code: "E_TYPE_MISMATCH",
            category: "typecheck",
            message: "test",
            filename: "test.kara",
            span: &span,
            suggestion: None,
            extra_json: None,
            lint_name: None,
            fix_it: None,
            class: Some("TYPE_MISMATCH"),
            expected: Some("i32"),
            got: Some("String"),
            stub_hint_json: None,
        });
        let json = diags.to_json_array();
        assert!(!json.contains("\"fix_it\":"));
        assert!(!json.contains("\"fixes\":"));
        // Typed fields are still present.
        assert!(json.contains("\"class\":\"TYPE_MISMATCH\""));
        assert!(json.contains("\"expected\":\"i32\""));
        assert!(json.contains("\"got\":\"String\""));
    }

    #[test]
    fn fixes_array_description_falls_back_to_lint_name() {
        // When the diagnostic carries a `lint_name`, the fix's
        // description uses it instead of the generic "apply
        // suggested edit". Gives LLM/IDE consumers a recognisable
        // anchor for which rule the fix derives from.
        let mut diags = DiagnosticJson::new();
        let span = synth_span();
        let fix = FixIt {
            span,
            replacement: "_".to_string(),
        };
        diags.add(DiagEntry {
            id: "d1",
            severity: "warning",
            phase: "typecheck",
            code: "W0246",
            category: "typecheck",
            message: "test",
            filename: "test.kara",
            span: &span,
            suggestion: None,
            extra_json: None,
            lint_name: Some("missing_non_exhaustive"),
            fix_it: Some(&fix),
            class: Some("LINT_WARNING"),
            expected: None,
            got: None,
            stub_hint_json: None,
        });
        let json = diags.to_json_array();
        assert!(
            json.contains("\"description\":\"missing_non_exhaustive\""),
            "fix description should adopt lint_name; got: {json}"
        );
    }
}
