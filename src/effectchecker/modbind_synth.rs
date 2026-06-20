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
    /// Source span of the binding declaration. Used as the secondary
    /// label on the slice-7 par-block conflict diagnostic so the
    /// programmer can see both the offending `par { }` and the
    /// declaration that bound the synthetic resource.
    pub(crate) decl_span: Span,
    /// `true` when the binding's declared type is one of the
    /// explicit concurrency primitives — `Atomic[T]`, `Mutex[T]`,
    /// `RwLock[T]`, `Arc[T]` — per design.md §1328. Slice 7's
    /// par-block check uses this to skip the rejection: those types
    /// carry their own synchronisation, so writing them concurrently
    /// is well-defined. `#[thread_local]` also escapes the
    /// rejection via the resource-wrapping path (the resource name
    /// is `ThreadLocal[BINDING_resource]`, which never conflicts
    /// with itself across tasks), so this flag stays `false` for
    /// thread-locals; the par-block check filters them out by
    /// resource-name prefix instead.
    pub(crate) is_concurrency_primitive: bool,
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
            let is_concurrency_primitive =
                b.ty.as_ref()
                    .map(type_is_concurrency_primitive)
                    .unwrap_or(false);
            self.modbind_let_mut.insert(
                b.name.clone(),
                ModBindingInfo {
                    resource_name,
                    decl_span: b.span.clone(),
                    is_concurrency_primitive,
                },
            );
        }
    }

    /// Look up a binding name in the `let mut` table and return its
    /// metadata. Slice 7's par-block check uses this through the
    /// synthetic resource name carried on the offending effect.
    pub(crate) fn lookup_modbind_by_resource(
        &self,
        resource: &str,
    ) -> Option<(&str, &ModBindingInfo)> {
        // Resource names take one of two forms — `<NAME>_resource` for
        // bare bindings and `ThreadLocal[<NAME>_resource]` for
        // `#[thread_local]` ones. We strip either decoration and look
        // the source name up in the table.
        let inner = resource
            .strip_prefix("ThreadLocal[")
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(resource);
        let name = inner.strip_suffix("_resource")?;
        self.modbind_let_mut
            .get_key_value(name)
            .map(|(k, v)| (k.as_str(), v))
    }

    /// `true` when `resource` is the synthetic effect-resource name
    /// of a module-level `let mut` binding (with or without the
    /// `ThreadLocal[...]` wrapper). Used by slice 8 / verify_declarations
    /// to filter synthetic effects out of the missing/over-declared
    /// checks — they can't be declared in source, so the generic
    /// "add `with reads(X)`" fix-it would be wrong; slice 8's
    /// dedicated rejection owns those diagnostics instead.
    pub(crate) fn is_synthetic_modbind_resource(&self, resource: &str) -> bool {
        self.lookup_modbind_by_resource(resource).is_some()
    }
}

/// Returns `true` when the outermost type name of `ty` is one of the
/// supported concurrency primitives per design.md §1328. Generics
/// are not inspected — `Atomic[i64]`, `Mutex[Vec[i64]]`,
/// `RwLock[shared struct S]`, `Arc[shared struct S]` are all
/// recognised purely by the root path segment.
fn type_is_concurrency_primitive(ty: &TypeExpr) -> bool {
    let segments = match &ty.kind {
        TypeKind::Path(p) => &p.segments,
        _ => return false,
    };
    let last = match segments.last() {
        Some(s) => s.as_str(),
        None => return false,
    };
    matches!(last, "Atomic" | "Mutex" | "RwLock" | "Arc")
}

impl<'a> super::EffectChecker<'a> {
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

