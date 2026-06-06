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
fn cluster_returned_chain_is_sanctioned_rootlink() {
    // Pre-C1b this shape was a blocked escape (`dummy.next` returned).
    // C1b sanctions exactly this tail: the single outside-loop store
    // is b2, so the chain transfers structurally at rc==1 per node and
    // the root header frees alone. (The unsanctioned variants stay
    // blocked — see `fresh_return_requires_b2` /
    // `fresh_return_mid_fn_return_still_poisons`.)
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
    let c = &r.elided_clusters["build"][0];
    assert!(c.b2);
    assert_eq!(c.returned, karac::ownership::ReturnedChain::RootLink);
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
fn cluster_coexists_with_member_type_parameter() {
    // Phase C1a: a member-type param no longer poisons by presence.
    // The flow walls keep `seed` foreign (it can't join membership,
    // can't be link-stored, and full RC covers it); the fn-local
    // chain still clusters. Headerless demotes — the param is a
    // signature mention of the member type.
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
    assert_eq!(
        cluster_root(&r, "extend").as_deref(),
        Some("dummy"),
        "clusters: {:?}",
        r.elided_clusters
    );
    let c = &r.elided_clusters["extend"][0];
    assert!(!c.headerless, "param sig mention must demote headerless");
}

#[test]
fn cluster_param_walls_add_two_numbers_shape() {
    // Kata #2's exact builder: member-type params walked via if-let,
    // a canonical-triple loop append, RootLink tail. C1a + C1b
    // compose: the cluster forms, builds count-free (b2), and the
    // chain transfers out through `dummy.next`.
    let src = format!(
        "{NODE}\
         fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let mut tail = dummy;\n\
             let mut a = l1;\n\
             let mut b = l2;\n\
             let mut carry: i64 = 0;\n\
             loop {{\n\
                 let mut s: i64 = carry;\n\
                 let mut done = true;\n\
                 if let Some(n) = a {{\n\
                     s = s + n.val;\n\
                     a = n.next;\n\
                     done = false;\n\
                 }}\n\
                 if let Some(n) = b {{\n\
                     s = s + n.val;\n\
                     b = n.next;\n\
                     done = false;\n\
                 }}\n\
                 if done and s == 0 {{\n\
                     break;\n\
                 }}\n\
                 let node = ListNode {{ val: s % 10, next: None }};\n\
                 tail.next = Some(node);\n\
                 tail = node;\n\
                 carry = s / 10;\n\
             }}\n\
             dummy.next\n\
         }}\n\
         fn main() {{\n\
             let x = ListNode {{ val: 7, next: None }};\n\
             let y = ListNode {{ val: 5, next: None }};\n\
             let r = add_two_numbers(Some(x), Some(y));\n\
             if r.is_some() {{ println(r.unwrap().val); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert_eq!(
        cluster_root(&r, "add_two_numbers").as_deref(),
        Some("dummy"),
        "clusters: {:?}",
        r.elided_clusters
    );
    let c = &r.elided_clusters["add_two_numbers"][0];
    assert!(c.b2, "canonical triple must recognize as b2");
    assert_eq!(c.returned, karac::ownership::ReturnedChain::RootLink);
    assert!(!c.headerless);
}

#[test]
fn cluster_blocks_param_spliced_into_link() {
    // A param value in link-store position is a non-fresh store —
    // the splice wall. No cluster forms.
    let src = format!(
        "{NODE}\
         fn graft(seed: ListNode) -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             dummy.next = Some(seed);\n\
             dummy.val\n\
         }}\n\
         fn main() {{\n\
             let s = ListNode {{ val: 9, next: None }};\n\
             println(graft(s));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "graft").is_none());
}

#[test]
fn cluster_blocks_fresh_node_stored_under_param() {
    // A fresh cluster node stored under a param-rooted place escapes
    // the cluster (default-deny Identifier arm) — the free-walk would
    // double-free it against the param chain's RC drop.
    let src = format!(
        "{NODE}\
         fn leak_into(seed: ListNode) -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let node = ListNode {{ val: 1, next: None }};\n\
             dummy.next = Some(node);\n\
             let node2 = ListNode {{ val: 2, next: None }};\n\
             seed.next = Some(node2);\n\
             dummy.val\n\
         }}\n\
         fn main() {{\n\
             let s = ListNode {{ val: 9, next: None }};\n\
             println(leak_into(s));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "leak_into").is_none());
}

#[test]
fn cluster_blocks_param_name_shadowing_cluster_name() {
    // A param sharing a cluster binding's name — the name-keyed
    // analysis could misattribute; the shadow check still poisons.
    let src = format!(
        "{NODE}\
         fn run(node: ListNode) -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let node = ListNode {{ val: 1, next: None }};\n\
             dummy.next = Some(node);\n\
             dummy.val\n\
         }}\n\
         fn main() {{\n\
             let s = ListNode {{ val: 9, next: None }};\n\
             println(run(s));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(cluster_root(&r, "run").is_none());
}

// ════════════════════════════════════════════════════════════════
// Phase C1c — caller adoption (fresh-return call results free-walk).
// ════════════════════════════════════════════════════════════════

/// SomeRoot builder reused by the adoption tests.
const ADOPT_BUILDER: &str = "fn build(n: i64) -> Option[ListNode] {\n\
     let head = ListNode { val: 1, next: None };\n\
     let mut tail = head;\n\
     let mut i = 2;\n\
     while i <= n {\n\
         let node = ListNode { val: i, next: None };\n\
         tail.next = Some(node);\n\
         tail = node;\n\
         i = i + 1;\n\
     }\n\
     Some(head)\n\
 }\n";

fn adopted_cluster<'r>(
    result: &'r OwnershipCheckResult,
    fn_name: &str,
) -> Option<&'r karac::ownership::ElidedCluster> {
    result
        .elided_clusters
        .get(fn_name)
        .and_then(|v| v.iter().find(|c| c.adopted))
}

#[test]
fn adopts_matched_builder_result() {
    // Kata #2 `main` shape: builder-call result bound in a loop,
    // head read through the sanctioned match, dropped per iteration.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn main() {{\n\
             let mut total = 0;\n\
             let mut iter = 0;\n\
             while iter < 3 {{\n\
                 let out = build(5);\n\
                 match out {{\n\
                     Some(node) => {{ total = total + node.val; }}\n\
                     None => {{}}\n\
                 }}\n\
                 iter = iter + 1;\n\
             }}\n\
             println(total);\n\
         }}"
    );
    let r = analyze(&src);
    let c = adopted_cluster(&r, "main").expect("main adopts the build() result");
    assert_eq!(c.root, "out");
    assert!(c.b2, "adopted families reuse the b2 count-free roles");
    assert!(!c.headerless);
    assert_eq!(c.returned, karac::ownership::ReturnedChain::No);
    assert!(c.bare_cursors.contains("node"), "{:?}", c.bare_cursors);
}

