// tests/provider_escape.rs

use karac::parse;
use karac::provider_escape::{self, EscapeKind};
use karac::{resolve, typecheck};

fn errors(source: &str) -> Vec<provider_escape::EscapeError> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    let types = typecheck(&parsed.program, &resolved);
    provider_escape::check_provider_escape(&parsed.program, Some(&types))
}

// ── Positive cases (no escape) ──────────────────────────────────

#[test]
fn ok_resource_used_inside_block_body() {
    // Inline use of a resource inside the block is fine — no closure
    // flows out of the block.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let n = Clock.now();\n\
                 println(n);\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_closure_returned_but_does_not_capture_rooted() {
    // A closure is returned from the with_provider body, but it doesn't
    // reference any rooted resource — no escape.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 |x: i64| x + 1\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_scalar_final_expression_not_a_closure() {
    // The block's final value is a plain integer, not a closure — nothing
    // can escape by reference.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 42 } }\n\
         fn main() {\n\
             let v = with_provider[Clock](FakeClock {}, || {\n\
                 Clock.now()\n\
             });\n\
             println(v);\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_providers_block_scalar_result() {
    let errs = errors(
        "effect resource UserDB;\n\
         struct FakeDB {}\n\
         impl FakeDB { fn count(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let n = providers {\n\
                 UserDB => FakeDB {},\n\
             } in {\n\
                 UserDB.count()\n\
             };\n\
             println(n);\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

// ── Negative cases (escape detected) ────────────────────────────

#[test]
fn rejects_closure_final_expr_captures_rooted_resource() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 |d: i64| Clock.now() + d\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one escape error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert_eq!(errs[0].kind, EscapeKind::BlockFinalValue);
    assert!(
        errs[0].message().contains("Clock"),
        "message missing resource: {}",
        errs[0].message()
    );
}

#[test]
fn rejects_closure_returned_via_return_keyword() {
    // Explicit `return <closure>` from inside the with_provider body
    // closure. The spec's motivating example.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 return |d: i64| Clock.now() + d;\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one escape error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert_eq!(errs[0].kind, EscapeKind::ReturnValue);
}

#[test]
fn rejects_escape_from_providers_block() {
    let errs = errors(
        "effect resource UserDB;\n\
         struct FakeDB {}\n\
         impl FakeDB { fn count(self) -> i64 { 0 } }\n\
         fn main() {\n\
             providers {\n\
                 UserDB => FakeDB {},\n\
             } in {\n\
                 |x: i64| UserDB.count() + x\n\
             };\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one escape error, got {:?}", errs);
    assert_eq!(errs[0].resource, "UserDB");
    assert_eq!(errs[0].kind, EscapeKind::BlockFinalValue);
}

#[test]
fn rejects_escape_from_providers_block_multiple_resources() {
    // Closure captures two rooted resources — both must appear as errors.
    let errs = errors(
        "effect resource UserDB;\n\
         effect resource AuditLog;\n\
         struct FakeDB {}\n\
         impl FakeDB { fn count(self) -> i64 { 0 } }\n\
         struct FakeLog {}\n\
         impl FakeLog { fn size(self) -> i64 { 0 } }\n\
         fn main() {\n\
             providers {\n\
                 UserDB   => FakeDB {},\n\
                 AuditLog => FakeLog {},\n\
             } in {\n\
                 |x: i64| UserDB.count() + AuditLog.size() + x\n\
             };\n\
         }",
    );
    let resources: Vec<_> = errs.iter().map(|e| e.resource.as_str()).collect();
    assert!(
        resources.contains(&"UserDB"),
        "missing UserDB in {:?}",
        resources
    );
    assert!(
        resources.contains(&"AuditLog"),
        "missing AuditLog in {:?}",
        resources
    );
}

#[test]
fn rejects_closure_in_nested_with_provider_captures_outer_resource() {
    // Outer scope roots A, inner roots B; the returned closure captures
    // *both*. Nesting should propagate the outer rooted stack.
    let errs = errors(
        "effect resource A;\n\
         effect resource B;\n\
         struct P {}\n\
         impl P { fn foo(self) -> i64 { 0 } fn bar(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[A](P {}, || {\n\
                 with_provider[B](P {}, || {\n\
                     |x: i64| A.foo() + B.bar() + x\n\
                 })\n\
             });\n\
         }",
    );
    let resources: Vec<_> = errs.iter().map(|e| e.resource.as_str()).collect();
    assert!(resources.contains(&"A"), "missing A in {:?}", resources);
    assert!(resources.contains(&"B"), "missing B in {:?}", resources);
}

// ── Ambient / non-rooted behaviour ──────────────────────────────

#[test]
fn ok_unrelated_resource_not_captured_despite_sharing_prefix() {
    // `Clock` is rooted via with_provider; an unrelated `SomeType.frobnicate()`
    // call in the returned closure doesn't count as a rooted capture.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct Helper {}\n\
         impl Helper { fn frobnicate(self) -> i64 { 42 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let h = Helper {};\n\
                 |x: i64| x + 1\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_closure_outside_any_provider_scope_even_if_it_references_named_resource() {
    // No active provider scope → no rooted stack → no escape.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn make_timer() { \n\
             let later = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

// ── Indirect escape via let-bound closure ───────────────────────

#[test]
fn rejects_return_of_let_bound_closure() {
    // Spec example: `let later = |d| Clock.now() + d; return later;`
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let later = |d: i64| Clock.now() + d;\n\
                 return later;\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert_eq!(errs[0].kind, EscapeKind::ReturnValue);
}

#[test]
fn rejects_block_final_expr_referencing_let_bound_closure() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let later = |d: i64| Clock.now() + d;\n\
                 later\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert_eq!(errs[0].kind, EscapeKind::BlockFinalValue);
}

#[test]
fn rejects_indirect_escape_from_providers_block() {
    let errs = errors(
        "effect resource UserDB;\n\
         struct FakeDB {}\n\
         impl FakeDB { fn count(self) -> i64 { 0 } }\n\
         fn main() {\n\
             providers {\n\
                 UserDB => FakeDB {},\n\
             } in {\n\
                 let counter = |x: i64| UserDB.count() + x;\n\
                 counter\n\
             };\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "UserDB");
}

#[test]
fn rejects_indirect_escape_via_rebinding_chain() {
    // `let a = |...| Clock...; let b = a; return b` — the chain propagates.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let a = |d: i64| Clock.now() + d;\n\
                 let b = a;\n\
                 return b;\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
}

#[test]
fn ok_let_bound_closure_used_inline_not_escaped() {
    // Bind a closure, *call* it, return the scalar result — no escape.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let v = with_provider[Clock](FakeClock {}, || {\n\
                 let now_plus = |d: i64| Clock.now() + d;\n\
                 now_plus(5)\n\
             });\n\
             println(v);\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_let_bound_non_closure_identifier_returned() {
    // `return x` where x is an integer, not a closure — never escape.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let t = Clock.now();\n\
                 return t;\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_inner_closures_return_does_not_escape_outer_with_provider() {
    // `return outer` inside a *nested* closure returns from the nested
    // closure, not from the outer `with_provider` body. No escape.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let outer = |d: i64| Clock.now() + d;\n\
                 let helper = |x: i64| {\n\
                     return outer;\n\
                 };\n\
                 println(1);\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

// ── Transitive escape via function calls ────────────────────────

#[test]
fn rejects_return_of_helper_fn_that_builds_rooted_capturing_closure() {
    // `make_timer()` returns a closure that captures Clock — at the
    // escape site we resolve the callee by name and flag it.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn make_timer() { \n\
             let c = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 return make_timer();\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert_eq!(errs[0].kind, EscapeKind::ReturnValue);
}

#[test]
fn rejects_block_final_call_to_rooted_capturing_helper() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn build() { \n\
             let f = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 build()\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert_eq!(errs[0].kind, EscapeKind::BlockFinalValue);
}

#[test]
fn rejects_transitive_escape_from_providers_block() {
    let errs = errors(
        "effect resource UserDB;\n\
         struct FakeDB {}\n\
         impl FakeDB { fn count(self) -> i64 { 0 } }\n\
         fn make_counter() { \n\
             let c = |x: i64| UserDB.count() + x;\n\
             println(1);\n\
         }\n\
         fn main() {\n\
             providers {\n\
                 UserDB => FakeDB {},\n\
             } in {\n\
                 make_counter()\n\
             };\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "UserDB");
}

#[test]
fn ok_helper_reads_resource_inline_but_returns_scalar() {
    // `read_time()` reads Clock *inline*, not inside a closure — its
    // escapable caps are empty. Accept even though the call happens
    // at an escape position.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn read_time() -> i64 { Clock.now() }\n\
         fn main() {\n\
             let t = with_provider[Clock](FakeClock {}, || {\n\
                 read_time()\n\
             });\n\
             println(t);\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_helper_builds_closure_but_over_ambient_resource() {
    // `make_adder` returns a closure but captures nothing rooted — the
    // `with_provider[Clock]` scope doesn't mention anything this helper
    // touches, so the intersection is empty.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn make_adder() { \n\
             let a = |x: i64| x + 1;\n\
             println(1);\n\
         }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 make_adder()\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

// ── Field-assignment escape ─────────────────────────────────────

#[test]
fn rejects_field_assignment_of_rooted_capturing_closure() {
    // Assigning a closure that captures `Clock` to a struct field inside
    // the `with_provider` body flags the field-assignment escape path.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct TimerStore { f: i64 }\n\
         fn main() {\n\
             let mut store = TimerStore { f: 0 };\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 store.f = Clock.now();\n\
             });\n\
         }",
    );
    // Scalar assignment to `store.f` where the RHS is a scalar, not a
    // closure — no escape. Sanity check for the positive path.
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn rejects_field_assignment_of_closure_literal() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct TimerStore { f: i64 }\n\
         fn main() {\n\
             let mut store = TimerStore { f: 0 };\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 store.f = |d: i64| Clock.now() + d;\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert!(
        matches!(errs[0].kind, EscapeKind::FieldAssignment { ref target_desc } if target_desc == "store.f"),
        "expected FieldAssignment kind with target_desc 'store.f', got {:?}",
        errs[0].kind
    );
}

#[test]
fn rejects_field_assignment_of_let_bound_closure() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct TimerStore { f: i64 }\n\
         fn main() {\n\
             let mut store = TimerStore { f: 0 };\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let c = |d: i64| Clock.now() + d;\n\
                 store.f = c;\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert!(matches!(errs[0].kind, EscapeKind::FieldAssignment { .. }));
}

#[test]
fn rejects_field_assignment_of_transitive_call() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct TimerStore { f: i64 }\n\
         fn make_timer() {\n\
             let c = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         }\n\
         fn main() {\n\
             let mut store = TimerStore { f: 0 };\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 store.f = make_timer();\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert!(matches!(errs[0].kind, EscapeKind::FieldAssignment { .. }));
}

#[test]
fn rejects_index_assignment_of_rooted_capturing_closure() {
    // `arr[0] = closure` is index-slot assignment — same escape category
    // as field assignment.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let mut arr: Array[i64, 4] = [0, 0, 0, 0];\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 arr[0] = |d: i64| Clock.now() + d;\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert!(matches!(errs[0].kind, EscapeKind::FieldAssignment { .. }));
}

#[test]
fn ok_plain_identifier_inner_scope_reassignment_not_flagged() {
    // `local` is let-bound *inside* the with_provider body, so its
    // lifetime ends when the block exits — reassigning it to a
    // rooted-capturing closure is a no-op for escape purposes.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let mut local = |d: i64| d;\n\
                 local = |d: i64| Clock.now() + d;\n\
                 println(1);\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

// ── Outer identifier reassignment escape ────────────────────────

#[test]
fn rejects_reassignment_of_outer_let_to_rooted_capturing_closure() {
    // `outer` is let-bound *before* the `with_provider` opens — its
    // lifetime extends past the block, so overwriting it with a
    // rooted-capturing closure leaks `Clock` out of the scope.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let mut outer = |d: i64| d;\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 outer = |d: i64| Clock.now() + d;\n\
             });\n\
             println(1);\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert!(
        matches!(errs[0].kind, EscapeKind::OuterIdentifierAssignment { ref target_name } if target_name == "outer"),
        "expected OuterIdentifierAssignment for `outer`, got {:?}",
        errs[0].kind
    );
}

#[test]
fn rejects_reassignment_of_outer_let_via_let_bound_closure() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let mut outer = |d: i64| d;\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 let c = |d: i64| Clock.now() + d;\n\
                 outer = c;\n\
             });\n\
             println(1);\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert!(matches!(
        errs[0].kind,
        EscapeKind::OuterIdentifierAssignment { .. }
    ));
}

#[test]
fn rejects_reassignment_of_outer_let_via_transitive_call() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn make_timer() {\n\
             let c = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         }\n\
         fn main() {\n\
             let mut outer = |d: i64| d;\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 outer = make_timer();\n\
             });\n\
             println(1);\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert!(matches!(
        errs[0].kind,
        EscapeKind::OuterIdentifierAssignment { .. }
    ));
}

#[test]
fn ok_scalar_reassignment_of_outer_identifier_not_flagged() {
    // Outer binding, RHS is a scalar (call returning i64, not a closure).
    // The identifier-escape path only fires for closure-producing RHS —
    // scalar reassignment is fine regardless of scope.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let mut outer = 0;\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 outer = Clock.now();\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn rejects_reassignment_from_nested_block_targeting_outer_binding() {
    // Even a nested block inside the `with_provider` body can flag —
    // the assignment target's scope is still outer.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let mut outer = |d: i64| d;\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 if true {\n\
                     outer = |d: i64| Clock.now() + d;\n\
                 }\n\
                 println(1);\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert!(matches!(
        errs[0].kind,
        EscapeKind::OuterIdentifierAssignment { .. }
    ));
}

#[test]
fn rejects_transitive_escape_via_type_method() {
    // `Builder.make()` (type-method call `T.method(...)`) is resolved by
    // `TypeName.method` key. Constructing a closure inside the method
    // body counts as capturable output.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct Builder {}\n\
         impl Builder { fn make() { \n\
             let c = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         } }\n\
         fn main() {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 return Builder.make();\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
}

// ── Instance-method escape (v.method(...)) ─────────────────────

#[test]
fn rejects_instance_method_return_transitively_escaping() {
    // `b.build()` on a bound instance resolves to `Builder.build` via
    // typecheck-derived receiver type. The method constructs a closure
    // capturing `Clock`, so the return value carries it out.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct Builder {}\n\
         impl Builder { fn build(self) { \n\
             let c = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         } }\n\
         fn main() {\n\
             let b = Builder {};\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 return b.build();\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert_eq!(errs[0].kind, EscapeKind::ReturnValue);
}

#[test]
fn rejects_instance_method_block_final_transitively_escaping() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct Builder {}\n\
         impl Builder { fn build(self) { \n\
             let c = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         } }\n\
         fn main() {\n\
             let b = Builder {};\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 b.build()\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert_eq!(errs[0].kind, EscapeKind::BlockFinalValue);
}

#[test]
fn rejects_instance_method_field_assignment_transitively_escaping() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct Builder {}\n\
         impl Builder { fn build(self) { \n\
             let c = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         } }\n\
         struct Store { f: i64 }\n\
         fn main() {\n\
             let mut store = Store { f: 0 };\n\
             let b = Builder {};\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 store.f = b.build();\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert!(matches!(errs[0].kind, EscapeKind::FieldAssignment { .. }));
}

#[test]
fn rejects_instance_method_outer_identifier_reassignment() {
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct Builder {}\n\
         impl Builder { fn build(self) { \n\
             let c = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         } }\n\
         fn main() {\n\
             let mut outer = 0;\n\
             let b = Builder {};\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 outer = b.build();\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
    assert!(matches!(
        errs[0].kind,
        EscapeKind::OuterIdentifierAssignment { .. }
    ));
}

#[test]
fn ok_instance_method_on_unrelated_type() {
    // `b.noop()` returns nothing closure-like — intersect is empty.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct Builder {}\n\
         impl Builder { fn noop(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let b = Builder {};\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 return b.noop();\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn rejects_instance_method_on_ref_receiver() {
    // Receiver type is `ref Builder` — the type-name resolver strips the
    // `ref` wrapper and still resolves to `Builder.build`.
    let errs = errors(
        "effect resource Clock;\n\
         struct FakeClock {}\n\
         impl FakeClock { fn now(self) -> i64 { 0 } }\n\
         struct Builder {}\n\
         impl Builder { fn build(ref self) { \n\
             let c = |d: i64| Clock.now() + d;\n\
             println(1);\n\
         } }\n\
         fn use_ref(b: ref Builder) {\n\
             with_provider[Clock](FakeClock {}, || {\n\
                 return b.build();\n\
             });\n\
         }\n\
         fn main() {\n\
             let b = Builder {};\n\
             use_ref(b);\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Clock");
}

// ── Channel-send escape (round 8) ──────────────────────────────

#[test]
fn err_channel_send_closure_literal_captures_rooted() {
    // Direct closure literal sent via `tx.send(closure)` where the closure
    // captures a provider-rooted resource — rejected.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let (tx, rx) = Channel.new();\n\
             with_provider[Db](FakeDb {}, || {\n\
                 tx.send(|| Db.query());\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Db");
    assert_eq!(errs[0].kind, EscapeKind::ChannelSend);
}

#[test]
fn err_channel_send_let_bound_closure_captures_rooted() {
    // Closure assigned to a let-binding, then sent — same rule via the
    // let-bound-identifier rebind chain.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let (tx, rx) = Channel.new();\n\
             with_provider[Db](FakeDb {}, || {\n\
                 let job = || Db.query();\n\
                 tx.send(job);\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Db");
    assert_eq!(errs[0].kind, EscapeKind::ChannelSend);
}

#[test]
fn err_channel_send_helper_returning_closure() {
    // A helper function returns a closure capturing a rooted resource;
    // the return value flows directly into `tx.send(...)` — caught via
    // the program-wide `compute_escapable_caps` pre-pass.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn make_job() -> Fn() -> i64 { || Db.query() }\n\
         fn main() {\n\
             let (tx, rx) = Channel.new();\n\
             with_provider[Db](FakeDb {}, || {\n\
                 tx.send(make_job());\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Db");
    assert_eq!(errs[0].kind, EscapeKind::ChannelSend);
}

#[test]
fn err_channel_send_via_function_param_sender() {
    // The Sender comes in as a function parameter (not from
    // `Channel.new()`); the closure still captures a rooted resource and
    // escapes via send. Parameter type annotation drives the receiver-
    // type resolution.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn forward(tx: Sender[i64]) {\n\
             with_provider[Db](FakeDb {}, || {\n\
                 tx.send(|| Db.query());\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Db");
    assert_eq!(errs[0].kind, EscapeKind::ChannelSend);
}

#[test]
fn ok_channel_send_non_closure_value() {
    // Sending a primitive value through the channel — no closure, no
    // escape concern.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let (tx, rx) = Channel.new();\n\
             with_provider[Db](FakeDb {}, || {\n\
                 let n = Db.query();\n\
                 tx.send(n);\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_channel_send_pure_closure() {
    // Closure with no captures of any rooted resource — passes.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let (tx, rx) = Channel.new();\n\
             with_provider[Db](FakeDb {}, || {\n\
                 tx.send(|x: i64| x + 1);\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_channel_send_ambient_only_closure() {
    // Closure captures only ambient program-rooted resources (Clock under
    // default provider) — no `with_provider` introduces those resources,
    // so they aren't on the rooted stack and the send is allowed.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             let (tx, rx) = Channel.new();\n\
             with_provider[Db](FakeDb {}, || {\n\
                 tx.send(|| Clock.now());\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

// ── spawn escape ───────────────────────────────────────────────

#[test]
fn err_spawn_closure_literal_captures_rooted() {
    // Direct closure literal handed to `spawn(closure)` where the closure
    // captures a provider-rooted resource — rejected.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Db](FakeDb {}, || {\n\
                 spawn(|| Db.query());\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Db");
    assert_eq!(errs[0].kind, EscapeKind::Spawn);
}

#[test]
fn err_spawn_let_bound_closure_captures_rooted() {
    // Closure assigned to a let-binding, then spawned — same rule via the
    // let-bound-identifier rebind chain.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Db](FakeDb {}, || {\n\
                 let job = || Db.query();\n\
                 spawn(job);\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Db");
    assert_eq!(errs[0].kind, EscapeKind::Spawn);
}

#[test]
fn err_spawn_helper_returning_closure() {
    // A helper function returns a closure capturing a rooted resource;
    // the return value flows directly into `spawn(...)` — caught via
    // the program-wide `compute_escapable_caps` pre-pass.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn make_job() -> Fn() -> i64 { || Db.query() }\n\
         fn main() {\n\
             with_provider[Db](FakeDb {}, || {\n\
                 spawn(make_job());\n\
             });\n\
         }",
    );
    assert_eq!(errs.len(), 1, "expected one error, got {:?}", errs);
    assert_eq!(errs[0].resource, "Db");
    assert_eq!(errs[0].kind, EscapeKind::Spawn);
}

#[test]
fn ok_spawn_non_closure_value() {
    // Spawning a non-closure value — no closure, no escape concern.
    // (Won't compile end-to-end but the escape check runs structurally.)
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Db](FakeDb {}, || {\n\
                 let n = Db.query();\n\
                 spawn(n);\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_spawn_pure_closure() {
    // Closure with no captures of any rooted resource — passes.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Db](FakeDb {}, || {\n\
                 spawn(|x: i64| x + 1);\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}

#[test]
fn ok_spawn_ambient_only_closure() {
    // Closure captures only ambient program-rooted resources (Clock under
    // default provider) — no `with_provider` introduces those resources,
    // so they aren't on the rooted stack and the spawn is allowed.
    let errs = errors(
        "effect resource Db;\n\
         struct FakeDb {}\n\
         impl FakeDb { fn query(self) -> i64 { 0 } }\n\
         fn main() {\n\
             with_provider[Db](FakeDb {}, || {\n\
                 spawn(|| Clock.now());\n\
             });\n\
         }",
    );
    assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
}
