// tests/elision.rs — RC elision phase A: trivial intra-fn single-owner
// analysis (`src/ownership/elision.rs`; design record in
// docs/implementation_checklist/phase-7-codegen.md § "RC elision for
// provably-single-owner `shared struct` values").
//
// Analysis-level coverage: candidate eligibility, each allowed use
// shape, and one test per disqualifier. Codegen-side behavior (the
// `FreeSharedElided` cleanup) is pinned in tests/codegen.rs +
// tests/memory_sanitizer.rs.

use karac::ownership::OwnershipCheckResult;
use karac::{ownershipcheck, parse, resolve, typecheck};

fn analyze(source: &str) -> OwnershipCheckResult {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "Type errors: {:?}", typed.errors);
    ownershipcheck(&parsed.program, &typed)
}

fn elided(result: &OwnershipCheckResult, fn_name: &str, binding: &str) -> bool {
    result
        .elided_bindings
        .get(fn_name)
        .is_some_and(|s| s.contains(binding))
}

fn blocked_reason(result: &OwnershipCheckResult, fn_name: &str, binding: &str) -> Option<String> {
    result.elision_blocked.get(fn_name).and_then(|v| {
        v.iter()
            .find(|b| b.binding == binding)
            .map(|b| b.reason.clone())
    })
}

const STATS: &str = "shared struct Stats { mut count: i64, mut total: i64, active: bool }\n";

// ── Positive: the allowed-use surface ───────────────────────────