#[test]
fn adopts_walked_builder_result() {
    // The walking caller: option-cursor alias of the root, unwrap,
    // link-read advance — all existing cursor rules plus the C1c
    // alias shape.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn main() {{\n\
             let out = build(5);\n\
             let mut sum = 0;\n\
             let mut cur = out;\n\
             while cur.is_some() {{\n\
                 let x = cur.unwrap();\n\
                 sum = sum + x.val;\n\
                 cur = x.next;\n\
             }}\n\
             println(sum);\n\
         }}"
    );
    let r = analyze(&src);
    let c = adopted_cluster(&r, "main").expect("walked result adopts");
    assert_eq!(c.root, "out");
    assert!(c.option_cursors.contains("cur"), "{:?}", c.option_cursors);
    assert!(c.bare_cursors.contains("x"), "{:?}", c.bare_cursors);
}

#[test]
fn adoption_kata2_main_escaping_args_rejected_adopting_out() {
    // Kata #2's full main: l1/l2 escape as call args (their own scans
    // poison → full RC), while out adopts. Exactly one adopted family.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn consume(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let mut tail = dummy;\n\
             let mut a = l1;\n\
             let mut b = l2;\n\
             loop {{\n\
                 let mut s: i64 = 0;\n\
                 let mut done = true;\n\
                 if let Some(n) = a {{\n\
                     s = s + n.val;\n\
                     a = n.next;\n\
                     done = false;\n\
                 }}\n\
                 if let Some(n) = b {{\n\
                     s = s + n.val;\n\
                     b = n.next;\n\
                     done = false;\n\
                 }}\n\
                 if done {{\n\
                     break;\n\
                 }}\n\
                 let node = ListNode {{ val: s, next: None }};\n\
                 tail.next = Some(node);\n\
                 tail = node;\n\
             }}\n\
             dummy.next\n\
         }}\n\
         fn main() {{\n\
             let l1 = build(4);\n\
             let l2 = build(4);\n\
             let mut total = 0;\n\
             let mut iter = 0;\n\
             while iter < 3 {{\n\
                 let out = consume(l1, l2);\n\
                 match out {{\n\
                     Some(node) => {{ total = total + node.val; }}\n\
                     None => {{}}\n\
                 }}\n\
                 iter = iter + 1;\n\
             }}\n\
             println(total);\n\
         }}"
    );
    let r = analyze(&src);
    // C2b updated this contract: `consume`'s params BORROW (read-only
    // walk family), so passing l1/l2 at its borrowed positions is the
    // sanctioned-arg channel — all three bindings adopt, l1/l2 flagged
    // arg_sanctioned (active only under the headerless gate, which
    // this leak-free program passes). Pre-C2b, l1/l2 were rejected as
    // escapes and only `out` adopted.
    let adopted: Vec<_> = r.elided_clusters["main"]
        .iter()
        .filter(|c| c.adopted)
        .collect();
    assert_eq!(adopted.len(), 3, "{:?}", r.elided_clusters["main"]);
    for c in &adopted {
        assert_eq!(
            c.arg_sanctioned,
            c.root != "out",
            "l1/l2 use the sanctioned-arg channel; out does not: {:?}",
            c
        );
    }
    assert!(
        r.headerless_types.contains_key("ListNode"),
        "the kata-shaped program passes the headerless gate: {:?}",
        r.headerless_types
    );
}

