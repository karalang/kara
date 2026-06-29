//! FE-4 — effect enforcement from `#[gpu]` roots.
//!
//! The GPU execution model is allocation-free and has no access to host
//! resources beyond explicit GPU buffers. So a function reachable from a
//! `#[gpu]` root must not, anywhere in its transitive call graph, perform a
//! GPU-forbidden effect (design.md § GPU Subset Constraints — the *Not
//! Allowed* column):
//!
//! - **`allocates(Heap)`** — no host heap exists at kernel-execution level.
//! - **`sends` / `receives`** — channel / network operations have no GPU analog.
//! - **`reads` / `writes`** on a *host* resource other than `GpuBuffer` — the
//!   only host resource a GPU kernel may touch is an explicit GPU buffer.
//!
//! **`panics` is deferred to FE-4b** — see [`gpu_forbidden_reason`]: implicit
//! bounds-check / divide-by-zero panics are pervasive in pure arithmetic and
//! GPU-acceptable (trap/clamp, not unwind), so forbidding the `panics` effect
//! wholesale would reject every real kernel. Catching only the *explicit*
//! emitters (`panic()`/`todo()`/`unreachable()`) needs a panic-origin
//! refinement the effect set does not carry today.
//!
//! This reuses the effect checker's existing call-graph + direct-use
//! machinery (the same shape as `target_gate.rs`, which gates host resources
//! from `main`): after inference settles, it charges each forbidden effect to
//! the function that directly performs it (a `Direct` origin, or a `Callee`
//! origin whose callee is body-less — host fn / extern / builtin — so its
//! effect is charged at the call site), then walks from each `#[gpu]` root and
//! reports every reachable direct violation with the call chain from the root.
//! No provider-discharge bookkeeping (providers are an SSR concern, not GPU).

use std::collections::{HashMap, HashSet};

use crate::ast::EffectVerbKind;
use crate::token::Span;

use super::{Effect, EffectError, EffectErrorKind, EffectOrigin};

impl<'a> super::EffectChecker<'a> {
    /// FE-4 entry point — invoked after `infer_effects` settles (alongside
    /// `check_target_gate`). No-op when the program has no `#[gpu]` function.
    pub(crate) fn check_gpu_effect_gate(&mut self) {
        // `#[gpu]` roots, keyed identically to the call graph / inferred
        // effects (`name` for free fns, `Type.method` for methods).
        let mut roots: Vec<String> = self
            .function_bodies
            .iter()
            .chain(self.method_bodies.iter())
            .filter(|(_, f)| f.is_gpu)
            .map(|(k, _)| k.clone())
            .collect();
        if roots.is_empty() {
            return;
        }
        roots.sort();
        roots.dedup();

        let graph = self.build_call_graph();

        // Direct GPU-forbidden uses per function. A `Callee` origin whose
        // callee has a body node is examined at that node; a body-less
        // callee (host fn / extern / builtin) is charged at the call site.
        let mut direct: HashMap<String, Vec<(String, Span)>> = HashMap::new();
        for (fn_name, set) in &self.inferred_effects {
            for te in &set.effects {
                let Some(reason) = gpu_forbidden_reason(&te.effect) else {
                    continue;
                };
                let span_here = match &te.origin {
                    EffectOrigin::Direct(span) => Some(span.clone()),
                    EffectOrigin::Callee {
                        fn_name: callee,
                        span,
                    } => {
                        if graph.contains_key(callee) {
                            None
                        } else {
                            Some(span.clone())
                        }
                    }
                };
                if let Some(span) = span_here {
                    direct
                        .entry(fn_name.clone())
                        .or_default()
                        .push((reason, span));
                }
            }
        }

        let mut errors: Vec<EffectError> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut path: Vec<String> = Vec::new();
        for root in &roots {
            gpu_gate_dfs(root, &graph, &direct, &mut path, &mut visited, &mut errors);
        }
        self.errors.extend(errors);
    }
}

/// DFS from a `#[gpu]` root over the call graph. Global `visited` (each node's
/// direct violations reported once, with the first root→…→node path found);
/// `path` carries the chain for the diagnostic.
fn gpu_gate_dfs(
    fn_name: &str,
    graph: &HashMap<String, Vec<(String, Span)>>,
    direct: &HashMap<String, Vec<(String, Span)>>,
    path: &mut Vec<String>,
    visited: &mut HashSet<String>,
    errors: &mut Vec<EffectError>,
) {
    if !visited.insert(fn_name.to_string()) {
        return;
    }
    path.push(fn_name.to_string());

    if let Some(violations) = direct.get(fn_name) {
        let chain = path.join(" → ");
        for (reason, span) in violations {
            errors.push(EffectError {
                message: format!(
                    "a `#[gpu]` function must not perform {reason}; it is reached on the call \
                     chain {chain}. The GPU subset is allocation-free and has no host I/O or \
                     channels — restructure to keep it off the `#[gpu]` call graph. See \
                     design.md § GPU Subset Constraints.",
                ),
                span: span.clone(),
                kind: EffectErrorKind::GpuEffectViolation,
                subtype_trace: None,
                replacement: None,
            });
        }
    }

    if let Some(calls) = graph.get(fn_name) {
        let mut seen: HashSet<&str> = HashSet::new();
        for (callee, _) in calls {
            if seen.insert(callee.as_str()) {
                gpu_gate_dfs(callee, graph, direct, path, visited, errors);
            }
        }
    }

    path.pop();
}

/// Classify an effect as GPU-forbidden, returning a human reason (already
/// back-tick-wrapped where it names the effect) or `None` if allowed.
///
/// **`panics` is intentionally NOT forbidden here (tracked as FE-4b).** Pure
/// arithmetic infers `panics` from implicit bounds-check (`__builtin_index`)
/// and divide-by-zero (`__builtin_div_rem`) traps, which a GPU handles by
/// trapping/clamping rather than unwinding — so they are GPU-acceptable. The
/// design's target is *explicit* emitters (`panic()`/`todo()`/`unreachable()`);
/// the effect set conflates the two, so distinguishing them needs a
/// panic-origin refinement. Forbidding `panics` wholesale would reject every
/// real kernel (the canonical `dot` indexes arrays), so it is deferred.
fn gpu_forbidden_reason(effect: &Effect) -> Option<String> {
    match &effect.verb {
        EffectVerbKind::Allocates if effect.resource == "Heap" => {
            Some("`allocates(Heap)` (heap allocation)".to_string())
        }
        EffectVerbKind::Sends => Some(format!(
            "`sends({})` (channel/network send)",
            effect.resource
        )),
        EffectVerbKind::Receives => Some(format!(
            "`receives({})` (channel/network receive)",
            effect.resource
        )),
        EffectVerbKind::Reads | EffectVerbKind::Writes
            if crate::target::is_host_resource(&effect.resource)
                && effect.resource != "GpuBuffer" =>
        {
            let verb = if matches!(effect.verb, EffectVerbKind::Reads) {
                "reads"
            } else {
                "writes"
            };
            Some(format!("`{verb}({})` (host I/O)", effect.resource))
        }
        _ => None,
    }
}