#[test]
fn elides_scratch_object_with_field_reads_and_writes() {
    let src = format!(
        "{STATS}fn main() {{\n\
             let s = Stats {{ count: 0, total: 0, active: true }};\n\
             s.count = s.count + 1;\n\
             s.total = s.total + 10;\n\
             println(s.total);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(elided(&r, "main", "s"), "blocked: {:?}", r.elision_blocked);
}

#[test]
fn elides_with_ref_fn_arg_and_ref_self_methods() {
    let src = format!(
        "{STATS}\
         fn reader(s: ref Stats) -> i64 {{ s.total }}\n\
         impl Stats {{\n\
             fn bump(mut ref self, n: i64) {{ self.total = self.total + n; }}\n\
             fn snapshot(ref self) -> i64 {{ self.total }}\n\
         }}\n\
         fn main() {{\n\
             let s = Stats {{ count: 0, total: 0, active: true }};\n\
             s.bump(3);\n\
             println(s.snapshot() + reader(s));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(elided(&r, "main", "s"), "blocked: {:?}", r.elision_blocked);
}

#[test]
fn elides_inside_loops_and_branches() {
    // Per-iteration scratch object in a loop body, with field use under
    // an `if` — control flow doesn't block; the cleanup-frame mechanics
    // are codegen's concern.
    let src = format!(
        "{STATS}fn main() {{\n\
             let mut grand = 0;\n\
             let mut i = 0;\n\
             while i < 10 {{\n\
                 let s = Stats {{ count: 0, total: i, active: true }};\n\
                 if s.total > 5 {{ grand = grand + s.total; }}\n\
                 i = i + 1;\n\
             }}\n\
             println(grand);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(elided(&r, "main", "s"), "blocked: {:?}", r.elision_blocked);
}

#[test]
fn elides_independent_binding_while_sibling_blocks() {
    // One candidate escapes (returned), the other stays local — the
    // analysis is per-binding, not per-function.
    let src = format!(
        "{STATS}\
         fn pick() -> Stats {{\n\
             let escapes = Stats {{ count: 1, total: 1, active: true }};\n\
             let local = Stats {{ count: 2, total: 2, active: true }};\n\
             println(local.total);\n\
             escapes\n\
         }}\n\
         fn main() {{ let p = pick(); println(p.total); }}"
    );
    let r = analyze(&src);
    assert!(
        !elided(&r, "pick", "escapes"),
        "escaping binding must not elide"
    );
    assert!(
        elided(&r, "pick", "local"),
        "local binding should elide; blocked: {:?}",
        r.elision_blocked
    );
}

// ── Candidate-eligibility gates ─────────────────────────────────

#[test]
fn non_shared_struct_is_not_a_candidate() {
    let src = "struct Plain { v: i64 }\n\
               fn main() {\n\
                   let p = Plain { v: 1 };\n\
                   println(p.v);\n\
               }";
    let r = analyze(src);
    assert!(!elided(&r, "main", "p"));
    // Not even blocked — never a candidate.
    assert!(blocked_reason(&r, "main", "p").is_none());
}

#[test]
fn heap_field_struct_is_not_a_candidate() {
    // An Option[shared Self] field means the recursive drop would walk
    // and dec — phase A excludes any non-primitive field.
    let src = "shared struct Node { val: i64, mut next: Option[Node] }\n\
               fn main() {\n\
                   let n = Node { val: 1, next: None };\n\
                   println(n.val);\n\
               }";
    let r = analyze(src);
    assert!(!elided(&r, "main", "n"));
    assert!(blocked_reason(&r, "main", "n").is_none());
}

#[test]
fn call_rhs_is_not_a_candidate() {
    // Phase A is literal-birth only; call results are phase-C territory
    // (fresh-return summaries).
    let src = format!(
        "{STATS}\
         fn make() -> Stats {{ Stats {{ count: 0, total: 0, active: true }} }}\n\
         fn main() {{\n\
             let s = make();\n\
             println(s.total);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
}

// ── Disqualifiers ───────────────────────────────────────────────

#[test]
fn blocks_alias_let() {
    let src = format!(
        "{STATS}fn main() {{\n\
             let s = Stats {{ count: 0, total: 0, active: true }};\n\
             let t = s;\n\
             println(t.total);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
    assert!(
        blocked_reason(&r, "main", "s").unwrap().contains("aliased"),
        "got: {:?}",
        blocked_reason(&r, "main", "s")
    );
}

#[test]
fn elides_read_only_declared_owned_arg() {
    // `consume` declares an owned param but its body only reads —
    // the INFERRED mode is Ref, which proves no retention: the
    // callee's receive-inc/scope-dec self-balance and elision stays
    // sound. (The would-be-mode inference is the load-bearing gate;
    // see the rule-3 note in src/ownership/elision.rs.)
    let src = format!(
        "{STATS}\
         fn consume(s: Stats) -> i64 {{ s.total }}\n\
         fn main() {{\n\
             let s = Stats {{ count: 0, total: 7, active: true }};\n\
             println(consume(s));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(elided(&r, "main", "s"), "blocked: {:?}", r.elision_blocked);
}

#[test]
fn blocks_arg_to_retaining_callee() {
    // `pass` returns its param — the body CONSUMES it, the inferred
    // mode is Own, and the caller's binding must keep real RC (the
    // returned alias outlives the call).
    let src = format!(
        "{STATS}\
         fn pass(s: Stats) -> Stats {{ s }}\n\
         fn main() {{\n\
             let s = Stats {{ count: 0, total: 7, active: true }};\n\
             let t = pass(s);\n\
             println(t.total);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
    assert!(blocked_reason(&r, "main", "s").unwrap().contains("owned"));
}

#[test]
fn blocks_owned_self_method_receiver() {
    let src = format!(
        "{STATS}\
         impl Stats {{ fn into_total(self) -> i64 {{ self.total }} }}\n\
         fn main() {{\n\
             let s = Stats {{ count: 0, total: 7, active: true }};\n\
             println(s.into_total());\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
    assert!(blocked_reason(&r, "main", "s")
        .unwrap()
        .contains("owned-self"));
}

#[test]
fn blocks_tail_return() {
    let src = format!(
        "{STATS}\
         fn build() -> Stats {{\n\
             let s = Stats {{ count: 0, total: 0, active: true }};\n\
             s\n\
         }}\n\
         fn main() {{ let b = build(); println(b.total); }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "build", "s"));
}

#[test]
fn blocks_store_into_struct_literal() {
    let src = format!(
        "{STATS}\
         shared struct Holder {{ mut inner_total: i64 }}\n\
         fn main() {{\n\
             let s = Stats {{ count: 0, total: 0, active: true }};\n\
             let pair = (s, 1);\n\
             println(pair.1);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
}

#[test]
fn blocks_closure_capture() {
    let src = format!(
        "{STATS}fn main() {{\n\
             let s = Stats {{ count: 0, total: 9, active: true }};\n\
             let get = || s.total;\n\
             println(get());\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
    assert!(blocked_reason(&r, "main", "s").unwrap().contains("closure"));
}

#[test]
fn blocks_par_region_use() {
    let src = format!(
        "{STATS}fn main() {{\n\
             let s = Stats {{ count: 0, total: 9, active: true }};\n\
             par {{\n\
                 println(s.total);\n\
                 println(1);\n\
             }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
}

#[test]
fn blocks_reassignment() {
    let src = format!(
        "{STATS}fn main() {{\n\
             let mut s = Stats {{ count: 0, total: 0, active: true }};\n\
             s = Stats {{ count: 1, total: 1, active: true }};\n\
             println(s.total);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
    assert!(blocked_reason(&r, "main", "s")
        .unwrap()
        .contains("reassigned"));
}

#[test]
fn blocks_rebound_name() {
    let src = format!(
        "{STATS}fn main() {{\n\
             let s = Stats {{ count: 0, total: 0, active: true }};\n\
             println(s.total);\n\
             let s = 5;\n\
             println(s);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
}

#[test]
fn blocks_enum_ctor_capture() {
    // `Some(s)` — the ctor stores the value; "Some" resolves no
    // param_modes entry → conservative block via the unresolved-arg
    // rule.
    let src = format!(
        "{STATS}fn main() {{\n\
             let s = Stats {{ count: 0, total: 0, active: true }};\n\
             let o = Some(s);\n\
             if o.is_some() {{ println(1); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!elided(&r, "main", "s"));
}

// ════════════════════════════════════════════════════════════════
// Phase B1 — append-only chain clusters (root free-walk).
// ════════════════════════════════════════════════════════════════

const NODE: &str = "shared struct ListNode { val: i64, mut next: Option[ListNode] }\n";

fn cluster_root(result: &OwnershipCheckResult, fn_name: &str) -> Option<String> {
    result
        .elided_clusters
        .get(fn_name)
        .and_then(|v| v.first())
        .map(|c| c.root.clone())
}

const CANONICAL_BUILDER: &str = "fn build_and_sum(n: i64) -> i64 {\n\
     let dummy = ListNode { val: 0, next: None };\n\
     let mut tail = dummy;\n\
     let mut i = 1;\n\
     while i <= n {\n\
         let node = ListNode { val: i, next: None };\n\
         tail.next = Some(node);\n\
         tail = node;\n\
         i = i + 1;\n\
     }\n\
     let mut sum = 0;\n\
     let mut cur = dummy.next;\n\
     while cur.is_some() {\n\
         let x = cur.unwrap();\n\
         sum = sum + x.val;\n\
         cur = x.next;\n\
     }\n\
     sum\n\
 }\n\
 fn main() { println(build_and_sum(5)); }";

#[test]
fn cluster_elides_canonical_append_builder() {
    let src = format!("{NODE}{CANONICAL_BUILDER}");
    let r = analyze(&src);
    assert_eq!(
        cluster_root(&r, "build_and_sum").as_deref(),
        Some("dummy"),
        "clusters: {:?}",
        r.elided_clusters
    );
    let c = &r.elided_clusters["build_and_sum"][0];
    assert_eq!(c.member_type, "ListNode");
    assert_eq!(c.link_field_index, 1);
    for b in ["dummy", "tail", "node", "cur", "x"] {
        assert!(c.bindings.contains(b), "missing {b}: {:?}", c.bindings);
    }
}

#[test]
fn cluster_blocks_chain_escaping_to_call() {
    // Passing any cluster binding to a call blocks the whole cluster
    // (v1: walks must be inline).
    let src = format!(
        "{NODE}\
         fn sum(head: Option[ListNode]) -> i64 {{\n\
             let mut t = 0;\n\
             let mut cur = head;\n\
             while cur.is_some() {{\n\
                 let n = cur.unwrap();\n\
                 t = t + n.val;\n\
                 cur = n.next;\n\
             }}\n\
             t\n\
         }}\n\
         fn run() -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let mut tail = dummy;\n\
             let node = ListNode {{ val: 1, next: None }};\n\
             tail.next = Some(node);\n\
             sum(dummy.next)\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "run").is_none());
}

#[test]
fn cluster_blocks_returned_chain() {
    let src = format!(
        "{NODE}\
         fn build() -> Option[ListNode] {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let node = ListNode {{ val: 1, next: None }};\n\
             dummy.next = Some(node);\n\
             dummy.next\n\
         }}\n\
         fn main() {{ let c = build(); if c.is_some() {{ println(1); }} }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "build").is_none());
}

#[test]
fn cluster_blocks_double_link_of_same_fresh_node() {
    // The same fresh node linked at two sites would give it two
    // parents — the free-walk would double-free.
    let src = format!(
        "{NODE}\
         fn run() -> i64 {{\n\
             let a = ListNode {{ val: 0, next: None }};\n\
             let b = ListNode {{ val: 1, next: None }};\n\
             let node = ListNode {{ val: 2, next: None }};\n\
             a.next = Some(node);\n\
             b.next = Some(node);\n\
             a.val + b.val\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "run").is_none());
}

#[test]
fn cluster_blocks_cursor_in_link_value_position() {
    // Re-parenting an existing (link-read) node — splice idioms are
    // not append-only.
    let src = format!(
        "{NODE}\
         fn run() -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let node = ListNode {{ val: 1, next: None }};\n\
             dummy.next = Some(node);\n\
             let c = dummy.next;\n\
             let n2 = c.unwrap();\n\
             dummy.next = Some(n2);\n\
             dummy.val\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "run").is_none());
}

#[test]
fn cluster_blocks_member_type_parameter() {
    // The cluster type entering via a parameter means non-local chains
    // of the same type exist — poison.
    let src = format!(
        "{NODE}\
         fn extend(seed: ListNode) -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let node = ListNode {{ val: 1, next: None }};\n\
             dummy.next = Some(node);\n\
             dummy.val + seed.val\n\
         }}\n\
         fn main() {{\n\
             let s = ListNode {{ val: 9, next: None }};\n\
             println(extend(s));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "extend").is_none());
}

#[test]
fn cluster_blocks_prepend_idiom() {
    // Prepend couples the literal link-init to the root reassignment —
    // flow-sensitive, deferred (B1.1). The literal with a non-None
    // link init never joins, so no cluster forms.
    let src = format!(
        "{NODE}\
         fn build(n: i64) -> i64 {{\n\
             let mut head: Option[ListNode] = None;\n\
             let mut i = 0;\n\
             while i < n {{\n\
                 let node = ListNode {{ val: i, next: head }};\n\
                 head = Some(node);\n\
                 i = i + 1;\n\
             }}\n\
             let mut sum = 0;\n\
             let mut cur = head;\n\
             while cur.is_some() {{\n\
                 let x = cur.unwrap();\n\
                 sum = sum + x.val;\n\
                 cur = x.next;\n\
             }}\n\
             sum\n\
         }}\n\
         fn main() {{ println(build(5)); }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "build").is_none());
}

#[test]
fn cluster_blocks_shadowed_fresh_name() {
    // A for-loop variable shadowing a cluster name could route an
    // external object through a cluster-classified use — poison.
    let src = format!(
        "{NODE}\
         fn run() -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let node = ListNode {{ val: 1, next: None }};\n\
             dummy.next = Some(node);\n\
             let mut t = 0;\n\
             for node in 0..3 {{\n\
                 t = t + node;\n\
             }}\n\
             t + dummy.val\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "run").is_none());
}

#[test]
fn cluster_allows_link_overwrite_displacement() {
    // Overwriting a link ORPHANS the displaced node (the store's
    // release-old frees it through normal RC) — the chain stays a
    // tree, append-only holds, cluster stays elidable.
    let src = format!(
        "{NODE}\
         fn run() -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let a = ListNode {{ val: 1, next: None }};\n\
             let b = ListNode {{ val: 2, next: None }};\n\
             dummy.next = Some(a);\n\
             dummy.next = Some(b);\n\
             let mut sum = 0;\n\
             let mut cur = dummy.next;\n\
             while cur.is_some() {{\n\
                 let x = cur.unwrap();\n\
                 sum = sum + x.val;\n\
                 cur = x.next;\n\
             }}\n\
             sum\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    assert_eq!(cluster_root(&r, "run").as_deref(), Some("dummy"));
}

#[test]
fn cluster_second_chain_same_type_keeps_standard_cleanup() {
    // A second never-linked literal of the same type joins the binding
    // set as a fresh node but is not the root — only the ROOT takes
    // the free-walk; everything else keeps RcDec, so a second
    // independent owner is sound (it dec-walks its own object).
    let src = format!(
        "{NODE}\
         fn run() -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let node = ListNode {{ val: 1, next: None }};\n\
             dummy.next = Some(node);\n\
             let lone = ListNode {{ val: 5, next: None }};\n\
             dummy.val + lone.val\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    assert_eq!(cluster_root(&r, "run").as_deref(), Some("dummy"));
}

// ── Phase B2 — build-side count elision flag ────────────────────

#[test]
fn b2_recognizes_canonical_triple() {
    let src = format!("{NODE}{CANONICAL_BUILDER}");
    let r = analyze(&src);
    let c = &r.elided_clusters["build_and_sum"][0];
    assert!(c.b2, "canonical triple should be b2");
    assert!(c.fresh_linked.contains("node"));
    assert!(c.bare_cursors.contains("tail"));
    assert!(c.bare_cursors.contains("x"));
    assert!(c.option_cursors.contains("cur"));
}

#[test]
fn b2_accepts_single_store_outside_loops() {
    let src = format!(
        "{NODE}\
         fn run() -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let node = ListNode {{ val: 7, next: None }};\n\
             dummy.next = Some(node);\n\
             let c = dummy.next;\n\
             let n2 = c.unwrap();\n\
             n2.val\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    let c = &r.elided_clusters["run"][0];
    assert!(c.b2, "single store outside loops should be b2");
}

#[test]
fn b2_rejects_non_adjacent_triple() {
    // Store + advance separated by control flow inside the loop —
    // displacement can no longer be ruled out structurally.
    let src = format!(
        "{NODE}\
         fn run(n: i64) -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let mut tail = dummy;\n\
             let mut i = 0;\n\
             while i < n {{\n\
                 let node = ListNode {{ val: i, next: None }};\n\
                 if i > 0 {{\n\
                     tail.next = Some(node);\n\
                     tail = node;\n\
                 }}\n\
                 i = i + 1;\n\
             }}\n\
             dummy.val\n\
         }}\n\
         fn main() {{ println(run(3)); }}"
    );
    let r = analyze(&src);
    let c = &r.elided_clusters["run"][0];
    assert!(!c.b2, "non-adjacent triple must not be b2 (B1 still ok)");
}

#[test]
fn b2_rejects_two_store_sites() {
    let src = format!(
        "{NODE}\
         fn run() -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let a = ListNode {{ val: 10, next: None }};\n\
             let b = ListNode {{ val: 20, next: None }};\n\
             dummy.next = Some(a);\n\
             dummy.next = Some(b);\n\
             dummy.val\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    let c = &r.elided_clusters["run"][0];
    assert!(!c.b2, "two store sites (displacement) must not be b2");
}

#[test]
fn b2_rejects_link_read_before_store() {
    let src = format!(
        "{NODE}\
         fn run() -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let early = dummy.next;\n\
             let node = ListNode {{ val: 7, next: None }};\n\
             dummy.next = Some(node);\n\
             if early.is_none() {{ dummy.val }} else {{ node.val }}\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    // The cluster may or may not survive B1 (node.val in a tail
    // branch is a prim read — allowed); the b2 flag must be off.
    if let Some(cs) = r.elided_clusters.get("run") {
        assert!(!cs[0].b2, "read-before-store must not be b2");
    }
}

#[test]
fn b2_rejects_unlinked_fresh_node() {
    let src = format!(
        "{NODE}\
         fn run() -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let node = ListNode {{ val: 7, next: None }};\n\
             let lone = ListNode {{ val: 9, next: None }};\n\
             dummy.next = Some(node);\n\
             lone.val\n\
         }}\n\
         fn main() {{ println(run()); }}"
    );
    let r = analyze(&src);
    let c = &r.elided_clusters["run"][0];
    assert!(!c.b2, "never-linked fresh node must not be b2");
}