    /// Slice-7 par-block conflict rule (design.md §1328). For every
    /// `par { }` expression reachable from any function or method
    /// body, walk the block as a single execution region (the spec
    /// rejects writes from *any* branch, so the union of branch
    /// effects is the conflict surface); for each transitive effect
    /// of the region that lands on a synthetic per-binding resource,
    /// reject if the binding's type is not an explicit concurrency
    /// primitive (`Atomic[T]` / `Mutex[T]` / `RwLock[T]` / `Arc[...]`)
    /// and not `#[thread_local]`. The diagnostic copies the §1328
    /// fix-it verbatim and embeds the binding-decl line so the
    /// programmer can find the declaration without re-reading the
    /// span column.
    pub(crate) fn check_modbind_par_conflicts(&mut self) {
        if self.modbind_let_mut.is_empty() {
            return;
        }
        // Snapshot the bodies so we can mutate `self.errors` inside the
        // analysis loop without re-borrowing the maps. Cloning is
        // cheap here: the walk produces few results, and this only
        // runs once per program.
        let work: Vec<(String, Block)> = self
            .function_bodies
            .iter()
            .map(|(n, f)| (n.clone(), f.body.clone()))
            .chain(
                self.method_bodies
                    .iter()
                    .map(|(n, f)| (n.clone(), f.body.clone())),
            )
            .collect();
        for (_fn_name, body) in &work {
            let par_blocks = collect_par_blocks_in_block(body);
            for (par_block, par_span) in par_blocks {
                self.check_one_par_block(&par_block, &par_span);
            }
        }
    }

    fn check_one_par_block(&mut self, block: &Block, par_span: &Span) {
        // Collect the par-block's transitive effects via the same
        // machinery `infer_function_effects` uses: direct calls +
        // synthetic modbind read/write entries → look up each
        // entry's seeded effect set.
        let bounds: HashMap<String, Vec<TraitBound>> = HashMap::new();
        let direct_calls = self.collect_calls_in_block(block, &bounds);
        let synth_calls = self.collect_modbind_synth_calls_in_block(block, &[]);
        let mut effects: Vec<Effect> = Vec::new();
        for (callee, _span) in direct_calls.into_iter().chain(synth_calls) {
            for e in self.get_callee_effects(&callee) {
                if !effects.contains(&e) {
                    effects.push(e);
                }
            }
        }
        // Track which bindings already reported so a par block that
        // writes the same binding twice still produces one
        // diagnostic per offending binding (not one per write site).
        let mut seen: HashSet<String> = HashSet::new();
        for effect in &effects {
            if !matches!(effect.verb, EffectVerbKind::Writes) {
                continue;
            }
            // `ThreadLocal[...]` resources never participate in the
            // conflict — per-task disjoint instances by construction.
            if effect.resource.starts_with("ThreadLocal[") {
                continue;
            }
            let Some((name, info)) = self.lookup_modbind_by_resource(&effect.resource) else {
                continue;
            };
            if info.is_concurrency_primitive {
                continue;
            }
            if !seen.insert(name.to_string()) {
                continue;
            }
            let decl_line = info.decl_span.line;
            let name_owned = name.to_string();
            self.errors.push(super::EffectError {
                message: format!(
                    "module-level let mut '{}' cannot be written from inside par {{ }} — wrap in Atomic[T], Mutex[T], or use #[thread_local] for per-task state (binding declared at line {})",
                    name_owned, decl_line
                ),
                span: par_span.clone(),
                kind: super::EffectErrorKind::ModuleBindingWriteInPar,
                subtype_trace: None,
                replacement: None,
            });
        }
    }

