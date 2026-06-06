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
