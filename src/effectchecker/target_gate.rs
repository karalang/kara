//! Phase-10: effect-driven target gating — `design.md § Cross-target
//! Compilation > Effect-Driven Target Gating`.
//!
//! After effect inference settles, walk the transitive call graph from
//! the build's entry point and reject any reachable function whose
//! effect set references a HOST resource the current target does not
//! provide (`target::target_provides`). The diagnostic carries the call
//! chain from the entry point to the offending use, in the `#[gpu]`
//! call-graph format the design specifies.
//!
//! **Provider discharge.** The design's primary SSR pattern binds
//! per-target providers over target-foreign resources (`Display =>
//! HtmlStringBuilder` on native), so a bound resource is NOT a gate
//! violation below its binding. Effect inference is provider-
//! transparent (sets aggregate through `providers { }` unchanged), so
//! the gate tracks discharge itself: provider bindings are recovered
//! per function from the synthetic `__providers_bind::<R>` pseudo-calls
//! the inference walk records, and the DFS carries a discharged-set
//! down the call chain. Granularity is the FUNCTION, not the binding
//! expression — a binding anywhere in `f` discharges the resource for
//! all of `f`'s calls and direct uses. That is slightly lenient for
//! bodies that mix bound and unbound regions (rare; the canonical
//! pattern wraps the whole entry body) and never rejects valid code.
//!
//! **What counts as a direct use at `f`.** A `TracedEffect` whose
//! origin is `Direct`, or whose origin is `Callee { fn_name }` where
//! `fn_name` has no body in this program (host fn / extern / builtin) —
//! body-less callees are not graph nodes, so their effects are charged
//! to the calling function with the call-site span. Body-having callees
//! are examined at their own node, where the discharge state may
//! differ.
//!
//! **Alias canonicalization** (prerequisite recorded in the phase-10
//! tracker): resource names in effect sets are clause strings, and
//! `import std.web.Display as Screen;` legitimately produces
//! `writes(Screen)`. Renamed gated-stdlib clones carry
//! `EffectResourceDecl::canonical_host_name`; the gate canonicalizes
//! through that map before keying the provided-resource table, so
//! renames cannot evade their gate. User-defined resources (no host
//! canonical name, not in the host set) are exempt by design — they
//! exist wherever a provider exists.

use std::collections::{HashMap, HashSet};

use crate::ast::Item;
use crate::token::Span;

use super::{EffectError, EffectErrorKind, EffectOrigin};

/// Prefix of the synthetic pseudo-call the inference walk records for
/// each `providers { R => ... }` binding.
pub(crate) const PROVIDERS_BIND_PREFIX: &str = "__providers_bind::";

impl<'a> super::EffectChecker<'a> {
    /// Run the target gate for the current compilation target. Assumes
    /// `infer_effects` has settled.
    pub(crate) fn check_target_gate(&mut self) {
        let target = crate::target::active_target();

        // Entry points: `main` today (native binaries). Library builds
        // and test entry points are follow-ons recorded in the tracker.
        if !self.function_bodies.contains_key("main") {
            return;
        }

        // Alias → canonical host-resource name map from renamed
        // gated-stdlib declarations in this program.
        let mut canonical: HashMap<String, String> = HashMap::new();
        for item in &self.program.items {
            if let Item::EffectResource(r) = item {
                if let Some(ref host) = r.canonical_host_name {
                    canonical.insert(r.name.clone(), host.clone());
                }
            }
        }
        let canon = |name: &str| -> String {
            canonical
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_string())
        };

        let graph = self.build_call_graph();

