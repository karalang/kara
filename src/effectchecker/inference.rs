//! Effect inference: walk each user-defined function body, gather
//! its set of effects via callee-effect propagation, and seed
//! `inferred_effects`. Driven by `infer_effects` (Phase B) and
//! `infer_private_trait_ceilings` (private-trait ceiling refinement).
//!
//! Houses the call-graph walk (`build_call_graph`), the per-function
//! fixpoint (`infer_function_effects`), the callee-effect lookup
//! (`get_callee_effects`), and the three-way body walker that
//! collects each call site (`collect_calls_in_block`,
//! `collect_calls_in_stmt`, `collect_calls_in_expr`) with its
//! resolved callee name + sub-bounds. The two polymorphism predicates
//! (`is_polymorphic_callee`, `is_user_function`) classify each edge.
//!
//! Lives in a sibling `impl<'a> super::EffectChecker<'a>` block.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::token::Span;

use super::{
    tarjan_scc, DeclaredEffects, Effect, EffectError, EffectErrorKind, EffectOrigin, EffectSet,
};

/// E0412 predicate: does this declared `with` clause mention `resource`
/// under some verb while omitting `writes(resource)`? Returns the
/// `/`-joined verb names that do mention it (for the diagnostic text),
/// or `None` when there is no contradiction. Clauses containing groups,
/// `with _`, or effect variables are skipped conservatively — any of
/// those could expand to `writes(resource)` after resolution, so no
/// definition-site contradiction can be proven.
fn clause_verbs_on_resource_without_writes(
    effects: Option<&EffectList>,
    resource: &str,
) -> Option<String> {
    let effects = effects?;
    let mut mentioned: Vec<&'static str> = Vec::new();
    for item in &effects.items {
        match item {
            EffectItem::Verb(v) => {
                if !v.resources.iter().any(|r| r.path.join(".") == resource) {
                    continue;
                }
                match v.kind {
                    EffectVerbKind::Writes => return None,
                    EffectVerbKind::Reads => mentioned.push("reads"),
                    EffectVerbKind::Sends => mentioned.push("sends"),
                    EffectVerbKind::Receives => mentioned.push("receives"),
                    EffectVerbKind::Allocates => mentioned.push("allocates"),
                    // Execution verbs / panics / user-defined verbs
                    // don't promise a read-only resource contract —
                    // mentioning the resource through them isn't the
                    // contradiction this check targets.
                    _ => {}
                }
            }
            EffectItem::Group(_) | EffectItem::Polymorphic | EffectItem::Variable(_) => {
                return None;
            }
        }
    }
    if mentioned.is_empty() {
        None
    } else {
        mentioned.dedup();
        Some(mentioned.join("/"))
    }
}

