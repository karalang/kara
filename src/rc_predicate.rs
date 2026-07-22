//! Formal RC-condition evaluator on top of the structured CFG and
//! dominator tree built in `src/cfg.rs` and `src/dominator.rs`.
//!
//! Per design.md § Part 4 RC Dataflow Specification, a binding requires
//! RC fallback when there exist two use-sites C and U (with C ≠ U) such
//! that C is a *consume* and neither block(C) dominates block(U) nor
//! block(U) dominates block(C):
//!
//! ```text
//!     ∃C, U.  C ≠ U  ∧  kind(C) = Consume
//!                    ∧  ¬dom(block(C), block(U))
//!                    ∧  ¬dom(block(U), block(C))
//! ```
//!
//! This is a *necessary* condition for trigger 1 (branch-divergent
//! re-use after consume); the loop-of-consume rule and the trigger-2/3
//! flavors layer in via the use classifier, which is staged for a
//! subsequent round.
//!
//! ## Status
//!
//! Round 12.8 ships the predicate evaluator with synthesized-CFG unit
//! tests. The use classifier (which uses `TypeCheckResult` to mark
//! Consume positions) and the wiring into `src/ownership.rs` follow in
//! later rounds — until then, the linear forward state machine in
//! `ownership.rs` continues to drive live RC fallback decisions.

use crate::ast::{Function, ImplBlock, ImplItem, Item, Program};
use crate::cfg::{
    build_cfg_with_classification, place_paths_disjoint, BlockId, Cfg, ConsumeOrigin, UseKind,
    UseSite,
};
use crate::dominator::{compute_dominators, DominatorTree};
use crate::token::Span;
use crate::typechecker::TypeCheckResult;
use crate::use_classifier::{
    classify_function_body_with, param_types_for_function, ClassifierPrelude,
};
use std::collections::{HashMap, HashSet};

/// Witness pair for a binding that satisfies the formal RC condition.
/// `consume_span` is the Consume use-site C; `other_use_span` is some
/// other use-site U (Read or Consume) where neither block dominates the
/// other. The first witness encountered per binding is kept.
///
/// `consume_origin` is the flavor tag carried on the Consume site C
/// (round 12.14): `Direct` for branch-divergent shapes, `ClosureCapture`
/// for capture-position consumes inside a closure body, `ContainerStore`
/// for sink-args of `mut ref self` method calls. The eventual in-place
/// integration into `OwnershipChecker::check_function_body` will map
/// this onto the legacy `RcTrigger` enum so `RcEntry` records carry the
/// same flavor labels the linear forward state machine produces today.
#[derive(Debug, Clone)]
pub struct RcWitness {
    pub binding: String,
    pub consume_span: Span,
    pub other_use_span: Span,
    pub consume_block: BlockId,
    pub other_block: BlockId,
    pub consume_origin: ConsumeOrigin,
}

/// Evaluate the formal RC predicate over every binding referenced in the
/// CFG. Returns one witness per binding satisfying the predicate (the
/// first such pair encountered in block-id / source order). Bindings
/// with no Consume sites, or whose Consume sites all dominate / are
/// dominated by every other use, are absent from the map.
pub fn rc_candidates(cfg: &Cfg, dom: &DominatorTree) -> HashMap<String, RcWitness> {
    // Bucket every (block, use) pair by binding name. Source order is
    // preserved within each bucket because `cfg.blocks` is iterated in
    // ascending id order and `block.uses` is in source order.
    let mut sites: HashMap<String, Vec<(BlockId, usize, UseSite)>> = HashMap::new();
    for block in &cfg.blocks {
        for (idx, u) in block.uses.iter().enumerate() {
            sites
                .entry(u.binding.clone())
                .or_default()
                .push((block.id, idx, u.clone()));
        }
    }

    let mut witnesses = HashMap::new();
    for (binding, uses) in &sites {
        if let Some(w) = first_witness(binding, uses, cfg, dom) {
            witnesses.insert(binding.clone(), w);
        }
    }
    witnesses
}

/// Strict-precedence relation over `(block, intra-block-index)` pairs.
/// Site A precedes site B iff A and B are in the same block and A's
/// intra-block index is strictly less than B's, OR A's block strictly
/// dominates B's. The relation is transitive: if A precedes M and
/// M precedes B then A precedes B (same-block index ordering composes
/// trivially; cross-block dominance is transitive; mixed cases reduce
/// to `dom(bA, bB)` since the same-block side either tightens to
/// `bA == bB` or feeds into the dominance check). Round 12.19 uses
/// this for the reassign-kill check.
fn precedes(ab: BlockId, ai: usize, bb: BlockId, bi: usize, dom: &DominatorTree) -> bool {
    if ab == bb {
        ai < bi
    } else {
        dom.dominates(ab, bb)
    }
}

/// True iff some Reassign R among `uses` lies strictly between C and U
/// in the precedence order — i.e. C precedes R and R precedes U. Such
/// an R rebinds the binding before U executes on every path that runs
/// both C and U, so the consume at C does not reach U.
///
/// Soundness for the RC predicate: when (C, U) are dominance-incomparable
/// (the formal RC shape — `¬dom(C,U) ∧ ¬dom(U,C)`), no R can satisfy
/// both `precedes(C, R)` and `precedes(R, U)` because precedence is
/// transitive (it would imply `precedes(C, U)`, contradicting
/// incomparability). The kill is therefore vacuous for `first_witness`;
/// it bites for the UAM check, where (C, U) are dominance-comparable.
fn reassign_kills(
    uses: &[(BlockId, usize, UseSite)],
    cb: BlockId,
    ci: usize,
    ub: BlockId,
    ui: usize,
    dom: &DominatorTree,
) -> bool {
    uses.iter().any(|(rb, ri, r)| {
        r.kind == UseKind::Reassign
            && precedes(cb, ci, *rb, *ri, dom)
            && precedes(*rb, *ri, ub, ui, dom)
    })
}