        // Per-function provider bindings (function-granular discharge)
        // and direct host-resource violations.
        let mut bound_in: HashMap<String, HashSet<String>> = HashMap::new();
        let mut direct: HashMap<String, Vec<(String, Span)>> = HashMap::new();
        for (fn_name, set) in &self.inferred_effects {
            for te in &set.effects {
                if te.effect.resource.is_empty() {
                    continue; // verb-only effects (blocks/suspends/panics)
                }
                let resource = canon(&te.effect.resource);
                if !crate::target::is_host_resource(&resource) {
                    continue;
                }
                if crate::target::target_provides(target, &resource) {
                    continue;
                }
                let direct_here = match &te.origin {
                    EffectOrigin::Direct(span) => Some(span.clone()),
                    EffectOrigin::Callee {
                        fn_name: callee,
                        span,
                    } => {
                        if graph.contains_key(callee) {
                            None // examined at the callee's own node
                        } else {
                            Some(span.clone()) // body-less callee: charge here
                        }
                    }
                };
                if let Some(span) = direct_here {
                    direct
                        .entry(fn_name.clone())
                        .or_default()
                        .push((resource.clone(), span));
                }
            }
        }
        // `build_call_graph` filters the synthetic pseudo-calls out of
        // the graph it returns, so bindings are recovered from a raw
        // call collection per body.
        let empty_bounds = HashMap::new();
        for (name, func) in self.function_bodies.iter().chain(self.method_bodies.iter()) {
            let bounds = self.fn_bounds_index.get(name).unwrap_or(&empty_bounds);
            for (callee, _) in self.collect_calls_in_block(&func.body, bounds) {
                if let Some(resource) = callee.strip_prefix(PROVIDERS_BIND_PREFIX) {
                    bound_in
                        .entry(name.clone())
                        .or_default()
                        .insert(canon(resource));
                }
            }
        }

        // DFS from `main`, carrying the discharged set. Memoized on
        // (function, discharged-set) so shared subgraphs re-walk only
        // when the discharge state genuinely differs.
        let mut errors: Vec<EffectError> = Vec::new();
        let mut visited: HashSet<(String, Vec<String>)> = HashSet::new();
        let mut path: Vec<String> = Vec::new();
        self.gate_dfs(
            "main",
            &HashSet::new(),
            target,
            &graph,
            &bound_in,
            &direct,
            &mut path,
            &mut visited,
            &mut errors,
        );
        self.errors.extend(errors);
    }

    #[allow(clippy::too_many_arguments)]
    fn gate_dfs(
        &self,
        fn_name: &str,
        discharged: &HashSet<String>,
        target: &str,
        graph: &HashMap<String, Vec<(String, Span)>>,
        bound_in: &HashMap<String, HashSet<String>>,
        direct: &HashMap<String, Vec<(String, Span)>>,
        path: &mut Vec<String>,
        visited: &mut HashSet<(String, Vec<String>)>,
        errors: &mut Vec<EffectError>,
    ) {
        // Discharge set including this function's own bindings — they
        // cover its own direct uses and everything below.
        let mut here = discharged.clone();
        if let Some(bound) = bound_in.get(fn_name) {
            here.extend(bound.iter().cloned());
        }

        let mut memo_key: Vec<String> = here.iter().cloned().collect();
        memo_key.sort();
        if !visited.insert((fn_name.to_string(), memo_key)) {
            return;
        }

        path.push(fn_name.to_string());

        if let Some(violations) = direct.get(fn_name) {
            for (resource, span) in violations {
                if here.contains(resource) {
                    continue;
                }
                let chain = path.join(" → ");
                errors.push(EffectError {
                    message: format!(
                        "target `{target}` does not provide resource '{resource}' — \
                         required on the call chain {chain}; gate it with \
                         `#[target(...)]`, bind a provider for '{resource}', or \
                         keep it off this target's entry path \
                         (design.md § Cross-target Compilation)",
                    ),
                    span: span.clone(),
                    kind: EffectErrorKind::TargetGateViolation,
                    subtype_trace: None,
                });
            }
        }

        if let Some(calls) = graph.get(fn_name) {
            // Dedup callees — multiple call sites to the same function
            // produce identical subtree walks.
            let mut seen: HashSet<&str> = HashSet::new();
            for (callee, _) in calls {
                if !seen.insert(callee.as_str()) {
                    continue;
                }
                self.gate_dfs(
                    callee, &here, target, graph, bound_in, direct, path, visited, errors,
                );
            }
        }

        path.pop();
    }
}
