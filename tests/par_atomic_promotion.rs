//! B-2026-08-01-33 mechanism 2 — whole-program atomicity promotion.
//!
//! Its own test binary, deliberately. The capability is opt-in via the
//! `KARAC_PAR_ATOMIC_PROMOTION` env var, and env is PROCESS-global while cargo
//! runs tests in parallel threads — setting it inside `tests/ownership.rs`
//! leaked into sibling tests there and made
//! `shared_struct_two_branch_use_still_fires_after_par_exemption` fail. A
//! separate integration test is a separate process, so the opt-in cannot
//! escape these cases.

use karac::ownership::OwnershipErrorKind;

mod common_helpers {
    pub fn ownership_ok(src: &str) -> karac::ownership::OwnershipCheckResult {
        let parsed = karac::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::ownershipcheck(&parsed.program, &typed)
    }
}
use common_helpers::ownership_ok;

fn ownership_errors(src: &str) -> Vec<karac::ownership::OwnershipError> {
    ownership_ok(src).errors
}

/// B-2026-08-01-33 mechanism 2 — WHOLE-PROGRAM ATOMICITY PROMOTION.
///
/// A `shared` value whose whole reachable type set is free of `mut` fields is
/// deeply immutable. Promoting those types to atomic refcounting removes the
/// header race; the immutability means there is no payload race to remove. So
/// the value may be TRAVERSED — interior handles materialized and bound — from
/// sibling branches, which the projection-only leg (66769bf) could not admit.
///
/// Measured on the shipped fix: every par branch touching the type censuses
/// atomic=4, PLAIN=0, including a branch that builds a fresh LOCAL instance —
/// the promotion is type-level, so it reaches inside callees and drop walks
/// where a per-binding promotion could not.
#[test]
fn immutable_shared_graph_is_admitted_across_branches() {
    // Opt-in: the promotion changes a documented language rule, so it ships
    // inert. See `atomic_promotion_closure` for why the default is off.
    std::env::set_var("KARAC_PAR_ATOMIC_PROMOTION", "1");
    let result = ownership_ok(
        "shared struct Node { val: i64, kids: Vec[Node] }\n\
         fn total(n: Node) -> i64 { n.val }\n\
         fn main() {\n\
             let root = Node { val: 10, kids: Vec.new() };\n\
             par {\n\
                 total(root);\n\
                 total(root);\n\
             }\n\
         }",
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. })),
        "a deeply immutable shared value must be shareable across branches; got: {:?}",
        result.errors
    );
    assert!(
        result.atomic_promoted_types.contains("Node"),
        "admitting the capture must promote the type to atomic RC; promoted: {:?}",
        result.atomic_promoted_types
    );
    std::env::remove_var("KARAC_PAR_ATOMIC_PROMOTION");
}

/// The promotion is only sound because the payload is immutable. Atomic RC
/// fixes the HEADER; a `mut` field written from two branches is still a data
/// race, which is exactly why `par struct` forces `Atomic[T]`/`Mutex[T]` on
/// its mutable fields. Both of these must keep firing.
#[test]
fn shared_struct_with_a_mut_field_is_still_rejected() {
    std::env::set_var("KARAC_PAR_ATOMIC_PROMOTION", "1");
    // (a) `mut` on the captured type itself.
    let direct = ownership_errors(
        "shared struct Node { val: i64, mut kids: Vec[Node] }\n\
         fn total(n: Node) -> i64 { n.val }\n\
         fn main() {\n\
             let root = Node { val: 10, kids: Vec.new() };\n\
             par {\n\
                 total(root);\n\
                 total(root);\n\
             }\n\
         }",
    );
    assert!(
        direct
            .iter()
            .any(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. })),
        "a mut field means a payload race atomic RC cannot fix; got: {direct:?}"
    );

    // (b) `mut` on a type reachable only THROUGH a field. This is the case the
    // transitive closure exists for — promoting `Outer` alone would leave
    // `Inner`'s payload racy, and stopping the walk at the named type would
    // silently admit it.
    let nested = ownership_errors(
        "shared struct Inner { mut v: i64 }\n\
         shared struct Outer { inner: Inner }\n\
         fn total(o: Outer) -> i64 { o.inner.v }\n\
         fn main() {\n\
             let root = Outer { inner: Inner { v: 1 } };\n\
             par {\n\
                 total(root);\n\
                 total(root);\n\
             }\n\
         }",
    );
    assert!(
        nested
            .iter()
            .any(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. })),
        "a mut field on a TRANSITIVELY reachable type must also reject; got: {nested:?}"
    );
    std::env::remove_var("KARAC_PAR_ATOMIC_PROMOTION");
}