impl<'a> super::EffectChecker<'a> {
    /// Seed `inferred_effects` for the synthetic `Resource.method` keys
    /// produced by `R.method(...)` call sites. For each
    /// `effect resource R: Trait` declaration, walk `Trait`'s methods
    /// and contribute the verb implied by the method's receiver mode:
    /// `mut ref self` / owned `self` → `writes(R)`, `ref self` →
    /// `reads(R)` — unioned with the method's declared effect ceiling
    /// (the parsed `declared_effects["Trait.method"]` entry, so groups
    /// arrive expanded and the trait-level ceiling fallback applies),
    /// so a clause like `with reads(Cfg) writes(Log)` contributes its
    /// `writes(Log)` to every `Cfg.get(...)` caller rather than being
    /// silently dropped. Polymorphic remainders (`with _`, effect
    /// variables) contribute nothing — same conservative skip as the
    /// E0412 predicate. Must run after `collect_declared_effects`.
    /// Methods with no `self` receiver are skipped — those
    /// are associated functions and don't go through the per-task
    /// provider stack at runtime, so they carry no inherent resource
    /// verb. Supertrait methods are intentionally not walked here; the
    /// typechecker handles dispatch through supertraits but the bug
    /// repro that motivated this seed (and the v1 surface) only
    /// exercises the direct provider trait.
    pub(crate) fn seed_resource_trait_dispatch_effects(&mut self, builtin_span: &Span) {
        // Collect the per-trait method facts once so each resource that
        // names that trait can reuse the lookup. Alongside the receiver
        // mode (which decides the seeded verb), carry the declared
        // `with` clause plus the trait/receiver spans so the E0412
        // contradiction check below can fire at the definition site
        // with a machine-applicable receiver rewrite.
        struct SeedMethod {
            trait_name: String,
            name: String,
            self_param: SelfParam,
            self_span: Option<Span>,
            span: Span,
            effects: Option<EffectList>,
        }
        let mut trait_methods: HashMap<String, Vec<SeedMethod>> = HashMap::new();
        // Copy the `&'a Program` out of `self` so the walk below can
        // push onto `self.errors` without holding a `&self` borrow.
        let program = self.program;
        for item in &program.items {
            let t = match item {
                Item::TraitDef(t) => t,
                _ => continue,
            };
            for ti in &t.items {
                let m = match ti {
                    TraitItem::Method(m) => m,
                    TraitItem::AssocType(_) => continue,
                };
                if let Some(ref sp) = m.self_param {
                    trait_methods
                        .entry(t.name.clone())
                        .or_default()
                        .push(SeedMethod {
                            trait_name: t.name.clone(),
                            name: m.name.clone(),
                            self_param: sp.clone(),
                            self_span: m.self_span.clone(),
                            span: m.span.clone(),
                            effects: m.effects.clone(),
                        });
                }
            }
        }

        for item in &program.items {
            let r = match item {
                Item::EffectResource(r) => r,
                _ => continue,
            };
            let Some(ref trait_name) = r.provider_trait else {
                continue;
            };
            let Some(methods) = trait_methods.get(trait_name) else {
                continue;
            };
            for m in methods {
                let verb = match m.self_param {
                    SelfParam::Ref => EffectVerbKind::Reads,
                    SelfParam::MutRef | SelfParam::Owned => EffectVerbKind::Writes,
                };
                // E0412: the receiver seeds `writes(R)` on every
                // `R.method(...)` call site, but the method's declared
                // `with` clause promises a non-writes contract on R.
                // The declaration can never hold — flag the definition
                // (the root cause) instead of letting each caller trip
                // over E0400 with no path back here.
                if verb == EffectVerbKind::Writes {
                    if let Some(declared) =
                        clause_verbs_on_resource_without_writes(m.effects.as_ref(), &r.name)
                    {
                        let receiver = match m.self_param {
                            SelfParam::Owned => "self",
                            SelfParam::MutRef => "mut ref self",
                            SelfParam::Ref => unreachable!("ref self seeds reads"),
                        };
                        let span = m.self_span.clone().unwrap_or_else(|| m.span.clone());
                        self.errors.push(EffectError {
                            message: format!(
                                "trait method '{}.{}' declares {}({}) but its `{}` \
                                 receiver makes every '{}.{}' call infer writes({}); \
                                 change the receiver to `ref self` or declare writes({})",
                                m.trait_name,
                                m.name,
                                declared,
                                r.name,
                                receiver,
                                r.name,
                                m.name,
                                r.name,
                                r.name,
                            ),
                            span: span.clone(),
                            kind: EffectErrorKind::ResourceReceiverContradiction,
                            subtype_trace: None,
                            replacement: Some(Box::new(crate::resolver::TextEdit {
                                offset: span.offset,
                                length: span.length,
                                replacement: "ref self".to_string(),
                            })),
                        });
                    }
                }
                let key = format!("{}.{}", r.name, m.name);
                let mut set = EffectSet::new();
                set.add(
                    Effect {
                        verb,
                        resource: r.name.clone(),
                    },
                    EffectOrigin::Direct(builtin_span.clone()),
                );
                // Union the method's declared effect ceiling: a dispatch
                // call site inherits everything the clause declares
                // (e.g. `writes(Log)` on `Cfg.get`), not just the
                // receiver-implied verb on R. The verb-on-R floor above
                // stays receiver-derived; E0412 already rejects
                // clause-vs-receiver contradictions on R, and union
                // dedupes the agreeing case. A `Polymorphic` ceiling
                // (`with _` / effect variable) contributes nothing —
                // the same conservative skip as the E0412 predicate.
                match self
                    .declared_effects
                    .get(&format!("{}.{}", m.trait_name, m.name))
                {
                    Some(DeclaredEffects::Explicit(declared))
                    | Some(DeclaredEffects::PolymorphicWithFixed(declared)) => {
                        for te in &declared.effects {
                            set.add(te.effect.clone(), te.origin.clone());
                        }
                    }
                    Some(DeclaredEffects::Polymorphic) | Some(DeclaredEffects::None) | None => {}
                }
                self.inferred_effects.insert(key, set);
            }
        }
    }

    // ── Phase B: Inference ──────────────────────────────────────

    pub(crate) fn infer_effects(&mut self) {
        // Initialize all functions with empty effect sets (except externs, already done)
        for name in self.function_bodies.keys().cloned().collect::<Vec<_>>() {
            self.inferred_effects.entry(name).or_default();
        }
        for name in self.method_bodies.keys().cloned().collect::<Vec<_>>() {
            self.inferred_effects.entry(name).or_default();
        }

        // Build the call graph and decompose into SCCs (Tarjan's algorithm).
        // tarjan_scc returns SCCs in reverse topological order — leaves first,
        // roots last — so processing them in order propagates callee effects
        // into callers before callers are processed.
        let call_graph = self.build_call_graph();
        let all_fn_names: HashSet<String> = self
            .function_bodies
            .keys()
            .chain(self.method_bodies.keys())
            .cloned()
            .collect();
        let sccs = tarjan_scc(&all_fn_names, &call_graph);

        for scc in &sccs {
            if scc.len() == 1 {
                // Non-recursive: one pass is sufficient because all callees
                // (in earlier SCCs) are already fully resolved.
                let name = &scc[0];
                let body = self
                    .function_bodies
                    .get(name)
                    .map(|f| f.body.clone())
                    .or_else(|| self.method_bodies.get(name).map(|f| f.body.clone()));
                if let Some(body) = body {
                    self.infer_function_effects(name, &body);
                }
            } else {
                // Mutually-recursive SCC: iterate until convergence.
                // Effects are monotone, so at most |SCC| passes are needed
                // (each pass can propagate effects one hop around the cycle).
                for _ in 0..=scc.len() {
                    let mut changed = false;
                    for name in scc {
                        let body = self
                            .function_bodies
                            .get(name)
                            .map(|f| f.body.clone())
                            .or_else(|| self.method_bodies.get(name).map(|f| f.body.clone()));
                        if let Some(body) = body {
                            if self.infer_function_effects(name, &body) {
                                changed = true;
                            }
                        }
                    }
                    if !changed {
                        break;
                    }
                }
            }
        }
    }

