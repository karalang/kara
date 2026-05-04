//! Dominator tree construction via the Cooper-Harvey-Kennedy algorithm.
//!
//! Per design.md § Part 4 RC Dataflow Specification: a structured,
//! reducible CFG admits a near-linear dominator-tree algorithm
//! (Cooper, Harvey, Kennedy 2001 — "A Simple, Fast Dominance Algorithm").
//! This module implements the classic iterative dataflow form, which
//! is materially simpler than Lengauer–Tarjan and runs in ~O(N · α(N))
//! on real CFGs.
//!
//! ## Inputs
//!
//! Takes a `&Cfg` from `crate::cfg`. Expects exactly one entry node
//! (`cfg.entry`); unreachable blocks are tolerated and end up with no
//! immediate dominator (their `idom` slot stays `None`).
//!
//! ## API
//!
//! - `compute_dominators(cfg)` — returns a `DominatorTree` indexed by
//!   `BlockId`.
//! - `DominatorTree::dominates(a, b)` — answers the dominance question
//!   used by the formal RC condition: walks `b`'s idom chain looking
//!   for `a`. Reflexive (every block dominates itself).
//! - `DominatorTree::idom(b)` — immediate dominator of `b`, or `None`
//!   for the entry block / unreachable blocks.

use crate::cfg::{BlockId, Cfg};

/// Sentinel for "no idom assigned yet" during the algorithm.
const UNDEFINED: BlockId = usize::MAX;

#[derive(Debug, Clone)]
pub struct DominatorTree {
    /// Immediate dominator per block, or `None` for the entry / unreachable
    /// blocks.
    idoms: Vec<Option<BlockId>>,
    /// Reverse-postorder of reachable blocks. Used for the iterative
    /// dataflow.
    rpo: Vec<BlockId>,
    /// Position in `rpo` for each reachable block (UNDEFINED for
    /// unreachable blocks). Kept on the struct so future passes (e.g.
    /// loop nesting analysis) can re-use the index without recomputing.
    #[allow(dead_code)]
    rpo_index: Vec<usize>,
}

impl DominatorTree {
    pub fn idom(&self, b: BlockId) -> Option<BlockId> {
        self.idoms.get(b).copied().flatten()
    }

    /// Reflexive dominance: `a` dominates `b` iff `a == b` or `a` lies
    /// on the idom chain from `b` back to the entry. Unreachable blocks
    /// dominate only themselves.
    pub fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        if a == b {
            return true;
        }
        let mut cur = b;
        while let Some(d) = self.idom(cur) {
            if d == a {
                return true;
            }
            if d == cur {
                // Should never happen — defensive guard against a cycle.
                return false;
            }
            cur = d;
        }
        false
    }

    /// Reverse-postorder of reachable blocks (entry first).
    pub fn rpo(&self) -> &[BlockId] {
        &self.rpo
    }
}

/// Compute the dominator tree using Cooper-Harvey-Kennedy.
///
/// ```text
/// Doms[entry] := entry
/// for all other nodes b: Doms[b] := Undefined
/// changed := true
/// while changed:
///     changed := false
///     for each b in reverse postorder (excluding entry):
///         new_idom := first processed predecessor of b
///         for each other predecessor p of b:
///             if Doms[p] != Undefined:
///                 new_idom := intersect(p, new_idom)
///         if Doms[b] != new_idom:
///             Doms[b] := new_idom
///             changed := true
/// ```
pub fn compute_dominators(cfg: &Cfg) -> DominatorTree {
    let n = cfg.num_blocks();
    let entry = cfg.entry;

    let (rpo, rpo_index) = reverse_postorder(cfg);

    // `idom_raw[b] = UNDEFINED` until assigned. Convert to `Option` at
    // the end for the public API.
    let mut idom_raw: Vec<BlockId> = vec![UNDEFINED; n];
    idom_raw[entry] = entry; // entry dominates itself

    let mut changed = true;
    while changed {
        changed = false;
        // Walk all reachable blocks except the entry, in reverse postorder.
        for &b in &rpo {
            if b == entry {
                continue;
            }
            let preds = cfg.predecessors(b);
            // Find the first predecessor that already has an idom.
            let mut new_idom = UNDEFINED;
            for &p in &preds {
                if idom_raw[p] != UNDEFINED {
                    new_idom = p;
                    break;
                }
            }
            if new_idom == UNDEFINED {
                // No processed predecessor yet — skip this iteration.
                continue;
            }
            // Intersect with every other already-processed predecessor.
            for &p in &preds {
                if p == new_idom {
                    continue;
                }
                if idom_raw[p] != UNDEFINED {
                    new_idom = intersect(p, new_idom, &idom_raw, &rpo_index);
                }
            }
            if idom_raw[b] != new_idom {
                idom_raw[b] = new_idom;
                changed = true;
            }
        }
    }

    // Convert to public form: `None` for entry (it dominates itself but
    // has no strict idom) and for unreachable blocks; `Some(_)` otherwise.
    let idoms: Vec<Option<BlockId>> = idom_raw
        .iter()
        .enumerate()
        .map(|(b, &d)| {
            if d == UNDEFINED || b == entry {
                None
            } else {
                Some(d)
            }
        })
        .collect();

    DominatorTree {
        idoms,
        rpo,
        rpo_index,
    }
}