    /// Slice-8 `pub fn` synthetic-resource rejection (design.md §1326).
    /// Runs only under `public_effects = "declared"`. For each public
    /// function (top-level or impl method), walk its inferred effect
    /// set and emit one diagnostic per offending (function, binding)
    /// pair when the effect's resource is the synthetic per-binding
    /// name of a `let mut` whose type is NOT an explicit concurrency
    /// primitive (`Atomic[T]` / `Mutex[T]` / `RwLock[T]` / `Arc[...]`)
    /// and is NOT `#[thread_local]` (whose `ThreadLocal[...]` wrapper
    /// never conflicts across tasks, so a public function carrying
    /// that effect raises no synchronisation concern).
    ///
    /// The two supported escapes — verbatim per §1326 — are: wrap the
    /// binding in a named concurrency primitive and declare effects
    /// on the well-known wrapper resource, or set
    /// `public_effects = "inferred"` at the project level. The
    /// diagnostic carries both, plus the binding's decl line so the
    /// programmer can navigate without re-reading the span column.
    pub(crate) fn verify_pub_fn_no_synthetic_resource(&mut self) {
        if self.public_effects_policy != super::PublicEffectsPolicy::Declared {
            return;
        }
        if self.modbind_let_mut.is_empty() {
            return;
        }
        let fn_names: Vec<String> = self.function_bodies.keys().cloned().collect();
        let method_names: Vec<String> = self.method_bodies.keys().cloned().collect();
        for name in fn_names.iter().chain(method_names.iter()) {
            let is_pub = self.function_visibility.get(name).copied().unwrap_or(false);
            if !is_pub {
                continue;
            }
            let Some(inferred) = self.inferred_effects.get(name).cloned() else {
                continue;
            };
            let span = self.function_spans.get(name).cloned().unwrap_or(Span {
                line: 0,
                column: 0,
                offset: 0,
                length: 0,
            });
            // One diagnostic per (function, offending binding). A
            // function that reads AND writes the same binding still
            // produces one diagnostic for that binding (writes
            // strictly dominates; both contribute the same fix).
            let mut seen: HashSet<String> = HashSet::new();
            for te in &inferred.effects {
                if self.is_transparent_verb(&te.effect.verb) {
                    continue;
                }
                // ThreadLocal-wrapped resources are per-task disjoint —
                // §1326's "no named synchronisation primitive" concern
                // doesn't apply, mirroring slice 7's filter.
                if te.effect.resource.starts_with("ThreadLocal[") {
                    continue;
                }
                let Some((binding_name, info)) =
                    self.lookup_modbind_by_resource(&te.effect.resource)
                else {
                    continue;
                };
                // Concurrency-primitive escape (§1326 path (a)): the
                // developer has chosen the supported wrapping; the
                // synthetic-resource concern doesn't apply.
                if info.is_concurrency_primitive {
                    continue;
                }
                let binding_name = binding_name.to_string();
                if !seen.insert(binding_name.clone()) {
                    continue;
                }
                let verb = super::verb_name(&te.effect.verb);
                let decl_line = info.decl_span.line;
                self.errors.push(super::EffectError {
                    message: format!(
                        "public function '{}' performs {}({}) on module-level let mut '{}' \
                         (binding declared at line {}); the synthetic per-binding resource \
                         is not nameable in a `with ...` declaration. Either wrap the \
                         binding in Atomic[T], Mutex[T], RwLock[T], or Arc[shared struct S] \
                         and expose pub fn methods that declare effects on those well-known \
                         resources, or set public_effects = \"inferred\" in kara.toml",
                        name, verb, te.effect.resource, binding_name, decl_line,
                    ),
                    span: span.clone(),
                    kind: super::EffectErrorKind::PubFnSyntheticResource,
                    subtype_trace: None,
                    replacement: None,
                });
            }
        }
    }
}

use std::collections::HashSet;

/// Recursively walk a block and collect every `par { }` expression
/// reachable from it, paired with the span of the par-block
/// expression itself. The returned span is the locus of the slice-7
/// diagnostic. Nested par blocks (a par inside an outer par's
/// branch) are reported too — each carries its own conflict
/// surface.
fn collect_par_blocks_in_block(block: &Block) -> Vec<(Block, Span)> {
    let mut out = Vec::new();
    for stmt in &block.stmts {
        collect_par_in_stmt(stmt, &mut out);
    }
    if let Some(ref e) = block.final_expr {
        collect_par_in_expr(e, &mut out);
    }
    out
}