    /// After the SCC inference pass, update `declared_effects` for private trait
    /// methods that have no explicit ceiling to the union of all impl inferred
    /// effects.  This makes the formal ceiling correct (spec compliance) and lets
    /// `verify_impl_trait_ceilings` use a real set instead of skipping with `None`.
    ///
    /// Public traits and methods that already carry an explicit `with` clause are
    /// left untouched — their declared ceilings are authoritative.
    pub(crate) fn infer_private_trait_ceilings(&mut self) {
        // Build (trait_name, method_name) → [impl_type_name] from all impl blocks.
        let mut trait_impl_types: HashMap<(String, String), Vec<String>> = HashMap::new();
        for item in &self.program.items {
            let imp = match item {
                Item::ImplBlock(i) => i,
                _ => continue,
            };
            let trait_name = match &imp.trait_name {
                Some(p) => p.segments.last().cloned().unwrap_or_default(),
                None => continue,
            };
            let type_name = match &imp.target_type.kind {
                TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                _ => continue,
            };
            for impl_item in &imp.items {
                let method = match impl_item {
                    ImplItem::Method(m) => m,
                    ImplItem::AssocType(_) => continue,
                };
                trait_impl_types
                    .entry((trait_name.clone(), method.name.clone()))
                    .or_default()
                    .push(type_name.clone());
            }
        }

        // For each private trait method with no explicit ceiling, union impl effects.
        let items: Vec<Item> = self.program.items.clone();
        for item in &items {
            let t = match item {
                Item::TraitDef(t) => t,
                _ => continue,
            };
            if t.is_pub {
                continue; // Public traits always use declared ceilings.
            }
            for trait_item in &t.items {
                let m = match trait_item {
                    TraitItem::Method(m) => m,
                    TraitItem::AssocType(_) => continue,
                };
                let trait_key = format!("{}.{}", t.name, m.name);
                // Only infer for methods with no explicit ceiling (None).
                if !matches!(
                    self.declared_effects.get(&trait_key),
                    Some(DeclaredEffects::None) | None
                ) {
                    continue;
                }
                let impl_types = trait_impl_types
                    .get(&(t.name.clone(), m.name.clone()))
                    .cloned()
                    .unwrap_or_default();
                let mut ceiling = EffectSet::new();
                for type_name in &impl_types {
                    let impl_key = format!("{}.{}", type_name, m.name);
                    if let Some(impl_set) = self.inferred_effects.get(&impl_key) {
                        for te in &impl_set.effects {
                            if !ceiling.contains(&te.effect) {
                                ceiling.add(te.effect.clone(), te.origin.clone());
                            }
                        }
                    }
                }
                if !ceiling.is_empty() {
                    self.declared_effects
                        .insert(trait_key, DeclaredEffects::Explicit(ceiling));
                }
            }
        }
    }

    /// Build a call graph restricted to user-defined functions and methods.
    /// Edges to builtins and external callees are omitted since they have no
    /// bodies to infer from (their effects are seeded directly into
    /// `inferred_effects` during initialization).
    pub(crate) fn build_call_graph(&self) -> HashMap<String, Vec<(String, Span)>> {
        let all_fn_names: HashSet<String> = self
            .function_bodies
            .keys()
            .chain(self.method_bodies.keys())
            .cloned()
            .collect();
        let mut graph: HashMap<String, Vec<(String, Span)>> = HashMap::new();
        let empty_bounds: HashMap<String, Vec<TraitBound>> = HashMap::new();
        for (name, func) in &self.function_bodies {
            let bounds = self.fn_bounds_index.get(name).unwrap_or(&empty_bounds);
            let relevant = self
                .collect_calls_in_block(&func.body, bounds)
                .into_iter()
                .filter(|(callee, _)| all_fn_names.contains(callee))
                .collect();
            graph.insert(name.clone(), relevant);
        }
        for (name, func) in &self.method_bodies {
            let bounds = self.fn_bounds_index.get(name).unwrap_or(&empty_bounds);
            let relevant = self
                .collect_calls_in_block(&func.body, bounds)
                .into_iter()
                .filter(|(callee, _)| all_fn_names.contains(callee))
                .collect();
            graph.insert(name.clone(), relevant);
        }
        graph
    }

    /// Extract the names of bindings introduced by a function's
    /// parameters (and `self`, if present) so the module-binding
    /// synthetic-resource walker can treat them as shadowing — a
    /// parameter named `COUNTER` (rare in practice — params are
    /// almost always lowercase) takes precedence over a module-level
    /// `let mut COUNTER`.
    pub(crate) fn function_param_names(&self, func: &Function) -> Vec<String> {
        let mut names = Vec::new();
        if func.self_param.is_some() {
            names.push("self".to_string());
        }
        for p in &func.params {
            names.extend(p.pattern.binding_names());
        }
        names
    }