/// True iff EVERY CFG path from the consume at `(cb, ci)` to the use at
/// `(ub, ui)` threads a Reassign of the binding that rebinds it after the
/// consume — so the consumed value is provably dead before U reads it.
///
/// This is the reachability-precise companion to [`reassign_kills`]. That
/// dominance-based check is (per its own doc) *vacuous* for the RC shape,
/// where (C, U) are dominance-incomparable: no reassign R can `precedes(C,
/// R) ∧ precedes(R, U)` because precedence is transitive. But a loop makes
/// (C, U) incomparable while still routing every real C→U path through an
/// in-loop reassign: the back-edge gives the loop header (hence the exit)
/// an *entry* predecessor, so the reassign block does not *dominate* the
/// exit even though control can only leave the loop after threading it
/// (`let mut buf; loop { let x = match buf {..}; buf = ..; }; buf`). Pure
/// dominance misses that and the formal predicate spuriously RC-boxes
/// `buf`; boxing a *reassigned* Option/enum local then miscompiles (the
/// value-typed store smashes the box-pointer slot — B-2026-07-10-4).
///
/// CFG reachability catches it: a block that reassigns the binding kills
/// the consumed value on every path threading it, so removing those
/// blocks and asking "is U still reachable from C?" is exactly "can the
/// consumed value reach U?". Reachable ⇒ a live path exists ⇒ NOT killed
/// (the genuine RC / UAM shapes, which carry no reassign, always stay
/// reachable and keep firing). `cb != ub` is guaranteed by the caller's
/// dominance-comparability filter (a block dominates itself).
fn reassign_kills_by_reachability(
    uses: &[(BlockId, usize, UseSite)],
    cfg: &Cfg,
    cb: BlockId,
    ci: usize,
    ub: BlockId,
    ui: usize,
) -> bool {
    // A `let x = …` re-declaration inside a loop re-initialises the binding
    // each iteration, so from a consume C's perspective the next iteration's
    // Define kills the consumed value just as a `x = …` Reassign would. Both
    // are rebinds; the three sibling predicates (loop-of-consume, trigger-2/3)
    // already treat `Reassign | Define` alike, so this reachability companion
    // must too — otherwise the natural "build a nested Vec row-by-row in a
    // loop" shape (`while … { let mut row = Vec.new(); …; outer.push(row) }`)
    // spuriously RC-boxes `row`, because the loop-body `let` isn't recognised
    // as the kill that disconnects the back-edge C→U path (B-2026-07-17-…).
    let is_rebind = |k: UseKind| matches!(k, UseKind::Reassign | UseKind::Define);
    // A rebind in C's own block, after C, kills the value on exit.
    let cb_kills = uses
        .iter()
        .any(|(rb, ri, r)| is_rebind(r.kind) && *rb == cb && *ri > ci);
    if cb_kills {
        return true;
    }
    // A rebind in U's own block, before U, kills the value before the read.
    let ub_kills = uses
        .iter()
        .any(|(rb, ri, r)| is_rebind(r.kind) && *rb == ub && *ri < ui);
    if ub_kills {
        return true;
    }
    // Any OTHER block that rebinds the binding kills every path threading it.
    let forbidden: HashSet<BlockId> = uses
        .iter()
        .filter(|(rb, _, r)| is_rebind(r.kind) && *rb != cb && *rb != ub)
        .map(|(rb, _, _)| *rb)
        .collect();
    // Nothing rebinds the value between C and U → the pure-dominance
    // predicate already had the final say; never kill here. (Guards the
    // ClosureCapture / ContainerStore RC shapes, whose C and U are not in
    // a forward C→U CFG relation at all — a reachability answer there is
    // meaningless, not a kill.)
    if forbidden.is_empty() {
        return false;
    }
    // The kill is only meaningful when reassigns are what disconnect an
    // otherwise-connected path: U must be forward-reachable from C in the
    // FULL CFG, yet unreachable once the reassigning blocks are removed.
    // If U is not forward-reachable from C even with every block present
    // (mutually-exclusive branches — the genuine two-arm RC shape), the
    // reassigns are irrelevant and we must not suppress.
    let reach = |blocked: &HashSet<BlockId>| -> bool {
        let mut seen: HashSet<BlockId> = HashSet::new();
        let mut stack: Vec<BlockId> = cfg.block(cb).successors.clone();
        while let Some(n) = stack.pop() {
            if n == ub {
                return true;
            }
            if blocked.contains(&n) || !seen.insert(n) {
                continue;
            }
            stack.extend(cfg.block(n).successors.iter().copied());
        }
        false
    };
    let empty = HashSet::new();
    // reachable-in-full ∧ ¬reachable-without-reassigns ⇒ every C→U path
    // threads a reassign ⇒ the consumed value is dead before U.
    reach(&empty) && !reach(&forbidden)
}

/// Find the first (C, U) pair for `binding` that satisfies the RC
/// predicate, scanning consume sites in source order and the partner U
/// in source order too. Returns `None` if no such pair exists.
fn first_witness(
    binding: &str,
    uses: &[(BlockId, usize, UseSite)],
    cfg: &Cfg,
    dom: &DominatorTree,
) -> Option<RcWitness> {
    for (i, (cb, ci, c)) in uses.iter().enumerate() {
        if c.kind != UseKind::Consume {
            continue;
        }
        for (j, (ub, ui, u)) in uses.iter().enumerate() {
            if i == j {
                continue;
            }
            // Reassign / Define markers are rebind signals — never the
            // U partner (a `let`/assign introduction is not a use).
            if matches!(u.kind, UseKind::Reassign | UseKind::Define) {
                continue;
            }
            // B-2026-07-02-25: partial moves of provably-disjoint sub-
            // places (`b.left` vs `b.right`, `t.0` vs `t.1`) don't touch
            // each other, so they are not an RC pair. Whole-value uses
            // carry the empty place, which overlaps everything.
            if place_paths_disjoint(&c.place, &u.place) {
                continue;
            }
            if dom.dominates(*cb, *ub) || dom.dominates(*ub, *cb) {
                continue;
            }
            if reassign_kills(uses, *cb, *ci, *ub, *ui, dom) {
                continue;
            }
            // Reachability-precise kill: suppress when every C→U path
            // threads a reassign (the loop-carried `buf` shape pure
            // dominance misses — B-2026-07-10-4).
            if reassign_kills_by_reachability(uses, cfg, *cb, *ci, *ub, *ui) {
                continue;
            }
            // B-2026-07-11-9: the terminal-`return` consume shape — e.g. the
            // loop-branch accumulator `while … { if … { return out } … } out`,
            // whose two mutually-exclusive returns of `out` (the early one and
            // the tail) pair as (C, U). The consume C sits in a block that
            // TERMINATES the function (its only successor is the synthetic CFG
            // exit — a `return <value>`), so there is no C → U path: control
            // leaves the function on that path, and U runs only when C did not.
            // With no reuse-after-consume, RC is spurious.
            //
            // Excluded — a use U inside a CLOSURE BODY (`closure_body_blocks`):
            // the body executes at the closure's unknown future invocation, so a
            // captured value consumed in the terminating outer scope IS a genuine
            // trigger-2 RC (the closure may run after the consume). Gated to
            // `Direct`; ClosureCapture / ContainerStore escapes keep firing.
            if c.consume_origin == ConsumeOrigin::Direct {
                let succ = &cfg.block(*cb).successors;
                let c_terminates = !succ.is_empty() && succ.iter().all(|s| *s == cfg.exit);
                if c_terminates && !cfg.closure_body_blocks.contains(ub) {
                    continue;
                }
            }
            return Some(RcWitness {
                binding: binding.to_string(),
                consume_span: c.span.clone(),
                other_use_span: u.span.clone(),
                consume_block: *cb,
                other_block: *ub,
                consume_origin: c.consume_origin,
            });
        }
    }
    None
}

// ── Whole-program driver ──────────────────────────────────────────
//
// Round 12.10: end-to-end pipeline runner. For every function in
// `program` (including impl methods), build the use classification,
// then the classification-aware CFG, then the dominator tree, then
// evaluate `rc_candidates`. Returns one `RcWitness` per (function,
// binding) pair that satisfies the formal RC predicate.
//
// Function keys mirror `OwnershipChecker::check_function`: free
// functions are keyed by bare name (`"my_fn"`); impl methods are
// keyed by `"Type.method"`. This shape is what the parity tests
// against `OwnershipCheckResult::rc_values` consume directly.
//
// Status: this driver does NOT yet replace the live RC routing in
// `src/ownership.rs`. Triggers 1 / 2 / 3 are now all detected
// structurally (rounds 12.10 / 12.11 / 12.12) — the legacy linear
// forward state machine remains authoritative for live diagnostics
// pending in-place integration. See `tests/rc_predicate_parity.rs`
// for the per-trigger parity matrix.

