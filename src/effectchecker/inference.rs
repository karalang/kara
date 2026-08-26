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

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::*;
use crate::intern::Symbol;
use crate::token::Span;

use super::{
    tarjan_scc, DeclaredEffects, Effect, EffectError, EffectErrorKind, EffectOrigin, EffectSet,
};

/// Which edge kinds `build_call_graph_with` puts in the graph.
///
/// The distinction only matters for a consumer that makes a claim about
/// CALLS specifically; everything that reasons about effect propagation
/// wants both, because a function mentioned as a value contributes its
/// effects exactly as a called one does (B-2026-08-23-7).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeKinds {
    /// Call edges plus function-as-value mention edges — the graph effect
    /// inference runs on.
    CallsAndValueRefs,
    /// Call edges only.
    CallsOnly,
}

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

/// The effect sets a callee contributes, borrowed from the checker's
/// tables — see [`super::EffectChecker::callee_effect_sets`]. `Two` is
/// the `PolymorphicWithFixed` union: iteration yields the first set in
/// full, then the second set filtered by membership in the first (the
/// same deduped union the old `HashSet`-collecting lookup built, minus
/// the allocations).
pub(crate) enum CalleeEffectSets<'a> {
    None,
    One(&'a EffectSet),
    Two(&'a EffectSet, &'a EffectSet),
}

pub(crate) struct CalleeEffectIter<'a> {
    first: std::slice::Iter<'a, super::TracedEffect>,
    /// `(filter_set, iter)` — yield only effects absent from `filter_set`.
    second: Option<(&'a EffectSet, std::slice::Iter<'a, super::TracedEffect>)>,
}

impl<'a> CalleeEffectSets<'a> {
    pub(crate) fn iter(&self) -> CalleeEffectIter<'a> {
        match self {
            CalleeEffectSets::None => CalleeEffectIter {
                first: [].iter(),
                second: None,
            },
            CalleeEffectSets::One(set) => CalleeEffectIter {
                first: set.effects.iter(),
                second: None,
            },
            CalleeEffectSets::Two(first, second) => CalleeEffectIter {
                first: first.effects.iter(),
                second: Some((first, second.effects.iter())),
            },
        }
    }
}

impl<'a> Iterator for CalleeEffectIter<'a> {
    type Item = &'a Effect;