    /// Walk a function body, find all calls, and add callee effects.
    /// Returns true if any new effects were added.
    pub(crate) fn infer_function_effects(&mut self, fn_name: &str, body: &Block) -> bool {
        let empty_bounds: HashMap<String, Vec<TraitBound>> = HashMap::new();
        let bounds = self
            .fn_bounds_index
            .get(fn_name)
            .cloned()
            .unwrap_or(empty_bounds);
        let mut calls = self.collect_calls_in_block(body, &bounds);
        // Append synthetic per-binding-resource call entries
        // (design.md §1322) — reads / writes of module-level `let mut`
        // bindings flow through the same call-graph propagation by
        // dispatching to seeded `__modbind_*` synthetic keys.
        let param_names: Vec<String> = self
            .function_bodies
            .get(fn_name)
            .map(|f| self.function_param_names(f))
            .or_else(|| {
                self.method_bodies
                    .get(fn_name)
                    .map(|f| self.function_param_names(f))
            })
            .unwrap_or_default();
        calls.extend(self.collect_modbind_synth_calls_in_block(body, &param_names));
        let mut new_effects = Vec::new();

        for (callee_name, call_span) in &calls {
            // Propagate the polymorphic marker only for callees that use
            // `with _` (anonymous polymorphism). A callee that declares
            // only `with E` (named) resolves its effect variable at the
            // call site against concrete bindings, so it does not "leak"
            // through callers that lack a `with _` of their own.
            let callee_is_poly = self.fn_uses_with_underscore.contains(callee_name)
                || self.calls_polymorphic.contains(callee_name);
            if callee_is_poly {
                self.calls_polymorphic.insert(fn_name.to_string());
            }

            // Look up callee's effects
            let callee_effects = self.get_callee_effects(callee_name);
            for effect in callee_effects {
                new_effects.push((
                    effect.clone(),
                    EffectOrigin::Callee {
                        fn_name: callee_name.clone(),
                        span: call_span.clone(),
                    },
                ));
            }
        }

        let mut changed = false;
        let current = self
            .inferred_effects
            .entry(fn_name.to_string())
            .or_default();
        for (effect, origin) in new_effects {
            if !current.contains(&effect) {
                current.add(effect, origin);
                changed = true;
            }
        }
        changed
    }

    /// Contract purity (design.md § Contracts rule 1): every contract
    /// expression — `requires`, `ensures` body, struct `invariant` — must
    /// have an inferred effect set that is a subset of `{panics}`. Any of the
    /// seven non-panic effects (`reads`/`writes`/`sends`/`receives`/
    /// `allocates`/`blocks`/`suspends`) appearing via a call inside a
    /// contract is a compile error naming the forbidden effect. `panics` is
    /// permitted (indexing / division / `unwrap` / `panic()` are idiomatic in
    /// predicates). Runs after inference so callee effect sets are settled.
    pub(crate) fn check_contract_purity(&mut self) {
        // Collect every (contract-expression, kind) pair up front so the
        // immutable call-collection walk doesn't overlap the mutable error
        // push. Contract clauses live on free functions, impl methods, and
        // struct invariants.
        let mut clauses: Vec<(Expr, &'static str)> = Vec::new();
        for item in &self.program.items {
            match item {
                Item::Function(f) => {
                    clauses.extend(f.requires.iter().map(|e| (e.clone(), "requires")));
                    clauses.extend(f.ensures.iter().map(|e| (e.body.clone(), "ensures")));
                }
                Item::StructDef(s) => {
                    clauses.extend(s.invariants.iter().map(|e| (e.clone(), "invariant")));
                    clauses.extend(s.impl_invariants.iter().map(|e| (e.clone(), "invariant")));
                }
                Item::ImplBlock(imp) => {
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            clauses.extend(m.requires.iter().map(|e| (e.clone(), "requires")));
                            clauses.extend(m.ensures.iter().map(|e| (e.body.clone(), "ensures")));
                        }
                    }
                }
                _ => {}
            }
        }

        let empty_bounds: HashMap<String, Vec<TraitBound>> = HashMap::new();
        let mut violations: Vec<(Effect, Span, &'static str)> = Vec::new();
        for (expr, kind) in &clauses {
            let mut calls = Vec::new();
            self.collect_calls_in_expr(expr, &mut calls, &empty_bounds);
            for (callee, call_span) in &calls {
                for effect in self.get_callee_effects(callee) {
                    if effect.verb != EffectVerbKind::Panics {
                        violations.push((effect, call_span.clone(), *kind));
                    }
                }
            }
        }

        for (effect, span, kind) in violations {
            let rendered = if effect.resource.is_empty() {
                super::verb_name(&effect.verb)
            } else {
                format!("{}({})", super::verb_name(&effect.verb), effect.resource)
            };
            self.errors.push(super::EffectError {
                message: format!(
                    "error[E_CONTRACT_IMPURE]: `{rendered}` is not permitted in a {kind} \
                     contract expression — contract predicates must be pure (effect set ⊆ \
                     {{panics}}); only `panics` (indexing, division, unwrap) is allowed"
                ),
                span,
                kind: super::EffectErrorKind::ForbiddenEffectInContract,
                subtype_trace: None,
                replacement: None,
            });
        }
    }