fn collect_par_in_stmt(stmt: &Stmt, out: &mut Vec<(Block, Span)>) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. }
        | StmtKind::Assign { value, .. }
        | StmtKind::CompoundAssign { value, .. }
        | StmtKind::Expr(value) => collect_par_in_expr(value, out),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            collect_par_in_expr(value, out);
            for s in &else_block.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = else_block.final_expr {
                collect_par_in_expr(e, out);
            }
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = body.final_expr {
                collect_par_in_expr(e, out);
            }
        }
        StmtKind::LetUninit { .. } => {}
    }
}

fn collect_par_in_expr(expr: &Expr, out: &mut Vec<(Block, Span)>) {
    match &expr.kind {
        ExprKind::Par(block) => {
            out.push((block.clone(), expr.span.clone()));
            // Walk INTO the par-block's branches so nested par blocks
            // are caught too.
            for s in &block.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = block.final_expr {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            for s in &b.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = b.final_expr {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_par_in_expr(condition, out);
            for s in &then_block.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = then_block.final_expr {
                collect_par_in_expr(e, out);
            }
            if let Some(e) = else_branch {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            collect_par_in_expr(value, out);
            for s in &then_block.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = then_block.final_expr {
                collect_par_in_expr(e, out);
            }
            if let Some(e) = else_branch {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_par_in_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_par_in_expr(g, out);
                }
                collect_par_in_expr(&arm.body, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_par_in_expr(condition, out);
            for s in &body.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = body.final_expr {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::WhileLet { value, body, .. } => {
            collect_par_in_expr(value, out);
            for s in &body.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = body.final_expr {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::For { iterable, body, .. } => {
            collect_par_in_expr(iterable, out);
            for s in &body.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = body.final_expr {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::Loop { body, .. } => {
            for s in &body.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = body.final_expr {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::Closure { body, .. } => collect_par_in_expr(body, out),
        ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
            collect_par_in_expr(left, out);
            collect_par_in_expr(right, out);
        }
        ExprKind::NilCoalesce { left, right } => {
            collect_par_in_expr(left, out);
            collect_par_in_expr(right, out);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
            collect_par_in_expr(operand, out);
        }
        ExprKind::OptionalChain { object, args, .. } => {
            collect_par_in_expr(object, out);
            if let Some(args) = args {
                for a in args {
                    collect_par_in_expr(&a.value, out);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            collect_par_in_expr(callee, out);
            for a in args {
                collect_par_in_expr(&a.value, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_par_in_expr(object, out);
            for a in args {
                collect_par_in_expr(&a.value, out);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            collect_par_in_expr(object, out);
        }
        ExprKind::Index { object, index } => {
            collect_par_in_expr(object, out);
            collect_par_in_expr(index, out);
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for e in items {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            collect_par_in_expr(value, out);
            collect_par_in_expr(count, out);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                collect_par_in_expr(k, out);
                collect_par_in_expr(v, out);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                collect_par_in_expr(&f.value, out);
            }
            if let Some(s) = spread {
                collect_par_in_expr(s, out);
            }
        }
        ExprKind::Cast { expr: inner, .. } => collect_par_in_expr(inner, out),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_par_in_expr(s, out);
            }
            if let Some(e) = end {
                collect_par_in_expr(e, out);
            }
        }
        ExprKind::Return(Some(inner)) => collect_par_in_expr(inner, out),
        ExprKind::Break {
            value: Some(inner), ..
        } => collect_par_in_expr(inner, out),
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                collect_par_in_expr(&b.value, out);
            }
            for s in &body.stmts {
                collect_par_in_stmt(s, out);
            }
            if let Some(ref e) = body.final_expr {
                collect_par_in_expr(e, out);
            }
        }
        // Leaves with no nested expressions.
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
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
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
            ExprKind::Block(b) | ExprKind::Comptime(b) => self.walk_block(b),
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