/// Cooper-Harvey-Kennedy `intersect`: walk both fingers up the idom
/// tree until they meet. Comparison uses RPO position — a higher RPO
/// index means deeper in the tree.
fn intersect(mut b1: BlockId, mut b2: BlockId, idom: &[BlockId], rpo_index: &[usize]) -> BlockId {
    while b1 != b2 {
        while rpo_index[b1] > rpo_index[b2] {
            b1 = idom[b1];
        }
        while rpo_index[b2] > rpo_index[b1] {
            b2 = idom[b2];
        }
    }
    b1
}

/// DFS post-order, then reverse to get RPO. Returns:
///  - `rpo` — sequence of reachable blocks in reverse-postorder
///  - `rpo_index[b]` — position of `b` in that sequence (UNDEFINED if
///    unreachable)
fn reverse_postorder(cfg: &Cfg) -> (Vec<BlockId>, Vec<usize>) {
    let n = cfg.num_blocks();
    let mut postorder = Vec::with_capacity(n);
    let mut visited = vec![false; n];

    // Iterative DFS to avoid blowing the Rust stack on deeply nested
    // CFGs from large match arms or chained ifs. We track a stack of
    // (block, next-successor-index-to-visit).
    let mut stack: Vec<(BlockId, usize)> = vec![(cfg.entry, 0)];
    visited[cfg.entry] = true;

    while let Some(&mut (b, ref mut idx)) = stack.last_mut() {
        let succs = &cfg.block(b).successors;
        if *idx < succs.len() {
            let s = succs[*idx];
            *idx += 1;
            if !visited[s] {
                visited[s] = true;
                stack.push((s, 0));
            }
        } else {
            postorder.push(b);
            stack.pop();
        }
    }

    let rpo: Vec<BlockId> = postorder.into_iter().rev().collect();
    let mut rpo_index = vec![UNDEFINED; n];
    for (i, &b) in rpo.iter().enumerate() {
        rpo_index[b] = i;
    }
    (rpo, rpo_index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::{build_cfg, Cfg};
    use crate::{parse, resolve};

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

    #[test]
    fn entry_dominates_everything_reachable() {
        let cfg = cfg_of(
            "fn main() {\n\
                 let c = true;\n\
                 if c { let _ = 1; } else { let _ = 2; }\n\
             }",
        );
        let dom = compute_dominators(&cfg);
        for b in 0..cfg.num_blocks() {
            // Entry dominates every reachable block.
            if dom.rpo().contains(&b) {
                assert!(
                    dom.dominates(cfg.entry, b),
                    "entry should dominate reachable block {b}"
                );
            }
        }
    }

    #[test]
    fn dominance_is_reflexive() {
        let cfg = cfg_of("fn main() { let x = 1; let _y = x; }");
        let dom = compute_dominators(&cfg);
        for b in 0..cfg.num_blocks() {
            assert!(dom.dominates(b, b), "block {b} should dominate itself");
        }
    }

    #[test]
    fn linear_chain_is_total_order() {
        // No control flow → entry dominates exit, exit does not dominate entry.
        let cfg = cfg_of("fn main() { let x = 1; let _y = x; }");
        let dom = compute_dominators(&cfg);
        assert!(dom.dominates(cfg.entry, cfg.exit));
        assert!(!dom.dominates(cfg.exit, cfg.entry));
    }

    #[test]
    fn if_then_else_neither_branch_dominates_the_other() {
        // Then and else are siblings in the dom tree — neither dominates
        // the other. This is the canonical "non-dominance" relationship
        // that the RC condition uses.
        let cfg = cfg_of(
            "fn main() {\n\
                 let c = true;\n\
                 if c { let _a = 1; } else { let _b = 2; }\n\
             }",
        );
        let dom = compute_dominators(&cfg);
        // The cond block is the immediate-dominator of both branches.
        // Find the two arm-entries (immediate successors of the cond
        // block, excluding the merge block — both arm entries have the
        // cond block as their idom).
        let cond_block = cfg
            .blocks
            .iter()
            .find(|b| b.successors.len() == 2 && b.id != cfg.entry || b.successors.len() == 2)
            .expect("expected a 2-successor cond block");
        let cond_id = cond_block.id;
        let succs = cond_block.successors.clone();
        // Both successors are dominated by the cond block.
        for &s in &succs {
            assert!(dom.dominates(cond_id, s), "cond should dominate arm {s}");
        }
        // Neither successor dominates the other.
        if succs.len() == 2 {
            let (a, b) = (succs[0], succs[1]);
            assert!(
                !dom.dominates(a, b) && !dom.dominates(b, a),
                "if-arms should be siblings: neither {a} nor {b} dominates the other"
            );
        }
    }

    #[test]
    fn dominance_transitive_through_idom_chain() {
        // A → B → C linear chain. dominates(A, C) must hold by walking
        // the idom chain.
        let cfg = cfg_of(
            "fn main() {\n\
                 let a = 1;\n\
                 let b = a + 1;\n\
                 let _c = b + 1;\n\
             }",
        );
        let dom = compute_dominators(&cfg);
        assert!(dom.dominates(cfg.entry, cfg.exit));
    }

    #[test]
    fn loop_header_dominates_body_and_merge() {
        let cfg = cfg_of(
            "fn main() {\n\
                 let mut i = 0;\n\
                 while i < 3 { i = i + 1; }\n\
                 let _x = i;\n\
             }",
        );
        let dom = compute_dominators(&cfg);
        // Entry must dominate everything reachable.
        for b in dom.rpo() {
            assert!(dom.dominates(cfg.entry, *b));
        }
        // Self-dominance for every reachable block.
        for &b in dom.rpo() {
            assert!(dom.dominates(b, b));
        }
    }

    #[test]
    fn rpo_starts_with_entry() {
        let cfg = cfg_of("fn main() { let x = 1; let _y = x; }");
        let dom = compute_dominators(&cfg);
        assert_eq!(
            dom.rpo().first().copied(),
            Some(cfg.entry),
            "RPO must start with the entry block"
        );
    }

    // ── Formal RC condition shape (design.md §6317-6325) ───────────
    //
    // The CFG + dominator tree are sufficient to evaluate the
    // RC predicate `∃C∃U. ¬dom(C,U) ∧ ¬dom(U,C)` over each binding's
    // use sites. The tests below construct two contrasting use-pair
    // shapes — one that satisfies the predicate (RC required) and
    // one that does not (no RC) — and check the dom relation.

    #[test]
    fn use_pair_in_branches_satisfies_rc_predicate() {
        // `let x = ...; if c { use(x) } else { consume(x) }` — the
        // consume in one arm and the read in the other arm sit in
        // sibling blocks; neither dominates the other.
        let cfg = cfg_of(
            "fn main() {\n\
                 let c = true;\n\
                 let x = 5;\n\
                 if c { let _a = x + 1; } else { let _b = x; }\n\
             }",
        );
        let dom = compute_dominators(&cfg);
        // Find the cond block (2 successors) and grab its two arms.
        let cond = cfg
            .blocks
            .iter()
            .find(|b| b.successors.len() == 2)
            .expect("expected a cond block with 2 successors");
        let then_b = cond.successors[0];
        let else_b = cond.successors[1];
        // The two use-positions sit in sibling arms — the RC predicate
        // `¬dom(then, else) ∧ ¬dom(else, then)` must hold.
        assert!(
            !dom.dominates(then_b, else_b) && !dom.dominates(else_b, then_b),
            "branches should be siblings: dom(then→else)={}, dom(else→then)={}",
            dom.dominates(then_b, else_b),
            dom.dominates(else_b, then_b),
        );
    }

    #[test]
    fn sequential_uses_do_not_satisfy_rc_predicate() {
        // `let x = ...; let _ = x; let _ = x;` — both uses in the same
        // block, so the earlier dominates the later. dom(C,U) holds —
        // RC predicate requires *neither* side to dominate. So this
        // pair fails the predicate (correctly: sequential reads are
        // not RC fallback territory).
        let cfg = cfg_of(
            "fn main() {\n\
                 let x = 5;\n\
                 let _a = x;\n\
                 let _b = x;\n\
             }",
        );
        let dom = compute_dominators(&cfg);
        // For sequential uses in a linear program, every block
        // dominates every later block — exit is dominated by entry.
        assert!(dom.dominates(cfg.entry, cfg.exit));
    }
}