    /// Get the effects of a callee function.
    /// For public functions: use declared effects (inference firewall).
    /// For private functions: use inferred effects.
    /// For polymorphic (`with _`) functions: use inferred effects (transparent —
    /// the function's own internal effects are contributed to the caller).
    /// Note: effects from closure arguments are already propagated because
    /// `collect_calls_in_expr` walks into closure bodies at the call site.
    pub(crate) fn get_callee_effects(&self, callee_name: &str) -> Vec<Effect> {
        let is_pub = self
            .function_visibility
            .get(callee_name)
            .copied()
            .unwrap_or(false);

        if is_pub {
            // Use declared effects
            match self.declared_effects.get(callee_name) {
                Some(DeclaredEffects::Explicit(set)) => set.effect_set().into_iter().collect(),
                Some(DeclaredEffects::Polymorphic) => {
                    // `with _` — transparent: use the callee's inferred effects so its
                    // own fixed effects (allocations, I/O, etc.) propagate to the caller.
                    // Closure-argument effects are handled separately by the caller's
                    // body scan (collect_calls_in_expr walks into closure bodies).
                    match self.inferred_effects.get(callee_name) {
                        Some(set) => set.effect_set().into_iter().collect(),
                        None => Vec::new(),
                    }
                }
                Some(DeclaredEffects::PolymorphicWithFixed(fixed)) => {
                    // Mixed declaration (e.g. `with reads(X) + _`): return the fixed
                    // effects plus the callee's inferred effects. Closure-argument effects
                    // are propagated by the caller's body scan.
                    let mut effects: HashSet<Effect> = fixed.effect_set();
                    if let Some(inferred) = self.inferred_effects.get(callee_name) {
                        effects.extend(inferred.effect_set());
                    }
                    effects.into_iter().collect()
                }
                Some(DeclaredEffects::None) | None => Vec::new(), // pure
            }
        } else {
            // Use inferred effects
            match self.inferred_effects.get(callee_name) {
                Some(set) => set.effect_set().into_iter().collect(),
                None => Vec::new(),
            }
        }
    }

    /// Empty bounds map for callers that don't have generic-param context.
    /// Threaded through `collect_calls_in_*` so they can redirect typeparam
    /// dispatch (`T.method()`, bare `method()` against a bound) to the
    /// matching `Trait.method` ceiling key.
    pub(crate) fn collect_calls_in_block(
        &self,
        block: &Block,
        bounds: &HashMap<String, Vec<TraitBound>>,
    ) -> Vec<(String, Span)> {
        let mut calls = Vec::new();
        for stmt in &block.stmts {
            self.collect_calls_in_stmt(stmt, &mut calls, bounds);
        }
        if let Some(ref expr) = block.final_expr {
            self.collect_calls_in_expr(expr, &mut calls, bounds);
        }
        calls
    }