    fn next(&mut self) -> Option<&'a Effect> {
        if let Some(te) = self.first.next() {
            return Some(&te.effect);
        }
        if let Some((filter_set, iter)) = &mut self.second {
            for te in iter.by_ref() {
                if !filter_set.contains(&te.effect) {
                    return Some(&te.effect);
                }
            }
        }
        None
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
        let mut trait_methods: FxHashMap<String, Vec<SeedMethod>> = FxHashMap::default();
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
                            self_span: m.self_span,
                            span: m.span,
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
            // The UNION of every declared bound's methods (design.md:7216 —
            // a provider must implement all of them, so every one of their
            // methods is reachable as `R.method(...)`). One bound is the
            // common case and iterates once; a bare resource iterates zero
            // times. B-2026-08-19-3.
            for m in r
                .provider_bounds
                .iter()
                .filter_map(|b| trait_methods.get(&b.name))
                .flatten()
            {
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
                        let span = m.self_span.unwrap_or(m.span);
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
                            span,
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
                let key = self.interner.dotted_str(&r.name, &m.name);
                let mut set = EffectSet::new();
                set.add(
                    Effect {
                        verb,
                        resource: r.name.clone(),
                    },
                    EffectOrigin::Direct(*builtin_span),
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
                    .get(&self.interner.dotted_str(&m.trait_name, &m.name))
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
        for name in self.function_bodies.keys().copied().collect::<Vec<_>>() {
            self.inferred_effects.entry(name).or_default();
        }
        for name in self.method_bodies.keys().copied().collect::<Vec<_>>() {
            self.inferred_effects.entry(name).or_default();
        }

        // Build the call graph and decompose into SCCs (Tarjan's algorithm).
        // tarjan_scc returns SCCs in reverse topological order — leaves first,
        // roots last — so processing them in order propagates callee effects
        // into callers before callers are processed.
        let call_graph = self.build_call_graph();
        let all_fn_names: FxHashSet<Symbol> = self
            .function_bodies
            .keys()
            .chain(self.method_bodies.keys())
            .copied()
            .collect();
        let sccs = tarjan_scc(&all_fn_names, &call_graph, &self.interner);

        for scc in &sccs {
            if scc.len() == 1 {
                // Non-recursive: one pass is sufficient because all callees
                // (in earlier SCCs) are already fully resolved.
                let name = scc[0];
                let body = self
                    .function_bodies
                    .get(&name)
                    .cloned()
                    .or_else(|| self.method_bodies.get(&name).cloned());
                if let Some(f) = body {
                    self.infer_function_effects(name, &f.body);
                }
            } else {
                // Mutually-recursive SCC: iterate until convergence.
                // Effects are monotone, so at most |SCC| passes are needed
                // (each pass can propagate effects one hop around the cycle).
                for _ in 0..=scc.len() {
                    let mut changed = false;
                    for &name in scc {
                        let body = self
                            .function_bodies
                            .get(&name)
                            .cloned()
                            .or_else(|| self.method_bodies.get(&name).cloned());
                        if let Some(f) = body {
                            if self.infer_function_effects(name, &f.body) {
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
        let mut trait_impl_types: FxHashMap<(Symbol, Symbol), Vec<Symbol>> = FxHashMap::default();
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
                    .entry((
                        self.interner.intern(&trait_name),
                        self.interner.intern(&method.name),
                    ))
                    .or_default()
                    .push(self.interner.intern(&type_name));
            }
        }

        // For each private trait method with no explicit ceiling, union impl effects.
        let items: &[Item] = &self.program.items;
        for item in items {
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
                let trait_key = self.interner.dotted_str(&t.name, &m.name);
                // Only infer for methods with no explicit ceiling (None).
                if !matches!(
                    self.declared_effects.get(&trait_key),
                    Some(DeclaredEffects::None) | None
                ) {
                    continue;
                }
                let impl_types = trait_impl_types
                    .get(&(self.interner.intern(&t.name), self.interner.intern(&m.name)))
                    .cloned()
                    .unwrap_or_default();
                let mut ceiling = EffectSet::new();
                for &type_sym in &impl_types {
                    let impl_key = self
                        .interner
                        .dotted(type_sym, self.interner.intern(&m.name));
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
    pub(crate) fn build_call_graph(&self) -> FxHashMap<Symbol, Vec<(Symbol, Span)>> {
        self.build_call_graph_with(EdgeKinds::CallsAndValueRefs)
    }

    /// The same graph with the function-as-value edges (B-2026-08-23-7) left
    /// out, so every remaining edge is a genuine call.
    ///
    /// Effect inference wants both edge kinds — a mentioned function's
    /// effects propagate exactly as a called one's do, which is the whole
    /// point of -7 — so `build_call_graph` is the right graph for every
    /// consumer that asks "whose effects feed whose". This variant exists
    /// for the one question where the distinction is load-bearing: the
    /// `mutual_recursion_note` claims two functions CALL each other, and a
    /// cycle built out of value references is not that (B-2026-08-23-13).
    pub(crate) fn build_call_graph_calls_only(&self) -> FxHashMap<Symbol, Vec<(Symbol, Span)>> {
        self.build_call_graph_with(EdgeKinds::CallsOnly)
    }

    fn build_call_graph_with(&self, kinds: EdgeKinds) -> FxHashMap<Symbol, Vec<(Symbol, Span)>> {
        let all_fn_names: FxHashSet<Symbol> = self
            .function_bodies
            .keys()
            .chain(self.method_bodies.keys())
            .copied()
            .collect();
        let mut graph: FxHashMap<Symbol, Vec<(Symbol, Span)>> = FxHashMap::default();
        let empty_bounds: FxHashMap<String, Vec<TraitBound>> = FxHashMap::default();
        // Function-as-value edges (B-2026-08-23-7) belong in the GRAPH, not
        // only in `infer_function_effects`. A single-member SCC is inferred
        // in ONE pass on the premise that every callee sits in an earlier
        // SCC and is already resolved; an edge missing here breaks that
        // premise, and a mentioned function whose own effects are INFERRED
        // (rather than declared, hence seeded up front) would be read while
        // still empty. Declared-effect callees would have masked this.
        for (&name, func) in &self.function_bodies {
            let bounds = self.fn_bounds_index.get(&name).unwrap_or(&empty_bounds);
            let relevant = self
                .collect_calls_in_block(&func.body, bounds)
                .into_iter()
                .chain(
                    self.fn_value_ref_calls
                        .get(&name)
                        .filter(|_| kinds == EdgeKinds::CallsAndValueRefs)
                        .into_iter()
                        .flatten()
                        .copied(),
                )
                .filter(|(callee, _)| all_fn_names.contains(callee))
                .collect();
            graph.insert(name, relevant);
        }
        for (&name, func) in &self.method_bodies {
            let bounds = self.fn_bounds_index.get(&name).unwrap_or(&empty_bounds);
            let relevant = self
                .collect_calls_in_block(&func.body, bounds)
                .into_iter()
                .chain(
                    self.fn_value_ref_calls
                        .get(&name)
                        .filter(|_| kinds == EdgeKinds::CallsAndValueRefs)
                        .into_iter()
                        .flatten()
                        .copied(),
                )
                .filter(|(callee, _)| all_fn_names.contains(callee))
                .collect();
            graph.insert(name, relevant);
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
    ///
    /// Callees are deduplicated per pass (first call site wins): repeated
    /// calls to the same callee could only re-contribute the same effects,
    /// and `EffectSet::add` keeps the first origin anyway, so skipping the
    /// duplicates is behavior-identical and drops the per-call-site
    /// effect-clone traffic. `fn_name`'s own entry is temporarily taken out
    /// of `inferred_effects` while its callees are consulted — a
    /// self-recursive lookup then sees an empty set instead of the pre-pass
    /// set, which is equivalent: everything the pre-pass set contains is
    /// already in `current`, so those adds were no-ops either way.
    pub(crate) fn infer_function_effects(&mut self, fn_name: Symbol, body: &Block) -> bool {
        let mut calls = {
            // Borrow the bounds map instead of cloning it per call — the
            // collection walk only needs `&self` (the old clone predates
            // that; DHAT flagged it as real per-pass traffic).
            let empty_bounds: FxHashMap<String, Vec<TraitBound>> = FxHashMap::default();
            let bounds = self.fn_bounds_index.get(&fn_name).unwrap_or(&empty_bounds);
            self.collect_calls_in_block(body, bounds)
        };
        // Append synthetic per-binding-resource call entries
        // (design.md §1322) — reads / writes of module-level `let mut`
        // bindings flow through the same call-graph propagation by
        // dispatching to seeded `__modbind_*` synthetic keys.
        let param_names: Vec<String> = self
            .function_bodies
            .get(&fn_name)
            .map(|f| self.function_param_names(f))
            .or_else(|| {
                self.method_bodies
                    .get(&fn_name)
                    .map(|f| self.function_param_names(f))
            })
            .unwrap_or_default();
        calls.extend(self.collect_modbind_synth_calls_in_block(body, &param_names));
        // Function-as-value edges (B-2026-08-23-7) ride the same channel: a
        // mention of a free function in value position contributes its
        // effects exactly as a direct call would. Precomputed per function,
        // so an SCC's convergence passes re-read rather than re-walk.
        calls.extend(
            self.fn_value_ref_calls
                .get(&fn_name)
                .into_iter()
                .flatten()
                .copied(),
        );

        // Dedupe callees in place (first call site wins — matching
        // `EffectSet::add`'s first-origin-wins semantics).
        let mut seen_callees: FxHashSet<Symbol> = FxHashSet::default();
        calls.retain(|&(callee, _)| seen_callees.insert(callee));

        // Propagate the polymorphic marker only for callees that use
        // `with _` (anonymous polymorphism). A callee that declares
        // only `with E` (named) resolves its effect variable at the
        // call site against concrete bindings, so it does not "leak"
        // through callers that lack a `with _` of their own. Hoisted
        // out of the effects loop so that loop holds no `&mut self`
        // (equivalent: the marker only ever inserts `fn_name` itself,
        // which could only influence a self-recursive callee check
        // that was already poly-or-not identically).
        let any_poly = calls.iter().any(|&(callee, _)| {
            self.fn_uses_with_underscore.contains(&callee)
                || self.calls_polymorphic.contains(&callee)
        });
        if any_poly {
            self.calls_polymorphic.insert(fn_name);
        }

        let mut changed = false;
        let mut current = self.inferred_effects.remove(&fn_name).unwrap_or_default();
        for &(callee_name, call_span) in &calls {
            // Iterate the callee's effects by reference — clone an
            // `Effect` only when it is genuinely new to `current`.
            for effect in self.callee_effect_sets(callee_name).iter() {
                if !current.contains(effect) {
                    current.add(
                        effect.clone(),
                        EffectOrigin::Callee {
                            fn_name: self.interner.resolve(callee_name).to_string(),
                            span: call_span,
                        },
                    );
                    changed = true;
                }
            }
        }
        self.inferred_effects.insert(fn_name, current);
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

        let empty_bounds: FxHashMap<String, Vec<TraitBound>> = FxHashMap::default();
        let mut violations: Vec<(Effect, Span, &'static str)> = Vec::new();
        for (expr, kind) in &clauses {
            let mut calls = Vec::new();
            self.collect_calls_in_expr(expr, &mut calls, &empty_bounds);
            for &(callee, call_span) in &calls {
                for effect in self.callee_effect_sets(callee).iter() {
                    if effect.verb != EffectVerbKind::Panics {
                        violations.push((effect.clone(), call_span, *kind));
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

    /// The effect sets a callee contributes, BY REFERENCE — no
    /// `HashSet` build, no `Effect` clone, no `Vec` (DHAT put the old
    /// collecting version at ~9% of all front-end allocation bytes;
    /// hashing `Effect`'s String resource was also a chunk of the
    /// remaining SipHash tail). Iterate with
    /// [`CalleeEffectSets::iter`]; clone only what you keep.
    ///
    /// For public functions: declared effects (inference firewall).
    /// For private functions: inferred effects.
    /// For polymorphic (`with _`) functions: inferred effects
    /// (transparent — the function's own internal effects are
    /// contributed to the caller). For `PolymorphicWithFixed`: the
    /// union of fixed + inferred, deduped by filtering the inferred
    /// side against the fixed side (same set the old `HashSet` union
    /// produced). Note: effects from closure arguments are already
    /// propagated because `collect_calls_in_expr` walks into closure
    /// bodies at the call site.
    pub(crate) fn callee_effect_sets(&self, callee_name: Symbol) -> CalleeEffectSets<'_> {
        let is_pub = self
            .function_visibility
            .get(&callee_name)
            .copied()
            .unwrap_or(false);

        if is_pub {
            match self.declared_effects.get(&callee_name) {
                Some(DeclaredEffects::Explicit(set)) => CalleeEffectSets::One(set),
                Some(DeclaredEffects::Polymorphic) => {
                    match self.inferred_effects.get(&callee_name) {
                        Some(set) => CalleeEffectSets::One(set),
                        None => CalleeEffectSets::None,
                    }
                }
                Some(DeclaredEffects::PolymorphicWithFixed(fixed)) => {
                    match self.inferred_effects.get(&callee_name) {
                        Some(inferred) => CalleeEffectSets::Two(fixed, inferred),
                        None => CalleeEffectSets::One(fixed),
                    }
                }
                Some(DeclaredEffects::None) | None => CalleeEffectSets::None, // pure
            }
        } else {
            match self.inferred_effects.get(&callee_name) {
                Some(set) => CalleeEffectSets::One(set),
                None => CalleeEffectSets::None,
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
        bounds: &FxHashMap<String, Vec<TraitBound>>,
    ) -> Vec<(Symbol, Span)> {
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
        calls: &mut Vec<(Symbol, Span)>,
        bounds: &FxHashMap<String, Vec<TraitBound>>,
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
    fn is_polymorphic_callee(&self, name: Symbol) -> bool {
        matches!(
            self.declared_effects.get(&name),
            Some(DeclaredEffects::Polymorphic | DeclaredEffects::PolymorphicWithFixed(_))
        )
    }

    /// The symbol for `name` iff it names a user-defined function (not a
    /// local variable). A non-inserting probe: a name the interner has
    /// never seen cannot key either body table.
    fn user_function_sym(&self, name: &str) -> Option<Symbol> {
        let sym = self.interner.get(name)?;
        (self.function_bodies.contains_key(&sym) || self.method_bodies.contains_key(&sym))
            .then_some(sym)
    }

    /// True iff `ty` is a path naming a refinement type alias — i.e. an
    /// `x as ty` cast is a refinement assertion that propagates `panics`.
    /// Borrow / pointer / generic wrappers around a refinement are not
    /// themselves assertion targets, so only a bare path is checked.
    fn cast_target_is_refinement(&self, ty: &crate::ast::TypeExpr) -> bool {
        if let crate::ast::TypeKind::Path(path) = &ty.kind {
            if let Some(name) = path.segments.last() {
                return self
                    .interner
                    .get(name)
                    .is_some_and(|sym| self.refinement_type_names.contains(&sym));
            }
        }
        false
    }

    pub(crate) fn collect_calls_in_expr(
        &self,
        expr: &Expr,
        calls: &mut Vec<(Symbol, Span)>,
        bounds: &FxHashMap<String, Vec<TraitBound>>,
    ) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                // Trait-assoc-fn redirect: when the callee dispatches through
                // a typeparam-bound trait, push the resolved `Trait.method`
                // key(s) so the trait's declared ceiling propagates to the
                // caller. Concrete-type dispatch (`Wrapper.method()`) and
                // ordinary free-function calls still use `extract_callee_name`.
                let trait_keys = self.extract_trait_assoc_fn_keys(callee, bounds);
                let callee_name: Option<Symbol> = if !trait_keys.is_empty() {
                    for &key in &trait_keys {
                        calls.push((key, expr.span));
                    }
                    trait_keys.into_iter().next()
                } else {
                    let n = self.extract_callee_name(callee);
                    if let Some(name) = n {
                        calls.push((name, expr.span));
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
                    if self
                        .interner
                        .get(n)
                        .is_some_and(|sym| self.refinement_type_names.contains(&sym))
                    {
                        calls.push((self.syms.builtin_refinement_assert, expr.span));
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
                if let Some(cname) = callee_name {
                    if self.is_polymorphic_callee(cname) || self.calls_polymorphic.contains(&cname)
                    {
                        for arg in args {
                            if let ExprKind::Identifier(arg_name) = &arg.value.kind {
                                if let Some(sym) = self.user_function_sym(arg_name) {
                                    calls.push((sym, arg.value.span));
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
                                    calls.push((self.interner.dotted_str(t, method), expr.span));
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
                    calls.push((precise_key, expr.span));
                }
                // `CACHE.get()` on a module-level `LazyLock[T]` — attribute the
                // stored initializer's effects to THIS function, which is what
                // design.md § Module-Level Bindings requires: "the effect system
                // attributes the first-access initialization to the *calling*
                // function, not to the module" (B-2026-08-26-16).
                //
                // Without this the effects vanished entirely. `LazyLock.get` is
                // a `#[compiler_builtin]` with no `with` clause, so the callee
                // key contributes nothing, and the closure is at the BINDING
                // rather than at the call site — so unlike
                // `cell.get_or_init(|| load())`, whose closure is a syntactic
                // argument the walk below descends into, there was nothing here
                // to descend into. A `pub fn` could therefore perform undeclared
                // effects, and since effect sets drive conflict analysis, what
                // the checker could not see the scheduler could not conflict-
                // check either.
                //
                // Attributing at EVERY `get()` rather than only the first is
                // deliberate: which call runs the initializer is a runtime race,
                // so the static answer has to hold for all of them. That
                // over-approximates for the callers that hit the cache, which is
                // the sound direction — and it is the same answer the caller
                // would get from `get_or_init`.
                //
                // Gated on the TYPECHECKER's resolved callee, not on the
                // receiver's name alone. A local binding may shadow a module
                // `LazyLock`'s name — `let CACHE: Map[String, i64] = ...;
                // CACHE.get("k")` inside a function, with a module-level
                // `CACHE: LazyLock[i64]` in scope — and a name-only test
                // attributes the LazyLock's effects to that unrelated
                // `Map.get`, which is the exact `map.get()` taint the precise-
                // resolution comment above exists to avoid. The resolved key
                // is `Map.get` there and `LazyLock.get` here, so it separates
                // them by type rather than by spelling.
                // (`is_some` matters: if neither name was ever minted both
                // sides are `None` and a bare `==` would compare equal.)
                let lazy_get_key = self.interner.get("LazyLock.get");
                if method == "get"
                    && lazy_get_key.is_some()
                    && self.resolve_method_callee_key(&expr.span) == lazy_get_key
                {
                    if let ExprKind::Identifier(recv) = &object.kind {
                        if let Some(sym) = self.interner.get(recv.as_str()) {
                            // Cloned out of the map so the borrow ends before the
                            // recursive call, which re-reads `self`.
                            let init = self.lazy_lock_inits.get(&sym).cloned();
                            if let Some(init) = init {
                                // Cycle guard: two `LazyLock`s whose closures call
                                // each other's `get()` form a loop through the
                                // binding table, not through the AST, so
                                // structural descent alone would not terminate.
                                let fresh = self.lazy_lock_in_progress.borrow_mut().insert(sym);
                                if fresh {
                                    self.collect_calls_in_expr(&init, calls, bounds);
                                    self.lazy_lock_in_progress.borrow_mut().remove(&sym);
                                }
                            }
                        }
                    }
                }
                // The bare method name as a symbol — a non-inserting probe:
                // a name the interner never minted cannot key the index, the
                // stdlib-seed map, or match a pre-minted builtin symbol.
                let method_sym = self.interner.get(method.as_str());
                // For method calls without a recorded precise callee, we'd need
                // type info to know the exact method. Fall back to every impl
                // method with a matching bare name, via the index built in
                // `collect_function_info` (scanning all `method_bodies` keys
                // here, once per call site, was the front end's top hotspot).
                if let Some(keys) = method_sym.and_then(|m| self.method_name_index.get(&m)) {
                    for &key in keys {
                        calls.push((key, expr.span));
                    }
                }
                // unwrap() and expect() always panic on None/Err (F-057).
                if method_sym == Some(self.syms.unwrap) {
                    calls.push((self.syms.builtin_unwrap, expr.span));
                } else if method_sym == Some(self.syms.expect) {
                    calls.push((self.syms.builtin_expect, expr.span));
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
                    // The lowercase ambient-module alias map, kept in step with
                    // the typechecker's (`method_identifier_receiver.rs`), the
                    // interpreter's, and codegen's `ambient_resource_for_alias`.
                    // Only `env` / `stdin` were routed before B-2026-08-23-8,
                    // so `fs.read_to_string(...)`, `clock.now()`,
                    // `rand.next_u64()` and the explicit `stdout.` / `stderr.`
                    // forms never reached their seeded keys at all.
                    //
                    // Receiver-keyed, NOT method-name-keyed: `lines` collides
                    // with `BufReader.lines` (`reads(FileSystem)`), so routing
                    // by receiver keeps `stdin.lines`'s `reads(Stdin), blocks`
                    // seed off File-backed readers. The capitalized forms
                    // (`Stdin.lines()`) already route via `extract_callee_name`.
                    // An unseeded method simply finds no entry, so listing a
                    // whole module here is inert for the ones with no effect.
                    let ambient = match mod_name.as_str() {
                        "env" => Some("Env"),
                        "clock" => Some("Clock"),
                        "rand" => Some("RandomSource"),
                        "stdin" => Some("Stdin"),
                        "stdout" => Some("Stdout"),
                        "stderr" => Some("Stderr"),
                        "fs" => Some("FileSystem"),
                        _ => None,
                    };
                    if let Some(res) = ambient {
                        calls.push((self.interner.dotted_str(res, method), expr.span));
                    }
                    if mod_name == "critical_section" {
                        // `critical_section.acquire()` → the dotted seed key
                        // (`writes(Hardware)`, seeded in
                        // `effectchecker.rs::seed_builtin_effects`). Receiver-
                        // keyed like `env`/`stdin` so the `Hardware` effect
                        // reaches the caller's inferred set.
                        calls.push((
                            self.interner.dotted_str("critical_section", method),
                            expr.span,
                        ));
                    }
                }
                // Stdlib methods whose effects are pre-seeded in inferred_effects.
                // Matched by method name (conservatively — no receiver type info
                // here; the seed list lives in the constructor's
                // `STDLIB_METHOD_MAP`). Conservative over-approximation is
                // acceptable; false negatives are not.
                if let Some(quals) = method_sym.and_then(|m| self.stdlib_method_seeds.get(&m)) {
                    for &qualified in quals {
                        calls.push((qualified, expr.span));
                    }
                }
                // Fallible-allocation instance companions (phase-8-stdlib-floor
                // item 2) — `try_push` / `try_insert` / `try_clone` / … carry
                // `allocates(Heap)`, like their panicking counterparts. Matched
                // by name (conservative over-approximation, consistent with the
                // map above) and routed to the seeded `TRY_ALLOC_EFFECT_KEY`.
                if crate::fallible_alloc::instance_companion_base(method).is_some() {
                    calls.push((self.syms.try_alloc, expr.span));
                }
                // `f.trunc_to_<intN>()` carries `panics` (the trapping float→int
                // form — phase-8 cast slice 2). Matched by name prefix: no
                // non-float type carries a `trunc_to_*` method, so a
                // receiver-type check is unnecessary (and conservative
                // over-approximation is acceptable here anyway). Routed to the
                // `float.trunc_to_int` key seeded in `seed_builtin_effects`.
                if let Some(suffix) = method.as_str().strip_prefix("trunc_to_") {
                    if crate::numeric_conv::is_int_target(suffix) {
                        calls.push((self.syms.float_trunc_to_int, expr.span));
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
                    if self.is_polymorphic_callee(callee_key)
                        || self.calls_polymorphic.contains(&callee_key)
                    {
                        for arg in args {
                            if let ExprKind::Identifier(arg_name) = &arg.value.kind {
                                if let Some(sym) = self.user_function_sym(arg_name) {
                                    calls.push((sym, arg.value.span));
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
                    calls.push((self.syms.builtin_div_rem, expr.span));
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
                calls.push((self.syms.builtin_index, expr.span));
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
                    calls.push((self.syms.builtin_refinement_assert, expr.span));
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
                // A path like Foo::bar used as a value — could be a function
                // reference. Non-inserting probe: every `method_bodies` key is
                // minted through `Interner::dotted`, so the pair cache is
                // authoritative; bare `function_bodies` names can never equal
                // a dotted composite, so that half of the old check was
                // vacuous and is dropped.
                if segments.len() == 2 {
                    if let Some(key) = self.interner.get_dotted(&segments[0], &segments[1]) {
                        if self.method_bodies.contains_key(&key) {
                            calls.push((key, expr.span));
                        }
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
                        self.interner
                            .intern(&format!("__providers_bind::{}", b.resource)),
                        b.resource_span,
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
            | ExprKind::ByteStringLit(_)
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