#[test]
fn adoption_rejects_returned_root() {
    // Re-exporting the adopted chain needs a transfer composition C1c
    // doesn't claim — the family poisons and keeps full RC.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn relay() -> Option[ListNode] {{\n\
             let out = build(3);\n\
             out\n\
         }}\n\
         fn main() {{\n\
             let c = relay();\n\
             if c.is_some() {{ println(1); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(adopted_cluster(&r, "relay").is_none());
}

#[test]
fn adoption_skips_fn_with_member_literals() {
    // A caller that also constructs member literals keeps the
    // literal-cluster machinery; adoption stands down for that type.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn main() {{\n\
             let out = build(3);\n\
             let local = ListNode {{ val: 9, next: None }};\n\
             if out.is_some() {{ println(local.val); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(adopted_cluster(&r, "main").is_none());
}

#[test]
fn adoption_rejects_guarded_match() {
    // A guard breaks the sanctioned read-only match shape — the
    // scrutinee mention falls back to default-deny.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn main() {{\n\
             let out = build(3);\n\
             let mut t = 0;\n\
             match out {{\n\
                 Some(node) if node.val > 0 => {{ t = t + 1; }}\n\
                 Some(node) => {{ t = t + node.val; }}\n\
                 None => {{}}\n\
             }}\n\
             println(t);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(adopted_cluster(&r, "main").is_none());
}

#[test]
fn adoption_rejects_root_reassignment() {
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn main() {{\n\
             let mut out = build(3);\n\
             out = build(4);\n\
             if out.is_some() {{ println(1); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(adopted_cluster(&r, "main").is_none());
}

#[test]
fn adoption_rejects_link_store_into_adopted_chain() {
    // Adopted families are read-only: even the `= None` severing store
    // (legal in literal clusters) poisons — the family's count-free
    // cursors skip release-old, so the severed tail would leak past
    // the root's walk.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn main() {{\n\
             let out = build(3);\n\
             let mut cur = out;\n\
             let x = cur.unwrap();\n\
             x.next = None;\n\
             println(x.val);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(adopted_cluster(&r, "main").is_none());
}

#[test]
fn adoption_requires_fresh_return_builder() {
    // A callee without a fresh-return cluster summary yields no
    // adoption — the result keeps full RC.
    let src = format!(
        "{NODE}\
         fn just_none() -> Option[ListNode] {{\n\
             None\n\
         }}\n\
         fn main() {{\n\
             let out = just_none();\n\
             if out.is_some() {{ println(1); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(adopted_cluster(&r, "main").is_none());
}

// ════════════════════════════════════════════════════════════════
// Phase C2a — borrowed-param walk families.
// ════════════════════════════════════════════════════════════════

fn borrowed_cluster<'r>(
    result: &'r OwnershipCheckResult,
    fn_name: &str,
) -> Option<&'r karac::ownership::ElidedCluster> {
    result
        .elided_clusters
        .get(fn_name)
        .and_then(|v| v.iter().find(|c| c.borrowed))
}

/// Kata #2's verbatim adder: two walked Option params + its own
/// literal cluster (b2 triple, RootLink return).
const ADDER: &str =
    "fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {\n\
     let dummy = ListNode { val: 0, next: None };\n\
     let mut tail = dummy;\n\
     let mut a = l1;\n\
     let mut b = l2;\n\
     let mut carry: i64 = 0;\n\
     loop {\n\
         let mut s: i64 = carry;\n\
         let mut done = true;\n\
         if let Some(n) = a {\n\
             s = s + n.val;\n\
             a = n.next;\n\
             done = false;\n\
         }\n\
         if let Some(n) = b {\n\
             s = s + n.val;\n\
             b = n.next;\n\
             done = false;\n\
         }\n\
         if done and s == 0 {\n\
             break;\n\
         }\n\
         let node = ListNode { val: s % 10, next: None };\n\
         tail.next = Some(node);\n\
         tail = node;\n\
         carry = s / 10;\n\
     }\n\
     dummy.next\n\
 }\n\
 fn main() {\n\
     let x = ListNode { val: 7, next: None };\n\
     let y = ListNode { val: 5, next: None };\n\
     let r = add_two_numbers(Some(x), Some(y));\n\
     if r.is_some() { println(r.unwrap().val); }\n\
 }";

#[test]
fn borrows_walked_option_params_kata2_adder() {
    // Both params join ONE family (the sibling if-lets share the
    // pattern-bound `n`); walk cursors take count-skip roles while the
    // params themselves stay out of the cursor sets (they keep the
    // balanced entry/exit ownership). The fn's own literal cluster
    // (RootLink) coexists untouched.
    let src = format!("{NODE}{ADDER}");
    let r = analyze(&src);
    let c = borrowed_cluster(&r, "add_two_numbers").expect("params borrow");
    assert_eq!(
        c.borrowed_params
            .iter()
            .map(|(_, i)| *i)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(c.b2);
    assert!(!c.headerless);
    assert!(!c.adopted);
    assert!(c.option_cursors.contains("a") && c.option_cursors.contains("b"));
    assert!(c.bare_cursors.contains("n"));
    assert!(
        !c.option_cursors.contains("l1") && !c.bare_cursors.contains("l1"),
        "params keep full registration"
    );
    let literal = r.elided_clusters["add_two_numbers"]
        .iter()
        .find(|c| !c.borrowed)
        .expect("literal cluster coexists");
    assert_eq!(literal.root, "dummy");
    assert_eq!(literal.returned, karac::ownership::ReturnedChain::RootLink);
}

#[test]
fn borrow_while_let_walk() {
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
         fn main() {{\n\
             let s = ListNode {{ val: 9, next: None }};\n\
             println(sum(Some(s)));\n\
         }}"
    );
    let r = analyze(&src);
    let c = borrowed_cluster(&r, "sum").expect("walked param borrows");
    assert!(c.option_cursors.contains("cur"));
    assert!(c.bare_cursors.contains("n"));
}

#[test]
fn borrow_rejects_param_passed_to_call() {
    let src = format!(
        "{NODE}\
         fn len(head: Option[ListNode]) -> i64 {{\n\
             if head.is_some() {{ 1 }} else {{ 0 }}\n\
         }}\n\
         fn relay(head: Option[ListNode]) -> i64 {{\n\
             len(head)\n\
         }}\n\
         fn main() {{\n\
             let s = ListNode {{ val: 9, next: None }};\n\
             println(relay(Some(s)));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(borrowed_cluster(&r, "relay").is_none());
    // `len` itself only does is_some — it borrows fine.
    assert!(borrowed_cluster(&r, "len").is_some());
}

#[test]
fn borrow_rejects_returned_param() {
    // Returning a borrow would hand the caller a second owner.
    let src = format!(
        "{NODE}\
         fn pass(head: Option[ListNode]) -> Option[ListNode] {{\n\
             head\n\
         }}\n\
         fn main() {{\n\
             let s = ListNode {{ val: 9, next: None }};\n\
             let r = pass(Some(s));\n\
             if r.is_some() {{ println(1); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(borrowed_cluster(&r, "pass").is_none());
}

#[test]
fn borrow_rejects_link_store_under_param_chain() {
    // Grafting a local node under the borrowed chain is a link store
    // into a read-only family — poison (kata #19\'s remove_nth shape
    // keeps full RC).
    let src = format!(
        "{NODE}\
         fn graft(head: Option[ListNode]) -> i64 {{\n\
             if let Some(n) = head {{\n\
                 let node = ListNode {{ val: 1, next: None }};\n\
                 n.next = Some(node);\n\
                 return n.val;\n\
             }}\n\
             0\n\
         }}\n\
         fn main() {{\n\
             let s = ListNode {{ val: 9, next: None }};\n\
             println(graft(Some(s)));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(borrowed_cluster(&r, "graft").is_none());
}

#[test]
fn borrow_rejects_param_rebound_by_let() {
    let src = format!(
        "{NODE}\
         fn shadowed(head: Option[ListNode]) -> i64 {{\n\
             let head = ListNode {{ val: 1, next: None }};\n\
             head.val\n\
         }}\n\
         fn main() {{\n\
             let s = ListNode {{ val: 9, next: None }};\n\
             println(shadowed(Some(s)));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(borrowed_cluster(&r, "shadowed").is_none());
}

// ════════════════════════════════════════════════════════════════
// Phase C2b — program-wide headerless-T gate.
// ════════════════════════════════════════════════════════════════

#[test]
fn headerless_gate_passes_builder_only_program() {
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn main() {{\n\
             let out = build(5);\n\
             match out {{\n\
                 Some(node) => {{ println(node.val); }}\n\
                 None => {{}}\n\
             }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(r.headerless_types.contains_key("ListNode"));
    let (link_idx, fns) = &r.headerless_types["ListNode"];
    assert_eq!(*link_idx, 1);
    assert!(fns.contains(&"build".to_string()) && fns.contains(&"main".to_string()));
}

#[test]
fn headerless_gate_rejects_fn_as_value() {
    // A summarized builder referenced as a VALUE creates an
    // unsummarized indirect call site — the two-sided residual-count
    // contract would leak/corrupt. The walker must see every position.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn main() {{\n\
             let f = build;\n\
             let out = f(5);\n\
             if out.is_some() {{ println(1); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!r.headerless_types.contains_key("ListNode"));
}

#[test]
fn headerless_gate_rejects_struct_field_holder() {
    // A foreign struct field can hold a headered-expectation T — the
    // surface scan (unchanged from phase D minus the fn-sig leniency)
    // blocks.
    let src = format!(
        "{NODE}\
         struct Holder {{ n: Option[ListNode] }}\n\
         {ADOPT_BUILDER}\
         fn main() {{\n\
             let out = build(5);\n\
             if out.is_some() {{ println(1); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!r.headerless_types.contains_key("ListNode"));
}

#[test]
fn headerless_gate_rejects_unadopted_builder_call() {
    // A builder call outside adopted-let position drops its chain
    // through the full-RC temp path — count traffic on T survives, so
    // T stays headered.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn main() {{\n\
             let out = build(5);\n\
             if out.is_some() {{ println(1); }}\n\
             let pair = (build(2), 1);\n\
             println(pair.1);\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!r.headerless_types.contains_key("ListNode"));
}

#[test]
fn headerless_gate_rejects_bare_member_param() {
    // A bare-T param is neither a fresh-return nor a borrowed-option
    // channel — the lenient sig scan still blocks it.
    let src = format!(
        "{NODE}{ADOPT_BUILDER}\
         fn peek(seed: ListNode) -> i64 {{ seed.val }}\n\
         fn main() {{\n\
             let out = build(5);\n\
             if out.is_some() {{ println(1); }}\n\
             let s = ListNode {{ val: 3, next: None }};\n\
             println(peek(s));\n\
         }}"
    );
    let r = analyze(&src);
    assert!(!r.headerless_types.contains_key("ListNode"));
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

// ── Phase D — headerless member layout (b2 + dual purity gate) ──

#[test]
fn headerless_on_canonical_builder() {
    let src = format!("{NODE}{CANONICAL_BUILDER}");
    let r = analyze(&src);
    let c = &r.elided_clusters["build_and_sum"][0];
    assert!(c.b2);
    assert!(
        c.headerless,
        "canonical builder in a pure program should be headerless"
    );
}

#[test]
fn headerless_demoted_by_fn_signature_mentioning_member() {
    // An (unused) fn whose signature mentions the member type could
    // carry a headered value across a fn boundary — program-level
    // purity demotes D while keeping b2.
    let src = format!("{NODE}fn touch(n: ListNode) -> i64 {{ n.val }}\n{CANONICAL_BUILDER}");
    let r = analyze(&src);
    let c = &r.elided_clusters["build_and_sum"][0];
    assert!(c.b2, "signature leak must not affect b2");
    assert!(!c.headerless, "fn signature mentioning member demotes D");
}

#[test]
fn headerless_demoted_by_other_struct_field_mentioning_member() {
    let src = format!("{NODE}struct Holder {{ n: Option[ListNode] }}\n{CANONICAL_BUILDER}");
    let r = analyze(&src);
    let c = &r.elided_clusters["build_and_sum"][0];
    assert!(c.b2);
    assert!(
        !c.headerless,
        "foreign struct field mentioning member demotes D"
    );
}

#[test]
fn headerless_demoted_by_free_member_literal() {
    // A member literal outside cluster-let position (here: inside a
    // tuple RHS) is a layout hazard — demote D, keep b2.
    let src = format!(
        "{NODE}fn build_and_sum(n: i64) -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let mut tail = dummy;\n\
             let mut i = 1;\n\
             while i <= n {{\n\
                 let node = ListNode {{ val: i, next: None }};\n\
                 tail.next = Some(node);\n\
                 tail = node;\n\
                 i = i + 1;\n\
             }}\n\
             let pair = (ListNode {{ val: 9, next: None }}, 1);\n\
             let mut sum = pair.1;\n\
             let mut cur = dummy.next;\n\
             while cur.is_some() {{\n\
                 let x = cur.unwrap();\n\
                 sum = sum + x.val;\n\
                 cur = x.next;\n\
             }}\n\
             sum\n\
         }}\n\
         fn main() {{ println(build_and_sum(5)); }}"
    );
    let r = analyze(&src);
    let c = &r.elided_clusters["build_and_sum"][0];
    assert!(c.b2, "free literal must not affect b2");
    assert!(!c.headerless, "free member literal demotes D");
}

#[test]
fn headerless_demoted_by_annotation_mentioning_member() {
    let src = format!(
        "{NODE}fn build_and_sum(n: i64) -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let mut tail = dummy;\n\
             let mut i = 1;\n\
             while i <= n {{\n\
                 let node = ListNode {{ val: i, next: None }};\n\
                 tail.next = Some(node);\n\
                 tail = node;\n\
                 i = i + 1;\n\
             }}\n\
             let z: Option[ListNode] = None;\n\
             let mut sum = 0;\n\
             if z.is_some() {{ sum = 1; }}\n\
             let mut cur = dummy.next;\n\
             while cur.is_some() {{\n\
                 let x = cur.unwrap();\n\
                 sum = sum + x.val;\n\
                 cur = x.next;\n\
             }}\n\
             sum\n\
         }}\n\
         fn main() {{ println(build_and_sum(5)); }}"
    );
    let r = analyze(&src);
    let c = &r.elided_clusters["build_and_sum"][0];
    assert!(c.b2, "annotation must not affect b2");
    assert!(!c.headerless, "annotation mentioning member demotes D");
}

#[test]
fn headerless_demoted_by_closure_in_fn() {
    let src = format!(
        "{NODE}fn build_and_sum(n: i64) -> i64 {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let mut tail = dummy;\n\
             let mut i = 1;\n\
             while i <= n {{\n\
                 let node = ListNode {{ val: i, next: None }};\n\
                 tail.next = Some(node);\n\
                 tail = node;\n\
                 i = i + 1;\n\
             }}\n\
             let bump = |v: i64| v + 0;\n\
             let mut sum = 0;\n\
             let mut cur = dummy.next;\n\
             while cur.is_some() {{\n\
                 let x = cur.unwrap();\n\
                 sum = sum + bump(x.val);\n\
                 cur = x.next;\n\
             }}\n\
             sum\n\
         }}\n\
         fn main() {{ println(build_and_sum(5)); }}"
    );
    let r = analyze(&src);
    let c = &r.elided_clusters["build_and_sum"][0];
    assert!(c.b2, "closure presence must not affect b2");
    assert!(!c.headerless, "boundary region (closure) demotes D");
}

#[test]
fn headerless_demoted_by_impl_on_member_type() {
    let src = format!(
        "{NODE}impl ListNode {{ fn value(ref self) -> i64 {{ self.val }} }}\n{CANONICAL_BUILDER}"
    );
    let r = analyze(&src);
    let c = &r.elided_clusters["build_and_sum"][0];
    assert!(c.b2);
    assert!(
        !c.headerless,
        "impl on the member type demotes D (coarse v1)"
    );
}

#[test]
fn headerless_demoted_by_type_alias_to_member() {
    let src = format!("{NODE}type Link = ListNode;\n{CANONICAL_BUILDER}");
    let r = analyze(&src);
    let c = &r.elided_clusters["build_and_sum"][0];
    assert!(c.b2);
    assert!(!c.headerless, "alias resolving to member demotes D");
}

// ── Phase C1b — fresh-return cluster summary ────────────────────

const SOMEROOT_BUILDER: &str = "fn build(n: i64) -> Option[ListNode] {\n\
     let head = ListNode { val: 1, next: None };\n\
     let mut tail = head;\n\
     let mut i = 2;\n\
     while i <= n {\n\
         let node = ListNode { val: i, next: None };\n\
         tail.next = Some(node);\n\
         tail = node;\n\
         i = i + 1;\n\
     }\n\
     Some(head)\n\
 }\n\
 fn main() {\n\
     let out = build(5);\n\
     if out.is_some() { println(out.unwrap().val); }\n\
 }";

const ROOTLINK_BUILDER: &str = "fn build(n: i64) -> Option[ListNode] {\n\
     let dummy = ListNode { val: 0, next: None };\n\
     let mut tail = dummy;\n\
     let mut i = 1;\n\
     while i <= n {\n\
         let node = ListNode { val: i, next: None };\n\
         tail.next = Some(node);\n\
         tail = node;\n\
         i = i + 1;\n\
     }\n\
     dummy.next\n\
 }\n\
 fn main() {\n\
     let out = build(5);\n\
     if out.is_some() { println(out.unwrap().val); }\n\
 }";

#[test]
fn fresh_return_someroot_accepted() {
    let src = format!("{NODE}{SOMEROOT_BUILDER}");
    let r = analyze(&src);
    let c = &r.elided_clusters["build"][0];
    assert!(c.b2, "builder is the canonical triple");
    assert_eq!(c.returned, karac::ownership::ReturnedChain::SomeRoot);
    assert!(
        !c.headerless,
        "returned cluster is never headerless (chain crosses the fn boundary headered)"
    );
}

#[test]
fn fresh_return_rootlink_accepted() {
    let src = format!("{NODE}{ROOTLINK_BUILDER}");
    let r = analyze(&src);
    let c = &r.elided_clusters["build"][0];
    assert!(c.b2);
    assert_eq!(c.returned, karac::ownership::ReturnedChain::RootLink);
    assert!(!c.headerless);
}

#[test]
fn fresh_return_requires_b2() {
    // The if-wrapped store breaks the adjacent triple (b2 rejected,
    // B1-shape only) — a returned chain without the count-free build
    // would leak (link-store retains leave rc==2 nodes), so NO cluster
    // forms at all and full RC stands.
    let src = format!(
        "{NODE}fn build(n: i64) -> Option[ListNode] {{\n\
             let dummy = ListNode {{ val: 0, next: None }};\n\
             let mut tail = dummy;\n\
             let mut i = 1;\n\
             while i <= n {{\n\
                 let node = ListNode {{ val: i, next: None }};\n\
                 if i > 0 {{\n\
                     tail.next = Some(node);\n\
                     tail = node;\n\
                 }}\n\
                 i = i + 1;\n\
             }}\n\
             dummy.next\n\
         }}\n\
         fn main() {{\n\
             let out = build(5);\n\
             if out.is_some() {{ println(out.unwrap().val); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(
        !r.elided_clusters.contains_key("build"),
        "returned + !b2 must form no cluster; got {:?}",
        r.elided_clusters.get("build")
    );
}

#[test]
fn fresh_return_mid_fn_return_still_poisons() {
    // A statement-position `return Some(head)` is NOT the sanctioned
    // tail shape — the default-deny Identifier escape stands and no
    // cluster forms.
    let src = format!(
        "{NODE}fn build(n: i64) -> Option[ListNode] {{\n\
             let head = ListNode {{ val: 1, next: None }};\n\
             let mut tail = head;\n\
             let mut i = 2;\n\
             while i <= n {{\n\
                 let node = ListNode {{ val: i, next: None }};\n\
                 tail.next = Some(node);\n\
                 tail = node;\n\
                 i = i + 1;\n\
             }}\n\
             if n > 3 {{\n\
                 return Some(head);\n\
             }}\n\
             None\n\
         }}\n\
         fn main() {{\n\
             let out = build(5);\n\
             if out.is_some() {{ println(out.unwrap().val); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(
        !r.elided_clusters.contains_key("build"),
        "mid-fn return must poison; got {:?}",
        r.elided_clusters.get("build")
    );
}

#[test]
fn fresh_return_some_of_cursor_rejected() {
    // `Some(<cursor>)` at tail is NOT sanctioned (only the root) —
    // returning the tail cursor would alias mid-chain and the root's
    // cleanup semantics wouldn't match. The escape poisons.
    let src = format!(
        "{NODE}fn build(n: i64) -> Option[ListNode] {{\n\
             let head = ListNode {{ val: 1, next: None }};\n\
             let mut tail = head;\n\
             let mut i = 2;\n\
             while i <= n {{\n\
                 let node = ListNode {{ val: i, next: None }};\n\
                 tail.next = Some(node);\n\
                 tail = node;\n\
                 i = i + 1;\n\
             }}\n\
             Some(tail)\n\
         }}\n\
         fn main() {{\n\
             let out = build(5);\n\
             if out.is_some() {{ println(out.unwrap().val); }}\n\
         }}"
    );
    let r = analyze(&src);
    assert!(
        !r.elided_clusters.contains_key("build"),
        "Some(cursor) tail must poison; got {:?}",
        r.elided_clusters.get("build")
    );
}