    pub(crate) fn collect_calls_in_stmt(
        &self,
        stmt: &Stmt,
        calls: &mut Vec<(String, Span)>,
        bounds: &HashMap<String, Vec<TraitBound>>,
    ) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } => self.collect_calls_in_expr(value, calls, bounds),
            StmtKind::LetUninit { .. } => {}
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                self.collect_calls_in_expr(value, calls, bounds);
                let block_calls = self.collect_calls_in_block(else_block, bounds);
                calls.extend(block_calls);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                let block_calls = self.collect_calls_in_block(body, bounds);
                calls.extend(block_calls);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.collect_calls_in_expr(target, calls, bounds);
                self.collect_calls_in_expr(value, calls, bounds);
            }
            StmtKind::Expr(expr) => self.collect_calls_in_expr(expr, calls, bounds),
        }
    }

    /// True if `name` is a `with _` (polymorphic) callee.
    fn is_polymorphic_callee(&self, name: &str) -> bool {
        matches!(
            self.declared_effects.get(name),
            Some(DeclaredEffects::Polymorphic | DeclaredEffects::PolymorphicWithFixed(_))
        )
    }

    /// True if `name` is a user-defined function (not a local variable).
    fn is_user_function(&self, name: &str) -> bool {
        self.function_bodies.contains_key(name) || self.method_bodies.contains_key(name)
    }

    /// True iff `ty` is a path naming a refinement type alias — i.e. an
    /// `x as ty` cast is a refinement assertion that propagates `panics`.
    /// Borrow / pointer / generic wrappers around a refinement are not
    /// themselves assertion targets, so only a bare path is checked.
    fn cast_target_is_refinement(&self, ty: &crate::ast::TypeExpr) -> bool {
        if let crate::ast::TypeKind::Path(path) = &ty.kind {
            if let Some(name) = path.segments.last() {
                return self.refinement_type_names.contains(name);
            }
        }
        false
    }

    pub(crate) fn collect_calls_in_expr(
        &self,
        expr: &Expr,
        calls: &mut Vec<(String, Span)>,
        bounds: &HashMap<String, Vec<TraitBound>>,
    ) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                // Trait-assoc-fn redirect: when the callee dispatches through
                // a typeparam-bound trait, push the resolved `Trait.method`
                // key(s) so the trait's declared ceiling propagates to the
                // caller. Concrete-type dispatch (`Wrapper.method()`) and
                // ordinary free-function calls still use `extract_callee_name`.
                let trait_keys = self.extract_trait_assoc_fn_keys(callee, bounds);
                let callee_name: Option<String> = if !trait_keys.is_empty() {
                    for key in &trait_keys {
                        calls.push((key.clone(), expr.span.clone()));
                    }
                    trait_keys.into_iter().next()
                } else {
                    let n = self.extract_callee_name(callee);
                    if let Some(ref name) = n {
                        calls.push((name.clone(), expr.span.clone()));
                    }
                    n
                };
                // Combined distinct-type constructor `T(value)` where
                // `distinct type T = Base where pred`: the constructor runs a
                // runtime predicate assertion → propagates `panics`,
                // attributed to the synthetic `__builtin_refinement_assert`
                // callee (mirrors the `x as Refined` cast arm). The callee is
                // a bare `Identifier(T)`; `Path` callees (`T.try_from`) carry
                // their own declared effects.
                if let ExprKind::Identifier(n) = &callee.kind {
                    if self.refinement_type_names.contains(n) {
                        calls.push(("__builtin_refinement_assert".to_string(), expr.span.clone()));
                    }
                }
                self.collect_calls_in_expr(callee, calls, bounds);
                for arg in args {
                    self.collect_calls_in_expr(&arg.value, calls, bounds);
                }
                // When calling a `with _` function (or a private function that
                // transitively calls one), named-function-reference args contribute
                // their effects directly (per-call-site resolution).
                // Inline closure args are already handled by the recursive walk above.
                if let Some(ref cname) = callee_name {
                    if self.is_polymorphic_callee(cname)
                        || self.calls_polymorphic.contains(cname.as_str())
                    {
                        for arg in args {
                            if let ExprKind::Identifier(arg_name) = &arg.value.kind {
                                if self.is_user_function(arg_name) {
                                    calls.push((arg_name.clone(), arg.value.span.clone()));
                                }
                            }
                        }
                    }
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                // `Self.method()` inside a trait default body dispatches
                // through the trait's supertraits; redirect to the matching
                // `Trait.method` ceiling key. Bounds for Self are populated
                // by `trait_method_bounds` (supertraits as Self bounds).
                if matches!(object.kind, ExprKind::SelfType) {
                    if let Some(bs) = bounds.get("Self") {
                        for b in bs {
                            if let Some(t) = b.path.last() {
                                if self.trait_declares_no_self_method(t, method) {
                                    calls.push((format!("{}.{}", t, method), expr.span.clone()));
                                }
                            }
                        }
                    }
                }
                // Precise resolution first: when the typechecker recorded the
                // exact `Type.method` callee for this call site, push that key.
                // This is the only path that reaches the effects of baked-stdlib
                // instance methods whose `inferred_effects` seed is keyed by the
                // fully-qualified name but which are absent from `method_bodies`
                // and from the name-only `STDLIB_METHOD_MAP` below — notably the
                // `std.http` client surface (`Client.get` / `Client.post` /
                // `RequestBuilder.send`), whose `sends(Network)`/`receives(Network)`
                // seeds were otherwise unreachable (the name-only heuristics can't
                // distinguish `client.get()` from `map.get()`, so they cannot
                // safely map `get`/`post`/`send`). Precise and additive: it can
                // only contribute effects that genuinely belong to the resolved
                // method, so it never taints `map.get()` / `sender.send()`.
                if let Some(precise_key) = self.resolve_method_callee_key(&expr.span) {
                    calls.push((precise_key, expr.span.clone()));
                }
                // For method calls without a recorded precise callee, we'd need
                // type info to know the exact method. Fall back to searching all
                // impl methods with matching name.
                for key in self.method_bodies.keys() {
                    if key.ends_with(&format!(".{}", method)) {
                        calls.push((key.clone(), expr.span.clone()));
                    }
                }
                // unwrap() and expect() always panic on None/Err (F-057).
                if method == "unwrap" {
                    calls.push(("__builtin_unwrap".to_string(), expr.span.clone()));
                } else if method == "expect" {
                    calls.push(("__builtin_expect".to_string(), expr.span.clone()));
                }
                // Lowercase stdlib module aliases routed through `MethodCall`
                // syntax (`env.set(...)`, etc.). The parser produces a
                // `MethodCall` with `object = Identifier("env")` because the
                // module name is value-class — `Env.set(...)` would parse as
                // `Call(Path([Env, set]))` and route through `extract_callee_name`
                // above, but the lowercase form does not. Mirror the
                // typechecker's `infer_method_call` lowercase-module branch
                // (line ~8725) and the interpreter's `eval_method_call` alias
                // map by emitting the capitalized `Env.<method>` call key here
                // so seeded `inferred_effects` flow to the caller.
                if let ExprKind::Identifier(mod_name) = &object.kind {
                    if mod_name == "env" {
                        calls.push((format!("Env.{}", method), expr.span.clone()));
                    }
                }
                // Stdlib methods whose effects are pre-seeded in inferred_effects.
                // Matched by method name (conservatively — no receiver type info here).
                // Conservative over-approximation is acceptable; false negatives are not.
                const STDLIB_METHOD_MAP: &[(&str, &str)] = &[
                    ("push", "Vec.push"),
                    ("extend_from_slice", "Vec.extend_from_slice"),
                    // `VecDeque[T]` mutating method surface — paired with
                    // the matching `inferred_effects` seeds in
                    // `effectchecker.rs::seed_builtin_effects`. Without
                    // these, the auto-parallelizer's
                    // `method_effects_imply_receiver_mutation` lookup
                    // doesn't find any non-pure verb on a bare
                    // `push_back`/etc. method name and the receiver is
                    // racily captured-by-value (Map+VecDeque corruption
                    // repro 2026-05-16).
                    ("push_back", "VecDeque.push_back"),
                    ("push_front", "VecDeque.push_front"),
                    ("pop_back", "VecDeque.pop_back"),
                    ("pop_front", "VecDeque.pop_front"),
                    ("push_str", "String.push_str"),
                    ("insert", "Map.insert"),
                    ("insert", "SortedSet.insert"),
                    ("insert", "Set.insert"),
                    ("try_insert", "Map.try_insert"),
                    ("entry", "Map.entry"),
                    ("extend", "Map.extend"),
                    ("merge", "Map.merge"),
                    ("keys", "Map.keys"),
                    ("values", "Map.values"),
                    ("entries", "Map.entries"),
                    ("clone", "Map.clone"),
                    ("clone", "Set.clone"),
                    ("union", "Set.union"),
                    ("intersection", "Set.intersection"),
                    ("difference", "Set.difference"),
                    ("send", "Sender.send"),
                    ("recv", "Receiver.recv"),
                    ("chunk_by", "Iterator.chunk_by"),
                    ("chunks", "Iterator.chunks"),
                    ("windows", "Iterator.windows"),
                ];
                for &(method_name, qualified) in STDLIB_METHOD_MAP {
                    if method.as_str() == method_name {
                        calls.push((qualified.to_string(), expr.span.clone()));
                    }
                }
                // Fallible-allocation instance companions (phase-8-stdlib-floor
                // item 2) — `try_push` / `try_insert` / `try_clone` / … carry
                // `allocates(Heap)`, like their panicking counterparts. Matched
                // by name (conservative over-approximation, consistent with the
                // map above) and routed to the seeded `TRY_ALLOC_EFFECT_KEY`.
                if crate::fallible_alloc::instance_companion_base(method).is_some() {
                    calls.push((
                        crate::fallible_alloc::TRY_ALLOC_EFFECT_KEY.to_string(),
                        expr.span.clone(),
                    ));
                }
                // `f.trunc_to_<intN>()` carries `panics` (the trapping float→int
                // form — phase-8 cast slice 2). Matched by name prefix: no
                // non-float type carries a `trunc_to_*` method, so a
                // receiver-type check is unnecessary (and conservative
                // over-approximation is acceptable here anyway). Routed to the
                // `float.trunc_to_int` key seeded in `seed_builtin_effects`.
                if let Some(suffix) = method.as_str().strip_prefix("trunc_to_") {
                    if crate::numeric_conv::is_int_target(suffix) {
                        calls.push(("float.trunc_to_int".to_string(), expr.span.clone()));
                    }
                }
                // Function-reference argument propagation, mirror of the
                // `Call` branch above. When the resolved `Type.method` callee
                // is `with _`-polymorphic (or transitively calls a `with _`
                // function), every named-fn argument contributes its effects
                // directly so the caller's inferred set covers the closure
                // performed via the function reference. Without this, a public
                // method like `Runner.run_each(write_log)` would infer no
                // effects through `write_log` even though `run_each` calls
                // it with `with E`.
                if let Some(callee_key) = self.resolve_method_callee_key(&expr.span) {
                    if self.is_polymorphic_callee(&callee_key)
                        || self.calls_polymorphic.contains(callee_key.as_str())
                    {
                        for arg in args {
                            if let ExprKind::Identifier(arg_name) = &arg.value.kind {
                                if self.is_user_function(arg_name) {
                                    calls.push((arg_name.clone(), arg.value.span.clone()));
                                }
                            }
                        }
                    }
                }
                self.collect_calls_in_expr(object, calls, bounds);
                for arg in args {
                    self.collect_calls_in_expr(&arg.value, calls, bounds);
                }
            }
            ExprKind::Binary { left, op, right } => {
                self.collect_calls_in_expr(left, calls, bounds);
                self.collect_calls_in_expr(right, calls, bounds);
                if matches!(op, BinOp::Div | BinOp::Mod) {
                    calls.push(("__builtin_div_rem".to_string(), expr.span.clone()));
                }
            }
            ExprKind::Pipe { left, right } => {
                self.collect_calls_in_expr(left, calls, bounds);
                self.collect_calls_in_expr(right, calls, bounds);
            }
            ExprKind::Unary { operand, .. } => {
                self.collect_calls_in_expr(operand, calls, bounds);
            }
            ExprKind::Block(block) | ExprKind::Comptime(block) => {
                for stmt in &block.stmts {
                    self.collect_calls_in_stmt(stmt, calls, bounds);
                }
                if let Some(ref e) = block.final_expr {
                    self.collect_calls_in_expr(e, calls, bounds);
                }
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.collect_calls_in_expr(condition, calls, bounds);
                let block_calls = self.collect_calls_in_block(then_block, bounds);
                calls.extend(block_calls);
                if let Some(ref e) = else_branch {
                    self.collect_calls_in_expr(e, calls, bounds);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.collect_calls_in_expr(value, calls, bounds);
                let block_calls = self.collect_calls_in_block(then_block, bounds);
                calls.extend(block_calls);
                if let Some(ref e) = else_branch {
                    self.collect_calls_in_expr(e, calls, bounds);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.collect_calls_in_expr(scrutinee, calls, bounds);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.collect_calls_in_expr(guard, calls, bounds);
                    }
                    self.collect_calls_in_expr(&arm.body, calls, bounds);
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
                self.collect_calls_in_expr(condition, calls, bounds);
                let block_calls = self.collect_calls_in_block(body, bounds);
                calls.extend(block_calls);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.collect_calls_in_expr(value, calls, bounds);
                let block_calls = self.collect_calls_in_block(body, bounds);
                calls.extend(block_calls);
            }
            ExprKind::Loop { body, .. }
            | ExprKind::Unsafe(body)
            | ExprKind::Try(body)
            | ExprKind::Seq(body)
            | ExprKind::Par(body) => {
                let block_calls = self.collect_calls_in_block(body, bounds);
                calls.extend(block_calls);
            }
            ExprKind::LabeledBlock { body, .. } => {
                let block_calls = self.collect_calls_in_block(body, bounds);
                calls.extend(block_calls);
            }
            ExprKind::Lock { body, .. } => {
                let block_calls = self.collect_calls_in_block(body, bounds);
                calls.extend(block_calls);
            }
            ExprKind::Closure { body, .. } => {
                self.collect_calls_in_expr(body, calls, bounds);
            }
            ExprKind::Return(Some(inner)) | ExprKind::Question(inner) => {
                self.collect_calls_in_expr(inner, calls, bounds);
            }
            ExprKind::Break {
                value: Some(inner), ..
            } => {
                self.collect_calls_in_expr(inner, calls, bounds);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.collect_calls_in_expr(object, calls, bounds);
            }
            ExprKind::Index { object, index } => {
                self.collect_calls_in_expr(object, calls, bounds);
                self.collect_calls_in_expr(index, calls, bounds);
                // Indexing with [] calls Index::index which has panics effect
                // (can panic on out-of-bounds). Use .get() for fallible access.
                calls.push(("__builtin_index".to_string(), expr.span.clone()));
            }
            ExprKind::Tuple(exprs) => {
                for e in exprs {
                    self.collect_calls_in_expr(e, calls, bounds);
                }
            }
            ExprKind::MapLiteral(entries) => {
                for (key, val) in entries {
                    self.collect_calls_in_expr(key, calls, bounds);
                    self.collect_calls_in_expr(val, calls, bounds);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.collect_calls_in_expr(&f.value, calls, bounds);
                }
                if let Some(ref s) = spread {
                    self.collect_calls_in_expr(s, calls, bounds);
                }
            }
            ExprKind::Cast { expr: inner, ty } => {
                // `x as Refined` is a refinement assertion — a runtime
                // predicate check that panics on failure (design.md §
                // Refinement Types, "`as` disambiguation rule"). Attribute
                // the synthetic `__builtin_refinement_assert` callee so the
                // `panics` effect propagates to the enclosing function.
                // Numeric / pointer / other casts carry no effect.
                if self.cast_target_is_refinement(ty) {
                    calls.push(("__builtin_refinement_assert".to_string(), expr.span.clone()));
                }
                self.collect_calls_in_expr(inner, calls, bounds);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.collect_calls_in_expr(s, calls, bounds);
                }
                if let Some(e) = end {
                    self.collect_calls_in_expr(e, calls, bounds);
                }
            }
            ExprKind::Path { segments, .. } => {
                // A path like Foo::bar used as a value — could be a function reference
                if segments.len() == 2 {
                    let key = format!("{}.{}", segments[0], segments[1]);
                    if self.function_bodies.contains_key(&key)
                        || self.method_bodies.contains_key(&key)
                    {
                        calls.push((key, expr.span.clone()));
                    }
                }
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.collect_calls_in_expr(object, calls, bounds);
                if let Some(args) = args {
                    for arg in args {
                        self.collect_calls_in_expr(&arg.value, calls, bounds);
                    }
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.collect_calls_in_expr(left, calls, bounds);
                self.collect_calls_in_expr(right, calls, bounds);
            }
            ExprKind::ArrayLiteral(elements) => {
                for e in elements {
                    self.collect_calls_in_expr(e, calls, bounds);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.collect_calls_in_expr(value, calls, bounds);
                self.collect_calls_in_expr(count, calls, bounds);
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.collect_calls_in_expr(e, calls, bounds);
                }
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    // Phase-10 target gate: record the binding as a
                    // synthetic pseudo-call (same trick as the
                    // `__modbind_*` keys) so the gate pass can recover
                    // per-function provider bindings from the existing
                    // call collection. Real consumers are unaffected:
                    // `build_call_graph` filters to known fn names and
                    // `get_callee_effects` misses harmlessly.
                    calls.push((
                        format!("__providers_bind::{}", b.resource),
                        b.resource_span.clone(),
                    ));
                    self.collect_calls_in_expr(&b.value, calls, bounds);
                }
                let block_calls = self.collect_calls_in_block(body, bounds);
                calls.extend(block_calls);
            }
            // Leaf expressions — no calls to collect
            ExprKind::Identifier(_)
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
}