/// Run the predicate pipeline over `function_body` with the given
/// `param_types`, producing the `(Cfg, DominatorTree, witnesses)`
/// triple. Exposed for tests that want the intermediate artifacts.
///
/// The returned witness map merges three sources: the formal RC
/// predicate (`rc_candidates`), the loop-of-consume rule (round
/// 12.22 — `loop_of_consume_candidates`), and per-binding mutual
/// exclusivity with the direct-UAM predicate (`direct_uam_candidates`).
/// Formal RC wins on collisions; loop-of-consume only fires on
/// bindings the formal predicates leave silent. The merge ensures a
/// downstream consumer (`OwnershipChecker::populate_predicate_outputs`)
/// gets one canonical RC list per function, with UAM emitted
/// independently and never colliding with RC on the same binding.
pub fn run_predicate_for_function(
    program: &Program,
    tc: &TypeCheckResult,
    f: &Function,
) -> (Cfg, DominatorTree, HashMap<String, RcWitness>) {
    let prelude = ClassifierPrelude::new(program, tc);
    run_predicate_for_function_with(&prelude, tc, f)
}

/// Collect the binding names of every parameter of `f` (patterns
/// flattened). Seeds the CFG builder's shadow-detection visibility set
/// so a body-level `let` re-binding a parameter gets a fresh binding
/// identity (B-2026-07-02-32).
fn param_binding_names(f: &Function) -> Vec<String> {
    f.params
        .iter()
        .flat_map(|p| p.pattern.binding_names())
        .collect()
}

/// As [`run_predicate_for_function`] but against a pre-built
/// [`ClassifierPrelude`]. Whole-program drivers build the prelude once
/// and call this per function so the classifier's whole-program tables
/// are not recollected for every body.
pub fn run_predicate_for_function_with(
    prelude: &ClassifierPrelude,
    tc: &TypeCheckResult,
    f: &Function,
) -> (Cfg, DominatorTree, HashMap<String, RcWitness>) {
    let param_types = param_types_for_function(f, tc);
    let classification = classify_function_body_with(prelude, tc, &f.body, param_types);
    let param_names = param_binding_names(f);
    let cfg = build_cfg_with_classification(&f.body, &classification, &param_names);
    let dom = compute_dominators(&cfg);
    let mut witnesses = rc_candidates(&cfg, &dom);
    let uam_keys: HashSet<String> = direct_uam_candidates(&cfg, &dom).into_keys().collect();
    for (binding, w) in loop_of_consume_candidates(&cfg, &dom) {
        if witnesses.contains_key(&binding) || uam_keys.contains(&binding) {
            continue;
        }
        witnesses.insert(binding, w);
    }
    (cfg, dom, witnesses)
}

/// Whole-program driver: run the predicate pipeline for every
/// function and every impl method, returning `function_key →
/// binding_name → witness`. Functions with no qualifying binding are
/// absent from the outer map (matching the shape of
/// `OwnershipCheckResult::rc_values`).
pub fn predicate_rc_candidates_for_program(
    program: &Program,
    tc: &TypeCheckResult,
) -> HashMap<String, HashMap<String, RcWitness>> {
    let prelude = ClassifierPrelude::new(program, tc);
    let mut out: HashMap<String, HashMap<String, RcWitness>> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                let (_, _, witnesses) = run_predicate_for_function_with(&prelude, tc, f);
                if !witnesses.is_empty() {
                    out.insert(f.name.clone(), witnesses);
                }
            }
            Item::ImplBlock(impl_block) => {
                let Some(target_name) = impl_target_head(impl_block) else {
                    continue;
                };
                for impl_item in &impl_block.items {
                    if let ImplItem::Method(method) = impl_item {
                        let (_, _, witnesses) =
                            run_predicate_for_function_with(&prelude, tc, method);
                        if !witnesses.is_empty() {
                            out.insert(format!("{target_name}.{}", method.name), witnesses);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn impl_target_head(impl_block: &ImplBlock) -> Option<String> {
    if let crate::ast::TypeKind::Path(path) = &impl_block.target_type.kind {
        path.segments.last().cloned()
    } else {
        None
    }
}

// ── Direct use-after-move predicate (round 12.15) ─────────────────
//
// Companion to `rc_candidates`. Detects the *error* case the formal
// RC predicate explicitly excludes: a Consume site C that
// strictly precedes another use U of the same binding on every path
// — the use is unreachable without first consuming. The formal
// predicate filters this shape out (dom(C, U) holds), so the linear
// forward state machine has been authoritative for it. With this
// predicate the in-place ownership.rs integration round can route
// both RC fallback recording and use-after-move detection through
// the same (CFG, dominator) artifact.
//
// Predicate (per binding): ∃C, U with U ≠ C, kind(C) = Consume, and
// either
//   • block(C) == block(U) and C precedes U in source order within
//     that block's `uses` vector, OR
//   • block(C) ≠ block(U) and dom(block(C), block(U)).
//
// The dom(U, C) shape is *not* a UAM error (read-then-consume is the
// fine sequential case); the formal RC predicate excludes both
// dominance directions, so the residue is exactly direct-error UAM.

/// Witness pair for a binding that satisfies the direct UAM
/// predicate. Same shape as `RcWitness` minus the flavor tag — UAM
/// errors are always direct (no closure / container layering).
#[derive(Debug, Clone)]
pub struct UamWitness {
    pub binding: String,
    pub consume_span: Span,
    pub other_use_span: Span,
    pub consume_block: BlockId,
    pub other_block: BlockId,
}

/// Evaluate the direct UAM predicate over every binding in the CFG.
/// Returns one witness per binding satisfying the predicate (the
/// first such pair encountered in source order). Bindings with no
/// Consume sites — or whose Consumes are all dominance-incomparable
/// (RC fallback shape) or read-then-consumed (sequentially fine) —
/// are absent from the map.
pub fn direct_uam_candidates(cfg: &Cfg, dom: &DominatorTree) -> HashMap<String, UamWitness> {
    let mut sites: HashMap<String, Vec<(BlockId, usize, UseSite)>> = HashMap::new();
    for block in &cfg.blocks {
        for (idx, u) in block.uses.iter().enumerate() {
            sites
                .entry(u.binding.clone())
                .or_default()
                .push((block.id, idx, u.clone()));
        }
    }

    let mut witnesses = HashMap::new();
    for (binding, uses) in &sites {
        if let Some(w) = first_uam_witness(binding, uses, dom) {
            witnesses.insert(binding.clone(), w);
        }
    }
    witnesses
}

/// First (C, U) pair where C is a Consume that strictly precedes U
/// (same-block source order or cross-block dominance) AND no
/// reassignment of the binding rebinds it between C and U.
fn first_uam_witness(
    binding: &str,
    uses: &[(BlockId, usize, UseSite)],
    dom: &DominatorTree,
) -> Option<UamWitness> {
    for (i, (cb, ci, c)) in uses.iter().enumerate() {
        if c.kind != UseKind::Consume {
            continue;
        }
        for (j, (ub, ui, u)) in uses.iter().enumerate() {
            if i == j {
                continue;
            }
            // Reassign / Define markers are rebind signals — never the U
            // partner (a `let`/assign introduction is not a use, so it
            // must not be reported as a "used again" UAM site).
            if matches!(u.kind, UseKind::Reassign | UseKind::Define) {
                continue;
            }
            // B-2026-07-02-25: provably-disjoint sub-place partial moves
            // (`b.left` then `b.right`) are not a use-after-move — they
            // touch different sub-places. A whole-value use (empty place)
            // after a partial move still pairs (empty overlaps everything).
            if place_paths_disjoint(&c.place, &u.place) {
                continue;
            }
            if !precedes(*cb, *ci, *ub, *ui, dom) {
                continue;
            }
            if reassign_kills(uses, *cb, *ci, *ub, *ui, dom) {
                continue;
            }
            return Some(UamWitness {
                binding: binding.to_string(),
                consume_span: c.span.clone(),
                other_use_span: u.span.clone(),
                consume_block: *cb,
                other_block: *ub,
            });
        }
    }
    None
}

// ── Loop-of-consume rule (round 12.22) ────────────────────────────
//
// The formal RC predicate fires only when two distinct use sites C
// and U are dominance-incomparable. A *single* Consume of a binding
// inside a loop body has no second use site in source order — the
// formal predicate cannot fire. But the back-edge re-enters the
// same Consume on the next iteration, and a value moved on
// iteration N is moved when iteration N+1 reaches the same site.
// Trigger-1-style detection for this pattern needs a separate rule
// that pairs the in-loop Consume with the implicit "next iteration
// of the same site" partner.
//
// Predicate: ∃C such that
//   • kind(C) = Consume, AND
//   • ∃ natural loop L with block(C) ∈ L AND no Reassign of C's
//     binding sits in L's blocks — i.e. the inner back-edge re-
//     enters C's site without the binding having been rebound.
//
// The reassign-suppression handles `let mut x; while c { let _ = x;
// x = next(); }` (the common consuming-iterator shape — clean code
// that rebinds before the back-edge fires) without flagging it.
// Per-loop precision (matched to the consume's containing natural
// loop, not the union of every loop in the function) ensures a
// rebind in a sibling or non-containing loop does not spuriously
// suppress the rule: sibling loops do not close the consume's
// back-edge, so the loop-of-consume condition still holds there.
//
// The witness has `consume_span == other_use_span` (both point at
// the in-loop Consume site). `consume_origin` carries the
// classifier's flavor tag, so
// `OwnershipChecker::populate_predicate_outputs` produces a
// flavor-correct `RcEntry`.

/// Compute each natural loop's body block set. A back edge is
/// `(b → v)` where `v` dominates `b`; the natural loop with header
/// `v` and back-edge source `b` is `{v}` plus every block that
/// reaches `b` along predecessor edges without crossing `v`. One
/// entry per back-edge; nested loops appear as separate entries
/// (the inner loop's set is a subset of its enclosing loop's set).
fn natural_loops(cfg: &Cfg, dom: &DominatorTree) -> Vec<HashSet<BlockId>> {
    let mut loops: Vec<HashSet<BlockId>> = Vec::new();
    for b in 0..cfg.num_blocks() {
        for &v in &cfg.block(b).successors {
            if !dom.dominates(v, b) {
                continue;
            }
            // Back edge `b → v`. Walk predecessors backwards from
            // `b`, accumulating the natural loop's body blocks.
            // Never traverse past `v`: when popping `v` itself
            // (only possible when `b == v`, the self-loop case),
            // do not walk its predecessors — they sit outside the
            // loop and would be wrongly absorbed.
            let mut visited: HashSet<BlockId> = HashSet::new();
            visited.insert(v);
            visited.insert(b);
            let mut stack = vec![b];
            while let Some(n) = stack.pop() {
                if n == v {
                    continue;
                }
                for p in cfg.predecessors(n) {
                    if visited.insert(p) {
                        stack.push(p);
                    }
                }
            }
            loops.push(visited);
        }
    }
    loops
}

/// Find each binding's first in-loop Consume site that fires the
/// loop-of-consume rule. Returns one witness per binding with
/// `consume_span == other_use_span` (both point at the in-loop
/// Consume). A Consume qualifies iff at least one natural loop
/// contains it AND that loop has no Reassign of the same binding
/// — sibling-loop and non-containing-loop rebinds do not suppress.
pub fn loop_of_consume_candidates(cfg: &Cfg, dom: &DominatorTree) -> HashMap<String, RcWitness> {
    let loops = natural_loops(cfg, dom);
    if loops.is_empty() {
        return HashMap::new();
    }

    let mut sites: HashMap<String, Vec<(BlockId, usize, UseSite)>> = HashMap::new();
    for block in &cfg.blocks {
        for (idx, u) in block.uses.iter().enumerate() {
            sites
                .entry(u.binding.clone())
                .or_default()
                .push((block.id, idx, u.clone()));
        }
    }

    let mut witnesses = HashMap::new();
    for (binding, uses) in &sites {
        for (cb, _ci, c) in uses.iter() {
            if c.kind != UseKind::Consume {
                continue;
            }
            // Per-loop precision: the Consume fires loop-of-consume
            // iff at least one natural loop contains its block AND
            // that same loop has no Reassign of the binding. A
            // rebind in a sibling loop (or in an enclosing loop
            // outside the inner body) does not close the inner
            // back-edge — it leaves the Consume re-entering its
            // already-moved value on the next iteration.
            let fires = loops.iter().any(|nloop| {
                if !nloop.contains(cb) {
                    return false;
                }
                // A rebind of the binding INSIDE the same natural loop
                // suppresses the rule: either a `name = …` reassignment
                // (`Reassign`) or a loop-local `let name = …` binding
                // (`Define`, B-2026-06-12-6 cluster 2). Both give the
                // consume a fresh value each iteration, so it is not a
                // next-iteration use-after-move. A `Define` OUTSIDE the
                // loop (`let v = make(); loop { consume(v) }`) is not in
                // `nloop`, so the rule still fires there — the genuine RC
                // case is preserved.
                let has_rebind = uses.iter().any(|(rb, _, u)| {
                    matches!(u.kind, UseKind::Reassign | UseKind::Define) && nloop.contains(rb)
                });
                !has_rebind
            });
            if !fires {
                continue;
            }
            witnesses.insert(
                binding.clone(),
                RcWitness {
                    binding: binding.clone(),
                    consume_span: c.span.clone(),
                    other_use_span: c.span.clone(),
                    consume_block: *cb,
                    other_block: *cb,
                    consume_origin: c.consume_origin,
                },
            );
            break;
        }
    }
    witnesses
}

/// Whole-program direct-UAM driver. Mirrors
/// `predicate_rc_candidates_for_program` shape for consistency with
/// the parity-test scaffolding.
pub fn predicate_uam_candidates_for_program(
    program: &Program,
    tc: &TypeCheckResult,
) -> HashMap<String, HashMap<String, UamWitness>> {
    let prelude = ClassifierPrelude::new(program, tc);
    let mut out: HashMap<String, HashMap<String, UamWitness>> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                let param_types = param_types_for_function(f, tc);
                let classification =
                    classify_function_body_with(&prelude, tc, &f.body, param_types);
                let param_names = param_binding_names(f);
                let cfg = build_cfg_with_classification(&f.body, &classification, &param_names);
                let dom = compute_dominators(&cfg);
                let witnesses = direct_uam_candidates(&cfg, &dom);
                if !witnesses.is_empty() {
                    out.insert(f.name.clone(), witnesses);
                }
            }
            Item::ImplBlock(impl_block) => {
                let Some(target_name) = impl_target_head(impl_block) else {
                    continue;
                };
                for impl_item in &impl_block.items {
                    if let ImplItem::Method(method) = impl_item {
                        let param_types = param_types_for_function(method, tc);
                        let classification =
                            classify_function_body_with(&prelude, tc, &method.body, param_types);
                        let param_names = param_binding_names(method);
                        let cfg = build_cfg_with_classification(
                            &method.body,
                            &classification,
                            &param_names,
                        );
                        let dom = compute_dominators(&cfg);
                        let witnesses = direct_uam_candidates(&cfg, &dom);
                        if !witnesses.is_empty() {
                            out.insert(format!("{target_name}.{}", method.name), witnesses);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::{build_cfg, Cfg, UseKind};
    use crate::dominator::compute_dominators;
    use crate::{parse, resolve};

    /// Build a CFG from the body of `fn main` in `src`.
    fn cfg_of(src: &str) -> Cfg {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = resolve(&parsed.program);
        assert!(
            resolved.errors.is_empty(),
            "resolve errors: {:?}",
            resolved.errors
        );
        let main = parsed
            .program
            .items
            .iter()
            .find_map(|i| {
                if let crate::ast::Item::Function(f) = i {
                    if f.name == "main" {
                        return Some(f);
                    }
                }
                None
            })
            .expect("no fn main in source");
        build_cfg(&main.body)
    }

    /// Mark every use of `binding` whose source line matches `line` as
    /// `UseKind::Consume`. The CFG builder emits everything as Read by
    /// default (the classifier that uses TypeCheckResult is a separate
    /// pass, staged for a later round); these tests inject Consume
    /// classifications by hand to exercise the predicate alone.
    fn mark_consume_at_line(cfg: &mut Cfg, binding: &str, line: usize) {
        let mut hit = false;
        for block in &mut cfg.blocks {
            for u in &mut block.uses {
                if u.binding == binding && u.span.line == line {
                    u.kind = UseKind::Consume;
                    hit = true;
                }
            }
        }
        assert!(
            hit,
            "no use of {binding:?} found at line {line} — test setup bug"
        );
    }

    // ── Trigger-1 shape: consume in one branch, read in another ────

    #[test]
    fn two_branch_consume_then_outer_use_satisfies_predicate() {
        // Line 3: `if c { consume(x); }`  — consume.
        // Line 4: `let _ = x;`            — outer read after merge.
        // The two arms are siblings, neither dominating the other; the
        // outer read is in the merge block, dominated by entry but not
        // by the consume's branch-arm. Predicate fires.
        let src = "fn main() {\n\
                       let c = true; let x = 5;\n\
                       if c { let _a = x; }\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        // The `let _a = x;` on line 3 is the consume in this synthesis.
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "expected 'x' to satisfy the RC predicate; got {:?}",
            cands.keys().collect::<Vec<_>>()
        );
        let w = &cands["x"];
        assert_eq!(w.consume_span.line, 3);
        assert_eq!(w.other_use_span.line, 4);
    }

    #[test]
    fn match_arm_consume_with_outer_read_satisfies_predicate() {
        let src = "fn main() {\n\
                       let n = 1; let x = 2;\n\
                       match n { 0 => { let _a = x; }, _ => {} }\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(cands.contains_key("x"));
    }

    // ── Linear shapes: predicate must NOT fire ─────────────────────

    #[test]
    fn linear_sequential_consume_does_not_satisfy_predicate() {
        // `let x = ...; consume(x); use(x);` — both uses on the same
        // straight-line path; the consume's block dominates the read's
        // block (or they share a block). Predicate must not fire.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       let _a = x;\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "linear sequential consume must not produce a witness; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn reads_only_no_consume_no_witness() {
        // No Consume site at all → predicate trivially false.
        let src = "fn main() {\n\
                       let c = true; let x = 5;\n\
                       if c { let _a = x; } else { let _b = x; }\n\
                       let _c = x;\n\
                   }";
        let cfg = cfg_of(src);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(
            cands.is_empty(),
            "no Consume sites → no witnesses; got {cands:?}"
        );
    }

    #[test]
    fn single_use_only_no_witness() {
        // One Consume site, no other use → no partner U exists.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       let _a = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(!cands.contains_key("x"));
    }

    #[test]
    fn two_consumes_in_same_block_no_witness() {
        // Two Consumes in the same straight-line block: the block
        // dominates itself, so dom(C, U) holds for the trailing one.
        // Predicate fails (real-life: this is a use-after-move error,
        // handled by a separate rule).
        let src = "fn main() {\n\
                       let x = 5;\n\
                       let _a = x;\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        mark_consume_at_line(&mut cfg, "x", 4);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(!cands.contains_key("x"));
    }

    // ── Multi-binding mix ──────────────────────────────────────────

    #[test]
    fn multi_binding_only_rc_ones_appear() {
        // `x` satisfies the predicate (consume in arm + outer read);
        // `y` is read-only on both branches → no witness.
        let src = "fn main() {\n\
                       let c = true; let x = 1; let y = 2;\n\
                       if c { let _a = x; let _ay = y; }\n\
                       else { let _by = y; }\n\
                       let _bx = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(cands.contains_key("x"), "x should be RC");
        assert!(!cands.contains_key("y"), "y is read-only — no RC");
    }

    // ── Loop body single-Consume — predicate alone does not fire ──

    #[test]
    fn loop_body_single_consume_no_witness_from_predicate() {
        // `while cond { consume(x); }` — the body is a single block
        // with one Consume of `x`. There's no second use site U for
        // the formal predicate to pair against. The existing pass
        // catches this via a separate "consume in loop body of an
        // outer-scope binding" rule, not via the dom-tree predicate.
        let src = "fn main() {\n\
                       let mut i = 0; let x = 5;\n\
                       while i < 3 { let _a = x; i = i + 1; }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "single Consume in loop body: predicate alone produces no witness (separate rule handles it)"
        );
    }

    #[test]
    fn loop_body_consume_with_pre_loop_read_satisfies_predicate() {
        // `let _ = x; while cond { consume(x); }` — pre-loop read in
        // entry, Consume in loop body. The body is reached only when
        // the loop runs ≥ once; entry is always reached. Entry
        // dominates body (loop header reachable only through entry),
        // but body does NOT dominate entry. dom(C,U)? body→entry: no.
        // dom(U,C)? entry→body: yes. So predicate fails for this pair.
        // This shape is correct: the read happened before the consume,
        // sequentially, so it isn't RC fallback.
        let src = "fn main() {\n\
                       let mut i = 0; let x = 5;\n\
                       let _pre = x;\n\
                       while i < 3 { let _a = x; i = i + 1; }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 4);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "pre-loop read + in-loop consume is sequential consume — predicate must not fire"
        );
    }

    #[test]
    fn loop_body_consume_with_post_loop_read_satisfies_predicate() {
        // `while cond { consume(x); }; let _ = x;` — the consume runs
        // only when the loop executes; the post-loop read always runs.
        // Neither dominates the other (you can reach post-loop without
        // executing the body). Predicate fires.
        let src = "fn main() {\n\
                       let mut i = 0; let x = 5;\n\
                       while i < 3 { let _a = x; i = i + 1; }\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "post-loop read after in-loop consume should fire predicate; got {:?}",
            cands.keys().collect::<Vec<_>>()
        );
    }

    // ── Witness selection: first consume × first partner ──────────

    #[test]
    fn witness_picks_first_consume_in_source_order() {
        // Two consumes both qualify; the first one (lower block id /
        // earlier source position) should be picked.
        let src = "fn main() {\n\
                       let c = true; let x = 5;\n\
                       if c { let _a = x; } else { let _b = x; }\n\
                       let _c = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        // Mark both arm-uses as Consume.
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        let w = cands.get("x").expect("expected witness for x");
        // Both Consumes are on line 3; the partner U is line 4 (post-merge read).
        assert_eq!(w.consume_span.line, 3);
        assert_eq!(w.other_use_span.line, 4);
    }

    // ── Round 12.14: witness consume_origin propagation ────────────

    #[test]
    fn synthesized_consume_witness_has_direct_origin() {
        // The synthetic-CFG tests in this module mark sites as Consume
        // by hand on a CFG built without classification — every UseSite
        // therefore carries `ConsumeOrigin::Direct`. Witnesses produced
        // by `rc_candidates` must surface that origin verbatim.
        let src = "fn main() {\n\
                       let c = true; let x = 5;\n\
                       if c { let _a = x; }\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let w = rc_candidates(&cfg, &dom)
            .remove("x")
            .expect("expected witness for x");
        assert_eq!(w.consume_origin, ConsumeOrigin::Direct);
    }

    // ── Round 12.15: direct use-after-move predicate ───────────────

    #[test]
    fn uam_same_block_consume_then_use_fires() {
        // `let x = 5; consume(x); use(x);` — both uses in the entry
        // block, in source order. Direct UAM.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       let _a = x;\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        let w = cands.get("x").expect("expected UAM witness for x");
        assert_eq!(w.consume_span.line, 3);
        assert_eq!(w.other_use_span.line, 4);
    }

    #[test]
    fn uam_two_consumes_same_block_first_dominates_second() {
        // Two Consumes in the same block; the first strictly precedes
        // the second in source order — the FIRST is the consume in
        // the witness, the SECOND is the partner use.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       let _a = x;\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        mark_consume_at_line(&mut cfg, "x", 4);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        let w = cands.get("x").expect("expected UAM witness for x");
        assert_eq!(w.consume_span.line, 3);
        assert_eq!(w.other_use_span.line, 4);
    }

    #[test]
    fn uam_cross_block_strict_dominance_fires() {
        // Pre-loop consume + in-loop use: entry block dominates loop
        // body, so the consume on line 3 strictly dominates the use
        // on line 4 — Direct UAM.
        let src = "fn main() {\n\
                       let mut i = 0; let x = 5;\n\
                       let _pre = x;\n\
                       while i < 3 { let _a = x; i = i + 1; }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "pre-loop consume + in-loop use must fire UAM; got {:?}",
            cands.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn uam_branch_divergent_consume_does_not_fire() {
        // Trigger-1 RC fallback shape: consume in one arm, read after
        // merge. Neither dominates the other → not Direct UAM (this
        // is the RC predicate's domain).
        let src = "fn main() {\n\
                       let c = true; let x = 5;\n\
                       if c { let _a = x; }\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "branch-divergent shape is RC fallback, not UAM; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn uam_read_then_consume_does_not_fire() {
        // `let _ = x; consume(x);` — read precedes consume; the
        // consume is the LAST use, no partner U after it. UAM
        // predicate must not fire.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       let _a = x;\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 4);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "read-then-consume is sequentially fine — no UAM; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn uam_no_consume_no_witness() {
        // Reads only — UAM trivially false.
        let src = "fn main() {\n\
                       let c = true; let x = 5;\n\
                       if c { let _a = x; } else { let _b = x; }\n\
                       let _c = x;\n\
                   }";
        let cfg = cfg_of(src);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        assert!(cands.is_empty());
    }

    #[test]
    fn uam_single_consume_no_other_use_no_witness() {
        // One Consume, nothing else — no partner U.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       let _a = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        assert!(!cands.contains_key("x"));
    }

    #[test]
    fn uam_post_loop_use_after_in_loop_consume_does_not_fire() {
        // `while c { consume(x); }; let _ = x;` — in-loop consume
        // does NOT dominate the post-loop use (the loop body may
        // execute zero times). RC predicate handles this shape; UAM
        // predicate must not fire.
        let src = "fn main() {\n\
                       let mut i = 0; let x = 5;\n\
                       while i < 3 { let _a = x; i = i + 1; }\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "post-loop use after in-loop consume is RC fallback, not UAM"
        );
    }

    // ── Round 12.19: reassignment-resets-state kill behavior ────────

    /// Mark every use of `binding` whose source line matches `line` as
    /// `UseKind::Reassign`. Pairs with `mark_consume_at_line` so kill
    /// scenarios can be exercised on a synthesized CFG without going
    /// through the real classifier.
    fn mark_reassign_at_line(cfg: &mut Cfg, binding: &str, line: usize) {
        let mut hit = false;
        for block in &mut cfg.blocks {
            for u in &mut block.uses {
                if u.binding == binding && u.span.line == line {
                    u.kind = UseKind::Reassign;
                    hit = true;
                }
            }
        }
        assert!(
            hit,
            "no use of {binding:?} found at line {line} — test setup bug"
        );
    }

    #[test]
    fn uam_same_block_reassign_between_kills_consume() {
        // The documented `test_reassignment_resets_state` shape:
        //   let mut x; consume(x); x = ...; consume(x);
        // C at line 3, R at line 4, U at line 5 — all in the same
        // straight-line block. The Reassign R sits between C and U, so
        // C does not reach U; UAM predicate must not fire.
        let src = "fn main() {\n\
                       let mut x = 1;\n\
                       let _a = x;\n\
                       x = 2;\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        mark_reassign_at_line(&mut cfg, "x", 4);
        mark_consume_at_line(&mut cfg, "x", 5);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "same-block reassign between consumes must kill UAM; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn uam_same_block_no_reassign_still_fires() {
        // Regression sentinel: identical shape minus the reassign — UAM
        // must fire so we know the kill check is doing real work and
        // not silently disabling everything.
        let src = "fn main() {\n\
                       let x = 1;\n\
                       let _a = x;\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        mark_consume_at_line(&mut cfg, "x", 4);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "two consumes back-to-back with no reassign IS UAM"
        );
    }

    #[test]
    fn uam_reassign_in_else_branch_does_not_kill() {
        // `if c { x = ...; } use(x);` after an earlier consume — the
        // reassign is on the then-branch only; on the else path, x is
        // still moved when used. Reassign does NOT dominate the use.
        // Kill check must NOT trigger here.
        let src = "fn main() {\n\
                       let mut c = true;\n\
                       let mut x = 1;\n\
                       let _a = x;\n\
                       if c { x = 2; }\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 4);
        mark_reassign_at_line(&mut cfg, "x", 5);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        // `x` on line 6 is dominance-comparable with line 4 (post-if
        // dominates… actually line-4 dominates line-6 directly). Kill
        // check evaluates `dom(bC, bR) ∧ dom(bR, bU)` — bR is the
        // then-block and dom(bR=then-block, bU=after-if) is FALSE
        // because the else branch reaches bU without bR. So kill does
        // NOT apply, UAM fires.
        assert!(
            cands.contains_key("x"),
            "branch-only reassign must not kill the cross-branch UAM; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn uam_cross_block_reassign_dominates_kills() {
        // Linear control flow with a reassign in a dominated position.
        // Round 12.19's CFG lowering keeps `consume(x); x = ...;
        // consume(x);` in a single block, so the kill is same-block —
        // synthesize a multi-block shape via `if c { /*nop*/ };` to
        // exercise the cross-block leg of the precedence relation.
        // After the `if`, the after-if block dominates the trailing use,
        // and the reassign placed there dominates the use. With no
        // branching strictly between C and U beyond the no-op `if`,
        // the reassign on the after-if join line kills C.
        let src = "fn main() {\n\
                       let mut c = true;\n\
                       let mut x = 1;\n\
                       let _a = x;\n\
                       if c { let _z = 0; }\n\
                       x = 2;\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 4);
        mark_reassign_at_line(&mut cfg, "x", 6);
        mark_consume_at_line(&mut cfg, "x", 7);
        let dom = compute_dominators(&cfg);
        let cands = direct_uam_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "cross-block reassign that dominates the second consume kills UAM; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn same_branch_reassign_after_consume_kills_rc_witness() {
        // Branch-divergent shape whose then-branch reassigns `x` AFTER
        // consuming it: `if c { let _a = x; x = 2; } let _b = x;`. The
        // pure-dominance `reassign_kills` is vacuous here (C in the then-
        // branch and U after the merge are dominance-incomparable, so no
        // R can `precedes(C, R) ∧ precedes(R, U)`), which used to leave a
        // spurious RC witness. But on EVERY execution `_b` reads a
        // reassigned-or-untouched `x`, never the consumed value: on the
        // taken path the `x = 2` between C and U rebinds it, on the
        // skipped path C never ran. The reachability-precise kill
        // (`reassign_kills_by_reachability`) removes the reassigning block
        // and finds U no longer reachable from C, so no RC is required —
        // the consumed value is dead before U. (B-2026-07-10-4.)
        let src = "fn main() {\n\
                       let mut c = true;\n\
                       let mut x = 1;\n\
                       if c {\n\
                           let _a = x;\n\
                           x = 2;\n\
                       }\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 5);
        mark_reassign_at_line(&mut cfg, "x", 6);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "a reassign after the consume that every C→U path threads must \
             kill the RC witness; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn branch_divergent_consume_without_reassign_still_fires_rc() {
        // The companion to the kill test: the SAME branch-divergent shape
        // WITHOUT the post-consume reassign keeps its RC witness. With no
        // reassigning block to remove, U stays reachable from C, so the
        // reachability kill never bites and the genuine RC condition (the
        // consumed value can reach the outer use) still holds.
        let src = "fn main() {\n\
                       let mut c = true;\n\
                       let x = 1;\n\
                       if c {\n\
                           let _a = x;\n\
                       }\n\
                       let _b = x;\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 5);
        let dom = compute_dominators(&cfg);
        let cands = rc_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "branch-divergent consume with no intervening reassign stays an \
             RC witness; got {:?}",
            cands.keys().collect::<Vec<_>>()
        );
    }

    // ── Round 12.22: loop-of-consume rule ────────────────────────────

    #[test]
    fn loop_of_consume_fires_for_in_loop_consume() {
        // Single Consume of an outer-scope binding inside a loop body.
        // The formal RC predicate cannot fire (only one use site in
        // source), but the back-edge re-enters the same site on every
        // iteration. Loop-of-consume rule supplies the witness.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       let mut i = 0;\n\
                       while i < 3 { let _a = x; i = i + 1; }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 4);
        let dom = compute_dominators(&cfg);
        let cands = loop_of_consume_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "in-loop consume of outer-scope binding must fire loop-of-consume; got {:?}",
            cands.keys().collect::<Vec<_>>()
        );
        let w = &cands["x"];
        // Witness fields point at the same in-loop consume site —
        // there is no second source-level use to pair against.
        assert_eq!(w.consume_span.line, 4);
        assert_eq!(w.other_use_span.line, 4);
        assert_eq!(w.consume_block, w.other_block);
    }

    #[test]
    fn loop_of_consume_does_not_fire_outside_loops() {
        // Sanity: a Consume outside any loop produces no
        // loop-of-consume witness, even when other in-loop activity
        // exists in the function. Pins that the rule's gating on
        // `block(C) ∈ loop_blocks` is doing real work.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       let _a = x;\n\
                       let mut i = 0;\n\
                       while i < 3 { i = i + 1; }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = loop_of_consume_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "consume outside loop must not fire loop-of-consume; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn loop_of_consume_suppressed_for_match_arm_binding_in_loop() {
        // B-2026-07-22-13: a match-arm payload binding consumed once inside a
        // loop is freshly re-bound by the pattern each iteration, so it must
        // NOT fire loop-of-consume. The CFG builder now records a `Define`
        // for the arm binding (like `let`), which the rule's rebind check
        // reads. The arm binding is alpha-renamed (`g@armN`), so locate it
        // dynamically and mark its consume.
        let src = "fn maybe(i: i64) -> Option[String] {\n\
                       if i > 0 { Some(\"x\".to_string()) } else { None }\n\
                   }\n\
                   fn take(s: String) -> i64 { s.len() as i64 }\n\
                   fn main() {\n\
                       let mut i = 0;\n\
                       while i < 3 {\n\
                           match maybe(i) { Some(g) => { take(g); } None => {} }\n\
                           i = i + 1;\n\
                       }\n\
                   }";
        let mut cfg = cfg_of(src);
        // Find the renamed arm binding (starts with `g`) and mark its
        // non-Define use (the `take(g)` consume) as Consume.
        let renamed: String = cfg
            .blocks
            .iter()
            .flat_map(|b| b.uses.iter())
            .find(|u| u.binding.starts_with('g') && u.kind != UseKind::Define)
            .map(|u| u.binding.clone())
            .expect("arm binding `g` use not found — test setup bug");
        for block in &mut cfg.blocks {
            for u in &mut block.uses {
                if u.binding == renamed && u.kind != UseKind::Define {
                    u.kind = UseKind::Consume;
                }
            }
        }
        let dom = compute_dominators(&cfg);
        let cands = loop_of_consume_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key(&renamed),
            "a match-arm binding freshly bound each iteration must not fire \
             loop-of-consume; got {:?}",
            cands.get(&renamed)
        );
    }

    #[test]
    fn loop_of_consume_suppressed_by_in_loop_reassign() {
        // `while c { consume(x); x = next(); }` — the rebind closes
        // the loop-of-consume gap. v1 coarse rule suppresses on any
        // in-loop reassign, regardless of position relative to the
        // consume; pins the suppression catches the common rebind
        // shape.
        let src = "fn main() {\n\
                       let mut x = 5;\n\
                       let mut i = 0;\n\
                       while i < 3 { let _a = x; x = i; i = i + 1; }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 4);
        mark_reassign_at_line(&mut cfg, "x", 4);
        let dom = compute_dominators(&cfg);
        let cands = loop_of_consume_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "in-loop reassign must suppress loop-of-consume; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn loop_of_consume_fires_for_sibling_loop_reassign() {
        // Per-loop precision: consume in loop L1, reassign in
        // sibling loop L2 (sequential, not nested) — the reassign
        // does NOT close L1's back-edge, so the rule must fire.
        // Under the prior coarse rule, any in-loop reassign of the
        // binding suppressed the rule for the whole function.
        let src = "fn main() {\n\
                       let mut x = 5;\n\
                       let mut i = 0;\n\
                       while i < 3 { let _a = x; i = i + 1; }\n\
                       while i < 6 { x = i; i = i + 1; }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 4);
        mark_reassign_at_line(&mut cfg, "x", 5);
        let dom = compute_dominators(&cfg);
        let cands = loop_of_consume_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "sibling-loop reassign must not suppress consume in a different loop; got {:?}",
            cands.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn loop_of_consume_fires_when_reassign_is_outside_inner_loop() {
        // Per-loop precision: nested loops where the consume sits
        // in the inner body and the reassign sits in the outer
        // body before the inner header. The inner loop's natural
        // blocks do not contain the reassign, so the inner back-
        // edge re-enters the consume on an already-moved value —
        // rule must fire. (The outer loop's natural blocks do
        // contain the rebind, so the outer scope alone would
        // suppress; the inner-scope firing is what the precision
        // improvement surfaces.)
        let src = "fn main() {\n\
                       let mut x = 5;\n\
                       let mut i = 0;\n\
                       while i < 3 {\n\
                           x = i;\n\
                           while i < 5 { let _a = x; i = i + 1; }\n\
                       }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 6);
        mark_reassign_at_line(&mut cfg, "x", 5);
        let dom = compute_dominators(&cfg);
        let cands = loop_of_consume_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "inner-loop consume must fire when only the enclosing-loop body rebinds; got {:?}",
            cands.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn loop_of_consume_fires_for_for_loop_body() {
        // `for` loops lower with the same back-edge shape as `while`
        // — make sure the rule fires on for-body consumes too.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       for _i in 0..3 { let _a = x; }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = loop_of_consume_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "for-loop body consume must fire loop-of-consume; got {:?}",
            cands.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn loop_of_consume_fires_for_loop_keyword_body() {
        // `loop { ... if c { break; } }` — bare `loop` form with a
        // conditional break preserves a reachable back-edge from the
        // else-path of the break-`if` to the header. The consume on
        // line 3 sits in a block that lies on that back-edge cycle,
        // so the rule fires.
        let src = "fn main() {\n\
                       let mut c = true; let x = 5;\n\
                       loop { let _a = x; if c { break; } }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = loop_of_consume_candidates(&cfg, &dom);
        assert!(
            cands.contains_key("x"),
            "loop body consume with reachable back-edge must fire loop-of-consume; got {:?}",
            cands.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn loop_of_consume_does_not_fire_for_one_shot_loop_with_unconditional_break() {
        // `loop { let _a = x; break; }` — the body runs exactly once;
        // the back-edge from the post-break sink to the header is
        // unreachable from entry. No "next iteration's same use"
        // semantically exists, so the rule must not fire.
        let src = "fn main() {\n\
                       let x = 5;\n\
                       loop { let _a = x; break; }\n\
                   }";
        let mut cfg = cfg_of(src);
        mark_consume_at_line(&mut cfg, "x", 3);
        let dom = compute_dominators(&cfg);
        let cands = loop_of_consume_candidates(&cfg, &dom);
        assert!(
            !cands.contains_key("x"),
            "one-shot loop (unconditional break) has no live back-edge — must not fire; got {:?}",
            cands.get("x")
        );
    }

    #[test]
    fn run_predicate_merges_loop_of_consume_with_formal_rc() {
        // End-to-end: `run_predicate_for_function` returns merged
        // witnesses. Verify via the program-level driver that a
        // function combining a formal-RC binding and a
        // loop-of-consume binding lists both. (Uses real
        // classifier — picks non-Copy types so consume sites are
        // emitted naturally.)
        let src = "struct Data { value: i64 }\n\
                   fn consume(d: Data) { }\n\
                   fn main() {\n\
                       let d = Data { value: 1 };\n\
                       let mut i = 0;\n\
                       while i < 3 { consume(d); i = i + 1; }\n\
                   }";
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = resolve(&parsed.program);
        assert!(
            resolved.errors.is_empty(),
            "resolve errors: {:?}",
            resolved.errors
        );
        let tc = crate::typecheck(&parsed.program, &resolved);
        let by_fn = predicate_rc_candidates_for_program(&parsed.program, &tc);
        let main = by_fn.get("main").expect("main should have an RC entry");
        assert!(
            main.contains_key("d"),
            "main.d must be flagged as loop-of-consume; got {:?}",
            main.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn defer_body_inner_local_does_not_fire_formal_rc() {
        // Round 12.41 lowers each defer body per-exit-site. Inner-
        // locals introduced inside the body are duplicated across
        // cleanup blocks (one per exit edge). Without per-cleanup-site
        // alpha-renaming in `cfg.rs`, those duplicates would pair as
        // dominance-incomparable Consume sites and spuriously fire
        // the formal RC predicate for an inner-local that has only
        // one live instance per cleanup-site emission. The renaming
        // gives each emission its own binding name so no pairing
        // occurs.
        let src = "struct Data { value: i64 }\n\
                   fn use_data(d: Data) {}\n\
                   fn main() {\n\
                       let x = 1;\n\
                       defer { let local = Data { value: 0 }; use_data(local); }\n\
                       if x > 0 { return; }\n\
                   }";
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = resolve(&parsed.program);
        assert!(
            resolved.errors.is_empty(),
            "resolve errors: {:?}",
            resolved.errors
        );
        let tc = crate::typecheck(&parsed.program, &resolved);
        let by_fn = predicate_rc_candidates_for_program(&parsed.program, &tc);
        if let Some(main) = by_fn.get("main") {
            assert!(
                !main.contains_key("local"),
                "defer-body inner-local `local` must not produce an RC witness; got {:?}",
                main.keys().collect::<Vec<_>>()
            );
            for k in main.keys() {
                assert!(
                    !k.starts_with("local@"),
                    "no mangled `local@cuN` may produce an RC witness; got {k:?}"
                );
            }
        }
    }

    #[test]
    fn loop_of_consume_yields_to_uam_on_same_binding() {
        // Pre-loop consume + in-loop consume → formal UAM fires for
        // `d`. The loop-of-consume rule must yield (UAM is a hard
        // error; firing RC alongside would be redundant noise).
        // Check via the merged `run_predicate_for_function` output.
        let src = "struct Data { value: i64 }\n\
                   fn consume(d: Data) { }\n\
                   fn main() {\n\
                       let d = Data { value: 1 };\n\
                       consume(d);\n\
                       while true { consume(d); }\n\
                   }";
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = resolve(&parsed.program);
        assert!(
            resolved.errors.is_empty(),
            "resolve errors: {:?}",
            resolved.errors
        );
        let tc = crate::typecheck(&parsed.program, &resolved);
        let main_fn = parsed
            .program
            .items
            .iter()
            .find_map(|i| match i {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .expect("no main");
        let (_cfg, _dom, witnesses) = run_predicate_for_function(&parsed.program, &tc, main_fn);
        assert!(
            !witnesses.contains_key("d"),
            "RC must not fire when UAM has flagged the same binding; got {:?}",
            witnesses.keys().collect::<Vec<_>>()
        );
    }
}
